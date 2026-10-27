//! Core domain types and extension points of James.
//!
//! Everything platform- or provider-specific lives behind the traits defined
//! here, so new platforms (Discord, …), sources (Spotify, …) and TTS/STT
//! providers can be added without touching the instance runtime.

pub mod audio;
pub mod media;
pub mod platform;
pub mod process;
pub mod source;
pub mod text;
