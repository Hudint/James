//! TeamSpeak 3 / TeamSpeak 6 adapter based on tsclientlib (native client protocol).
//!
//! TeamSpeak 6 servers accept the TS3 client protocol from clients reporting
//! version 3.6.0 or newer, which is what this adapter announces by default.

mod channel;
mod convert;

use std::collections::HashMap;

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
        // Everything but the nickname is part of the connection (the nickname is changed on
        // the running one).
        match (parse_settings(old), parse_settings(new)) {
            (Ok(old), Ok(new)) => {
                let mut new = new;
                new.nickname = old.nickname.clone();
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

fn parse_settings(settings: &serde_json::Value) -> anyhow::Result<TeamSpeakConfig> {
    serde_json::from_value(settings.clone()).context("invalid TeamSpeak settings")
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
        if let Some(channel) = &self.config.channel {
            options = match channel.parse::<u64>() {
                Ok(id) => options.channel_id(ChannelId(id)),
                Err(_) => options.channel(channel.clone()),
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
        let _ = ctx.events.send(PlatformEvent::Connected {
            snapshot: snapshot(con.get_state()?),
        });
        self.persist_identity(&con, ctx);

        let mut session = Session {
            pending: HashMap::new(),
            created: Vec::new(),
            can_send_audio: true,
            reconnecting: false,
            audio_closed: false,
        };
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

struct Session {
    pending: HashMap<MessageHandle, Reply<()>>,
    created: Vec<PendingCreate>,
    can_send_audio: bool,
    /// tsclientlib is re-establishing the connection; ids in the book change.
    reconnecting: bool,
    /// The audio sender was dropped (engine gone); stop polling it.
    audio_closed: bool,
}

enum Input {
    Event(Option<Result<StreamItem, tsclientlib::Error>>),
    Command(Option<PlatformCommand>),
    Audio(Option<OpusFrame>),
}

impl Session {
    /// Returns the disconnect reason on a regular disconnect.
    async fn run(&mut self, con: &mut Connection, ctx: &mut PlatformContext) -> anyhow::Result<String> {
        loop {
            let input = {
                let mut events = con.events();
                tokio::select! {
                    e = events.next() => Input::Event(e),
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
                    // After a reconnect all client ids are new: publish a fresh snapshot.
                    self.reconnecting = false;
                    tracing::info!("reconnected");
                    let _ = ctx.events.send(PlatformEvent::Connected {
                        snapshot: snapshot(state),
                    });
                    return;
                }
                let converted = events.iter().filter_map(|e| convert::event(state, e)).collect();
                for event in convert::coalesce(converted) {
                    let _ = ctx.events.send(event);
                }
            }
            StreamItem::MessageResult(handle, result) => {
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
            PlatformCommand::UpdateSettings { settings, reply } => {
                // Only the nickname can change on a running connection.
                let result = parse_settings(&settings).and_then(|config| {
                    let state = con.get_state()?;
                    let current = state.clients.get(&state.own_client).map(|c| c.name.as_str());
                    Ok((current != Some(config.nickname.as_str())).then(|| {
                        let mut update = state.client_update();
                        update.name = Some(config.nickname.as_str().into());
                        update.to_packet()
                    }))
                });
                match result {
                    Ok(Some(packet)) => self.send(con, Ok(packet), reply),
                    Ok(None) => {
                        let _ = reply.send(Ok(()));
                    }
                    Err(e) => {
                        let _ = reply.send(Err(e));
                    }
                }
            }
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
        let exported = format!("{}V{}", from_key.counter(), from_key.key().to_ts_obfuscated());
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
    fn only_the_nickname_changes_without_reconnecting() {
        let factory = TeamSpeakFactory;
        let base = serde_json::json!({"address": "ts.example", "nickname": "James"});
        let renamed = serde_json::json!({"address": "ts.example", "nickname": "Jimmy"});
        assert!(!factory.requires_reconnect(&base, &renamed));
        for changed in [
            serde_json::json!({"address": "other.example", "nickname": "James"}),
            serde_json::json!({"address": "ts.example", "nickname": "James", "server_password": "x"}),
            serde_json::json!({"address": "ts.example", "nickname": "James", "channel": "Lobby"}),
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
