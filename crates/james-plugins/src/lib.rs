//! Plugins: supervision of the Node.js plugin host, the IPC protocol and the plugin API
//! bridge (see `docs/plugins/` for the plugin developer documentation).

mod bridge;
mod command;
mod host;
pub mod ipc;
mod manager;
pub mod manifest;

pub use manager::{
    ConfigureError, DiscoveryError, HostStatus, HttpEvent, PluginInfo, PluginManager, PluginSettings,
    PluginStatus, WebHandler,
};
