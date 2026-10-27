//! Instance manager and shared services.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use anyhow::{anyhow, bail};
pub use james_core::platform::PlatformFactory;
use james_core::source::SourceRegistry;
use tokio::sync::broadcast;

use crate::commands::CommandRegistry;
use crate::instance::{self, EventEnvelope, InstanceConfig, InstanceHandle};

/// Persistence for per-instance runtime data.
pub trait InstanceStore: Send + Sync {
    fn load_credentials(&self, instance: &str) -> Option<serde_json::Value>;
    fn save_credentials(&self, instance: &str, data: serde_json::Value);
}

/// Non-persistent store (tests, ephemeral setups).
#[derive(Default)]
pub struct MemoryStore(Mutex<HashMap<String, serde_json::Value>>);

impl InstanceStore for MemoryStore {
    fn load_credentials(&self, instance: &str) -> Option<serde_json::Value> {
        self.0.lock().unwrap().get(instance).cloned()
    }
    fn save_credentials(&self, instance: &str, data: serde_json::Value) {
        self.0.lock().unwrap().insert(instance.to_owned(), data);
    }
}

/// Services shared by all instances.
#[derive(Clone)]
pub struct Services {
    pub sources: Arc<RwLock<SourceRegistry>>,
    pub platforms: Arc<HashMap<String, Arc<dyn PlatformFactory>>>,
    pub commands: CommandRegistry,
    pub store: Arc<dyn InstanceStore>,
    pub events: broadcast::Sender<EventEnvelope>,
    pub ffmpeg: String,
}

impl Services {
    pub fn new(
        sources: SourceRegistry,
        platforms: Vec<Arc<dyn PlatformFactory>>,
        store: Arc<dyn InstanceStore>,
        ffmpeg: impl Into<String>,
    ) -> Self {
        Self {
            sources: Arc::new(RwLock::new(sources)),
            platforms: Arc::new(platforms.into_iter().map(|p| (p.id().to_owned(), p)).collect()),
            commands: CommandRegistry::default(),
            store,
            events: broadcast::channel(1024).0,
            ffmpeg: ffmpeg.into(),
        }
    }
}

pub struct InstanceManager {
    services: Services,
    configs: RwLock<HashMap<String, InstanceConfig>>,
    running: RwLock<HashMap<String, InstanceHandle>>,
}

impl InstanceManager {
    pub fn new(services: Services) -> Self {
        Self {
            services,
            configs: Default::default(),
            running: Default::default(),
        }
    }

    pub fn services(&self) -> &Services {
        &self.services
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EventEnvelope> {
        self.services.events.subscribe()
    }

    /// Adds or replaces an instance configuration (a running instance keeps its old config until restarted).
    pub fn upsert(&self, config: InstanceConfig) -> anyhow::Result<()> {
        if config.id.trim().is_empty() {
            bail!("instance id must not be empty");
        }
        if !self.services.platforms.contains_key(&config.platform) {
            bail!("unknown platform '{}'", config.platform);
        }
        self.configs.write().unwrap().insert(config.id.clone(), config);
        Ok(())
    }

    pub fn remove(&self, id: &str) {
        self.stop(id);
        self.configs.write().unwrap().remove(id);
    }

    pub fn configs(&self) -> Vec<InstanceConfig> {
        let mut list: Vec<_> = self.configs.read().unwrap().values().cloned().collect();
        list.sort_by(|a, b| a.id.cmp(&b.id));
        list
    }

    pub fn config(&self, id: &str) -> Option<InstanceConfig> {
        self.configs.read().unwrap().get(id).cloned()
    }

    pub fn get(&self, id: &str) -> Option<InstanceHandle> {
        self.running.read().unwrap().get(id).cloned()
    }

    pub fn running(&self) -> Vec<InstanceHandle> {
        self.running.read().unwrap().values().cloned().collect()
    }

    pub fn start(&self, id: &str) -> anyhow::Result<InstanceHandle> {
        if let Some(handle) = self.get(id) {
            return Ok(handle);
        }
        let config = self
            .config(id)
            .ok_or_else(|| anyhow!("unknown instance '{id}'"))?;
        let handle = instance::spawn(config, self.services.clone())?;
        self.running
            .write()
            .unwrap()
            .insert(id.to_owned(), handle.clone());
        Ok(handle)
    }

    pub fn stop(&self, id: &str) {
        if let Some(handle) = self.running.write().unwrap().remove(id) {
            handle.shutdown();
        }
    }

    pub fn start_autostart(&self) {
        for config in self.configs().into_iter().filter(|c| c.autostart) {
            if let Err(e) = self.start(&config.id) {
                tracing::error!(instance = %config.id, error = %format!("{e:#}"), "failed to start instance");
            }
        }
    }

    pub fn stop_all(&self) {
        let ids: Vec<_> = self.running.read().unwrap().keys().cloned().collect();
        for id in ids {
            self.stop(&id);
        }
    }
}
