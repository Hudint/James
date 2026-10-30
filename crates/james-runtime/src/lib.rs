//! Instance runtime: one instance per bot, each connecting a platform adapter,
//! an audio engine and a player.

pub mod builtin;
pub mod commands;
pub mod engine;
pub mod instance;
pub mod manager;
pub mod player;
pub mod speech;
pub mod store;

pub use commands::{Command, CommandContext, CommandRegistry, PermissionChecker};
pub use engine::{audio_runtime, AudioEngineFactory, EngineFactory, PlaybackEngine};
pub use instance::{
    AudioSettings, EventEnvelope, InstanceConfig, InstanceEvent, InstanceHandle, InstanceStatus, PlayMode,
    SpeechQueueFull, SpeechSettings,
};
pub use manager::{ConfigureError, ConfigureMode, InstanceManager, PlatformFactory, Services};
pub use player::{QueueItem, RepeatMode, Requester};
pub use store::{InstanceStore, MemoryStore};
