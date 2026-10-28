//! Plugin manifests (as reported by the plugin host) and the permission catalog.

use serde::{Deserialize, Serialize};

/// How a plugin is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginKind {
    /// Native James plugin (`@james/api`).
    James,
    /// SinusBot script (`registerPlugin(...)`), run through the compat layer.
    Sinusbot,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    /// Unique id (directory or file name).
    pub id: String,
    pub kind: PluginKind,
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    /// Required James version (semver range), checked by the host.
    #[serde(default)]
    pub engine: Option<String>,
    /// Requested permissions (see [`PERMISSIONS`]).
    #[serde(default)]
    pub permissions: Vec<String>,
    /// Configuration fields shown in the web interface (SinusBot `vars` schema).
    #[serde(default)]
    pub config: Vec<serde_json::Value>,
    /// Supported platforms; empty = all.
    #[serde(default)]
    pub platforms: Vec<String>,
    /// Always active on every instance; cannot be disabled per instance.
    #[serde(default)]
    pub autorun: bool,
    #[serde(default)]
    pub hidden: bool,
    /// Ships a web page (`web/` directory) shown in the web interface.
    #[serde(default)]
    pub web: bool,
    /// Directory of the web page's files (reported by discovery; never sent to clients).
    #[serde(default, skip_serializing)]
    pub web_dir: Option<std::path::PathBuf>,
    /// The plugin's files (reported by discovery; never sent to clients).
    #[serde(default, skip_serializing)]
    pub paths: Option<PluginPaths>,
}

/// Where a plugin's code lives; checked by James to lie inside the plugin directory.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginPaths {
    /// The plugin's directory (SinusBot: the plugin directory itself).
    pub dir: std::path::PathBuf,
    /// Module to load.
    pub entry: std::path::PathBuf,
    /// Files and directories the plugin process may read (besides its data directory).
    pub readable: Vec<std::path::PathBuf>,
}

impl Manifest {
    /// Whether the plugin can run on an instance of `platform`.
    pub fn supports(&self, platform: &str) -> bool {
        self.platforms.is_empty() || self.platforms.iter().any(|p| p == platform)
    }

    /// Requested permissions that are not part of the catalog.
    pub fn unknown_permissions(&self) -> Vec<&str> {
        self.permissions
            .iter()
            .map(String::as_str)
            .filter(|p| permission(p).is_none())
            .collect()
    }
}

/// Valid plugin ids: lowercase letters, digits, `-` and `_`, 1-64 characters (no `.`, which
/// separates the parts of permission names like `plugin.<id>.<name>`).
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_'))
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Permission {
    pub id: &'static str,
    /// Enforced by James or the plugin's process sandbox (`true`), or only by the module guard
    /// inside the plugin (`false`; plugins are trusted code, see the plugin docs).
    pub enforced: bool,
    pub risk: Risk,
    pub description: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Risk {
    Low,
    Medium,
    High,
}

/// Every permission a plugin can request. Everything else of the plugin API (chat, playback,
/// reading the server state, the plugin's own store) needs no permission.
pub const PERMISSIONS: &[Permission] = &[
    Permission {
        id: "moderation",
        enforced: true,
        risk: Risk::High,
        description: "Kick, ban and move clients and change their server and channel groups",
    },
    Permission {
        id: "channels",
        enforced: true,
        risk: Risk::Medium,
        description: "Create, edit, move and delete channels",
    },
    Permission {
        id: "playlists",
        enforced: true,
        risk: Risk::Low,
        description: "Create, change and delete saved playlists",
    },
    Permission {
        id: "instances",
        enforced: true,
        risk: Risk::Medium,
        description: "Start, stop and inspect all bot instances",
    },
    Permission {
        id: "users",
        enforced: true,
        risk: Risk::High,
        description: "Read and manage James user accounts",
    },
    Permission {
        id: "web",
        enforced: true,
        // The page runs with the session of whoever opens it, administrators included.
        risk: Risk::High,
        description:
            "Serve a web page and HTTP/WebSocket endpoints; the page can act with the viewer's session",
    },
    Permission {
        id: "http",
        enforced: false,
        risk: Risk::Medium,
        description: "Make HTTP requests (fetch) through the bot's network policy",
    },
    Permission {
        id: "net",
        enforced: false,
        risk: Risk::High,
        description: "Open raw TCP/UDP connections",
    },
    Permission {
        id: "fs",
        enforced: true,
        risk: Risk::High,
        description:
            "Access all files the bot can access (without it: only its own folder and data directory)",
    },
    Permission {
        id: "db",
        enforced: false,
        risk: Risk::Medium,
        description: "Open SQLite databases in the plugin's data directory",
    },
    Permission {
        id: "process",
        enforced: true,
        risk: Risk::High,
        description: "Run external programs and load native modules",
    },
];

pub fn permission(id: &str) -> Option<&'static Permission> {
    PERMISSIONS.iter().find(|p| p.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_restricted() {
        assert!(valid_id("welcome-bot_2"));
        for bad in ["", ".hidden", "a.b", "Upper", "a/b", "a b", &"x".repeat(65)] {
            assert!(!valid_id(bad), "{bad}");
        }
    }

    #[test]
    fn platform_support_and_unknown_permissions() {
        let manifest: Manifest = serde_json::from_value(serde_json::json!({
            "id": "x", "kind": "sinusbot", "name": "X",
            "permissions": ["http", "teleport"], "platforms": ["teamspeak"]
        }))
        .unwrap();
        assert!(manifest.supports("teamspeak"));
        assert!(!manifest.supports("discord"));
        assert_eq!(manifest.unknown_permissions(), ["teleport"]);
    }
}
