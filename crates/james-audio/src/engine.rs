//! Per-session real-time playback engine.

use std::task::Poll;
use std::time::Duration;

use james_core::audio::{OpusFrame, FRAME_MS};
use james_core::source::MediaInput;
use tokio::sync::{mpsc, watch};
use tokio::time::{interval, Instant, MissedTickBehavior};

use crate::decoder::Decoder;
use crate::dsp::{volume_to_gain, GainRamp, Limiter, Normalizer};
use crate::encoder::OpusEncoder;

#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub ffmpeg: String,
    /// Loudness target in LUFS.
    pub target_lufs: f32,
    /// Limiter ceiling in dBFS.
    pub ceiling_db: f32,
    /// Opus bitrate in bit/s.
    pub bitrate: u32,
    /// Initial volume 0–100.
    pub volume: u8,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            ffmpeg: "ffmpeg".into(),
            target_lufs: -18.0,
            ceiling_db: -1.0,
            bitrate: 128_000,
            volume: 50,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PlayRequest {
    /// Caller-chosen id reported back in [`EngineEvent`]s.
    pub id: u64,
    pub input: MediaInput,
    pub start: Duration,
    /// Cached integrated loudness of the track, if known.
    pub known_lufs: Option<f32>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EngineEvent {
    Started {
        id: u64,
    },
    Finished {
        id: u64,
        error: Option<String>,
        measured_lufs: Option<f32>,
        stopped: bool,
    },
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct EngineStatus {
    pub current: Option<u64>,
    pub position: Duration,
    pub paused: bool,
    pub volume: u8,
    /// Number of 20 ms frames the decoder could not deliver in time.
    pub underruns: u64,
}

enum Command {
    Play(PlayRequest),
    Stop,
    Pause(bool),
    Volume(u8),
    Seek(Duration),
    Bitrate(u32),
}

/// Handle to a running engine task. Dropping all handles stops the task.
#[derive(Clone)]
pub struct AudioEngine {
    commands: mpsc::UnboundedSender<Command>,
    status: watch::Receiver<EngineStatus>,
}

impl AudioEngine {
    /// Starts the engine. Encoded frames go to `audio`; an empty frame marks the end of a
    /// transmission (playback stopped or paused).
    pub fn spawn(
        config: EngineConfig,
        audio: mpsc::Sender<OpusFrame>,
        events: mpsc::UnboundedSender<EngineEvent>,
    ) -> anyhow::Result<Self> {
        let encoder = OpusEncoder::new(config.bitrate)?;
        let (commands, rx) = mpsc::unbounded_channel();
        let (status_tx, status) = watch::channel(EngineStatus {
            volume: config.volume,
            ..EngineStatus::default()
        });
        let task = EngineTask {
            volume_ramp: GainRamp::new(volume_to_gain(config.volume)),
            limiter: Limiter::new(config.ceiling_db),
            config,
            encoder,
            audio,
            events,
            status: status_tx,
            current: None,
            transmitting: false,
        };
        tokio::spawn(task.run(rx));
        Ok(Self { commands, status })
    }

    pub fn play(&self, request: PlayRequest) {
        let _ = self.commands.send(Command::Play(request));
    }

    pub fn stop(&self) {
        let _ = self.commands.send(Command::Stop);
    }

    pub fn pause(&self, paused: bool) {
        let _ = self.commands.send(Command::Pause(paused));
    }

    pub fn set_volume(&self, volume: u8) {
        let _ = self.commands.send(Command::Volume(volume.min(100)));
    }

    pub fn seek(&self, position: Duration) {
        let _ = self.commands.send(Command::Seek(position));
    }

    pub fn set_bitrate(&self, bitrate: u32) {
        let _ = self.commands.send(Command::Bitrate(bitrate));
    }

    pub fn status(&self) -> EngineStatus {
        self.status.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<EngineStatus> {
        self.status.clone()
    }
}

struct Current {
    request: PlayRequest,
    decoder: Decoder,
    normalizer: Normalizer,
    position: Duration,
    /// Playback starts once enough is buffered (or the decoder finished / timed out).
    buffering_since: Option<Instant>,
}

struct EngineTask {
    config: EngineConfig,
    encoder: OpusEncoder,
    volume_ramp: GainRamp,
    limiter: Limiter,
    audio: mpsc::Sender<OpusFrame>,
    events: mpsc::UnboundedSender<EngineEvent>,
    status: watch::Sender<EngineStatus>,
    current: Option<Current>,
    transmitting: bool,
}

/// Frames to buffer before starting (200 ms), and the maximum wait for them.
const PREROLL_FRAMES: usize = 10;
const PREROLL_TIMEOUT: Duration = Duration::from_secs(5);

impl EngineTask {
    async fn run(mut self, mut commands: mpsc::UnboundedReceiver<Command>) {
        let mut ticker = interval(Duration::from_millis(FRAME_MS.into()));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                command = commands.recv() => match command {
                    Some(command) => self.handle(command),
                    None => break,
                },
                _ = ticker.tick() => self.tick(),
            }
        }
        self.finish(true, None);
    }

    fn handle(&mut self, command: Command) {
        match command {
            Command::Play(request) => {
                self.finish(true, None);
                self.start(request);
            }
            Command::Stop => {
                self.finish(true, None);
                self.end_transmission();
            }
            Command::Pause(paused) => {
                self.status.send_modify(|s| s.paused = paused);
                if paused {
                    self.end_transmission();
                }
            }
            Command::Volume(volume) => self.status.send_modify(|s| s.volume = volume),
            Command::Seek(position) => {
                if let Some(current) = self.current.take() {
                    let mut request = current.request;
                    request.start = position;
                    // Keep the loudness measured so far so the level does not jump.
                    request.known_lufs = request.known_lufs.or(current.normalizer.measured_lufs());
                    self.open(request, false);
                }
            }
            Command::Bitrate(bitrate) => {
                if let Err(e) = self.encoder.set_bitrate(bitrate) {
                    tracing::warn!(error = %e, "failed to set bitrate");
                }
            }
        }
    }

    fn start(&mut self, request: PlayRequest) {
        let id = request.id;
        if self.open(request, true) {
            let _ = self.events.send(EngineEvent::Started { id });
        }
    }

    /// Opens a decoder; reports a failure as a finished track.
    fn open(&mut self, request: PlayRequest, fresh: bool) -> bool {
        match Decoder::spawn(&self.config.ffmpeg, &request.input, request.start) {
            Ok(decoder) => {
                let normalizer = Normalizer::new(self.config.target_lufs, request.known_lufs);
                let position = request.start;
                self.status.send_modify(|s| {
                    s.current = Some(request.id);
                    s.position = position;
                    if fresh {
                        s.underruns = 0;
                    }
                });
                self.current = Some(Current {
                    request,
                    decoder,
                    normalizer,
                    position,
                    buffering_since: Some(Instant::now()),
                });
                true
            }
            Err(e) => {
                let _ = self.events.send(EngineEvent::Finished {
                    id: request.id,
                    error: Some(format!("{e:#}")),
                    measured_lufs: None,
                    stopped: false,
                });
                false
            }
        }
    }

    fn finish(&mut self, stopped: bool, error: Option<String>) {
        if let Some(mut current) = self.current.take() {
            let error = error.or_else(|| current.decoder.error());
            let _ = self.events.send(EngineEvent::Finished {
                id: current.request.id,
                error,
                measured_lufs: current.normalizer.measured_lufs(),
                stopped,
            });
            self.status.send_modify(|s| {
                s.current = None;
                s.position = Duration::ZERO;
            });
        }
    }

    fn end_transmission(&mut self) {
        if self.transmitting {
            self.transmitting = false;
            let _ = self.audio.try_send(OpusFrame {
                data: Default::default(),
            });
        }
    }

    fn tick(&mut self) {
        if self.status.borrow().paused {
            return;
        }
        let Some(current) = &mut self.current else { return };

        if let Some(since) = current.buffering_since {
            let ready = current.decoder.buffered() >= PREROLL_FRAMES
                || current.decoder.is_finished()
                || since.elapsed() > PREROLL_TIMEOUT;
            if !ready {
                return;
            }
            current.buffering_since = None;
        }

        match current.decoder.try_next() {
            Poll::Ready(Some(mut frame)) => {
                current.normalizer.process(&mut frame);
                let volume = volume_to_gain(self.status.borrow().volume);
                self.volume_ramp.process(&mut frame, volume);
                self.limiter.process(&mut frame);
                current.position += Duration::from_millis(FRAME_MS.into());
                let position = current.position;
                self.status.send_modify(|s| s.position = position);
                match self.encoder.encode(&frame) {
                    Ok(packet) => {
                        // A full queue means the adapter is not sending (e.g. reconnecting): drop.
                        let _ = self.audio.try_send(packet);
                        self.transmitting = true;
                    }
                    Err(e) => tracing::warn!(error = %e, "opus encoding failed"),
                }
            }
            Poll::Ready(None) => {
                self.finish(false, None);
                self.end_transmission();
            }
            Poll::Pending => self.status.send_modify(|s| s.underruns += 1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decoder::tests::sine_file;

    #[tokio::test]
    async fn plays_a_file_in_real_time() {
        let dir = tempfile::tempdir().unwrap();
        let path = sine_file(dir.path(), 1);
        let (audio_tx, mut audio_rx) = mpsc::channel(512);
        let (events_tx, mut events_rx) = mpsc::unbounded_channel();
        let engine = AudioEngine::spawn(EngineConfig::default(), audio_tx, events_tx).unwrap();

        let start = Instant::now();
        engine.play(PlayRequest {
            id: 7,
            input: MediaInput::File(path),
            start: Duration::ZERO,
            known_lufs: None,
        });
        assert_eq!(events_rx.recv().await, Some(EngineEvent::Started { id: 7 }));
        let finished = events_rx.recv().await.unwrap();
        let elapsed = start.elapsed();
        let EngineEvent::Finished {
            id,
            error,
            measured_lufs,
            stopped,
        } = finished
        else {
            panic!()
        };
        assert_eq!((id, error, stopped), (7, None, false));
        assert!(measured_lufs.is_some());
        assert!(
            elapsed >= Duration::from_millis(950),
            "played too fast: {elapsed:?}"
        );

        let mut packets = Vec::new();
        while let Ok(p) = audio_rx.try_recv() {
            packets.push(p);
        }
        assert_eq!(packets.len(), 51, "50 audio frames + end marker");
        assert!(packets.last().unwrap().data.is_empty());
    }

    #[tokio::test]
    async fn stop_and_errors_are_reported() {
        let (audio_tx, _audio_rx) = mpsc::channel(512);
        let (events_tx, mut events_rx) = mpsc::unbounded_channel();
        let engine = AudioEngine::spawn(EngineConfig::default(), audio_tx, events_tx).unwrap();
        engine.play(PlayRequest {
            id: 1,
            input: MediaInput::File("/nonexistent.flac".into()),
            start: Duration::ZERO,
            known_lufs: None,
        });
        assert_eq!(events_rx.recv().await, Some(EngineEvent::Started { id: 1 }));
        let EngineEvent::Finished { error, .. } = events_rx.recv().await.unwrap() else {
            panic!()
        };
        assert!(error.unwrap().contains("No such file"));
    }
}
