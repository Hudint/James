//! Platform adapter boundary (TeamSpeak, Discord, …).
//!
//! An adapter runs as one task per connection. The instance runtime talks to
//! it exclusively through [`PlatformContext`]: it sends [`PlatformCommand`]s
//! and Opus frames and receives [`PlatformEvent`]s.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

use crate::audio::OpusFrame;

/// Platform-specific identifier of a client, channel or server group, as a string.
pub type PlatformId = String;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Capabilities {
    /// Number of independent voice sessions (TeamSpeak: 1, Discord: one per guild).
    pub multi_session: bool,
    pub pokes: bool,
    pub server_groups: bool,
    pub channel_groups: bool,
    pub avatars: bool,
    pub video: bool,
    /// Maximum Opus bitrate the platform accepts, in bit/s.
    pub max_bitrate: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Client {
    pub id: PlatformId,
    /// Stable identity across sessions (TeamSpeak UID, Discord user id).
    pub uid: String,
    pub name: String,
    pub channel: Option<PlatformId>,
    pub server_groups: Vec<PlatformId>,
    pub is_self: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Channel {
    pub id: PlatformId,
    pub parent: Option<PlatformId>,
    pub name: String,
    pub topic: String,
    pub description: String,
    pub codec_quality: Option<u8>,
    pub order: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerGroup {
    pub id: PlatformId,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
    pub platform: String,
    pub welcome_message: String,
}

/// Full view of the server as seen by the bot.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub server: Option<ServerInfo>,
    pub own_client: Option<PlatformId>,
    pub clients: Vec<Client>,
    pub channels: Vec<Channel>,
    pub server_groups: Vec<ServerGroup>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MessageTarget {
    /// Private message to a client.
    Client,
    /// Message to the bot's current channel.
    Channel,
    /// Server-wide message.
    Server,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum PlatformEvent {
    Connected {
        snapshot: Snapshot,
    },
    Disconnected {
        reason: String,
    },
    Message {
        mode: MessageTarget,
        from: Client,
        text: String,
    },
    Poke {
        from: Client,
        text: String,
    },
    ClientEnter {
        client: Client,
    },
    ClientLeave {
        client: Client,
        reason: String,
    },
    ClientMove {
        client: Client,
        from: Option<PlatformId>,
        to: Option<PlatformId>,
        invoker: Option<Client>,
    },
    ClientUpdate {
        client: Client,
    },
    ChannelCreate {
        channel: Channel,
    },
    ChannelUpdate {
        channel: Channel,
    },
    ChannelDelete {
        id: PlatformId,
    },
    TalkerCount {
        count: usize,
    },
}

pub type Reply<T> = oneshot::Sender<anyhow::Result<T>>;

#[derive(Debug)]
pub enum PlatformCommand {
    SendMessage {
        target: MessageTarget,
        client: Option<PlatformId>,
        text: String,
        reply: Reply<()>,
    },
    Poke {
        client: PlatformId,
        text: String,
        reply: Reply<()>,
    },
    MoveSelf {
        channel: PlatformId,
        password: Option<String>,
        reply: Reply<()>,
    },
    MoveClient {
        client: PlatformId,
        channel: PlatformId,
        reply: Reply<()>,
    },
    Kick {
        client: PlatformId,
        from_server: bool,
        reason: String,
        reply: Reply<()>,
    },
    SetNickname {
        name: String,
        reply: Reply<()>,
    },
    SetDescription {
        text: String,
        reply: Reply<()>,
    },
    AddServerGroup {
        client: PlatformId,
        group: PlatformId,
        reply: Reply<()>,
    },
    RemoveServerGroup {
        client: PlatformId,
        group: PlatformId,
        reply: Reply<()>,
    },
    Snapshot {
        reply: Reply<Snapshot>,
    },
    Disconnect {
        reason: String,
    },
}

/// Channels connecting an adapter to its instance.
pub struct PlatformContext {
    pub events: mpsc::UnboundedSender<PlatformEvent>,
    pub commands: mpsc::Receiver<PlatformCommand>,
    pub audio: mpsc::Receiver<OpusFrame>,
}

#[async_trait]
pub trait PlatformAdapter: Send {
    fn capabilities(&self) -> Capabilities;

    /// Connects and runs until disconnected or the command channel closes.
    /// Returns `Ok(())` on a requested disconnect, an error otherwise (the
    /// instance runtime decides about reconnecting).
    async fn run(&mut self, ctx: &mut PlatformContext) -> anyhow::Result<()>;
}
