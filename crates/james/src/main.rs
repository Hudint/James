mod config;
mod integration;
mod library;
mod logging;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use james_api::{AppState, LinkCodes};
use james_core::source::SourceRegistry;
use james_db::Db;
use james_runtime::{AudioEngineFactory, InstanceConfig, InstanceManager, Services};
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

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let config = Config::load(&args.config)?;
    logging::init(&config.log);
    // Created before the main runtime so it is independent of it.
    let audio = james_runtime::audio_runtime(config.tools.audio_threads)?;
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run(args, config, audio))
}

/// Creates a directory readable only by the current user (it holds keys and the database).
fn private_dir(path: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(path).with_context(|| format!("cannot create directory {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

async fn run(args: Args, config: Config, audio: tokio::runtime::Handle) -> anyhow::Result<()> {
    tracing::info!(version = env!("CARGO_PKG_VERSION"), config = %args.config.display(), "starting James");

    private_dir(&config.data_dir)?;
    // Private scratch space (cookie copies for yt-dlp); leftovers of a crash are removed.
    let work_dir = config.data_dir.join("tmp");
    let _ = std::fs::remove_dir_all(&work_dir);
    private_dir(&work_dir)?;
    let media_dir = config.data_dir.join("media");
    std::fs::create_dir_all(&media_dir)?;
    let db = Db::open(&config.data_dir.join("james.db"))
        .await
        .context("cannot open database")?;

    james_sources::set_max_concurrent_tools(config.tools.max_concurrent);
    let mut media_dirs = vec![media_dir.clone()];
    media_dirs.extend(config.media.dirs.iter().cloned());
    let mut sources = SourceRegistry::default();
    sources.register(
        0,
        Arc::new(LocalResolver::new(
            media_dirs.clone(),
            config.tools.ffprobe.clone(),
        )),
    );
    // Every outbound media connection goes through the egress proxy (SSRF protection).
    // The PO token provider sidecar usually lives on a private address.
    let exempt: Vec<String> = config
        .ytdlp
        .pot_provider
        .as_deref()
        .and_then(|p| url::Url::parse(p).ok())
        .and_then(|u| Some(format!("{}:{}", u.host_str()?, u.port_or_known_default()?)))
        .into_iter()
        .collect();
    let egress = james_sources::egress::EgressProxy::start(config.network.clone(), exempt).await?;
    sources.register(
        10,
        Arc::new(StreamResolver::new(config.network.clone(), Some(egress.url()))),
    );
    sources.register(
        100,
        Arc::new(YtDlpResolver::new(
            config.ytdlp.clone(),
            config.network.clone(),
            Some(egress.url().to_owned()),
            work_dir.clone(),
        )),
    );

    let mut services = Services::new(
        sources,
        vec![Arc::new(james_teamspeak::TeamSpeakFactory)],
        Arc::new(AudioEngineFactory::new(audio)),
        Arc::new(integration::DbStore(db.clone())),
        config.tools.ffmpeg.clone(),
    );
    services.http_proxy = Some(egress.url().to_owned());
    let links = LinkCodes::default();
    james_runtime::builtin::register(&services.commands);
    integration::register_link_command(&services.commands, db.clone(), links.clone());
    services
        .commands
        .set_permission_checker(Arc::new(integration::DbPermissions(db.clone())));

    let manager = Arc::new(InstanceManager::new(services));
    load_instances(&db, &manager, &config.instances).await?;
    manager.start_autostart().await;

    let scanner = Arc::new(library::LibraryScanner::new(
        db.clone(),
        media_dirs,
        media_dir.join("uploads"),
        config.tools.ffprobe.clone(),
    ));
    {
        let scanner = scanner.clone();
        tokio::spawn(async move {
            if let Err(e) = james_api::LibraryService::scan(&*scanner).await {
                tracing::warn!(error = %format!("{e:#}"), "initial library scan failed");
            }
        });
    }

    // First run: a one-time setup token proves that whoever creates the admin account has
    // access to the server (the web interface may already be reachable from the internet).
    let setup_token = if db.user_count().await? == 0 {
        let token = james_api::linking::random_code(24);
        tracing::warn!(setup_token = %token, "no account exists yet: open the web interface and enter this setup token");
        Some(token)
    } else {
        None
    };
    let state = AppState {
        setup_token: Arc::new(parking_lot::Mutex::new(setup_token)),
        db: db.clone(),
        manager: manager.clone(),
        links,
        logins: Default::default(),
        actions: Default::default(),
        trusted_proxies: Arc::new(config.http.trusted_proxies.clone()),
        allowed_hosts: Arc::new(config.http.allowed_hosts.clone()),
        ws_revalidate: std::time::Duration::from_secs(30),
        library: scanner,
        secure_cookies: config.http.secure_cookies,
    };
    let app = james_api::app(state, config.http.web_root.clone());
    if !config.http.bind.ip().is_loopback() && config.http.allowed_hosts.is_empty() {
        tracing::warn!(
            "the web interface is reachable from other hosts: set http.allowed_hosts to its domain \
             (protects against DNS rebinding) and serve it via HTTPS with http.secure_cookies = true"
        );
    }
    let listener = tokio::net::TcpListener::bind(config.http.bind)
        .await
        .with_context(|| format!("cannot listen on {}", config.http.bind))?;
    tracing::info!(address = %config.http.bind, "web interface and API listening");

    // Connect info lets the login throttle see client addresses.
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;
    tracing::info!("shutting down");
    manager.stop_all().await;
    Ok(())
}

/// Instances live in the database; instances from the config file are imported once.
async fn load_instances(
    db: &Db,
    manager: &InstanceManager,
    from_config: &[InstanceConfig],
) -> anyhow::Result<()> {
    let mut stored: Vec<InstanceConfig> = db.instances().await?;
    for instance in from_config {
        if !stored.iter().any(|s| s.id == instance.id) {
            tracing::info!(instance = %instance.id, "importing instance from config file");
            db.save_instance(&instance.id, instance).await?;
            stored.push(instance.clone());
        }
    }
    for instance in stored {
        if let Err(e) = manager.upsert(instance.clone()) {
            tracing::error!(instance = %instance.id, error = %format!("{e:#}"), "invalid instance configuration");
        }
    }
    Ok(())
}

/// Resolves on SIGINT (Ctrl+C) or SIGTERM (`docker stop`).
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}
