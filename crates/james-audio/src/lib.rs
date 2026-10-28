//! Real-time audio pipeline.
//!
//! ```text
//! ffmpeg decoder ──► normaliser ──► volume ──► mixer ──► limiter ──► Opus ──► platform adapter
//!   (48 kHz f32)    (EBU R128)               (+ overlays)
//! ```
//!
//! One [`AudioEngine`] runs per voice session as a single tokio task ticking
//! every 20 ms, so the per-instance cost stays small.

pub mod decoder;
pub mod dsp;
pub mod encoder;
pub mod engine;

pub use engine::{AudioEngine, EngineConfig, EngineEvent, EngineStatus, PlayRequest, SpeechRequest};

/// One interleaved stereo frame of 20 ms.
pub type Frame = Box<[f32; james_core::audio::FRAME_LEN]>;

pub fn silent_frame() -> Frame {
    Box::new([0.0; james_core::audio::FRAME_LEN])
}
