# James – Project Plan

A modern, plugin-extensible voice bot for TeamSpeak 3/6 (and later Discord), inspired by SinusBot.
License: AGPL-3.0 **with a plugin exception** (plugins using only the public plugin API may use any
license; the plugin API package `@james/api` is MIT). Repository: private GitHub repo `James`.

## Goals

**First release (everything up to M6, no intermediate release):**
- High-quality music playback: YouTube & co. (yt-dlp), local library, radio/streams
- Text-to-speech: provider-based, Piper as default
- Plugins in TypeScript/JavaScript; the API covers **at least everything** of the
  [SinusBot scripting API](https://sinusbot.github.io/scripting-docs/), modernised (typed, async),
  documented at least as well, optional SinusBot compat layer
- Web interface, chat commands, REST + WebSocket API
- Multiple bot instances per process
- Accounts: local users + roles, TS identity linking, OIDC/SSO
- Docker deployment (amd64), tuned for VPS / datacenter hosting

**TeamSpeak targets:** the production server runs **TS3** (primary test target). **TS6 compatibility
is mandatory** for the first release (tested against a TS6 Docker server). Full TS6 feature parity
(incl. video/screen sharing) is a later goal, so platform adapters expose *capabilities* and the
media pipeline is designed so video tracks can be added without restructuring.

**Later (architecture prepared, not implemented):** TS6 video / feature parity, Discord (M7), Spotify (resolved
to other sources), plugin catalog, more platforms, Postgres, ARM64 image, multi-host scaling.

## Non-functional requirements

- **Scale:** start small (1–10 instances), designed for many (100+). Low per-instance footprint
  (target: < 30 MB RAM and < 2 % of one core per idle/playing instance excluding decoders), no
  per-instance Node process, instance runtime separable from controller/API so instances can later
  be distributed across processes/hosts.
- **Audio quality:** 48 kHz float pipeline, high-quality resampling, EBU R128 loudness
  normalisation, true-peak limiting (no clipping), gapless/crossfade, highest bitrate the
  channel/platform allows (TS: Opus Music; Discord: channel bitrate limit, max 384 kbit/s).
- **Robustness:** a crashing plugin or decoder never takes down the bot; auto-reconnect.

## Architecture

```
┌─────────────────────────── james (Rust binary) ───────────────────────────┐
│ Web UI (SvelteKit SPA, embedded) ◄── HTTP API (axum: REST + WS, OpenAPI)   │
│ Auth (argon2, roles/permissions, OIDC, identity linking) │ SQLite (sqlx)   │
│                                                                            │
│ Controller ── Instance Manager ── Instance runtime(s)                      │
│                                     ├─ Player (queue, playlists, volume)   │
│                                     ├─ Audio pipeline: decode → 48k f32 →  │
│                                     │  R128 normalise → mixer (music + TTS │
│                                     │  with ducking) → limiter → Opus      │
│                                     └─ Platform adapter (trait)            │
│                                          ├─ teamspeak (tsclientlib)        │
│                                          └─ discord   (later: songbird)    │
│ Registries (traits): SourceResolver · TtsProvider · SttProvider (later) ·  │
│                      Command · WebExtension                                │
└──────────────┬─────────────────────────────────────────────────────────────┘
               │ IPC (socket pair on fd 3, length-prefixed JSON, versioned)
┌──────────────▼──────────── plugin host (Node.js 24) ───────────────────────┐
│ One process per plugin (Node permission model), worker threads inside     │
│ TS executed natively (type stripping), full Node + npm + fetch             │
│ Manifest with declared permissions, confirmed by admin, enforced by the    │
│ bot API (trusted-code model, not a hard sandbox) · hot reload              │
│ `@james/api` (typed modern API) + `sinusbot-compat` layer                  │
└────────────────────────────────────────────────────────────────────────────┘
```

External tools in the image: ffmpeg, yt-dlp (+ bgutil PO-token provider), Piper (piper1-gpl) with
de/en voices, Node.js 24.

### Key technical decisions

| Area | Choice | Reason |
|---|---|---|
| Core | Rust | Native TS3 protocol, real-time audio, low footprint |
| TeamSpeak | tsclientlib (maintained, last commit 07/2026); fork if TS6 needs changes | Own client, no headless official client, no Xvfb |
| Discord (later) | serenity + songbird ≥ 0.6 (DAVE/E2EE supported via `davey`) | DAVE mandatory since 03/2026 |
| Plugins | One Node.js process per plugin (permission model), worker threads inside | fetch/TCP/UDP/WS/DB/npm out of the box; crash isolation; scales (one host, not one per instance) |
| YouTube on VPS | yt-dlp + PO-token provider + cookie import + optional proxy, self-update | Datacenter IPs are flagged by YouTube |
| API / Web | axum, utoipa (OpenAPI), WebSocket events; SvelteKit, i18n en/de | |
| DB | SQLite (sqlx, migrations), repository layer kept DB-agnostic | |
| Docs | VitePress site + TypeDoc for the plugin API | Guides, reference, examples per module/event |

### Plugin API (coverage of SinusBot modules)

Modern modules under `@james/api` (and SinusBot-style `require('event')` etc. via compat layer):
`event`, `engine` (logging, config, instances), `backend` (clients, channels, server groups,
messages, pokes, kicks, moves – platform-agnostic with platform-specific extensions), `media`
(playback, queue, playlists, yt-dlp, streams, library), `audio` (volume, mute, ducking;
speech recognition: `speech` event, done in 0.8 with Whisper), `tts`, `store` (persistent KV per plugin/instance/global), `db`,
`http`/`fetch`, `net` (TCP/UDP), `ws` (WebSocket channel to the web UI), `fs` (scoped), `crypto`,
`helpers`, `graphics`, `commands` (chat commands with arguments, permissions, help), `web`
(plugin settings form + custom web UI pages), `sources`/`tts` providers (plugins can register
their own). All SinusBot events (chat, poke, clientMove, connect/disconnect, track, trackEnd, ytdl,
api:*, public:*, talkerCount, …) get equivalents. A coverage table SinusBot ↔ James is part of
the docs and of the acceptance criteria.

## Milestones

1. **M0 – Foundation:** repo, Cargo workspace, CI, config, logging
2. **M1 – TeamSpeak + audio:** connect via tsclientlib, **TS6 compatibility spike first**,
   play a file; automated audio quality test (a second test client records, comparing spectrum/levels)
3. **M2 – Player & sources:** queue, playlists, yt-dlp (PO tokens, cookies), local library,
   streams, chat commands
4. **M3 – Persistence & API:** SQLite, REST/WS API, multi-instance, auth (local, roles,
   identity linking, OIDC)
5. **M4 – Web UI**
6. **M5 – Plugins:** Node plugin host, IPC protocol, full API, `.d.ts`, compat layer,
   examples, API docs, coverage table
7. **M6 – TTS:** provider trait, Piper, mixer/ducking, plugin access → **first release**
   (Docker image, docs, integration tests against TS3 + TS6 Docker servers)
8. **M7 – Discord adapter** (later)

## Risks

- **TS6 + tsclientlib:** unverified → spike at the start of M1. If it fails, adapt the protocol in a
  fork (may be significant effort).
- **YouTube blocks on datacenter IPs:** mitigated with a PO-token provider, cookies, proxy, and
  fast yt-dlp updates; never fully under our control.
- **Audio quality can only be verified by measurement during development;** final listening test by
  the user.
- **Scope:** the full release is large and will take many working sessions; progress is tracked
  per milestone in the repo.
