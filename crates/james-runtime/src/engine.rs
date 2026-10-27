//! Playback engine abstraction, so the instance logic can be tested without ffmpeg.

use std::time::Duration;

use james_audio::{AudioEngine, EngineConfig, EngineEvent, PlayRequest};
use james_core::audio::OpusFrame;
use tokio::runtime::Handle;
use tokio::sync::mpsc;

/// The operations an instance needs from its audio engine.
pub trait PlaybackEngine: Send + Sync {
    fn play(&self, request: PlayRequest);
    fn stop(&self);
    fn pause(&self, paused: bool);
    fn set_volume(&self, volume: u8);
    fn seek(&self, position: Duration);
    fn set_bitrate(&self, bitrate: u32);
    fn position(&self) -> Duration;
}

impl PlaybackEngine for AudioEngine {
    fn play(&self, request: PlayRequest) {
        AudioEngine::play(self, request)
    }
    fn stop(&self) {
        AudioEngine::stop(self)
    }
    fn pause(&self, paused: bool) {
        AudioEngine::pause(self, paused)
    }
    fn set_volume(&self, volume: u8) {
        AudioEngine::set_volume(self, volume)
    }
    fn seek(&self, position: Duration) {
        AudioEngine::seek(self, position)
    }
    fn set_bitrate(&self, bitrate: u32) {
        AudioEngine::set_bitrate(self, bitrate)
    }
    fn position(&self) -> Duration {
        self.status().position
    }
}

pub trait EngineFactory: Send + Sync {
    fn spawn(
        &self,
        config: EngineConfig,
        audio: mpsc::Sender<OpusFrame>,
        events: mpsc::UnboundedSender<EngineEvent>,
    ) -> anyhow::Result<Box<dyn PlaybackEngine>>;
}

/// Spawns real audio engines on a runtime dedicated to audio.
pub struct AudioEngineFactory {
    runtime: Handle,
}

impl AudioEngineFactory {
    /// Uses `runtime` for all engines (see [`audio_runtime`]).
    pub fn new(runtime: Handle) -> Self {
        Self { runtime }
    }
}

impl EngineFactory for AudioEngineFactory {
    fn spawn(
        &self,
        config: EngineConfig,
        audio: mpsc::Sender<OpusFrame>,
        events: mpsc::UnboundedSender<EngineEvent>,
    ) -> anyhow::Result<Box<dyn PlaybackEngine>> {
        Ok(Box::new(AudioEngine::spawn_on(
            &self.runtime,
            config,
            audio,
            events,
        )?))
    }
}

/// Starts a multi-threaded runtime reserved for audio engines and decoders, so blocking or
/// slow work elsewhere can never delay the 20 ms audio clock. The runtime lives for the
/// rest of the process.
pub fn audio_runtime(threads: usize) -> anyhow::Result<Handle> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(threads.max(1))
        .thread_name("james-audio")
        .enable_all()
        .build()?;
    let handle = runtime.handle().clone();
    std::thread::Builder::new()
        .name("james-audio-runtime".into())
        .spawn(move || {
            runtime.block_on(std::future::pending::<()>());
        })?;
    Ok(handle)
}
