//! Conversion of tsclientlib's book (server state) and events into platform-neutral types.

use std::collections::HashMap;

use james_core::platform::{self, LeaveReason, MessageTarget, MoveReason, PlatformEvent, Snapshot};
use tsclientlib::data::{self, Connection};
use tsclientlib::events::{Event, PropertyId, PropertyValue};
use tsclientlib::{ChannelId, ChannelType, ClientType, Invoker, MaxClients, Reason};

pub fn snapshot(state: &Connection) -> Snapshot {
    let channels = ordered_channels(state)
        .into_iter()
        .map(|(rank, c)| platform::Channel {
            order: rank,
            ..channel(c)
        })
        .collect();
    let (server_groups, channel_groups) = groups(state);
    Snapshot {
        server: Some(server(state)),
        own_client: Some(state.own_client.0.to_string()),
        clients: state.clients.values().map(|c| client(state, c)).collect(),
        channels,
        server_groups,
        channel_groups,
    }
}

pub fn server(state: &Connection) -> platform::ServerInfo {
    let s = &state.server;
    platform::ServerInfo {
        name: s.name.clone(),
        version: s.version.clone(),
        platform: s.platform.clone(),
        welcome_message: s.welcome_message.clone(),
        id: s.id.to_string(),
        uid: s.public_key.get_uid().to_string(),
        phonetic_name: s.phonetic_name.clone(),
        host_message: s.hostmessage.clone(),
        host_banner_url: s.hostbanner_url.clone(),
        host_banner_gfx_url: s.hostbanner_gfx_url.clone(),
        host_banner_gfx_interval: s.hostbanner_gfx_interval.whole_seconds().max(0) as u64,
        host_button_url: s.hostbutton_url.clone(),
        host_button_gfx_url: s.hostbutton_gfx_url.clone(),
        icon: s.icon.0,
        max_clients: u32::from(s.max_clients),
        default_server_group: Some(s.default_server_group.0.to_string()),
        default_channel_group: Some(s.default_channel_group.0.to_string()),
        codec_encryption_mode: s.codec_encryption_mode as u8,
        created: s.created.unix_timestamp(),
    }
}

/// Server and channel groups, sorted by id (regular groups only).
pub fn groups(state: &Connection) -> (Vec<platform::ServerGroup>, Vec<platform::ChannelGroup>) {
    let mut server: Vec<_> = state
        .server_groups
        .values()
        .map(|g| platform::ServerGroup {
            id: g.id.0.to_string(),
            name: g.name.clone(),
            icon: g.icon.0,
        })
        .collect();
    server.sort_by_key(|g| g.id.parse::<u64>().unwrap_or(0));
    let mut channel: Vec<_> = state
        .channel_groups
        .values()
        .map(|g| platform::ChannelGroup {
            id: g.id.0.to_string(),
            name: g.name.clone(),
            icon: g.icon.0,
        })
        .collect();
    channel.sort_by_key(|g| g.id.parse::<u64>().unwrap_or(0));
    (server, channel)
}

/// Channels in tree order (depth first), each with its rank. TeamSpeak's `order` field is
/// the id of the sibling above, so siblings form a linked list per parent.
fn ordered_channels(state: &Connection) -> Vec<(i64, &data::Channel)> {
    let mut children: HashMap<ChannelId, Vec<&data::Channel>> = HashMap::new();
    for c in state.channels.values() {
        children.entry(c.parent).or_default().push(c);
    }
    fn emit<'a>(
        parent: ChannelId,
        children: &HashMap<ChannelId, Vec<&'a data::Channel>>,
        depth: usize,
        out: &mut Vec<(i64, &'a data::Channel)>,
    ) {
        // Guards against malformed (cyclic) parent links.
        if depth > 64 {
            return;
        }
        let siblings = children.get(&parent).map(Vec::as_slice).unwrap_or_default();
        for c in sort_siblings(siblings, |c| c.id.0, |c| c.order.0) {
            out.push((out.len() as i64, c));
            emit(c.id, children, depth + 1, out);
        }
    }
    let mut out = Vec::with_capacity(state.channels.len());
    emit(ChannelId(0), &children, 0, &mut out);
    out
}

/// Follows the `order` links (id of the item above, 0 = first); unlinked items go last by id.
fn sort_siblings<'a, T>(siblings: &[&'a T], id: impl Fn(&T) -> u64, above: impl Fn(&T) -> u64) -> Vec<&'a T> {
    let mut by_above: HashMap<u64, &T> = siblings.iter().map(|c| (above(c), *c)).collect();
    let mut sorted = Vec::with_capacity(siblings.len());
    let mut previous = 0;
    while let Some(c) = by_above.remove(&previous) {
        sorted.push(c);
        previous = id(c);
    }
    let mut rest: Vec<_> = by_above.into_values().collect();
    rest.sort_by_key(|c| id(c));
    sorted.extend(rest);
    sorted
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
        database_id: (c.database_id.0 != 0).then(|| c.database_id.0.to_string()),
        channel_group: (c.channel_group.0 != 0).then(|| c.channel_group.0.to_string()),
        description: c.description.clone(),
        phonetic_name: c.phonetic_name.clone(),
        away: c.away_message.clone(),
        input_muted: c.input_muted,
        output_muted: c.output_muted,
        recording: c.is_recording,
        talking: false,
        country: c.country_code.clone(),
        query: matches!(c.client_type, ClientType::Query { .. }),
        talk_power: c.talk_power,
        priority_speaker: c.is_priority_speaker,
        channel_commander: c.is_channel_commander,
        info: c.optional_data.as_ref().map(|d| platform::ClientInfo {
            version: d.version.clone(),
            platform: d.platform.clone(),
            created: d.created.unix_timestamp(),
            last_connected: d.last_connected.unix_timestamp(),
            total_connections: d.connections_total,
        }),
        connection: c.connection_data.as_ref().map(|d| platform::ConnectionInfo {
            ping_ms: d.ping.map(|p| p.as_seconds_f64() * 1000.0),
            packet_loss: d.server_to_client_packetloss_total,
            bytes_sent: sum(&[d.bytes_sent_speech, d.bytes_sent_keepalive, d.bytes_sent_control]),
            bytes_received: sum(&[
                d.bytes_received_speech,
                d.bytes_received_keepalive,
                d.bytes_received_control,
            ]),
            connected_ms: d.connected_time.map(|t| t.whole_milliseconds().max(0) as u64),
            idle_ms: Some(d.idle_time.whole_milliseconds().max(0) as u64),
            address: d.client_address.map(|a| a.ip().to_string()),
        }),
    }
}

fn sum(values: &[Option<u64>]) -> Option<u64> {
    values
        .iter()
        .copied()
        .try_fold(0u64, |acc, v| Some(acc.saturating_add(v?)))
}

fn max_clients(value: Option<MaxClients>) -> Option<i32> {
    match value {
        Some(MaxClients::Limited(n)) => Some(i32::from(n)),
        _ => None,
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
        codec: Some(c.codec as u8),
        codec_quality: c.codec_quality,
        order: c.order.0 as i64,
        max_clients: max_clients(c.max_clients),
        max_family_clients: max_clients(c.max_family_clients),
        permanent: c.channel_type == ChannelType::Permanent,
        semi_permanent: c.channel_type == ChannelType::SemiPermanent,
        default: c.is_default.unwrap_or(false),
        has_password: c.has_password.unwrap_or(false),
        encrypted: !c.is_unencrypted.unwrap_or(false),
        needed_talk_power: c.needed_talk_power.unwrap_or(0),
        delete_delay: c
            .delete_delay
            .map(|d| d.whole_seconds().max(0) as u32)
            .unwrap_or(0),
        phonetic_name: c.phonetic_name.clone().unwrap_or_default(),
        icon: c.icon.map(|i| i.0).unwrap_or(0),
        text: false,
        subscribed: c.subscribed,
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
            is_self: invoker.id == state.own_client,
            ..Default::default()
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
        Event::PropertyAdded { id, invoker, extra } => {
            let invoker = invoker.as_ref().map(|i| invoker_client(state, i));
            match id {
                PropertyId::Client(id) => state.clients.get(id).map(|c| {
                    if extra.reason == Some(Reason::Subscription) {
                        // Already on the server; only became visible.
                        PlatformEvent::ClientMove {
                            client: client(state, c),
                            from: None,
                            to: channel_ref(c.channel),
                            invoker: None,
                            kind: MoveReason::Visible,
                        }
                    } else {
                        PlatformEvent::ClientEnter {
                            client: client(state, c),
                        }
                    }
                }),
                PropertyId::Channel(id) => state.channels.get(id).map(|c| PlatformEvent::ChannelCreate {
                    channel: channel(c),
                    invoker,
                }),
                PropertyId::ServerGroup(_) | PropertyId::ChannelGroup(_) => Some(groups_update(state)),
                _ => client_change(state, id).or_else(|| channel_change(state, id, invoker)),
            }
        }
        Event::PropertyRemoved {
            id,
            old,
            invoker,
            extra,
            ..
        } => {
            let invoker = invoker.as_ref().map(|i| invoker_client(state, i));
            match (id, old) {
                (PropertyId::Client(_), PropertyValue::Client(c)) => Some(PlatformEvent::ClientLeave {
                    client: client(state, c),
                    reason: String::new(),
                    kind: leave_reason(extra.reason),
                    invoker,
                }),
                (PropertyId::Channel(id), _) => Some(PlatformEvent::ChannelDelete {
                    id: id.0.to_string(),
                    invoker,
                }),
                (PropertyId::ServerGroup(_) | PropertyId::ChannelGroup(_), _) => Some(groups_update(state)),
                _ => client_change(state, id).or_else(|| channel_change(state, id, invoker)),
            }
        }
        Event::PropertyChanged {
            id,
            old,
            invoker,
            extra,
        } => {
            let invoker = invoker.as_ref().map(|i| invoker_client(state, i));
            match (id, old) {
                (PropertyId::ClientChannel(id), PropertyValue::ChannelId(from)) => {
                    state.clients.get(id).map(|c| PlatformEvent::ClientMove {
                        client: client(state, c),
                        from: channel_ref(*from),
                        to: channel_ref(c.channel),
                        kind: match extra.reason {
                            Some(Reason::KickChannel) => MoveReason::Kicked,
                            Some(Reason::Moved) => MoveReason::Moved,
                            _ => MoveReason::Switched,
                        },
                        invoker,
                    })
                }
                _ if is_server_property(id) => Some(PlatformEvent::ServerUpdate {
                    server: server(state),
                }),
                _ if is_group_property(id) => Some(groups_update(state)),
                _ => client_change(state, id).or_else(|| channel_change(state, id, invoker)),
            }
        }
    }
}

/// One batch of book events often changes many properties of the same object (e.g. a
/// connection info reply, or the groups list after connecting): only the last update per
/// client, of the server and of the groups is kept, at its position.
pub fn coalesce(events: Vec<PlatformEvent>) -> Vec<PlatformEvent> {
    fn key(event: &PlatformEvent) -> Option<String> {
        match event {
            PlatformEvent::ClientUpdate { client } => Some(format!("client:{}", client.id)),
            PlatformEvent::ServerUpdate { .. } => Some("server".into()),
            PlatformEvent::GroupsUpdate { .. } => Some("groups".into()),
            _ => None,
        }
    }
    let last: HashMap<String, usize> = events
        .iter()
        .enumerate()
        .filter_map(|(i, e)| key(e).map(|k| (k, i)))
        .collect();
    let keep: Vec<bool> = events
        .iter()
        .enumerate()
        .map(|(i, e)| key(e).is_none_or(|k| last.get(&k) == Some(&i)))
        .collect();
    events
        .into_iter()
        .zip(keep)
        .filter_map(|(e, keep)| keep.then_some(e))
        .collect()
}

fn leave_reason(reason: Option<Reason>) -> LeaveReason {
    match reason {
        Some(Reason::LostConnection) => LeaveReason::Timeout,
        Some(Reason::KickServer) => LeaveReason::Kicked,
        // The ban duration is not part of the notification.
        Some(Reason::KickServerBan) => LeaveReason::Banned { seconds: None },
        Some(Reason::Serverstop | Reason::ClientdisconnectServerShutdown) => LeaveReason::ServerShutdown,
        Some(Reason::Subscription | Reason::Channelupdate) => LeaveReason::Invisible,
        _ => LeaveReason::Disconnect,
    }
}

fn groups_update(state: &Connection) -> PlatformEvent {
    let (server_groups, channel_groups) = groups(state);
    PlatformEvent::GroupsUpdate {
        server_groups,
        channel_groups,
    }
}

fn is_server_property(id: &PropertyId) -> bool {
    format!("{id:?}").starts_with("Server") && !is_group_property(id)
}

fn is_group_property(id: &PropertyId) -> bool {
    let name = format!("{id:?}");
    name.starts_with("ServerGroup") || name.starts_with("ChannelGroup")
}

/// A property of a client (any field, server groups, optional or connection data).
fn client_change(state: &Connection, id: &PropertyId) -> Option<PlatformEvent> {
    let client = property_client(id)?;
    client_update(state, client)
}

fn channel_change(
    state: &Connection,
    id: &PropertyId,
    invoker: Option<platform::Client>,
) -> Option<PlatformEvent> {
    let channel_id = property_channel(id)?;
    state
        .channels
        .get(&channel_id)
        .map(|c| PlatformEvent::ChannelUpdate {
            channel: channel(c),
            invoker,
        })
}

/// The client a property id refers to (the generated `PropertyId` has one variant per field,
/// all `Client*`/`OptionalClientData`/`ConnectionClientData` variants carry the id first).
fn property_client(id: &PropertyId) -> Option<tsclientlib::ClientId> {
    match id {
        PropertyId::ClientServerGroup(c, _) => Some(*c),
        PropertyId::OptionalClientData(c) | PropertyId::ConnectionClientData(c) => Some(*c),
        _ => {
            let name = format!("{id:?}");
            if !(name.starts_with("Client")
                || name.starts_with("OptionalClientData")
                || name.starts_with("ConnectionClientData"))
            {
                return None;
            }
            // Debug form: `ClientName(ClientId(5))`.
            leading_id(&name, "ClientId(")
                .and_then(|id| u16::try_from(id).ok())
                .map(tsclientlib::ClientId)
        }
    }
}

fn property_channel(id: &PropertyId) -> Option<ChannelId> {
    let name = format!("{id:?}");
    if !(name.starts_with("Channel") || name.starts_with("OptionalChannelData")) || is_group_property(id) {
        return None;
    }
    leading_id(&name, "ChannelId(").map(ChannelId)
}

fn leading_id(debug: &str, marker: &str) -> Option<u64> {
    let start = debug.find(marker)? + marker.len();
    let end = debug[start..].find(')')? + start;
    debug[start..end].parse().ok()
}

fn client_update(state: &Connection, id: tsclientlib::ClientId) -> Option<PlatformEvent> {
    state.clients.get(&id).map(|c| PlatformEvent::ClientUpdate {
        client: client(state, c),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Locks the Debug format of tsclientlib's property ids that the id extraction relies on.
    #[test]
    fn property_ids_resolve_to_clients_and_channels() {
        use tsclientlib::{ClientId, ServerGroupId};
        assert_eq!(
            property_client(&PropertyId::ClientName(ClientId(5))),
            Some(ClientId(5))
        );
        assert_eq!(
            property_client(&PropertyId::ClientAwayMessage(ClientId(7))),
            Some(ClientId(7))
        );
        assert_eq!(
            property_client(&PropertyId::ConnectionClientData(ClientId(9))),
            Some(ClientId(9))
        );
        assert_eq!(
            property_client(&PropertyId::ClientServerGroup(ClientId(3), ServerGroupId(8))),
            Some(ClientId(3))
        );
        assert_eq!(property_client(&PropertyId::ChannelName(ChannelId(4))), None);
        assert_eq!(
            property_channel(&PropertyId::ChannelTopic(ChannelId(4))),
            Some(ChannelId(4))
        );
        assert_eq!(
            property_channel(&PropertyId::OptionalChannelData(ChannelId(6))),
            Some(ChannelId(6))
        );
        assert_eq!(
            property_channel(&PropertyId::ChannelGroup(tsclientlib::ChannelGroupId(2))),
            None
        );
        assert_eq!(property_channel(&PropertyId::ClientName(ClientId(5))), None);
        assert!(is_server_property(&PropertyId::ServerName));
        assert!(is_group_property(&PropertyId::ServerGroup(ServerGroupId(1))));
        assert!(!is_server_property(&PropertyId::ServerGroup(ServerGroupId(1))));
    }

    #[test]
    fn updates_are_coalesced_per_client() {
        let client = |id: &str, name: &str| platform::Client {
            id: id.into(),
            name: name.into(),
            ..Default::default()
        };
        let events = vec![
            PlatformEvent::ClientUpdate {
                client: client("1", "a"),
            },
            PlatformEvent::ClientUpdate {
                client: client("2", "x"),
            },
            PlatformEvent::ClientEnter {
                client: client("3", "c"),
            },
            PlatformEvent::ClientUpdate {
                client: client("1", "b"),
            },
        ];
        let names: Vec<String> = coalesce(events)
            .into_iter()
            .map(|e| match e {
                PlatformEvent::ClientUpdate { client } | PlatformEvent::ClientEnter { client } => client.name,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(names, ["x", "c", "b"]);
    }

    #[test]
    fn siblings_follow_order_links() {
        // (id, id of the sibling above)
        let (a, b, c, orphan) = ((5u64, 0u64), (2, 5), (9, 2), (7, 99));
        let sorted = sort_siblings(&[&c, &orphan, &a, &b], |x| x.0, |x| x.1);
        assert_eq!(sorted.iter().map(|x| x.0).collect::<Vec<_>>(), [5, 2, 9, 7]);
    }
}
