//! Instance configurations and their adapter credentials.

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::{now, Db, DbError, Result};

impl Db {
    /// Stores an instance config (any serialisable type, kept as JSON).
    pub async fn save_instance<T: Serialize>(&self, id: &str, config: &T) -> Result<()> {
        let json = serde_json::to_string(config)?;
        let time = now();
        sqlx::query(
            "INSERT INTO instances (id, config, created_at, updated_at) VALUES (?, ?, ?, ?)
             ON CONFLICT (id) DO UPDATE SET config = excluded.config, updated_at = excluded.updated_at",
        )
        .bind(id)
        .bind(json)
        .bind(time)
        .bind(time)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn instances<T: DeserializeOwned>(&self) -> Result<Vec<T>> {
        let rows: Vec<String> = sqlx::query_scalar("SELECT config FROM instances ORDER BY created_at, id")
            .fetch_all(self.pool())
            .await?;
        rows.iter()
            .map(|r| serde_json::from_str(r).map_err(Into::into))
            .collect()
    }

    pub async fn delete_instance(&self, id: &str) -> Result<()> {
        let result = sqlx::query("DELETE FROM instances WHERE id = ?")
            .bind(id)
            .execute(self.pool())
            .await?;
        if result.rows_affected() == 0 {
            return Err(DbError::NotFound);
        }
        Ok(())
    }

    pub async fn instance_credentials(&self, id: &str) -> Result<Option<serde_json::Value>> {
        let raw: Option<Option<String>> =
            sqlx::query_scalar("SELECT credentials FROM instances WHERE id = ?")
                .bind(id)
                .fetch_optional(self.pool())
                .await?;
        raw.flatten()
            .map(|r| serde_json::from_str(&r).map_err(Into::into))
            .transpose()
    }

    pub async fn save_instance_credentials(&self, id: &str, credentials: &serde_json::Value) -> Result<()> {
        sqlx::query("UPDATE instances SET credentials = ? WHERE id = ?")
            .bind(serde_json::to_string(credentials)?)
            .bind(id)
            .execute(self.pool())
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn round_trip() {
        let db = Db::memory().await.unwrap();
        db.save_instance("a", &json!({"id": "a", "name": "A"}))
            .await
            .unwrap();
        db.save_instance("a", &json!({"id": "a", "name": "A2"}))
            .await
            .unwrap();
        let all: Vec<serde_json::Value> = db.instances().await.unwrap();
        assert_eq!(all, vec![json!({"id": "a", "name": "A2"})]);
        assert_eq!(db.instance_credentials("a").await.unwrap(), None);
        db.save_instance_credentials("a", &json!({"identity": 1}))
            .await
            .unwrap();
        assert_eq!(
            db.instance_credentials("a").await.unwrap(),
            Some(json!({"identity": 1}))
        );
        db.delete_instance("a").await.unwrap();
        assert!(matches!(db.delete_instance("a").await, Err(DbError::NotFound)));
    }
}
