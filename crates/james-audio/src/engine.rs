//! Per-session real-time playback engine.

use std::collections::VecDeque;
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
    /// Largest encoded frame the platform delivers (see [`crate::encoder::OpusEncoder::new`]).
    pub max_frame_bytes: usize,
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
            max_frame_bytes: crate::encoder::MAX_PACKET,
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

/// Speech (or any other overlay) mixed over the music; the music is ducked meanwhile.
#[derive(Debug, Clone)]
pub struct SpeechRequest {
    /// Caller-chosen id reported back in [`EngineEvent::SpeechStarted`] / `SpeechFinished`.
    pub id: u64,
    pub input: MediaInput,
    /// Integrated loudness, if known (speech is short: measured live it would barely be
    /// corrected).
    pub known_lufs: Option<f32>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EngineEvent {
    /// A speech request started playing.
    SpeechStarted { id: u64 },
    /// A speech request ended (`error` if it failed, `stopped` if it was cancelled).
    SpeechFinished {
        id: u64,
        error: Option<String>,
        stopped: bool,
    },
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
    /// Speech request playing, if any.
    pub speech: Option<u64>,
}

enum Command {
    Play(PlayRequest),
    Stop,
    Pause(bool),
    Volume(u8),
    Seek(Duration),
    Bitrate(u32),
    TargetLufs(f32),
    Speak(SpeechRequest),
    StopSpeech,
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
        let encoder = OpusEncoder::new(config.bitrate, config.max_frame_bytes)?;
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
            speech: None,
            speech_queue: VecDeque::new(),
            duck: GainRamp::new(1.0),
            duck_db: 0.0,
        };
        // In the caller's span: messages belong to the instance's log.
        runtime.spawn(tracing::Instrument::in_current_span(task.run(rx)));
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

    /// Queues speech; it plays over the music (ducked) after earlier speech requests.
    pub fn speak(&self, request: SpeechRequest) {
        let _ = self.commands.send(Command::Speak(request));
    }

    /// Cancels the playing and all queued speech.
    pub fn stop_speech(&self) {
        let _ = self.commands.send(Command::StopSpeech);
    }

    /// Loudness target for the next tracks.
    pub fn set_target_lufs(&self, lufs: f32) {
        let _ = self.commands.send(Command::TargetLufs(lufs));
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
/// Music level while speech plays.
const DUCK_DB: f32 = -14.0;
/// Duck gain change per 20 ms frame: down in ~100 ms, back up in ~300 ms.
const DUCK_ATTACK_DB: f32 = 2.8;
const DUCK_RELEASE_DB: f32 = 0.9;
/// Speech frames buffered before it starts, and the longest wait for a speech decoder.
const SPEECH_PREROLL: usize = 5;
const SPEECH_TIMEOUT: Duration = Duration::from_secs(10);
/// Speech that stops delivering audio for this long has failed.
const SPEECH_STALL: Duration = Duration::from_secs(3);
/// Queued speech requests (older ones are dropped beyond this).
const MAX_SPEECH_QUEUE: usize = 32;
/// Loudness of speech (a little above the music target, it must be understood).
const SPEECH_LUFS_OFFSET: f32 = 2.0;

/// The speech request playing.
struct Speech {
    id: u64,
    decoder: Decoder,
    normalizer: Normalizer,
    started: bool,
    since: Instant,
    last_frame: Instant,
    ended_at: Option<Instant>,
}

impl Speech {
    /// Audible (the music is ducked): not while buffering or after the end.
    fn speaking(&self) -> bool {
        self.started && self.ended_at.is_none()
    }
}

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
    speech: Option<Speech>,
    speech_queue: VecDeque<SpeechRequest>,
    /// Ducking of the music while speech plays.
    duck: GainRamp,
    duck_db: f32,
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
        self.stop_speech();
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
                self.end_if_silent();
            }
            Command::Pause(paused) => {
                self.status.send_modify(|s| s.paused = paused);
                if paused {
                    self.end_if_silent();
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
            // Applies from the next track (the playing one keeps its level).
            Command::TargetLufs(lufs) => self.config.target_lufs = lufs,
            Command::Speak(request) => {
                if self.speech_queue.len() >= MAX_SPEECH_QUEUE {
                    if let Some(dropped) = self.speech_queue.pop_front() {
                        self.speech_finished(dropped.id, Some("too much speech queued".into()), true);
                    }
                }
                self.speech_queue.push_back(request);
            }
            Command::StopSpeech => {
                self.stop_speech();
                self.end_if_silent();
            }
        }
    }

    /// Cancels the playing and all queued speech.
    fn stop_speech(&mut self) {
        for request in std::mem::take(&mut self.speech_queue) {
            self.speech_finished(request.id, None, true);
        }
        if let Some(speech) = self.speech.take() {
            self.speech_finished(speech.id, None, true);
        }
    }

    fn speech_finished(&self, id: u64, error: Option<String>, stopped: bool) {
        self.status.send_modify(|s| {
            if s.speech == Some(id) {
                s.speech = None;
            }
        });
        let _ = self
            .events
            .send(EngineEvent::SpeechFinished { id, error, stopped });
    }

    /// Ends the transmission unless speech is still talking.
    fn end_if_silent(&mut self) {
        if self.speech.is_none() {
            self.end_transmission();
        }
    }

    /// Starts the next queued speech request if none plays.
    fn next_speech(&mut self) {
        while self.speech.is_none() {
            let Some(request) = self.speech_queue.pop_front() else {
                return;
            };
            match Decoder::spawn(&self.config.ffmpeg, &request.input, Duration::ZERO, None) {
                Ok(decoder) => {
                    let id = request.id;
                    self.status.send_modify(|s| s.speech = Some(id));
                    self.speech = Some(Speech {
                        id,
                        decoder,
                        normalizer: Normalizer::new(
                            self.config.target_lufs + SPEECH_LUFS_OFFSET,
                            request.known_lufs,
                        ),
                        started: false,
                        since: Instant::now(),
                        last_frame: Instant::now(),
                        ended_at: None,
                    });
                }
                Err(e) => self.speech_finished(request.id, Some(format!("{e:#}")), false),
            }
        }
    }

    /// The next speech frame (`None` while buffering or when no speech plays).
    fn speech_frame(&mut self) -> Option<crate::Frame> {
        self.next_speech();
        let speech = self.speech.as_mut()?;
        if let Some(ended) = speech.ended_at {
            let exit = speech.decoder.exit().cloned();
            if exit.is_some() || ended.elapsed() > EXIT_TIMEOUT {
                let error = exit
                    .filter(|e| !e.success || !speech.started)
                    .and_then(|e| e.error());
                let id = speech.id;
                self.speech = None;
                self.speech_finished(id, error, false);
            }
            return None;
        }
        if !speech.started {
            let ready = speech.decoder.buffered() >= SPEECH_PREROLL || speech.decoder.is_finished();
            if !ready {
                if speech.since.elapsed() > SPEECH_TIMEOUT {
                    let id = speech.id;
                    self.speech = None;
                    self.speech_finished(id, Some("the speech did not start in time".into()), false);
                }
                return None;
            }
        }
        match speech.decoder.try_next() {
            Poll::Ready(Some(mut frame)) => {
                if !speech.started {
                    speech.started = true;
                    let _ = self.events.send(EngineEvent::SpeechStarted { id: speech.id });
                }
                speech.last_frame = Instant::now();
                speech.normalizer.process(&mut frame);
                Some(frame)
            }
            Poll::Ready(None) => {
                speech.ended_at = Some(Instant::now());
                None
            }
            Poll::Pending if speech.started && speech.last_frame.elapsed() > SPEECH_STALL => {
                let id = speech.id;
                self.speech = None;
                self.speech_finished(id, Some("the speech stalled".into()), false);
                None
            }
            // A short gap in speech is filled with silence: the music goes on and the
            // transmission does not end mid-sentence.
            Poll::Pending if speech.started => Some(Box::new([0.0; james_core::audio::FRAME_LEN])),
            Poll::Pending => None,
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
                self.end_if_silent();
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
        let paused = self.status.borrow().paused;
        let music = if paused { None } else { self.music_frame() };
        let speech = self.speech_frame();
        // The music is ducked while speech is audible (also during gaps in the speech); the
        // ramp is linear in dB, so it sounds even.
        let speaking = self.speech.as_ref().is_some_and(Speech::speaking);
        let duck_target = if speaking { DUCK_DB } else { 0.0 };
        let rate = if duck_target < self.duck_db {
            DUCK_ATTACK_DB
        } else {
            DUCK_RELEASE_DB
        };
        self.duck_db += (duck_target - self.duck_db).clamp(-rate, rate);
        let mut frame = match (music, speech) {
            (None, None) => {
                // Nothing to say this tick: listeners see the bot stop talking.
                self.end_transmission();
                return;
            }
            (Some(mut music), speech) => {
                self.duck
                    .process(&mut music, crate::dsp::db_to_gain(self.duck_db));
                if let Some(speech) = speech {
                    for (m, s) in music.iter_mut().zip(speech.iter()) {
                        *m += s;
                    }
                }
                music
            }
            (None, Some(speech)) => speech,
        };
        let volume = volume_to_gain(self.status.borrow().volume);
        self.volume_ramp.process(&mut frame, volume);
        self.limiter.process(&mut frame);
        match self.encoder.encode(&frame) {
            Ok(packet) => {
                // A full queue means the adapter is not sending (e.g. reconnecting): drop.
                let _ = self.audio.try_send(packet);
                self.transmitting = true;
            }
            Err(e) => tracing::warn!(error = %e, "opus encoding failed"),
        }
    }

    /// The next normalised music frame, or `None` (nothing playing, buffering, ended).
    fn music_frame(&mut self) -> Option<crate::Frame> {
        let current = self.current.as_mut()?;

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
            return None;
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
                return None;
            }
            current.buffering_since = None;
        }

        match current.decoder.try_next() {
            Poll::Ready(Some(mut frame)) => {
                current.last_frame = Instant::now();
                current.has_frame = true;
                current.normalizer.process(&mut frame);
                current.position += Duration::from_millis(FRAME_MS.into());
                let position = current.position;
                if !current.started {
                    current.started = true;
                    let _ = self.events.send(EngineEvent::Started {
                        id: current.request.id,
                    });
                }
                self.status.send_modify(|s| s.position = position);
                Some(frame)
            }
            Poll::Ready(None) => {
                current.ended_at = Some(Instant::now());
                None
            }
            Poll::Pending => {
                // Underrun: rebuffer instead of stuttering frame by frame.
                current.buffering_since = Some(Instant::now());
                self.status.send_modify(|s| s.underruns += 1);
                None
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

#[cfg(test)]
mod speech_tests {
    use super::*;
    use crate::decoder::tests::sine_file;

    fn silence_file(dir: &std::path::Path, secs: u32) -> std::path::PathBuf {
        let path = dir.join("silence.wav");
        let status = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i"])
            .arg(format!("anullsrc=r=22050:cl=mono:d={secs}"))
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success());
        path
    }

    fn engine() -> (
        AudioEngine,
        mpsc::Receiver<OpusFrame>,
        mpsc::UnboundedReceiver<EngineEvent>,
    ) {
        let (audio_tx, audio_rx) = mpsc::channel(1024);
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let config = EngineConfig {
            volume: 100,
            ..Default::default()
        };
        (
            AudioEngine::spawn(config, audio_tx, events_tx).unwrap(),
            audio_rx,
            events_rx,
        )
    }

    fn rms(decoder: &mut audiopus::coder::Decoder, packet: &OpusFrame) -> f32 {
        let mut out = vec![0f32; 5760 * 2];
        let n = decoder
            .decode_float(
                Some(audiopus::packet::Packet::try_from(&packet.data[..]).unwrap()),
                audiopus::MutSignals::try_from(&mut out[..]).unwrap(),
                false,
            )
            .unwrap();
        let samples = &out[..n * 2];
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
    }

    #[tokio::test]
    async fn speech_plays_without_music() {
        let dir = tempfile::tempdir().unwrap();
        let (engine, mut audio, mut events) = engine();
        engine.speak(SpeechRequest {
            id: 1,
            input: MediaInput::File(sine_file(dir.path(), 1)),
            known_lufs: None,
        });
        assert_eq!(events.recv().await, Some(EngineEvent::SpeechStarted { id: 1 }));
        assert_eq!(
            events.recv().await,
            Some(EngineEvent::SpeechFinished {
                id: 1,
                error: None,
                stopped: false
            })
        );
        let mut frames = 0;
        loop {
            let packet = tokio::time::timeout(Duration::from_secs(1), audio.recv())
                .await
                .unwrap()
                .unwrap();
            if packet.data.is_empty() {
                break; // end of transmission after the speech
            }
            frames += 1;
        }
        assert!(
            (45..=52).contains(&frames),
            "about 1 s of speech, got {frames} frames"
        );
    }

    #[tokio::test]
    async fn music_is_ducked_and_keeps_playing_during_speech() {
        let music_dir = tempfile::tempdir().unwrap();
        let speech_dir = tempfile::tempdir().unwrap();
        let (engine, mut audio, mut events) = engine();
        engine.play(PlayRequest {
            id: 7,
            input: MediaInput::File(sine_file(music_dir.path(), 4)),
            start: Duration::ZERO,
            known_lufs: None,
        });
        assert_eq!(events.recv().await, Some(EngineEvent::Started { id: 7 }));
        let mut decoder =
            audiopus::coder::Decoder::new(audiopus::SampleRate::Hz48000, audiopus::Channels::Stereo).unwrap();
        // Music alone for ~1 s.
        let mut before = Vec::new();
        for _ in 0..50 {
            let packet = audio.recv().await.unwrap();
            before.push(rms(&mut decoder, &packet));
        }
        // Silent "speech": only the ducking changes the level.
        engine.speak(SpeechRequest {
            id: 2,
            input: MediaInput::File(silence_file(speech_dir.path(), 1)),
            known_lufs: None,
        });
        let mut during = Vec::new();
        let mut speaking = false;
        loop {
            tokio::select! {
                packet = audio.recv() => {
                    let packet = packet.unwrap();
                    assert!(!packet.data.is_empty(), "no gap in the transmission during speech");
                    if speaking {
                        during.push(rms(&mut decoder, &packet));
                    } else {
                        rms(&mut decoder, &packet);
                    }
                }
                event = events.recv() => match event.unwrap() {
                    EngineEvent::SpeechStarted { id: 2 } => speaking = true,
                    EngineEvent::SpeechFinished { id: 2, error, .. } => {
                        assert_eq!(error, None);
                        break;
                    }
                    other => panic!("unexpected {other:?}"),
                },
            }
        }
        let level = |v: &[f32]| v[v.len() / 2..].iter().sum::<f32>() / (v.len() / 2) as f32;
        let ratio = level(&during) / level(&before[25..]);
        assert!(
            (0.15..0.3).contains(&ratio),
            "music ducked to about -14 dB, ratio {ratio}"
        );
        // The music goes on afterwards.
        let EngineEvent::Finished {
            id: 7,
            stopped: false,
            error: None,
            ..
        } = events.recv().await.unwrap()
        else {
            panic!("the music should finish normally");
        };
    }

    #[tokio::test]
    async fn stopping_speech_cancels_the_queue() {
        let dir = tempfile::tempdir().unwrap();
        let (engine, _audio, mut events) = engine();
        let file = sine_file(dir.path(), 3);
        for id in [1, 2] {
            engine.speak(SpeechRequest {
                id,
                input: MediaInput::File(file.clone()),
                known_lufs: None,
            });
        }
        assert_eq!(events.recv().await, Some(EngineEvent::SpeechStarted { id: 1 }));
        engine.stop_speech();
        let mut finished = Vec::new();
        for _ in 0..2 {
            if let Some(EngineEvent::SpeechFinished { id, stopped, .. }) = events.recv().await {
                finished.push((id, stopped));
            }
        }
        finished.sort();
        assert_eq!(finished, [(1, true), (2, true)]);
    }
}
