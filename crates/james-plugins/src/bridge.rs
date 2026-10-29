//! Plugin API calls from the host (`call` requests): `{plugin, instance?, method, params}`.
//!
//! Every method checks that the plugin is running, that it is active on the instance it acts
//! on (unless it holds `instances`) and that it holds the permission the method needs.
//! Methods are documented in `docs/plugins/ipc.md`.

use std::time::Duration;

use james_core::platform::{ChannelParams, MessageTarget, PlatformCommand};
use james_core::source::Track;
use james_runtime::{InstanceHandle, PlayMode, Requester};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::command::PluginCommand;
use crate::manager::{PluginManager, WebHandler};

/// Store quotas per scope.
const MAX_STORE_VALUE: usize = 1 << 20;
const MAX_STORE_KEYS: i64 = 10_000;
const MAX_STORE_BYTES: i64 = 64 << 20;
const MAX_KEY_LEN: usize = 256;
const MAX_GLOBAL_KEYS: i64 = 1_000;
const MAX_GLOBAL_BYTES: i64 = 4 << 20;
/// Upper bound for text sent to the voice platform (TeamSpeak cuts at 8 KiB anyway).
const MAX_TEXT: usize = 8 * 1024;

#[derive(Deserialize)]
struct Call {
    #[serde(default)]
    instance: Option<String>,
    method: String,
    #[serde(default)]
    params: Value,
}

type Result<T> = std::result::Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    format!("{e:#}")
}

fn args<T: DeserializeOwned>(params: Value) -> Result<T> {
    serde_json::from_value(params).map_err(|e| format!("invalid arguments: {e}"))
}

fn ok<T: serde::Serialize>(value: T) -> Result<Value> {
    serde_json::to_value(value).map_err(err)
}

fn text(value: String) -> Result<String> {
    if value.len() > MAX_TEXT {
        return Err(format!("text longer than {MAX_TEXT} bytes"));
    }
    Ok(value)
}

struct Ctx<'a> {
    manager: &'a PluginManager,
    plugin: String,
    instance: Option<String>,
}

impl Ctx<'_> {
    fn require(&self, permission: &str) -> Result<()> {
        if self.manager.granted(&self.plugin, permission) {
            Ok(())
        } else {
            Err(format!("the plugin lacks the permission '{permission}'"))
        }
    }

    /// The instance the call acts on; plugins may only act on instances they are active on
    /// unless they hold `instances`.
    fn instance(&self) -> Result<InstanceHandle> {
        let id = self.instance.as_deref().ok_or("this method needs an instance")?;
        if !self.manager.has_context(&self.plugin, id) && !self.manager.granted(&self.plugin, "instances") {
            return Err(format!("the plugin is not active on instance '{id}'"));
        }
        self.manager
            .instances
            .get(id)
            .ok_or_else(|| format!("instance '{id}' is not running"))
    }

    /// The call's instance, which the plugin must be active on (not merely existing).
    fn active_instance(&self) -> Result<&str> {
        let id = self.instance.as_deref().ok_or("this method needs an instance")?;
        if self.manager.has_context(&self.plugin, id) {
            Ok(id)
        } else {
            Err(format!("the plugin is not active on instance '{id}'"))
        }
    }

    fn requester(&self) -> Requester {
        Requester {
            uid: format!("plugin:{}", self.plugin),
            name: self.plugin.clone(),
        }
    }

    /// Store scope: (`plugin`, `scope`) as stored in the database.
    fn scope(&self, scope: &str) -> Result<(String, String)> {
        match scope {
            "plugin" => Ok((self.plugin.clone(), String::new())),
            "instance" => {
                let instance = self.active_instance()?;
                Ok((self.plugin.clone(), format!("instance:{instance}")))
            }
            "global" => Ok((String::new(), String::new())),
            other => Err(format!("unknown store scope '{other}'")),
        }
    }
}

/// Handles a call of `plugin` (bound to the connection it came from, never taken from the
/// request).
pub(crate) async fn call(manager: &PluginManager, plugin: &str, params: Value) -> Result<Value> {
    let call: Call = args(params)?;
    if !manager.is_running(plugin) {
        return Err(format!("plugin '{plugin}' is not running"));
    }
    let ctx = Ctx {
        manager,
        plugin: plugin.to_owned(),
        instance: call.instance,
    };
    dispatch(&ctx, &call.method, call.params).await
}

async fn dispatch(ctx: &Ctx<'_>, method: &str, params: Value) -> Result<Value> {
    let (area, _) = method.split_once('.').unwrap_or((method, ""));
    match area {
        "instance" | "instances" => instance(ctx, method, params).await,
        "chat" | "client" | "bot" | "channel" => platform(ctx, method, params).await,
        "media" => media(ctx, method, params).await,
        "store" => store(ctx, method, params).await,
        "playlists" | "library" => library(ctx, method, params).await,
        "users" => users(ctx, method, params).await,
        "commands" => commands(ctx, method, params),
        "web" => web(ctx, method, params),
        "tts" => tts(ctx, method, params).await,
        "engine" => engine(ctx, method, params).await,
        _ => Err(format!("unknown method '{method}'")),
    }
}

// ---- instances ------------------------------------------------------------------------------

async fn instance(ctx: &Ctx<'_>, method: &str, params: Value) -> Result<Value> {
    #[derive(Deserialize)]
    struct Id {
        id: String,
    }
    match method {
        "instance.status" => ok(ctx.instance()?.status().await.map_err(err)?),
        "instance.queue" => ok(ctx.instance()?.queue().await.map_err(err)?),
        "instance.snapshot" => {
            let snapshot = ctx.instance()?.snapshot().await.map_err(err)?;
            // Client IP addresses only reach plugins that may moderate.
            ok(if ctx.manager.granted(&ctx.plugin, "moderation") {
                snapshot
            } else {
                snapshot.without_addresses()
            })
        }
        "instance.info" => {
            let handle = ctx.instance()?;
            let config = ctx
                .manager
                .instances
                .config(handle.id())
                .ok_or("unknown instance")?;
            ok(json!({
                "id": config.id,
                "name": config.name,
                "platform": config.platform,
                "commandPrefix": config.command_prefix,
                "nickname": config.settings.get("nickname"),
                "defaultChannel": config.settings.get("channel"),
            }))
        }
        "instances.list" => {
            let running: Vec<String> = ctx
                .manager
                .instances
                .running()
                .iter()
                .map(|h| h.id().to_owned())
                .collect();
            let all = ctx.manager.granted(&ctx.plugin, "instances");
            ok(ctx
                .manager
                .instances
                .configs()
                .into_iter()
                .filter(|c| all || ctx.manager.has_context(&ctx.plugin, &c.id))
                .map(|c| {
                    json!({
                        "id": c.id,
                        "name": c.name,
                        "platform": c.platform,
                        "running": running.contains(&c.id),
                    })
                })
                .collect::<Vec<_>>())
        }
        "instances.start" | "instances.stop" | "instances.restart" => {
            ctx.require("instances")?;
            let Id { id } = args(params)?;
            let instances = &ctx.manager.instances;
            match method {
                "instances.start" => instances.start(&id).await.map(|_| ()).map_err(err)?,
                "instances.stop" => instances.stop(&id).await,
                _ => instances.restart(&id).await.map(|_| ()).map_err(err)?,
            }
            ok(())
        }
        _ => Err(format!("unknown method '{method}'")),
    }
}

// ---- voice platform -------------------------------------------------------------------------

async fn platform(ctx: &Ctx<'_>, method: &str, params: Value) -> Result<Value> {
    #[derive(Deserialize)]
    struct Message {
        target: MessageTarget,
        #[serde(default)]
        client: Option<String>,
        text: String,
    }
    #[derive(Deserialize)]
    struct ClientText {
        client: String,
        #[serde(default)]
        text: String,
    }
    #[derive(Deserialize)]
    struct Move {
        client: String,
        channel: String,
    }
    #[derive(Deserialize)]
    struct Kick {
        client: String,
        #[serde(default)]
        from_server: bool,
        #[serde(default)]
        reason: String,
    }
    #[derive(Deserialize)]
    struct Ban {
        client: String,
        #[serde(default)]
        seconds: u64,
        #[serde(default)]
        reason: String,
    }
    #[derive(Deserialize)]
    struct Group {
        client: String,
        group: String,
    }
    #[derive(Deserialize)]
    struct ChannelGroup {
        client: String,
        channel: String,
        group: String,
    }
    #[derive(Deserialize)]
    struct Client {
        client: String,
    }
    #[derive(Deserialize)]
    struct SelfMove {
        channel: String,
        #[serde(default)]
        password: Option<String>,
    }
    #[derive(Deserialize)]
    struct Text {
        text: String,
    }
    #[derive(Deserialize)]
    struct Away {
        #[serde(default)]
        message: Option<String>,
    }
    #[derive(Deserialize)]
    struct Channel {
        channel: String,
    }
    #[derive(Deserialize)]
    struct ChannelEdit {
        channel: String,
        params: ChannelParams,
    }
    #[derive(Deserialize)]
    struct ChannelMove {
        channel: String,
        #[serde(default)]
        parent: Option<String>,
        #[serde(default)]
        order: Option<String>,
    }
    #[derive(Deserialize)]
    struct ChannelDelete {
        channel: String,
        #[serde(default)]
        force: bool,
    }
    #[derive(Deserialize)]
    struct Subscribe {
        channel: String,
        subscribe: bool,
    }

    let handle = ctx.instance()?;
    let unit = |r: anyhow::Result<()>| r.map(|()| Value::Null).map_err(err);
    match method {
        "chat.send" => {
            let m: Message = args(params)?;
            let text = text(m.text)?;
            unit(handle.send_message(m.target, m.client, text).await)
        }
        "client.poke" => {
            let a: ClientText = args(params)?;
            let text = text(a.text)?;
            unit(
                handle
                    .platform(|reply| PlatformCommand::Poke {
                        client: a.client,
                        text,
                        reply,
                    })
                    .await,
            )
        }
        "client.requestInfo" => {
            let Client { client } = args(params)?;
            unit(
                handle
                    .platform(|reply| PlatformCommand::RequestClientInfo { client, reply })
                    .await,
            )
        }
        "client.move" => {
            ctx.require("moderation")?;
            let Move { client, channel } = args(params)?;
            unit(
                handle
                    .platform(|reply| PlatformCommand::MoveClient {
                        client,
                        channel,
                        reply,
                    })
                    .await,
            )
        }
        "client.kick" => {
            ctx.require("moderation")?;
            let a: Kick = args(params)?;
            unit(
                handle
                    .platform(|reply| PlatformCommand::Kick {
                        client: a.client,
                        from_server: a.from_server,
                        reason: a.reason,
                        reply,
                    })
                    .await,
            )
        }
        "client.ban" => {
            ctx.require("moderation")?;
            let a: Ban = args(params)?;
            unit(
                handle
                    .platform(|reply| PlatformCommand::Ban {
                        client: a.client,
                        seconds: a.seconds,
                        reason: a.reason,
                        reply,
                    })
                    .await,
            )
        }
        "client.addServerGroup" | "client.removeServerGroup" => {
            ctx.require("moderation")?;
            let Group { client, group } = args(params)?;
            let add = method == "client.addServerGroup";
            unit(
                handle
                    .platform(|reply| {
                        if add {
                            PlatformCommand::AddServerGroup { client, group, reply }
                        } else {
                            PlatformCommand::RemoveServerGroup { client, group, reply }
                        }
                    })
                    .await,
            )
        }
        "client.setChannelGroup" => {
            ctx.require("moderation")?;
            let a: ChannelGroup = args(params)?;
            unit(
                handle
                    .platform(|reply| PlatformCommand::SetChannelGroup {
                        client: a.client,
                        channel: a.channel,
                        group: a.group,
                        reply,
                    })
                    .await,
            )
        }
        "bot.move" => {
            let SelfMove { channel, password } = args(params)?;
            unit(
                handle
                    .platform(|reply| PlatformCommand::MoveSelf {
                        channel,
                        password,
                        reply,
                    })
                    .await,
            )
        }
        "bot.setNick" => {
            let Text { text: name } = args(params)?;
            if name.trim().is_empty() || name.chars().count() > 30 {
                return Err("nicknames are 1-30 characters".into());
            }
            unit(
                handle
                    .platform(|reply| PlatformCommand::SetNickname { name, reply })
                    .await,
            )
        }
        "bot.setDescription" => {
            let Text { text: t } = args(params)?;
            let text = text(t)?;
            unit(
                handle
                    .platform(|reply| PlatformCommand::SetDescription { text, reply })
                    .await,
            )
        }
        "bot.setAway" => {
            let Away { message } = args(params)?;
            unit(
                handle
                    .platform(|reply| PlatformCommand::SetAway { message, reply })
                    .await,
            )
        }
        "channel.create" => {
            ctx.require("channels")?;
            let params: ChannelParams = args(params)?;
            ok(handle
                .platform(|reply| PlatformCommand::CreateChannel { params, reply })
                .await
                .map_err(err)?)
        }
        "channel.edit" => {
            ctx.require("channels")?;
            let ChannelEdit { channel, params } = args(params)?;
            unit(
                handle
                    .platform(|reply| PlatformCommand::EditChannel {
                        channel,
                        params,
                        reply,
                    })
                    .await,
            )
        }
        "channel.move" => {
            ctx.require("channels")?;
            let a: ChannelMove = args(params)?;
            unit(
                handle
                    .platform(|reply| PlatformCommand::MoveChannel {
                        channel: a.channel,
                        parent: a.parent,
                        order: a.order,
                        reply,
                    })
                    .await,
            )
        }
        "channel.delete" => {
            ctx.require("channels")?;
            let ChannelDelete { channel, force } = args(params)?;
            unit(
                handle
                    .platform(|reply| PlatformCommand::DeleteChannel {
                        channel,
                        force,
                        reply,
                    })
                    .await,
            )
        }
        "channel.subscribe" => {
            let Subscribe { channel, subscribe } = args(params)?;
            unit(
                handle
                    .platform(|reply| PlatformCommand::Subscribe {
                        channel,
                        subscribe,
                        reply,
                    })
                    .await,
            )
        }
        "channel.chat" => {
            // Channel messages go to the bot's current channel (TeamSpeak has no other way).
            let Channel { channel } = args::<Channel>(params.clone())?;
            let Text { text: t } = args(params)?;
            let text = text(t)?;
            let snapshot = handle.snapshot().await.map_err(err)?;
            let current = snapshot
                .own_client
                .and_then(|own| snapshot.clients.into_iter().find(|c| c.id == own))
                .and_then(|c| c.channel);
            if current.as_deref() != Some(channel.as_str()) {
                return Err("the bot can only write to the channel it is in".into());
            }
            unit(handle.send_message(MessageTarget::Channel, None, text).await)
        }
        _ => Err(format!("unknown method '{method}'")),
    }
}

// ---- playback -------------------------------------------------------------------------------

async fn media(ctx: &Ctx<'_>, method: &str, params: Value) -> Result<Value> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Play {
        #[serde(default)]
        input: Option<String>,
        #[serde(default)]
        tracks: Option<Vec<Track>>,
        #[serde(default = "append")]
        mode: PlayMode,
    }
    fn append() -> PlayMode {
        PlayMode::Append
    }
    #[derive(Deserialize)]
    struct Input {
        input: String,
    }
    #[derive(Deserialize)]
    struct Search {
        query: String,
        #[serde(default = "ten")]
        limit: usize,
    }
    fn ten() -> usize {
        10
    }
    #[derive(Deserialize)]
    struct Item {
        item: u64,
    }
    #[derive(Deserialize)]
    struct Index {
        index: usize,
    }
    #[derive(Deserialize)]
    struct MoveItem {
        item: u64,
        to: usize,
    }
    #[derive(Deserialize)]
    struct Number {
        value: f64,
    }
    #[derive(Deserialize)]
    struct Flag {
        value: bool,
    }
    #[derive(Deserialize)]
    struct Repeat {
        value: james_runtime::RepeatMode,
    }

    let handle = ctx.instance()?;
    let unit = |r: anyhow::Result<()>| r.map(|()| Value::Null).map_err(err);
    match method {
        "media.play" => {
            let p: Play = args(params)?;
            let items = match (p.input, p.tracks) {
                (Some(input), None) => handle.play(&input, p.mode, Some(ctx.requester())).await,
                (None, Some(tracks)) => {
                    for track in &tracks {
                        track.check_client_limits().map_err(err)?;
                    }
                    let known = handle.services().sources.read().ids();
                    if let Some(t) = tracks.iter().find(|t| !known.contains(&t.source)) {
                        return Err(format!("unknown source '{}'", t.source));
                    }
                    let tracks = tracks.into_iter().map(Track::sanitized).collect();
                    handle.add(tracks, p.mode, Some(ctx.requester())).await
                }
                _ => return Err("pass either input or tracks".into()),
            };
            ok(items.map_err(err)?)
        }
        "media.resolve" => {
            let Input { input } = args(params)?;
            ok(handle.resolve(&input).await.map_err(err)?)
        }
        "media.search" => {
            let Search { query, limit } = args(params)?;
            ok(handle.search(&query, limit.min(50)).await.map_err(err)?)
        }
        "media.skip" => unit(handle.skip()),
        "media.previous" => unit(handle.previous()),
        "media.stop" => unit(handle.stop()),
        "media.pause" => {
            let Flag { value } = args(params)?;
            unit(handle.pause(value))
        }
        "media.seek" => {
            let Number { value } = args(params)?;
            unit(handle.seek(Duration::from_millis(value.max(0.0) as u64)))
        }
        "media.volume" => {
            let Number { value } = args(params)?;
            unit(handle.set_volume(value.clamp(0.0, 100.0) as u8))
        }
        "media.repeat" => {
            let Repeat { value } = args(params)?;
            unit(handle.set_repeat(value))
        }
        "media.shuffle" => {
            let Flag { value } = args(params)?;
            unit(handle.set_shuffle(value))
        }
        "media.shuffleQueue" => unit(handle.shuffle_queue()),
        "media.clear" => unit(handle.clear_queue()),
        "media.remove" => {
            let Item { item } = args(params)?;
            ok(handle.remove(item).await.map_err(err)?)
        }
        "media.removeAt" => {
            let Index { index } = args(params)?;
            ok(handle.remove_at(index).await.map_err(err)?)
        }
        "media.move" => {
            let MoveItem { item, to } = args(params)?;
            unit(handle.move_item(item, to).await)
        }
        "media.jump" => {
            let Item { item } = args(params)?;
            unit(handle.jump(item).await)
        }
        _ => Err(format!("unknown method '{method}'")),
    }
}

// ---- store ----------------------------------------------------------------------------------

async fn store(ctx: &Ctx<'_>, method: &str, params: Value) -> Result<Value> {
    #[derive(Deserialize)]
    struct Scope {
        scope: String,
    }
    #[derive(Deserialize)]
    struct Set {
        scope: String,
        key: String,
        value: Value,
    }
    #[derive(Deserialize)]
    struct Key {
        scope: String,
        key: String,
    }
    let db = &ctx.manager.db;
    match method {
        "store.load" => {
            let Scope { scope } = args(params)?;
            let (plugin, scope) = ctx.scope(&scope)?;
            let entries = db.store_all(&plugin, &scope).await.map_err(err)?;
            ok(entries.into_iter().collect::<serde_json::Map<String, Value>>())
        }
        "store.set" => {
            let Set { scope, key, value } = args(params)?;
            let (plugin, scope) = ctx.scope(&scope)?;
            if key.is_empty() || key.len() > MAX_KEY_LEN {
                return Err(format!("keys are 1-{MAX_KEY_LEN} bytes"));
            }
            let size = value.to_string().len();
            if size > MAX_STORE_VALUE {
                return Err(format!("values are at most {MAX_STORE_VALUE} bytes"));
            }
            // Quota check and write must not interleave with other writes.
            let _guard = ctx.manager.store_lock.lock().await;
            // The global store is shared by every plugin (and mirrored in each): kept small.
            let global = plugin.is_empty();
            let (max_keys, max_bytes) = if global {
                (MAX_GLOBAL_KEYS, MAX_GLOBAL_BYTES)
            } else {
                (MAX_STORE_KEYS, MAX_STORE_BYTES)
            };
            let (count, bytes) = db.store_usage(&plugin, &scope).await.map_err(err)?;
            if count >= max_keys || bytes + size as i64 > max_bytes {
                // Overwriting an existing key is fine even at the limit.
                if db.store_get(&plugin, &scope, &key).await.map_err(err)?.is_none() || bytes > max_bytes {
                    return Err("store quota exceeded".into());
                }
            }
            db.store_set(&plugin, &scope, &key, &value).await.map_err(err)?;
            if global {
                ctx.manager.global_store_changed(&ctx.plugin, &key, &value, false);
            }
            ok(())
        }
        "store.unset" => {
            let Key { scope, key } = args(params)?;
            let (plugin, scope) = ctx.scope(&scope)?;
            let _guard = ctx.manager.store_lock.lock().await;
            db.store_unset(&plugin, &scope, &key).await.map_err(err)?;
            if plugin.is_empty() {
                ctx.manager
                    .global_store_changed(&ctx.plugin, &key, &Value::Null, true);
            }
            ok(())
        }
        _ => Err(format!("unknown method '{method}'")),
    }
}

// ---- playlists and library ------------------------------------------------------------------

async fn library(ctx: &Ctx<'_>, method: &str, params: Value) -> Result<Value> {
    #[derive(Deserialize)]
    struct Id {
        id: i64,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct PlayPlaylist {
        id: i64,
        #[serde(default)]
        start: usize,
        #[serde(default)]
        mode: Option<PlayMode>,
    }
    #[derive(Deserialize)]
    struct Name {
        name: String,
    }
    #[derive(Deserialize)]
    struct Rename {
        id: i64,
        name: String,
    }
    #[derive(Deserialize)]
    struct SetTracks {
        id: i64,
        tracks: Vec<Track>,
    }
    #[derive(Deserialize)]
    struct Search {
        #[serde(default)]
        query: String,
        #[serde(default = "twenty")]
        limit: i64,
        #[serde(default)]
        offset: i64,
    }
    fn twenty() -> i64 {
        20
    }
    let db = &ctx.manager.db;
    match method {
        "playlists.list" => ok(db.playlists().await.map_err(err)?),
        "playlists.get" => {
            let Id { id } = args(params)?;
            let playlist = db.playlist(id).await.map_err(err)?;
            let tracks: Vec<Track> = db.playlist_tracks(id).await.map_err(err)?;
            let mut value = serde_json::to_value(playlist).map_err(err)?;
            value["tracks"] = serde_json::to_value(tracks).map_err(err)?;
            Ok(value)
        }
        "playlists.play" => {
            let p: PlayPlaylist = args(params)?;
            let handle = ctx.instance()?;
            let tracks: Vec<Track> = db.playlist_tracks(p.id).await.map_err(err)?;
            let tracks: Vec<Track> = tracks.into_iter().skip(p.start).collect();
            if tracks.is_empty() {
                return Err("the playlist has no tracks at this position".into());
            }
            ok(handle
                .add(tracks, p.mode.unwrap_or(PlayMode::Now), Some(ctx.requester()))
                .await
                .map_err(err)?)
        }
        "playlists.create" => {
            ctx.require("playlists")?;
            let Name { name } = args(params)?;
            ok(db.create_playlist(&name, None).await.map_err(err)?)
        }
        "playlists.rename" => {
            ctx.require("playlists")?;
            let Rename { id, name } = args(params)?;
            db.rename_playlist(id, &name).await.map_err(err)?;
            ok(())
        }
        "playlists.delete" => {
            ctx.require("playlists")?;
            let Id { id } = args(params)?;
            db.delete_playlist(id).await.map_err(err)?;
            ok(())
        }
        "playlists.setTracks" => {
            ctx.require("playlists")?;
            let SetTracks { id, tracks } = args(params)?;
            if tracks.len() > 5000 {
                return Err("a playlist holds at most 5000 tracks".into());
            }
            for track in &tracks {
                track.check_client_limits().map_err(err)?;
            }
            let tracks: Vec<Track> = tracks.into_iter().map(Track::sanitized).collect();
            db.set_playlist_tracks(id, &tracks).await.map_err(err)?;
            ok(())
        }
        "library.search" => {
            let Search { query, limit, offset } = args(params)?;
            ok(db
                .search_library(&query, limit.clamp(1, 200), offset.max(0))
                .await
                .map_err(err)?)
        }
        "library.get" => {
            let Id { id } = args(params)?;
            ok(db.library_track(id).await.map_err(err)?)
        }
        _ => Err(format!("unknown method '{method}'")),
    }
}

// ---- users ----------------------------------------------------------------------------------

async fn users(ctx: &Ctx<'_>, method: &str, params: Value) -> Result<Value> {
    #[derive(Deserialize)]
    struct Id {
        id: i64,
    }
    #[derive(Deserialize)]
    struct Name {
        username: String,
    }
    #[derive(Deserialize)]
    struct Identity {
        platform: String,
        uid: String,
    }
    #[derive(Deserialize)]
    struct Create {
        username: String,
        #[serde(default)]
        password: Option<String>,
    }
    #[derive(Deserialize)]
    struct Password {
        id: i64,
        #[serde(default)]
        password: Option<String>,
    }
    #[derive(Deserialize)]
    struct Roles {
        id: i64,
        roles: Vec<String>,
    }
    #[derive(Deserialize)]
    struct Link {
        id: i64,
        platform: String,
        uid: String,
        #[serde(default)]
        name: String,
    }

    ctx.require("users")?;
    let db = &ctx.manager.db;
    // Plugins never touch privileged accounts or hand out privileged rights (judged by the
    // effective grants, whatever the roles are called).
    let not_privileged = |id: i64| async move {
        let user = db.user(id).await.map_err(err)?;
        let grants = db.user_grants(id).await.map_err(err)?;
        if user.is_admin || privileged(&grants) {
            Err("accounts with administrative permissions cannot be changed by plugins".to_string())
        } else {
            Ok(())
        }
    };
    let with_identities = |user: james_db::users::User| async move {
        let identities = db.identities(user.id).await.map_err(err)?;
        let mut value = serde_json::to_value(&user).map_err(err)?;
        value["identities"] = serde_json::to_value(identities).map_err(err)?;
        Ok::<_, String>(value)
    };
    match method {
        "users.list" => ok(db.users().await.map_err(err)?),
        "users.get" => {
            let Id { id } = args(params)?;
            with_identities(db.user(id).await.map_err(err)?).await
        }
        "users.byName" => {
            let Name { username } = args(params)?;
            with_identities(db.user_by_name(&username).await.map_err(err)?).await
        }
        "users.byIdentity" => {
            let Identity { platform, uid } = args(params)?;
            match db.user_by_identity(&platform, &uid).await.map_err(err)? {
                Some(user) => with_identities(user).await,
                None => Ok(Value::Null),
            }
        }
        "users.create" => {
            let Create { username, password } = args(params)?;
            ok(db
                .create_user(&username, password.as_deref(), false)
                .await
                .map_err(err)?)
        }
        "users.setPassword" => {
            let Password { id, password } = args(params)?;
            not_privileged(id).await?;
            db.set_password(id, password.as_deref()).await.map_err(err)?;
            db.revoke_user_tokens(id, None).await.map_err(err)?;
            ok(())
        }
        "users.setRoles" => {
            let Roles { id, roles } = args(params)?;
            not_privileged(id).await?;
            for role in &roles {
                let role = db.role(role).await.map_err(err)?;
                if privileged(&role.grants) {
                    return Err(format!("plugins cannot grant the role '{}'", role.name));
                }
            }
            db.set_user_roles(id, &roles).await.map_err(err)?;
            ok(())
        }
        "users.link" => {
            let Link {
                id,
                platform,
                uid,
                name,
            } = args(params)?;
            not_privileged(id).await?;
            // Never move an identity away from another account.
            if let Some(owner) = db.user_by_identity(&platform, &uid).await.map_err(err)? {
                if owner.id != id {
                    return Err("this identity is linked to another account".into());
                }
            }
            db.link_identity(&platform, &uid, id, &name).await.map_err(err)?;
            ok(())
        }
        "users.delete" => {
            let Id { id } = args(params)?;
            not_privileged(id).await?;
            db.delete_user(id).await.map_err(err)?;
            ok(())
        }
        _ => Err(format!("unknown method '{method}'")),
    }
}

/// Grants that make an account administrative (managing users, plugins or instances, or
/// reading private chat or instance logs).
fn privileged(grants: &[james_db::permissions::Grant]) -> bool {
    const ADMINISTRATIVE: [&str; 5] = [
        "users.manage",
        "plugins.manage",
        "instance.manage",
        "instance.logs",
        "server.chat.read",
    ];
    grants.iter().any(|g| {
        ADMINISTRATIVE
            .iter()
            .any(|p| james_db::permissions::permission_matches(&g.permission, p))
    })
}

// ---- commands and engine --------------------------------------------------------------------

fn commands(ctx: &Ctx<'_>, method: &str, params: Value) -> Result<Value> {
    #[derive(Deserialize)]
    struct Name {
        name: String,
    }
    match method {
        "commands.register" => {
            let mut command: PluginCommand = args(params)?;
            command.validate(&ctx.plugin)?;
            ctx.manager.register_command(&ctx.plugin, command)?;
            ok(())
        }
        "commands.unregister" => {
            let Name { name } = args(params)?;
            ctx.manager.unregister_command(&ctx.plugin, &name);
            ok(())
        }
        "commands.prefix" => {
            let handle = ctx.instance()?;
            ok(ctx
                .manager
                .instances
                .config(handle.id())
                .map(|c| c.command_prefix)
                .unwrap_or_default())
        }
        _ => Err(format!("unknown method '{method}'")),
    }
}

async fn tts(ctx: &Ctx<'_>, method: &str, params: Value) -> Result<Value> {
    #[derive(Deserialize)]
    struct Say {
        text: String,
        #[serde(default)]
        voice: Option<String>,
        #[serde(default)]
        language: Option<String>,
    }
    match method {
        "tts.voices" => ok(ctx.manager.instances.services().tts.read().voices()),
        "tts.say" => {
            let Say {
                text,
                voice,
                language,
            } = args(params)?;
            let handle = ctx.instance()?;
            ok(handle
                .say(&text, voice.as_deref(), language.as_deref())
                .await
                .map_err(err)?)
        }
        "tts.stop" => {
            ctx.instance()?.stop_speech().map_err(err)?;
            ok(())
        }
        _ => Err(format!("unknown method '{method}'")),
    }
}

fn web(ctx: &Ctx<'_>, method: &str, params: Value) -> Result<Value> {
    #[derive(Deserialize)]
    struct Register {
        name: String,
        #[serde(default)]
        public: bool,
        #[serde(default)]
        permission: Option<String>,
    }
    match method {
        "web.register" => {
            ctx.require("web")?;
            let r: Register = args(params)?;
            let valid = |n: &str| {
                !n.is_empty()
                    && n.len() <= 64
                    && n.chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            };
            if !valid(&r.name) {
                return Err("endpoint names are 1-64 letters, digits, '-' or '_'".into());
            }
            let permission = match r.permission {
                Some(p) if valid(&p) => Some(format!("plugin.{}.{p}", ctx.plugin)),
                Some(_) => return Err("invalid permission name".into()),
                None => None,
            };
            ctx.manager.register_web(
                &ctx.plugin,
                r.name,
                WebHandler {
                    public: r.public,
                    permission,
                },
            )?;
            ok(())
        }
        _ => Err(format!("unknown method '{method}'")),
    }
}

async fn engine(ctx: &Ctx<'_>, method: &str, params: Value) -> Result<Value> {
    #[derive(Deserialize)]
    struct Notice {
        message: String,
    }
    #[derive(Deserialize)]
    struct Config {
        config: Value,
    }
    match method {
        "engine.notify" => {
            let Notice { message } = args(params)?;
            let instance = ctx.active_instance()?.to_owned();
            if !ctx.manager.allow_notice(&ctx.plugin) {
                return Err("too many notices, at most 6 per minute".into());
            }
            let _ = ctx
                .manager
                .instances
                .services()
                .events
                .send(james_runtime::EventEnvelope {
                    instance,
                    event: james_runtime::InstanceEvent::Notice {
                        source: ctx.plugin.clone(),
                        message: message.chars().take(500).collect(),
                    },
                    sensitive: false,
                });
            ok(())
        }
        "engine.saveConfig" => {
            let Config { config } = args(params)?;
            let instance = ctx.active_instance()?;
            if !config.is_object() || config.to_string().len() > 64 * 1024 {
                return Err("the configuration must be an object of at most 64 KiB".into());
            }
            ctx.manager
                .db
                .set_plugin_instance_config(&ctx.plugin, instance, &config)
                .await
                .map_err(err)?;
            ok(())
        }
        "engine.version" => ok(env!("CARGO_PKG_VERSION")),
        _ => Err(format!("unknown method '{method}'")),
    }
}

/// `ws.send` event from a plugin: `{instance, connection?, data, close?}` (no connection =
/// all connections of the plugin on the instance).
pub(crate) fn ws_send(manager: &PluginManager, plugin: &str, data: &Value) {
    let (Some(instance), Some(text)) = (data["instance"].as_str(), data["data"].as_str()) else {
        return;
    };
    if text.len() > 64 * 1024 {
        tracing::debug!(%plugin, "WebSocket message over 64 KiB dropped");
        return;
    }
    let connection = data["connection"].as_str().and_then(|c| c.parse::<u64>().ok());
    let close = data["close"].as_bool().unwrap_or(false);
    let mut ws = manager.ws.lock();
    let targets: Vec<u64> = ws
        .iter()
        .filter(|(id, c)| {
            c.plugin == plugin && c.instance == instance && connection.is_none_or(|t| t == **id)
        })
        .map(|(id, _)| *id)
        .collect();
    for id in targets {
        if close {
            ws.remove(&id);
        } else if let Some(c) = ws.get(&id) {
            // A slow client loses messages instead of blocking the plugin.
            let _ = c.sender.try_send(text.to_owned());
        }
    }
}
