//! Audio format shared by all components.

use bytes::Bytes;

/// Internal sample rate of the whole pipeline.
pub const SAMPLE_RATE: u32 = 48_000;
/// Internal channel count (interleaved stereo).
pub const CHANNELS: usize = 2;
/// Duration of one audio frame in milliseconds.
pub const FRAME_MS: u32 = 20;
/// Samples per channel in one frame.
pub const FRAME_SAMPLES: usize = (SAMPLE_RATE * FRAME_MS / 1000) as usize;
/// Interleaved samples in one frame.
pub const FRAME_LEN: usize = FRAME_SAMPLES * CHANNELS;

/// One 20 ms Opus packet ready to be sent by a platform adapter.
#[derive(Debug, Clone)]
pub struct OpusFrame {
    pub data: Bytes,
}
