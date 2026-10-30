//! Chat command framework.
//!
//! Commands are registered by name (plus aliases) in a [`CommandRegistry`]
//! shared by all instances. Built-in commands live in [`crate::builtin`];
//! plugins register their own through the plugin host.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;

use async_trait::async_trait;
use james_core::platform::{Client, MessageTarget};

use crate::instance::InstanceHandle;

pub struct CommandContext {
    pub instance: InstanceHandle,
    pub invoker: Client,
    /// Where the command was written; replies go to the same place.
    pub mode: MessageTarget,
    /// Everything after the command name, trimmed.
    pub args: String,
}

#[async_trait]
pub trait Command: Send + Sync {
    fn name(&self) -> &str;
    fn aliases(&self) -> Vec<&str> {
        Vec::new()
    }
    /// Argument syntax shown in help, e.g. `<url|search>`.
    fn usage(&self) -> &str {
        ""
    }
    fn description(&self) -> &str;
    /// Permission required to run the command (checked by the [`PermissionChecker`]).
    fn permission(&self) -> Option<&str> {
        None
    }
    /// Whether the command exists on an instance (plugin commands only exist where the
    /// plugin is enabled).
    fn available(&self, _instance: &str) -> bool {
        true
    }
    /// Whether the command may be spoken (voice commands): only James' own commands, so that
    /// plugins without the `listen` permission never receive what people say.
    fn spoken(&self) -> bool {
        false
    }
    /// Runs the command; the returned text is sent back as a reply.
    async fn run(&self, ctx: &CommandContext) -> anyhow::Result<Option<String>>;
}

/// Decides whether a client may run a command requiring a permission.
#[async_trait]
pub trait PermissionChecker: Send + Sync {
    async fn allowed(&self, instance: &InstanceHandle, client: &Client, permission: &str) -> bool;
}

/// Allows everything (used until accounts and roles are configured).
pub struct AllowAll;

#[async_trait]
impl PermissionChecker for AllowAll {
    async fn allowed(&self, _: &InstanceHandle, _: &Client, _: &str) -> bool {
        true
    }
}

#[derive(Default)]
struct Inner {
    commands: HashMap<String, Arc<dyn Command>>,
    aliases: HashMap<String, String>,
}

#[derive(Clone)]
pub struct CommandRegistry {
    inner: Arc<RwLock<Inner>>,
    permissions: Arc<RwLock<Arc<dyn PermissionChecker>>>,
    /// Commands logged per instance and client, and per instance.
    log_limit: Arc<james_core::logs::RateLimiter>,
    log_total: Arc<james_core::logs::RateLimiter>,
}

impl Default for CommandRegistry {
    fn default() -> Self {
        Self {
            inner: Default::default(),
            permissions: Arc::new(RwLock::new(Arc::new(AllowAll))),
            log_limit: Arc::new(james_core::logs::RateLimiter::new(
                20,
                std::time::Duration::from_secs(60),
            )),
            log_total: Arc::new(james_core::logs::RateLimiter::new(
                200,
                std::time::Duration::from_secs(60),
            )),
        }
    }
}

impl CommandRegistry {
    pub fn register(&self, command: Arc<dyn Command>) {
        let mut inner = self.inner.write();
        let name = command.name().to_ascii_lowercase();
        for alias in command.aliases() {
            inner.aliases.insert(alias.to_ascii_lowercase(), name.clone());
        }
        inner.commands.insert(name, command);
    }

    /// Registers a command unless its name or an alias is already taken.
    pub fn try_register(&self, command: Arc<dyn Command>) -> Result<(), String> {
        let mut inner = self.inner.write();
        let name = command.name().to_ascii_lowercase();
        let aliases: Vec<String> = command.aliases().iter().map(|a| a.to_ascii_lowercase()).collect();
        for taken in std::iter::once(&name).chain(&aliases) {
            if inner.commands.contains_key(taken) || inner.aliases.contains_key(taken) {
                return Err(format!("command name '{taken}' is already in use"));
            }
        }
        for alias in aliases {
            inner.aliases.insert(alias, name.clone());
        }
        inner.commands.insert(name, command);
        Ok(())
    }

    pub fn unregister(&self, name: &str) {
        let mut inner = self.inner.write();
        let name = name.to_ascii_lowercase();
        inner.commands.remove(&name);
        inner.aliases.retain(|_, target| *target != name);
    }

    pub fn set_permission_checker(&self, checker: Arc<dyn PermissionChecker>) {
        *self.permissions.write() = checker;
    }

    /// Whether `client` has `permission` on the instance.
    pub async fn allowed(&self, instance: &InstanceHandle, client: &Client, permission: &str) -> bool {
        let checker = self.permissions.read().clone();
        checker.allowed(instance, client, permission).await
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Command>> {
        let inner = self.inner.read();
        let name = name.to_ascii_lowercase();
        let name = inner.aliases.get(&name).unwrap_or(&name);
        inner.commands.get(name).cloned()
    }

    /// A command available on `instance`.
    pub fn get_for(&self, name: &str, instance: &str) -> Option<Arc<dyn Command>> {
        self.get(name).filter(|c| c.available(instance))
    }

    /// All commands available on `instance`, sorted by name.
    pub fn list_for(&self, instance: &str) -> Vec<Arc<dyn Command>> {
        let mut list = self.list();
        list.retain(|c| c.available(instance));
        list
    }

    /// All commands sorted by name.
    pub fn list(&self) -> Vec<Arc<dyn Command>> {
        let inner = self.inner.read();
        let mut list: Vec<_> = inner.commands.values().cloned().collect();
        list.sort_by(|a, b| a.name().cmp(b.name()));
        list
    }

    /// Whether a command of this client is logged (per client and per instance limits).
    fn log_allowed(&self, instance: &str, client: &str) -> bool {
        self.log_limit.allow(&format!("{instance}/{client}")) && self.log_total.allow(instance)
    }

    /// Parses `text` and runs the command if it starts with `prefix`.
    /// Returns `None` if the text is not a command.
    pub async fn dispatch(
        &self,
        prefix: &str,
        text: &str,
        instance: InstanceHandle,
        invoker: Client,
        mode: MessageTarget,
    ) -> Option<String> {
        let rest = text.trim().strip_prefix(prefix)?;
        let (name, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        if name.is_empty() {
            return None;
        }
        let Some(command) = self.get_for(name, instance.id()) else {
            return Some(format!(
                "Unknown command '{}'. Try {prefix}help.",
                james_core::text::escape_markup(name)
            ));
        };
        if let Some(permission) = command.permission() {
            let checker = self.permissions.read().clone();
            if !checker.allowed(&instance, &invoker, permission).await {
                if self.log_allowed(instance.id(), &invoker.uid) {
                    tracing::info!(client = ?invoker.name, "{prefix}{name} refused: no permission");
                }
                return Some("You are not allowed to use this command.".into());
            }
        }
        // Without the arguments (they may hold link codes and the like); a client sending many
        // commands cannot push everything else out of the instance's log.
        let logged = self.log_allowed(instance.id(), &invoker.uid);
        if logged {
            tracing::info!(client = ?invoker.name, "{prefix}{name}");
        }
        let ctx = CommandContext {
            instance,
            invoker,
            mode,
            args: args.trim().to_owned(),
        };
        match command.run(&ctx).await {
            Ok(reply) => reply,
            Err(e) => {
                let detail = format!("{e:#}");
                // The error may repeat the arguments: the details only in James' own debug output.
                tracing::debug!(error = ?detail, "{prefix}{name} failed");
                if logged {
                    tracing::info!("{prefix}{name} failed");
                }
                Some(format!(
                    "Error: {}",
                    james_core::text::escape_markup(&james_core::text::user_message(&detail))
                ))
            }
        }
    }
}
