//! Plugin key-value store and plugin registry state.

use serde::{Deserialize, Serialize};

use crate::{Db, Result};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginState {
    pub id: String,
    pub enabled: bool,
    pub granted_permissions: Vec<String>,
    pub config: serde_json::Value,
    /// `None` = active on all instances.
    pub instances: Option<Vec<String>>,
}

impl Db {
    pub async fn store_get(&self, plugin: &str, scope: &str, key: &str) -> Result<Option<serde_json::Value>> {
        let raw: Option<String> =
            sqlx::query_scalar("SELECT value FROM plugin_store WHERE plugin = ? AND scope = ? AND key = ?")
                .bind(plugin)
                .bind(scope)
                .bind(key)
                .fetch_optional(self.pool())
                .await?;
        raw.map(|r| serde_json::from_str(&r).map_err(Into::into))
            .transpose()
    }

    pub async fn store_set(
        &self,
        plugin: &str,
        scope: &str,
        key: &str,
        value: &serde_json::Value,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO plugin_store (plugin, scope, key, value) VALUES (?, ?, ?, ?)
             ON CONFLICT (plugin, scope, key) DO UPDATE SET value = excluded.value",
        )
        .bind(plugin)
        .bind(scope)
        .bind(key)
        .bind(serde_json::to_string(value)?)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn store_unset(&self, plugin: &str, scope: &str, key: &str) -> Result<()> {
        sqlx::query("DELETE FROM plugin_store WHERE plugin = ? AND scope = ? AND key = ?")
            .bind(plugin)
            .bind(scope)
            .bind(key)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    pub async fn store_keys(&self, plugin: &str, scope: &str) -> Result<Vec<String>> {
        Ok(
            sqlx::query_scalar("SELECT key FROM plugin_store WHERE plugin = ? AND scope = ? ORDER BY key")
                .bind(plugin)
                .bind(scope)
                .fetch_all(self.pool())
                .await?,
        )
    }

    /// All entries of a scope (loaded into the plugin host's cache).
    pub async fn store_all(&self, plugin: &str, scope: &str) -> Result<Vec<(String, serde_json::Value)>> {
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT key, value FROM plugin_store WHERE plugin = ? AND scope = ? ORDER BY key")
                .bind(plugin)
                .bind(scope)
                .fetch_all(self.pool())
                .await?;
        rows.into_iter()
            .map(|(k, v)| Ok((k, serde_json::from_str(&v)?)))
            .collect()
    }

    /// Number of entries and total value size of a scope (for quotas).
    pub async fn store_usage(&self, plugin: &str, scope: &str) -> Result<(i64, i64)> {
        Ok(sqlx::query_as(
            "SELECT COUNT(*), COALESCE(SUM(LENGTH(value)), 0) FROM plugin_store WHERE plugin = ? AND scope = ?",
        )
        .bind(plugin)
        .bind(scope)
        .fetch_one(self.pool())
        .await?)
    }

    pub async fn plugin_instance_configs(&self, plugin: &str) -> Result<Vec<(String, serde_json::Value)>> {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT instance, config FROM plugin_instance_config WHERE plugin = ? ORDER BY instance",
        )
        .bind(plugin)
        .fetch_all(self.pool())
        .await?;
        rows.into_iter()
            .map(|(i, c)| Ok((i, serde_json::from_str(&c)?)))
            .collect()
    }

    pub async fn set_plugin_instance_config(
        &self,
        plugin: &str,
        instance: &str,
        config: &serde_json::Value,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO plugin_instance_config (plugin, instance, config) VALUES (?, ?, ?)
             ON CONFLICT (plugin, instance) DO UPDATE SET config = excluded.config",
        )
        .bind(plugin)
        .bind(instance)
        .bind(serde_json::to_string(config)?)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Removes per-instance plugin data of a deleted instance.
    pub async fn delete_plugin_instance_data(&self, instance: &str) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        sqlx::query("DELETE FROM plugin_instance_config WHERE instance = ?")
            .bind(instance)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM plugin_store WHERE scope = ?")
            .bind(format!("instance:{instance}"))
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn plugin_states(&self) -> Result<Vec<PluginState>> {
        let rows: Vec<(String, bool, String, String, Option<String>)> = sqlx::query_as(
            "SELECT id, enabled, granted_permissions, config, instances FROM plugins ORDER BY id",
        )
        .fetch_all(self.pool())
        .await?;
        rows.into_iter()
            .map(|(id, enabled, granted, config, instances)| {
                Ok(PluginState {
                    id,
                    enabled,
                    granted_permissions: serde_json::from_str(&granted)?,
                    config: serde_json::from_str(&config)?,
                    instances: instances.map(|i| serde_json::from_str(&i)).transpose()?,
                })
            })
            .collect()
    }

    pub async fn save_plugin_state(&self, state: &PluginState) -> Result<()> {
        sqlx::query(
            "INSERT INTO plugins (id, enabled, granted_permissions, config, instances) VALUES (?, ?, ?, ?, ?)
             ON CONFLICT (id) DO UPDATE SET enabled = excluded.enabled,
                granted_permissions = excluded.granted_permissions, config = excluded.config,
                instances = excluded.instances",
        )
        .bind(&state.id)
        .bind(state.enabled)
        .bind(serde_json::to_string(&state.granted_permissions)?)
        .bind(serde_json::to_string(&state.config)?)
        .bind(state.instances.as_ref().map(serde_json::to_string).transpose()?)
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
    async fn kv_and_plugin_state() {
        let db = Db::memory().await.unwrap();
        db.store_set("p", "", "k", &json!({"a": 1})).await.unwrap();
        db.store_set("p", "inst", "k", &json!(2)).await.unwrap();
        assert_eq!(db.store_get("p", "", "k").await.unwrap(), Some(json!({"a": 1})));
        assert_eq!(db.store_keys("p", "inst").await.unwrap(), ["k"]);
        assert_eq!(
            db.store_all("p", "inst").await.unwrap(),
            vec![("k".to_string(), json!(2))]
        );
        assert_eq!(db.store_usage("p", "inst").await.unwrap(), (1, 1));
        db.store_unset("p", "", "k").await.unwrap();
        db.set_plugin_instance_config("p", "a", &json!({"x": 1}))
            .await
            .unwrap();
        db.set_plugin_instance_config("p", "a", &json!({"x": 2}))
            .await
            .unwrap();
        assert_eq!(
            db.plugin_instance_configs("p").await.unwrap(),
            vec![("a".to_string(), json!({"x": 2}))]
        );
        db.store_set("p", "instance:a", "k", &json!(1)).await.unwrap();
        db.delete_plugin_instance_data("a").await.unwrap();
        assert!(db.plugin_instance_configs("p").await.unwrap().is_empty());
        assert!(db.store_keys("p", "instance:a").await.unwrap().is_empty());
        assert_eq!(db.store_get("p", "", "k").await.unwrap(), None);

        let state = PluginState {
            id: "greeter".into(),
            enabled: true,
            granted_permissions: vec!["net".into()],
            config: json!({"text": "hi"}),
            instances: Some(vec!["a".into()]),
        };
        db.save_plugin_state(&state).await.unwrap();
        assert_eq!(db.plugin_states().await.unwrap(), vec![state]);
    }
}
