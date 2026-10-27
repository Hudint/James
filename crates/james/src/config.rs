//! Configuration loading.
//!
//! Sources, in increasing priority: built-in defaults, the TOML config file,
//! and environment variables prefixed with `JAMES_` (nested keys separated by
//! `__`, e.g. `JAMES_HTTP__BIND=0.0.0.0:8087`).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use figment::providers::{Env, Format, Serialized, Toml};
use figment::Figment;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Directory for the database, media library, plugins and caches.
    pub data_dir: PathBuf,
    pub http: HttpConfig,
    pub log: LogConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HttpConfig {
    pub bind: SocketAddr,
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
        }
    }
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            bind: ([127, 0, 0, 1], 8087).into(),
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
    fn rejects_unknown_keys() {
        figment::Jail::expect_with(|jail| {
            jail.create_file("james.toml", "unknown_key = 1\n")?;
            assert!(Config::load(Path::new("james.toml")).is_err());
            Ok(())
        });
    }
}
