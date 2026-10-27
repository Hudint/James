//! Instance manager and shared services.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail};
use james_core::source::SourceRegistry;
use parking_lot::{Mutex, RwLock};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

pub use james_core::platform::PlatformFactory;

use crate::commands::CommandRegistry;
use crate::engine::EngineFactory;
use crate::instance::{self, EventEnvelope, InstanceConfig, InstanceHandle};
pub use crate::store::{InstanceStore, MemoryStore};

/// Services shared by all instances.
#[derive(Clone)]
pub struct Services {
    pub sources: Arc<RwLock<SourceRegistry>>,
    pub platforms: Arc<HashMap<String, Arc<dyn PlatformFactory>>>,
    pub engines: Arc<dyn EngineFactory>,
    pub commands: CommandRegistry,
    pub store: Arc<dyn InstanceStore>,
    pub events: broadcast::Sender<EventEnvelope>,
    pub ffmpeg: String,
    /// Egress proxy for ffmpeg network inputs.
    pub http_proxy: Option<String>,
}

impl Services {
    pub fn new(
        sources: SourceRegistry,
        platforms: Vec<Arc<dyn PlatformFactory>>,
        engines: Arc<dyn EngineFactory>,
        store: Arc<dyn InstanceStore>,
        ffmpeg: impl Into<String>,
    ) -> Self {
        Self {
            sources: Arc::new(RwLock::new(sources)),
            platforms: Arc::new(platforms.into_iter().map(|p| (p.id().to_owned(), p)).collect()),
            engines,
            commands: CommandRegistry::default(),
            store,
            events: broadcast::channel(4096).0,
            ffmpeg: ffmpeg.into(),
            http_proxy: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigureMode {
    Create,
    Update,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigureError {
    #[error("the instance already exists")]
    Exists,
    #[error("the instance does not exist")]
    NotFound,
}

struct Running {
    handle: InstanceHandle,
    task: JoinHandle<()>,
}

pub struct InstanceManager {
    services: Services,
    configs: RwLock<HashMap<String, InstanceConfig>>,
    running: Mutex<HashMap<String, Running>>,
    /// Serialises start/stop/restart per instance id, so an instance that is still
    /// disconnecting can never run in parallel with its replacement.
    lifecycle: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

/// Maximum time to wait for an instance to shut down cleanly.
const STOP_TIMEOUT: Duration = Duration::from_secs(8);

impl InstanceManager {
    pub fn new(services: Services) -> Self {
        Self {
            services,
            configs: Default::default(),
            running: Default::default(),
            lifecycle: Default::default(),
        }
    }

    pub fn services(&self) -> &Services {
        &self.services
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EventEnvelope> {
        self.services.events.subscribe()
    }

    fn lifecycle_lock(&self, id: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.lifecycle.lock().entry(id.to_owned()).or_default().clone()
    }

    fn validate(&self, config: &InstanceConfig) -> anyhow::Result<()> {
        if config.id.trim().is_empty() {
            bail!("instance id must not be empty");
        }
        config.validate()?;
        let factory = self
            .services
            .platforms
            .get(&config.platform)
            .ok_or_else(|| anyhow!("unknown platform '{}'", config.platform))?;
        factory.validate(&config.settings)
    }

    /// Adds or replaces an instance configuration without starting it (used at startup).
    pub fn upsert(&self, config: InstanceConfig) -> anyhow::Result<()> {
        self.validate(&config)?;
        self.configs.write().insert(config.id.clone(), config);
        Ok(())
    }

    /// Creates or updates an instance atomically with respect to other lifecycle operations:
    /// validates, runs `persist`, stores the config and (re)starts the instance as needed.
    pub async fn configure<F, Fut>(
        &self,
        config: InstanceConfig,
        mode: ConfigureMode,
        persist: F,
    ) -> anyhow::Result<()>
    where
        F: FnOnce(InstanceConfig) -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<()>>,
    {
        self.validate(&config)?;
        let id = config.id.clone();
        let lock = self.lifecycle_lock(&id);
        let _guard = lock.lock().await;
        let exists = self.configs.read().contains_key(&id);
        match (mode, exists) {
            (ConfigureMode::Create, true) => return Err(ConfigureError::Exists.into()),
            (ConfigureMode::Update, false) => return Err(ConfigureError::NotFound.into()),
            _ => {}
        }
        persist(config.clone()).await?;
        let autostart = config.autostart;
        self.configs.write().insert(id.clone(), config);
        let running = self.get(&id).is_some();
        if running {
            self.stop_locked(&id).await;
        }
        if running || (mode == ConfigureMode::Create && autostart) {
            // The configuration is saved either way; a start failure leaves the instance
            // stopped (visible in its status) instead of failing the save.
            if let Err(e) = self.start_locked(&id) {
                tracing::warn!(instance = %id, error = %format!("{e:#}"), "instance could not be started");
            }
        }
        Ok(())
    }

    /// Stops and removes an instance; `persist` runs under the same lock.
    pub async fn delete<F, Fut>(&self, id: &str, persist: F) -> anyhow::Result<()>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<()>>,
    {
        let lock = self.lifecycle_lock(id);
        let _guard = lock.lock().await;
        if !self.configs.read().contains_key(id) {
            return Err(ConfigureError::NotFound.into());
        }
        self.stop_locked(id).await;
        persist().await?;
        // The lifecycle lock entry stays: another task may still be waiting on it.
        self.configs.write().remove(id);
        Ok(())
    }

    /// Stops the instance and removes its configuration.
    pub async fn remove(&self, id: &str) {
        let lock = self.lifecycle_lock(id);
        let _guard = lock.lock().await;
        self.stop_locked(id).await;
        self.configs.write().remove(id);
    }

    pub fn configs(&self) -> Vec<InstanceConfig> {
        let mut list: Vec<_> = self.configs.read().values().cloned().collect();
        list.sort_by(|a, b| a.id.cmp(&b.id));
        list
    }

    pub fn config(&self, id: &str) -> Option<InstanceConfig> {
        self.configs.read().get(id).cloned()
    }

    /// The running instance; instances whose task ended (e.g. after a panic) are dropped.
    pub fn get(&self, id: &str) -> Option<InstanceHandle> {
        let mut running = self.running.lock();
        match running.get(id) {
            Some(r) if r.handle.is_alive() && !r.task.is_finished() => Some(r.handle.clone()),
            Some(_) => {
                tracing::error!(instance = %id, "instance task ended unexpectedly");
                running.remove(id);
                None
            }
            None => None,
        }
    }

    pub fn running(&self) -> Vec<InstanceHandle> {
        let ids: Vec<String> = self.running.lock().keys().cloned().collect();
        ids.iter().filter_map(|id| self.get(id)).collect()
    }

    /// Starts an instance (no-op if it is already running).
    pub async fn start(&self, id: &str) -> anyhow::Result<InstanceHandle> {
        let lock = self.lifecycle_lock(id);
        let _guard = lock.lock().await;
        self.start_locked(id)
    }

    fn start_locked(&self, id: &str) -> anyhow::Result<InstanceHandle> {
        let config = self
            .config(id)
            .ok_or_else(|| anyhow!("unknown instance '{id}'"))?;
        if let Some(handle) = self.get(id) {
            return Ok(handle);
        }
        let (handle, task) = instance::spawn(config, self.services.clone())?;
        self.running.lock().insert(
            id.to_owned(),
            Running {
                handle: handle.clone(),
                task,
            },
        );
        Ok(handle)
    }

    /// Stops an instance and waits until it has disconnected (bounded).
    pub async fn stop(&self, id: &str) {
        let lock = self.lifecycle_lock(id);
        let _guard = lock.lock().await;
        self.stop_locked(id).await;
    }

    async fn stop_locked(&self, id: &str) {
        let Some(running) = self.running.lock().remove(id) else {
            return;
        };
        running.handle.shutdown();
        let mut task = running.task;
        if tokio::time::timeout(STOP_TIMEOUT, &mut task).await.is_err() {
            tracing::warn!(instance = %id, "instance did not stop in time, aborting");
            task.abort();
            let _ = task.await;
        }
    }

    pub async fn restart(&self, id: &str) -> anyhow::Result<InstanceHandle> {
        let lock = self.lifecycle_lock(id);
        let _guard = lock.lock().await;
        self.stop_locked(id).await;
        self.start_locked(id)
    }

    pub async fn start_autostart(&self) {
        for config in self.configs().into_iter().filter(|c| c.autostart) {
            if let Err(e) = self.start(&config.id).await {
                tracing::error!(instance = %config.id, error = %format!("{e:#}"), "failed to start instance");
            }
        }
    }

    /// Stops all instances in parallel and waits for them.
    pub async fn stop_all(&self) {
        let ids: Vec<String> = self.running.lock().keys().cloned().collect();
        futures::future::join_all(ids.iter().map(|id| self.stop(id))).await;
    }
}
