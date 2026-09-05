//! The colour grade a Milkdrop frame passes through on its way to the screen.
//!
//! Presets were drawn on black by people who assumed black, and most of
//! the twenty-year corpus lights a few shapes against it. On the dark
//! theme that's the look. On the light theme it's a dark hole in a pale
//! window, and no amount of fading toward the panel background fixes a
//! frame whose own background is the wrong colour. So the frame gets a
//! grade before it's composited: nothing on the dark theme, and one of
//! two remaps on the light one.
//!
//! It lives here for the same reason [`crate::fade`] does: the Milkdrop
//! panel and the app-wide backdrop both draw a frame through a one-pass
//! chain, and both want the same maths in the same slots. The WGSL is
//! one string prepended to each pass's own source, and [`Grade`] is the
//! Rust side that fills the slots the string reads.
//!
//! ## The two remaps
//!
//! `Theme` inverts the frame's Oklab lightness on the light theme and
//! leaves the dark theme alone. Chroma and hue are kept, so a blue
//! streak stays blue while the black behind it becomes white: the
//! preset's own design survives, upside down in lightness only.
//!
//! `Palette` maps the frame's lightness onto a ramp from the theme's root
//! background to its accent, interpolated in Oklab so the midpoints
//! don't go grey. Black lands on the background and white on the accent,
//! which is the app's own colours in the preset's shape, and it follows
//! the cover for free when song theming drives the accent. `Cover` is the
//! same ramp topped with the playing cover's dominant colour, for the
//! cover's colour with song theming off; the accent stands in while
//! there's no cover to take it from. The cover colour lends its hue and
//! chroma only: its lightness is replaced by the accent's, which the
//! palette already set against the background. A dark cover taken as-is
//! made a ramp from near-black to dark brown, and a frame with no
//! contrast in it is a frame nobody can see.
//!
//! Both run in linear light: the frame is sampled from an sRGB texture
//! that decodes on read, and Oklab is defined from linear sRGB. The theme
//! colours are handed over already linearised for the same reason.

use gpui::Rgba;
use rox_design::palette;

/// The slot layout every Milkdrop pass shares. The fade, hue and tint are
/// the panel's; the backdrop leaves the tint at zero. Named so the two
/// consumers can't disagree on a number.
pub const SLOT_FADE: usize = 0;
pub const SLOT_HUE: usize = 1;
pub const SLOT_TINT: usize = 2;
pub const SLOT_MODE: usize = 3;
pub const SLOT_BG: usize = 4;
pub const SLOT_LIGHT: usize = 7;
pub const SLOT_ACCENT: usize = 8;
pub const SLOT_COVER: usize = 11;

/// How the frame's colours meet the theme. Mirrors rox-core's
/// `MilkdropColor` one for one; that one is the setting on disk, this
/// one is what a pass reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GradeMode {
    /// The preset's own colours, whatever the theme.
    Preset,
    /// The preset's colours on the dark theme, lightness inverted on the
    /// light one.
    #[default]
    Theme,
    /// Lightness mapped onto the theme's background-to-accent ramp.
    Palette,
    /// The same ramp topped with the playing cover's colour.
    Cover,
}

/// What a pass needs to grade a frame: the mode, which theme it's under,
/// and the two colours the palette ramp runs between, in linear light.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grade {
    pub mode: GradeMode,
    pub light: bool,
    pub bg: [f32; 3],
    pub accent: [f32; 3],
    /// The cover ramp's top stop: the cover's hue and chroma at the
    /// accent's lightness, or the accent again while there's no cover
    /// with colour in it.
    pub cover: [f32; 3],
}

impl Grade {
    /// A grade from explicit colours. `bg` and `accent` arrive as the
    /// palette hands them out, sRGB, and go into the slots linear.
    /// `cover` is the playing cover's dominant colour, None while nothing
    /// plays or the cover is grey.
    pub fn new(mode: GradeMode, light: bool, bg: Rgba, accent: Rgba, cover: Option<Rgba>) -> Grade {
        Grade {
            mode,
            light,
            bg: linear(bg),
            accent: linear(accent),
            cover: linear(cover.map_or(accent, |cover| cover_top(cover, accent))),
        }
    }

    /// The grade for whatever palette scope the caller is rendering in:
    /// the panel's own theme override inside a themed body, the window's
    /// tint outside one. Light is read off the root background rather
    /// than the app-wide theme pick, so a light panel in a dark app
    /// grades as light.
    pub fn from_scope(mode: GradeMode, cover: Option<Rgba>) -> Grade {
        let bg = palette::bg_root_opaque();
        Grade::new(mode, is_light(bg), bg, palette::accent(), cover)
    }

    /// Whether the album tint still applies over this grade. The palette
    /// ramp is the theme's colours by choice, and turning those toward the
    /// cover would undo the choice; the cover ramp is already the cover's
    /// colour, so there's nothing left to turn.
    pub fn tints(&self) -> bool {
        !matches!(self.mode, GradeMode::Palette | GradeMode::Cover)
    }

    /// Fill the grade's slots. The fade, hue and tint slots are the
    /// caller's and are left alone.
    pub fn write(&self, signals: &mut [f32; 16]) {
        signals[SLOT_MODE] = match self.mode {
            GradeMode::Preset => 0.0,
            GradeMode::Theme => 1.0,
            GradeMode::Palette => 2.0,
            GradeMode::Cover => 3.0,
        };
        signals[SLOT_BG..SLOT_BG + 3].copy_from_slice(&self.bg);
        signals[SLOT_LIGHT] = if self.light { 1.0 } else { 0.0 };
        signals[SLOT_ACCENT..SLOT_ACCENT + 3].copy_from_slice(&self.accent);
        signals[SLOT_COVER..SLOT_COVER + 3].copy_from_slice(&self.cover);
    }
}

/// The least chroma the cover ramp tops out at. A cover whose dominant
/// colour is a muted tan still has a hue worth showing, and at the
/// accent's lightness that hue needs some chroma behind it to read as a
/// colour rather than a warm grey.
const COVER_CHROMA_FLOOR: f32 = 0.1;

/// The cover ramp's top stop: the cover's hue, its chroma or the floor,
/// and the accent's lightness. The lightness swap is the whole point: the
/// ramp's contrast is the distance between its ends, and the palette
/// already put the accent that distance from the background.
pub fn cover_top(cover: Rgba, accent: Rgba) -> Rgba {
    let (lightness, _, _) = palette::rgba_to_oklch(accent);
    let (_, chroma, hue) = palette::rgba_to_oklch(cover);
    palette::oklch_to_rgba(lightness, chroma.max(COVER_CHROMA_FLOOR), hue, 1.0)
}

/// Whether a background reads as light: past the midpoint of Oklab
/// lightness. The two shipped themes sit far either side of it, and a
/// hand-made theme near the middle gets whichever half it's on.
pub fn is_light(bg: Rgba) -> bool {
    palette::rgba_to_oklch(bg).0 > 0.5
}

fn linear(color: Rgba) -> [f32; 3] {
    let channel = |c: f32| {
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    [channel(color.r), channel(color.g), channel(color.b)]
}

/// A pass's full source: the grade's helpers, then the pass's own body,
/// which calls `grade(rgb)` on the sampled frame.
pub fn wgsl(body: &str) -> String {
    format!("{WGSL}\n{body}")
}

/// The helpers every Milkdrop pass gets in scope. Oklab both ways, a gamut
/// fit, the album tint's hue turn, and `grade`, which reads the slots
/// [`Grade::write`] fills.
///
/// The gamut fit is the same trade `palette::oklch_to_rgba` makes:
/// lightness and hue are the promise, chroma is the budget. A colour that
/// has no sRGB pixel gives chroma back through a few bisection steps until
/// it fits, and only pixels that need it pay for the loop.
pub const WGSL: &str = "
fn linear_to_oklab(rgb: vec3<f32>) -> vec3<f32> {
    let l = dot(rgb, vec3<f32>(0.4122214708, 0.5363325363, 0.0514459929));
    let m = dot(rgb, vec3<f32>(0.2119034982, 0.6806995451, 0.1073969566));
    let s = dot(rgb, vec3<f32>(0.0883024619, 0.2817188376, 0.6299787005));
    let root = pow(max(vec3<f32>(l, m, s), vec3<f32>(0.0)), vec3<f32>(1.0 / 3.0));
    return vec3<f32>(
        dot(root, vec3<f32>(0.2104542553, 0.7936177850, -0.0040720468)),
        dot(root, vec3<f32>(1.9779984951, -2.4285922050, 0.4505937099)),
        dot(root, vec3<f32>(0.0259040371, 0.7827717662, -0.8086757660)),
    );
}

fn oklab_to_linear(lab: vec3<f32>) -> vec3<f32> {
    let l = lab.x + 0.3963377774 * lab.y + 0.2158037573 * lab.z;
    let m = lab.x - 0.1055613458 * lab.y - 0.0638541728 * lab.z;
    let s = lab.x - 0.0894841775 * lab.y - 1.2914855480 * lab.z;
    let cubed = vec3<f32>(l * l * l, m * m * m, s * s * s);
    return vec3<f32>(
        dot(cubed, vec3<f32>(4.0767416621, -3.3077115913, 0.2309699292)),
        dot(cubed, vec3<f32>(-1.2684380046, 2.6097574011, -0.3413193965)),
        dot(cubed, vec3<f32>(-0.0041960863, -0.7034186147, 1.7076147010)),
    );
}

fn in_gamut(rgb: vec3<f32>) -> bool {
    return all(rgb >= vec3<f32>(-0.0001)) && all(rgb <= vec3<f32>(1.0001));
}

fn fit_gamut(lab: vec3<f32>) -> vec3<f32> {
    var out = oklab_to_linear(lab);
    if (!in_gamut(out)) {
        let chroma = length(lab.yz);
        let direction = select(vec2<f32>(0.0, 0.0), lab.yz / max(chroma, 0.0001), chroma > 0.0001);
        var lo = 0.0;
        var hi = chroma;
        for (var i = 0; i < 5; i++) {
            let mid = (lo + hi) * 0.5;
            if (in_gamut(oklab_to_linear(vec3<f32>(lab.x, direction * mid)))) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        out = oklab_to_linear(vec3<f32>(lab.x, direction * lo));
    }
    return clamp(out, vec3<f32>(0.0), vec3<f32>(1.0));
}

fn turn_hue(rgb: vec3<f32>, hue: f32, amount: f32) -> vec3<f32> {
    if (amount <= 0.0) {
        return rgb;
    }
    let lab = linear_to_oklab(rgb);
    let chroma = length(lab.yz);
    if (chroma <= 0.0001) {
        return rgb;
    }
    let start = atan2(lab.z, lab.y);
    // The short way around: fold the difference into a half turn either
    // side, so a red frame against a magenta cover goes the near way and
    // not the long way through green.
    let apart = hue - start;
    let delta = apart - 6.28318530718 * round(apart / 6.28318530718);
    let turned = start + delta * amount;
    return fit_gamut(vec3<f32>(lab.x, vec2<f32>(cos(turned), sin(turned)) * chroma));
}

// Slot 3 is the mode, 4-6 the theme's root background, 7 the light flag,
// 8-10 the accent, 11-13 the cover's colour; 1 and 2 are the album
// tint's hue and amount.
fn grade(rgb: vec3<f32>) -> vec3<f32> {
    let mode = params.signals[0].w;
    let light = params.signals[1].w;
    var out = rgb;
    if (mode >= 1.5) {
        let lab = linear_to_oklab(rgb);
        let floor = linear_to_oklab(params.signals[1].xyz);
        let cover = vec3<f32>(params.signals[2].w, params.signals[3].x, params.signals[3].y);
        let ceiling = linear_to_oklab(select(params.signals[2].xyz, cover, mode >= 2.5));
        out = fit_gamut(mix(floor, ceiling, clamp(lab.x, 0.0, 1.0)));
    } else if (mode >= 0.5 && light > 0.5) {
        let lab = linear_to_oklab(rgb);
        out = fit_gamut(vec3<f32>(1.0 - lab.x, lab.yz));
    }
    return turn_hue(out, params.signals[0].y, clamp(params.signals[0].z, 0.0, 1.0));
}
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grade_fills_its_own_slots_and_no_others() {
        let grade = Grade::new(
            GradeMode::Palette,
            true,
            gpui::rgb(0xededed),
            gpui::rgb(0xffb300),
            Some(gpui::rgb(0x0000ff)),
        );
        let mut signals = [0.5f32; 16];
        grade.write(&mut signals);
        assert_eq!(signals[SLOT_FADE], 0.5, "the fade is the caller's");
        assert_eq!(signals[SLOT_HUE], 0.5);
        assert_eq!(signals[SLOT_TINT], 0.5);
        assert_eq!(signals[SLOT_MODE], 2.0);
        assert_eq!(signals[SLOT_LIGHT], 1.0);
        // The colours go in linear: sRGB 0xed is well above 0.85 as a
        // fraction and lands lower once decoded.
        assert!(signals[SLOT_BG] > 0.8 && signals[SLOT_BG] < 0.86);
        assert_eq!(signals[SLOT_BG], signals[SLOT_BG + 1]);
        assert!(signals[SLOT_ACCENT] > 0.99, "full red decodes to one");
        assert!(signals[SLOT_ACCENT + 2] < 0.01, "no blue decodes to none");
        assert!(
            signals[SLOT_COVER + 2] > 0.99,
            "the cover's blue lands in its slot"
        );
        assert_eq!(signals[14], 0.5, "the slots past the cover are untouched");
    }

    #[test]
    fn the_theme_mode_is_the_default_and_the_palette_drops_the_tint() {
        assert_eq!(GradeMode::default(), GradeMode::Theme);
        let bg = gpui::rgb(0x121212);
        let accent = gpui::rgb(0xffb300);
        assert!(Grade::new(GradeMode::Theme, false, bg, accent, None).tints());
        assert!(Grade::new(GradeMode::Preset, false, bg, accent, None).tints());
        assert!(!Grade::new(GradeMode::Palette, false, bg, accent, None).tints());
        assert!(!Grade::new(GradeMode::Cover, false, bg, accent, None).tints());
    }

    /// A dark cover colour keeps its hue and takes the accent's
    /// lightness, so the ramp still spans the same contrast Palette's
    /// does.
    #[test]
    fn a_dark_cover_is_lifted_to_the_accents_lightness() {
        let accent = gpui::rgb(0xffb300);
        let cover = gpui::rgb(0x3a2410);
        let top = cover_top(cover, accent);
        let (want_l, _, _) = palette::rgba_to_oklch(accent);
        let (cover_l, _, cover_h) = palette::rgba_to_oklch(cover);
        let (got_l, got_c, got_h) = palette::rgba_to_oklch(top);
        assert!(cover_l < 0.4, "the cover really is dark: {cover_l}");
        assert!(
            (got_l - want_l).abs() < 0.05,
            "lifted to {got_l}, wanted {want_l}"
        );
        assert!(
            (got_h - cover_h).abs() < 0.2,
            "hue kept: {got_h} vs {cover_h}"
        );
        assert!(
            got_c >= COVER_CHROMA_FLOOR - 0.03,
            "chroma floored: {got_c}"
        );
    }

    /// No cover means the cover ramp tops out at the accent, so the mode
    /// degrades to Palette rather than to black.
    #[test]
    fn a_missing_cover_falls_back_to_the_accent() {
        let grade = Grade::new(
            GradeMode::Cover,
            false,
            gpui::rgb(0x121212),
            gpui::rgb(0xffb300),
            None,
        );
        assert_eq!(grade.cover, grade.accent);
    }

    #[test]
    fn the_two_shipped_backgrounds_land_either_side_of_light() {
        assert!(!is_light(gpui::rgb(0x121212)));
        assert!(is_light(gpui::rgb(0xededed)));
    }
}
