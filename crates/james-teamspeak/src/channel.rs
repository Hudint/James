//! Channel create/edit packets from platform-neutral [`ChannelParams`].

use std::borrow::Cow;

use anyhow::{anyhow, bail, Context};
use james_core::platform::ChannelParams;
use tsclientlib::messages::c2s;
use tsclientlib::{ChannelId, Codec};
use tsproto_packets::packets::OutCommand;

fn codec(id: u8) -> anyhow::Result<Codec> {
    Ok(match id {
        0 => Codec::SpeexNarrowband,
        1 => Codec::SpeexWideband,
        2 => Codec::SpeexUltrawideband,
        3 => Codec::CeltMono,
        4 => Codec::OpusVoice,
        5 => Codec::OpusMusic,
        _ => bail!("unknown codec {id} (0-5)"),
    })
}

fn channel_id(id: &str) -> anyhow::Result<ChannelId> {
    Ok(ChannelId(
        id.parse().with_context(|| format!("invalid channel id {id}"))?,
    ))
}

/// `(limit, unlimited flag)` for a max-clients value; negative = unlimited.
fn limit(value: Option<i32>) -> (Option<i32>, Option<bool>) {
    match value {
        Some(n) if n < 0 => (None, Some(true)),
        Some(n) => (Some(n), Some(false)),
        None => (None, None),
    }
}

/// Common fields of create and edit, validated once.
struct Fields<'a> {
    codec: Option<Codec>,
    order: Option<ChannelId>,
    max_clients: (Option<i32>, Option<bool>),
    max_family_clients: (Option<i32>, Option<bool>),
    password: Option<Cow<'a, str>>,
    has_password: Option<bool>,
    delete_delay: Option<time::Duration>,
}

fn fields(params: &ChannelParams) -> anyhow::Result<Fields<'_>> {
    if params.permanent == Some(true) && params.semi_permanent == Some(true) {
        bail!("a channel cannot be permanent and semi-permanent at the same time");
    }
    if let Some(q) = params.codec_quality {
        if q > 10 {
            bail!("codec quality must be 0-10");
        }
    }
    Ok(Fields {
        codec: params.codec.map(codec).transpose()?,
        order: params.position.as_deref().map(channel_id).transpose()?,
        max_clients: limit(params.max_clients),
        max_family_clients: limit(params.max_family_clients),
        // An empty password removes it. TeamSpeak expects the hashed form, as clients send it.
        password: params
            .password
            .as_deref()
            .filter(|p| !p.is_empty())
            .map(|p| Cow::Owned(tsproto_types::crypto::encode_password(p.as_bytes()))),
        has_password: params.password.as_ref().map(|p| !p.is_empty()),
        delete_delay: params.delete_delay.map(|s| time::Duration::seconds(i64::from(s))),
    })
}

pub fn create(params: &ChannelParams) -> anyhow::Result<OutCommand> {
    let name = params
        .name
        .as_deref()
        .filter(|n| !n.trim().is_empty())
        .ok_or_else(|| anyhow!("a channel needs a name"))?;
    let f = fields(params)?;
    let part = c2s::OutChannelCreatePart {
        parent_id: params.parent.as_deref().map(channel_id).transpose()?,
        name: name.into(),
        topic: params.topic.as_deref().map(Cow::Borrowed),
        description: params.description.as_deref().map(Cow::Borrowed),
        password: f.password,
        codec: f.codec,
        codec_quality: params.codec_quality,
        max_clients: f.max_clients.0,
        max_family_clients: f.max_family_clients.0,
        order: f.order,
        has_password: f.has_password,
        is_unencrypted: params.encrypted.map(|e| !e),
        delete_delay: f.delete_delay,
        is_max_clients_unlimited: f.max_clients.1,
        is_max_family_clients_unlimited: f.max_family_clients.1,
        inherits_max_family_clients: None,
        phonetic_name: params.phonetic_name.as_deref().map(Cow::Borrowed),
        is_permanent: params.permanent,
        is_semi_permanent: params.semi_permanent,
        is_default: params.default,
    };
    Ok(c2s::OutChannelCreateMessage::new(&mut std::iter::once(part)))
}

pub fn edit(id: ChannelId, params: &ChannelParams) -> anyhow::Result<OutCommand> {
    let f = fields(params)?;
    let part = c2s::OutChannelEditPart {
        channel_id: id,
        order: f.order,
        name: params.name.as_deref().map(Cow::Borrowed),
        topic: params.topic.as_deref().map(Cow::Borrowed),
        is_default: params.default,
        has_password: f.has_password,
        password: f.password,
        is_permanent: params.permanent,
        is_semi_permanent: params.semi_permanent,
        codec: f.codec,
        codec_quality: params.codec_quality,
        needed_talk_power: params.needed_talk_power,
        max_clients: f.max_clients.0,
        max_family_clients: f.max_family_clients.0,
        codec_latency_factor: None,
        is_unencrypted: params.encrypted.map(|e| !e),
        delete_delay: f.delete_delay,
        is_max_clients_unlimited: f.max_clients.1,
        is_max_family_clients_unlimited: f.max_family_clients.1,
        inherits_max_family_clients: None,
        phonetic_name: params.phonetic_name.as_deref().map(Cow::Borrowed),
        description: params.description.as_deref().map(Cow::Borrowed),
    };
    Ok(c2s::OutChannelEditMessage::new(&mut std::iter::once(part)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_params() {
        assert!(create(&ChannelParams::default()).is_err(), "name required");
        let both = ChannelParams {
            name: Some("x".into()),
            permanent: Some(true),
            semi_permanent: Some(true),
            ..Default::default()
        };
        assert!(create(&both).is_err());
        let codec = ChannelParams {
            codec: Some(9),
            ..Default::default()
        };
        assert!(edit(ChannelId(1), &codec).is_err());
    }

    #[test]
    fn builds_create_packets() {
        let params = ChannelParams {
            name: Some("Music".into()),
            parent: Some("3".into()),
            max_clients: Some(-1),
            password: Some("pw".into()),
            codec: Some(5),
            ..Default::default()
        };
        let packet = create(&params).unwrap();
        let text = String::from_utf8_lossy(packet.0.content()).into_owned();
        assert!(text.starts_with("channelcreate"), "{text}");
        for part in [
            "channel_name=Music",
            "cpid=3",
            "channel_flag_maxclients_unlimited=1",
            "channel_codec=5",
        ] {
            assert!(text.contains(part), "{part} missing in {text}");
        }
        // The password is sent hashed (SHA-1, base64), never in plain text.
        assert!(
            text.contains("channel_password=GpHWL3ymc5liWkNopqtdSjuqYHM="),
            "{text}"
        );
        assert!(!text.contains("channel_password=pw"), "{text}");
    }
}
