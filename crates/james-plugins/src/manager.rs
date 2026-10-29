//! Plugin registry and lifecycle.
//!
//! James is the source of truth. Discovery runs in a separate process (see [`crate::host`]).
//! Every enabled plugin whose requested permissions are granted runs in its own process,
//! supervised here: it is told to create and remove per-instance contexts
//! (`context.start` / `context.stop`) and receives the events of those instances
//! (`instance.event`). Everything a plugin asks for goes through [`crate::bridge`] with the
//! plugin id bound to its connection, which enforces the granted permissions.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::StreamExt;
use james_db::store::PluginState;
use james_db::Db;
use james_runtime::{EventEnvelope, InstanceEvent, InstanceManager};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot, Semaphore};

use crate::command::PluginCommand;
use crate::ipc::{Handler, IpcError, Peer};
use crate::manifest::{self, Manifest};

/// Timeout for context requests (starting runs the plugin's `setup`).
const CONTEXT_TIMEOUT: Duration = Duration::from_secs(30);
/// Contexts of one plugin started at the same time.
const PARALLEL_STARTS: usize = 8;
/// Crashes within [`CRASH_WINDOW`] after which a plugin stays failed.
const MAX_CRASHES: usize = 3;
const CRASH_WINDOW: Duration = Duration::from_secs(5 * 60);
const RESTART_DELAYS: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(5),
    Duration::from_secs(15),
];
/// Public HTTP events of one plugin in flight.
const PARALLEL_PUBLIC: usize = 8;
const MAX_CONFIG_BYTES: usize = 64 * 1024;
const MAX_COMMANDS_PER_PLUGIN: usize = 64;
const MAX_WS_PER_PLUGIN: usize = 256;
const MAX_WS_PER_USER: usize = 8;
/// Notices a plugin may post per minute.
const NOTICES_PER_MINUTE: usize = 6;
/// Largest payload of a custom event between plugins.
const MAX_EVENT_BYTES: usize = 64 * 1024;
const RECONCILE_INTERVAL: Duration = Duration::from_secs(30);
/// Files checked for symlinks in one plugin (bounds the walk).
const MAX_PLUGIN_FILES: usize = 100_000;

#[derive(Debug, Clone)]
pub struct PluginSettings {
    /// Node.js executable.
    pub node: String,
    /// Entry point of the plugin host (`plugin-host/src/main.ts`).
    pub host_script: PathBuf,
    /// Directory containing the plugins.
    pub plugins_dir: PathBuf,
    /// Base directory for the plugins' private data (`<data>/<plugin id>/`).
    pub data_dir: PathBuf,
    /// Egress proxy used for the plugins' HTTP requests.
    pub http_proxy: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum PluginStatus {
    Disabled,
    /// Requests permissions that were not (yet) granted.
    NeedsApproval {
        missing: Vec<String>,
    },
    Starting,
    Running,
    Failed {
        error: String,
    },
    /// Plugins cannot run (Node.js is missing or discovery failed).
    Unavailable,
}

/// State of the plugin system as a whole (discovery, Node.js).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum HostStatus {
    Starting,
    Running,
    Unavailable { error: String },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginInfo {
    pub manifest: Manifest,
    pub state: PluginState,
    pub status: PluginStatus,
    /// Instances the plugin is currently active on.
    pub active_instances: Vec<String>,
    pub commands: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryError {
    pub id: String,
    pub error: String,
}

/// An HTTP endpoint registered by a plugin (`bot.web.handle`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebHandler {
    #[serde(default)]
    pub public: bool,
    /// Permission callers need (full id `plugin.<id>.<name>`); `None` = `plugins.use`.
    #[serde(default)]
    pub permission: Option<String>,
}

struct Entry {
    manifest: Manifest,
    state: PluginState,
    status: PluginStatus,
    commands: Vec<String>,
    web: HashMap<String, WebHandler>,
}

#[derive(Default)]
struct Inner {
    host: Option<HostStatus>,
    plugins: BTreeMap<String, Entry>,
    errors: Vec<DiscoveryError>,
    /// (plugin, instance) pairs active (or starting) in the plugin processes.
    contexts: BTreeSet<(String, String)>,
}

/// A plugin WebSocket connection of a web client.
pub(crate) struct WsConnection {
    pub plugin: String,
    pub instance: String,
    pub user: i64,
    pub sender: mpsc::Sender<String>,
}

/// A running plugin process.
struct Process {
    generation: u64,
    peer: Option<Peer>,
    task: tokio::task::AbortHandle,
    public: Arc<Semaphore>,
}

pub struct PluginManager {
    pub(crate) db: Db,
    pub(crate) instances: Arc<InstanceManager>,
    pub(crate) settings: PluginSettings,
    inner: RwLock<Inner>,
    processes: Mutex<HashMap<String, Process>>,
    generation: AtomicU64,
    /// Discovery replaces all plugins (write); everything else runs alongside (read).
    discovery: tokio::sync::RwLock<()>,
    /// Serialises lifecycle changes of one plugin (process, contexts, configuration).
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Wakes the reconcile loop (coalesces bursts of instance starts/stops).
    reconcile_wanted: tokio::sync::Notify,
    /// Plugins being reconciled → whether another run was requested meanwhile.
    reconciling: Mutex<HashMap<String, bool>>,
    pub(crate) ws: Mutex<HashMap<u64, WsConnection>>,
    /// Serialises store writes (quota check and write).
    pub(crate) store_lock: tokio::sync::Mutex<()>,
    notices: Mutex<HashMap<String, VecDeque<Instant>>>,
    /// Messages per plugin and instance that reach the instance's log.
    log_limit: james_core::logs::RateLimiter,
    /// "Overloaded" warnings per plugin and instance (one per minute).
    overload_log: james_core::logs::RateLimiter,
    next_ws: AtomicU64,
    this: Weak<PluginManager>,
}

impl PluginManager {
    pub fn new(db: Db, instances: Arc<InstanceManager>, settings: PluginSettings) -> Arc<Self> {
        Arc::new_cyclic(|this| Self {
            db,
            instances,
            settings,
            inner: RwLock::default(),
            processes: Mutex::default(),
            generation: AtomicU64::new(1),
            discovery: tokio::sync::RwLock::new(()),
            locks: Mutex::default(),
            reconcile_wanted: tokio::sync::Notify::new(),
            reconciling: Mutex::default(),
            ws: Mutex::default(),
            store_lock: tokio::sync::Mutex::new(()),
            notices: Mutex::default(),
            log_limit: james_core::logs::RateLimiter::new(120, Duration::from_secs(60)),
            overload_log: james_core::logs::RateLimiter::new(1, Duration::from_secs(60)),
            next_ws: AtomicU64::new(1),
            this: this.clone(),
        })
    }

    fn plugin_lock(&self, id: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.locks.lock().entry(id.to_owned()).or_default().clone()
    }

    /// Asks the reconcile loop to run soon.
    pub fn request_reconcile(&self) {
        self.reconcile_wanted.notify_one();
    }

    /// Reconciles on request and every [`RECONCILE_INTERVAL`] (retries failed starts).
    async fn reconcile_loop(self: Arc<Self>) {
        loop {
            let _ = tokio::time::timeout(RECONCILE_INTERVAL, self.reconcile_wanted.notified()).await;
            self.reconcile().await;
        }
    }

    fn arc(&self) -> Arc<Self> {
        self.this
            .upgrade()
            .expect("the plugin manager is alive while used")
    }

    /// Discovers the plugins, starts the enabled ones and follows instance events. Returns
    /// when discovery is done.
    pub async fn start(self: &Arc<Self>) {
        tokio::spawn(self.clone().forward_events());
        tokio::spawn(self.clone().reconcile_loop());
        if let Err(e) = self.rediscover().await {
            tracing::warn!(error = %format!("{e:#}"), "plugins are unavailable");
        }
    }

    pub fn host_status(&self) -> HostStatus {
        self.inner.read().host.clone().unwrap_or(HostStatus::Starting)
    }

    pub fn plugins_dir(&self) -> &std::path::Path {
        &self.settings.plugins_dir
    }

    pub fn discovery_errors(&self) -> Vec<DiscoveryError> {
        self.inner.read().errors.clone()
    }

    pub fn list(&self) -> Vec<PluginInfo> {
        let inner = self.inner.read();
        inner
            .plugins
            .iter()
            .map(|(id, e)| info(id, e, &inner.contexts))
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<PluginInfo> {
        let inner = self.inner.read();
        inner.plugins.get(id).map(|e| info(id, e, &inner.contexts))
    }

    pub(crate) fn is_running(&self, plugin: &str) -> bool {
        self.inner
            .read()
            .plugins
            .get(plugin)
            .is_some_and(|e| e.status == PluginStatus::Running)
    }

    /// Directory of a running plugin's web page, if it has one and may serve it.
    pub fn web_dir(&self, plugin: &str) -> Option<PathBuf> {
        let inner = self.inner.read();
        let entry = inner.plugins.get(plugin)?;
        if entry.status != PluginStatus::Running
            || !entry.state.granted_permissions.iter().any(|p| p == "web")
        {
            return None;
        }
        entry.manifest.web_dir.clone()
    }

    pub fn has_context(&self, plugin: &str, instance: &str) -> bool {
        self.inner
            .read()
            .contexts
            .contains(&(plugin.to_owned(), instance.to_owned()))
    }

    pub(crate) fn granted(&self, plugin: &str, permission: &str) -> bool {
        self.inner
            .read()
            .plugins
            .get(plugin)
            .is_some_and(|e| e.state.granted_permissions.iter().any(|p| p == permission))
    }

    fn peer(&self, plugin: &str) -> Option<Peer> {
        self.processes
            .lock()
            .get(plugin)
            .and_then(|p| p.peer.clone())
            .filter(|p| !p.is_closed())
    }

    pub(crate) async fn request(
        &self,
        plugin: &str,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, IpcError> {
        let peer = self.peer(plugin).ok_or(IpcError::Closed)?;
        peer.request(method, params, timeout).await
    }

    fn current_generation(&self, plugin: &str) -> Option<u64> {
        self.processes.lock().get(plugin).map(|p| p.generation)
    }

    /// Sets the status unless the process `generation` was replaced or stopped meanwhile.
    fn set_status_of(&self, plugin: &str, generation: u64, status: PluginStatus) {
        // Lock order processes → inner (as in `process_ready`).
        let processes = self.processes.lock();
        if processes.get(plugin).is_some_and(|p| p.generation == generation) {
            self.set_status(plugin, status);
        }
    }

    fn set_status(&self, plugin: &str, status: PluginStatus) {
        if let Some(entry) = self.inner.write().plugins.get_mut(plugin) {
            entry.status = status;
        }
    }

    // ---- discovery ------------------------------------------------------------------------

    /// Stops every plugin, discovers again and starts the enabled ones.
    async fn rediscover(&self) -> anyhow::Result<()> {
        let _guard = self.discovery.write().await;
        self.stop_all().await;
        let discovery = match crate::host::discover(&self.settings).await {
            Ok(discovery) => discovery,
            Err(e) => {
                let error = format!("{e:#}");
                let mut inner = self.inner.write();
                inner.host = Some(HostStatus::Unavailable { error });
                for entry in inner.plugins.values_mut() {
                    if entry.state.enabled {
                        entry.status = PluginStatus::Unavailable;
                    }
                }
                return Err(e);
            }
        };
        let states: HashMap<String, PluginState> = self
            .db
            .plugin_states()
            .await?
            .into_iter()
            .map(|s| (s.id.clone(), s))
            .collect();
        let mut errors = discovery.errors;
        let mut plugins = BTreeMap::new();
        // The discovery process ran plugin code: check what it reports (walks the plugins'
        // files, so off the async workers).
        let root = self.settings.plugins_dir.clone();
        let checked = tokio::task::spawn_blocking(move || {
            let mut symlinks = HashMap::new();
            discovery
                .plugins
                .into_iter()
                .map(|mut manifest| {
                    let result = check_manifest(&root, &mut manifest, &mut symlinks);
                    (manifest, result)
                })
                .collect::<Vec<_>>()
        })
        .await;
        let checked = match checked {
            Ok(checked) => checked,
            Err(e) => {
                let mut inner = self.inner.write();
                inner.host = Some(HostStatus::Unavailable {
                    error: format!("checking the plugins failed: {e}"),
                });
                // The processes were stopped above.
                for entry in inner.plugins.values_mut() {
                    if entry.state.enabled {
                        entry.status = PluginStatus::Unavailable;
                    }
                }
                anyhow::bail!("checking the plugins failed: {e}");
            }
        };
        let mut withdrawn = Vec::new();
        for (manifest, result) in checked {
            if let Err(error) = result {
                errors.push(DiscoveryError {
                    id: manifest.id.clone(),
                    error,
                });
                continue;
            }
            if plugins.contains_key(&manifest.id) {
                errors.push(DiscoveryError {
                    id: manifest.id.clone(),
                    error: "two plugins use this id".into(),
                });
                plugins.remove(&manifest.id);
                continue;
            }
            let mut state = states.get(&manifest.id).cloned().unwrap_or_else(|| PluginState {
                id: manifest.id.clone(),
                enabled: false,
                granted_permissions: Vec::new(),
                config: json!({}),
                instances: None,
            });
            // Grants the plugin no longer offers are withdrawn for good: if a later version
            // asks again, the administrator has to approve it again.
            let granted = state.granted_permissions.len();
            state.granted_permissions.retain(|p| manifest.grantable(p));
            if state.granted_permissions.len() != granted {
                withdrawn.push(state.clone());
            }
            plugins.insert(
                manifest.id.clone(),
                Entry {
                    status: PluginStatus::Disabled,
                    manifest,
                    state,
                    commands: Vec::new(),
                    web: HashMap::new(),
                },
            );
        }
        for state in &withdrawn {
            tracing::info!(plugin = %state.id, "withdrew permissions the plugin no longer offers");
            if let Err(e) = self.db.save_plugin_state(state).await {
                tracing::warn!(plugin = %state.id, error = %e, "cannot save the plugin's permissions");
            }
        }
        let ids: Vec<String> = plugins.keys().cloned().collect();
        {
            let mut inner = self.inner.write();
            inner.plugins = plugins;
            inner.errors = errors;
            inner.host = Some(HostStatus::Running);
        }
        for id in ids {
            self.start_process(&id);
        }
        Ok(())
    }

    // ---- processes ------------------------------------------------------------------------

    /// Starts the process of an enabled, approved plugin; returns when the first start
    /// attempt finished (or immediately if the plugin does not run).
    fn start_process(&self, id: &str) -> Option<oneshot::Receiver<()>> {
        let (manifest, permissions) = {
            let mut inner = self.inner.write();
            let entry = inner.plugins.get_mut(id)?;
            if !entry.state.enabled {
                entry.status = PluginStatus::Disabled;
                return None;
            }
            let missing: Vec<String> = entry
                .manifest
                .permissions
                .iter()
                .filter(|p| !entry.state.granted_permissions.contains(p))
                .cloned()
                .collect();
            if !missing.is_empty() {
                entry.status = PluginStatus::NeedsApproval { missing };
                return None;
            }
            entry.status = PluginStatus::Starting;
            (entry.manifest.clone(), entry.state.granted_permissions.clone())
        };
        let generation = self.generation.fetch_add(1, Ordering::Relaxed);
        let (started, started_rx) = oneshot::channel();
        // The entry exists before the supervisor runs (it may finish immediately).
        let mut processes = self.processes.lock();
        let task = tokio::spawn(supervise(self.arc(), manifest, permissions, generation, started));
        processes.insert(
            id.to_owned(),
            Process {
                generation,
                peer: None,
                task: task.abort_handle(),
                public: Arc::new(Semaphore::new(PARALLEL_PUBLIC)),
            },
        );
        Some(started_rx)
    }

    /// Stops a plugin's process (its `unload` handlers get a few seconds) and forgets its
    /// runtime state.
    async fn stop_process(&self, id: &str) {
        let process = self.processes.lock().remove(id);
        if let Some(process) = process {
            if let Some(peer) = process.peer {
                let _ = peer
                    .request("plugin.unload", json!({ "id": id }), Duration::from_secs(5))
                    .await;
            }
            process.task.abort();
        }
        self.forget_plugin_state(id);
    }

    /// Drops contexts, commands, web handlers and WebSockets of a plugin that stopped.
    fn forget_plugin_state(&self, id: &str) {
        let commands = {
            let mut inner = self.inner.write();
            inner.contexts.retain(|(plugin, _)| plugin != id);
            match inner.plugins.get_mut(id) {
                Some(entry) => {
                    entry.web.clear();
                    std::mem::take(&mut entry.commands)
                }
                None => Vec::new(),
            }
        };
        let registry = &self.instances.services().commands;
        for name in commands {
            registry.unregister(&name);
        }
        self.ws.lock().retain(|_, c| c.plugin != id);
    }

    /// Called by the supervisor when the plugin is loaded.
    fn process_ready(&self, id: &str, generation: u64, peer: Peer) -> bool {
        let mut processes = self.processes.lock();
        match processes.get_mut(id) {
            Some(process) if process.generation == generation => {
                process.peer = Some(peer);
                // Under the processes lock: a concurrent stop cannot interleave.
                if let Some(entry) = self.inner.write().plugins.get_mut(id) {
                    entry.status = PluginStatus::Running;
                }
                true
            }
            _ => false,
        }
    }

    /// Called by the supervisor when the process ended; returns whether it should restart.
    fn process_ended(&self, id: &str, generation: u64) -> bool {
        let current = self
            .processes
            .lock()
            .get_mut(id)
            .filter(|p| p.generation == generation)
            .map(|p| p.peer = None)
            .is_some();
        if current {
            self.forget_plugin_state(id);
        }
        current
    }

    fn process_failed(&self, id: &str, generation: u64, error: String) {
        let mut processes = self.processes.lock();
        if processes.get(id).is_some_and(|p| p.generation == generation) {
            processes.remove(id);
            drop(processes);
            tracing::warn!(plugin = %id, %error, "plugin failed");
            self.set_status(id, PluginStatus::Failed { error });
        }
    }

    // ---- contexts -------------------------------------------------------------------------

    /// Starts and stops contexts so that every running plugin is active on exactly the
    /// running instances it is enabled for.
    pub async fn reconcile(&self) {
        let ids: Vec<String> = self.inner.read().plugins.keys().cloned().collect();
        for id in ids {
            self.reconcile_in_background(id);
        }
    }

    /// Reconciles one plugin in its own task, so a slow plugin never delays the others; a
    /// request while it runs makes it run once more afterwards.
    fn reconcile_in_background(&self, id: String) {
        {
            let mut reconciling = self.reconciling.lock();
            if let Some(again) = reconciling.get_mut(&id) {
                *again = true;
                return;
            }
            reconciling.insert(id.clone(), false);
        }
        let this = self.arc();
        tokio::spawn(async move {
            // Even if reconciling panics, the plugin can be reconciled again later.
            struct Done(Arc<PluginManager>, String, bool);
            impl Drop for Done {
                fn drop(&mut self) {
                    if self.2 {
                        self.0.reconciling.lock().remove(&self.1);
                    }
                }
            }
            let mut done = Done(this.clone(), id.clone(), true);
            loop {
                {
                    let _discovery = this.discovery.read().await;
                    let lock = this.plugin_lock(&id);
                    let _guard = lock.lock().await;
                    this.reconcile_plugin(&id).await;
                }
                let mut reconciling = this.reconciling.lock();
                if reconciling.get(&id) == Some(&true) {
                    reconciling.insert(id.clone(), false);
                } else {
                    // Removed under the same lock as the check: no request is lost.
                    reconciling.remove(&id);
                    done.2 = false;
                    return;
                }
            }
        });
    }

    /// Stops every plugin process (unload handlers run), in parallel.
    async fn stop_all(&self) {
        let ids: Vec<String> = self.processes.lock().keys().cloned().collect();
        futures::future::join_all(ids.iter().map(|id| self.stop_process(id))).await;
    }

    /// Stops all plugins (on shutdown): their `unload` handlers run and pending store writes
    /// are saved.
    pub async fn shutdown(&self) {
        let stop = async {
            let _guard = self.discovery.write().await;
            self.stop_all().await;
        };
        if tokio::time::timeout(Duration::from_secs(15), stop).await.is_err() {
            tracing::warn!("plugins did not stop in time");
        }
    }

    async fn reconcile_plugin(&self, id: &str) {
        let Some(peer) = self.peer(id) else { return };
        let running: Vec<(String, String)> = self
            .instances
            .running()
            .iter()
            .map(|h| (h.id().to_owned(), h.platform_id().to_owned()))
            .collect();
        let (desired, current) = {
            let inner = self.inner.read();
            let Some(entry) = inner
                .plugins
                .get(id)
                .filter(|e| e.status == PluginStatus::Running)
            else {
                return;
            };
            let desired: BTreeMap<String, String> = running
                .into_iter()
                .filter(|(instance, platform)| {
                    // The administrator's selection wins over the manifest's `autorun`.
                    let allowed = entry
                        .state
                        .instances
                        .as_ref()
                        .is_none_or(|list| list.contains(instance));
                    allowed && entry.manifest.supports(platform)
                })
                .collect();
            let current: Vec<String> = inner
                .contexts
                .iter()
                .filter(|(p, _)| p == id)
                .map(|(_, i)| i.clone())
                .collect();
            (desired, current)
        };
        for instance in current.iter().filter(|i| !desired.contains_key(*i)) {
            self.stop_context(id, &peer, instance).await;
        }
        let starts: Vec<(String, String)> = desired
            .into_iter()
            .filter(|(instance, _)| !current.contains(instance))
            .collect();
        futures::stream::iter(starts)
            .for_each_concurrent(PARALLEL_STARTS, |(instance, platform)| {
                let peer = peer.clone();
                async move { self.start_context(id, &peer, &instance, &platform).await }
            })
            .await;
    }

    async fn start_context(&self, plugin: &str, peer: &Peer, instance: &str, platform: &str) {
        let key = (plugin.to_owned(), instance.to_owned());
        let config = self.instance_config(plugin, instance).await;
        // Registered before the start so events during `setup` are delivered (the plugin
        // process queues them behind the start).
        self.inner.write().contexts.insert(key.clone());
        let result = peer
            .request(
                "context.start",
                json!({ "instance": instance, "platform": platform, "config": config }),
                CONTEXT_TIMEOUT,
            )
            .await;
        if result.is_ok() {
            tracing::info!(%plugin, %instance, "plugin started");
        }
        if let Err(e) = result {
            tracing::warn!(%plugin, %instance, error = %e, "failed to start plugin context");
            self.inner.write().contexts.remove(&key);
            // The plugin may still be starting the context: make sure it is gone.
            let _ = peer
                .request("context.stop", json!({ "instance": instance }), CONTEXT_TIMEOUT)
                .await;
        }
    }

    async fn stop_context(&self, plugin: &str, peer: &Peer, instance: &str) {
        if let Err(e) = peer
            .request("context.stop", json!({ "instance": instance }), CONTEXT_TIMEOUT)
            .await
        {
            tracing::warn!(%plugin, %instance, error = %e, "failed to stop plugin context");
        } else {
            tracing::info!(%plugin, %instance, "plugin stopped");
        }
        self.inner
            .write()
            .contexts
            .remove(&(plugin.to_owned(), instance.to_owned()));
        // Web clients of the plugin on that instance are disconnected.
        self.ws
            .lock()
            .retain(|_, c| !(c.plugin == plugin && c.instance == instance));
    }

    pub(crate) async fn instance_config(&self, plugin: &str, instance: &str) -> Value {
        match self.db.plugin_instance_configs(plugin).await {
            Ok(configs) => configs
                .into_iter()
                .find(|(i, _)| i == instance)
                .map(|(_, c)| c)
                .unwrap_or_else(|| json!({})),
            Err(e) => {
                tracing::warn!(%plugin, error = %e, "cannot read plugin configuration");
                json!({})
            }
        }
    }

    // ---- administration -----------------------------------------------------------------

    /// Enables or disables a plugin. Enabling grants exactly `permissions`, which must cover
    /// what the manifest requests; `instances` = `None` activates it on all instances.
    /// Waits until the plugin started (or failed to).
    pub async fn configure(
        &self,
        id: &str,
        enabled: bool,
        permissions: Vec<String>,
        instances: Option<Vec<String>>,
    ) -> Result<PluginInfo, ConfigureError> {
        self.update_state(id, |_| Change {
            enabled,
            permissions,
            instances,
            only_removes: false,
        })
        .await
    }

    /// Replaces the granted permissions (and enables the plugin with `enable`), keeping where it
    /// runs; also restarts it (e.g. after it failed).
    pub async fn set_permissions(
        &self,
        id: &str,
        permissions: Vec<String>,
        enable: bool,
    ) -> Result<PluginInfo, ConfigureError> {
        self.update_state(id, |state| Change {
            enabled: state.enabled || enable,
            permissions,
            instances: state.instances.clone(),
            only_removes: false,
        })
        .await
    }

    /// Activates or deactivates a plugin on one instance, keeping the others. `permissions`
    /// replaces the granted ones (`None` keeps them); `all` lists every instance: a plugin that
    /// was active on all of them gets an explicit list (instances created later are not
    /// included), and ids of deleted instances are dropped. Not for `autorun` plugins (they run
    /// on every instance).
    pub async fn set_active(
        &self,
        id: &str,
        instance: &str,
        active: bool,
        permissions: Option<Vec<String>>,
        all: &[String],
    ) -> Result<PluginInfo, ConfigureError> {
        if self
            .inner
            .read()
            .plugins
            .get(id)
            .is_some_and(|e| e.manifest.autorun)
        {
            return Err(ConfigureError::Invalid(
                "this plugin always runs on every instance".into(),
            ));
        }
        self.update_state(id, |state| {
            let mut instances = match (&state.instances, state.enabled) {
                (_, false) => Vec::new(),
                (None, true) => all.to_vec(),
                (Some(list), true) => list.clone(),
            };
            instances.retain(|i| i != instance && all.contains(i));
            if active {
                instances.push(instance.to_owned());
            }
            let only_removes = !active && permissions.is_none();
            Change {
                enabled: !instances.is_empty(),
                permissions: permissions.unwrap_or_else(|| state.granted_permissions.clone()),
                instances: Some(instances),
                only_removes,
            }
        })
        .await
    }

    /// Forgets a deleted instance in every plugin's instance list (the database was updated
    /// with the instance's deletion).
    pub async fn forget_instance(&self, instance: &str) {
        let _discovery = self.discovery.read().await;
        let ids: Vec<String> = self.inner.read().plugins.keys().cloned().collect();
        for id in ids {
            // Under the plugin's lock: a change in progress cannot write the old list back.
            let lock = self.plugin_lock(&id);
            let _guard = lock.lock().await;
            let state = {
                let inner = self.inner.read();
                let Some(entry) = inner.plugins.get(&id) else {
                    continue;
                };
                let Some(list) = &entry.state.instances else {
                    continue;
                };
                if !list.iter().any(|i| i == instance) {
                    continue;
                }
                let mut state = entry.state.clone();
                let list: Vec<String> = list.iter().filter(|i| *i != instance).cloned().collect();
                state.enabled = state.enabled && !list.is_empty();
                state.instances = Some(list);
                state
            };
            if let Err(e) = self.db.save_plugin_state(&state).await {
                tracing::warn!(plugin = %id, error = %e, "cannot save the plugin's instances");
            }
            if let Some(entry) = self.inner.write().plugins.get_mut(&id) {
                entry.state = state;
            }
        }
    }

    /// Replaces a plugin's state with what `change` makes of the current one (under the
    /// plugin's lock), saves it and restarts the plugin.
    async fn update_state(
        &self,
        id: &str,
        change: impl FnOnce(&PluginState) -> Change,
    ) -> Result<PluginInfo, ConfigureError> {
        if !self.inner.read().plugins.contains_key(id) {
            return Err(ConfigureError::NotFound);
        }
        let started = {
            let _discovery = self.discovery.read().await;
            let lock = self.plugin_lock(id);
            let _guard = lock.lock().await;
            let state = {
                let inner = self.inner.read();
                let entry = inner.plugins.get(id).ok_or(ConfigureError::NotFound)?;
                let Change {
                    enabled,
                    permissions,
                    mut instances,
                    only_removes,
                } = change(&entry.state);
                // Only instances that exist (one may have been deleted meanwhile).
                if let Some(list) = &mut instances {
                    let existing: Vec<String> = self.instances.configs().into_iter().map(|c| c.id).collect();
                    list.retain(|i| existing.contains(i));
                }
                if let Some(unknown) = permissions.iter().find(|p| manifest::permission(p).is_none()) {
                    return Err(ConfigureError::Invalid(format!("unknown permission '{unknown}'")));
                }
                // Taking it off an instance needs no approval of what an update asks for.
                if enabled && !only_removes {
                    if let Some(missing) = entry
                        .manifest
                        .permissions
                        .iter()
                        .find(|p| !permissions.contains(p))
                    {
                        return Err(ConfigureError::Invalid(format!(
                            "the plugin requires the permission '{missing}'"
                        )));
                    }
                }
                PluginState {
                    id: id.to_owned(),
                    enabled,
                    // Never more than requested (or offered as optional).
                    granted_permissions: permissions
                        .into_iter()
                        .filter(|p| entry.manifest.grantable(p))
                        .collect(),
                    config: entry.state.config.clone(),
                    instances,
                }
            };
            self.db
                .save_plugin_state(&state)
                .await
                .map_err(|e| ConfigureError::Internal(e.to_string()))?;
            // Permissions are fixed per process: always restart on changes.
            self.stop_process(id).await;
            if let Some(entry) = self.inner.write().plugins.get_mut(id) {
                entry.state = state;
            }
            if matches!(self.host_status(), HostStatus::Running) {
                self.start_process(id)
            } else {
                self.set_status(id, PluginStatus::Unavailable);
                None
            }
        };
        if let Some(started) = started {
            let _ = tokio::time::timeout(Duration::from_secs(60), started).await;
        }
        self.get(id).ok_or(ConfigureError::NotFound)
    }

    /// Saves the configuration of a plugin on one instance and restarts its context there.
    pub async fn set_instance_config(
        &self,
        plugin: &str,
        instance: &str,
        config: Value,
    ) -> Result<(), ConfigureError> {
        if !self.inner.read().plugins.contains_key(plugin) {
            return Err(ConfigureError::NotFound);
        }
        if !config.is_object() || config.to_string().len() > MAX_CONFIG_BYTES {
            return Err(ConfigureError::Invalid(format!(
                "the configuration must be an object of at most {MAX_CONFIG_BYTES} bytes"
            )));
        }
        self.db
            .set_plugin_instance_config(plugin, instance, &config)
            .await
            .map_err(|e| ConfigureError::Internal(e.to_string()))?;
        let _discovery = self.discovery.read().await;
        let lock = self.plugin_lock(plugin);
        let _guard = lock.lock().await;
        if let Some(peer) = self.peer(plugin) {
            if self.has_context(plugin, instance) {
                self.stop_context(plugin, &peer, instance).await;
            }
            self.reconcile_plugin(plugin).await;
        }
        Ok(())
    }

    /// Re-scans the plugin directory and restarts every plugin (picks up code changes).
    pub async fn reload(&self) -> anyhow::Result<()> {
        self.rediscover().await
    }

    pub async fn plugin_instance_configs(&self, plugin: &str) -> anyhow::Result<Vec<(String, Value)>> {
        Ok(self.db.plugin_instance_configs(plugin).await?)
    }

    // ---- web integration ------------------------------------------------------------------

    /// The HTTP endpoint `name` of a plugin active on `instance` (with `web`).
    pub fn web_handler(&self, plugin: &str, instance: &str, name: &str) -> Option<WebHandler> {
        if !self.granted(plugin, "web") || !self.has_context(plugin, instance) {
            return None;
        }
        self.inner.read().plugins.get(plugin)?.web.get(name).cloned()
    }

    pub(crate) fn register_web(&self, plugin: &str, name: String, handler: WebHandler) -> Result<(), String> {
        let mut inner = self.inner.write();
        let entry = inner.plugins.get_mut(plugin).ok_or("unknown plugin")?;
        if entry.web.len() >= 64 && !entry.web.contains_key(&name) {
            return Err("a plugin can register at most 64 endpoints".into());
        }
        entry.web.insert(name, handler);
        Ok(())
    }

    /// Forwards an HTTP event to a plugin; returns the handler's response. The caller has
    /// checked [`PluginManager::web_handler`] (existence, `public`, permission).
    pub async fn http_event(
        &self,
        plugin: &str,
        instance: &str,
        request: HttpEvent,
    ) -> Result<Value, ConfigureError> {
        let handler = self
            .web_handler(plugin, instance, &request.name)
            .ok_or(ConfigureError::NotFound)?;
        if request.public && !handler.public {
            return Err(ConfigureError::NotFound);
        }
        // Unauthenticated callers share a small budget per plugin.
        let _permit = if request.public {
            let slots = self.processes.lock().get(plugin).map(|p| p.public.clone());
            let slots = slots.ok_or(ConfigureError::Unavailable)?;
            Some(slots.try_acquire_owned().map_err(|_| ConfigureError::Busy)?)
        } else {
            None
        };
        let params = json!({ "instance": instance, "request": request });
        self.request(plugin, "http.event", params, Duration::from_secs(30))
            .await
            .map_err(|e| match e {
                IpcError::Remote(error) => ConfigureError::Invalid(error),
                other => ConfigureError::Internal(other.to_string()),
            })
    }

    /// Opens a plugin WebSocket connection of `user`; returns its id and the outgoing
    /// messages.
    pub fn ws_open(
        &self,
        plugin: &str,
        instance: &str,
        user: i64,
        username: &str,
    ) -> Result<(u64, mpsc::Receiver<String>), ConfigureError> {
        if !self.granted(plugin, "web") || !self.has_context(plugin, instance) {
            return Err(ConfigureError::NotFound);
        }
        let peer = self.peer(plugin).ok_or(ConfigureError::Unavailable)?;
        let id = self.next_ws.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = mpsc::channel(256);
        {
            let mut ws = self.ws.lock();
            let of_plugin = ws.values().filter(|c| c.plugin == plugin);
            if of_plugin.clone().count() >= MAX_WS_PER_PLUGIN
                || of_plugin.filter(|c| c.user == user).count() >= MAX_WS_PER_USER
            {
                return Err(ConfigureError::Busy);
            }
            ws.insert(
                id,
                WsConnection {
                    plugin: plugin.to_owned(),
                    instance: instance.to_owned(),
                    user,
                    sender,
                },
            );
        }
        peer.notify(
            "ws.open",
            json!({
                "instance": instance,
                "connection": id.to_string(),
                "user": { "id": user, "username": username },
            }),
        );
        Ok((id, receiver))
    }

    /// Forwards a message from a web client; returns `false` if the plugin is overloaded.
    pub fn ws_message(&self, connection: u64, data: String) -> bool {
        let target = self
            .ws
            .lock()
            .get(&connection)
            .map(|c| (c.plugin.clone(), c.instance.clone()));
        let Some((plugin, instance)) = target else {
            return false;
        };
        let Some(peer) = self.peer(&plugin) else {
            return false;
        };
        peer.notify(
            "ws.message",
            json!({ "instance": instance, "connection": connection.to_string(), "data": data }),
        )
    }

    pub fn ws_close(&self, connection: u64) {
        if let Some(c) = self.ws.lock().remove(&connection) {
            if let Some(peer) = self.peer(&c.plugin) {
                peer.notify(
                    "ws.close",
                    json!({ "instance": c.instance, "connection": connection.to_string() }),
                );
            }
        }
    }

    // ---- events ---------------------------------------------------------------------------

    /// Forwards instance events to the plugins and keeps contexts in sync with running
    /// instances. Runs until the event bus closes.
    async fn forward_events(self: Arc<Self>) {
        let mut events = self.instances.subscribe();
        loop {
            let envelope = match events.recv().await {
                Ok(envelope) => envelope,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(missed = n, "plugin event forwarding lagged");
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            };
            if matches!(envelope.event, InstanceEvent::Started | InstanceEvent::Stopped) {
                self.request_reconcile();
            }
            self.forward(&envelope);
        }
    }

    /// Plugins with a context on `instance` (`None` = any).
    fn plugins_on(&self, instance: Option<&str>) -> BTreeSet<String> {
        self.inner
            .read()
            .contexts
            .iter()
            .filter(|(_, i)| instance.is_none_or(|wanted| wanted == i))
            .map(|(p, _)| p.clone())
            .collect()
    }

    fn forward(&self, envelope: &EventEnvelope) {
        let plugins = self.plugins_on(Some(&envelope.instance));
        if plugins.is_empty() {
            return;
        }
        let Ok(mut data) = serde_json::to_value(envelope) else {
            return;
        };
        data["sensitive"] = Value::Bool(envelope.sensitive);
        // Client IP addresses only reach plugins that may moderate (e.g. to match clients).
        let mut without_addresses: Option<Value> = None;
        for plugin in plugins {
            if let Some(peer) = self.peer(&plugin) {
                let data = if self.granted(&plugin, "moderation") {
                    &data
                } else {
                    without_addresses.get_or_insert_with(|| {
                        let mut stripped = data.clone();
                        james_core::platform::strip_client_addresses(&mut stripped);
                        stripped
                    })
                };
                if !peer.notify("instance.event", data.clone())
                    && self
                        .overload_log
                        .allow(&format!("{plugin}/{}", envelope.instance))
                {
                    tracing::warn!(%plugin, instance = %envelope.instance, "plugin overloaded, events dropped");
                }
            }
        }
    }

    /// A custom event (`bot.emit`) for all plugins on one instance or, with `broadcast`, on
    /// every instance.
    pub(crate) fn plugin_event(&self, from: &str, instance: Option<&str>, name: &str, data: &Value) {
        if !name.is_empty() && name.len() <= 100 && data.to_string().len() <= MAX_EVENT_BYTES {
            for plugin in self.plugins_on(instance) {
                if let Some(peer) = self.peer(&plugin) {
                    peer.notify(
                        "plugin.event",
                        json!({ "instance": instance, "name": name, "data": data, "from": from }),
                    );
                }
            }
        }
    }

    /// Tells every other plugin about a change of the global store.
    pub(crate) fn global_store_changed(&self, from: &str, key: &str, value: &Value, deleted: bool) {
        let peers: Vec<Peer> = self
            .processes
            .lock()
            .iter()
            .filter(|(id, _)| *id != from)
            .filter_map(|(_, p)| p.peer.clone())
            .collect();
        for peer in peers {
            peer.notify(
                "store.changed",
                json!({ "scope": "global", "key": key, "value": value, "deleted": deleted }),
            );
        }
    }

    /// Rate limit of `engine.notify`.
    pub(crate) fn allow_notice(&self, plugin: &str) -> bool {
        let now = Instant::now();
        let mut notices = self.notices.lock();
        let recent = notices.entry(plugin.to_owned()).or_default();
        while recent
            .front()
            .is_some_and(|t| now.duration_since(*t) > Duration::from_secs(60))
        {
            recent.pop_front();
        }
        if recent.len() >= NOTICES_PER_MINUTE {
            return false;
        }
        recent.push_back(now);
        true
    }

    // ---- commands -------------------------------------------------------------------------

    pub(crate) fn register_command(&self, plugin: &str, mut command: PluginCommand) -> Result<(), String> {
        command.manager = self.this.clone();
        let name = command.name.to_ascii_lowercase();
        let registry = &self.instances.services().commands;
        let mut inner = self.inner.write();
        let entry = inner
            .plugins
            .get_mut(plugin)
            .filter(|e| e.status == PluginStatus::Running)
            .ok_or_else(|| "plugin is not running".to_string())?;
        // Re-registration by the same plugin (e.g. after a worker restart) replaces it.
        if entry.commands.contains(&name) {
            registry.unregister(&name);
            entry.commands.retain(|c| *c != name);
        }
        if entry.commands.len() >= MAX_COMMANDS_PER_PLUGIN {
            return Err(format!(
                "a plugin can register at most {MAX_COMMANDS_PER_PLUGIN} commands"
            ));
        }
        registry.try_register(Arc::new(command))?;
        entry.commands.push(name);
        Ok(())
    }

    pub(crate) fn unregister_command(&self, plugin: &str, name: &str) {
        let name = name.to_ascii_lowercase();
        let mut inner = self.inner.write();
        if let Some(entry) = inner.plugins.get_mut(plugin) {
            if entry.commands.contains(&name) {
                entry.commands.retain(|c| *c != name);
                self.instances.services().commands.unregister(&name);
            }
        }
    }
}

/// Runs a plugin process, restarting it after crashes (with backoff) until it crashed
/// [`MAX_CRASHES`] times within [`CRASH_WINDOW`].
async fn supervise(
    manager: Arc<PluginManager>,
    manifest: Manifest,
    permissions: Vec<String>,
    generation: u64,
    started: oneshot::Sender<()>,
) {
    let id = manifest.id.clone();
    let mut started = Some(started);
    let mut crashes: VecDeque<Instant> = VecDeque::new();
    loop {
        let ready = |peer: Peer| {
            if manager.process_ready(&id, generation, peer) {
                manager.request_reconcile();
            }
            // The configuration request waits for this first start.
            if let Some(started) = started.take() {
                let _ = started.send(());
            }
        };
        let result = crate::host::run_plugin(&manager, &manifest, &permissions, generation, ready).await;
        if let Some(started) = started.take() {
            let _ = started.send(());
        }
        let error = match result {
            Ok(reason) => reason,
            Err(e) => format!("{e:#}"),
        };
        if !manager.process_ended(&id, generation) {
            return; // stopped or replaced meanwhile
        }
        let now = Instant::now();
        crashes.push_back(now);
        while crashes
            .front()
            .is_some_and(|t| now.duration_since(*t) > CRASH_WINDOW)
        {
            crashes.pop_front();
        }
        if crashes.len() >= MAX_CRASHES {
            manager.process_failed(&id, generation, error);
            return;
        }
        tracing::warn!(plugin = %id, %error, "plugin stopped, restarting");
        manager.set_status_of(&id, generation, PluginStatus::Starting);
        tokio::time::sleep(RESTART_DELAYS[crashes.len().saturating_sub(1).min(RESTART_DELAYS.len() - 1)])
            .await;
    }
}

/// An HTTP request forwarded to a plugin.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpEvent {
    pub name: String,
    pub public: bool,
    pub method: String,
    pub body: Value,
    pub query: BTreeMap<String, String>,
    pub remote_addr: String,
    /// The authenticated user (id, username), if any.
    pub user: Option<Value>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigureError {
    #[error("plugin not found")]
    NotFound,
    #[error("plugins are not available")]
    Unavailable,
    #[error("the plugin is busy, try again later")]
    Busy,
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Internal(String),
}

fn info(id: &str, entry: &Entry, contexts: &BTreeSet<(String, String)>) -> PluginInfo {
    PluginInfo {
        manifest: entry.manifest.clone(),
        state: entry.state.clone(),
        status: entry.status.clone(),
        active_instances: contexts
            .iter()
            .filter(|(p, _)| p == id)
            .map(|(_, i)| i.clone())
            .collect(),
        commands: entry.commands.clone(),
    }
}

/// A new plugin state (see `PluginManager::update_state`).
struct Change {
    enabled: bool,
    permissions: Vec<String>,
    instances: Option<Vec<String>>,
    /// Only takes the plugin off instances: needs no newly requested permission.
    only_removes: bool,
}

/// Validates a discovered manifest. Discovery ran plugin code (in probe processes), so where a
/// plugin's code lives is derived here from its id and kind; discovery only picks the entry
/// module inside the plugin's own directory and its web directory.
fn check_manifest(
    root: &std::path::Path,
    manifest: &mut Manifest,
    symlinks: &mut HashMap<PathBuf, Result<(), String>>,
) -> Result<(), String> {
    if !manifest::valid_id(&manifest.id) {
        return Err("invalid plugin id".into());
    }
    // A plugin that needs an unknown permission can never be enabled; an unknown optional one
    // is just not offered.
    if let Some(unknown) = manifest
        .permissions
        .iter()
        .find(|p| manifest::permission(p).is_none())
    {
        return Err(format!("unknown permission '{unknown}'"));
    }
    manifest
        .optional_permissions
        .retain(|p| manifest::permission(p).is_some());
    let reported = manifest.paths.take().ok_or("the plugin's files are unknown")?;
    let canonical = |path: &std::path::Path| std::fs::canonicalize(path).ok();
    let (dir, entry, own, readable) = match manifest.kind {
        // A directory plugin, unless discovery read a single file `<id>.ts` (a directory of the
        // same name next to it does not change that).
        manifest::PluginKind::James
            if reported.entry.parent().and_then(canonical).as_deref() != Some(root) =>
        {
            let dir = canonical(&root.join(&manifest.id))
                .filter(|d| d.parent() == Some(root) && d.is_dir())
                .ok_or("the plugin directory is missing")?;
            let entry = canonical(&reported.entry)
                .filter(|e| e.starts_with(&dir) && *e != dir && e.is_file())
                .ok_or("the entry point is outside the plugin's directory")?;
            (dir.clone(), entry, dir.clone(), vec![dir])
        }
        manifest::PluginKind::James => {
            // A single file `<id>.ts`, not a symlink; it may use the shared `node_modules`.
            let entry = root.join(format!("{}.ts", manifest.id));
            if !std::fs::symlink_metadata(&entry).is_ok_and(|m| m.is_file()) {
                return Err("the plugin file is missing".into());
            }
            let mut readable = vec![entry.clone()];
            let modules = root.join("node_modules");
            if std::fs::symlink_metadata(&modules).is_ok_and(|m| m.is_dir()) {
                readable.push(modules);
            }
            // No web page: `own` is a directory that does not exist.
            (root.to_path_buf(), entry, root.join(&manifest.id), readable)
        }
        manifest::PluginKind::Sinusbot => {
            // `<id>.js` (the file name may differ in case), not a symlink.
            let file = std::fs::read_dir(root)
                .map_err(|e| e.to_string())?
                .flatten()
                .find(|e| {
                    e.file_name().to_string_lossy().to_lowercase() == format!("{}.js", manifest.id)
                        && e.file_type().is_ok_and(|t| t.is_file())
                })
                .ok_or("the script file is missing")?;
            let entry = file.path();
            let stem = entry.file_stem().map(|s| root.join(s)).unwrap_or_default();
            let mut readable = vec![entry.clone()];
            for extra in [stem.clone(), root.join("node_modules")] {
                if std::fs::symlink_metadata(&extra).is_ok_and(|m| m.is_dir()) {
                    readable.push(extra);
                }
            }
            (root.to_path_buf(), entry, stem, readable)
        }
    };
    // A symlink pointing outside would let Node read through it.
    for path in &readable {
        // Shared directories (`node_modules` of SinusBot scripts) are walked once.
        // Package managers (pnpm) hard-link files in `node_modules`; plugin files must not be.
        let hard_links = !path.ends_with("node_modules");
        symlinks
            .entry(path.clone())
            .or_insert_with(|| check_symlinks(path, hard_links))
            .clone()?;
    }
    manifest.web_dir = manifest
        .web_dir
        .as_ref()
        .and_then(|d| canonical(d))
        .filter(|d| d.starts_with(&own) && *d != own && d.is_dir());
    manifest.web = manifest.web_dir.is_some();
    manifest.paths = Some(manifest::PluginPaths { dir, entry, readable });
    Ok(())
}

/// Refuses symlinks under `base` that lead outside of it (checked without following them
/// while walking).
fn check_symlinks(base: &std::path::Path, refuse_hard_links: bool) -> Result<(), String> {
    let base = std::fs::canonicalize(base).map_err(|e| e.to_string())?;
    if refuse_hard_links {
        use std::os::unix::fs::MetadataExt;
        // `base` itself may be a file (a SinusBot script).
        let meta = std::fs::metadata(&base).map_err(|e| e.to_string())?;
        if meta.is_file() && meta.nlink() > 1 {
            return Err(format!("'{}' is a hard link", base.display()));
        }
    }
    // (directory, inside a `node_modules` tree: package managers like pnpm hard-link files)
    let mut stack = vec![(base.clone(), !refuse_hard_links)];
    let mut seen = 0usize;
    while let Some((dir, packages)) = stack.pop() {
        let meta = std::fs::symlink_metadata(&dir).map_err(|e| e.to_string())?;
        if !meta.is_dir() {
            continue;
        }
        for entry in std::fs::read_dir(&dir).map_err(|e| e.to_string())?.flatten() {
            seen += 1;
            if seen > MAX_PLUGIN_FILES {
                return Err("the plugin has too many files".into());
            }
            let path = entry.path();
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            if kind.is_file() && !packages {
                // A hard link could point at any file of the bot user (e.g. the database).
                use std::os::unix::fs::MetadataExt;
                if entry.metadata().is_ok_and(|m| m.nlink() > 1) {
                    return Err(format!(
                        "'{}' is a hard link",
                        path.strip_prefix(&base).unwrap_or(&path).display()
                    ));
                }
            } else if kind.is_symlink() {
                let target = std::fs::canonicalize(&path).ok();
                if !target.is_some_and(|t| t.starts_with(&base)) {
                    return Err(format!(
                        "'{}' links outside the plugin",
                        path.strip_prefix(&base).unwrap_or(&path).display()
                    ));
                }
            } else if kind.is_dir() {
                let packages = packages || entry.file_name() == "node_modules";
                stack.push((path, packages));
            }
        }
    }
    Ok(())
}

/// Replaces control characters (log lines cannot be forged with newlines).
pub(crate) fn sanitize(text: &str) -> String {
    text.trim_end_matches(['\n', '\r'])
        .chars()
        .map(|c| if c.is_control() && c != '\t' { '⏎' } else { c })
        .collect()
}

/// Text of a `log` event.
pub(crate) fn log_text(data: &Value) -> String {
    james_core::logs::clean(data["message"].as_str().unwrap_or_default())
}

/// Requests and events from one plugin's process; the plugin id is bound to the connection.
pub(crate) struct HostHandler {
    manager: Arc<PluginManager>,
    plugin: String,
    generation: u64,
}

impl HostHandler {
    pub fn new(manager: Arc<PluginManager>, plugin: String, generation: u64) -> Self {
        Self {
            manager,
            plugin,
            generation,
        }
    }
}

#[async_trait]
impl Handler for HostHandler {
    async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        match method {
            "call" => crate::bridge::call(&self.manager, &self.plugin, params).await,
            _ => Err(format!("unknown method '{method}'")),
        }
    }

    async fn event(&self, name: &str, data: Value) {
        let plugin = self.plugin.as_str();
        match name {
            "log" => {
                let message = log_text(&data);
                let instance = sanitize(data["instance"].as_str().unwrap_or_default());
                let level = data["level"].as_str().unwrap_or("info");
                // Messages reach an instance's log only from plugins running on it, and a chatty
                // plugin cannot push everything else out of it (debug output does not count: it
                // is only kept when James' own level shows it).
                let instance_log = !instance.is_empty() && self.manager.has_context(plugin, &instance);
                if instance_log
                    && level != "debug"
                    && !self.manager.log_limit.allow(&format!("{plugin}/{instance}"))
                {
                    // Dropped (also from James' own output), with one note a minute.
                    if self
                        .manager
                        .overload_log
                        .allow(&format!("log/{plugin}/{instance}"))
                    {
                        tracing::warn!(%plugin, %instance, "plugin logs too much, messages dropped");
                    }
                    return;
                }
                match level {
                    "error" => {
                        tracing::error!(target: "plugin", %plugin, %instance, instance_log, "{message}")
                    }
                    "warn" => tracing::warn!(target: "plugin", %plugin, %instance, instance_log, "{message}"),
                    "debug" => {
                        tracing::debug!(target: "plugin", %plugin, %instance, instance_log, "{message}")
                    }
                    _ => tracing::info!(target: "plugin", %plugin, %instance, instance_log, "{message}"),
                }
            }
            "plugin.failed" => {
                // Its workers crashed repeatedly: stop the process for good.
                let error = sanitize(data["error"].as_str().unwrap_or("crashed"));
                let manager = self.manager.clone();
                let plugin = plugin.to_owned();
                let generation = self.generation;
                tokio::spawn(async move {
                    let _discovery = manager.discovery.read().await;
                    let lock = manager.plugin_lock(&plugin);
                    let _guard = lock.lock().await;
                    // Only the process that reported it (not a newer one).
                    if manager.current_generation(&plugin) == Some(generation) {
                        manager.stop_process(&plugin).await;
                        tracing::warn!(%plugin, %error, "plugin failed");
                        manager.set_status(&plugin, PluginStatus::Failed { error });
                    }
                });
            }
            "ws.send" => crate::bridge::ws_send(&self.manager, plugin, &data),
            "context.failed" => {
                // Setup failed after a worker restart: forget it, it is started again later.
                let current = self.manager.current_generation(plugin) == Some(self.generation);
                if let (Some(instance), true) = (data["instance"].as_str(), current) {
                    self.manager
                        .inner
                        .write()
                        .contexts
                        .remove(&(plugin.to_owned(), instance.to_owned()));
                    self.manager
                        .ws
                        .lock()
                        .retain(|_, c| !(c.plugin == plugin && c.instance == instance));
                    self.manager.request_reconcile();
                }
            }
            "emit" => {
                let instance = data["instance"].as_str();
                let broadcast = data["broadcast"].as_bool().unwrap_or(false);
                // A plugin can only emit on instances it is active on.
                let allowed = instance.is_some_and(|i| self.manager.has_context(plugin, i));
                if allowed {
                    let target = if broadcast { None } else { instance };
                    let name = data["name"].as_str().unwrap_or_default();
                    self.manager.plugin_event(plugin, target, name, &data["data"]);
                }
            }
            _ => tracing::debug!(%name, %plugin, "unknown event from a plugin"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{PluginKind, PluginPaths};

    fn manifest(id: &str, kind: PluginKind, entry: PathBuf) -> Manifest {
        serde_json::from_value::<Manifest>(json!({ "id": id, "kind": kind, "name": id }))
            .map(|mut m| {
                m.paths = Some(PluginPaths {
                    dir: entry.parent().unwrap().to_path_buf(),
                    entry,
                    readable: vec![PathBuf::from("/")],
                });
                m
            })
            .unwrap()
    }

    #[test]
    fn unknown_permissions_are_refused_or_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        std::fs::write(root.join("script.js"), "").unwrap();
        let mut required = manifest("script", PluginKind::Sinusbot, root.join("script.js"));
        required.permissions = vec!["moderaton".into()];
        let error = check_manifest(&root, &mut required, &mut HashMap::new()).unwrap_err();
        assert_eq!(error, "unknown permission 'moderaton'");

        let mut optional = manifest("script", PluginKind::Sinusbot, root.join("script.js"));
        optional.optional_permissions = vec!["channels".into(), "teleport".into()];
        check_manifest(&root, &mut optional, &mut HashMap::new()).unwrap();
        assert_eq!(optional.optional_permissions, ["channels"]);
    }

    #[test]
    fn plugin_files_are_derived_from_the_id() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        for dir in ["good", "victim"] {
            std::fs::create_dir(root.join(dir)).unwrap();
            std::fs::write(root.join(dir).join("index.js"), "").unwrap();
        }
        std::fs::write(root.join("Legacy.js"), "").unwrap();
        std::fs::create_dir(root.join("Legacy")).unwrap();

        let mut good = manifest("good", PluginKind::James, root.join("good/index.js"));
        check_manifest(&root, &mut good, &mut HashMap::new()).unwrap();
        let paths = good.paths.unwrap();
        assert_eq!(
            paths.readable,
            vec![root.join("good")],
            "discovery's readable list is ignored"
        );

        // Discovery pointing a plugin at another plugin's code is refused.
        let mut hijacked = manifest("good", PluginKind::James, root.join("victim/index.js"));
        assert!(check_manifest(&root, &mut hijacked, &mut HashMap::new()).is_err());

        let mut script = manifest("legacy", PluginKind::Sinusbot, root.join("victim/index.js"));
        check_manifest(&root, &mut script, &mut HashMap::new()).unwrap();
        let paths = script.paths.unwrap();
        assert_eq!(paths.entry, root.join("Legacy.js"));
        assert_eq!(paths.readable, vec![root.join("Legacy.js"), root.join("Legacy")]);
    }

    #[test]
    fn symlinks_leaving_the_plugin_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        std::fs::create_dir_all(root.join("sneaky/lib")).unwrap();
        std::fs::write(root.join("sneaky/index.js"), "").unwrap();
        std::os::unix::fs::symlink(root.join("sneaky/index.js"), root.join("sneaky/lib/inside")).unwrap();
        let mut ok = manifest("sneaky", PluginKind::James, root.join("sneaky/index.js"));
        check_manifest(&root, &mut ok, &mut HashMap::new()).unwrap();

        std::os::unix::fs::symlink("/etc", root.join("sneaky/lib/etc")).unwrap();
        let mut bad = manifest("sneaky", PluginKind::James, root.join("sneaky/index.js"));
        let error = check_manifest(&root, &mut bad, &mut HashMap::new()).unwrap_err();
        assert!(error.contains("links outside"), "{error}");

        std::fs::remove_file(root.join("sneaky/lib/etc")).unwrap();
        std::fs::write(root.join("secret.db"), "x").unwrap();
        std::fs::hard_link(root.join("secret.db"), root.join("sneaky/lib/db")).unwrap();
        let mut hard = manifest("sneaky", PluginKind::James, root.join("sneaky/index.js"));
        let error = check_manifest(&root, &mut hard, &mut HashMap::new()).unwrap_err();
        assert!(error.contains("hard link"), "{error}");
    }

    #[test]
    fn hard_links_are_allowed_only_in_node_modules() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        std::fs::create_dir_all(root.join("pkg/node_modules/dep")).unwrap();
        std::fs::write(root.join("pkg/index.js"), "").unwrap();
        std::fs::write(root.join("store.js"), "").unwrap();
        std::fs::hard_link(root.join("store.js"), root.join("pkg/node_modules/dep/index.js")).unwrap();
        let mut pnpm = manifest("pkg", PluginKind::James, root.join("pkg/index.js"));
        check_manifest(&root, &mut pnpm, &mut HashMap::new()).unwrap();

        std::fs::write(root.join("Script.js"), "").unwrap();
        std::fs::hard_link(root.join("Script.js"), root.join("copy.js")).unwrap();
        let mut script = manifest("script", PluginKind::Sinusbot, root.join("Script.js"));
        let error = check_manifest(&root, &mut script, &mut HashMap::new()).unwrap_err();
        assert!(error.contains("hard link"), "the script file itself: {error}");
    }

    #[test]
    fn log_text_has_no_control_characters() {
        assert_eq!(sanitize("a\nb\r\x1b[31mc\n"), "a⏎b⏎⏎[31mc");
    }
}
