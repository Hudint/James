//! Conversion of tsclientlib's book (server state) and events into platform-neutral types.

use james_core::platform::{self, MessageTarget, PlatformEvent, Snapshot};
use tsclientlib::data::{self, Connection};
use tsclientlib::events::{Event, PropertyId, PropertyValue};
use tsclientlib::{ChannelId, Invoker};

pub fn snapshot(state: &Connection) -> Snapshot {
    let mut channels: Vec<_> = state.channels.values().map(channel).collect();
    channels.sort_by_key(|c| c.order);
    Snapshot {
        server: Some(platform::ServerInfo {
            name: state.server.name.clone(),
            version: state.server.version.clone(),
            platform: state.server.platform.clone(),
            welcome_message: state.server.welcome_message.clone(),
        }),
        own_client: Some(state.own_client.0.to_string()),
        clients: state.clients.values().map(|c| client(state, c)).collect(),
        channels,
        server_groups: state
            .server_groups
            .values()
            .map(|g| platform::ServerGroup {
                id: g.id.0.to_string(),
                name: g.name.clone(),
            })
            .collect(),
    }
}

fn channel_ref(id: ChannelId) -> Option<String> {
    (id.0 != 0).then(|| id.0.to_string())
}

pub fn client(state: &Connection, c: &data::Client) -> platform::Client {
    let mut server_groups: Vec<_> = c.server_groups.iter().map(|g| g.0.to_string()).collect();
    server_groups.sort();
    platform::Client {
        id: c.id.0.to_string(),
        uid: c.uid.as_ref().map(|u| u.as_ref().to_string()).unwrap_or_default(),
        name: c.name.clone(),
        channel: channel_ref(c.channel),
        server_groups,
        is_self: c.id == state.own_client,
    }
}

pub fn channel(c: &data::Channel) -> platform::Channel {
    platform::Channel {
        id: c.id.0.to_string(),
        parent: channel_ref(c.parent),
        name: c.name.clone(),
        topic: c.topic.clone().unwrap_or_default(),
        description: c
            .optional_data
            .as_ref()
            .map(|d| d.description.clone())
            .unwrap_or_default(),
        codec_quality: c.codec_quality,
        order: c.order.0 as i64,
    }
}

fn invoker_client(state: &Connection, invoker: &Invoker) -> platform::Client {
    match state.clients.get(&invoker.id) {
        Some(c) => client(state, c),
        None => platform::Client {
            id: invoker.id.0.to_string(),
            uid: invoker
                .uid
                .as_ref()
                .map(|u| u.as_ref().to_string())
                .unwrap_or_default(),
            name: invoker.name.clone(),
            channel: None,
            server_groups: Vec::new(),
            is_self: invoker.id == state.own_client,
        },
    }
}

/// Translates one book event; `state` is the book *after* the event was applied.
pub fn event(state: &Connection, event: &Event) -> Option<PlatformEvent> {
    match event {
        Event::Message {
            target,
            invoker,
            message,
        } => {
            if invoker.id == state.own_client {
                return None;
            }
            let from = invoker_client(state, invoker);
            let text = message.clone();
            Some(match target {
                tsclientlib::MessageTarget::Poke(_) => PlatformEvent::Poke { from, text },
                tsclientlib::MessageTarget::Client(_) => PlatformEvent::Message {
                    mode: MessageTarget::Client,
                    from,
                    text,
                },
                tsclientlib::MessageTarget::Channel => PlatformEvent::Message {
                    mode: MessageTarget::Channel,
                    from,
                    text,
                },
                tsclientlib::MessageTarget::Server => PlatformEvent::Message {
                    mode: MessageTarget::Server,
                    from,
                    text,
                },
            })
        }
        Event::PropertyAdded { id, .. } => match id {
            PropertyId::Client(id) => state.clients.get(id).map(|c| PlatformEvent::ClientEnter {
                client: client(state, c),
            }),
            PropertyId::Channel(id) => state
                .channels
                .get(id)
                .map(|c| PlatformEvent::ChannelCreate { channel: channel(c) }),
            PropertyId::ClientServerGroup(id, _) => client_update(state, *id),
            _ => None,
        },
        Event::PropertyRemoved { id, old, extra, .. } => match (id, old) {
            (PropertyId::Client(_), PropertyValue::Client(c)) => Some(PlatformEvent::ClientLeave {
                client: client(state, c),
                reason: extra.reason.map(|r| format!("{r:?}")).unwrap_or_default(),
            }),
            (PropertyId::Channel(id), _) => Some(PlatformEvent::ChannelDelete { id: id.0.to_string() }),
            (PropertyId::ClientServerGroup(id, _), _) => client_update(state, *id),
            _ => None,
        },
        Event::PropertyChanged { id, old, invoker, .. } => match (id, old) {
            (PropertyId::ClientChannel(id), PropertyValue::ChannelId(from)) => {
                state.clients.get(id).map(|c| PlatformEvent::ClientMove {
                    client: client(state, c),
                    from: channel_ref(*from),
                    to: channel_ref(c.channel),
                    invoker: invoker.as_ref().map(|i| invoker_client(state, i)),
                })
            }
            (PropertyId::ClientName(id) | PropertyId::ClientDescription(id), _) => client_update(state, *id),
            (
                PropertyId::ChannelName(id)
                | PropertyId::ChannelTopic(id)
                | PropertyId::ChannelParent(id)
                | PropertyId::ChannelOrder(id)
                | PropertyId::ChannelCodecQuality(id)
                | PropertyId::OptionalChannelData(id),
                _,
            ) => state
                .channels
                .get(id)
                .map(|c| PlatformEvent::ChannelUpdate { channel: channel(c) }),
            _ => None,
        },
    }
}

fn client_update(state: &Connection, id: tsclientlib::ClientId) -> Option<PlatformEvent> {
    state.clients.get(&id).map(|c| PlatformEvent::ClientUpdate {
        client: client(state, c),
    })
}
