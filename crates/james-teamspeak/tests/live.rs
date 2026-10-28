//! End-to-end tests against a real TeamSpeak server.
//!
//! Run with e.g. `JAMES_TS_TEST_ADDR=localhost:19987 cargo test -p james-teamspeak --test live`.
//! Skipped when the variable is not set.

use std::time::Duration;

use futures::prelude::*;
use james_audio::{AudioEngine, EngineConfig, EngineEvent, PlayRequest};
use james_core::platform::{
    ChannelParams, MessageTarget, PlatformAdapter, PlatformCommand, PlatformContext, PlatformEvent,
};
use james_core::source::MediaInput;
use james_teamspeak::{TeamSpeakAdapter, TeamSpeakConfig};
use tokio::sync::{mpsc, oneshot};
use tsclientlib::{Connection, DisconnectOptions, Identity, StreamItem};
use tsproto_packets::packets::AudioData;

fn server() -> Option<String> {
    std::env::var("JAMES_TS_TEST_ADDR").ok()
}

/// A plain tsclientlib client acting as a listener.
async fn listener(address: &str) -> Connection {
    let version = james_teamspeak::ClientVersion::default();
    let mut con = Connection::build(address.to_owned())
        .name("JamesTestListener")
        .identity(Identity::create())
        .version(tsclientlib::Version::Custom {
            platform: version.platform,
            version: version.version,
            signature: base64::Engine::decode(&base64::prelude::BASE64_STANDARD, version.signature).unwrap(),
        })
        .connect()
        .unwrap();
    con.events()
        .try_filter(|e| future::ready(matches!(e, StreamItem::BookEvents(_))))
        .next()
        .await
        .unwrap()
        .unwrap();
    con
}

fn sine_file(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("sine.flac");
    let status = std::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=3",
        ])
        .arg(&path)
        .status()
        .unwrap();
    assert!(status.success());
    path
}

/// Dominant frequency via zero crossings of the left channel.
fn estimate_frequency(samples: &[f32]) -> f32 {
    let left: Vec<f32> = samples.chunks_exact(2).map(|c| c[0]).collect();
    let crossings = left.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
    crossings as f32 / (left.len() as f32 / 48_000.0)
}

#[tokio::test]
async fn plays_audio_and_chats() {
    let Some(address) = server() else {
        eprintln!("JAMES_TS_TEST_ADDR not set, skipping");
        return;
    };

    let mut listener = listener(&address).await;
    let listener_id = listener.get_state().unwrap().own_client;

    // Bot: adapter + audio engine.
    let (events_tx, mut events) = mpsc::unbounded_channel();
    let (commands, commands_rx) = mpsc::channel(16);
    let (audio_tx, audio_rx) = mpsc::channel(64);
    let mut ctx = PlatformContext {
        events: events_tx,
        commands: commands_rx,
        audio: audio_rx,
    };
    let mut adapter = TeamSpeakAdapter::new(
        TeamSpeakConfig {
            address: address.clone(),
            nickname: "JamesTest".into(),
            ..Default::default()
        },
        None,
    )
    .unwrap();
    let bot = tokio::spawn(async move { adapter.run(&mut ctx).await });

    let bot_id = loop {
        match events.recv().await {
            Some(PlatformEvent::Connected { snapshot }) => break snapshot.own_client.unwrap(),
            Some(PlatformEvent::Disconnected { reason }) => {
                panic!("bot could not connect: {reason} (TS3 servers temporarily ban clients that reconnect too often)")
            }
            Some(_) => continue,
            None => panic!("bot connection task ended: {:?}", bot.await),
        }
    };

    // Wait until the listener sees the bot.
    let bot_ts_id = tsclientlib::ClientId(bot_id.parse().unwrap());
    while !listener.get_state().unwrap().clients.contains_key(&bot_ts_id) {
        tokio::time::timeout(Duration::from_secs(5), listener.events().next())
            .await
            .expect("bot not visible");
    }

    // Chat: listener -> bot (private message).
    {
        let state = listener.get_state().unwrap();
        let bot_client = &state.clients[&tsclientlib::ClientId(bot_id.parse().unwrap())];
        tsclientlib::OutCommandExt::send(bot_client.send_textmessage("!ping"), &mut listener).unwrap();
    }
    let mut got_message = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !got_message {
        let mut listener_events = listener.events();
        tokio::select! {
            // Only the listener's message (other bots on the server may greet the test bot).
            e = events.recv() => if let Some(PlatformEvent::Message { mode, from, text }) = e {
                if from.id != listener_id.0.to_string() {
                    continue;
                }
                assert_eq!(mode, MessageTarget::Client);
                assert_eq!(text, "!ping");
                got_message = true;
            },
            _ = listener_events.next() => {},
            _ = tokio::time::sleep_until(deadline) => panic!("bot did not receive the message"),
        }
    }

    // Chat: bot -> listener.
    let (reply, reply_rx) = oneshot::channel();
    commands
        .send(PlatformCommand::SendMessage {
            target: MessageTarget::Client,
            client: Some(listener_id.0.to_string()),
            text: "pong".into(),
            reply,
        })
        .await
        .unwrap();

    // Audio: play a 3 s sine through the engine.
    let dir = tempfile::tempdir().unwrap();
    let (engine_events_tx, mut engine_events) = mpsc::unbounded_channel();
    let engine = AudioEngine::spawn(
        EngineConfig {
            volume: 100,
            ..Default::default()
        },
        audio_tx,
        engine_events_tx,
    )
    .unwrap();
    engine.play(PlayRequest {
        id: 1,
        input: MediaInput::File(sine_file(dir.path())),
        start: Duration::ZERO,
        known_lufs: None,
    });

    let mut decoder =
        audiopus::coder::Decoder::new(audiopus::SampleRate::Hz48000, audiopus::Channels::Stereo).unwrap();
    let mut pcm = Vec::new();
    let mut packets = 0;
    let mut got_pong = false;
    let mut reply_rx = Some(reply_rx);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let mut listener_events = listener.events();
        let finished = tokio::select! {
            item = listener_events.next() => {
                match item.unwrap().unwrap() {
                    StreamItem::Audio(packet) => {
                        // Only the bot under test (other bots may play in the same channel).
                        if let AudioData::S2C { data, from, .. } = packet.data().data() {
                            if *from != bot_ts_id.0 {
                                continue;
                            }
                            if !data.is_empty() {
                                packets += 1;
                                let mut out = vec![0f32; 5760 * 2];
                                let n = decoder
                                    .decode_float(Some(audiopus::packet::Packet::try_from(*data).unwrap()),
                                        audiopus::MutSignals::try_from(&mut out[..]).unwrap(), false)
                                    .unwrap();
                                pcm.extend_from_slice(&out[..n * 2]);
                            }
                        }
                    }
                    StreamItem::BookEvents(events) => {
                        for e in events {
                            if let tsclientlib::events::Event::Message { message, .. } = e {
                                if message == "pong" { got_pong = true; }
                            }
                        }
                    }
                    _ => {}
                }
                false
            }
            r = async { reply_rx.as_mut().unwrap().await }, if reply_rx.is_some() => {
                r.unwrap().unwrap();
                reply_rx = None;
                false
            }
            e = engine_events.recv() => matches!(e, Some(EngineEvent::Finished { .. })),
            _ = tokio::time::sleep_until(deadline) => panic!("timeout"),
        };
        if finished {
            break;
        }
    }
    // Drain the remaining in-flight packets.
    let drain_until = tokio::time::Instant::now() + Duration::from_millis(500);
    while let Ok(Some(Ok(item))) = tokio::time::timeout_at(drain_until, listener.events().next()).await {
        if let StreamItem::Audio(packet) = item {
            // Only the bot under test (other bots may play in the same channel).
            if let AudioData::S2C { data, from, .. } = packet.data().data() {
                if *from != bot_ts_id.0 {
                    continue;
                }
                if !data.is_empty() {
                    packets += 1;
                }
            }
        }
    }

    assert!(got_pong, "listener did not receive the bot's message");
    assert!(packets >= 145, "expected ~150 audio packets, got {packets}");
    // Skip the first 0.5 s (normaliser convergence), analyse the rest.
    let steady = &pcm[48_000..];
    let frequency = estimate_frequency(steady);
    assert!((frequency - 440.0).abs() < 3.0, "frequency {frequency} Hz");
    let peak = steady.iter().fold(0f32, |m, s| m.max(s.abs()));
    assert!(peak < 0.95, "peak {peak} should be below the limiter ceiling");
    assert!(peak > 0.05, "peak {peak} too quiet");
    let rms = (steady.iter().map(|s| s * s).sum::<f32>() / steady.len() as f32).sqrt();
    eprintln!("received {packets} packets, {frequency:.1} Hz, peak {peak:.3}, rms {rms:.3}");

    drop(commands);
    let _ = bot.await.unwrap();
    listener.disconnect(DisconnectOptions::new()).unwrap();
    let _ = tokio::time::timeout(
        Duration::from_secs(2),
        listener.events().for_each(|_| future::ready(())),
    )
    .await;
}

/// Channel management, away status and client info requests against a real server.
#[tokio::test]
async fn manages_channels_and_status() {
    let Some(address) = server() else {
        eprintln!("JAMES_TS_TEST_ADDR not set, skipping");
        return;
    };
    let (events_tx, mut events) = mpsc::unbounded_channel();
    let (commands, commands_rx) = mpsc::channel(16);
    let (_audio_tx, audio_rx) = mpsc::channel(4);
    let mut ctx = PlatformContext {
        events: events_tx,
        commands: commands_rx,
        audio: audio_rx,
    };
    let mut adapter = TeamSpeakAdapter::new(
        TeamSpeakConfig {
            address,
            nickname: "JamesChannels".into(),
            ..Default::default()
        },
        None,
    )
    .unwrap();
    let bot = tokio::spawn(async move { adapter.run(&mut ctx).await });
    let own = loop {
        match events.recv().await {
            Some(PlatformEvent::Connected { snapshot }) => break snapshot.own_client.unwrap(),
            Some(PlatformEvent::Disconnected { reason }) => panic!("bot could not connect: {reason}"),
            Some(_) => continue,
            None => panic!("bot ended: {:?}", bot.await),
        }
    };

    macro_rules! run {
        ($make:expr) => {{
            let (reply, rx) = oneshot::channel();
            commands.send($make(reply)).await.unwrap();
            tokio::time::timeout(Duration::from_secs(5), rx)
                .await
                .expect("no answer")
                .unwrap()
        }};
    }
    /// Waits for a platform event matching `f`.
    async fn expect(
        events: &mut mpsc::UnboundedReceiver<PlatformEvent>,
        what: &str,
        f: impl Fn(&PlatformEvent) -> bool,
    ) -> PlatformEvent {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            match tokio::time::timeout_at(deadline, events.recv()).await {
                Ok(Some(e)) if f(&e) => return e,
                Ok(Some(_)) => continue,
                _ => panic!("no {what} event"),
            }
        }
    }

    let name = format!("james-test-{}", std::process::id());
    let created = run!(|reply| PlatformCommand::CreateChannel {
        params: ChannelParams {
            name: Some(name.clone()),
            topic: Some("created by a test".into()),
            max_clients: Some(3),
            ..Default::default()
        },
        reply,
    });
    match created {
        Ok(id) => {
            let event = expect(
                &mut events,
                "channel create",
                |e| matches!(e, PlatformEvent::ChannelCreate { channel, .. } if channel.name == name),
            )
            .await;
            let PlatformEvent::ChannelCreate { channel, .. } = event else {
                unreachable!()
            };
            assert_eq!(channel.id, id);
            assert_eq!(channel.max_clients, Some(3));

            run!(|reply| PlatformCommand::EditChannel {
                channel: id.clone(),
                params: ChannelParams {
                    topic: Some("edited".into()),
                    ..Default::default()
                },
                reply,
            })
            .unwrap();
            expect(&mut events, "channel update", |e| matches!(e, PlatformEvent::ChannelUpdate { channel, .. } if channel.id == id && channel.topic == "edited")).await;

            run!(|reply| PlatformCommand::DeleteChannel {
                channel: id.clone(),
                force: true,
                reply,
            })
            .unwrap();
            expect(
                &mut events,
                "channel delete",
                |e| matches!(e, PlatformEvent::ChannelDelete { id: deleted, .. } if *deleted == id),
            )
            .await;
        }
        // Servers can deny guests creating channels.
        Err(e) => eprintln!("channel creation refused by the server ({e}), skipping channel checks"),
    }

    run!(|reply| PlatformCommand::SetAway {
        message: Some("testing".into()),
        reply,
    })
    .unwrap();
    expect(&mut events, "away update", |e| matches!(e, PlatformEvent::ClientUpdate { client } if client.id == own && client.away.as_deref() == Some("testing"))).await;
    run!(|reply| PlatformCommand::SetAway { message: None, reply }).unwrap();
    expect(
        &mut events,
        "back update",
        |e| matches!(e, PlatformEvent::ClientUpdate { client } if client.id == own && client.away.is_none()),
    )
    .await;

    run!(|reply| PlatformCommand::RequestClientInfo {
        client: own.clone(),
        reply,
    })
    .unwrap();
    expect(&mut events, "connection info", |e| matches!(e, PlatformEvent::ClientUpdate { client } if client.id == own && client.connection.is_some())).await;

    drop(commands);
    let _ = bot.await.unwrap();
}
