//! TeamSpeak 3 / TeamSpeak 6 adapter based on tsclientlib (native client protocol).
//!
//! TeamSpeak 6 servers accept the TS3 client protocol from clients reporting
//! version 3.6.0 or newer, which is what this adapter announces by default.

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
use tsclientlib::prelude::*;
use tsclientlib::{
    ChannelId, ClientId, Connection, DisconnectOptions, Identity, MessageHandle, Reason, ServerGroupId,
    StreamItem, Version,
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
        if config.nickname.trim().is_empty() || config.nickname.chars().count() > 30 {
            bail!("nickname must be 1-30 characters");
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
}

fn parse_settings(settings: &serde_json::Value) -> anyhow::Result<TeamSpeakConfig> {
    serde_json::from_value(settings.clone()).context("invalid TeamSpeak settings")
}

fn capabilities() -> Capabilities {
    Capabilities {
        multi_session: false,
        pokes: true,
        server_groups: true,
        channel_groups: true,
        avatars: true,
        video: false,
        max_bitrate: 256_000,
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

struct Session {
    pending: HashMap<MessageHandle, Reply<()>>,
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
                for event in events {
                    if let Some(event) = convert::event(state, &event) {
                        let _ = ctx.events.send(event);
                    }
                }
            }
            StreamItem::MessageResult(handle, result) => {
                if let Some(reply) = self.pending.remove(&handle) {
                    let _ = reply.send(result.map_err(|e| anyhow!("{e}")));
                }
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
                let _ = ctx.events.send(PlatformEvent::Disconnected {
                    reason: format!("{reason:?}"),
                });
            }
            _ => {}
        }
    }

    fn handle_command(&mut self, con: &mut Connection, command: PlatformCommand) {
        match command {
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
            PlatformCommand::Extension { name, reply, .. } => {
                let _ = reply.send(Err(anyhow!("unsupported TeamSpeak extension '{name}'")));
            }
            // Handled by the session loop before dispatching.
            PlatformCommand::Disconnect { .. } => {}
        }
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
