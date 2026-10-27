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
        db.store_unset("p", "", "k").await.unwrap();
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
