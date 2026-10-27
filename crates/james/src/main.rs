mod config;
mod logging;

use std::path::PathBuf;

use clap::Parser;

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

    tokio::signal::ctrl_c().await?;
    tracing::info!("shutting down");
    Ok(())
}
