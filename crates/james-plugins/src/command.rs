//! Chat commands registered by plugins.

use std::sync::Weak;
use std::time::Duration;

use async_trait::async_trait;
use james_runtime::{Command, CommandContext};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::manager::PluginManager;

/// How long a plugin may take to answer a command.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PluginCommand {
    #[serde(skip)]
    pub manager: Weak<PluginManager>,
    #[serde(skip)]
    pub plugin: String,
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub usage: String,
    #[serde(default)]
    pub description: String,
    /// Full permission id (`plugin.<id>.<name>`), see [`PluginCommand::validate`].
    #[serde(default)]
    pub permission: Option<String>,
}

impl PluginCommand {
    /// Checks names and namespaces the permission: plugins can only require their own
    /// permissions (`plugin.<id>.<name>`), never built-in ones.
    pub fn validate(&mut self, plugin: &str) -> Result<(), String> {
        let valid_name = |n: &str| {
            !n.is_empty() && n.len() <= 32 && n.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        };
        if !valid_name(&self.name) || !self.aliases.iter().all(|a| valid_name(a)) || self.aliases.len() > 8 {
            return Err("command names must be 1-32 letters, digits, '-' or '_' (at most 8 aliases)".into());
        }
        if self.usage.len() > 200 || self.description.len() > 500 {
            return Err("usage or description too long".into());
        }
        self.permission = match self.permission.take() {
            Some(p) if valid_name(&p) => Some(format!("plugin.{plugin}.{p}")),
            Some(_) => return Err("invalid permission name".into()),
            None => None,
        };
        self.plugin = plugin.to_owned();
        Ok(())
    }
}

#[async_trait]
impl Command for PluginCommand {
    fn name(&self) -> &str {
        &self.name
    }
    fn aliases(&self) -> Vec<&str> {
        self.aliases.iter().map(String::as_str).collect()
    }
    fn usage(&self) -> &str {
        &self.usage
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn permission(&self) -> Option<&str> {
        self.permission.as_deref()
    }
    fn available(&self, instance: &str) -> bool {
        self.manager
            .upgrade()
            .is_some_and(|m| m.has_context(&self.plugin, instance))
    }

    async fn run(&self, ctx: &CommandContext) -> anyhow::Result<Option<String>> {
        let manager = self
            .manager
            .upgrade()
            .ok_or_else(|| anyhow::anyhow!("plugins are not available"))?;
        let mut params = json!({
            "instance": ctx.instance.id(),
            "name": self.name,
            "args": ctx.args,
            "mode": ctx.mode,
            "invoker": ctx.invoker,
        });
        // Client IP addresses only reach plugins that may moderate.
        if !manager.granted(&self.plugin, "moderation") {
            james_core::platform::strip_client_addresses(&mut params);
        }
        let result = manager
            .request(&self.plugin, "command.run", params, COMMAND_TIMEOUT)
            .await?;
        Ok(match result {
            Value::String(text) if !text.is_empty() => Some(text),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(value: Value) -> PluginCommand {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn permissions_are_namespaced() {
        let mut c = command(json!({"name": "greet", "permission": "use"}));
        c.validate("welcome").unwrap();
        assert_eq!(c.permission.as_deref(), Some("plugin.welcome.use"));

        let mut builtin = command(json!({"name": "greet", "permission": "playback.play"}));
        assert!(builtin.validate("welcome").is_err(), "dots are rejected");
        let mut bad = command(json!({"name": "a b"}));
        assert!(bad.validate("welcome").is_err());
    }
}
