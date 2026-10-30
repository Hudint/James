//! Glue between the runtime and the database: persistence, chat permissions, `!link`.

use std::sync::Arc;

use async_trait::async_trait;
use james_api::LinkCodes;
use james_core::platform::Client;
use james_core::text::escape_markup;
use james_db::permissions::allowed;
use james_db::Db;
use james_runtime::{Command, CommandContext, InstanceHandle, InstanceStore, PermissionChecker};

/// Runtime persistence backed by the database.
pub struct DbStore(pub Db);

#[async_trait]
impl InstanceStore for DbStore {
    async fn load_credentials(&self, instance: &str) -> Option<serde_json::Value> {
        self.0.instance_credentials(instance).await.unwrap_or_else(|e| {
            tracing::error!(%instance, error = %e, "failed to load credentials");
            None
        })
    }

    async fn save_credentials(&self, instance: &str, data: serde_json::Value) -> anyhow::Result<()> {
        Ok(self.0.save_instance_credentials(instance, &data).await?)
    }

    async fn loudness(&self, source: &str, locator: &str) -> Option<f32> {
        self.0.loudness(source, locator).await.ok().flatten()
    }

    async fn save_loudness(&self, source: &str, locator: &str, lufs: f32) {
        if let Err(e) = self.0.save_loudness(source, locator, lufs).await {
            tracing::warn!(error = %e, "failed to save loudness");
        }
    }
}

/// Names edited in the library, used when local files are played.
pub struct LibraryNames(pub Db);

#[async_trait]
impl james_sources::local::LibraryIndex for LibraryNames {
    async fn names(&self, path: &std::path::Path) -> Option<james_sources::local::FileNames> {
        let names = self
            .0
            .library_names(path.to_str()?)
            .await
            .inspect_err(|e| tracing::debug!(error = %e, "failed to read library names"))
            .ok()??;
        Some(james_sources::local::FileNames {
            title: names.title,
            artist: names.artist,
            album: names.album,
        })
    }

    async fn path(&self, id: i64) -> anyhow::Result<Option<std::path::PathBuf>> {
        Ok(self.0.library_track(id).await?.map(|track| track.path.into()))
    }
}

/// Chat command permissions from roles (`everyone`, linked account, platform groups).
pub struct DbPermissions(pub Db);

#[async_trait]
impl PermissionChecker for DbPermissions {
    async fn allowed(&self, instance: &InstanceHandle, client: &Client, permission: &str) -> bool {
        match self
            .0
            .client_grants(instance.platform_id(), &client.uid, &client.server_groups)
            .await
        {
            Ok(grants) => allowed(&grants, permission, Some(instance.id())),
            Err(e) => {
                tracing::error!(error = %e, "permission lookup failed");
                false
            }
        }
    }
}

/// `!link <code>`: links the sender's platform identity to the account that requested the code.
pub struct LinkCommand {
    pub db: Db,
    pub links: LinkCodes,
}

#[async_trait]
impl Command for LinkCommand {
    fn name(&self) -> &str {
        "link"
    }
    fn usage(&self) -> &str {
        "<code>"
    }
    fn description(&self) -> &str {
        "Links your identity to your web account (get the code in the web interface)"
    }
    async fn run(&self, ctx: &CommandContext) -> anyhow::Result<Option<String>> {
        if ctx.mode != james_core::platform::MessageTarget::Client {
            // A code typed in a channel is visible to others: refuse and let it expire unused.
            return Ok(Some("Send !link to the bot in a private message.".into()));
        }
        if ctx.invoker.uid.is_empty() {
            anyhow::bail!("your client has no stable identity");
        }
        let Some(user_id) = self.links.redeem(&ctx.args) else {
            return Ok(Some("Invalid or expired code.".into()));
        };
        let user = self.db.user(user_id).await?;
        let platform = ctx.instance.platform_id();
        if let Some(owner) = self.db.user_by_identity(platform, &ctx.invoker.uid).await? {
            if owner.id != user.id {
                return Ok(Some(
                    "This identity is already linked to another account. Unlink it there first.".into(),
                ));
            }
        }
        self.db
            .link_identity(platform, &ctx.invoker.uid, user.id, &ctx.invoker.name)
            .await?;
        Ok(Some(format!("Linked to account '{}'.", user.username)))
    }
}

pub fn register_link_command(registry: &james_runtime::CommandRegistry, db: Db, links: LinkCodes) {
    registry.register(Arc::new(LinkCommand { db, links }));
}

/// `!ytdl`: saves a track (YouTube, …) to the library and plays it from there.
pub struct SaveCommand {
    pub library: Arc<dyn james_api::LibraryService>,
}

#[async_trait]
impl Command for SaveCommand {
    fn name(&self) -> &str {
        "ytdl"
    }
    fn aliases(&self) -> Vec<&str> {
        vec!["save"]
    }
    fn usage(&self) -> &str {
        "<url|search>"
    }
    fn description(&self) -> &str {
        "Saves a YouTube video (link or search) to the library and plays it"
    }
    fn permission(&self) -> Option<&str> {
        Some("library.download")
    }
    async fn run(&self, ctx: &CommandContext) -> anyhow::Result<Option<String>> {
        if ctx.args.is_empty() {
            anyhow::bail!("missing argument");
        }
        let input = james_core::source::youtube_input(&ctx.args);
        let library = self.library.clone();
        let instance = ctx.instance.clone();
        let invoker = ctx.invoker.clone();
        let mode = ctx.mode;
        // Saving takes a while: in its own task, so the client's other commands are not held up.
        tokio::spawn(async move {
            let owner = format!("client:{}:{}", instance.platform_id(), invoker.uid);
            let reply = match library.save(&input, &owner).await {
                Ok(track) => {
                    let may_play = instance
                        .services()
                        .commands
                        .allowed(&instance, &invoker, "playback.play")
                        .await;
                    let requester = Some(james_runtime::Requester {
                        uid: invoker.uid.clone(),
                        name: invoker.name.clone(),
                    });
                    if !may_play {
                        format!("Saved to the library: {}", escape_markup(&track.title))
                    } else {
                        match instance
                            .play(
                                &format!("library:{}", track.id),
                                james_runtime::PlayMode::Now,
                                requester,
                            )
                            .await
                        {
                            Ok(_) => {
                                format!("Saved to the library, playing: {}", escape_markup(&track.title))
                            }
                            Err(e) => format!(
                                "Saved to the library: {} (cannot play it: {})",
                                escape_markup(&track.title),
                                escape_markup(&james_core::text::user_message(&format!("{e:#}")))
                            ),
                        }
                    }
                }
                Err(e) => format!(
                    "Error: {}",
                    escape_markup(&james_core::text::user_message(&format!("{e:#}")))
                ),
            };
            let _ = instance.reply(mode, &invoker, reply).await;
        });
        Ok(Some("Saving to the library…".into()))
    }
}

pub fn register_save_command(
    registry: &james_runtime::CommandRegistry,
    library: Arc<dyn james_api::LibraryService>,
) {
    registry.register(Arc::new(SaveCommand { library }));
}
