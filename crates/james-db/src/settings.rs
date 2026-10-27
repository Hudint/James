//! Global key-value settings (JSON values).

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::{Db, Result};

impl Db {
    pub async fn setting<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let raw: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
            .bind(key)
            .fetch_optional(self.pool())
            .await?;
        raw.map(|r| serde_json::from_str(&r).map_err(Into::into))
            .transpose()
    }

    pub async fn set_setting<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        sqlx::query("INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value")
            .bind(key)
            .bind(serde_json::to_string(value)?)
            .execute(self.pool())
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn settings_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("james.db")).await.unwrap();
        assert_eq!(db.setting::<String>("x").await.unwrap(), None);
        db.set_setting("x", &"y").await.unwrap();
        drop(db);
        let db = Db::open(&dir.path().join("james.db")).await.unwrap();
        assert_eq!(
            db.setting::<String>("x").await.unwrap().as_deref(),
            Some("y"),
            "persisted"
        );
    }
}
