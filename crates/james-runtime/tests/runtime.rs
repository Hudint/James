//! End-to-end instance test: chat command → local source → real audio engine → fake platform.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{user, FakePlatform, PlatformState};
use james_core::platform::{MessageTarget, PlatformEvent};
use james_core::source::SourceRegistry;
use james_runtime::{
    AudioEngineFactory, InstanceConfig, InstanceEvent, InstanceManager, MemoryStore, Services,
};
use james_sources::local::LocalResolver;
use tokio::sync::mpsc;

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

    let platform = Arc::new(PlatformState::default());
    let (inject, inject_rx) = mpsc::unbounded_channel();
    *platform.inject.lock() = Some(inject_rx);

    let mut sources = SourceRegistry::default();
    sources.register(
        0,
        Arc::new(LocalResolver::new(vec![dir.path().into()], "ffprobe")),
    );
    let store = Arc::new(MemoryStore::default());
    let services = Services::new(
        sources,
        vec![Arc::new(FakePlatform(platform.clone()))],
        Arc::new(AudioEngineFactory::new(tokio::runtime::Handle::current())),
        store.clone(),
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
    manager.start("test").await.unwrap();

    loop {
        if let InstanceEvent::Connected { .. } = events.recv().await.unwrap().event {
            break;
        }
    }
    let alice = user("Alice");
    inject
        .send(PlatformEvent::Message {
            mode: MessageTarget::Channel,
            from: alice.clone(),
            text: "!play local:beep.flac".into(),
        })
        .unwrap();

    let mut started = false;
    let (item, error) = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match events.recv().await.unwrap().event {
                InstanceEvent::TrackStarted { item } => {
                    assert_eq!(item.track.title, "Beep");
                    assert_eq!(item.requested_by.as_ref().unwrap().name, "Alice");
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
    assert_eq!(error, None);
    assert_eq!(item.track.title, "Beep");
    assert_eq!(
        platform.sent.lock().first().map(String::as_str),
        Some("Playing: Beep (0:01)")
    );
    assert!(*platform.audio_frames.lock() >= 49);
    assert!(
        platform.descriptions.lock().iter().any(|d| d.contains("Beep")),
        "now playing description"
    );
    let registry = manager.services().sources.read().clone();
    let key = registry
        .cache_key(&item.track)
        .await
        .expect("local files are cacheable");
    assert!(
        key.starts_with(&item.track.locator),
        "key includes the file identity: {key}"
    );
    common::eventually("loudness saved", || async {
        james_runtime::InstanceStore::loudness(&*store, "local", &key)
            .await
            .is_some()
    })
    .await;
    assert!(
        james_runtime::InstanceStore::loudness(&*store, "local", &key)
            .await
            .is_some(),
        "loudness of a complete play is cached"
    );

    // Commands of one client run in order.
    for text in ["!nope", "!vol 80", "!vol"] {
        inject
            .send(PlatformEvent::Message {
                mode: MessageTarget::Client,
                from: alice.clone(),
                text: text.into(),
            })
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let sent = platform.sent.lock().clone();
    assert_eq!(
        &sent[1..],
        ["Unknown command 'nope'. Try !help.", "Volume: 80", "Volume: 80"],
        "{sent:?}"
    );

    // Rate limit: a flood of commands is cut off.
    for _ in 0..30 {
        inject
            .send(PlatformEvent::Message {
                mode: MessageTarget::Client,
                from: alice.clone(),
                text: "!vol".into(),
            })
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let replies = platform.sent.lock().len() - sent.len();
    assert!(replies <= 8, "rate limited, got {replies} replies");

    manager.stop_all().await;
}
