//! Per-session real-time playback engine.

use std::task::Poll;
use std::time::Duration;

use james_core::audio::{OpusFrame, FRAME_MS};
use james_core::source::MediaInput;
use tokio::runtime::Handle;
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
    /// Limiter ceiling in dBTP.
    pub ceiling_db: f32,
    /// Opus bitrate in bit/s.
    pub bitrate: u32,
    /// Initial volume 0–100.
    pub volume: u8,
    /// Egress proxy for network inputs (see `james_sources::egress`).
    pub http_proxy: Option<String>,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            ffmpeg: "ffmpeg".into(),
            target_lufs: -18.0,
            ceiling_db: -1.0,
            bitrate: 128_000,
            volume: 50,
            http_proxy: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PlayRequest {
    /// Caller-chosen id reported back in [`EngineEvent`]s. Use a fresh id per playback.
    pub id: u64,
    pub input: MediaInput,
    pub start: Duration,
    /// Cached integrated loudness of the track, if known.
    pub known_lufs: Option<f32>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EngineEvent {
    /// The first audio frame of the request was sent.
    Started { id: u64 },
    /// Playback of the request ended. `stopped` = ended by a command (play/stop), not by itself.
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
    /// Starts the engine on the current runtime. See [`Self::spawn_on`].
    pub fn spawn(
        config: EngineConfig,
        audio: mpsc::Sender<OpusFrame>,
        events: mpsc::UnboundedSender<EngineEvent>,
    ) -> anyhow::Result<Self> {
        Self::spawn_on(&Handle::current(), config, audio, events)
    }

    /// Starts the engine task on `runtime` (ideally a runtime dedicated to audio, so
    /// unrelated work never delays the 20 ms clock). Encoded frames go to `audio`; an
    /// empty frame marks the end of a transmission (playback stopped, paused or ended).
    pub fn spawn_on(
        runtime: &Handle,
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
            end_pending: false,
            pending_seek: None,
            seek_requested: None,
            last_seek: None,
        };
        runtime.spawn(task.run(rx));
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

/// Frames to buffer before (re)starting output (200 ms), and the maximum wait for them.
const PREROLL_FRAMES: usize = 10;
const PREROLL_TIMEOUT: Duration = Duration::from_secs(5);
/// A decoder that delivers nothing for this long is considered hung.
const STALL_TIMEOUT: Duration = Duration::from_secs(20);
/// Before the first frame a helper may need longer (extraction, tokens, download start).
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(90);
/// Quiet time after the last seek request before a helper input is restarted.
const PROCESS_SEEK_DEBOUNCE: Duration = Duration::from_millis(700);
/// Minimum time between two decoder restarts caused by seeking.
const SEEK_INTERVAL: Duration = Duration::from_millis(150);
/// Maximum wait for ffmpeg's exit status after its output ended.
const EXIT_TIMEOUT: Duration = Duration::from_secs(2);

/// State kept when re-opening the input after a seek.
struct Resume {
    started: bool,
    normalizer: Normalizer,
}

struct Current {
    request: PlayRequest,
    decoder: Decoder,
    normalizer: Normalizer,
    position: Duration,
    started: bool,
    /// Parts were skipped: the measured loudness does not describe the whole track.
    seeked: bool,
    /// This decoder delivered at least one frame.
    has_frame: bool,
    /// Output (re)starts once enough is buffered, the decoder finished, or the wait timed out.
    buffering_since: Option<Instant>,
    last_frame: Instant,
    /// Set once the decoder output ended; waiting for the exit status.
    ended_at: Option<Instant>,
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
    /// An end-of-transmission marker could not be queued yet.
    end_pending: bool,
    /// Latest requested seek, applied at most every [`SEEK_INTERVAL`].
    pending_seek: Option<Duration>,
    seek_requested: Option<Instant>,
    last_seek: Option<Instant>,
}

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
        self.finish_stopped();
    }

    fn handle(&mut self, command: Command) {
        match command {
            Command::Play(request) => {
                self.pending_seek = None;
                self.finish_stopped();
                self.open(request, None);
            }
            Command::Stop => {
                self.finish_stopped();
                self.end_transmission();
            }
            Command::Pause(paused) => {
                self.status.send_modify(|s| s.paused = paused);
                if paused {
                    self.end_transmission();
                } else if let Some(current) = &mut self.current {
                    // Time spent paused must not count as a decoder stall.
                    let now = Instant::now();
                    current.last_frame = now;
                    if current.buffering_since.is_some() {
                        current.buffering_since = Some(now);
                    }
                }
            }
            Command::Volume(volume) => self.status.send_modify(|s| s.volume = volume),
            // Applied in `tick`, so a burst of seeks (slider drags) starts one decoder, not many.
            Command::Seek(position) => {
                self.pending_seek = Some(position);
                self.seek_requested = Some(Instant::now());
            }
            Command::Bitrate(bitrate) => {
                if let Err(e) = self.encoder.set_bitrate(bitrate) {
                    tracing::warn!(error = %e, "failed to set bitrate");
                }
            }
        }
    }

    /// Opens a decoder for `request`; `resume` carries state over after a seek.
    fn open(&mut self, request: PlayRequest, resume: Option<Resume>) {
        match Decoder::spawn(
            &self.config.ffmpeg,
            &request.input,
            request.start,
            self.config.http_proxy.as_deref(),
        ) {
            Ok(decoder) => {
                let resumed = resume.is_some();
                let (started, normalizer) = match resume {
                    Some(r) => (r.started, r.normalizer),
                    None => (
                        false,
                        Normalizer::new(self.config.target_lufs, request.known_lufs),
                    ),
                };
                let (id, position) = (request.id, request.start);
                self.status.send_modify(|s| {
                    s.current = Some(id);
                    s.position = position;
                    if !resumed {
                        s.underruns = 0;
                    }
                });
                let now = Instant::now();
                self.current = Some(Current {
                    request,
                    decoder,
                    normalizer,
                    position,
                    started,
                    seeked: resumed,
                    has_frame: false,
                    buffering_since: Some(now),
                    last_frame: now,
                    ended_at: None,
                });
            }
            Err(e) => {
                self.status.send_modify(|s| {
                    s.current = None;
                    s.position = Duration::ZERO;
                });
                self.emit_finished(request.id, Some(format!("{e:#}")), None, false);
                self.end_transmission();
            }
        }
    }

    fn emit_finished(&self, id: u64, error: Option<String>, measured_lufs: Option<f32>, stopped: bool) {
        let _ = self.events.send(EngineEvent::Finished {
            id,
            error,
            measured_lufs,
            stopped,
        });
    }

    /// Ends the current request because of a command.
    fn finish_stopped(&mut self) {
        if let Some(current) = self.current.take() {
            self.emit_finished(current.request.id, None, current.normalizer.measured_lufs(), true);
            self.clear_status();
        }
    }

    /// Ends the current request on its own (end of input, error, stall).
    fn finish_ended(&mut self, error: Option<String>) {
        if let Some(current) = self.current.take() {
            // Only report loudness of complete, successful plays.
            let lufs = if error.is_none() && !current.seeked {
                current.normalizer.measured_lufs()
            } else {
                None
            };
            self.emit_finished(current.request.id, error, lufs, false);
            self.clear_status();
            self.end_transmission();
        }
    }

    fn clear_status(&self) {
        self.status.send_modify(|s| {
            s.current = None;
            s.position = Duration::ZERO;
        });
    }

    fn end_transmission(&mut self) {
        if self.transmitting {
            self.transmitting = false;
            self.end_pending = true;
        }
        self.flush_end_marker();
    }

    fn flush_end_marker(&mut self) {
        if self.end_pending
            && self
                .audio
                .try_send(OpusFrame {
                    data: Default::default(),
                })
                .is_ok()
        {
            self.end_pending = false;
        }
    }

    fn apply_seek(&mut self) {
        if self.last_seek.is_some_and(|t| t.elapsed() < SEEK_INTERVAL) {
            return;
        }
        // Inputs from a helper process (yt-dlp) restart a whole download on every seek: wait
        // until the user stopped dragging the slider.
        let is_process = self
            .current
            .as_ref()
            .is_some_and(|c| matches!(c.request.input, MediaInput::Process { .. }));
        if is_process
            && self
                .seek_requested
                .is_some_and(|t| t.elapsed() < PROCESS_SEEK_DEBOUNCE)
        {
            return;
        }
        let Some(position) = self.pending_seek.take() else {
            return;
        };
        if let Some(current) = self.current.take() {
            self.last_seek = Some(Instant::now());
            let mut request = current.request;
            request.start = position;
            // The normaliser (meter and gain) carries over, so the level neither jumps nor
            // locks onto the loudness of the part played so far.
            self.open(
                request,
                Some(Resume {
                    started: current.started,
                    normalizer: current.normalizer,
                }),
            );
        }
    }

    fn tick(&mut self) {
        self.flush_end_marker();
        self.apply_seek();
        if self.status.borrow().paused {
            return;
        }
        let Some(current) = &mut self.current else { return };

        if let Some(ended) = current.ended_at {
            // Output ended: decide success/failure from ffmpeg's exit.
            let exit = current.decoder.exit().cloned();
            match exit {
                Some(exit) => {
                    // After a seek to (or past) the end, no frames is a normal end, not an error.
                    let error = if (current.started || current.seeked) && exit.success {
                        None
                    } else {
                        exit.error()
                    };
                    self.finish_ended(error)
                }
                None if ended.elapsed() > EXIT_TIMEOUT => self.finish_ended(None),
                None => {}
            }
            return;
        }

        if let Some(since) = current.buffering_since {
            let ready = current.decoder.buffered() >= PREROLL_FRAMES
                || current.decoder.is_finished()
                || since.elapsed() > PREROLL_TIMEOUT;
            if !ready {
                // Waiting for a helper start slot is not a stall; the clock starts with the source.
                if !current.decoder.source_active() {
                    current.last_frame = Instant::now();
                }
                // Each decoder (also after a seek) gets the longer start time until its first frame.
                let limit = if current.has_frame {
                    STALL_TIMEOUT
                } else {
                    FIRST_FRAME_TIMEOUT
                };
                if current.last_frame.elapsed() > limit {
                    self.finish_ended(Some("the input stopped delivering audio".into()));
                }
                return;
            }
            current.buffering_since = None;
        }

        match current.decoder.try_next() {
            Poll::Ready(Some(mut frame)) => {
                current.last_frame = Instant::now();
                current.has_frame = true;
                current.normalizer.process(&mut frame);
                let volume = volume_to_gain(self.status.borrow().volume);
                self.volume_ramp.process(&mut frame, volume);
                self.limiter.process(&mut frame);
                current.position += Duration::from_millis(FRAME_MS.into());
                let position = current.position;
                if !current.started {
                    current.started = true;
                    let _ = self.events.send(EngineEvent::Started {
                        id: current.request.id,
                    });
                }
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
            Poll::Ready(None) => current.ended_at = Some(Instant::now()),
            Poll::Pending => {
                // Underrun: rebuffer instead of stuttering frame by frame.
                current.buffering_since = Some(Instant::now());
                self.status.send_modify(|s| s.underruns += 1);
                self.end_transmission();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decoder::tests::sine_file;

    fn engine() -> (
        AudioEngine,
        mpsc::Receiver<OpusFrame>,
        mpsc::UnboundedReceiver<EngineEvent>,
    ) {
        let (audio_tx, audio_rx) = mpsc::channel(512);
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        (
            AudioEngine::spawn(EngineConfig::default(), audio_tx, events_tx).unwrap(),
            audio_rx,
            events_rx,
        )
    }

    fn request(id: u64, input: MediaInput) -> PlayRequest {
        PlayRequest {
            id,
            input,
            start: Duration::ZERO,
            known_lufs: None,
        }
    }

    #[tokio::test]
    async fn plays_a_file_in_real_time() {
        let dir = tempfile::tempdir().unwrap();
        let (engine, mut audio_rx, mut events_rx) = engine();
        let start = Instant::now();
        engine.play(request(7, MediaInput::File(sine_file(dir.path(), 1))));
        assert_eq!(events_rx.recv().await, Some(EngineEvent::Started { id: 7 }));
        let EngineEvent::Finished {
            id,
            error,
            measured_lufs,
            stopped,
        } = events_rx.recv().await.unwrap()
        else {
            panic!()
        };
        let elapsed = start.elapsed();
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
        assert_eq!(engine.status().current, None);
    }

    #[tokio::test]
    async fn errors_are_reported_without_started() {
        let (engine, _audio, mut events_rx) = engine();
        engine.play(request(1, MediaInput::File("/nonexistent.flac".into())));
        let EngineEvent::Finished {
            id, error, stopped, ..
        } = events_rx.recv().await.unwrap()
        else {
            panic!("a failing input must not report Started")
        };
        assert_eq!((id, stopped), (1, false));
        assert!(error.unwrap().contains("No such file"));
    }

    #[tokio::test]
    async fn play_replaces_and_reports_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let path = sine_file(dir.path(), 3);
        let (engine, _audio, mut events_rx) = engine();
        engine.play(request(1, MediaInput::File(path.clone())));
        assert_eq!(events_rx.recv().await, Some(EngineEvent::Started { id: 1 }));
        engine.play(request(2, MediaInput::File(path)));
        let EngineEvent::Finished { id, stopped, .. } = events_rx.recv().await.unwrap() else {
            panic!()
        };
        assert_eq!((id, stopped), (1, true));
        assert_eq!(events_rx.recv().await, Some(EngineEvent::Started { id: 2 }));
        engine.stop();
        let EngineEvent::Finished { id, stopped, .. } = events_rx.recv().await.unwrap() else {
            panic!()
        };
        assert_eq!((id, stopped), (2, true));
    }

    #[tokio::test]
    async fn seeking_past_the_end_finishes_normally() {
        let dir = tempfile::tempdir().unwrap();
        let (engine, _audio, mut events_rx) = engine();
        engine.play(request(1, MediaInput::File(sine_file(dir.path(), 2))));
        assert_eq!(events_rx.recv().await, Some(EngineEvent::Started { id: 1 }));
        // A burst of seeks is coalesced; the last one (past the end) wins.
        for secs in [1, 5, 60] {
            engine.seek(Duration::from_secs(secs));
        }
        let EngineEvent::Finished {
            id, error, stopped, ..
        } = tokio::time::timeout(Duration::from_secs(3), events_rx.recv())
            .await
            .unwrap()
            .unwrap()
        else {
            panic!()
        };
        assert_eq!((id, error, stopped), (1, None, false));
    }

    #[tokio::test]
    async fn pause_and_seek() {
        let dir = tempfile::tempdir().unwrap();
        let (engine, mut audio_rx, mut events_rx) = engine();
        engine.play(request(1, MediaInput::File(sine_file(dir.path(), 5))));
        assert_eq!(events_rx.recv().await, Some(EngineEvent::Started { id: 1 }));
        engine.pause(true);
        tokio::time::sleep(Duration::from_millis(100)).await;
        while audio_rx.try_recv().is_ok() {}
        let paused_at = engine.status().position;
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(
            engine.status().position,
            paused_at,
            "position frozen while paused"
        );
        assert!(audio_rx.try_recv().is_err(), "nothing sent while paused");

        engine.seek(Duration::from_secs(4));
        engine.pause(false);
        let EngineEvent::Finished {
            id,
            error,
            stopped,
            measured_lufs,
        } = tokio::time::timeout(Duration::from_secs(3), events_rx.recv())
            .await
            .unwrap()
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(
            (id, error, stopped),
            (1, None, false),
            "remaining 1 s played after seeking to 4 s"
        );
        assert_eq!(
            measured_lufs, None,
            "a seeked playback must not update the loudness cache"
        );
    }
}
