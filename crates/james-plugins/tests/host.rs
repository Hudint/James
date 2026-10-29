//! End to end: real Node.js plugin host + fixture plugins + an instance on a fake platform.
//!
//! Skipped when Node.js (24+) is not available (`JAMES_NODE` or `node` on the PATH).

#[path = "../../james-runtime/tests/common/mod.rs"]
mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use common::{user, FakeEngineFactory, FakePlatform, FakeSource, PlatformState};
use james_core::platform::{MessageTarget, PlatformEvent};
use james_core::source::SourceRegistry;
use james_db::permissions::Grant;
use james_db::store::PluginState;
use james_db::Db;
use james_plugins::{HttpEvent, PluginManager, PluginSettings, PluginStatus};
use james_runtime::{InstanceConfig, InstanceManager, MemoryStore, Services};
use serde_json::json;
use tokio::sync::mpsc;

fn node() -> Option<String> {
    let node = std::env::var("JAMES_NODE").unwrap_or_else(|_| "node".into());
    let version = std::process::Command::new(&node).arg("--version").output().ok()?;
    let major: u32 = String::from_utf8_lossy(&version.stdout)
        .trim()
        .trim_start_matches('v')
        .split('.')
        .next()?
        .parse()
        .ok()?;
    (major >= 24).then_some(node)
}

async fn wait(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !condition() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn plugins_run_end_to_end() {
    let Some(node) = node() else {
        eprintln!("Node.js 24+ not found, skipping");
        return;
    };
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let data = tempfile::tempdir().unwrap();

    let platform = Arc::new(PlatformState::default());
    let (inject, inject_rx) = mpsc::unbounded_channel();
    *platform.inject.lock() = Some(inject_rx);
    let mut sources = SourceRegistry::default();
    sources.register(0, Arc::new(FakeSource::default()));
    let services = Services::new(
        sources,
        vec![Arc::new(FakePlatform(platform.clone()))],
        Arc::new(FakeEngineFactory(Default::default())),
        Arc::new(MemoryStore::default()),
        "ffmpeg",
    );
    james_runtime::builtin::register(&services.commands);
    let instances = Arc::new(InstanceManager::new(services));
    instances
        .upsert(InstanceConfig {
            id: "test".into(),
            platform: "fake".into(),
            ..Default::default()
        })
        .unwrap();

    let db = Db::memory().await.unwrap();
    db.save_plugin_state(&PluginState {
        id: "greeter".into(),
        enabled: true,
        granted_permissions: vec!["web".into()],
        config: json!({}),
        instances: None,
    })
    .await
    .unwrap();
    db.set_plugin_instance_config("greeter", "test", &json!({ "greeting": "Hi" }))
        .await
        .unwrap();
    db.save_plugin_state(&PluginState {
        id: "accounts".into(),
        enabled: true,
        granted_permissions: vec!["users".into()],
        config: json!({}),
        instances: None,
    })
    .await
    .unwrap();
    // An administrative role that is not called "admin", and its member.
    db.create_role("owner", "").await.unwrap();
    db.set_role_grants("owner", &[Grant::global("*")]).await.unwrap();
    let owner = db
        .create_user("olivia", Some("password-123"), false)
        .await
        .unwrap();
    db.set_user_roles(owner.id, &["owner".into()]).await.unwrap();
    db.link_identity("fake", "uid-owner", owner.id, "Olivia")
        .await
        .unwrap();
    let target = db.create_user("tom", None, false).await.unwrap();

    let plugins = PluginManager::new(
        db.clone(),
        instances.clone(),
        PluginSettings {
            node,
            host_script: root.join("plugin-host/src/main.ts").canonicalize().unwrap(),
            plugins_dir: root
                .join("plugin-host/test/fixtures/plugins")
                .canonicalize()
                .unwrap(),
            data_dir: data.path().to_path_buf(),
            http_proxy: None,
        },
    );
    plugins.start().await;
    instances.start("test").await.unwrap();

    wait("greeter active", || plugins.has_context("greeter", "test")).await;
    wait("accounts active", || plugins.has_context("accounts", "test")).await;
    let legacy = plugins.get("legacy").unwrap();
    assert_eq!(legacy.status, PluginStatus::Disabled, "not enabled");
    assert!(!plugins.has_context("legacy", "test"));

    // Event → plugin → platform.
    inject
        .send(PlatformEvent::ClientEnter {
            client: user("alice"),
        })
        .unwrap();
    wait("greeting", || {
        platform.sent.lock().iter().any(|t| t == "Hi, alice!")
    })
    .await;

    // Chat command registered by the plugin.
    inject
        .send(PlatformEvent::Message {
            mode: MessageTarget::Client,
            from: user("alice"),
            text: "!hello there".into(),
        })
        .unwrap();
    wait("command reply", || {
        platform.sent.lock().iter().any(|t| t == "Hello alice (1) there")
    })
    .await;

    // James-enforced permissions hold, whatever the plugin tries.
    inject
        .send(PlatformEvent::Message {
            mode: MessageTarget::Client,
            from: user("alice"),
            text: "!kickme".into(),
        })
        .unwrap();
    wait("kick refused", || {
        platform
            .sent
            .lock()
            .iter()
            .any(|t| t.contains("lacks the permission 'moderation'"))
    })
    .await;
    inject
        .send(PlatformEvent::Message {
            mode: MessageTarget::Client,
            from: user("alice"),
            text: format!("!accounts {} {}", target.id, owner.id),
        })
        .unwrap();
    wait("account attempts", || {
        platform.sent.lock().iter().any(|t| t.starts_with("grant owner:"))
    })
    .await;
    let report = platform
        .sent
        .lock()
        .iter()
        .find(|t| t.starts_with("grant owner:"))
        .cloned()
        .unwrap();
    assert!(
        report.contains("grant owner: plugins cannot grant the role 'owner'"),
        "{report}"
    );
    assert!(
        report.contains("password of owner: accounts with administrative permissions"),
        "{report}"
    );
    assert!(
        report.contains("steal identity: this identity is linked to another account"),
        "{report}"
    );
    assert!(report.contains("grant dj: allowed"), "{report}");
    assert_eq!(
        db.user_by_identity("fake", "uid-owner")
            .await
            .unwrap()
            .unwrap()
            .id,
        owner.id
    );

    // Plugins cannot read James' working directory (Node lets worker threads read their own
    // working directory, so plugin processes run in their data directory).
    let own_file = std::env::current_dir().unwrap().join("Cargo.toml");
    assert!(own_file.exists());
    inject
        .send(PlatformEvent::Message {
            mode: MessageTarget::Client,
            from: user("alice"),
            text: format!("!readfile {}", own_file.display()),
        })
        .unwrap();
    wait("read attempt", || {
        platform.sent.lock().iter().any(|t| t.starts_with("read:"))
    })
    .await;
    let read = platform
        .sent
        .lock()
        .iter()
        .find(|t| t.starts_with("read:"))
        .cloned()
        .unwrap();
    assert_eq!(read, "read: ERR_ACCESS_DENIED");

    // HTTP event.
    let response = plugins
        .http_event(
            "greeter",
            "test",
            HttpEvent {
                name: "ping".into(),
                public: false,
                method: "POST".into(),
                body: json!("x"),
                query: Default::default(),
                remote_addr: "127.0.0.1".into(),
                user: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(response["pong"], "x");

    // Disabling stops the plugin and removes its command.
    plugins.configure("greeter", false, vec![], None).await.unwrap();
    assert!(!plugins.has_context("greeter", "test"));
    assert!(instances.services().commands.get("hello").is_none());

    // Enabling returns as soon as the plugin runs (not after a timeout).
    let started = std::time::Instant::now();
    let info = plugins
        .configure("greeter", true, vec!["web".into()], None)
        .await
        .unwrap();
    assert_eq!(info.status, PluginStatus::Running);
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "took {:?}",
        started.elapsed()
    );
    wait("greeter active again", || plugins.has_context("greeter", "test")).await;
    plugins.configure("greeter", false, vec![], None).await.unwrap();

    // Enabling without the requested permission is refused.
    assert!(plugins.configure("greeter", true, vec![], None).await.is_err());

    // Per instance: on for one keeps the granted permissions; a plugin that was active on all
    // instances gets an explicit list; off for the last one disables it.
    let all = ["test".to_owned(), "other".to_owned()];
    plugins
        .configure("greeter", true, vec!["web".into()], None)
        .await
        .unwrap();
    let info = plugins
        .set_active("greeter", "other", false, None, &all)
        .await
        .unwrap();
    assert!(info.state.enabled);
    assert_eq!(info.state.instances, Some(vec!["test".to_owned()]));
    assert_eq!(info.state.granted_permissions, vec!["web".to_owned()]);
    wait("greeter active on test", || {
        plugins.has_context("greeter", "test")
    })
    .await;
    let info = plugins
        .set_active("greeter", "test", false, None, &all)
        .await
        .unwrap();
    assert!(!info.state.enabled);
    assert_eq!(info.state.instances, Some(vec![]));
    assert!(!plugins.has_context("greeter", "test"));
    let info = plugins
        .set_active("greeter", "test", true, None, &all)
        .await
        .unwrap();
    assert_eq!(info.state.instances, Some(vec!["test".to_owned()]));
    wait("greeter active again on test", || {
        plugins.has_context("greeter", "test")
    })
    .await;
    // Without the requested permission it cannot be activated, but it can always be taken off
    // an instance (e.g. while an update waits for approval).
    plugins
        .configure("greeter", true, vec![], Some(vec!["test".into(), "other".into()]))
        .await
        .unwrap_err();
    plugins.configure("greeter", false, vec![], None).await.unwrap();
    assert!(plugins
        .set_active("greeter", "test", true, None, &all)
        .await
        .is_err());
    let info = plugins
        .set_active("greeter", "test", true, Some(vec!["web".into()]), &all)
        .await
        .unwrap();
    assert_eq!(info.state.instances, Some(vec!["test".to_owned()]));
    // Permissions change without touching where it runs.
    let info = plugins
        .set_permissions("greeter", vec!["web".into()], false)
        .await
        .unwrap();
    assert_eq!(info.state.instances, Some(vec!["test".to_owned()]));
    assert!(info.state.enabled);
    // Only instances that exist are kept (this test runtime has only "test").
    let info = plugins
        .set_active("greeter", "other", true, None, &all)
        .await
        .unwrap();
    assert_eq!(info.state.instances, Some(vec!["test".to_owned()]));
}
