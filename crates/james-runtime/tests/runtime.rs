//! Instance runtime tests with a fake platform adapter.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use james_core::platform::{
    Capabilities, Client, MessageTarget, PlatformAdapter, PlatformCommand, PlatformContext, PlatformEvent,
    Snapshot,
};
use james_core::source::SourceRegistry;
use james_runtime::{InstanceConfig, InstanceEvent, InstanceManager, MemoryStore, PlatformFactory, Services};
use james_sources::local::LocalResolver;
use tokio::sync::mpsc;

#[derive(Default)]
struct Shared {
    sent: Mutex<Vec<String>>,
    audio_frames: Mutex<usize>,
    inject: Mutex<Option<mpsc::UnboundedReceiver<PlatformEvent>>>,
}

struct FakeFactory(Arc<Shared>);

impl PlatformFactory for FakeFactory {
    fn id(&self) -> &str {
        "fake"
    }
    fn create(
        &self,
        _: &serde_json::Value,
        _: Option<serde_json::Value>,
    ) -> anyhow::Result<Box<dyn PlatformAdapter>> {
        Ok(Box::new(FakeAdapter(self.0.clone())))
    }
}

struct FakeAdapter(Arc<Shared>);

#[async_trait]
impl PlatformAdapter for FakeAdapter {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            max_bitrate: 128_000,
            ..Default::default()
        }
    }

    async fn run(&mut self, ctx: &mut PlatformContext) -> anyhow::Result<()> {
        let mut inject = self.0.inject.lock().unwrap().take().expect("single connection");
        ctx.events.send(PlatformEvent::Connected {
            snapshot: Snapshot::default(),
        })?;
        loop {
            tokio::select! {
                Some(e) = inject.recv() => ctx.events.send(e)?,
                c = ctx.commands.recv() => match c {
                    Some(PlatformCommand::SendMessage { text, reply, .. }) => {
                        self.0.sent.lock().unwrap().push(text);
                        let _ = reply.send(Ok(()));
                    }
                    Some(PlatformCommand::SetDescription { reply, .. }) => { let _ = reply.send(Ok(())); }
                    Some(PlatformCommand::Disconnect { .. }) | None => return Ok(()),
                    Some(_) => {}
                },
                Some(frame) = ctx.audio.recv() => if !frame.data.is_empty() {
                    *self.0.audio_frames.lock().unwrap() += 1;
                },
            }
        }
    }
}

fn user() -> Client {
    Client {
        id: "5".into(),
        uid: "uid-5".into(),
        name: "Alice".into(),
        channel: None,
        server_groups: vec![],
        is_self: false,
    }
}

#[tokio::test]
async fn chat_command_plays_local_file() {
    let dir = tempfile::tempdir().unwrap();
    let status = std::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "sine=duration=1",
        ])
        .args(["-metadata", "title=Beep"])
        .arg(dir.path().join("beep.flac"))
        .status()
        .unwrap();
    assert!(status.success());

    let shared = Arc::new(Shared::default());
    let (inject_tx, inject_rx) = mpsc::unbounded_channel();
    *shared.inject.lock().unwrap() = Some(inject_rx);

    let mut sources = SourceRegistry::default();
    sources.register(
        0,
        Arc::new(LocalResolver::new(vec![dir.path().into()], "ffprobe")),
    );
    let services = Services::new(
        sources,
        vec![Arc::new(FakeFactory(shared.clone()))],
        Arc::new(MemoryStore::default()),
        "ffmpeg",
    );
    james_runtime::builtin::register(&services.commands);
    let manager = InstanceManager::new(services);
    let mut events = manager.subscribe();
    manager
        .upsert(InstanceConfig {
            id: "test".into(),
            platform: "fake".into(),
            ..Default::default()
        })
        .unwrap();
    manager.start("test").unwrap();

    // Wait for the connection, then send a chat command.
    loop {
        if let InstanceEvent::Connected { .. } = events.recv().await.unwrap().event {
            break;
        }
    }
    inject_tx
        .send(PlatformEvent::Message {
            mode: MessageTarget::Channel,
            from: user(),
            text: "!play local:beep.flac".into(),
        })
        .unwrap();

    let mut started = false;
    let ended = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match events.recv().await.unwrap().event {
                InstanceEvent::TrackStarted { item } => {
                    assert_eq!(item.track.title, "Beep");
                    assert_eq!(item.requested_by.unwrap().name, "Alice");
                    started = true;
                }
                InstanceEvent::TrackEnded { item, error } => break (item, error),
                _ => {}
            }
        }
    })
    .await
    .expect("track did not finish");
    assert!(started);
    assert_eq!(ended.1, None);
    assert_eq!(ended.0.track.title, "Beep");
    assert_eq!(
        shared.sent.lock().unwrap().first().map(String::as_str),
        Some("Playing: Beep (0:01)")
    );
    assert!(*shared.audio_frames.lock().unwrap() >= 49);

    // Unknown command and status queries.
    inject_tx
        .send(PlatformEvent::Message {
            mode: MessageTarget::Client,
            from: user(),
            text: "!nope".into(),
        })
        .unwrap();
    inject_tx
        .send(PlatformEvent::Message {
            mode: MessageTarget::Client,
            from: user(),
            text: "!vol 80".into(),
        })
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let sent = shared.sent.lock().unwrap().clone();
    assert!(
        sent.iter().any(|m| m.starts_with("Unknown command 'nope'")),
        "{sent:?}"
    );
    assert!(sent.iter().any(|m| m == "Volume: 80"), "{sent:?}");
    assert_eq!(manager.get("test").unwrap().status().await.unwrap().volume, 80);

    manager.stop_all();
}
