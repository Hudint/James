//! Fakes for runtime tests: platform adapter, audio engine and source.

#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use james_audio::{EngineConfig, EngineEvent, PlayRequest};
use james_core::audio::OpusFrame;
use james_core::platform::{
    Capabilities, Client, PlatformAdapter, PlatformCommand, PlatformContext, PlatformEvent, PlatformFactory,
    Snapshot,
};
use james_core::source::{MediaInput, SourceRegistry, SourceResolver, Track};
use james_runtime::{EngineFactory, InstanceConfig, InstanceManager, MemoryStore, PlaybackEngine, Services};
use parking_lot::Mutex;
use tokio::sync::{mpsc, Notify};

// ---- platform ---------------------------------------------------------------------------

#[derive(Default)]
pub struct PlatformState {
    pub sent: Mutex<Vec<String>>,
    pub descriptions: Mutex<Vec<String>>,
    pub audio_frames: Mutex<usize>,
    /// Adapters created (= connections).
    pub connections: Mutex<usize>,
    pub nicknames: Mutex<Vec<String>>,
    pub inject: Mutex<Option<mpsc::UnboundedReceiver<PlatformEvent>>>,
}

pub struct FakePlatform(pub Arc<PlatformState>);

impl PlatformFactory for FakePlatform {
    fn id(&self) -> &str {
        "fake"
    }
    fn create(
        &self,
        _: &serde_json::Value,
        _: Option<serde_json::Value>,
    ) -> anyhow::Result<Box<dyn PlatformAdapter>> {
        *self.0.connections.lock() += 1;
        Ok(Box::new(FakeAdapter(self.0.clone())))
    }
    /// Like TeamSpeak: the nickname changes on the running connection.
    fn requires_reconnect(&self, old: &serde_json::Value, new: &serde_json::Value) -> bool {
        let without_nickname = |v: &serde_json::Value| {
            let mut v = v.clone();
            if let Some(map) = v.as_object_mut() {
                map.remove("nickname");
            }
            v
        };
        without_nickname(old) != without_nickname(new)
    }
    fn validate(&self, _: &serde_json::Value) -> anyhow::Result<()> {
        Ok(())
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            max_bitrate: 128_000,
            max_frame_bytes: None,
            ..Default::default()
        }
    }
    fn import_credentials(&self, input: &str) -> anyhow::Result<serde_json::Value> {
        Ok(serde_json::json!({ "key": input }))
    }
    fn describe_credentials(&self, data: &serde_json::Value) -> Option<serde_json::Value> {
        Some(data.clone())
    }
}

/// A store whose first credential save takes a while (like a busy database); later ones are
/// quick, so a slow old save could overtake them.
pub struct SlowStore {
    pub inner: james_runtime::MemoryStore,
    pub delay: Duration,
    pub saves: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl james_runtime::InstanceStore for SlowStore {
    async fn load_credentials(&self, instance: &str) -> Option<serde_json::Value> {
        self.inner.load_credentials(instance).await
    }
    async fn save_credentials(&self, instance: &str, data: serde_json::Value) -> anyhow::Result<()> {
        if self.saves.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            tokio::time::sleep(self.delay).await;
        }
        self.inner.save_credentials(instance, data).await
    }
    async fn loudness(&self, source: &str, locator: &str) -> Option<f32> {
        self.inner.loudness(source, locator).await
    }
    async fn save_loudness(&self, source: &str, locator: &str, lufs: f32) {
        self.inner.save_loudness(source, locator, lufs).await
    }
}

struct FakeAdapter(Arc<PlatformState>);

#[async_trait]
impl PlatformAdapter for FakeAdapter {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            max_bitrate: 128_000,
            max_frame_bytes: None,
            ..Default::default()
        }
    }

    async fn run(&mut self, ctx: &mut PlatformContext) -> anyhow::Result<()> {
        let mut inject = self.0.inject.lock().take();
        ctx.events.send(PlatformEvent::Connected {
            snapshot: Snapshot::default(),
        })?;
        loop {
            tokio::select! {
                Some(e) = async { match &mut inject { Some(rx) => rx.recv().await, None => std::future::pending().await } } => {
                    ctx.events.send(e)?;
                }
                c = ctx.commands.recv() => match c {
                    Some(PlatformCommand::SendMessage { text, reply, .. }) => {
                        self.0.sent.lock().push(text);
                        let _ = reply.send(Ok(()));
                    }
                    Some(PlatformCommand::UpdateSettings { settings, reply }) => {
                        if let Some(name) = settings.get("nickname").and_then(|n| n.as_str()) {
                            self.0.nicknames.lock().push(name.to_owned());
                        }
                        let _ = reply.send(Ok(()));
                    }
                    Some(PlatformCommand::SetDescription { text, reply }) => {
                        self.0.descriptions.lock().push(text);
                        let _ = reply.send(Ok(()));
                    }
                    Some(PlatformCommand::Disconnect { .. }) | None => return Ok(()),
                    Some(_) => {}
                },
                Some(frame) = ctx.audio.recv() => if !frame.data.is_empty() {
                    *self.0.audio_frames.lock() += 1;
                },
            }
        }
    }
}

pub fn user(name: &str) -> Client {
    Client {
        id: format!("id-{name}"),
        uid: format!("uid-{name}"),
        name: name.into(),
        ..Default::default()
    }
}

// ---- engine -----------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum Call {
    Play { id: u64, locator: String },
    Stop,
    Pause(bool),
    Volume(u8),
    Seek(Duration),
    Speak(u64),
    StopSpeech,
    Bitrate(u32),
    TargetLufs(i32),
}

#[derive(Default)]
pub struct EngineState {
    pub calls: Mutex<Vec<Call>>,
    pub events: Mutex<Option<mpsc::UnboundedSender<EngineEvent>>>,
}

impl EngineState {
    pub fn emit(&self, event: EngineEvent) {
        self.events
            .lock()
            .as_ref()
            .expect("engine spawned")
            .send(event)
            .unwrap();
    }

    pub fn plays(&self) -> Vec<(u64, String)> {
        self.calls
            .lock()
            .iter()
            .filter_map(|c| match c {
                Call::Play { id, locator } => Some((*id, locator.clone())),
                _ => None,
            })
            .collect()
    }

    /// Waits until the engine was asked to play `count` requests; returns the last one.
    pub async fn wait_plays(&self, count: usize) -> (u64, String) {
        for _ in 0..200 {
            let plays = self.plays();
            if plays.len() >= count {
                return plays[count - 1].clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("expected {count} play requests, got {:?}", self.plays());
    }
}

pub struct FakeEngineFactory(pub Arc<EngineState>);

impl EngineFactory for FakeEngineFactory {
    fn spawn(
        &self,
        _: EngineConfig,
        _: mpsc::Sender<OpusFrame>,
        events: mpsc::UnboundedSender<EngineEvent>,
    ) -> anyhow::Result<Box<dyn PlaybackEngine>> {
        *self.0.events.lock() = Some(events);
        Ok(Box::new(FakeEngine(self.0.clone())))
    }
}

struct FakeEngine(Arc<EngineState>);

impl PlaybackEngine for FakeEngine {
    fn play(&self, request: PlayRequest) {
        let locator = match request.input {
            MediaInput::File(path) => path.display().to_string(),
            MediaInput::Url { url, .. } => url,
            MediaInput::Process { args, .. } => args.last().cloned().unwrap_or_default(),
        };
        self.0.calls.lock().push(Call::Play {
            id: request.id,
            locator,
        });
    }
    fn stop(&self) {
        self.0.calls.lock().push(Call::Stop);
    }
    fn pause(&self, paused: bool) {
        self.0.calls.lock().push(Call::Pause(paused));
    }
    fn set_volume(&self, volume: u8) {
        self.0.calls.lock().push(Call::Volume(volume));
    }
    fn seek(&self, position: Duration) {
        self.0.calls.lock().push(Call::Seek(position));
    }
    fn set_bitrate(&self, bitrate: u32) {
        self.0.calls.lock().push(Call::Bitrate(bitrate));
    }
    fn set_target_lufs(&self, lufs: f32) {
        self.0.calls.lock().push(Call::TargetLufs(lufs as i32));
    }
    fn position(&self) -> Duration {
        Duration::ZERO
    }
    fn speak(&self, request: james_audio::SpeechRequest) {
        self.0.calls.lock().push(Call::Speak(request.id));
    }
    fn stop_speech(&self) {
        self.0.calls.lock().push(Call::StopSpeech);
    }
}

// ---- source -----------------------------------------------------------------------------

/// Resolves "a,b,c" to tracks a, b, c. Opening `slow*` waits for `gate`, `fail*` fails.
#[derive(Default)]
pub struct FakeSource {
    pub gate: Notify,
}

#[async_trait]
impl SourceResolver for FakeSource {
    fn id(&self) -> &str {
        "fake"
    }
    fn accepts(&self, _: &str) -> bool {
        true
    }
    async fn resolve(&self, input: &str) -> anyhow::Result<Vec<Track>> {
        Ok(input.split(',').map(|name| track(name.trim())).collect())
    }
    async fn open(&self, track: &Track) -> anyhow::Result<MediaInput> {
        if track.locator.starts_with("slow") {
            self.gate.notified().await;
        }
        if track.locator.starts_with("fail") {
            anyhow::bail!("cannot open {}", track.locator);
        }
        Ok(MediaInput::File(track.locator.clone().into()))
    }
}

pub fn track(name: &str) -> Track {
    Track {
        source: "fake".into(),
        locator: name.into(),
        title: name.into(),
        artist: None,
        album: None,
        duration: None,
        thumbnail: None,
        is_stream: false,
    }
}

// ---- harness ----------------------------------------------------------------------------

pub struct Harness {
    pub manager: InstanceManager,
    pub platform: Arc<PlatformState>,
    pub engine: Arc<EngineState>,
    pub source: Arc<FakeSource>,
    pub inject: mpsc::UnboundedSender<PlatformEvent>,
}

impl Harness {
    pub async fn start() -> Self {
        Self::start_with_store(Arc::new(MemoryStore::default())).await
    }

    pub async fn start_with_store(store: Arc<dyn james_runtime::InstanceStore>) -> Self {
        let platform = Arc::new(PlatformState::default());
        let (inject, inject_rx) = mpsc::unbounded_channel();
        *platform.inject.lock() = Some(inject_rx);
        let engine = Arc::new(EngineState::default());
        let source = Arc::new(FakeSource::default());
        let mut sources = SourceRegistry::default();
        sources.register(0, source.clone());
        let services = Services::new(
            sources,
            vec![Arc::new(FakePlatform(platform.clone()))],
            Arc::new(FakeEngineFactory(engine.clone())),
            store,
            "ffmpeg",
        );
        james_runtime::builtin::register(&services.commands);
        let manager = InstanceManager::new(services);
        manager
            .upsert(InstanceConfig {
                id: "test".into(),
                platform: "fake".into(),
                ..Default::default()
            })
            .unwrap();
        let mut events = manager.subscribe();
        manager.start("test").await.unwrap();
        loop {
            if let james_runtime::InstanceEvent::Connected { .. } = events.recv().await.unwrap().event {
                break;
            }
        }
        Self {
            manager,
            platform,
            engine,
            source,
            inject,
        }
    }

    pub fn handle(&self) -> james_runtime::InstanceHandle {
        self.manager.get("test").unwrap()
    }
}

/// Polls `condition` until it holds (or panics after 3 s). Avoids fixed sleeps in tests.
pub async fn eventually<F, Fut>(what: &str, mut condition: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while !condition().await {
        if tokio::time::Instant::now() > deadline {
            panic!("timed out waiting for: {what}");
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
