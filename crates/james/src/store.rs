//! JSON file store for instance runtime data (credentials).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use james_runtime::InstanceStore;

pub struct FileStore {
    path: PathBuf,
    data: Mutex<HashMap<String, serde_json::Value>>,
}

impl FileStore {
    pub fn open(path: PathBuf) -> anyhow::Result<Self> {
        let data = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(e) => return Err(e.into()),
        };
        Ok(Self {
            path,
            data: Mutex::new(data),
        })
    }

    fn flush(&self, data: &HashMap<String, serde_json::Value>) {
        let tmp = self.path.with_extension("tmp");
        let result = serde_json::to_vec_pretty(data)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| Ok(std::fs::write(&tmp, bytes)?))
            .and_then(|_| Ok(std::fs::rename(&tmp, &self.path)?));
        if let Err(e) = result {
            tracing::error!(path = %self.path.display(), error = %e, "failed to write state");
        }
    }
}

impl InstanceStore for FileStore {
    fn load_credentials(&self, instance: &str) -> Option<serde_json::Value> {
        self.data.lock().unwrap().get(instance).cloned()
    }

    fn save_credentials(&self, instance: &str, data: serde_json::Value) {
        let mut map = self.data.lock().unwrap();
        if map.get(instance) != Some(&data) {
            map.insert(instance.to_owned(), data);
            self.flush(&map);
        }
    }
}
