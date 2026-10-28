# James

A modern, plugin-extensible voice bot for TeamSpeak 3 / TeamSpeak 6 (Discord planned), inspired by SinusBot.

> **Status:** early development – see [PLAN.md](PLAN.md) for scope, architecture and milestones.

## Features (planned for the first release)

- High-quality music playback: YouTube & co. (yt-dlp), local library, radio/streams
- Text-to-speech (Piper by default, swappable providers)
- Plugins in TypeScript/JavaScript with an API covering everything SinusBot scripts can do,
  and SinusBot scripts running unchanged ([plugin guide](docs/plugins/README.md),
  [SinusBot compatibility](docs/plugins/sinusbot.md))
- Web interface, chat commands, REST + WebSocket API
- Multiple bot instances per process
- Local accounts, roles, TeamSpeak identity linking, OIDC/SSO

## Development

Requirements: Rust (stable), `cmake`, `libopus-dev`, `pkg-config`, `ffmpeg`, Node.js 24+ (plugin
host and web interface).

```sh
cargo run -p james -- --config james.example.toml
cargo test
(cd plugin-host && npm ci && npm run check && npm test)
(cd web && npm ci && npm run check && npm run build)
```

## License

James is licensed under the [GNU AGPL v3](LICENSE) with a
[plugin exception](LICENSE-PLUGIN-EXCEPTION): plugins that only use the public plugin API may be
licensed freely. The plugin API package `@james/api` is MIT-licensed.
