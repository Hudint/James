//! Instance state machine tests with a fake engine (no ffmpeg involved).

mod common;

use std::time::Duration;

use common::{eventually, Call, Harness};
use james_audio::EngineEvent;
use james_runtime::{InstanceEvent, PlayMode, RepeatMode};

fn finished(id: u64) -> EngineEvent {
    EngineEvent::Finished {
        id,
        error: None,
        measured_lufs: None,
        stopped: false,
    }
}

#[tokio::test]
async fn skip_while_opening_ignores_the_stale_open() {
    let h = Harness::start().await;
    let instance = h.handle();
    instance.play("slow-a", PlayMode::Now, None).await.unwrap();
    instance.play("b", PlayMode::Now, None).await.unwrap();
    let (id_b, locator) = h.engine.wait_plays(1).await;
    assert_eq!(locator, "b");
    // The slow open of "a" completes now, after it was replaced.
    h.source.gate.notify_waiters();
    // A status round trip guarantees the control task processed everything sent before.
    tokio::time::sleep(Duration::from_millis(20)).await;
    instance.status().await.unwrap();
    assert_eq!(
        h.engine.plays(),
        vec![(id_b, "b".into())],
        "stale open must not start playback"
    );
}

#[tokio::test]
async fn stale_finished_after_previous_does_not_advance() {
    let h = Harness::start().await;
    let instance = h.handle();
    instance.play("a,b", PlayMode::Now, None).await.unwrap();
    let (gen_a, _) = h.engine.wait_plays(1).await;
    h.engine.emit(EngineEvent::Started { id: gen_a });
    instance.skip().unwrap();
    h.engine.wait_plays(2).await;
    instance.previous().unwrap();
    let (gen_a2, locator) = h.engine.wait_plays(3).await;
    assert_eq!(locator, "a");
    assert_ne!(gen_a, gen_a2, "replaying an item uses a new generation");
    // The first playback of "a" reports its end late; then a fresh event for the current one.
    h.engine.emit(finished(gen_a));
    h.engine.emit(EngineEvent::Started { id: gen_a2 });
    eventually("current playback audible", || async {
        !instance.status().await.unwrap().loading
    })
    .await;
    let status = instance.status().await.unwrap();
    assert_eq!(status.current.unwrap().track.title, "a");
    assert_eq!(h.engine.plays().len(), 3, "no advance on a stale event");
}

#[tokio::test]
async fn repeat_one_replays_and_advance_follows_queue() {
    let h = Harness::start().await;
    let instance = h.handle();
    instance.play("a,b", PlayMode::Now, None).await.unwrap();
    let (gen1, _) = h.engine.wait_plays(1).await;
    instance.set_repeat(RepeatMode::One).unwrap();
    // Control and engine events use different channels: sync on the control channel first.
    instance.status().await.unwrap();
    h.engine.emit(EngineEvent::Started { id: gen1 });
    h.engine.emit(finished(gen1));
    let (gen2, locator) = h.engine.wait_plays(2).await;
    assert_eq!(locator, "a");
    instance.set_repeat(RepeatMode::Off).unwrap();
    instance.status().await.unwrap();
    h.engine.emit(finished(gen2));
    let (gen3, locator) = h.engine.wait_plays(3).await;
    assert_eq!(locator, "b");
    h.engine.emit(finished(gen3));
    eventually("queue exhausted", || async {
        instance.status().await.unwrap().current.is_none()
    })
    .await;
    assert_eq!(h.engine.calls.lock().last(), Some(&Call::Stop));
}

#[tokio::test]
async fn consecutive_failures_stop_playback() {
    let h = Harness::start().await;
    let instance = h.handle();
    let mut events = h.manager.subscribe();
    instance
        .play("fail1,fail2,fail3,fail4,fail5,fail6,good", PlayMode::Now, None)
        .await
        .unwrap();
    let mut errors = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while errors < 5 {
        let envelope = tokio::time::timeout_at(deadline, events.recv())
            .await
            .expect("timeout")
            .unwrap();
        if let InstanceEvent::TrackEnded { error: Some(_), .. } = envelope.event {
            errors += 1;
        }
    }
    let status = instance.status().await.unwrap();
    assert!(status.current.is_none(), "playback stopped after 5 failures");
    assert_eq!(status.queue_length, 2, "remaining tracks stay queued");
    assert!(h.engine.plays().is_empty());
}

#[tokio::test]
async fn engine_errors_count_as_failures_and_started_resets() {
    let h = Harness::start().await;
    let instance = h.handle();
    instance.play("a,b", PlayMode::Now, None).await.unwrap();
    let (gen_a, _) = h.engine.wait_plays(1).await;
    h.engine.emit(EngineEvent::Finished {
        id: gen_a,
        error: Some("403".into()),
        measured_lufs: None,
        stopped: false,
    });
    let (gen_b, locator) = h.engine.wait_plays(2).await;
    assert_eq!(locator, "b", "a failed decoder advances");
    assert!(
        instance.status().await.unwrap().loading,
        "not audible before Started"
    );
    h.engine.emit(EngineEvent::Started { id: gen_b });
    eventually("audible after Started", || async {
        !instance.status().await.unwrap().loading
    })
    .await;
}

#[tokio::test]
async fn pause_is_lifted_when_a_new_track_starts() {
    let h = Harness::start().await;
    let instance = h.handle();
    instance.play("a", PlayMode::Now, None).await.unwrap();
    h.engine.wait_plays(1).await;
    instance.pause(true).unwrap();
    assert!(instance.status().await.unwrap().paused);
    instance.play("b", PlayMode::Now, None).await.unwrap();
    h.engine.wait_plays(2).await;
    assert!(!instance.status().await.unwrap().paused);
    assert!(h.engine.calls.lock().contains(&Call::Pause(false)));
}

#[tokio::test]
async fn seek_while_loading_is_ignored() {
    let h = Harness::start().await;
    let instance = h.handle();
    instance.play("a", PlayMode::Now, None).await.unwrap();
    h.engine.wait_plays(1).await;
    instance.seek(Duration::from_secs(30)).unwrap();
    instance.status().await.unwrap();
    assert!(
        !h.engine.calls.lock().iter().any(|c| matches!(c, Call::Seek(_))),
        "not audible yet"
    );
}

#[tokio::test]
async fn queue_is_bounded() {
    let h = Harness::start().await;
    let instance = h.handle();
    let names: Vec<String> = (0..james_runtime::instance::MAX_QUEUE + 10)
        .map(|i| format!("t{i}"))
        .collect();
    let added = instance
        .play(&names.join(","), PlayMode::Append, None)
        .await
        .unwrap();
    assert_eq!(
        added.len(),
        james_runtime::instance::MAX_QUEUE,
        "truncated to the free space"
    );
    // The first track became current (idle start), which leaves room for exactly one more.
    assert_eq!(
        instance.status().await.unwrap().queue_length,
        james_runtime::instance::MAX_QUEUE - 1
    );
    instance.play("one-more", PlayMode::Append, None).await.unwrap();
    assert!(instance
        .play("too-many", PlayMode::Append, None)
        .await
        .unwrap_err()
        .to_string()
        .contains("full"));
}

#[tokio::test]
async fn concurrent_lifecycle_operations_never_duplicate_instances() {
    let h = Harness::start().await;
    let manager = &h.manager;
    let (a, b, c) = tokio::join!(
        manager.restart("test"),
        manager.start("test"),
        manager.restart("test")
    );
    a.unwrap();
    b.unwrap();
    c.unwrap();
    assert_eq!(manager.running().len(), 1);
    manager.stop("test").await;
    assert!(manager.get("test").is_none());
    manager.start("test").await.unwrap();
    assert!(manager.get("test").unwrap().is_alive());
    manager.stop_all().await;
    assert!(manager.running().is_empty());
}

#[tokio::test]
async fn queue_operations_use_stable_ids() {
    let h = Harness::start().await;
    let instance = h.handle();
    instance.play("a", PlayMode::Now, None).await.unwrap();
    let queued = instance.play("b,c,d", PlayMode::Append, None).await.unwrap();
    let (b, d) = (queued[0].id, queued[2].id);
    instance.move_item(d, 0).await.unwrap();
    let titles: Vec<String> = instance
        .queue()
        .await
        .unwrap()
        .into_iter()
        .map(|i| i.track.title)
        .collect();
    assert_eq!(titles, ["d", "b", "c"]);
    instance.remove(b).await.unwrap();
    assert!(
        instance.remove(b).await.is_err(),
        "a stale id is rejected, nothing else is removed"
    );
    assert_eq!(instance.queue().await.unwrap().len(), 2);
    instance.jump(d).await.unwrap();
    let (_, locator) = h.engine.wait_plays(2).await;
    assert_eq!(locator, "d", "jump starts a new playback of the item");
    assert_eq!(instance.status().await.unwrap().current.unwrap().track.title, "d");
}
