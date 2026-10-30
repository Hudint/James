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
#[serde(rename_all = "camelCase")]
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
    /// Largest encoded audio frame (20 ms) the platform delivers; `None` = no limit.
    pub max_frame_bytes: Option<usize>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Client {
    pub id: PlatformId,
    /// Stable identity across sessions (TeamSpeak UID, Discord user id).
    pub uid: String,
    pub name: String,
    pub channel: Option<PlatformId>,
    pub server_groups: Vec<PlatformId>,
    pub is_self: bool,
    /// Permanent database id (TeamSpeak), if known.
    pub database_id: Option<String>,
    pub channel_group: Option<PlatformId>,
    pub description: String,
    pub phonetic_name: String,
    /// Away message; `Some("")` when away without a message.
    pub away: Option<String>,
    /// Microphone muted.
    pub input_muted: bool,
    /// Speakers / sound muted.
    pub output_muted: bool,
    pub recording: bool,
    pub talking: bool,
    /// ISO country code, if the server reports it.
    pub country: String,
    /// Server query client instead of a normal client.
    pub query: bool,
    pub talk_power: i32,
    pub priority_speaker: bool,
    pub channel_commander: bool,
    /// Client software details (TeamSpeak: requested lazily, see [`PlatformCommand::RequestClientInfo`]).
    pub info: Option<ClientInfo>,
    /// Connection statistics (TeamSpeak: requested on demand).
    pub connection: Option<ConnectionInfo>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct ClientInfo {
    pub version: String,
    pub platform: String,
    /// First connection, Unix seconds.
    pub created: i64,
    pub last_connected: i64,
    pub total_connections: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct ConnectionInfo {
    pub ping_ms: Option<f64>,
    /// Packet loss (0-1).
    pub packet_loss: Option<f32>,
    pub bytes_sent: Option<u64>,
    pub bytes_received: Option<u64>,
    pub connected_ms: Option<u64>,
    pub idle_ms: Option<u64>,
    /// Remote IP address (only with sufficient permissions). Personal data: removed with
    /// [`strip_client_addresses`] before it reaches anyone who does not need it.
    pub address: Option<String>,
}

/// Removes the clients' IP addresses (`connection.address`) anywhere in serialized platform
/// data: snapshots, events and event envelopes.
pub fn strip_client_addresses(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::Object(connection)) = map.get_mut("connection") {
                connection.remove("address");
            }
            map.values_mut().for_each(strip_client_addresses);
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(strip_client_addresses),
        _ => {}
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Channel {
    pub id: PlatformId,
    pub parent: Option<PlatformId>,
    pub name: String,
    pub topic: String,
    pub description: String,
    /// Codec id (TeamSpeak: 0-5, 4 = Opus Voice, 5 = Opus Music).
    pub codec: Option<u8>,
    pub codec_quality: Option<u8>,
    /// Rank in tree order (depth first) within the snapshot.
    pub order: i64,
    /// Maximum clients; `None` = unlimited.
    pub max_clients: Option<i32>,
    pub max_family_clients: Option<i32>,
    pub permanent: bool,
    pub semi_permanent: bool,
    pub default: bool,
    pub has_password: bool,
    pub encrypted: bool,
    pub needed_talk_power: i32,
    /// Delay before an empty temporary channel is deleted, in seconds.
    pub delete_delay: u32,
    pub phonetic_name: String,
    pub icon: u32,
    /// Text-only channel (Discord); TeamSpeak channels are always voice channels.
    pub text: bool,
    /// The bot is subscribed (sees the clients inside).
    pub subscribed: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct ServerGroup {
    pub id: PlatformId,
    pub name: String,
    pub icon: u32,
}

/// Channel group (TeamSpeak); same shape as a server group.
pub type ChannelGroup = ServerGroup;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
    pub platform: String,
    pub welcome_message: String,
    /// Stable server identifier (TeamSpeak: virtual server id).
    pub id: String,
    pub uid: String,
    pub phonetic_name: String,
    pub host_message: String,
    pub host_banner_url: String,
    pub host_banner_gfx_url: String,
    /// Banner reload interval in seconds.
    pub host_banner_gfx_interval: u64,
    pub host_button_url: String,
    pub host_button_gfx_url: String,
    pub icon: u32,
    pub max_clients: u32,
    pub default_server_group: Option<PlatformId>,
    pub default_channel_group: Option<PlatformId>,
    /// TeamSpeak codec encryption mode: 0 per channel, 1 forced off, 2 forced on.
    pub codec_encryption_mode: u8,
    /// Creation time, Unix seconds.
    pub created: i64,
}

/// Properties for creating or editing a channel; unset fields are left unchanged.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct ChannelParams {
    pub name: Option<String>,
    /// Parent channel (create only; `None` = top level).
    pub parent: Option<PlatformId>,
    pub topic: Option<String>,
    pub description: Option<String>,
    pub password: Option<String>,
    pub codec: Option<u8>,
    pub codec_quality: Option<u8>,
    pub encrypted: Option<bool>,
    pub permanent: Option<bool>,
    pub semi_permanent: Option<bool>,
    pub default: Option<bool>,
    /// Id of the sibling above (0 = first).
    pub position: Option<String>,
    /// -1 = unlimited.
    pub max_clients: Option<i32>,
    pub max_family_clients: Option<i32>,
    pub needed_talk_power: Option<i32>,
    pub delete_delay: Option<u32>,
    pub phonetic_name: Option<String>,
}

/// Why a client left the server.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum LeaveReason {
    /// Regular disconnect.
    #[default]
    Disconnect,
    /// Connection timed out.
    Timeout,
    /// Kicked from the server.
    Kicked,
    /// Banned; `seconds` = 0 means permanently, `None` if unknown.
    Banned { seconds: Option<u64> },
    /// Server shut down.
    ServerShutdown,
    /// No longer visible (e.g. channel unsubscribed), but still on the server.
    Invisible,
}

/// Why a client changed channels.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum MoveReason {
    /// Moved by itself.
    #[default]
    Switched,
    /// Moved by someone else (see `invoker`).
    Moved,
    /// Kicked from its channel.
    Kicked,
    /// Became visible because the bot subscribed to its channel (or it joined one).
    Visible,
}

/// Full view of the server as seen by the bot.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub server: Option<ServerInfo>,
    pub own_client: Option<PlatformId>,
    pub clients: Vec<Client>,
    pub channels: Vec<Channel>,
    pub server_groups: Vec<ServerGroup>,
    pub channel_groups: Vec<ChannelGroup>,
}

impl Snapshot {
    /// The snapshot without the clients' IP addresses.
    pub fn without_addresses(mut self) -> Self {
        for client in &mut self.clients {
            if let Some(connection) = &mut client.connection {
                connection.address = None;
            }
        }
        self
    }
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
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
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
        /// Human-readable reason; empty on TeamSpeak (the notification has no message).
        reason: String,
        #[serde(default)]
        kind: LeaveReason,
        #[serde(default)]
        invoker: Option<Client>,
    },
    ClientMove {
        client: Client,
        from: Option<PlatformId>,
        to: Option<PlatformId>,
        invoker: Option<Client>,
        #[serde(default)]
        kind: MoveReason,
    },
    /// Any property of a client changed; `client` is the new state.
    ClientUpdate {
        client: Client,
    },
    ChannelCreate {
        channel: Channel,
        #[serde(default)]
        invoker: Option<Client>,
    },
    ChannelUpdate {
        channel: Channel,
        #[serde(default)]
        invoker: Option<Client>,
    },
    ChannelDelete {
        id: PlatformId,
        #[serde(default)]
        invoker: Option<Client>,
    },
    /// Server properties changed.
    ServerUpdate {
        server: ServerInfo,
    },
    /// Server or channel groups changed (added, renamed, removed).
    GroupsUpdate {
        server_groups: Vec<ServerGroup>,
        channel_groups: Vec<ChannelGroup>,
    },
    TalkerCount {
        count: usize,
    },
    /// Adapter-specific credentials changed and should be persisted (e.g. an
    /// improved TeamSpeak identity). Passed back to the adapter on the next start.
    CredentialsUpdated {
        data: serde_json::Value,
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
    /// Bans a client; `seconds` = 0 bans permanently.
    Ban {
        client: PlatformId,
        seconds: u64,
        reason: String,
        reply: Reply<()>,
    },
    SetAway {
        /// `None` = back.
        message: Option<String>,
        reply: Reply<()>,
    },
    SetChannelGroup {
        client: PlatformId,
        channel: PlatformId,
        group: PlatformId,
        reply: Reply<()>,
    },
    /// Creates a channel and returns its id.
    CreateChannel {
        params: ChannelParams,
        reply: Reply<PlatformId>,
    },
    EditChannel {
        channel: PlatformId,
        params: ChannelParams,
        reply: Reply<()>,
    },
    /// Moves a channel below a new parent, after the sibling `order` (0 = first).
    MoveChannel {
        channel: PlatformId,
        parent: Option<PlatformId>,
        order: Option<String>,
        reply: Reply<()>,
    },
    DeleteChannel {
        channel: PlatformId,
        /// Also delete when clients are inside.
        force: bool,
        reply: Reply<()>,
    },
    /// Subscribes to (or unsubscribes from) a channel to see its clients.
    Subscribe {
        channel: PlatformId,
        subscribe: bool,
        reply: Reply<()>,
    },
    /// Requests fresh [`Client::info`] / [`Client::connection`] data; the result arrives as
    /// a [`PlatformEvent::ClientUpdate`].
    RequestClientInfo {
        client: PlatformId,
        reply: Reply<()>,
    },
    Snapshot {
        reply: Reply<Snapshot>,
    },
    /// Platform-specific operation (e.g. TeamSpeak channel management), so plugins can
    /// use features without a dedicated variant. Unknown names fail with an error.
    Extension {
        name: String,
        args: serde_json::Value,
        reply: Reply<serde_json::Value>,
    },
    /// Changed settings that need no new connection ([`PlatformFactory::requires_reconnect`]
    /// returned `false`), e.g. a new nickname; the adapter applies what changed.
    UpdateSettings {
        settings: serde_json::Value,
        reply: Reply<()>,
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
    /// What clients say, for speech recognition (`None`: nobody listens; packets are dropped
    /// when the receiver is behind).
    pub voice: Option<mpsc::Sender<VoicePacket>>,
}

/// One Opus frame a client sent (20 ms); empty `data` marks the end of what it said.
#[derive(Debug, Clone)]
pub struct VoicePacket {
    pub client: PlatformId,
    pub data: Vec<u8>,
}

#[async_trait]
pub trait PlatformAdapter: Send {
    fn capabilities(&self) -> Capabilities;

    /// Connects and runs until disconnected or the command channel closes.
    /// Returns `Ok(())` on a requested disconnect, an error otherwise (the
    /// instance runtime decides about reconnecting).
    async fn run(&mut self, ctx: &mut PlatformContext) -> anyhow::Result<()>;
}

/// Creates platform adapters from instance settings (one factory per platform).
pub trait PlatformFactory: Send + Sync {
    /// Platform id used in instance configs, e.g. `"teamspeak"`.
    fn id(&self) -> &str;
    /// Creates an adapter; `credentials` are the last persisted
    /// [`PlatformEvent::CredentialsUpdated`] data.
    fn create(
        &self,
        settings: &serde_json::Value,
        credentials: Option<serde_json::Value>,
    ) -> anyhow::Result<Box<dyn PlatformAdapter>>;

    /// Validates settings without side effects (no connection, no key generation).
    fn validate(&self, settings: &serde_json::Value) -> anyhow::Result<()>;

    /// Capabilities of adapters created by this factory.
    fn capabilities(&self) -> Capabilities;

    /// Whether changing the settings from `old` to `new` needs a new connection. Changes that
    /// do not (the nickname) are applied to the running connection instead.
    fn requires_reconnect(&self, old: &serde_json::Value, new: &serde_json::Value) -> bool {
        old != new
    }

    /// Reads credentials an administrator entered (e.g. an exported TeamSpeak identity) into
    /// the form [`PlatformFactory::create`] receives.
    fn import_credentials(&self, input: &str) -> anyhow::Result<serde_json::Value> {
        let _ = input;
        anyhow::bail!("this platform has no credentials to import")
    }

    /// Stored credentials in the form an administrator could import them again elsewhere (e.g.
    /// the TeamSpeak client's `123V…`). Secret: only for explicit exports.
    fn export_credentials(&self, data: &serde_json::Value) -> anyhow::Result<String> {
        let _ = data;
        anyhow::bail!("this platform has no credentials to export")
    }

    /// What may be shown about stored credentials (e.g. the identity's public unique id; never
    /// secrets). `None` if there is nothing to show.
    fn describe_credentials(&self, data: &serde_json::Value) -> Option<serde_json::Value> {
        let _ = data;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_fields_are_camel_case() {
        let event = PlatformEvent::GroupsUpdate {
            server_groups: vec![],
            channel_groups: vec![],
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "type": "groupsUpdate", "serverGroups": [], "channelGroups": [] })
        );
    }

    #[test]
    fn client_addresses_are_stripped_everywhere() {
        let mut value = serde_json::json!({
            "instance": "music",
            "event": { "type": "clientUpdate", "client": { "id": "5", "connection": { "pingMs": 9.0, "address": "203.0.113.7" } } },
            "snapshot": { "clients": [{ "connection": { "address": "198.51.100.1" } }, { "connection": null }] }
        });
        strip_client_addresses(&mut value);
        let text = value.to_string();
        assert!(
            !text.contains("203.0.113.7") && !text.contains("198.51.100.1"),
            "{text}"
        );
        assert_eq!(value["event"]["client"]["connection"]["pingMs"], 9.0);
    }
}
