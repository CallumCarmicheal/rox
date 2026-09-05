//! XSPF read and write, the third interop surface for playlists (ADR 16).
//! XSPF is the XML one, and the only one of the three with a real
//! specification behind it: locations are URIs, so a path with a space or a
//! non-ASCII letter survives the trip instead of depending on the reader's
//! guess about the file's encoding.
//!
//! The writer is hand-rolled because the document has six element types and
//! no attributes worth the name; a serializer crate would earn nothing here.
//! The reader uses `roxmltree`, a read-only tree over the whole document,
//! which a playlist file is comfortably small enough for.
//!
//! Deliberately excluded: `<extension>` blocks, per-track `<image>` and
//! `<info>`, and playlist-level metadata. rox stores all of that in the
//! catalog, and a file is a snapshot, never where a playlist lives.

use std::path::Path;

use crate::playlists::ExportTrack;

/// Serialize playable rows to an XSPF document. Locations are `file:` URIs so
/// spaces and non-ASCII survive; empty fields are left out entirely rather
/// than written as empty elements, and an unknown duration is omitted the way
/// the spec asks instead of getting M3U's `-1` sentinel.
///
/// A cue subsong's `path#N` is written as a URI fragment, which is what the
/// fragment syntax is for. [`crate::cue::TrackKey::from_fragment`] reads it
/// back on the other side.
pub fn to_xspf(rows: &[ExportTrack]) -> String {
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<playlist version=\"1\" xmlns=\"http://xspf.org/ns/0/\">\n");
    out.push_str("  <trackList>\n");
    for row in rows {
        out.push_str("    <track>\n");
        out.push_str(&format!(
            "      <location>{}</location>\n",
            escape(&location(&row.path))
        ));
        if !row.title.is_empty() {
            out.push_str(&format!("      <title>{}</title>\n", escape(&row.title)));
        }
        if !row.artist.is_empty() {
            out.push_str(&format!(
                "      <creator>{}</creator>\n",
                escape(&row.artist)
            ));
        }
        if row.duration_secs > 0 {
            out.push_str(&format!(
                "      <duration>{}</duration>\n",
                row.duration_secs * 1000
            ));
        }
        out.push_str("    </track>\n");
    }
    out.push_str("  </trackList>\n");
    out.push_str("</playlist>\n");
    out
}

/// Pull the track locations out of an XSPF document in document order, each
/// turned back into the path string the resolver takes. A `file:` URI is
/// decoded to a native path, fragment reattached; anything else (an `http:`
/// stream, or the bare relative path some writers emit instead of a URI)
/// passes through verbatim so the resolver's relative-path rule gets a shot
/// at it.
///
/// Matching is on local names only. Files in the wild drop the XSPF
/// namespace often enough that requiring it would reject documents every
/// other player reads.
pub fn parse(text: &str) -> Vec<String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let Ok(doc) = roxmltree::Document::parse(text) else {
        return Vec::new();
    };
    doc.descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "track")
        .filter_map(|track| {
            track
                .children()
                .find(|child| child.is_element() && child.tag_name().name() == "location")
                .and_then(|node| node.text())
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(from_location)
        })
        .collect()
}

/// A path (possibly carrying a `#N` cue fragment) as a `file:` URI. Falls
/// back to the raw string for anything `Url` refuses, which is a relative
/// path: those are legal in XSPF and the reader on the other end resolves
/// them against the file's folder anyway.
fn location(path: &str) -> String {
    let (base, fragment) = split_fragment(path);
    match url::Url::from_file_path(Path::new(base)) {
        Ok(url) => match fragment {
            Some(sub) => format!("{url}#{sub}"),
            None => url.to_string(),
        },
        Err(()) => path.to_owned(),
    }
}

/// One location back to a path string. See [`parse`] for what passes through
/// untouched.
fn from_location(text: &str) -> String {
    let Ok(url) = url::Url::parse(text) else {
        return text.to_owned();
    };
    if url.scheme() != "file" {
        return text.to_owned();
    }
    let Ok(path) = url.to_file_path() else {
        return text.to_owned();
    };
    let path = path.to_string_lossy().into_owned();
    match url.fragment() {
        Some(sub) if !sub.is_empty() => format!("{path}#{sub}"),
        _ => path,
    }
}

/// Split a trailing `#N` cue fragment off a path. Only a positive integer
/// counts, the same reading [`crate::cue::TrackKey::from_fragment`] uses, so
/// a file whose name really ends in `#hits` keeps it.
fn split_fragment(path: &str) -> (&str, Option<u16>) {
    match path.rsplit_once('#') {
        Some((base, sub)) => match sub.parse::<u16>() {
            Ok(sub) if sub > 0 => (base, Some(sub)),
            _ => (path, None),
        },
        None => (path, None),
    }
}

/// The three characters that cannot sit in XML text as themselves. Quotes are
/// left alone: nothing here writes an attribute value.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(path: &str, artist: &str, title: &str, secs: i64) -> ExportTrack {
        ExportTrack {
            path: path.into(),
            title: title.into(),
            artist: artist.into(),
            duration_secs: secs,
        }
    }

    #[test]
    fn writes_uris_and_escapes_text() {
        let xspf = to_xspf(&[row("/m/Bad & Über/one two.mp3", "A & B", "One", 210)]);
        assert!(
            xspf.contains("<location>file:///m/Bad%20&amp;%20%C3%9Cber/one%20two.mp3</location>"),
            "{xspf}"
        );
        assert!(xspf.contains("<title>One</title>"));
        assert!(xspf.contains("<creator>A &amp; B</creator>"));
        assert!(xspf.contains("<duration>210000</duration>"));
        assert_eq!(parse(&xspf), ["/m/Bad & Über/one two.mp3"]);
    }

    #[test]
    fn empty_fields_write_no_elements() {
        let xspf = to_xspf(&[row("/m/one.mp3", "", "", 0)]);
        assert!(!xspf.contains("<title>"));
        assert!(!xspf.contains("<creator>"));
        assert!(!xspf.contains("<duration>"));
    }

    #[test]
    fn round_trips_paths_in_order() {
        let rows = [
            row("/m/a.flac", "Artist", "A", 5),
            row("/m/b.flac", "Artist", "B", 6),
        ];
        assert_eq!(parse(&to_xspf(&rows)), ["/m/a.flac", "/m/b.flac"]);
    }

    #[test]
    fn a_cue_fragment_round_trips() {
        let xspf = to_xspf(&[row("/m/Album/disc.flac#3", "X", "Three", 180)]);
        assert!(
            xspf.contains("<location>file:///m/Album/disc.flac#3</location>"),
            "{xspf}"
        );
        assert_eq!(parse(&xspf), ["/m/Album/disc.flac#3"]);
    }

    #[test]
    fn a_non_file_location_comes_back_verbatim() {
        let text = "<playlist xmlns=\"http://xspf.org/ns/0/\"><trackList>\
                    <track><location>http://stream.example/live.ogg</location></track>\
                    <track><location>sub/relative.mp3</location></track>\
                    </trackList></playlist>";
        assert_eq!(
            parse(text),
            ["http://stream.example/live.ogg", "sub/relative.mp3"]
        );
    }

    #[test]
    fn a_document_without_the_namespace_still_parses() {
        let text = "<playlist version=\"1\"><trackList>\
                    <track><location>file:///m/one.mp3</location><title>One</title></track>\
                    </trackList></playlist>";
        assert_eq!(parse(text), ["/m/one.mp3"]);
    }

    #[test]
    fn malformed_xml_yields_nothing() {
        assert!(parse("<playlist><trackList>").is_empty());
    }
}
