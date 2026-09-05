//! One door in front of the three playlist formats rox reads and writes
//! (ADR 16): M3U/M3U8, PLS, and XSPF. Everything above this module deals in
//! "a playlist file" and lets the dispatcher decide which parser runs, so the
//! panel's import path, the command line, and a window drop all gained the
//! two new formats at once.
//!
//! Reading sniffs the content rather than trusting the extension. A `.m3u`
//! holding a PLS body is a thing that exists in the wild, and getting it
//! wrong means an import that silently produces an empty playlist. Writing
//! goes the other way and takes the extension, because on export the name the
//! user typed in the save dialog is the only format signal there is.

use std::path::Path;

use crate::playlists::ExportTrack;

/// The playlist formats rox speaks. Not a list of everything the Wikipedia
/// table names: ASX and WPL are Windows Media shapes whose files have mostly
/// stopped being written, and adding them costs a parser each.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    M3u,
    Pls,
    Xspf,
}

impl Format {
    /// Every format, in the order a picker lists them.
    pub const ALL: [Format; 3] = [Format::M3u, Format::Pls, Format::Xspf];

    /// The name a picker shows for the format.
    pub fn label(self) -> &'static str {
        match self {
            Format::M3u => "M3U",
            Format::Pls => "PLS",
            Format::Xspf => "XSPF",
        }
    }

    /// The format an extension claims, case-insensitively. `None` for
    /// anything that is not a playlist name at all.
    pub fn from_path(path: &Path) -> Option<Format> {
        let ext = path.extension()?.to_str()?;
        if ext.eq_ignore_ascii_case("m3u") || ext.eq_ignore_ascii_case("m3u8") {
            Some(Format::M3u)
        } else if ext.eq_ignore_ascii_case("pls") {
            Some(Format::Pls)
        } else if ext.eq_ignore_ascii_case("xspf") {
            Some(Format::Xspf)
        } else {
            None
        }
    }

    /// The format a document's own first line claims. A `[playlist]` header
    /// is PLS, an opening angle bracket is XML and so XSPF (the `<?xml`
    /// declaration counts, since that is what an XSPF file actually starts
    /// with), and everything else falls to M3U, which is the permissive one:
    /// a bare list of paths is a valid M3U and nothing else.
    pub fn sniff(text: &str) -> Format {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let first = text
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or_default();
        if first
            .get(..9)
            .is_some_and(|head| head.eq_ignore_ascii_case("[playlist"))
        {
            Format::Pls
        } else if first.starts_with('<') {
            Format::Xspf
        } else {
            Format::M3u
        }
    }

    /// The extension to save this format under. M3U writes `m3u8` because
    /// what the writer emits is UTF-8 and the `.m3u8` name is what says so.
    pub fn extension(self) -> &'static str {
        match self {
            Format::M3u => "m3u8",
            Format::Pls => "pls",
            Format::Xspf => "xspf",
        }
    }
}

/// Every extension that names a playlist file, for the open and drop paths.
pub const EXTENSIONS: &[&str] = &["m3u", "m3u8", "pls", "xspf"];

/// True for a file whose extension rox recognizes as a playlist.
pub fn is_playlist_file(path: &Path) -> bool {
    Format::from_path(path).is_some()
}

/// Pull the path entries out of a playlist document, whichever of the three
/// formats it is. Order is the file's order in every case.
pub fn parse(text: &str) -> Vec<String> {
    match Format::sniff(text) {
        Format::M3u => crate::m3u::parse(text),
        Format::Pls => crate::pls::parse(text),
        Format::Xspf => crate::xspf::parse(text),
    }
}

/// Serialize playable rows in the named format.
pub fn write(format: Format, rows: &[ExportTrack]) -> String {
    match format {
        Format::M3u => crate::m3u::to_m3u8(rows),
        Format::Pls => crate::pls::to_pls(rows),
        Format::Xspf => crate::xspf::to_xspf(rows),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn row(path: &str) -> ExportTrack {
        ExportTrack {
            path: path.into(),
            title: "One".into(),
            artist: "A".into(),
            duration_secs: 210,
        }
    }

    #[test]
    fn extensions_map_to_formats_case_insensitively() {
        let of = |name: &str| Format::from_path(&PathBuf::from(name));
        assert_eq!(of("a.m3u"), Some(Format::M3u));
        assert_eq!(of("a.M3U8"), Some(Format::M3u));
        assert_eq!(of("a.PLS"), Some(Format::Pls));
        assert_eq!(of("a.xspf"), Some(Format::Xspf));
        assert_eq!(of("a.txt"), None);
        assert_eq!(of("bare"), None);
        assert!(is_playlist_file(&PathBuf::from("/m/set.pls")));
        assert!(!is_playlist_file(&PathBuf::from("/m/song.flac")));
    }

    #[test]
    fn sniff_reads_the_body_not_the_name() {
        assert_eq!(Format::sniff("[playlist]\nFile1=/m/a.mp3\n"), Format::Pls);
        assert_eq!(
            Format::sniff("\u{feff}\n\n[PLAYLIST]\nfile1=/m/a.mp3\n"),
            Format::Pls
        );
        assert_eq!(
            Format::sniff("<?xml version=\"1.0\"?>\n<playlist/>"),
            Format::Xspf
        );
        assert_eq!(Format::sniff("<playlist version=\"1\"/>"), Format::Xspf);
        assert_eq!(Format::sniff("#EXTM3U\n/m/a.mp3\n"), Format::M3u);
        assert_eq!(Format::sniff("/m/a.mp3\n"), Format::M3u);
        assert_eq!(Format::sniff(""), Format::M3u);
    }

    #[test]
    fn parse_dispatches_on_the_sniffed_format() {
        // The case sniffing exists for: a .m3u name over a PLS body.
        let entries = parse("[playlist]\nFile1=/m/a.mp3\nNumberOfEntries=1\n");
        assert_eq!(entries, ["/m/a.mp3"]);
    }

    #[test]
    fn every_format_round_trips_through_the_door() {
        for format in [Format::M3u, Format::Pls, Format::Xspf] {
            let text = write(format, &[row("/m/a.mp3"), row("/m/b.mp3")]);
            assert_eq!(Format::sniff(&text), format, "{format:?} sniffs as itself");
            assert_eq!(parse(&text), ["/m/a.mp3", "/m/b.mp3"], "{format:?}");
        }
    }

    #[test]
    fn extensions_cover_the_constant() {
        for ext in EXTENSIONS {
            assert!(Format::from_path(&PathBuf::from(format!("a.{ext}"))).is_some());
        }
    }
}
