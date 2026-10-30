//! A single bot instance: platform connection + audio engine + player.
//!
//! The instance runs as one control task. Everything else (adapter, engine,
//! track opening, command execution, description updates) runs in separate
//! tasks and talks to it through channels, so slow operations never block
//! playback control.
//!
//! Every playback gets a fresh *generation* number that is used as the engine
//! request id. Events and open results carrying an older generation are
//! ignored, so skip/previous/play sequences can never act on stale events.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context};
use james_audio::{EngineConfig, EngineEvent, PlayRequest, SpeechRequest};
use james_core::platform::{
    Client, MessageTarget, PlatformCommand, PlatformContext, PlatformEvent, PlatformId, Snapshot, VoicePacket,
};
use james_core::source::{MediaInput, Track};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tracing::Instrument;

use crate::engine::PlaybackEngine;
use crate::manager::Services;
use crate::player::{Player, QueueItem, RepeatMode, Requester};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InstanceConfig {
    pub id: String,
    pub name: String,
    /// Platform id, e.g. `teamspeak`.
    pub platform: String,
    /// Platform-specific settings (address, nickname, …).
    pub settings: serde_json::Value,
    pub audio: AudioSettings,
    pub command_prefix: String,
    /// Show the current track in the bot's description.
    pub now_playing_description: bool,
    pub autostart: bool,
    /// Voice for text to speech (`None` = the default voice).
    pub tts_voice: Option<String>,
    /// Speech recognition: what clients say in the bot's channel.
    pub speech: SpeechSettings,
}

impl InstanceConfig {
    /// Checks values that the platform factory does not validate.
    pub fn validate(&self) -> anyhow::Result<()> {
        let prefix = &self.command_prefix;
        if prefix.is_empty() || prefix.chars().count() > 5 || prefix.chars().any(char::is_whitespace) {
            anyhow::bail!("the command prefix must be 1-5 characters without spaces");
        }
        if self.name.trim().is_empty() || self.name.chars().count() > 64 {
            anyhow::bail!("the name must be 1-64 characters");
        }
        self.audio.validate()?;
        self.speech.validate()
    }
}

impl AudioSettings {
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.volume > 100 {
            anyhow::bail!("volume must be 0-100");
        }
        if !(self.target_lufs.is_finite() && (-40.0..=-5.0).contains(&self.target_lufs)) {
            anyhow::bail!("the loudness target must be between -40 and -5 LUFS");
        }
        if let Some(bitrate) = self.bitrate {
            if !(16_000..=510_000).contains(&bitrate) {
                anyhow::bail!("the bitrate must be between 16 and 510 kbit/s");
            }
        }
        Ok(())
    }
}

impl Default for InstanceConfig {
    fn default() -> Self {
        Self {
            id: "default".into(),
            name: "James".into(),
            platform: "teamspeak".into(),
            settings: serde_json::Value::Object(Default::default()),
            audio: AudioSettings::default(),
            command_prefix: "!".into(),
            now_playing_description: true,
            autostart: true,
            tts_voice: None,
            speech: SpeechSettings::default(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SpeechSettings {
    /// Listen to the clients in the bot's channel (off by default: they are not asked).
    pub enabled: bool,
    /// Language spoken (ISO 639-1, e.g. `de`); `None` = detect it each time (slower).
    pub language: Option<String>,
    /// Spoken commands start with this word, e.g. "James, skip" (`None` = no voice commands).
    pub wake_word: Option<String>,
    /// Speech recognition provider (`None` = the default).
    pub provider: Option<String>,
}

impl SpeechSettings {
    pub fn validate(&self) -> anyhow::Result<()> {
        if let Some(language) = &self.language {
            if !james_core::stt::LANGUAGES.contains(&language.as_str()) {
                anyhow::bail!("the speech language must be a two-letter code such as 'de'");
            }
        }
        if let Some(word) = &self.wake_word {
            let words = word.split_whitespace().count();
            if !(1..=3).contains(&words)
                || word.chars().count() > 40
                || !word
                    .chars()
                    .all(|c| c.is_alphanumeric() || c.is_whitespace() || c == '-')
            {
                anyhow::bail!("the wake word must be 1-3 words of letters and digits");
            }
        }
        if let Some(provider) = &self.provider {
            if provider.is_empty()
                || provider.len() > 32
                || !provider.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            {
                anyhow::bail!("unknown speech recognition provider");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod speech_settings_tests {
    use super::SpeechSettings;

    #[test]
    fn validation() {
        let ok = |language: Option<&str>, wake: Option<&str>| {
            SpeechSettings {
                enabled: true,
                language: language.map(Into::into),
                wake_word: wake.map(Into::into),
                provider: None,
            }
            .validate()
            .is_ok()
        };
        assert!(ok(Some("de"), Some("James")));
        assert!(ok(None, Some("Hey Jöns-Bot")));
        assert!(!ok(Some("deu"), None));
        assert!(!ok(Some("DE"), None));
        assert!(!ok(Some("xx"), None), "not a language Whisper knows");
        assert!(!ok(None, Some("Hey, James!")));
        assert!(!ok(None, Some("James\u{202E}")));
        assert!(!ok(None, Some("a b c d")));
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AudioSettings {
    pub volume: u8,
    /// Opus bitrate in bit/s; default 128 kbit/s, at most the platform maximum.
    pub bitrate: Option<u32>,
    pub target_lufs: f32,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            volume: 50,
            bitrate: None,
            target_lufs: -18.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlayMode {
    /// Replace the current track immediately.
    Now,
    /// Insert at the front of the queue.
    Next,
    /// Append to the queue (starts playback if idle).
    Append,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceStatus {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub connected: bool,
    pub current: Option<QueueItem>,
    /// True while the current track is being opened/buffered and not yet audible.
    pub loading: bool,
    pub position_ms: u64,
    pub paused: bool,
    pub volume: u8,
    pub repeat: RepeatMode,
    pub shuffle: bool,
    pub queue_length: usize,
    /// Why the last connection attempt failed (while not connected).
    pub last_error: Option<String>,
    /// Texts being spoken or waiting to be.
    pub speech: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum InstanceEvent {
    Started,
    Stopped,
    Connected {
        snapshot: Box<Snapshot>,
    },
    Disconnected {
        reason: String,
    },
    Platform {
        event: Box<PlatformEvent>,
    },
    TrackStarted {
        item: QueueItem,
    },
    TrackEnded {
        item: QueueItem,
        error: Option<String>,
    },
    QueueChanged,
    PlaybackChanged {
        status: Box<InstanceStatus>,
    },
    /// Message for the users of the web interface (e.g. from a plugin).
    Notice {
        source: String,
        message: String,
    },
    /// Speech started playing.
    SpeechStarted {
        id: u64,
        text: String,
    },
    /// Speech ended (`error` if it failed, `stopped` if it was cancelled).
    SpeechEnded {
        id: u64,
        error: Option<String>,
        stopped: bool,
    },
    /// A client said something in the bot's channel (speech recognition).
    SpeechRecognized {
        client: Box<Client>,
        text: String,
        language: Option<String>,
    },
}

/// An event tagged with the instance it belongs to (one bus for all instances).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub instance: String,
    #[serde(flatten)]
    pub event: InstanceEvent,
    /// Private messages and command lines (which may contain secrets such as `!link`
    /// codes). Delivered to plugins, but to web clients only with `server.chat.read`.
    #[serde(skip)]
    pub sensitive: bool,
}

type Reply<T> = oneshot::Sender<anyhow::Result<T>>;

enum Control {
    Add {
        tracks: Vec<Track>,
        mode: PlayMode,
        requester: Option<Requester>,
        reply: Reply<Vec<QueueItem>>,
    },
    Skip,
    Previous,
    Stop,
    Pause(bool),
    Volume(u8),
    Seek(Duration),
    Repeat(RepeatMode),
    Shuffle(bool),
    ShuffleQueue,
    Clear,
    Remove {
        item: u64,
        reply: Reply<QueueItem>,
    },
    Move {
        item: u64,
        to: usize,
        reply: Reply<()>,
    },
    Jump {
        item: u64,
        reply: Reply<()>,
    },
    Status {
        reply: oneshot::Sender<InstanceStatus>,
    },
    Say {
        text: String,
        voice: Option<String>,
        language: Option<String>,
        reply: Reply<u64>,
    },
    StopSpeech,
    Queue {
        reply: oneshot::Sender<Vec<QueueItem>>,
    },
    /// Applies a changed configuration to the running instance; answers `false` if it needs
    /// a restart (new connection).
    Reconfigure {
        config: Box<InstanceConfig>,
        reply: oneshot::Sender<bool>,
    },
    Shutdown,
}

enum Internal {
    /// A client said something (speech recognition).
    Heard {
        client: Box<Client>,
        transcript: james_core::stt::Transcript,
    },
    Synthesized {
        id: u64,
        result: anyhow::Result<james_core::tts::Speech>,
    },
    Opened {
        generation: u64,
        /// Decoder input, cached loudness and the cache key it was looked up with.
        result: anyhow::Result<(MediaInput, Option<f32>, Option<String>)>,
    },
}

/// Cloneable handle to a running instance.
#[derive(Clone)]
pub struct InstanceHandle {
    id: Arc<str>,
    /// Credential saves in progress; the manager waits for them after the instance stopped,
    /// so a replacement saved afterwards (identity import) is never overwritten.
    credential_saves: Arc<parking_lot::Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    resolve_slots: Arc<tokio::sync::Semaphore>,
    platform_id: Arc<str>,
    control: mpsc::UnboundedSender<Control>,
    platform: mpsc::Sender<PlatformCommand>,
    services: Services,
}

/// Timeout for platform commands (including waiting for a free slot while reconnecting).
const PLATFORM_TIMEOUT: Duration = Duration::from_secs(10);

impl InstanceHandle {
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Platform of the instance, e.g. `teamspeak`.
    pub fn platform_id(&self) -> &str {
        &self.platform_id
    }

    pub fn services(&self) -> &Services {
        &self.services
    }

    /// Whether the instance task is still running.
    pub fn is_alive(&self) -> bool {
        !self.control.is_closed()
    }

    fn send(&self, control: Control) -> anyhow::Result<()> {
        self.control
            .send(control)
            .map_err(|_| anyhow!("instance is not running"))
    }

    async fn ask<T>(&self, make: impl FnOnce(oneshot::Sender<T>) -> Control) -> anyhow::Result<T> {
        let (tx, rx) = oneshot::channel();
        self.send(make(tx))?;
        rx.await.map_err(|_| anyhow!("instance is not running"))
    }

    /// Resolves user input (URL, search, path, …) through the source registry.
    pub async fn resolve(&self, input: &str) -> anyhow::Result<Vec<Track>> {
        // Bounds the external tools one instance can occupy, so one busy server cannot
        // starve the shared tool slots of all others.
        let _slot = self.resolve_slot().await?;
        let registry = self.services.sources.read().clone();
        registry.resolve(input).await
    }

    pub async fn add(
        &self,
        tracks: Vec<Track>,
        mode: PlayMode,
        requester: Option<Requester>,
    ) -> anyhow::Result<Vec<QueueItem>> {
        self.ask(|reply| Control::Add {
            tracks,
            mode,
            requester,
            reply,
        })
        .await?
    }

    /// Resolves `input` and plays/enqueues the result.
    pub async fn play(
        &self,
        input: &str,
        mode: PlayMode,
        requester: Option<Requester>,
    ) -> anyhow::Result<Vec<QueueItem>> {
        let tracks = self.resolve(input).await?;
        self.add(tracks, mode, requester).await
    }

    pub fn skip(&self) -> anyhow::Result<()> {
        self.send(Control::Skip)
    }
    pub fn previous(&self) -> anyhow::Result<()> {
        self.send(Control::Previous)
    }
    pub fn stop(&self) -> anyhow::Result<()> {
        self.send(Control::Stop)
    }
    pub fn pause(&self, paused: bool) -> anyhow::Result<()> {
        self.send(Control::Pause(paused))
    }
    pub fn set_volume(&self, volume: u8) -> anyhow::Result<()> {
        self.send(Control::Volume(volume.min(100)))
    }
    pub fn seek(&self, position: Duration) -> anyhow::Result<()> {
        self.send(Control::Seek(position))
    }
    pub fn set_repeat(&self, mode: RepeatMode) -> anyhow::Result<()> {
        self.send(Control::Repeat(mode))
    }
    pub fn set_shuffle(&self, shuffle: bool) -> anyhow::Result<()> {
        self.send(Control::Shuffle(shuffle))
    }
    pub fn shuffle_queue(&self) -> anyhow::Result<()> {
        self.send(Control::ShuffleQueue)
    }
    pub fn clear_queue(&self) -> anyhow::Result<()> {
        self.send(Control::Clear)
    }
    /// Waits (bounded) for one of the instance's resolve slots.
    async fn resolve_slot(&self) -> anyhow::Result<tokio::sync::SemaphorePermit<'_>> {
        match tokio::time::timeout(Duration::from_secs(30), self.resolve_slots.acquire()).await {
            Ok(Ok(permit)) => Ok(permit),
            Ok(Err(_)) => Err(anyhow!("instance is shutting down")),
            Err(_) => Err(anyhow!("the bot is busy, please try again in a moment")),
        }
    }

    /// Removes a queued item by its id.
    pub async fn remove(&self, item: u64) -> anyhow::Result<QueueItem> {
        self.ask(|reply| Control::Remove { item, reply }).await?
    }
    /// Removes the item at a 0-based queue position (chat commands).
    pub async fn remove_at(&self, index: usize) -> anyhow::Result<QueueItem> {
        let id = self.item_at(index).await?;
        self.remove(id).await
    }
    /// Moves a queued item (by id) to a 0-based position.
    pub async fn move_item(&self, item: u64, to: usize) -> anyhow::Result<()> {
        self.ask(|reply| Control::Move { item, to, reply }).await?
    }
    /// Plays a queued item (by id) immediately.
    pub async fn jump(&self, item: u64) -> anyhow::Result<()> {
        self.ask(|reply| Control::Jump { item, reply }).await?
    }
    /// Id of the item at a 0-based queue position.
    pub async fn item_at(&self, index: usize) -> anyhow::Result<u64> {
        let queue = self.queue().await?;
        queue
            .get(index)
            .map(|i| i.id)
            .ok_or_else(|| anyhow!("no queue entry #{}", index + 1))
    }
    /// Searches with the search-capable source (yt-dlp) without queueing anything.
    pub async fn search(&self, query: &str, limit: usize) -> anyhow::Result<Vec<Track>> {
        let _slot = self.resolve_slot().await?;
        let registry = self.services.sources.read().clone();
        registry.search(query, limit).await
    }
    pub async fn status(&self) -> anyhow::Result<InstanceStatus> {
        self.ask(|reply| Control::Status { reply }).await
    }
    /// Speaks `text` over the music (with the instance's voice unless `voice` or `language`
    /// pick another). Resolves once the speech is queued.
    pub async fn say(&self, text: &str, voice: Option<&str>, language: Option<&str>) -> anyhow::Result<u64> {
        let (tx, rx) = oneshot::channel();
        self.send(Control::Say {
            text: text.to_owned(),
            voice: voice.map(str::to_owned),
            language: language.map(str::to_owned),
            reply: tx,
        })?;
        rx.await.map_err(|_| anyhow!("instance is not running"))?
    }

    /// Cancels the playing and queued speech.
    pub fn stop_speech(&self) -> anyhow::Result<()> {
        self.send(Control::StopSpeech)
    }

    pub async fn queue(&self) -> anyhow::Result<Vec<QueueItem>> {
        self.ask(|reply| Control::Queue { reply }).await
    }

    /// Waits for (or, after `timeout`, cancels) credential saves still in progress.
    /// `true` if one had to be cancelled (it may still reach the database).
    pub(crate) async fn finish_credential_saves(&self, timeout: Duration) -> bool {
        let saves = std::mem::take(&mut *self.credential_saves.lock());
        let mut aborted = false;
        for mut save in saves {
            if tokio::time::timeout(timeout, &mut save).await.is_err() {
                save.abort();
                let _ = save.await;
                aborted = true;
            }
        }
        aborted
    }

    /// Shows a message to the users of the web interface.
    pub(crate) fn notice(&self, source: &str, message: &str) {
        let _ = self.services.events.send(EventEnvelope {
            instance: self.id.to_string(),
            event: InstanceEvent::Notice {
                source: source.to_owned(),
                message: message.chars().take(500).collect(),
            },
            sensitive: false,
        });
    }

    /// Applies a changed configuration without reconnecting where possible; `Ok(false)` if
    /// the change needs a restart (e.g. another server address).
    pub(crate) async fn reconfigure(&self, config: InstanceConfig) -> anyhow::Result<bool> {
        self.ask(|reply| Control::Reconfigure {
            config: Box::new(config),
            reply,
        })
        .await
    }

    pub(crate) fn shutdown(&self) {
        let _ = self.control.send(Control::Shutdown);
    }

    /// Sends a command to the platform adapter and waits for the result.
    pub async fn platform<T>(
        &self,
        make: impl FnOnce(james_core::platform::Reply<T>) -> PlatformCommand,
    ) -> anyhow::Result<T> {
        let (tx, rx) = oneshot::channel();
        let command = make(tx);
        tokio::time::timeout(PLATFORM_TIMEOUT, async {
            self.platform
                .send(command)
                .await
                .map_err(|_| anyhow!("not connected"))?;
            rx.await.map_err(|_| anyhow!("connection lost"))?
        })
        .await
        .context("the voice server did not answer in time")?
    }

    pub async fn snapshot(&self) -> anyhow::Result<Snapshot> {
        self.platform(|reply| PlatformCommand::Snapshot { reply }).await
    }

    pub async fn send_message(
        &self,
        target: MessageTarget,
        client: Option<PlatformId>,
        text: impl Into<String>,
    ) -> anyhow::Result<()> {
        let text = text.into();
        self.platform(|reply| PlatformCommand::SendMessage {
            target,
            client,
            text,
            reply,
        })
        .await
    }

    /// Replies to a client in the same place it wrote from.
    pub async fn reply(
        &self,
        mode: MessageTarget,
        to: &Client,
        text: impl Into<String>,
    ) -> anyhow::Result<()> {
        let client = (mode == MessageTarget::Client).then(|| to.id.clone());
        self.send_message(mode, client, text).await
    }
}

/// Opus bitrate when an instance sets none.
const DEFAULT_BITRATE: u32 = 128_000;

/// Speech queued or being synthesized per instance.
const MAX_PENDING_SPEECH: usize = 16;
/// Longest a synthesis may take (including loading the voice).
const SYNTHESIS_TIMEOUT: Duration = Duration::from_secs(90);

/// The instance has too much speech queued.
#[derive(Debug)]
pub struct SpeechQueueFull;

impl std::fmt::Display for SpeechQueueFull {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("too much speech queued, try again later")
    }
}

impl std::error::Error for SpeechQueueFull {}

/// A background task that is cancelled when this is dropped.
struct AbortOnDrop(tokio::task::AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Concurrent resolves (URL/search lookups) per instance.
const RESOLVE_SLOTS: usize = 2;
/// Maximum queue length per instance (bounds memory per instance).
pub const MAX_QUEUE: usize = 5000;
/// Maximum consecutive tracks that fail before playback stops.
const MAX_CONSECUTIVE_FAILURES: u32 = 5;
/// Per-client command limits: queued commands, and commands per window.
const COMMAND_QUEUE: usize = 4;
const COMMAND_RATE: usize = 8;
const COMMAND_WINDOW: Duration = Duration::from_secs(10);
/// A per-client command worker exits after this long without commands.
const COMMAND_WORKER_IDLE: Duration = Duration::from_secs(60);

pub(crate) fn spawn(
    config: InstanceConfig,
    services: Services,
) -> anyhow::Result<(InstanceHandle, JoinHandle<()>)> {
    let factory = services
        .platforms
        .get(&config.platform)
        .cloned()
        .ok_or_else(|| anyhow!("unknown platform '{}'", config.platform))?;
    factory.validate(&config.settings)?;
    let capabilities = factory.capabilities();

    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let (internal_tx, internal_rx) = mpsc::unbounded_channel();
    let (platform_tx, platform_rx) = mpsc::channel(64);
    let (platform_events_tx, platform_events_rx) = mpsc::unbounded_channel();
    let (audio_tx, audio_rx) = mpsc::channel(64);
    let (engine_events_tx, engine_events_rx) = mpsc::unbounded_channel();

    // 128 kbit/s is transparent for music with Opus; more never exceeds what the platform takes.
    let bitrate = config
        .audio
        .bitrate
        .unwrap_or(DEFAULT_BITRATE)
        .min(capabilities.max_bitrate);
    // Everything logged by the instance's tasks belongs to its log (see the web interface).
    let span = tracing::info_span!("instance", instance = %config.id);
    let engine = span.in_scope(|| {
        services.engines.spawn(
            EngineConfig {
                ffmpeg: services.ffmpeg.clone(),
                target_lufs: config.audio.target_lufs,
                bitrate,
                max_frame_bytes: capabilities
                    .max_frame_bytes
                    .unwrap_or(james_audio::encoder::MAX_PACKET),
                volume: config.audio.volume,
                http_proxy: services.http_proxy.clone(),
                ..EngineConfig::default()
            },
            audio_tx,
            engine_events_tx,
        )
    })?;

    let handle = InstanceHandle {
        credential_saves: Arc::default(),
        id: config.id.as_str().into(),
        resolve_slots: Arc::new(tokio::sync::Semaphore::new(RESOLVE_SLOTS)),
        platform_id: config.platform.as_str().into(),
        control: control_tx,
        platform: platform_tx,
        services: services.clone(),
    };

    let (voice_tx, speech_tasks) = span.in_scope(|| listen(&config, &services, &handle, &internal_tx));
    let (settings_tx, settings_rx) = watch::channel(config.settings.clone());
    let adapter_task = tokio::spawn(
        run_adapter(config.id.clone(), settings_rx, factory, services.clone(), {
            PlatformContext {
                events: platform_events_tx,
                commands: platform_rx,
                audio: audio_rx,
                voice: voice_tx,
            }
        })
        .instrument(span.clone()),
    );

    let (description_tx, description_rx) = watch::channel(String::new());
    // Also when disabled: it can be switched on (and off, clearing it) while running.
    tokio::spawn(description_updater(handle.clone(), description_rx).instrument(span.clone()));
    // Settings changed while connected (see `reconfigure`) reach the platform in order.
    tokio::spawn(settings_updater(handle.clone(), settings_tx.subscribe()).instrument(span.clone()));

    let task = InstanceTask {
        volume: config.audio.volume,
        config,
        services,
        handle: handle.clone(),
        engine,
        player: Player::default(),
        connected: false,
        paused: false,
        generation: 0,
        playing: None,
        cache_key: None,
        last_error: None,
        open_task: None,
        internal: internal_tx,
        failures: 0,
        adapter_task,
        description: description_tx,
        settings: settings_tx,
        command_workers: HashMap::new(),
        command_times: HashMap::new(),
        speech: HashMap::new(),
        synthesizing: HashMap::new(),
        next_speech: 0,
        failure_log: james_core::logs::RateLimiter::new(10, Duration::from_secs(60)),
        speech_tasks,
        announced: false,
    };
    let join = tokio::spawn(
        task.run(control_rx, internal_rx, platform_events_rx, engine_events_rx)
            .instrument(span),
    );
    Ok((handle, join))
}

/// Starts speech recognition if the instance wants it; returns where the platform sends voice
/// and the tasks to stop with the instance.
fn listen(
    config: &InstanceConfig,
    services: &Services,
    handle: &InstanceHandle,
    internal: &mpsc::UnboundedSender<Internal>,
) -> (Option<mpsc::Sender<VoicePacket>>, Vec<JoinHandle<()>>) {
    if !config.speech.enabled {
        return (None, Vec::new());
    }
    let Some(provider) = services.stt.read().get(config.speech.provider.as_deref()) else {
        tracing::warn!("speech recognition is enabled but not available (see the configuration)");
        return (None, Vec::new());
    };
    provider.prepare();
    let (voice_tx, voice_rx) = mpsc::channel(VOICE_QUEUE);
    let (heard_tx, mut heard_rx) = mpsc::unbounded_channel();
    let settings = crate::speech::ListenerConfig {
        language: config.speech.language.clone(),
        prompt: config.speech.wake_word.clone(),
    };
    // Other bots (James' own instances) are not listened to: what they play or say could
    // otherwise run voice commands in their name.
    let resolve: crate::speech::Resolve = {
        let (handle, bots) = (handle.clone(), services.bot_uids.clone());
        Arc::new(move |id| {
            let (handle, bots) = (handle.clone(), bots.clone());
            Box::pin(async move {
                let snapshot = handle.snapshot().await.ok()?;
                let own = snapshot.own_client.clone()?;
                let channel = snapshot.clients.iter().find(|c| c.id == own)?.channel.clone();
                // Only people in the bot's channel (whose voice it hears).
                snapshot.clients.into_iter().find(|c| {
                    c.id == id && !c.is_self && c.channel == channel && !bots.read().contains(&c.uid)
                })
            })
        })
    };
    let listener = spawn_here(crate::speech::listen(
        provider, settings, resolve, voice_rx, heard_tx,
    ));
    let (handle, internal) = (handle.clone(), internal.clone());
    let forwarder = spawn_here(async move {
        while let Some(heard) = heard_rx.recv().await {
            // Still the same person (client ids are reused), and not too late to act on.
            let present = match handle.snapshot().await {
                Ok(snapshot) => snapshot
                    .clients
                    .iter()
                    .any(|c| c.id == heard.client.id && c.uid == heard.client.uid),
                Err(_) => false,
            };
            if !present || heard.at.elapsed() > SPEECH_MAX_AGE {
                continue;
            }
            let message = Internal::Heard {
                client: Box::new(heard.client),
                transcript: heard.transcript,
            };
            if internal.send(message).is_err() {
                return;
            }
        }
    });
    (Some(voice_tx), vec![listener, forwarder])
}

/// Recognised speech older than this is not acted on.
const SPEECH_MAX_AGE: Duration = Duration::from_secs(30);

/// Voice packets waiting for recognition (about 5 s of one speaker); more are dropped.
const VOICE_QUEUE: usize = 256;

/// Spawns a task that logs into the current instance's log (see `spawn`).
fn spawn_here<F>(future: F) -> JoinHandle<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    tokio::spawn(future.in_current_span())
}

/// Runs the platform adapter, reconnecting with exponential backoff until the instance stops.
async fn run_adapter(
    instance: String,
    settings: watch::Receiver<serde_json::Value>,
    factory: Arc<dyn james_core::platform::PlatformFactory>,
    services: Services,
    mut ctx: PlatformContext,
) {
    let mut backoff = Duration::from_secs(1);
    loop {
        let credentials = services.store.load_credentials(&instance).await;
        let started = Instant::now();
        let current = settings.borrow().clone();
        let result = match factory.create(&current, credentials) {
            Ok(mut adapter) => adapter.run(&mut ctx).await,
            Err(e) => Err(e),
        };
        match result {
            Ok(()) => break,
            Err(e) => {
                if ctx.commands.is_closed() {
                    break;
                }
                if started.elapsed() > Duration::from_secs(60) {
                    backoff = Duration::from_secs(1);
                }
                tracing::warn!(error = %format!("{e:#}"), retry_in = ?backoff, "platform connection failed");
                let _ = ctx.events.send(PlatformEvent::Disconnected {
                    reason: format!("{e:#}"),
                });
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(60));
            }
        }
    }
}

/// Applies changed settings to the running connection (the platform picks what it can change,
/// e.g. the nickname); only the latest value is sent, failures are shown.
async fn settings_updater(handle: InstanceHandle, mut rx: watch::Receiver<serde_json::Value>) {
    while rx.changed().await.is_ok() {
        let settings = rx.borrow_and_update().clone();
        if let Err(e) = handle
            .platform(|reply| PlatformCommand::UpdateSettings { settings, reply })
            .await
        {
            let message = format!("{e:#}");
            tracing::warn!(error = %message, "cannot apply the settings");
            handle.notice("settings", &james_core::text::user_message(&message));
        }
    }
}

/// Pushes the latest description to the platform; intermediate values are skipped.
async fn description_updater(handle: InstanceHandle, mut rx: watch::Receiver<String>) {
    while rx.changed().await.is_ok() {
        let text = rx.borrow_and_update().clone();
        if let Err(e) = handle
            .platform(|reply| PlatformCommand::SetDescription { text, reply })
            .await
        {
            tracing::debug!(error = %e, "failed to update description");
        }
    }
}

/// The playback the engine is (or will be) working on.
#[derive(Debug, Clone, Copy)]
struct Playing {
    generation: u64,
    item: u64,
    /// The engine reported the first audio frame.
    audible: bool,
}

struct CommandJob {
    mode: MessageTarget,
    from: Client,
    text: String,
    /// Spoken (speech recognition): needs the `voice.command` permission as well.
    spoken: bool,
}

struct InstanceTask {
    config: InstanceConfig,
    services: Services,
    handle: InstanceHandle,
    engine: Box<dyn PlaybackEngine>,
    player: Player,
    connected: bool,
    /// Requested state; the engine applies it asynchronously.
    volume: u8,
    paused: bool,
    generation: u64,
    playing: Option<Playing>,
    /// Last connection error, shown while disconnected.
    last_error: Option<String>,
    /// Loudness cache key of the active playback.
    cache_key: Option<String>,
    /// The task opening the current track (yt-dlp etc.), aborted when superseded.
    open_task: Option<tokio::task::AbortHandle>,
    internal: mpsc::UnboundedSender<Internal>,
    failures: u32,
    adapter_task: JoinHandle<()>,
    description: watch::Sender<String>,
    /// Platform settings the adapter uses when it (re)connects.
    settings: watch::Sender<serde_json::Value>,
    command_workers: HashMap<String, mpsc::Sender<CommandJob>>,
    command_times: HashMap<String, VecDeque<Instant>>,
    /// Speech being synthesized, queued or playing: its text and audio file (removed when it
    /// ended).
    speech: HashMap<u64, (String, Option<Arc<james_core::source::TempFile>>)>,
    /// Syntheses in progress (cancelled when dropped, e.g. on stop or shutdown).
    synthesizing: HashMap<u64, AbortOnDrop>,
    next_speech: u64,
    /// Failures logged per kind and minute.
    failure_log: james_core::logs::RateLimiter,
    /// Speech recognition of this instance, stopped with it (queued utterances are dropped).
    speech_tasks: Vec<JoinHandle<()>>,
    /// The channel was told that the bot listens.
    announced: bool,
}

impl InstanceTask {
    fn emit(&self, event: InstanceEvent) {
        self.emit_with(event, false);
    }

    fn emit_with(&self, event: InstanceEvent, sensitive: bool) {
        let _ = self.services.events.send(EventEnvelope {
            instance: self.config.id.clone(),
            event,
            sensitive,
        });
    }

    async fn run(
        mut self,
        mut control: mpsc::UnboundedReceiver<Control>,
        mut internal: mpsc::UnboundedReceiver<Internal>,
        mut platform: mpsc::UnboundedReceiver<PlatformEvent>,
        mut engine: mpsc::UnboundedReceiver<EngineEvent>,
    ) {
        tracing::info!("instance started");
        self.emit(InstanceEvent::Started);
        loop {
            tokio::select! {
                c = control.recv() => match c {
                    Some(Control::Shutdown) | None => break,
                    Some(c) => self.handle_control(c),
                },
                Some(i) = internal.recv() => self.handle_internal(i),
                Some(e) = platform.recv() => self.handle_platform(e),
                Some(e) = engine.recv() => self.handle_engine(e),
            }
        }
        self.cancel_open();
        self.engine.stop();
        let _ = self.handle.platform.try_send(PlatformCommand::Disconnect {
            reason: "shutting down".into(),
        });
        if tokio::time::timeout(Duration::from_secs(5), &mut self.adapter_task)
            .await
            .is_err()
        {
            self.adapter_task.abort();
        }
        for task in &self.speech_tasks {
            task.abort();
        }
        self.emit(InstanceEvent::Stopped);
        tracing::info!("instance stopped");
    }

    fn status(&self) -> InstanceStatus {
        let audible = self.playing.is_some_and(|p| p.audible);
        InstanceStatus {
            id: self.config.id.clone(),
            name: self.config.name.clone(),
            platform: self.config.platform.clone(),
            connected: self.connected,
            current: self.player.current().cloned(),
            loading: self.playing.is_some() && !audible,
            position_ms: if audible {
                self.engine.position().as_millis() as u64
            } else {
                0
            },
            paused: self.paused,
            volume: self.volume,
            repeat: self.player.repeat,
            shuffle: self.player.shuffle,
            queue_length: self.player.queue().len(),
            last_error: if self.connected {
                None
            } else {
                self.last_error.clone()
            },
            speech: self.speech.len(),
        }
    }

    fn playback_changed(&self) {
        self.emit(InstanceEvent::PlaybackChanged {
            status: Box::new(self.status()),
        });
    }

    fn handle_control(&mut self, control: Control) {
        match control {
            Control::Add {
                mut tracks,
                mode,
                requester,
                reply,
            } => {
                let free = MAX_QUEUE.saturating_sub(self.player.queue().len());
                // In "now" mode the first track does not go into the queue.
                let allowed = if mode == PlayMode::Now { free + 1 } else { free };
                tracks.truncate(allowed);
                if tracks.is_empty() {
                    let error = if allowed == 0 {
                        anyhow!("the queue is full ({MAX_QUEUE} tracks)")
                    } else {
                        anyhow!("nothing to play")
                    };
                    let _ = reply.send(Err(error));
                    return;
                }
                let idle = self.player.current().is_none();
                let items = match mode {
                    PlayMode::Now => {
                        let count = tracks.len();
                        let first = self.player.play_now(tracks, requester);
                        let mut items: Vec<_> = first.into_iter().collect();
                        items.extend(self.player.queue().iter().take(count - 1).cloned());
                        self.start_current();
                        items
                    }
                    PlayMode::Next => self.player.enqueue_next(tracks, requester),
                    PlayMode::Append => self.player.enqueue(tracks, requester),
                };
                if idle && mode != PlayMode::Now {
                    self.advance(true);
                } else {
                    self.emit(InstanceEvent::QueueChanged);
                }
                let _ = reply.send(Ok(items));
            }
            Control::Skip => self.advance(true),
            Control::Previous => {
                if self.player.previous().is_some() {
                    self.start_current();
                    self.emit(InstanceEvent::QueueChanged);
                }
            }
            Control::Stop => {
                self.player.stop();
                self.go_idle();
            }
            Control::Pause(paused) => {
                self.engine.pause(paused);
                self.paused = paused;
                self.playback_changed();
            }
            Control::Volume(volume) => {
                self.engine.set_volume(volume);
                self.volume = volume;
                self.playback_changed();
            }
            Control::Seek(position) => {
                // Seeking only makes sense once the engine plays the track (not while opening).
                let audible = self.playing.is_some_and(|p| p.audible);
                if audible && self.player.current().is_some_and(|c| !c.track.is_stream) {
                    self.engine.seek(position);
                    let mut status = self.status();
                    status.position_ms = position.as_millis() as u64;
                    self.emit(InstanceEvent::PlaybackChanged {
                        status: Box::new(status),
                    });
                }
            }
            Control::Repeat(mode) => {
                self.player.repeat = mode;
                self.playback_changed();
            }
            Control::Shuffle(shuffle) => {
                self.player.shuffle = shuffle;
                self.playback_changed();
            }
            Control::ShuffleQueue => {
                self.player.shuffle_queue();
                self.emit(InstanceEvent::QueueChanged);
            }
            Control::Clear => {
                self.player.clear();
                self.emit(InstanceEvent::QueueChanged);
            }
            Control::Remove { item, reply } => {
                let result = self
                    .player
                    .remove(item)
                    .ok_or_else(|| anyhow!("the track is no longer queued"));
                if result.is_ok() {
                    self.emit(InstanceEvent::QueueChanged);
                }
                let _ = reply.send(result);
            }
            Control::Move { item, to, reply } => {
                let moved = self.player.move_item(item, to);
                if moved {
                    self.emit(InstanceEvent::QueueChanged);
                }
                let _ = reply.send(if moved {
                    Ok(())
                } else {
                    Err(anyhow!("the track is no longer queued"))
                });
            }
            Control::Jump { item, reply } => {
                if self.player.jump(item).is_some() {
                    self.start_current();
                    self.emit(InstanceEvent::QueueChanged);
                    let _ = reply.send(Ok(()));
                } else {
                    let _ = reply.send(Err(anyhow!("the track is no longer queued")));
                }
            }
            Control::Status { reply } => {
                let _ = reply.send(self.status());
            }
            Control::Queue { reply } => {
                let _ = reply.send(self.player.queue().iter().cloned().collect());
            }
            // Handled in `run`.
            Control::Say {
                text,
                voice,
                language,
                reply,
            } => self.say(text, voice, language, reply),
            Control::StopSpeech => self.stop_speech(),
            Control::Reconfigure { config, reply } => {
                let _ = reply.send(self.reconfigure(*config));
            }
            Control::Shutdown => {}
        }
    }

    /// Applies what can be changed on the running instance; `false` if a restart is needed.
    fn reconfigure(&mut self, new: InstanceConfig) -> bool {
        let old = &self.config;
        let Some(factory) = self.services.platforms.get(&new.platform).cloned() else {
            return false;
        };
        // Listening starts with the connection.
        if new.platform != old.platform
            || new.speech != old.speech
            || factory.requires_reconnect(&old.settings, &new.settings)
        {
            return false;
        }
        if new.audio.volume != old.audio.volume {
            self.engine.set_volume(new.audio.volume);
            self.volume = new.audio.volume;
        }
        if new.audio.bitrate != old.audio.bitrate {
            let cap = factory.capabilities().max_bitrate;
            self.engine
                .set_bitrate(new.audio.bitrate.unwrap_or(DEFAULT_BITRATE).min(cap));
        }
        if new.audio.target_lufs != old.audio.target_lufs {
            self.engine.set_target_lufs(new.audio.target_lufs);
        }
        if new.command_prefix != old.command_prefix {
            // Workers parse with the prefix they started with.
            self.command_workers.clear();
        }
        let description = new.now_playing_description != old.now_playing_description;
        // Reconnects use the new settings (e.g. the nickname); the settings updater applies
        // them to the running connection. Also when unchanged: saving again retries what the
        // platform could not apply (e.g. a channel it was refused).
        self.settings.send_replace(new.settings.clone());
        self.config = new;
        if description {
            if self.config.now_playing_description {
                self.update_description();
            } else {
                self.description.send_replace(String::new());
            }
        }
        tracing::info!("configuration applied without reconnecting");
        // Other clients refetch (name, audio settings).
        self.emit(InstanceEvent::PlaybackChanged {
            status: Box::new(self.status()),
        });
        true
    }

    /// Stops the engine and publishes the idle state (the player keeps its queue).
    fn go_idle(&mut self) {
        self.cancel_open();
        self.playing = None;
        self.engine.stop();
        self.update_description();
        self.playback_changed();
    }

    /// Moves to the next track (`skip` ignores repeat-one) and starts it, or goes idle.
    fn advance(&mut self, skip: bool) {
        let next = if skip {
            self.player.skip()
        } else {
            self.player.advance()
        };
        match next {
            Some(_) => self.start_current(),
            None => self.go_idle(),
        }
        self.emit(InstanceEvent::QueueChanged);
    }

    /// Starts a new playback generation for the current item; audio starts on [`Internal::Opened`].
    fn start_current(&mut self) {
        let Some(item) = self.player.current().cloned() else {
            return;
        };
        self.generation += 1;
        let generation = self.generation;
        self.playing = Some(Playing {
            generation,
            item: item.id,
            audible: false,
        });
        // Silence the previous track right away instead of letting it run while opening.
        self.engine.stop();
        if self.paused {
            self.engine.pause(false);
            self.paused = false;
        }
        self.playback_changed();
        let registry = self.services.sources.read().clone();
        let store = self.services.store.clone();
        let internal = self.internal.clone();
        // A newer playback supersedes this one: cancel its open (kills yt-dlp via kill_on_drop).
        self.cancel_open();
        let task = spawn_here(async move {
            let result = async {
                let input = registry.open(&item.track).await?;
                let key = registry.cache_key(&item.track).await;
                let lufs = match &key {
                    Some(key) => store.loudness(&item.track.source, key).await,
                    None => None,
                };
                Ok((input, lufs, key))
            }
            .await;
            let _ = internal.send(Internal::Opened { generation, result });
        });
        self.open_task = Some(task.abort_handle());
    }

    fn cancel_open(&mut self) {
        if let Some(task) = self.open_task.take() {
            task.abort();
        }
    }

    /// The current item, if `generation` is the active playback.
    fn current_item(&self, generation: u64) -> Option<QueueItem> {
        let playing = self.playing.filter(|p| p.generation == generation)?;
        self.player.current().filter(|i| i.id == playing.item).cloned()
    }

    /// Checks the request and answers with its id, then synthesizes in the background (Piper
    /// takes a moment) and queues the speech. Failures after the answer end in `SpeechEnded`.
    fn say(&mut self, text: String, voice: Option<String>, language: Option<String>, reply: Reply<u64>) {
        if self.speech.len() >= MAX_PENDING_SPEECH {
            let _ = reply.send(Err(SpeechQueueFull.into()));
            return;
        }
        let tts = self.services.tts.read().clone();
        // An explicit language picks among the voices; otherwise the instance's voice (if it
        // is still installed, else the default one).
        let voice = voice.or_else(|| {
            language
                .is_none()
                .then(|| self.config.tts_voice.clone())
                .flatten()
                .filter(|v| tts.has_voice(v))
        });
        let (text, voice) = match tts.prepare(&text, voice.as_deref(), language.as_deref()) {
            Ok(prepared) => prepared,
            Err(e) => {
                let _ = reply.send(Err(e));
                return;
            }
        };
        self.next_speech += 1;
        let id = self.next_speech;
        self.speech.insert(id, (text.clone(), None));
        let internal = self.internal.clone();
        let task = spawn_here(async move {
            let result = match tokio::time::timeout(SYNTHESIS_TIMEOUT, tts.speak(&text, &voice)).await {
                Ok(result) => result,
                Err(_) => Err(anyhow!("text to speech took too long")),
            };
            let _ = internal.send(Internal::Synthesized { id, result });
        });
        self.synthesizing.insert(id, AbortOnDrop(task.abort_handle()));
        let _ = reply.send(Ok(id));
    }

    /// Cancels the syntheses in progress and the queued and playing speech.
    fn stop_speech(&mut self) {
        let cancelled: Vec<u64> = self.synthesizing.drain().map(|(id, _)| id).collect();
        for id in cancelled {
            if self.speech.remove(&id).is_some() {
                self.emit(InstanceEvent::SpeechEnded {
                    id,
                    error: None,
                    stopped: true,
                });
            }
        }
        self.engine.stop_speech();
    }

    fn handle_internal(&mut self, internal: Internal) {
        match internal {
            Internal::Heard { client, transcript } => self.heard(*client, transcript),
            Internal::Synthesized { id, result } => {
                // Cancelled meanwhile: the result (and its audio file) is dropped.
                if self.synthesizing.remove(&id).is_none() {
                    return;
                }
                match result {
                    Ok(speech) => {
                        if let Some(entry) = self.speech.get_mut(&id) {
                            entry.1 = speech.temp;
                        }
                        self.engine.speak(SpeechRequest {
                            id,
                            input: speech.input,
                            known_lufs: speech.loudness,
                        });
                    }
                    Err(e) => {
                        let error = format!("{e:#}");
                        if self.failure_log.allow("tts") {
                            tracing::warn!(%error, "text to speech failed");
                        }
                        self.speech.remove(&id);
                        self.emit(InstanceEvent::SpeechEnded {
                            id,
                            error: Some(james_core::text::user_message(&error)),
                            stopped: false,
                        });
                    }
                }
            }
            Internal::Opened { generation, result } => {
                let Some(item) = self.current_item(generation) else {
                    return;
                };
                match result {
                    Ok((input, known_lufs, cache_key)) => {
                        self.cache_key = cache_key;
                        self.engine.play(PlayRequest {
                            id: generation,
                            input,
                            start: Duration::ZERO,
                            known_lufs,
                        });
                    }
                    Err(e) => self.track_failed(item, format!("{e:#}")),
                }
            }
        }
    }

    fn track_failed(&mut self, item: QueueItem, error: String) {
        // Users can make tracks fail at will: limited, so the log keeps other messages.
        if self.failure_log.allow("track") {
            tracing::warn!(track = ?item.track.title, %error, "track failed");
        }
        self.emit(InstanceEvent::TrackEnded {
            item,
            error: Some(james_core::text::user_message(&error)),
        });
        self.failures += 1;
        if self.failures >= MAX_CONSECUTIVE_FAILURES {
            tracing::warn!("too many consecutive failures, stopping playback");
            self.failures = 0;
            self.player.stop();
            self.go_idle();
            self.emit(InstanceEvent::QueueChanged);
        } else {
            self.advance(true);
        }
    }

    fn handle_engine(&mut self, event: EngineEvent) {
        match event {
            EngineEvent::SpeechStarted { id } => {
                if let Some((text, _)) = self.speech.get(&id) {
                    let text = text.clone();
                    self.emit(InstanceEvent::SpeechStarted { id, text });
                }
            }
            EngineEvent::SpeechFinished { id, error, stopped } => {
                // Dropping the entry removes the audio file.
                if self.speech.remove(&id).is_some() {
                    if let Some(error) = &error {
                        if self.failure_log.allow("speech") {
                            tracing::warn!(%error, "speech failed");
                        }
                    }
                    self.emit(InstanceEvent::SpeechEnded { id, error, stopped });
                }
            }
            EngineEvent::Started { id } => {
                let Some(item) = self.current_item(id) else { return };
                if let Some(playing) = &mut self.playing {
                    playing.audible = true;
                }
                self.failures = 0;
                tracing::info!(source = %item.track.source, title = ?item.track.title, "playing");
                self.emit(InstanceEvent::TrackStarted { item });
                self.update_description();
                self.playback_changed();
            }
            EngineEvent::Finished {
                id,
                error,
                measured_lufs,
                stopped,
            } => {
                if stopped {
                    return; // replaced or stopped by us
                }
                let Some(item) = self.current_item(id) else { return };
                match error {
                    Some(error) => self.track_failed(item, error),
                    None => {
                        if let (Some(lufs), Some(key)) = (measured_lufs, self.cache_key.take()) {
                            let store = self.services.store.clone();
                            let source = item.track.source.clone();
                            tokio::spawn(async move { store.save_loudness(&source, &key, lufs).await });
                        }
                        self.emit(InstanceEvent::TrackEnded { item, error: None });
                        self.advance(false);
                    }
                }
            }
        }
    }

    fn handle_platform(&mut self, event: PlatformEvent) {
        match &event {
            PlatformEvent::Connected { snapshot } => {
                self.connected = true;
                self.last_error = None;
                let own = snapshot.own_client.as_ref();
                if let Some(me) = snapshot.clients.iter().find(|c| Some(&c.id) == own) {
                    self.services.bot_uids.write().insert(me.uid.clone());
                }
                // Once per start (not on every reconnect).
                if self.config.speech.enabled && !self.speech_tasks.is_empty() && !self.announced {
                    self.announced = true;
                    self.announce_listening();
                }
                self.emit(InstanceEvent::Connected {
                    snapshot: Box::new(snapshot.clone()),
                });
                self.update_description();
                return;
            }
            PlatformEvent::Disconnected { reason } => {
                let reason = james_core::text::user_message(reason);
                let changed = self.connected || self.last_error.as_deref() != Some(reason.as_str());
                self.connected = false;
                self.last_error = Some(reason.clone());
                if changed {
                    self.emit(InstanceEvent::Disconnected { reason });
                    self.playback_changed();
                }
                return;
            }
            PlatformEvent::CredentialsUpdated { data } => {
                let (store, id, data) = (self.services.store.clone(), self.config.id.clone(), data.clone());
                let mut saves = self.handle.credential_saves.lock();
                saves.retain(|t| !t.is_finished());
                saves.push(tokio::spawn(async move {
                    if let Err(e) = store.save_credentials(&id, data).await {
                        tracing::error!(instance = %id, error = %format!("{e:#}"), "cannot save the credentials");
                    }
                }));
                return;
            }
            PlatformEvent::Message { mode, from, text } => {
                let is_command = text.trim_start().starts_with(&self.config.command_prefix);
                if is_command {
                    self.queue_command(CommandJob {
                        mode: *mode,
                        from: from.clone(),
                        text: text.clone(),
                        spoken: false,
                    });
                }
                let sensitive = is_command || *mode == MessageTarget::Client;
                self.emit_with(
                    InstanceEvent::Platform {
                        event: Box::new(event),
                    },
                    sensitive,
                );
                return;
            }
            PlatformEvent::Poke { .. } => {
                self.emit_with(
                    InstanceEvent::Platform {
                        event: Box::new(event),
                    },
                    true,
                );
                return;
            }
            _ => {}
        }
        self.emit(InstanceEvent::Platform {
            event: Box::new(event),
        });
    }

    /// Tells the bot's channel that it listens (people are not asked otherwise).
    fn announce_listening(&self) {
        let handle = self.handle.clone();
        spawn_here(async move {
            let text = "Speech recognition is on: what is said in this channel is turned into text.";
            if let Err(e) = handle.send_message(MessageTarget::Channel, None, text).await {
                tracing::debug!(error = %e, "cannot announce speech recognition");
            }
        });
    }

    /// Something a client said: to plugins (and web clients that may read chat), and run as a
    /// command if it starts with the wake word.
    fn heard(&mut self, client: Client, transcript: james_core::stt::Transcript) {
        tracing::debug!(chars = transcript.text.chars().count(), "speech recognised");
        let command = self.config.speech.wake_word.as_deref().and_then(|wake| {
            let spoken = crate::speech::spoken_command(&crate::speech::wake_command(&transcript.text, wake)?);
            let name = spoken.split_whitespace().next()?;
            // Anything else that sounds like the wake word is not answered.
            self.services
                .commands
                .get_for(name, &self.config.id)
                .filter(|c| c.spoken())?;
            Some(spoken)
        });
        if let Some(command) = command {
            self.queue_command(CommandJob {
                mode: MessageTarget::Client,
                from: client.clone(),
                text: format!("{}{command}", self.config.command_prefix),
                spoken: true,
            });
        }
        self.emit_with(
            InstanceEvent::SpeechRecognized {
                client: Box::new(client),
                text: transcript.text,
                language: transcript.language,
            },
            true,
        );
    }

    /// Runs chat commands of one client in order, with a per-client rate limit.
    fn queue_command(&mut self, job: CommandJob) {
        let key = if job.from.uid.is_empty() {
            job.from.id.clone()
        } else {
            job.from.uid.clone()
        };
        let now = Instant::now();
        let times = self.command_times.entry(key.clone()).or_default();
        while times
            .front()
            .is_some_and(|t| now.duration_since(*t) > COMMAND_WINDOW)
        {
            times.pop_front();
        }
        if times.len() >= COMMAND_RATE {
            tracing::debug!(client = %key, "command rate limit hit");
            return;
        }
        times.push_back(now);
        self.command_times.retain(|_, t| !t.is_empty());

        let job = match self.command_workers.get(&key) {
            Some(worker) => match worker.try_send(job) {
                Ok(()) => return,
                // The client already has a full queue: drop the command.
                Err(mpsc::error::TrySendError::Full(_)) => return,
                Err(mpsc::error::TrySendError::Closed(job)) => job,
            },
            None => job,
        };
        // No live worker for this client: start one.
        let (tx, rx) = mpsc::channel(COMMAND_QUEUE);
        let _ = tx.try_send(job);
        tokio::spawn(
            command_worker(self.handle.clone(), self.config.command_prefix.clone(), rx).in_current_span(),
        );
        self.command_workers.retain(|_, w| !w.is_closed());
        self.command_workers.insert(key, tx);
    }

    fn update_description(&self) {
        if !self.config.now_playing_description || !self.connected {
            return;
        }
        let text = match self.player.current() {
            // Titles come from websites: neutralise markup (e.g. BBCode images on TeamSpeak).
            Some(item) => james_core::text::escape_markup(&match &item.track.artist {
                Some(artist) => format!("♪ {} – {}", artist, item.track.title),
                None => format!("♪ {}", item.track.title),
            }),
            None => String::new(),
        };
        // TeamSpeak limits descriptions; escaping makes brackets longer, so cut afterwards.
        let text: String = text.chars().take(200).collect();
        // Always notify (also after reconnects); the updater sends only the latest value.
        self.description.send_replace(text);
    }
}

async fn command_worker(handle: InstanceHandle, prefix: String, mut jobs: mpsc::Receiver<CommandJob>) {
    while let Ok(Some(job)) = tokio::time::timeout(COMMAND_WORKER_IDLE, jobs.recv()).await {
        let commands = handle.services().commands.clone();
        // Anyone's microphone may pick up a command someone else said (or a clip played), so
        // speaking commands is a permission of its own.
        if job.spoken && !commands.allowed(&handle, &job.from, "voice.command").await {
            continue;
        }
        let reply = commands
            .dispatch(&prefix, &job.text, handle.clone(), job.from.clone(), job.mode)
            .await;
        if let Some(reply) = reply {
            if let Err(e) = handle.reply(job.mode, &job.from, reply).await {
                tracing::debug!(error = %e, "failed to send command reply");
            }
        }
    }
}
