//! Persistence the runtime needs (implemented on top of the database by the application).

use std::collections::HashMap;

use async_trait::async_trait;
use parking_lot::Mutex;

#[async_trait]
pub trait InstanceStore: Send + Sync {
    async fn load_credentials(&self, instance: &str) -> Option<serde_json::Value>;
    async fn save_credentials(&self, instance: &str, data: serde_json::Value);
    /// Cached integrated loudness of a track (LUFS).
    async fn loudness(&self, source: &str, locator: &str) -> Option<f32>;
    async fn save_loudness(&self, source: &str, locator: &str, lufs: f32);
}

/// Non-persistent store (tests, ephemeral setups).
#[derive(Default)]
pub struct MemoryStore {
    credentials: Mutex<HashMap<String, serde_json::Value>>,
    loudness: Mutex<HashMap<(String, String), f32>>,
}

#[async_trait]
impl InstanceStore for MemoryStore {
    async fn load_credentials(&self, instance: &str) -> Option<serde_json::Value> {
        self.credentials.lock().get(instance).cloned()
    }
    async fn save_credentials(&self, instance: &str, data: serde_json::Value) {
        self.credentials.lock().insert(instance.to_owned(), data);
    }
    async fn loudness(&self, source: &str, locator: &str) -> Option<f32> {
        self.loudness
            .lock()
            .get(&(source.to_owned(), locator.to_owned()))
            .copied()
    }
    async fn save_loudness(&self, source: &str, locator: &str, lufs: f32) {
        self.loudness
            .lock()
            .insert((source.to_owned(), locator.to_owned()), lufs);
    }
}
