//! The fade a visual surface runs when the audio stops and starts again.
//!
//! It lives here because more than one surface needs the same curve: the
//! Milkdrop panel fades its frame down to the panel background, and the
//! app-wide Milkdrop backdrop fades its frame down to the blurred cover
//! under it. Both write premultiplied alpha into a shader pass, so both
//! want the same shaping and the same "where is the fade right now"
//! arithmetic, and two copies of that would drift.
//!
//! Time in, opacity out, nothing else. Nothing here touches a window, so
//! the curve is testable without one and the result doesn't depend on how
//! often a view happens to be re-rendered.

use std::time::{Duration, Instant};

/// Where a fade started, where it's going, and when it began. `from` and
/// `to` being equal is a settled fade, which is most of a surface's life.
#[derive(Clone, Copy)]
pub struct Fade {
    pub from: f32,
    pub to: f32,
    pub since: Instant,
}

impl Default for Fade {
    /// Settled and fully drawn. A surface comes up visible and the first
    /// tick with nothing playing starts the fade out.
    fn default() -> Self {
        Fade {
            from: 1.0,
            to: 1.0,
            since: Instant::now(),
        }
    }
}

impl Fade {
    /// Settled at `at`, so a surface can come up already gone.
    pub fn settled(at: f32) -> Fade {
        Fade {
            from: at,
            to: at,
            since: Instant::now(),
        }
    }

    /// Where the fade stands right now: 1.0 fully drawn, 0.0 fully gone.
    pub fn opacity(&self, duration: Duration) -> f32 {
        opacity(self.since.elapsed(), duration, self.from, self.to)
    }

    /// Whether the fade is still moving. What keeps a surface asking for
    /// frames after the event that started the fade is long gone.
    pub fn running(&self, duration: Duration) -> bool {
        self.from != self.to && self.since.elapsed() < duration
    }

    /// Head for an opacity, starting from wherever the current fade got
    /// to. A stop half a second into a fade-in turns around there rather
    /// than snapping back to full and dropping.
    pub fn retarget(&mut self, to: f32, duration: Duration) {
        if self.to == to {
            return;
        }
        *self = Fade {
            from: self.opacity(duration),
            to,
            since: Instant::now(),
        };
    }
}

/// The opacity a fade is at: a straight ramp from `from` to `to` across
/// `duration`, pinned at `to` once the time is up. A zero-length fade is
/// already over, which is what makes a "no fade" setting a cut.
pub fn opacity(elapsed: Duration, duration: Duration, from: f32, to: f32) -> f32 {
    if duration.is_zero() {
        return to;
    }
    let progress = (elapsed.as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0);
    from + (to - from) * progress
}

/// The shaping exponent that turns the straight ramp into an even-looking
/// fade, derived rather than picked.
///
/// A user shader chain writes into the swapchain's own format, which blade
/// hands back as `Bgra8Unorm` under `ColorSpace::Srgb`: no hardware encode,
/// so the numbers a pass writes are already gamma-encoded, the same space a
/// `bg()` quad's palette colour goes out in. A blend there moves the code
/// value linearly, and displayed luminance goes as roughly the 2.2 power of
/// a code value while perceived lightness goes as the cube root of
/// luminance. So a mix of `m` reads as brightness `m^(2.2/3)`, which is
/// steepest at the bottom: a straight ramp holds bright for most of its
/// travel and then dumps the rest, which is the plop. Raising the ramp to
/// `3/2.2` cancels that exactly and the perceived brightness falls at a
/// constant rate.
pub const SHAPE: f32 = 3.0 / 2.2;

/// The straight opacity ramp shaped into an even-looking fade. Both ends
/// are fixed points, so the bottom and the full-brightness top are exactly
/// where they were.
///
/// Near-black backgrounds are what the derivation assumes, which is every
/// stock theme; over a pale one the composite never gets far from the
/// background's own lightness and the shaping matters less either way.
pub fn mix(opacity: f32) -> f32 {
    // `max` after the clamp is the NaN guard: clamp passes a NaN straight
    // through, `powf` on one stays NaN, and a NaN in the uniform block is
    // a surface that renders nothing. `max` takes the other operand.
    opacity.clamp(0.0, 1.0).max(0.0).powf(SHAPE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ramp_runs_on_the_clock_and_stops_at_its_target() {
        let second = Duration::from_secs(1);

        // Out: full at the start, half way across, gone at the end.
        assert_eq!(opacity(Duration::ZERO, second, 1.0, 0.0), 1.0);
        assert_eq!(opacity(Duration::from_millis(500), second, 1.0, 0.0), 0.5);
        assert_eq!(opacity(second, second, 1.0, 0.0), 0.0);
        // A surface that wasn't rendered for a while comes back settled
        // rather than somewhere past the end of the ramp.
        assert_eq!(opacity(Duration::from_secs(30), second, 1.0, 0.0), 0.0);

        // In is the same ramp the other way.
        assert_eq!(opacity(Duration::ZERO, second, 0.0, 1.0), 0.0);
        assert_eq!(opacity(Duration::from_millis(250), second, 0.0, 1.0), 0.25);
        assert_eq!(opacity(second, second, 0.0, 1.0), 1.0);

        // A fade that turned around part way runs from where it was.
        let turned = opacity(Duration::from_millis(500), second, 0.4, 1.0);
        assert!((turned - 0.7).abs() < 1e-6, "half way from 0.4 to 1.0");

        // Zero length is already over, in both directions: that's the
        // "no fade" setting, and it has to be a cut rather than a
        // division by zero.
        assert_eq!(opacity(Duration::ZERO, Duration::ZERO, 1.0, 0.0), 0.0);
        assert_eq!(opacity(Duration::ZERO, Duration::ZERO, 0.0, 1.0), 1.0);
    }

    /// The shaping the shader mixes with: same endpoints as the straight
    /// ramp, so the bottom and the full-brightness top are untouched,
    /// and everything in between pulled down.
    #[test]
    fn the_shaping_holds_both_ends_and_bends_the_middle() {
        assert_eq!(mix(0.0), 0.0);
        assert_eq!(mix(1.0), 1.0);

        // Under the straight line everywhere in between: the frame gives
        // up its brightness earlier, which is what stops the last stretch
        // carrying the whole visible change.
        for step in 1..10 {
            let ramp = step as f32 / 10.0;
            let mixed = mix(ramp);
            assert!(mixed < ramp, "{ramp} shaped to {mixed}");
            assert!(mixed > 0.0);
        }

        // Monotone, so a fade never doubles back.
        let mut last = mix(0.0);
        for step in 1..=100 {
            let mixed = mix(step as f32 / 100.0);
            assert!(mixed > last, "step {step}");
            last = mixed;
        }

        // Out-of-range input clamps rather than going imaginary: powf on a
        // negative base with a fractional exponent is NaN, and a NaN in the
        // uniform block is a surface that renders nothing.
        assert_eq!(mix(-1.0), 0.0);
        assert_eq!(mix(2.0), 1.0);
        assert!(mix(f32::NAN).is_finite());

        // Half the fade's travel reads as half the brightness, which is
        // the whole point of the exponent: raising the shaped mix by the
        // perceptual power comes back to the straight ramp.
        for step in 1..10 {
            let ramp = step as f32 / 10.0;
            let perceived = mix(ramp).powf(2.2 / 3.0);
            assert!(
                (perceived - ramp).abs() < 1e-5,
                "{ramp} read as {perceived}"
            );
        }
    }

    #[test]
    fn a_turnaround_starts_where_the_last_fade_got_to() {
        let second = Duration::from_secs(1);
        let mut fade = Fade::settled(1.0);
        fade.retarget(0.0, second);
        assert!(fade.running(second));
        // Retargeting to the same place is a no-op, so a tick that repeats
        // the current intent doesn't restart the clock.
        let since = fade.since;
        fade.retarget(0.0, second);
        assert_eq!(fade.since, since);
        fade.retarget(1.0, second);
        assert!(fade.from < 1.0, "turned around at {}", fade.from);
    }
}
