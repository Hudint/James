use tracing_subscriber::EnvFilter;

use crate::config::{LogConfig, LogFormat};

/// Installs the global tracing subscriber. `RUST_LOG` takes precedence over the config.
pub fn init(config: &LogConfig) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&config.level));
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    match config.format {
        LogFormat::Pretty => builder.init(),
        LogFormat::Json => builder.json().init(),
    }
}
