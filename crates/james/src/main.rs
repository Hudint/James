mod config;
mod logging;
mod store;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use james_core::source::SourceRegistry;
use james_runtime::{InstanceManager, Services};
use james_sources::local::LocalResolver;
use james_sources::stream::StreamResolver;
use james_sources::ytdlp::YtDlpResolver;

use crate::config::Config;

#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// Path to the configuration file.
    #[arg(short, long, env = "JAMES_CONFIG", default_value = "james.toml")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let config = Config::load(&args.config)?;
    logging::init(&config.log);
    tracing::info!(version = env!("CARGO_PKG_VERSION"), config = %args.config.display(), "starting James");

    let media_dir = config.data_dir.join("media");
    std::fs::create_dir_all(&media_dir)
        .with_context(|| format!("cannot create data directory {}", config.data_dir.display()))?;

    let mut sources = SourceRegistry::default();
    let mut media_dirs = vec![media_dir];
    media_dirs.extend(config.media.dirs.iter().cloned());
    sources.register(
        0,
        Arc::new(LocalResolver::new(media_dirs, config.tools.ffprobe.clone())),
    );
    sources.register(10, Arc::new(StreamResolver::new()));
    sources.register(100, Arc::new(YtDlpResolver::new(config.ytdlp.clone())));

    let store = Arc::new(store::FileStore::open(config.data_dir.join("state.json"))?);
    let services = Services::new(
        sources,
        vec![Arc::new(james_teamspeak::TeamSpeakFactory)],
        store,
        config.tools.ffmpeg.clone(),
    );
    james_runtime::builtin::register(&services.commands);

    let manager = InstanceManager::new(services);
    for instance in &config.instances {
        manager
            .upsert(instance.clone())
            .with_context(|| format!("invalid instance '{}'", instance.id))?;
    }
    manager.start_autostart();

    tokio::signal::ctrl_c().await?;
    tracing::info!("shutting down");
    manager.stop_all();
    tokio::time::sleep(Duration::from_millis(500)).await;
    Ok(())
}
