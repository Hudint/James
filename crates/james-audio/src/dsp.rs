//! Signal processing: loudness normalisation, volume and true-peak limiting.

use std::collections::VecDeque;

use james_core::audio::{CHANNELS, FRAME_LEN, FRAME_SAMPLES, SAMPLE_RATE};

pub fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

pub fn gain_to_db(gain: f32) -> f32 {
    20.0 * gain.max(1e-9).log10()
}

/// Maps a user-facing volume (0–100) to a linear gain with a perceptual curve.
pub fn volume_to_gain(volume: u8) -> f32 {
    let v = f32::from(volume.min(100)) / 100.0;
    v * v
}

/// Applies a gain that ramps linearly across the frame to avoid zipper noise.
#[derive(Debug, Clone)]
pub struct GainRamp {
    current: f32,
}

impl GainRamp {
    pub fn new(gain: f32) -> Self {
        Self { current: gain }
    }

    pub fn process(&mut self, frame: &mut [f32; FRAME_LEN], target: f32) {
        let start = self.current;
        let step = (target - start) / FRAME_SAMPLES as f32;
        for (i, pair) in frame.chunks_exact_mut(CHANNELS).enumerate() {
            let gain = start + step * (i + 1) as f32;
            for sample in pair {
                *sample *= gain;
            }
        }
        self.current = target;
    }
}

/// Gain limits so silence or very quiet tracks are not boosted absurdly.
const MAX_BOOST_DB: f32 = 12.0;
const MAX_CUT_DB: f32 = -24.0;
/// The gain is re-evaluated every 100 ms (5 frames), matching the meter's block size.
const UPDATE_FRAMES: u64 = 5;
/// Boosting starts only after this much audio was measured, so quiet intros are not over-boosted.
const BOOST_HOLD_FRAMES: u64 = 150;
/// Maximum gain change per update (100 ms): cuts react fast, boosts slowly.
const CUT_STEP_FAST_DB: f32 = 1.0;
const CUT_STEP_DB: f32 = 0.3;
const BOOST_STEP_DB: f32 = 0.1;

/// Loudness normalisation towards a target level (EBU R128 / LUFS).
///
/// Uses the gated integrated loudness measured so far. Cuts are applied quickly
/// (a loud track must not clip into the limiter for long), boosts slowly and only
/// after 3 s of audio, so quiet intros are not pumped up. If the loudness of a
/// track is already known (cache from an earlier play), the gain is correct from
/// the first sample and stays fixed.
pub struct Normalizer {
    meter: ebur128::EbuR128,
    target_lufs: f32,
    known_lufs: Option<f32>,
    gain_db: f32,
    elapsed_frames: u64,
    ramp: GainRamp,
}

impl Normalizer {
    pub fn new(target_lufs: f32, known_lufs: Option<f32>) -> Self {
        // HISTOGRAM keeps memory and per-query cost constant regardless of track length.
        let meter = ebur128::EbuR128::new(
            CHANNELS as u32,
            SAMPLE_RATE,
            ebur128::Mode::I | ebur128::Mode::HISTOGRAM,
        )
        .expect("valid ebur128 parameters");
        let gain_db = known_lufs
            .map(|l| (target_lufs - l).clamp(MAX_CUT_DB, MAX_BOOST_DB))
            .unwrap_or(0.0);
        Self {
            meter,
            target_lufs,
            known_lufs,
            gain_db,
            elapsed_frames: 0,
            ramp: GainRamp::new(db_to_gain(gain_db)),
        }
    }

    /// Gated integrated loudness of everything seen so far (useful for caching).
    pub fn measured_lufs(&self) -> Option<f32> {
        self.meter
            .loudness_global()
            .ok()
            .filter(|l| l.is_finite())
            .map(|l| l as f32)
    }

    pub fn gain_db(&self) -> f32 {
        self.gain_db
    }

    pub fn process(&mut self, frame: &mut [f32; FRAME_LEN]) {
        let _ = self.meter.add_frames_f32(frame);
        self.elapsed_frames += 1;
        if self.known_lufs.is_none() && self.elapsed_frames % UPDATE_FRAMES == 0 {
            self.update_gain();
        }
        self.ramp.process(frame, db_to_gain(self.gain_db));
    }

    fn update_gain(&mut self) {
        let Some(measured) = self.measured_lufs() else {
            return;
        };
        let wanted = (self.target_lufs - measured).clamp(MAX_CUT_DB, MAX_BOOST_DB);
        let delta = wanted - self.gain_db;
        let step = if delta < 0.0 {
            let max = if self.elapsed_frames < BOOST_HOLD_FRAMES {
                CUT_STEP_FAST_DB
            } else {
                CUT_STEP_DB
            };
            delta.max(-max)
        } else if self.elapsed_frames >= BOOST_HOLD_FRAMES {
            delta.min(BOOST_STEP_DB)
        } else {
            0.0
        };
        self.gain_db += step;
    }
}

/// Look-ahead of the limiter (5 ms).
const LOOKAHEAD: usize = (SAMPLE_RATE as usize * 5) / 1000;

/// Estimates inter-sample (true) peaks with 4× oversampling by Catmull-Rom
/// interpolation, per channel. Slightly underestimates compared to the ITU
/// BS.1770 polyphase filter; the limiter ceiling leaves headroom for that.
#[derive(Default)]
struct TruePeak {
    history: [[f32; 4]; CHANNELS],
}

impl TruePeak {
    /// Peak of the interval ending at the newest sample.
    fn push(&mut self, pair: &[f32]) -> f32 {
        let mut peak = 0f32;
        for (ch, &sample) in pair.iter().enumerate() {
            let h = &mut self.history[ch];
            h.rotate_left(1);
            h[3] = sample;
            // Interpolate between h[1] and h[2] (one sample of latency is irrelevant for detection).
            let (p0, p1, p2, p3) = (h[0], h[1], h[2], h[3]);
            peak = peak.max(p2.abs());
            for t in [0.25f32, 0.5, 0.75] {
                let t2 = t * t;
                let t3 = t2 * t;
                let v = 0.5
                    * ((2.0 * p1)
                        + (-p0 + p2) * t
                        + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t2
                        + (-p0 + 3.0 * p1 - 3.0 * p2 + p3) * t3);
                peak = peak.max(v.abs());
            }
            peak = peak.max(sample.abs());
        }
        peak
    }
}

/// Look-ahead true-peak limiter: keeps the output below `ceiling` without audible clipping.
///
/// The required gain per sample is run through a sliding minimum and a box filter, both
/// `LOOKAHEAD` samples long. This makes the gain ramp down linearly and reach the required
/// value exactly when the peak leaves the delay line; release is exponential.
pub struct Limiter {
    ceiling: f32,
    detector: TruePeak,
    delay: VecDeque<[f32; CHANNELS]>,
    /// Monotonic deque of (index, required gain) for the sliding minimum.
    minimum: VecDeque<(u64, f32)>,
    /// Last `LOOKAHEAD` sliding-minimum values and their sum (box filter).
    boxcar: VecDeque<f32>,
    boxcar_sum: f64,
    index: u64,
    gain: f32,
    release: f32,
}

impl Limiter {
    /// `ceiling_db` e.g. -1.0 dBTP; release 150 ms.
    pub fn new(ceiling_db: f32) -> Self {
        let release = 1.0 - (-1.0 / (0.150 * SAMPLE_RATE as f32)).exp();
        let mut delay = VecDeque::with_capacity(LOOKAHEAD);
        delay.extend(std::iter::repeat_n([0.0; CHANNELS], LOOKAHEAD - 1));
        let mut boxcar = VecDeque::with_capacity(LOOKAHEAD);
        boxcar.extend(std::iter::repeat_n(1.0, LOOKAHEAD));
        Self {
            ceiling: db_to_gain(ceiling_db),
            detector: TruePeak::default(),
            delay,
            minimum: VecDeque::new(),
            boxcar,
            boxcar_sum: LOOKAHEAD as f64,
            index: 0,
            gain: 1.0,
            release,
        }
    }

    /// Output latency in samples per channel.
    pub fn latency(&self) -> usize {
        LOOKAHEAD - 1
    }

    pub fn process(&mut self, frame: &mut [f32; FRAME_LEN]) {
        for pair in frame.chunks_exact_mut(CHANNELS) {
            let peak = self.detector.push(pair);
            let required = if peak > self.ceiling {
                self.ceiling / peak
            } else {
                1.0
            };

            // Sliding minimum over the last LOOKAHEAD required gains.
            while self.minimum.back().is_some_and(|&(_, g)| g >= required) {
                self.minimum.pop_back();
            }
            self.minimum.push_back((self.index, required));
            while self
                .minimum
                .front()
                .is_some_and(|&(i, _)| i + LOOKAHEAD as u64 <= self.index)
            {
                self.minimum.pop_front();
            }
            let min = self.minimum.front().map_or(1.0, |&(_, g)| g);

            // Box filter turns the step of the minimum into a linear ramp.
            self.boxcar_sum += f64::from(min) - f64::from(self.boxcar.pop_front().unwrap_or(1.0));
            self.boxcar.push_back(min);
            let smoothed = (self.boxcar_sum / LOOKAHEAD as f64) as f32;

            self.gain = smoothed.min(self.gain + (1.0 - self.gain) * self.release);

            self.delay.push_back([pair[0], pair[1]]);
            let delayed = self.delay.pop_front().expect("delay line is pre-filled");
            for (out, sample) in pair.iter_mut().zip(delayed) {
                // Hard clip as a last resort against interpolation error.
                *out = (sample * self.gain).clamp(-1.0, 1.0);
            }
            self.index += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine_frames(amplitude: f32, freq: f32, frames: usize) -> Vec<[f32; FRAME_LEN]> {
        let mut t = 0usize;
        (0..frames)
            .map(|_| {
                let mut frame = [0.0; FRAME_LEN];
                for pair in frame.chunks_exact_mut(2) {
                    let s = (t as f32 * freq * std::f32::consts::TAU / SAMPLE_RATE as f32).sin() * amplitude;
                    pair[0] = s;
                    pair[1] = s;
                    t += 1;
                }
                frame
            })
            .collect()
    }

    fn loudness(frames: &[[f32; FRAME_LEN]]) -> f32 {
        let mut meter = ebur128::EbuR128::new(2, SAMPLE_RATE, ebur128::Mode::I).unwrap();
        for frame in frames {
            meter.add_frames_f32(frame).unwrap();
        }
        meter.loudness_global().unwrap() as f32
    }

    #[test]
    fn limiter_keeps_true_peaks_below_ceiling() {
        let ceiling = db_to_gain(-1.0);
        // High frequency content has inter-sample peaks above the sample values.
        for freq in [1000.0, 11_025.0] {
            let mut limiter = Limiter::new(-1.0);
            let mut detector = TruePeak::default();
            let mut max = 0f32;
            for mut frame in sine_frames(2.0, freq, 100) {
                limiter.process(&mut frame);
                for pair in frame.chunks_exact(2) {
                    max = max.max(detector.push(pair));
                }
            }
            assert!(
                max <= ceiling * 1.01,
                "{freq} Hz: true peak {max} > ceiling {ceiling}"
            );
            assert!(max > ceiling * 0.85, "{freq} Hz: over-attenuated ({max})");
        }
    }

    #[test]
    fn limiter_ramps_smoothly_on_transients() {
        let mut limiter = Limiter::new(-1.0);
        // Quiet signal followed by a sudden burst far above the ceiling.
        let mut input = sine_frames(0.1, 200.0, 5);
        input.extend(sine_frames(1.5, 200.0, 5));
        let mut output = input.clone();
        for frame in &mut output {
            limiter.process(frame);
        }
        let flat_in: Vec<f32> = input.iter().flatten().step_by(2).copied().collect();
        let flat_out: Vec<f32> = output.iter().flatten().step_by(2).copied().collect();
        let delay = limiter.latency();
        // Effective gain per sample where the input is not near a zero crossing.
        let gains: Vec<(usize, f32)> = (0..flat_in.len() - delay)
            .filter(|&i| flat_in[i].abs() > 0.05)
            .map(|i| (i, flat_out[i + delay] / flat_in[i]))
            .collect();
        let quiet_end = 5 * FRAME_SAMPLES - LOOKAHEAD;
        assert!(
            gains
                .iter()
                .filter(|(i, _)| *i < quiet_end)
                .all(|(_, g)| (g - 1.0).abs() < 1e-4),
            "quiet part before the look-ahead window must be untouched"
        );
        // The gain falls along a linear ramp: the slope never exceeds the ramp slope.
        let depth = 1.0 - db_to_gain(-1.0) / 1.5;
        let max_slope = 1.5 * depth / LOOKAHEAD as f32;
        for pair in gains.windows(2) {
            let ((i0, g0), (i1, g1)) = (pair[0], pair[1]);
            let slope = (g0 - g1) / (i1 - i0) as f32;
            assert!(
                slope <= max_slope,
                "gain slope {slope} > {max_slope} at sample {i1}"
            );
        }
        let min_gain = gains.iter().map(|(_, g)| *g).fold(1.0f32, f32::min);
        assert!(min_gain < 0.65, "burst must be limited (min gain {min_gain})");
    }

    #[test]
    fn limiter_is_transparent_below_ceiling() {
        let mut limiter = Limiter::new(-1.0);
        let input = sine_frames(0.5, 1000.0, 10);
        let mut output = input.clone();
        for frame in &mut output {
            limiter.process(frame);
        }
        let flat_in: Vec<f32> = input.iter().flatten().copied().collect();
        let flat_out: Vec<f32> = output.iter().flatten().copied().collect();
        let delay = limiter.latency() * CHANNELS;
        for i in 0..flat_in.len() - delay {
            assert!((flat_in[i] - flat_out[i + delay]).abs() < 1e-6);
        }
    }

    #[test]
    fn normalizer_converges_to_target() {
        let mut normalizer = Normalizer::new(-18.0, None);
        let mut frames = sine_frames(0.05, 1000.0, 1500); // quiet sine (~ -29 LUFS), 30 s
        for frame in &mut frames {
            normalizer.process(frame);
        }
        let out = loudness(&frames[1000..]);
        assert!((out + 18.0).abs() < 0.5, "output loudness {out} LUFS");
    }

    #[test]
    fn normalizer_does_not_overboost_quiet_intros() {
        let mut normalizer = Normalizer::new(-18.0, None);
        let mut intro = sine_frames(0.01, 1000.0, 250); // 5 s at ~ -43 LUFS
        for frame in &mut intro {
            normalizer.process(frame);
        }
        assert!(
            normalizer.gain_db() <= 2.5,
            "boosted {} dB during intro",
            normalizer.gain_db()
        );
        let mut loud = sine_frames(0.7, 1000.0, 100); // loud part (~ -6 LUFS)
        for frame in &mut loud {
            normalizer.process(frame);
        }
        assert!(
            normalizer.gain_db() < 0.0,
            "loud part must be cut quickly, gain {}",
            normalizer.gain_db()
        );
    }

    #[test]
    fn normalizer_uses_known_loudness_immediately() {
        let normalizer = Normalizer::new(-18.0, Some(-10.0));
        assert!((normalizer.gain_db() + 8.0).abs() < 1e-6);
    }

    #[test]
    fn volume_curve() {
        assert_eq!(volume_to_gain(100), 1.0);
        assert_eq!(volume_to_gain(0), 0.0);
        assert!((gain_to_db(volume_to_gain(50)) + 12.04).abs() < 0.1);
    }
}
