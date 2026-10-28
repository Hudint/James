//! Configuration loading.
//!
//! Sources, in increasing priority: built-in defaults, the TOML config file,
//! and environment variables prefixed with `JAMES_` (nested keys separated by
//! `__`, e.g. `JAMES_HTTP__BIND=0.0.0.0:8087`).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use figment::providers::{Env, Format, Serialized, Toml};
use figment::Figment;
use james_runtime::InstanceConfig;
use james_sources::net::NetworkPolicy;
use james_sources::ytdlp::YtDlpConfig;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Directory for the database, media library, plugins and caches.
    pub data_dir: PathBuf,
    pub http: HttpConfig,
    pub log: LogConfig,
    pub tools: ToolsConfig,
    pub media: MediaConfig,
    pub ytdlp: YtDlpConfig,
    pub network: NetworkPolicy,
    pub plugins: PluginsConfig,
    /// Instances defined in the config file (more can be created via the API).
    pub instances: Vec<InstanceConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ToolsConfig {
    pub ffmpeg: String,
    pub ffprobe: String,
    /// Maximum number of yt-dlp/ffprobe processes running at once.
    pub max_concurrent: usize,
    /// Threads of the dedicated audio runtime.
    pub audio_threads: usize,
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            ffmpeg: "ffmpeg".into(),
            ffprobe: "ffprobe".into(),
            max_concurrent: 8,
            audio_threads: 2,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PluginsConfig {
    pub enabled: bool,
    /// Node.js 24+ executable.
    pub node: String,
    /// Entry point of the plugin host.
    pub host: PathBuf,
    /// Plugin directory; default `<data_dir>/plugins`.
    pub dir: Option<PathBuf>,
}

impl Default for PluginsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            node: "node".into(),
            host: PathBuf::from("plugin-host/src/main.ts"),
            dir: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MediaConfig {
    /// Additional media directories; `<data_dir>/media` is always included.
    pub dirs: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HttpConfig {
    pub bind: SocketAddr,
    /// Directory with the built web UI; `None` serves the API only.
    pub web_root: Option<PathBuf>,
    /// Mark session cookies `Secure` (enable when served via HTTPS).
    pub secure_cookies: bool,
    /// Addresses of reverse proxies whose `X-Forwarded-For` header is trusted. The proxy must
    /// pass the original `Host` header (nginx: `proxy_set_header Host $host;`), otherwise
    /// browsers without `Sec-Fetch-Site` fail the same-origin check.
    pub trusted_proxies: Vec<std::net::IpAddr>,
    /// Host names the web interface is reachable under, e.g. `["bot.example.com"]`. Requests
    /// for other names are refused (protects against DNS rebinding). Empty = no check.
    pub allowed_hosts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LogConfig {
    /// Filter directive in `tracing_subscriber::EnvFilter` syntax.
    pub level: String,
    pub format: LogFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    Pretty,
    Json,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("data"),
            http: HttpConfig::default(),
            log: LogConfig::default(),
            tools: ToolsConfig::default(),
            media: MediaConfig::default(),
            ytdlp: YtDlpConfig::default(),
            network: NetworkPolicy::default(),
            plugins: PluginsConfig::default(),
            instances: Vec::new(),
        }
    }
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            bind: ([127, 0, 0, 1], 8087).into(),
            web_root: None,
            secure_cookies: false,
            trusted_proxies: Vec::new(),
            allowed_hosts: Vec::new(),
        }
    }
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: "info".into(),
            format: LogFormat::Pretty,
        }
    }
}

impl Config {
    pub fn figment(path: &Path) -> Figment {
        Figment::from(Serialized::defaults(Config::default()))
            .merge(Toml::file(path))
            .merge(Env::prefixed("JAMES_").split("__"))
    }

    pub fn load(path: &Path) -> anyhow::Result<Self> {
        Ok(Self::figment(path).extract()?)
    }
}

#[cfg(test)]
#[allow(clippy::result_large_err)] // figment::Jail's closure signature
mod tests {
    use super::*;

    #[test]
    fn defaults_without_file() {
        figment::Jail::expect_with(|_| {
            let config = Config::load(Path::new("missing.toml")).unwrap();
            assert_eq!(config.http.bind.port(), 8087);
            assert_eq!(config.log.format, LogFormat::Pretty);
            Ok(())
        });
    }

    #[test]
    fn file_and_env_override() {
        figment::Jail::expect_with(|jail| {
            jail.create_file(
                "james.toml",
                "data_dir = \"/srv/james\"\n[log]\nformat = \"json\"\n",
            )?;
            jail.set_env("JAMES_HTTP__BIND", "0.0.0.0:9000");
            let config = Config::load(Path::new("james.toml")).unwrap();
            assert_eq!(config.data_dir, PathBuf::from("/srv/james"));
            assert_eq!(config.log.format, LogFormat::Json);
            assert_eq!(config.http.bind.port(), 9000);
            Ok(())
        });
    }

    #[test]
    fn parses_instances() {
        figment::Jail::expect_with(|jail| {
            jail.create_file(
                "james.toml",
                r#"
                [[instances]]
                id = "music"
                platform = "teamspeak"
                [instances.settings]
                address = "ts.example.com"
                nickname = "DJ"
                [instances.audio]
                volume = 30
                "#,
            )?;
            let config = Config::load(Path::new("james.toml")).unwrap();
            let instance = &config.instances[0];
            assert_eq!(instance.id, "music");
            assert_eq!(instance.settings["address"], "ts.example.com");
            assert_eq!(instance.audio.volume, 30);
            assert_eq!(instance.command_prefix, "!");
            Ok(())
        });
    }

    #[test]
    fn rejects_unknown_keys() {
        figment::Jail::expect_with(|jail| {
            jail.create_file("james.toml", "unknown_key = 1\n")?;
            assert!(Config::load(Path::new("james.toml")).is_err());
            Ok(())
        });
    }
}
