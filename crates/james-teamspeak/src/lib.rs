//! TeamSpeak 3 / TeamSpeak 6 adapter based on tsclientlib (native client protocol).
//!
//! TeamSpeak 6 servers accept the TS3 client protocol from clients reporting
//! version 3.6.0 or newer, which is what this adapter announces by default.

mod channel;
mod convert;

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{anyhow, bail, Context};
use async_trait::async_trait;
use base64::prelude::*;
use futures::prelude::*;
use james_core::audio::OpusFrame;
use james_core::platform::{
    Capabilities, MessageTarget, PlatformAdapter, PlatformCommand, PlatformContext, PlatformEvent,
    PlatformFactory, Reply,
};
use serde::{Deserialize, Serialize};
use tsclientlib::messages::c2s;
use tsclientlib::prelude::*;
use tsclientlib::{
    ChannelGroupId, ChannelId, ClientId, Connection, DisconnectOptions, Identity, MessageHandle, Reason,
    ServerGroupId, StreamItem, Version,
};
use tsproto_packets::packets::{AudioData, CodecType, OutAudio, OutCommand};

pub use convert::snapshot;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TeamSpeakConfig {
    /// Server address, e.g. `ts.example.com`, `ts.example.com:9987` or an IP.
    pub address: String,
    pub nickname: String,
    pub server_password: Option<String>,
    /// Default channel as path (`Lobby/Music`) or numeric id.
    pub channel: Option<String>,
    pub channel_password: Option<String>,
    /// Client version reported to the server. Defaults to 3.6.0 (required by TS6).
    pub version: Option<ClientVersion>,
}

impl Default for TeamSpeakConfig {
    fn default() -> Self {
        Self {
            address: "localhost".into(),
            nickname: "James".into(),
            server_password: None,
            channel: None,
            channel_password: None,
            version: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientVersion {
    pub platform: String,
    pub version: String,
    /// Base64-encoded version signature.
    pub signature: String,
}

impl Default for ClientVersion {
    fn default() -> Self {
        Self {
            platform: "Linux".into(),
            version: "3.6.0 [Build: 1666781655]".into(),
            signature:
                "PdBDIrgplpy9//chwhIRbrLyiQarUll/uEh0xdCcvrLHjY2LG+B/N/RpDJfSZkrXoojRHyjcTPBV47/HXc65Cg=="
                    .into(),
        }
    }
}

/// Persisted credentials (see [`PlatformEvent::CredentialsUpdated`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub identity: Identity,
}

pub struct TeamSpeakFactory;

impl PlatformFactory for TeamSpeakFactory {
    fn id(&self) -> &str {
        "teamspeak"
    }

    fn create(
        &self,
        settings: &serde_json::Value,
        credentials: Option<serde_json::Value>,
    ) -> anyhow::Result<Box<dyn PlatformAdapter>> {
        Ok(Box::new(TeamSpeakAdapter::new(
            parse_settings(settings)?,
            credentials,
        )?))
    }

    fn validate(&self, settings: &serde_json::Value) -> anyhow::Result<()> {
        let config = parse_settings(settings)?;
        if config.address.trim().is_empty() {
            bail!("address must not be empty");
        }
        // TeamSpeak's own limits; the name is sent as entered.
        if config.nickname.trim() != config.nickname || !(3..=30).contains(&config.nickname.chars().count()) {
            bail!("nickname must be 3-30 characters without spaces at the start or end");
        }
        if let Some(version) = &config.version {
            BASE64_STANDARD
                .decode(&version.signature)
                .context("invalid version signature")?;
        }
        Ok(())
    }

    fn capabilities(&self) -> Capabilities {
        capabilities()
    }

    fn requires_reconnect(&self, old: &serde_json::Value, new: &serde_json::Value) -> bool {
        // Everything but the nickname and the default channel is part of the connection (those
        // two change on the running one).
        match (parse_settings(old), parse_settings(new)) {
            (Ok(old), Ok(new)) => {
                let mut new = new;
                new.nickname = old.nickname.clone();
                new.channel = old.channel.clone();
                new.channel_password = old.channel_password.clone();
                serde_json::to_value(&old).ok() != serde_json::to_value(&new).ok()
            }
            _ => true,
        }
    }

    fn import_credentials(&self, input: &str) -> anyhow::Result<serde_json::Value> {
        Ok(serde_json::to_value(Credentials {
            identity: import_identity(input)?,
        })?)
    }

    fn export_credentials(&self, data: &serde_json::Value) -> anyhow::Result<String> {
        // Without the parse error: it may quote the stored key.
        let identity = serde_json::from_value::<Credentials>(data.clone())
            .map_err(|_| anyhow::anyhow!("the stored identity is unreadable"))?
            .identity;
        Ok(export_identity(&identity))
    }

    fn describe_credentials(&self, data: &serde_json::Value) -> Option<serde_json::Value> {
        let identity = serde_json::from_value::<Credentials>(data.clone()).ok()?.identity;
        Some(serde_json::json!({
            "uid": identity.key().to_pub().get_uid(),
            "uidTs6": ts6_uid(&identity),
            "securityLevel": identity.level(),
        }))
    }
}

/// The unique id TeamSpeak 6 shows for an identity: SHA-256 instead of SHA-1 of the public key.
fn ts6_uid(identity: &Identity) -> String {
    use sha2::Digest as _;
    let omega = identity.key().to_pub().to_ts();
    BASE64_STANDARD.encode(sha2::Sha256::digest(omega.as_bytes()))
}

/// Security level an imported identity is raised to if it is lower (servers commonly require
/// 8; raising it keeps the unique id).
const MIN_SECURITY_LEVEL: u8 = 8;

/// Reads an identity as the TeamSpeak client exports it (`identity="123V…"`, also used by
/// SinusBot) or as a base64 private key.
pub fn import_identity(input: &str) -> anyhow::Result<Identity> {
    // A whole exported identity file (`[Identity]`, `id=…`, `identity="…"`, `nickname=…`) or
    // just the value.
    let text = input
        .lines()
        .find_map(|line| line.trim().strip_prefix("identity="))
        .unwrap_or(input)
        .trim()
        .trim_matches('"');
    let mut identity = Identity::new_from_str(text)
        .map_err(|_| anyhow::anyhow!("not a TeamSpeak identity (expected e.g. 123V… or a base64 key)"))?;
    if identity.level() < MIN_SECURITY_LEVEL {
        identity.upgrade_level(MIN_SECURITY_LEVEL);
    }
    Ok(identity)
}

/// An identity as the TeamSpeak client exports it (`<counter>V<obfuscated key>`), readable by
/// [`import_identity`], the TeamSpeak client and SinusBot.
pub fn export_identity(identity: &Identity) -> String {
    format!("{}V{}", identity.counter(), identity.key().to_ts_obfuscated())
}

fn parse_settings(settings: &serde_json::Value) -> anyhow::Result<TeamSpeakConfig> {
    let mut config: TeamSpeakConfig =
        serde_json::from_value(settings.clone()).context("invalid TeamSpeak settings")?;
    // A blank channel means the server's default channel (as when connecting).
    let blank = |v: &Option<String>| v.as_deref().is_some_and(|s| s.trim().is_empty());
    if blank(&config.channel) {
        config.channel = None;
    }
    if blank(&config.channel_password) {
        config.channel_password = None;
    }
    Ok(config)
}

/// Largest Opus frame that fits into a TeamSpeak voice packet.
pub const MAX_FRAME_BYTES: usize = 500 - 13 - 3;

fn capabilities() -> Capabilities {
    Capabilities {
        multi_session: false,
        pokes: true,
        server_groups: true,
        channel_groups: true,
        avatars: true,
        video: false,
        // A voice packet may have at most 500 bytes (13 bytes packet header, 3 bytes voice
        // header): larger frames never arrive. 192 kbit/s averages 480 bytes per 20 ms frame.
        max_bitrate: 192_000,
        max_frame_bytes: Some(MAX_FRAME_BYTES),
    }
}

pub struct TeamSpeakAdapter {
    config: TeamSpeakConfig,
    identity: Identity,
}

impl TeamSpeakAdapter {
    /// `credentials` are the data from the last [`PlatformEvent::CredentialsUpdated`], if any.
    pub fn new(config: TeamSpeakConfig, credentials: Option<serde_json::Value>) -> anyhow::Result<Self> {
        let identity = match credentials {
            Some(data) => {
                serde_json::from_value::<Credentials>(data)
                    .context("invalid credentials")?
                    .identity
            }
            None => Identity::create(),
        };
        Ok(Self { config, identity })
    }

    fn build(&self) -> anyhow::Result<tsclientlib::ConnectOptions> {
        let version = self.config.version.clone().unwrap_or_default();
        let version = Version::Custom {
            platform: version.platform,
            version: version.version,
            signature: BASE64_STANDARD
                .decode(version.signature)
                .context("invalid version signature")?,
        };
        let mut options = Connection::build(self.config.address.clone())
            .name(self.config.nickname.clone())
            .version(version)
            .identity(self.identity.clone());
        if let Some(password) = &self.config.server_password {
            options = options.password(password.clone());
        }
        if let Some(channel) = self
            .config
            .channel
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty())
        {
            options = match channel.parse::<u64>() {
                Ok(id) => options.channel_id(ChannelId(id)),
                Err(_) => options.channel(channel.to_owned()),
            };
        }
        if let Some(password) = &self.config.channel_password {
            options = options.channel_password(password.clone());
        }
        Ok(options)
    }
}

#[async_trait]
impl PlatformAdapter for TeamSpeakAdapter {
    fn capabilities(&self) -> Capabilities {
        capabilities()
    }

    async fn run(&mut self, ctx: &mut PlatformContext) -> anyhow::Result<()> {
        let mut con = self.build()?.connect()?;

        // Wait for the initial server state.
        let first = con
            .events()
            .try_filter(|e| future::ready(matches!(e, StreamItem::BookEvents(_))))
            .next()
            .await;
        match first {
            Some(Ok(_)) => {}
            Some(Err(e)) => return Err(e).context("failed to connect"),
            None => bail!("connection closed during connect"),
        }
        tracing::info!(address = %self.config.address, "connected to TeamSpeak server");
        self.persist_identity(&con, ctx);

        let mut session = Session {
            config: self.config.clone(),
            applied_channel: Arc::default(),
            pending: HashMap::new(),
            created: Vec::new(),
            can_send_audio: true,
            reconnecting: false,
            audio_closed: false,
            reapply_pending: false,
            info_limiter: InfoLimiter::default(),
            subscribing: None,
        };
        session.begin_connected(&mut con, ctx);
        let result = session.run(&mut con, ctx).await;
        if result.is_err() {
            let _ = con.disconnect(DisconnectOptions::new());
        }
        let reason = match &result {
            Ok(reason) => reason.clone(),
            Err(e) => format!("{e:#}"),
        };
        // Drive the connection until the disconnect completes (bounded).
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            con.events().for_each(|_| future::ready(())),
        )
        .await;
        let _ = ctx.events.send(PlatformEvent::Disconnected { reason });
        result.map(|_| ())
    }
}

impl TeamSpeakAdapter {
    fn persist_identity(&mut self, con: &Connection, ctx: &PlatformContext) {
        if let Some(identity) = con.get_options().get_identity() {
            self.identity = identity.clone();
            if let Ok(data) = serde_json::to_value(Credentials {
                identity: identity.clone(),
            }) {
                let _ = ctx.events.send(PlatformEvent::CredentialsUpdated { data });
            }
        }
    }
}

/// A channel creation waiting for its result; the new id is looked up in the book.
struct PendingCreate {
    name: String,
    parent: ChannelId,
    done: tokio::sync::oneshot::Receiver<anyhow::Result<()>>,
    reply: Reply<String>,
}

/// A default channel and its password, as in the settings.
type ChannelChoice = (Option<String>, Option<String>);

struct Session {
    /// The settings wanted (changed by `UpdateSettings`).
    config: TeamSpeakConfig,
    /// Default channel and password the bot is known to be in; `None` = unknown (after
    /// connecting). A move is only recorded once the server accepted it, so a refused one is
    /// tried again on the next save.
    applied_channel: Arc<parking_lot::Mutex<Option<ChannelChoice>>>,
    pending: HashMap<MessageHandle, Reply<()>>,
    created: Vec<PendingCreate>,
    can_send_audio: bool,
    /// tsclientlib is re-establishing the connection; ids in the book change.
    reconnecting: bool,
    /// The audio sender was dropped (engine gone); stop polling it.
    audio_closed: bool,
    /// The settings are applied once the own client is in the book (it arrives after the
    /// first server state).
    reapply_pending: bool,
    info_limiter: InfoLimiter,
    /// Subscribing to all channels after (re)connecting, until the reply or the deadline: the
    /// clients that become visible meanwhile go into the `Connected` snapshot, not into events.
    subscribing: Option<(MessageHandle, tokio::time::Instant)>,
}

/// Longest wait for the subscription to all channels before `Connected` is sent anyway.
const SUBSCRIBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Shortest interval between two info requests for the same client.
const INFO_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);
/// Most clients whose info is requested per second.
const INFO_BURST: usize = 10;

/// Limits client info requests (two commands each) to stay clear of the server's anti-flood.
#[derive(Default)]
struct InfoLimiter {
    /// Last request per client.
    last: HashMap<String, std::time::Instant>,
    /// Requests in the current second.
    window: Option<(std::time::Instant, usize)>,
}

impl InfoLimiter {
    fn allow(&mut self, client: &str, now: std::time::Instant) -> bool {
        if self
            .last
            .get(client)
            .is_some_and(|at| now.duration_since(*at) < INFO_INTERVAL)
        {
            return false;
        }
        let (start, count) = match self.window {
            Some((start, count)) if now.duration_since(start) < std::time::Duration::from_secs(1) => {
                (start, count)
            }
            _ => (now, 0),
        };
        if count >= INFO_BURST {
            return false;
        }
        self.window = Some((start, count + 1));
        self.last.retain(|_, at| now.duration_since(*at) < INFO_INTERVAL);
        self.last.insert(client.to_owned(), now);
        true
    }
}

enum Input {
    Event(Option<Result<StreamItem, tsclientlib::Error>>),
    Command(Option<PlatformCommand>),
    Audio(Option<OpusFrame>),
    SubscribeTimeout,
}

impl Session {
    /// Returns the disconnect reason on a regular disconnect.
    async fn run(&mut self, con: &mut Connection, ctx: &mut PlatformContext) -> anyhow::Result<String> {
        loop {
            let deadline = self.subscribing.map(|(_, deadline)| deadline);
            let input = {
                let mut events = con.events();
                tokio::select! {
                    e = events.next() => Input::Event(e),
                    _ = tokio::time::sleep_until(deadline.unwrap_or_else(tokio::time::Instant::now)),
                        if deadline.is_some() => Input::SubscribeTimeout,
                    c = ctx.commands.recv() => Input::Command(c),
                    a = ctx.audio.recv(), if !self.audio_closed => Input::Audio(a),
                }
            };
            // Replies whose caller gave up (timeout) are dropped instead of accumulating.
            self.pending.retain(|_, reply| !reply.is_closed());
            match input {
                // The server closed the connection (kick, ban, shutdown): reconnect.
                Input::Event(None) => bail!("connection closed by the server"),
                Input::Event(Some(Err(e))) => return Err(e).context("connection error"),
                Input::Event(Some(Ok(item))) => self.handle_item(con, ctx, item),
                Input::Command(None) => {
                    con.disconnect(DisconnectOptions::new())?;
                    return Ok("instance stopped".into());
                }
                Input::Command(Some(PlatformCommand::Disconnect { reason })) => {
                    con.disconnect(DisconnectOptions::new().message(reason.clone()))?;
                    return Ok(reason);
                }
                Input::Command(Some(command)) => self.handle_command(con, command),
                Input::SubscribeTimeout => {
                    tracing::warn!("no answer to the channel subscription");
                    self.finish_connected(con, ctx);
                }
                Input::Audio(None) => self.audio_closed = true,
                Input::Audio(Some(frame)) => {
                    if self.can_send_audio {
                        let packet = OutAudio::new(&AudioData::C2S {
                            id: 0,
                            codec: CodecType::OpusMusic,
                            data: &frame.data,
                        });
                        if let Err(e) = con.send_audio(packet) {
                            tracing::trace!(error = %e, "dropped audio packet");
                        }
                    }
                }
            }
        }
    }

    fn handle_item(&mut self, con: &mut Connection, ctx: &PlatformContext, item: StreamItem) {
        match item {
            StreamItem::BookEvents(events) => {
                let Ok(state) = con.get_state() else { return };
                if self.reconnecting {
                    // After a reconnect all client ids are new: a fresh snapshot follows.
                    self.reconnecting = false;
                    self.info_limiter = InfoLimiter::default();
                    tracing::info!("reconnected");
                    self.begin_connected(con, ctx);
                    return;
                }
                if self.subscribing.is_some() {
                    // Clients and channels are part of the snapshot that follows; chat, pokes and
                    // leaves (a kick or ban is not in it) still go out (after a reconnect already
                    // with the new client ids, which plugins learn with `Connected`).
                    for event in events.iter().filter_map(|e| convert::event(state, e)) {
                        if matches!(
                            event,
                            PlatformEvent::Message { .. }
                                | PlatformEvent::Poke { .. }
                                | PlatformEvent::ClientLeave { .. }
                        ) {
                            let _ = ctx.events.send(event);
                        }
                    }
                    return;
                }
                let converted = events.iter().filter_map(|e| convert::event(state, e)).collect();
                for event in convert::coalesce(converted) {
                    let _ = ctx.events.send(event);
                }
                if self.reapply_pending {
                    self.reapply(con);
                }
            }
            StreamItem::MessageResult(handle, result) => {
                if self.subscribing.is_some_and(|(h, _)| h == handle) {
                    if let Err(e) = result {
                        tracing::warn!(error = %e, "cannot subscribe to the channels");
                    }
                    self.finish_connected(con, ctx);
                    return;
                }
                if let Some(reply) = self.pending.remove(&handle) {
                    let _ = reply.send(result.map_err(|e| anyhow!("{e}")));
                }
                self.finish_creates(con);
            }
            StreamItem::AudioChange(tsclientlib::AudioEvent::CanSendAudio(can)) => {
                tracing::debug!(can, "can send audio changed");
                self.can_send_audio = can;
            }
            StreamItem::DisconnectedTemporarily(reason) => {
                tracing::warn!(?reason, "temporarily disconnected, reconnecting");
                self.reconnecting = true;
                // `Connected` follows the reconnect, not this connection's subscription.
                self.subscribing = None;
                for (_, reply) in self.pending.drain() {
                    let _ = reply.send(Err(anyhow!("connection lost")));
                }
                self.finish_creates(con);
                let _ = ctx.events.send(PlatformEvent::Disconnected {
                    reason: format!("{reason:?}"),
                });
            }
            _ => {}
        }
    }

    fn handle_command(&mut self, con: &mut Connection, command: PlatformCommand) {
        match command {
            PlatformCommand::UpdateSettings { settings, reply } => match parse_settings(&settings) {
                Ok(new) => {
                    self.config = new;
                    if self.reapply_pending {
                        // Applied as soon as the own client is known.
                        let _ = reply.send(Ok(()));
                    } else {
                        self.apply_live(con, reply);
                    }
                }
                Err(e) => {
                    let _ = reply.send(Err(e));
                }
            },
            PlatformCommand::Snapshot { reply } => {
                let _ = reply.send(con.get_state().map(snapshot).map_err(Into::into));
            }
            PlatformCommand::SendMessage {
                target,
                client,
                text,
                reply,
            } => {
                let result = (|| {
                    let state = con.get_state()?;
                    let packet = match target {
                        MessageTarget::Client => {
                            let id = parse_client(client.as_deref())?;
                            state
                                .clients
                                .get(&id)
                                .ok_or_else(|| anyhow!("unknown client"))?
                                .send_textmessage(&text)
                        }
                        MessageTarget::Channel => {
                            state.send_message(tsclientlib::MessageTarget::Channel, &text)
                        }
                        MessageTarget::Server => {
                            state.send_message(tsclientlib::MessageTarget::Server, &text)
                        }
                    };
                    Ok(packet)
                })();
                self.send(con, result, reply);
            }
            PlatformCommand::Poke { client, text, reply } => {
                let result = with_client(con, &client, |c| Ok(c.poke(&text)));
                self.send(con, result, reply);
            }
            PlatformCommand::MoveSelf {
                channel,
                password,
                reply,
            } => {
                let result = (|| {
                    let state = con.get_state()?;
                    let own = state
                        .clients
                        .get(&state.own_client)
                        .ok_or_else(|| anyhow!("own client missing"))?;
                    let part = own.client_move(parse_channel(&channel)?);
                    Ok(match &password {
                        Some(p) => part.set_password(p).to_packet(),
                        None => part.to_packet(),
                    })
                })();
                self.send(con, result, reply);
            }
            PlatformCommand::MoveClient {
                client,
                channel,
                reply,
            } => {
                let result = with_client(con, &client, |c| {
                    Ok(c.client_move(parse_channel(&channel)?).to_packet())
                });
                self.send(con, result, reply);
            }
            PlatformCommand::Kick {
                client,
                from_server,
                reason,
                reply,
            } => {
                let kind = if from_server {
                    Reason::KickServer
                } else {
                    Reason::KickChannel
                };
                let result = with_client(con, &client, |c| {
                    Ok(c.kick(kind).set_reason_message(&reason).to_packet())
                });
                self.send(con, result, reply);
            }
            PlatformCommand::SetNickname { name, reply } => {
                let result = con.get_state().map_err(Into::into).map(|s| {
                    let mut update = s.client_update();
                    update.name = Some(name.as_str().into());
                    update.to_packet()
                });
                self.send(con, result, reply);
            }
            PlatformCommand::SetDescription { text, reply } => {
                let result = (|| {
                    let state = con.get_state()?;
                    let own = state
                        .clients
                        .get(&state.own_client)
                        .ok_or_else(|| anyhow!("own client missing"))?;
                    Ok(own.edit().set_description(&text).to_packet())
                })();
                self.send(con, result, reply);
            }
            PlatformCommand::AddServerGroup { client, group, reply } => {
                let result = with_client(con, &client, |c| Ok(c.add_server_group(parse_group(&group)?)));
                self.send(con, result, reply);
            }
            PlatformCommand::RemoveServerGroup { client, group, reply } => {
                let result = with_client(con, &client, |c| Ok(c.remove_server_group(parse_group(&group)?)));
                self.send(con, result, reply);
            }
            PlatformCommand::Ban {
                client,
                seconds,
                reason,
                reply,
            } => {
                let result = with_client(con, &client, |c| {
                    Ok(c2s::OutBanClientMessage::new(&mut std::iter::once(
                        c2s::OutBanClientPart {
                            client_id: c.id,
                            time: (seconds > 0)
                                .then(|| time::Duration::seconds(seconds.min(i64::MAX as u64) as i64)),
                            ban_reason: (!reason.is_empty()).then(|| reason.as_str().into()),
                        },
                    )))
                });
                self.send(con, result, reply);
            }
            PlatformCommand::SetAway { message, reply } => {
                let result = con.get_state().map_err(Into::into).map(|s| {
                    let mut update = s.client_update();
                    update.is_away = Some(message.is_some());
                    update.away_message = Some(message.as_deref().unwrap_or("").into());
                    update.to_packet()
                });
                self.send(con, result, reply);
            }
            PlatformCommand::SetChannelGroup {
                client,
                channel,
                group,
                reply,
            } => {
                let result = (|| {
                    let state = con.get_state()?;
                    let c = state
                        .clients
                        .get(&parse_client(Some(&client))?)
                        .ok_or_else(|| anyhow!("unknown client {client}"))?;
                    Ok(c2s::OutSetClientChannelGroupMessage::new(&mut std::iter::once(
                        c2s::OutSetClientChannelGroupPart {
                            channel_group: ChannelGroupId(
                                group
                                    .parse()
                                    .with_context(|| format!("invalid channel group id {group}"))?,
                            ),
                            channel_id: parse_channel(&channel)?,
                            client_db_id: c.database_id,
                        },
                    )))
                })();
                self.send(con, result, reply);
            }
            PlatformCommand::CreateChannel { params, reply } => {
                let name = params.name.clone().unwrap_or_default();
                let parent = match params.parent.as_deref() {
                    Some(p) => match parse_channel(p) {
                        Ok(id) => id,
                        Err(e) => {
                            let _ = reply.send(Err(e));
                            return;
                        }
                    },
                    None => ChannelId(0),
                };
                // Invalid parameters are answered right away (no server round trip).
                let packet = match channel::create(&params) {
                    Ok(packet) => packet,
                    Err(e) => {
                        let _ = reply.send(Err(e));
                        return;
                    }
                };
                let (tx, rx) = tokio::sync::oneshot::channel();
                self.send(con, Ok(packet), tx);
                self.created.push(PendingCreate {
                    name,
                    parent,
                    done: rx,
                    reply,
                });
            }
            PlatformCommand::EditChannel {
                channel,
                params,
                reply,
            } => {
                let result = parse_channel(&channel).and_then(|id| channel::edit(id, &params));
                self.send(con, result, reply);
            }
            PlatformCommand::MoveChannel {
                channel,
                parent,
                order,
                reply,
            } => {
                let result = (|| {
                    Ok(c2s::OutChannelMoveMessage::new(&mut std::iter::once(
                        c2s::OutChannelMovePart {
                            channel_id: parse_channel(&channel)?,
                            parent_id: match parent.as_deref() {
                                Some(p) => parse_channel(p)?,
                                None => ChannelId(0),
                            },
                            order: order.as_deref().map(parse_channel).transpose()?,
                        },
                    )))
                })();
                self.send(con, result, reply);
            }
            PlatformCommand::DeleteChannel {
                channel,
                force,
                reply,
            } => {
                let result = parse_channel(&channel).map(|id| {
                    c2s::OutChannelDeleteMessage::new(&mut std::iter::once(c2s::OutChannelDeletePart {
                        channel_id: id,
                        force,
                    }))
                });
                self.send(con, result, reply);
            }
            PlatformCommand::Subscribe {
                channel,
                subscribe,
                reply,
            } => {
                let result = parse_channel(&channel).map(|id| {
                    if subscribe {
                        c2s::OutChannelSubscribeMessage::new(&mut std::iter::once(
                            c2s::OutChannelSubscribePart { channel_id: id },
                        ))
                    } else {
                        c2s::OutChannelUnsubscribeMessage::new(&mut std::iter::once(
                            c2s::OutChannelUnsubscribePart { channel_id: id },
                        ))
                    }
                });
                self.send(con, result, reply);
            }
            PlatformCommand::RequestClientInfo { client, reply } => {
                // Several plugins polling the same clients must not get the bot flood-banned:
                // a client's data is requested at most every INFO_INTERVAL, and at most
                // INFO_BURST clients per second; other requests fail with "throttled". Only
                // clients that exist count.
                if let Err(e) = with_client(con, &client, |c| Ok(c.connection_info().to_packet())) {
                    let _ = reply.send(Err(e));
                    return;
                }
                // Keyed by the parsed id ("5" and "05" are the same client).
                let key = parse_client(Some(&client))
                    .map(|id| id.0.to_string())
                    .unwrap_or_else(|_| client.clone());
                if !self.info_limiter.allow(&key, std::time::Instant::now()) {
                    let _ = reply.send(Err(anyhow!(
                        "throttled: the client's info was requested recently"
                    )));
                    return;
                }
                // Variables (version, platform, connection count) and connection info both
                // update the book, which emits ClientUpdate events.
                let variables = with_client(con, &client, |c| Ok(c.variables().to_packet()));
                if let Ok(packet) = variables {
                    let _ = packet.send(con);
                }
                let result = with_client(con, &client, |c| Ok(c.connection_info().to_packet()));
                self.send(con, result, reply);
            }
            PlatformCommand::Extension { name, reply, .. } => {
                let _ = reply.send(Err(anyhow!("unsupported TeamSpeak extension '{name}'")));
            }
            // Handled by the session loop before dispatching.
            PlatformCommand::Disconnect { .. } => {}
        }
    }

    /// Answers finished channel creations. The server announces the channel before the
    /// command result, so it is already in the book.
    fn finish_creates(&mut self, con: &Connection) {
        let Ok(state) = con.get_state() else { return };
        self.created.retain_mut(|pending| {
            let result = match pending.done.try_recv() {
                Ok(result) => result,
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => return !pending.reply.is_closed(),
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => Err(anyhow!("connection lost")),
            };
            let reply = std::mem::replace(&mut pending.reply, tokio::sync::oneshot::channel().0);
            let _ = reply.send(result.and_then(|()| {
                state
                    .channels
                    .values()
                    .filter(|c| c.name == pending.name && c.parent == pending.parent)
                    .map(|c| c.id.0)
                    .max()
                    .map(|id| id.to_string())
                    .ok_or_else(|| anyhow!("channel was created but is not visible"))
            }));
            false
        });
    }

    /// After (re)connecting: subscribes to every channel, as the TeamSpeak client does (otherwise
    /// the bot only sees the clients in its own channel, and plugins miss everyone else).
    /// `Connected` follows once the server answered, with all clients in the snapshot.
    fn begin_connected(&mut self, con: &mut Connection, ctx: &PlatformContext) {
        match c2s::OutChannelSubscribeAllMessage::new().send_with_result(con) {
            Ok(handle) => {
                self.subscribing = Some((handle, tokio::time::Instant::now() + SUBSCRIBE_TIMEOUT));
            }
            Err(e) => {
                tracing::warn!(error = %e, "cannot subscribe to the channels");
                self.finish_connected(con, ctx);
            }
        }
    }

    /// Publishes the connection with the full client list and applies the settings (the library
    /// (re)connected with the original name and channel, and the join may have been refused).
    fn finish_connected(&mut self, con: &mut Connection, ctx: &PlatformContext) {
        self.subscribing = None;
        match con.get_state() {
            Ok(state) => {
                let _ = ctx.events.send(PlatformEvent::Connected {
                    snapshot: snapshot(state),
                });
            }
            Err(e) => tracing::warn!(error = %e, "no server state after connecting"),
        }
        self.reapply(con);
    }

    /// Applies the settings after (re)connecting: where the bot ended up is not known yet.
    fn reapply(&mut self, con: &mut Connection) {
        *self.applied_channel.lock() = None;
        self.reapply_pending = !con
            .get_state()
            .is_ok_and(|state| state.clients.contains_key(&state.own_client));
        if self.reapply_pending {
            return;
        }
        let (reply, result) = tokio::sync::oneshot::channel();
        self.apply_live(con, reply);
        tokio::spawn(tracing::Instrument::in_current_span(async move {
            if let Ok(Err(e)) = result.await {
                tracing::warn!(error = %format!("{e:#}"), "cannot apply the settings after connecting");
            }
        }));
    }

    /// Brings the running connection in line with `self.config`: the nickname, and the
    /// default channel when it changed since the bot last moved there.
    fn apply_live(&mut self, con: &mut Connection, reply: Reply<()>) {
        let state = match con.get_state() {
            Ok(state) => state,
            Err(e) => {
                let _ = reply.send(Err(e.into()));
                return;
            }
        };
        let Some(own) = state.clients.get(&state.own_client) else {
            let _ = reply.send(Err(anyhow!("own client missing")));
            return;
        };
        let new = &self.config;
        let mut packets = Vec::new();
        if own.name != new.nickname {
            let mut update = state.client_update();
            update.name = Some(new.nickname.as_str().into());
            packets.push(update.to_packet());
        }
        let wanted = (new.channel.clone(), new.channel_password.clone());
        let mut channel_error = None;
        let mut moved_to = None;
        if self.applied_channel.lock().as_ref() != Some(&wanted) {
            // No channel set: the server's default channel.
            let target = match &new.channel {
                Some(spec) => find_channel(state, spec),
                None => state
                    .channels
                    .values()
                    .find(|c| c.is_default == Some(true))
                    .map(|c| c.id),
            };
            match target {
                Some(target) if own.channel == target => *self.applied_channel.lock() = Some(wanted),
                Some(target) => {
                    let part = own.client_move(target);
                    packets.push(match &new.channel_password {
                        Some(p) => part.set_password(p).to_packet(),
                        None => part.to_packet(),
                    });
                    moved_to = Some(wanted);
                }
                None => {
                    channel_error = Some(anyhow!(
                        "the channel '{}' does not exist",
                        new.channel.as_deref().unwrap_or_default()
                    ));
                }
            }
        }
        let applied = self.applied_channel.clone();
        self.send_all(con, packets, reply, move |ok| {
            if let (true, Some(wanted)) = (ok, moved_to) {
                *applied.lock() = Some(wanted);
            }
            channel_error.map_or(Ok(()), Err)
        });
    }

    /// Sends several commands; `reply` gets the first error, or else what `finish` returns
    /// (it learns whether all succeeded).
    fn send_all(
        &mut self,
        con: &mut Connection,
        packets: Vec<OutCommand>,
        reply: Reply<()>,
        finish: impl FnOnce(bool) -> anyhow::Result<()> + Send + 'static,
    ) {
        let mut results = Vec::new();
        for packet in packets {
            let (tx, rx) = tokio::sync::oneshot::channel();
            self.send(con, Ok(packet), tx);
            results.push(rx);
        }
        tokio::spawn(tracing::Instrument::in_current_span(async move {
            let mut first_error = None;
            for rx in results {
                if let Err(e) = rx.await.unwrap_or_else(|_| Err(anyhow!("connection closed"))) {
                    first_error.get_or_insert(e);
                }
            }
            let finished = finish(first_error.is_none());
            let _ = reply.send(match first_error {
                Some(e) => Err(e),
                None => finished,
            });
        }));
    }

    fn send(&mut self, con: &mut Connection, packet: anyhow::Result<OutCommand>, reply: Reply<()>) {
        match packet.and_then(|p| p.send_with_result(con).map_err(Into::into)) {
            Ok(handle) => {
                self.pending.insert(handle, reply);
            }
            Err(e) => {
                let _ = reply.send(Err(e));
            }
        }
    }
}

fn with_client(
    con: &Connection,
    client: &str,
    f: impl FnOnce(&tsclientlib::data::Client) -> anyhow::Result<OutCommand>,
) -> anyhow::Result<OutCommand> {
    let state = con.get_state()?;
    let id = parse_client(Some(client))?;
    f(state
        .clients
        .get(&id)
        .ok_or_else(|| anyhow!("unknown client {client}"))?)
}

fn parse_client(id: Option<&str>) -> anyhow::Result<ClientId> {
    let id = id.ok_or_else(|| anyhow!("client id required"))?;
    Ok(ClientId(
        id.parse().with_context(|| format!("invalid client id {id}"))?,
    ))
}

/// A channel given as id or path (`Lobby/Music`), as in the default channel setting.
fn find_channel(state: &tsclientlib::data::Connection, spec: &str) -> Option<ChannelId> {
    find_channel_in(
        state
            .channels
            .values()
            .map(|c| (c.id.0, c.parent.0, c.name.as_str(), c.order.0)),
        spec,
    )
    .map(ChannelId)
}

/// [`find_channel`] over `(id, parent, name, order)`; among equal names the first in order wins.
fn find_channel_in<'a>(channels: impl Iterator<Item = (u64, u64, &'a str, u64)>, spec: &str) -> Option<u64> {
    let channels: Vec<_> = channels.collect();
    let spec = spec.trim();
    if let Ok(id) = spec.parse::<u64>() {
        return channels.iter().any(|c| c.0 == id).then_some(id);
    }
    let mut parent = 0;
    let mut found = None;
    for name in spec.split('/').map(str::trim).filter(|n| !n.is_empty()) {
        let channel = channels
            .iter()
            .filter(|c| c.1 == parent && c.2 == name)
            .min_by_key(|c| (c.3, c.0))?;
        parent = channel.0;
        found = Some(channel.0);
    }
    found
}

fn parse_channel(id: &str) -> anyhow::Result<ChannelId> {
    Ok(ChannelId(
        id.parse().with_context(|| format!("invalid channel id {id}"))?,
    ))
}

fn parse_group(id: &str) -> anyhow::Result<ServerGroupId> {
    Ok(ServerGroupId(
        id.parse()
            .with_context(|| format!("invalid server group id {id}"))?,
    ))
}

#[cfg(test)]
mod identity_import_tests {
    use super::*;

    const KEY: &str = "MG8DAgeAAgEgAiEA6rtKxDn/o/Bo50rNtAE5Ph3h2RKLHQ0gbFkvm2yA79kCIQCrfzAZts/\
                       vHP+3MOetKLjNnpZXt4c6U3UB4gWLKR4H9AIgYTyJofmztcTBjq3KZcDdxu+G4RPVwE5vg8VaN2jbQao=";
    const UID: &str = "test/9PZ9vww/Bpf5vJxtJhpz80=";

    #[test]
    fn identities_are_imported_from_the_usual_formats() {
        let from_key = import_identity(KEY).unwrap();
        assert_eq!(from_key.key().to_pub().get_uid(), UID);
        assert!(from_key.level() >= MIN_SECURITY_LEVEL, "raised to a usable level");

        // As the TeamSpeak client exports it (counter, `V`, obfuscated key), also as file line.
        let exported = export_identity(&from_key);
        for input in [
            exported.clone(),
            format!("identity=\"{exported}\""),
            format!("  {exported}\n"),
            // The file the TeamSpeak client exports.
            format!("[Identity]\r\nid=James\r\nidentity=\"{exported}\"\r\nnickname=James\r\n"),
        ] {
            let identity = import_identity(&input).unwrap();
            assert_eq!(identity.key().to_pub().get_uid(), UID, "{input}");
            assert_eq!(identity.counter(), from_key.counter());
        }

        assert!(import_identity("").is_err());
        assert!(import_identity("not an identity").is_err());
    }

    #[test]
    fn stored_credentials_are_exported_in_importable_form() {
        let factory = TeamSpeakFactory;
        let stored = factory.import_credentials(KEY).unwrap();
        let exported = factory.export_credentials(&stored).unwrap();
        let again = factory.import_credentials(&exported).unwrap();
        assert_eq!(again, stored, "the export imports as the same identity");
        assert!(factory.export_credentials(&serde_json::json!({})).is_err());
    }

    #[test]
    fn channels_are_found_by_id_or_path() {
        // (id, parent, name, order)
        let channels = [
            (1, 0, "Lobby", 0),
            (2, 1, "Music", 0),
            (3, 0, "Music", 1),
            (4, 0, "Twice", 2),
            (5, 0, "Twice", 1),
            (6, 0, "42", 3),
        ];
        let find = |spec: &str| find_channel_in(channels.iter().copied(), spec);
        assert_eq!(find("2"), Some(2));
        assert_eq!(find("99"), None, "unknown id");
        assert_eq!(find("Lobby/Music"), Some(2));
        assert_eq!(
            find(" /Lobby / Music/ "),
            Some(2),
            "spaces and slashes around parts"
        );
        assert_eq!(find("Music"), Some(3), "top level");
        assert_eq!(find("Lobby/Nothing"), None);
        assert_eq!(find("Twice"), Some(5), "the first in order");
        assert_eq!(find("42"), None, "a number is an id");
        assert_eq!(find(""), None);
        assert_eq!(find(" / "), None);
    }

    #[test]
    fn nickname_and_channel_change_without_reconnecting() {
        let factory = TeamSpeakFactory;
        let base = serde_json::json!({"address": "ts.example", "nickname": "James"});
        let renamed = serde_json::json!({"address": "ts.example", "nickname": "Jimmy"});
        assert!(!factory.requires_reconnect(&base, &renamed));
        let moved =
            serde_json::json!({"address": "ts.example", "nickname": "James", "channel": "Lobby/Music"});
        assert!(
            !factory.requires_reconnect(&base, &moved),
            "the bot moves instead"
        );
        let password =
            serde_json::json!({"address": "ts.example", "nickname": "James", "channel_password": "x"});
        assert!(!factory.requires_reconnect(&base, &password));
        assert!(parse_settings(&serde_json::json!({"channel": "  "}))
            .unwrap()
            .channel
            .is_none());
        for changed in [
            serde_json::json!({"address": "other.example", "nickname": "James"}),
            serde_json::json!({"address": "ts.example", "nickname": "James", "server_password": "x"}),
            serde_json::json!({"nonsense": true}),
        ] {
            assert!(factory.requires_reconnect(&base, &changed), "{changed}");
        }
    }

    #[test]
    fn credentials_are_described_without_the_key() {
        let factory = TeamSpeakFactory;
        let data = factory.import_credentials(KEY).unwrap();
        let info = factory.describe_credentials(&data).unwrap();
        assert_eq!(info["uid"], UID);
        // As the TeamSpeak 6 test server reported it for this identity.
        assert_eq!(info["uidTs6"], "3WhVP5ZzCWt4hDl4m2UFcqxTCChTs6N+RSpxytZuQeA=");
        assert!(info["securityLevel"].as_u64().unwrap() >= 8);
        assert!(
            !info.to_string().contains("MG8D"),
            "no private key in the description"
        );
        assert!(factory.describe_credentials(&serde_json::json!({})).is_none());
    }
}

#[cfg(test)]
mod info_limiter_tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn info_requests_are_limited_per_client_and_per_second() {
        let mut limiter = InfoLimiter::default();
        let start = Instant::now();
        assert!(limiter.allow("1", start));
        assert!(
            !limiter.allow("1", start + Duration::from_secs(5)),
            "same client too soon"
        );
        let allowed = (2..30).filter(|id| limiter.allow(&id.to_string(), start)).count();
        assert_eq!(allowed, INFO_BURST - 1, "burst per second");
        let later = start + Duration::from_secs(2);
        assert!(limiter.allow("29", later), "a new second allows more");
        assert!(
            limiter.allow("1", start + INFO_INTERVAL),
            "same client after the interval"
        );
    }
}
