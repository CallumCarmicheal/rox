//! Windowed-sinc resampler from the track rate to the device rate, interleaved
//! stereo, wrapping rubato's asynchronous sinc engine (ADR 19: this runs on the
//! decode thread, never in the output callback).
//!
//! The engine holds this module to three properties, and everything below is
//! shaped by them.
//!
//! A same-rate stream is bit-exact. `new` short-circuits to a passthrough when
//! the rates match, so rubato never sees a 48-to-48 stream and the bypass rule
//! ADR 19 defines stays checkable: with an empty chain, what the decoder
//! produced is what reaches the ring.
//!
//! The frame count is exact. N input frames at ratio dst/src yield exactly
//! `round(N * dst / src)` output frames once flushed, computed in integer
//! arithmetic so the float ratio can't wobble the answer. That matters at the
//! gapless boundary: a resampler that runs a filter length short at every track
//! end shifts every album seam, and the shift is audible as a click.
//!
//! Flush is idempotent and silent on an empty stream. It also resets, so the
//! wrapper is reusable afterwards.
//!
//! Hitting the exact count needs two corrections a linear interpolator didn't.
//! A sinc filter has group delay, so the first `output_delay()` output frames
//! are the filter warming up on silence and get dropped on the way out. And the
//! filter holds a tail: the last real input frames are still only half-convolved
//! when the input stops. Flush pushes silence through until the output count
//! reaches the target and truncates any surplus, which is what the linear
//! version's single carried frame stood in for.
//!
//! Fixed at two channels: the engine folds to stereo before it gets here.

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{
    Async, FixedAsync, Indexing, Resampler as RubatoResampler, SincInterpolationParameters,
    SincInterpolationType, WindowFunction,
};

/// Input frames handed to rubato per call. `FixedAsync::Input` means this is
/// the fixed side and the output count varies, which is the shape a decoder
/// wants: packets arrive, output falls out.
const CHUNK: usize = 1024;

/// Taps in the interpolation filter. 128 at a Blackman-Harris window is well
/// past transparent for 44.1-to-48, and half the cost of rubato's own 256
/// default.
const SINC_LEN: usize = 128;

/// How many silence chunks flush will push before giving up. Reaching this
/// means rubato stopped producing output, which shouldn't happen; the cap is
/// here so a decode thread can't spin on it.
const FLUSH_CHUNK_LIMIT: usize = 64;

pub struct Resampler {
    src_rate: u32,
    dst_rate: u32,
    inner: Inner,
}

enum Inner {
    /// Rates match, or the filter couldn't be built. Samples pass through
    /// untouched.
    Passthrough,
    Sinc(Box<Sinc>),
}

struct Sinc {
    rs: Async<f32>,
    /// Input frames held back because they don't fill a chunk yet. Never longer
    /// than `CHUNK` frames, so the capacity taken in `new` is the only one.
    chunk: Vec<f32>,
    /// Where rubato writes. Sized for `output_frames_max` up front.
    scratch: Vec<f32>,
    /// Real input frames fed since the last flush.
    consumed: u64,
    /// Output frames handed to the caller since the last flush.
    emitted: u64,
    /// Output frames still to drop off the front: the filter's group delay.
    skip: usize,
}

impl Resampler {
    pub fn new(src_rate: u32, dst_rate: u32) -> Self {
        let inner = if src_rate == dst_rate || src_rate == 0 || dst_rate == 0 {
            Inner::Passthrough
        } else {
            Self::build(src_rate, dst_rate)
        };
        Resampler {
            src_rate,
            dst_rate,
            inner,
        }
    }

    fn build(src_rate: u32, dst_rate: u32) -> Inner {
        let params = SincInterpolationParameters {
            sinc_len: SINC_LEN,
            // Let rubato pick the anti-alias cutoff from the ratio and the
            // window; it picks the highest one that keeps aliasing under the
            // window's sidelobes, which is what we'd be aiming at by hand.
            f_cutoff: None,
            oversampling_factor: 256,
            interpolation: SincInterpolationType::Cubic,
            window: WindowFunction::BlackmanHarris2,
        };
        // A relative ratio of 1.0 because the ratio never moves. Nothing here
        // corrects drift against the device clock; a rate change is a new
        // resampler, which is what the engine already does.
        let rs = match Async::<f32>::new_sinc(
            f64::from(dst_rate) / f64::from(src_rate),
            1.0,
            &params,
            CHUNK,
            2,
            FixedAsync::Input,
        ) {
            Ok(rs) => rs,
            Err(e) => {
                // Only reachable on a nonsense rate. Passing the samples
                // through plays the track at the wrong speed, which is wrong
                // but audible and recoverable; panicking on the decode thread
                // isn't.
                log::error!("resampler {src_rate} -> {dst_rate} could not be built: {e}");
                return Inner::Passthrough;
            }
        };
        let skip = rs.output_delay();
        let scratch = vec![0.0; rs.output_frames_max() * 2];
        Inner::Sinc(Box::new(Sinc {
            rs,
            chunk: Vec::with_capacity(CHUNK * 2),
            scratch,
            consumed: 0,
            emitted: 0,
            skip,
        }))
    }

    pub fn src_rate(&self) -> u32 {
        self.src_rate
    }

    /// Resample one interleaved stereo chunk, appending to `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        let Inner::Sinc(s) = &mut self.inner else {
            out.extend_from_slice(input);
            return;
        };

        let n_in = input.len() / 2;
        if n_in == 0 {
            return;
        }
        s.consumed += n_in as u64;

        // Walk the input in whole chunks. When nothing is held back, the
        // chunk is a window onto the caller's own slice and no copy happens
        // at all; only the ragged head and tail ever touch `chunk`.
        let mut pos = 0;
        loop {
            let held = s.chunk.len() / 2;
            if held + (n_in - pos) < CHUNK {
                break;
            }
            if held == 0 {
                let end = pos + CHUNK;
                s.feed(&input[pos * 2..end * 2], None, out);
                pos = end;
            } else {
                let take = CHUNK - held;
                s.chunk.extend_from_slice(&input[pos * 2..(pos + take) * 2]);
                pos += take;
                let full = std::mem::take(&mut s.chunk);
                s.feed(&full, None, out);
                s.chunk = full;
                s.chunk.clear();
            }
        }
        s.chunk.extend_from_slice(&input[pos * 2..]);
    }

    /// End of stream: push the filter's tail out, land on the exact output
    /// frame count the ratio calls for, and reset. Without this the last
    /// fraction of a second of every track goes missing when src != device
    /// rate, and every gapless boundary drifts by a filter length.
    ///
    /// Idempotent: a second flush emits nothing, because the first one left
    /// the wrapper looking like a fresh stream.
    pub fn flush(&mut self, out: &mut Vec<f32>) {
        let Inner::Sinc(s) = &mut self.inner else {
            return;
        };
        if s.consumed == 0 {
            s.chunk.clear();
            return;
        }

        // Integer round-half-up, so 44100 frames at 44.1k into 48k is 48000
        // and not 47999 on a ratio that doesn't divide.
        let src = u128::from(self.src_rate);
        let target = ((u128::from(s.consumed) * u128::from(self.dst_rate) + src / 2) / src) as u64;
        let start = out.len();

        // The ragged tail first. rubato reads the real frames and treats the
        // rest of the chunk as silence.
        let tail = std::mem::take(&mut s.chunk);
        s.feed(&tail, Some(tail.len() / 2), out);
        s.chunk = tail;

        // Then silence, until the count catches up. `chunk` is empty and its
        // capacity is already CHUNK frames, so zeroing it costs no allocation.
        s.chunk.clear();
        s.chunk.resize(CHUNK * 2, 0.0);
        let zeros = std::mem::take(&mut s.chunk);
        let mut pushed = 0;
        while s.emitted < target && pushed < FLUSH_CHUNK_LIMIT {
            s.feed(&zeros, None, out);
            pushed += 1;
        }
        if s.emitted < target {
            log::warn!(
                "resampler flush stalled at {} of {target} frames",
                s.emitted
            );
        }
        s.chunk = zeros;
        s.chunk.clear();

        // Silence overshoots by up to a chunk; trim back to the exact count,
        // never past what this flush appended.
        if s.emitted > target {
            let surplus = (((s.emitted - target) as usize) * 2).min(out.len() - start);
            out.truncate(out.len() - surplus);
        }

        s.reset_state();
    }

    /// Re-arm for a new stream at the same rates without rebuilding the sinc
    /// table. Construction computes SINC_LEN * oversampling_factor filter
    /// coefficients; a seek doesn't need them recomputed, only the filter
    /// history cleared.
    pub fn reset(&mut self) {
        if let Inner::Sinc(s) = &mut self.inner {
            s.reset_state();
        }
    }
}

impl Sinc {
    /// Hand rubato one input chunk and append the aligned output to `out`.
    /// `partial` gives the count of real frames when the chunk is short of
    /// `CHUNK`; rubato reads those and pads the rest with silence.
    fn feed(&mut self, input: &[f32], partial: Option<usize>, out: &mut Vec<f32>) {
        let Sinc {
            rs,
            scratch,
            emitted,
            skip,
            ..
        } = self;

        let need = rs.output_frames_next();
        if scratch.len() < need * 2 {
            scratch.resize(need * 2, 0.0);
        }
        let frames_in = partial.unwrap_or(CHUNK);

        let src = match InterleavedSlice::new(input, 2, frames_in) {
            Ok(src) => src,
            Err(e) => {
                log::error!("resampler input buffer rejected: {e}");
                return;
            }
        };
        let mut dst = match InterleavedSlice::new_mut(scratch.as_mut_slice(), 2, need) {
            Ok(dst) => dst,
            Err(e) => {
                log::error!("resampler output buffer rejected: {e}");
                return;
            }
        };
        let indexing = partial.map(|n| Indexing::new().partial_len(n));
        let written = match rs.process_into_buffer(&src, &mut dst, indexing.as_ref()) {
            Ok((_, written)) => written,
            Err(e) => {
                log::error!("resample failed: {e}");
                return;
            }
        };

        // The head of the very first output is the filter convolving against
        // its own zeroed history. Drop exactly that much and the output lines
        // up with the input in time.
        let dropped = (*skip).min(written);
        *skip -= dropped;
        out.extend_from_slice(&scratch[dropped * 2..written * 2]);
        *emitted += (written - dropped) as u64;
    }

    fn reset_state(&mut self) {
        self.rs.reset();
        self.chunk.clear();
        self.consumed = 0;
        self.emitted = 0;
        self.skip = self.rs.output_delay();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One second of a sine on the left channel, its inverse on the right.
    fn sine(frames: usize, freq: f64, rate: f64) -> Vec<f32> {
        let mut v = Vec::with_capacity(frames * 2);
        for n in 0..frames {
            let s = (std::f64::consts::TAU * freq * n as f64 / rate).sin() as f32;
            v.push(s);
            v.push(-s);
        }
        v
    }

    /// Fixed-seed LCG, so "random chunk sizes" means the same sizes on every
    /// run and a failure is reproducible.
    struct Lcg(u64);

    impl Lcg {
        fn upto(&mut self, hi: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 33) as usize % hi + 1
        }
    }

    /// A passthrough (src == dst) copies the input verbatim and flush adds
    /// nothing. Bit-exact, no interpolation path touched.
    #[test]
    fn passthrough_is_bit_exact() {
        let mut r = Resampler::new(48000, 48000);
        let input = vec![0.1, -0.2, 0.3, -0.4, 0.5, -0.6];
        let mut out = Vec::new();
        r.process(&input, &mut out);
        r.flush(&mut out);
        assert_eq!(out, input);
    }

    /// Flush is a no-op when the resampler never saw a frame, whatever the
    /// ratio, so a track that decoded nothing doesn't emit phantom samples.
    #[test]
    fn flush_without_input_emits_nothing() {
        let mut r = Resampler::new(44100, 48000);
        let mut out = Vec::new();
        r.flush(&mut out);
        assert!(out.is_empty());
    }

    /// Upsampling 2x: the filter's tail is still inside rubato when the input
    /// stops, so before flush the output is short of the frames the ratio
    /// calls for. Flush pushes silence through until it isn't. A sinc doesn't
    /// reproduce input samples verbatim, so this is a count claim, not a
    /// value one.
    #[test]
    fn flush_emits_final_frame_on_upsample() {
        let mut r = Resampler::new(24000, 48000);
        // Two stereo frames: (0, 0) and (1, -1).
        let input = vec![0.0, 0.0, 1.0, -1.0];
        let mut out = Vec::new();
        r.process(&input, &mut out);
        assert!(
            out.len() / 2 < 4,
            "the filter tail should still be held before flush, got {} frames",
            out.len() / 2
        );

        r.flush(&mut out);
        assert_eq!(out.len() / 2, 4, "flush must land on the exact frame count");
    }

    /// A known upsample produces the exact number of output frames. Feeding N
    /// source frames at ratio dst/src and flushing yields round(N * ratio),
    /// which is what the gapless boundary is measured against.
    #[test]
    fn upsample_frame_count_is_exact() {
        let mut r = Resampler::new(24000, 48000);
        // Ten source frames, left = index, right = -index.
        let mut input = Vec::new();
        for i in 0..10 {
            input.push(i as f32);
            input.push(-(i as f32));
        }
        let mut out = Vec::new();
        r.process(&input, &mut out);
        r.flush(&mut out);
        assert_eq!(out.len() / 2, 20);
    }

    /// Downsampling drops frames (decimation), so the exact last source frame
    /// need not appear; this checks that flush is safe to call and leaves the
    /// resampler reusable.
    #[test]
    fn flush_is_idempotent() {
        let mut r = Resampler::new(24000, 48000);
        let input = vec![0.0, 0.0, 1.0, 1.0];
        let mut out = Vec::new();
        r.process(&input, &mut out);
        r.flush(&mut out);
        let after_first = out.len();
        r.flush(&mut out);
        assert_eq!(out.len(), after_first, "second flush emits nothing");
    }

    /// The count holds over a real-length run at the ratio most of a CD-ripped
    /// library plays at, and it holds no matter how the decoder chops the
    /// input up. One second in, one second out.
    #[test]
    fn ratio_is_exact_over_a_long_run() {
        let input = sine(44100, 1000.0, 44100.0);
        let mut r = Resampler::new(44100, 48000);
        let mut out = Vec::new();
        let mut rng = Lcg(0x5EED);
        let mut pos = 0;
        while pos < 44100 {
            let n = rng.upto(4096).min(44100 - pos);
            r.process(&input[pos * 2..(pos + n) * 2], &mut out);
            pos += n;
        }
        r.flush(&mut out);
        assert_eq!(out.len() / 2, 48000);
    }

    /// The tone survives the trip. A 1 kHz sine into 48 kHz still crosses zero
    /// 2000 times a second and still peaks where it started, which a resampler
    /// with a wrong cutoff or a wrong ratio wouldn't manage.
    #[test]
    fn frequency_and_amplitude_survive() {
        let input = sine(44100, 1000.0, 44100.0);
        let mut r = Resampler::new(44100, 48000);
        let mut out = Vec::new();
        r.process(&input, &mut out);
        r.flush(&mut out);

        let left: Vec<f32> = out.iter().step_by(2).copied().collect();
        let crossings = left
            .windows(2)
            .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
            .count();
        // Two crossings per cycle, 1000 cycles.
        assert!(
            (1980..=2020).contains(&crossings),
            "expected ~2000 zero crossings, got {crossings}"
        );

        let peak_in = input.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        let peak_out = left.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(
            (peak_out - peak_in).abs() / peak_in < 0.02,
            "peak moved from {peak_in} to {peak_out}"
        );
    }

    /// How the decoder chops the input doesn't change a single sample of the
    /// output. The wrapper holds partial chunks back and hands rubato the same
    /// sequence of full chunks either way, so this is bit-exact, not
    /// approximate.
    #[test]
    fn chunking_does_not_change_the_output() {
        let input = sine(3000, 440.0, 44100.0);

        let mut whole = Vec::new();
        let mut r = Resampler::new(44100, 48000);
        r.process(&input, &mut whole);
        r.flush(&mut whole);

        let mut dripped = Vec::new();
        let mut r = Resampler::new(44100, 48000);
        for frame in input.chunks(2) {
            r.process(frame, &mut dripped);
        }
        r.flush(&mut dripped);

        assert_eq!(whole, dripped);
    }

    /// Downsampling lands on the exact count too. Halving the rate halves the
    /// frames, with no filter tail left behind.
    #[test]
    fn downsample_frame_count_is_exact() {
        let input = sine(9600, 1000.0, 96000.0);
        let mut r = Resampler::new(96000, 48000);
        let mut out = Vec::new();
        r.process(&input, &mut out);
        r.flush(&mut out);
        assert_eq!(out.len() / 2, 4800);
    }

    /// Reset puts the wrapper back where a fresh one starts, which is what a
    /// seek needs. Same input through a reset instance and a new one produces
    /// the same samples.
    #[test]
    fn reset_matches_a_fresh_resampler() {
        let input = sine(2000, 440.0, 44100.0);

        let mut reused = Resampler::new(44100, 48000);
        let mut scratch = Vec::new();
        reused.process(&input, &mut scratch);
        reused.flush(&mut scratch);
        reused.reset();

        let mut after_reset = Vec::new();
        reused.process(&input, &mut after_reset);
        reused.flush(&mut after_reset);

        let mut fresh = Resampler::new(44100, 48000);
        let mut expected = Vec::new();
        fresh.process(&input, &mut expected);
        fresh.flush(&mut expected);

        assert_eq!(after_reset, expected);
    }
}
