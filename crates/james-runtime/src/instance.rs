//! A single bot instance: platform connection + audio engine + player.
//!
//! The instance runs as one control task. Everything else (adapter, engine,
//! track opening, command execution) runs in separate tasks and talks to it
//! through channels, so slow operations never block playback control.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context};
use james_audio::{AudioEngine, EngineConfig, EngineEvent, PlayRequest};
use james_core::platform::{
    Client, MessageTarget, PlatformCommand, PlatformContext, PlatformEvent, PlatformId, Snapshot,
};
use james_core::source::{MediaInput, Track};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

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
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AudioSettings {
    pub volume: u8,
    /// Opus bitrate in bit/s; defaults to the platform maximum (capped at 192 kbit/s).
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
    pub position_ms: u64,
    pub paused: bool,
    pub volume: u8,
    pub repeat: RepeatMode,
    pub shuffle: bool,
    pub queue_length: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum InstanceEvent {
    Started,
    Stopped,
    Connected { snapshot: Snapshot },
    Disconnected { reason: String },
    Platform { event: PlatformEvent },
    TrackStarted { item: QueueItem },
    TrackEnded { item: QueueItem, error: Option<String> },
    QueueChanged,
    PlaybackChanged { status: Box<InstanceStatus> },
}

/// An event tagged with the instance it belongs to (one bus for all instances).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub instance: String,
    #[serde(flatten)]
    pub event: InstanceEvent,
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
        index: usize,
        reply: Reply<QueueItem>,
    },
    Move {
        from: usize,
        to: usize,
        reply: Reply<()>,
    },
    Status {
        reply: oneshot::Sender<InstanceStatus>,
    },
    Queue {
        reply: oneshot::Sender<Vec<QueueItem>>,
    },
    Shutdown,
}

enum Internal {
    Opened {
        item: u64,
        result: anyhow::Result<MediaInput>,
    },
}

/// Cloneable handle to a running instance.
#[derive(Clone)]
pub struct InstanceHandle {
    id: Arc<str>,
    control: mpsc::UnboundedSender<Control>,
    platform: mpsc::Sender<PlatformCommand>,
    services: Services,
}

impl InstanceHandle {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn services(&self) -> &Services {
        &self.services
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
        let registry = self.services.sources.read().unwrap().clone();
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
    pub async fn remove(&self, index: usize) -> anyhow::Result<QueueItem> {
        self.ask(|reply| Control::Remove { index, reply }).await?
    }
    pub async fn move_item(&self, from: usize, to: usize) -> anyhow::Result<()> {
        self.ask(|reply| Control::Move { from, to, reply }).await?
    }
    pub async fn status(&self) -> anyhow::Result<InstanceStatus> {
        self.ask(|reply| Control::Status { reply }).await
    }
    pub async fn queue(&self) -> anyhow::Result<Vec<QueueItem>> {
        self.ask(|reply| Control::Queue { reply }).await
    }

    pub fn shutdown(&self) {
        let _ = self.control.send(Control::Shutdown);
    }

    /// Sends a command to the platform adapter and waits for the result.
    pub async fn platform<T>(
        &self,
        make: impl FnOnce(james_core::platform::Reply<T>) -> PlatformCommand,
    ) -> anyhow::Result<T> {
        let (tx, rx) = oneshot::channel();
        self.platform
            .send(make(tx))
            .await
            .map_err(|_| anyhow!("not connected"))?;
        tokio::time::timeout(Duration::from_secs(10), rx)
            .await
            .context("platform did not answer")?
            .map_err(|_| anyhow!("connection lost"))?
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

/// Maximum consecutive tracks that fail to open before playback stops.
const MAX_CONSECUTIVE_FAILURES: u32 = 5;

pub(crate) fn spawn(config: InstanceConfig, services: Services) -> anyhow::Result<InstanceHandle> {
    let factory = services
        .platforms
        .get(&config.platform)
        .cloned()
        .ok_or_else(|| anyhow!("unknown platform '{}'", config.platform))?;
    // Validate the settings before spawning anything.
    let probe = factory.create(&config.settings, None)?;
    let capabilities = probe.capabilities();
    drop(probe);

    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let (internal_tx, internal_rx) = mpsc::unbounded_channel();
    let (platform_tx, platform_rx) = mpsc::channel(64);
    let (platform_events_tx, platform_events_rx) = mpsc::unbounded_channel();
    let (audio_tx, audio_rx) = mpsc::channel(64);
    let (engine_events_tx, engine_events_rx) = mpsc::unbounded_channel();

    let bitrate = config
        .audio
        .bitrate
        .unwrap_or(capabilities.max_bitrate.min(192_000));
    let engine = AudioEngine::spawn(
        EngineConfig {
            ffmpeg: services.ffmpeg.clone(),
            target_lufs: config.audio.target_lufs,
            bitrate,
            volume: config.audio.volume,
            ..EngineConfig::default()
        },
        audio_tx,
        engine_events_tx,
    )?;

    let handle = InstanceHandle {
        id: config.id.as_str().into(),
        control: control_tx,
        platform: platform_tx,
        services: services.clone(),
    };

    // Adapter task with reconnect backoff.
    let adapter_config = config.clone();
    let adapter_services = services.clone();
    let adapter_task = tokio::spawn(async move {
        let mut ctx = PlatformContext {
            events: platform_events_tx,
            commands: platform_rx,
            audio: audio_rx,
        };
        let mut backoff = Duration::from_secs(1);
        loop {
            let credentials = adapter_services.store.load_credentials(&adapter_config.id);
            let started = tokio::time::Instant::now();
            let result = match factory.create(&adapter_config.settings, credentials) {
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
                    tracing::warn!(instance = %adapter_config.id, error = %format!("{e:#}"), retry_in = ?backoff, "platform connection failed");
                    let _ = ctx.events.send(PlatformEvent::Disconnected {
                        reason: format!("{e:#}"),
                    });
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(60));
                }
            }
        }
    });

    let config_volume = config.audio.volume;
    let task = InstanceTask {
        config,
        services,
        handle: handle.clone(),
        engine,
        player: Player::default(),
        connected: false,
        volume: config_volume,
        paused: false,
        internal: internal_tx,
        failures: 0,
        adapter_task,
    };
    tokio::spawn(task.run(control_rx, internal_rx, platform_events_rx, engine_events_rx));
    Ok(handle)
}

struct InstanceTask {
    config: InstanceConfig,
    services: Services,
    handle: InstanceHandle,
    engine: AudioEngine,
    player: Player,
    connected: bool,
    /// Requested state; the engine applies it asynchronously.
    volume: u8,
    paused: bool,
    internal: mpsc::UnboundedSender<Internal>,
    failures: u32,
    adapter_task: tokio::task::JoinHandle<()>,
}

impl InstanceTask {
    fn emit(&self, event: InstanceEvent) {
        let _ = self.services.events.send(EventEnvelope {
            instance: self.config.id.clone(),
            event,
        });
    }

    async fn run(
        mut self,
        mut control: mpsc::UnboundedReceiver<Control>,
        mut internal: mpsc::UnboundedReceiver<Internal>,
        mut platform: mpsc::UnboundedReceiver<PlatformEvent>,
        mut engine: mpsc::UnboundedReceiver<EngineEvent>,
    ) {
        tracing::info!(instance = %self.config.id, "instance started");
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
        self.engine.stop();
        // Dropping the handle's platform sender alone is not enough while clones exist.
        let _ = self.handle.platform.try_send(PlatformCommand::Disconnect {
            reason: "shutting down".into(),
        });
        let _ = tokio::time::timeout(Duration::from_secs(5), &mut self.adapter_task).await;
        self.adapter_task.abort();
        self.emit(InstanceEvent::Stopped);
        tracing::info!(instance = %self.config.id, "instance stopped");
    }

    fn status(&self) -> InstanceStatus {
        let engine = self.engine.status();
        InstanceStatus {
            id: self.config.id.clone(),
            name: self.config.name.clone(),
            platform: self.config.platform.clone(),
            connected: self.connected,
            current: self.player.current().cloned(),
            position_ms: engine.position.as_millis() as u64,
            paused: self.paused,
            volume: self.volume,
            repeat: self.player.repeat,
            shuffle: self.player.shuffle,
            queue_length: self.player.queue().len(),
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
                tracks,
                mode,
                requester,
                reply,
            } => {
                if tracks.is_empty() {
                    let _ = reply.send(Err(anyhow!("nothing to play")));
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
                    self.next(false);
                }
                self.emit(InstanceEvent::QueueChanged);
                let _ = reply.send(Ok(items));
            }
            Control::Skip => self.next(true),
            Control::Previous => {
                if self.player.previous().is_some() {
                    self.start_current();
                    self.emit(InstanceEvent::QueueChanged);
                }
            }
            Control::Stop => {
                self.engine.stop();
                self.player.stop();
                self.update_description();
                self.playback_changed();
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
                self.engine.seek(position);
                let mut status = self.status();
                status.position_ms = position.as_millis() as u64;
                self.emit(InstanceEvent::PlaybackChanged {
                    status: Box::new(status),
                });
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
            Control::Remove { index, reply } => {
                let result = self
                    .player
                    .remove(index)
                    .ok_or_else(|| anyhow!("no queue entry #{}", index + 1));
                if result.is_ok() {
                    self.emit(InstanceEvent::QueueChanged);
                }
                let _ = reply.send(result);
            }
            Control::Move { from, to, reply } => {
                let moved = self.player.move_item(from, to);
                if moved {
                    self.emit(InstanceEvent::QueueChanged);
                }
                let _ = reply.send(if moved {
                    Ok(())
                } else {
                    Err(anyhow!("invalid queue position"))
                });
            }
            Control::Status { reply } => {
                let _ = reply.send(self.status());
            }
            Control::Queue { reply } => {
                let _ = reply.send(self.player.queue().iter().cloned().collect());
            }
            Control::Shutdown => unreachable!("handled in run"),
        }
    }

    /// Advances to the next track (`skip` ignores repeat-one) and starts it, or goes idle.
    fn next(&mut self, skip: bool) {
        let next = if skip {
            self.player.skip()
        } else {
            self.player.advance()
        };
        match next {
            Some(_) => self.start_current(),
            None => {
                self.engine.stop();
                self.update_description();
                self.playback_changed();
            }
        }
        self.emit(InstanceEvent::QueueChanged);
    }

    /// Opens the current track asynchronously; playback starts on [`Internal::Opened`].
    fn start_current(&mut self) {
        let Some(item) = self.player.current().cloned() else {
            return;
        };
        if self.paused {
            self.engine.pause(false);
            self.paused = false;
        }
        let registry = self.services.sources.read().unwrap().clone();
        let internal = self.internal.clone();
        tokio::spawn(async move {
            let result = registry.open(&item.track).await;
            let _ = internal.send(Internal::Opened {
                item: item.id,
                result,
            });
        });
    }

    fn handle_internal(&mut self, internal: Internal) {
        match internal {
            Internal::Opened { item, result } => {
                let Some(current) = self.player.current().cloned() else {
                    return;
                };
                if current.id != item {
                    return; // skipped meanwhile
                }
                match result {
                    Ok(input) => {
                        self.engine.play(PlayRequest {
                            id: item,
                            input,
                            start: Duration::ZERO,
                            known_lufs: None,
                        });
                    }
                    Err(e) => self.track_failed(current, format!("{e:#}")),
                }
            }
        }
    }

    fn track_failed(&mut self, item: QueueItem, error: String) {
        tracing::warn!(instance = %self.config.id, track = %item.track.title, %error, "track failed");
        self.emit(InstanceEvent::TrackEnded {
            item,
            error: Some(error),
        });
        self.failures += 1;
        if self.failures >= MAX_CONSECUTIVE_FAILURES {
            tracing::warn!(instance = %self.config.id, "too many consecutive failures, stopping playback");
            self.failures = 0;
            self.engine.stop();
            self.player.stop();
            self.update_description();
            self.playback_changed();
        } else {
            self.next(true);
        }
    }

    fn handle_engine(&mut self, event: EngineEvent) {
        match event {
            EngineEvent::Started { id } => {
                if let Some(item) = self.player.current().filter(|i| i.id == id).cloned() {
                    self.failures = 0;
                    self.emit(InstanceEvent::TrackStarted { item });
                    self.update_description();
                    self.playback_changed();
                }
            }
            EngineEvent::Finished {
                id, error, stopped, ..
            } => {
                if stopped {
                    return; // replaced or stopped by us
                }
                let Some(item) = self.player.current().filter(|i| i.id == id).cloned() else {
                    return;
                };
                match error {
                    Some(error) => self.track_failed(item, error),
                    None => {
                        self.emit(InstanceEvent::TrackEnded { item, error: None });
                        self.next(false);
                    }
                }
            }
        }
    }

    fn handle_platform(&mut self, event: PlatformEvent) {
        match &event {
            PlatformEvent::Connected { snapshot } => {
                self.connected = true;
                self.emit(InstanceEvent::Connected {
                    snapshot: snapshot.clone(),
                });
                self.update_description();
                return;
            }
            PlatformEvent::Disconnected { reason } => {
                if self.connected {
                    self.connected = false;
                    self.emit(InstanceEvent::Disconnected {
                        reason: reason.clone(),
                    });
                }
                return;
            }
            PlatformEvent::CredentialsUpdated { data } => {
                self.services
                    .store
                    .save_credentials(&self.config.id, data.clone());
                return;
            }
            PlatformEvent::Message { mode, from, text }
                if text.trim_start().starts_with(&self.config.command_prefix) =>
            {
                let commands = self.services.commands.clone();
                let prefix = self.config.command_prefix.clone();
                let (handle, mode, from, text) = (self.handle.clone(), *mode, from.clone(), text.clone());
                tokio::spawn(async move {
                    if let Some(reply) = commands
                        .dispatch(&prefix, &text, handle.clone(), from.clone(), mode)
                        .await
                    {
                        if let Err(e) = handle.reply(mode, &from, reply).await {
                            tracing::debug!(error = %e, "failed to send command reply");
                        }
                    }
                });
            }
            _ => {}
        }
        self.emit(InstanceEvent::Platform { event });
    }

    fn update_description(&self) {
        if !self.config.now_playing_description || !self.connected {
            return;
        }
        let text = match self.player.current() {
            Some(item) => match &item.track.artist {
                Some(artist) => format!("♪ {} – {}", artist, item.track.title),
                None => format!("♪ {}", item.track.title),
            },
            None => String::new(),
        };
        let handle = self.handle.clone();
        tokio::spawn(async move {
            if let Err(e) = handle
                .platform(|reply| PlatformCommand::SetDescription { text, reply })
                .await
            {
                tracing::debug!(error = %e, "failed to update description");
            }
        });
    }
}
