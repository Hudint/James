//! Instance runtime: one [`Instance`](instance::InstanceHandle) per bot, each
//! connecting a platform adapter, an audio engine and a player.

pub mod builtin;
pub mod commands;
pub mod instance;
pub mod manager;
pub mod player;

pub use commands::{Command, CommandContext, CommandRegistry, PermissionChecker};
pub use instance::{
    AudioSettings, EventEnvelope, InstanceConfig, InstanceEvent, InstanceHandle, InstanceStatus, PlayMode,
};
pub use manager::{InstanceManager, InstanceStore, MemoryStore, PlatformFactory, Services};
pub use player::{QueueItem, RepeatMode, Requester};
