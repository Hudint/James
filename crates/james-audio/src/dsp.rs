//! Signal processing: loudness normalisation, volume and peak limiting.

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

/// Loudness normalisation towards a target level (EBU R128 / LUFS).
///
/// Uses the integrated loudness measured so far, so the gain converges within
/// the first seconds and then stays stable (no pumping). If the loudness of a
/// track is already known (pre-analysis cache), the gain is correct from the
/// first sample.
pub struct Normalizer {
    meter: ebur128::EbuR128,
    target_lufs: f32,
    known_lufs: Option<f32>,
    gain_db: f32,
    elapsed_frames: u64,
    ramp: GainRamp,
}

/// Gain limits so silence or very quiet tracks are not boosted absurdly.
const MAX_BOOST_DB: f32 = 12.0;
const MAX_CUT_DB: f32 = -24.0;

impl Normalizer {
    pub fn new(target_lufs: f32, known_lufs: Option<f32>) -> Self {
        let meter = ebur128::EbuR128::new(CHANNELS as u32, SAMPLE_RATE, ebur128::Mode::I)
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

    /// Integrated loudness of everything seen so far (useful for caching).
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
        if self.known_lufs.is_none() {
            if let Some(measured) = self.measured_lufs() {
                let wanted = (self.target_lufs - measured).clamp(MAX_CUT_DB, MAX_BOOST_DB);
                // Converge quickly during the first 3 s, then at most 1 dB/s.
                let max_step = if self.elapsed_frames < 150 { 1.0 } else { 0.02 };
                self.gain_db += (wanted - self.gain_db).clamp(-max_step, max_step);
            }
        }
        self.ramp.process(frame, db_to_gain(self.gain_db));
    }
}

/// Look-ahead peak limiter: keeps the output below `ceiling` without audible clipping.
pub struct Limiter {
    ceiling: f32,
    lookahead: usize,
    delay: VecDeque<[f32; CHANNELS]>,
    /// Monotonic deque of (sample index, required gain) for the sliding minimum.
    window: VecDeque<(u64, f32)>,
    index: u64,
    gain: f32,
    release: f32,
}

impl Limiter {
    /// `ceiling_db` e.g. -1.0 dBFS, look-ahead 5 ms, release 150 ms.
    pub fn new(ceiling_db: f32) -> Self {
        let lookahead = (SAMPLE_RATE as usize * 5) / 1000;
        let release = 1.0 - (-1.0 / (0.150 * SAMPLE_RATE as f32)).exp();
        let mut delay = VecDeque::with_capacity(lookahead + 1);
        delay.extend(std::iter::repeat_n([0.0; CHANNELS], lookahead));
        Self {
            ceiling: db_to_gain(ceiling_db),
            lookahead,
            delay,
            window: VecDeque::new(),
            index: 0,
            gain: 1.0,
            release,
        }
    }

    pub fn process(&mut self, frame: &mut [f32; FRAME_LEN]) {
        for pair in frame.chunks_exact_mut(CHANNELS) {
            let peak = pair.iter().fold(0f32, |m, s| m.max(s.abs()));
            let required = if peak > self.ceiling {
                self.ceiling / peak
            } else {
                1.0
            };

            while self.window.back().is_some_and(|&(_, g)| g >= required) {
                self.window.pop_back();
            }
            self.window.push_back((self.index, required));
            while self
                .window
                .front()
                .is_some_and(|&(i, _)| i + (self.lookahead as u64) < self.index)
            {
                self.window.pop_front();
            }
            let target = self.window.front().map_or(1.0, |&(_, g)| g);

            // Attack: reach the target within the look-ahead window; release smoothly.
            if target < self.gain {
                self.gain += (target - self.gain) * 0.25;
                self.gain = self.gain.max(target);
            } else {
                self.gain += (target - self.gain) * self.release;
            }

            self.delay.push_back([pair[0], pair[1]]);
            let delayed = self.delay.pop_front().expect("delay line is pre-filled");
            for (out, sample) in pair.iter_mut().zip(delayed) {
                // Hard clip as a last resort, should practically never engage.
                *out = (sample * self.gain).clamp(-self.ceiling, self.ceiling);
            }
            self.index += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine_frames(amplitude: f32, frames: usize) -> Vec<[f32; FRAME_LEN]> {
        let mut t = 0usize;
        (0..frames)
            .map(|_| {
                let mut frame = [0.0; FRAME_LEN];
                for pair in frame.chunks_exact_mut(2) {
                    let s =
                        (t as f32 * 1000.0 * std::f32::consts::TAU / SAMPLE_RATE as f32).sin() * amplitude;
                    pair[0] = s;
                    pair[1] = s;
                    t += 1;
                }
                frame
            })
            .collect()
    }

    #[test]
    fn limiter_keeps_peaks_below_ceiling() {
        let mut limiter = Limiter::new(-1.0);
        let ceiling = db_to_gain(-1.0);
        let mut max = 0f32;
        for mut frame in sine_frames(2.0, 100) {
            limiter.process(&mut frame);
            max = frame.iter().fold(max, |m, s| m.max(s.abs()));
        }
        assert!(max <= ceiling + 1e-6, "max {max} > ceiling {ceiling}");
        assert!(max > ceiling * 0.9, "limiter should not over-attenuate ({max})");
    }

    #[test]
    fn limiter_is_transparent_below_ceiling() {
        let mut limiter = Limiter::new(-1.0);
        let input = sine_frames(0.5, 10);
        let mut output = input.clone();
        for frame in &mut output {
            limiter.process(frame);
        }
        // Output is the input delayed by the look-ahead.
        let flat_in: Vec<f32> = input.iter().flatten().copied().collect();
        let flat_out: Vec<f32> = output.iter().flatten().copied().collect();
        let delay = limiter.lookahead * CHANNELS;
        for i in 0..flat_in.len() - delay {
            assert!((flat_in[i] - flat_out[i + delay]).abs() < 1e-6);
        }
    }

    #[test]
    fn normalizer_converges_to_target() {
        let mut normalizer = Normalizer::new(-18.0, None);
        let mut frames = sine_frames(0.05, 500); // quiet sine (~ -29 LUFS)
        for frame in &mut frames {
            normalizer.process(frame);
        }
        let mut meter = ebur128::EbuR128::new(2, SAMPLE_RATE, ebur128::Mode::I).unwrap();
        for frame in &frames[250..] {
            meter.add_frames_f32(frame).unwrap();
        }
        let out = meter.loudness_global().unwrap() as f32;
        assert!((out + 18.0).abs() < 0.5, "output loudness {out} LUFS");
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
