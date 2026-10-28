# Installation

James runs on Linux (amd64). The Docker image contains everything: the bot, the web interface,
ffmpeg, yt-dlp, Node.js for plugins and Piper with a German and an English voice.

## Docker Compose (recommended)

Copy [`docker-compose.yml`](../docker-compose.yml) into an empty directory, then:

```sh
mkdir data && sudo chown 1001:1001 data   # the container runs as user 1001
docker compose up -d
docker compose logs james | grep setup_token
```

To run it as another user, set `user: "<uid>:<gid>"` in the compose file and give that user the
`data` directory instead.

Open `http://<server>:8087`, enter the setup token from the log and create the administrator
account. Then add a bot under **Instances → +**: the TeamSpeak server address, a nickname and
optionally a default channel.

The compose file also starts a [PO token provider](https://github.com/Brainicism/bgutil-ytdlp-pot-provider)
that YouTube requires on most server (datacenter) IP addresses.

Data (database, uploads, plugins and the bots' TeamSpeak identities) lives in `./data`. Back up
this directory; the TeamSpeak identities in it keep the bots' server groups.

### Behind a reverse proxy (HTTPS)

Serve the web interface via HTTPS and tell James its domain:

```yaml
    environment:
      JAMES_HTTP__ALLOWED_HOSTS: '["james.example.com"]'
      JAMES_HTTP__SECURE_COOKIES: "true"
      JAMES_HTTP__TRUSTED_PROXIES: '["172.18.0.1"]'   # address of the proxy as seen by James
```

WebSockets (`/api/v1/ws`, `/api/v1/instances/*/plugins/*/ws`) must be passed through.

### YouTube on server IPs

YouTube often blocks datacenter addresses ("Sign in to confirm you're not a bot"). In this order:

1. The PO token provider of the compose file (enabled by default).
2. Cookies of a YouTube account: export them in Netscape format (e.g. with a browser extension),
   put the file into `data/` and set `JAMES_YTDLP__COOKIES=/data/youtube-cookies.txt`. Use a
   separate account; the file grants access to it.
3. A residential proxy: `JAMES_YTDLP__PROXY=socks5://user:pass@host:1080`.

YouTube changes often and yt-dlp follows quickly. The published image is rebuilt every week with
the newest yt-dlp, so update regularly (see below). In between, an own yt-dlp binary can be used:
put it into `data/` and set `JAMES_YTDLP__BINARY=/data/yt-dlp`.

## Updating

```sh
docker compose pull && docker compose up -d
```

The release image is rebuilt weekly (newest yt-dlp), so this is worth running regularly, e.g. from
a cron job.

Database migrations run automatically.

## Without Docker

Requirements: Rust (stable), `cmake`, `libopus-dev`, `pkg-config`, `ffmpeg`, `yt-dlp`, Node.js 24+
(plugins, building the web interface), optionally Python 3 with `piper-tts` (text to speech) and
Deno (helps yt-dlp with YouTube).

```sh
git clone https://github.com/Hudint/James && cd James
cargo build --release -p james
(cd web && npm ci && npm run build)
cp james.example.toml james.toml   # adjust
./target/release/james --config james.toml
```

In `james.toml`, set `http.web_root = "web/build"` and `plugins.host = "plugin-host/src/main.ts"`
(relative to the working directory), and for text to speech the Python interpreter that has
`piper-tts` installed (see [Text to speech](tts.md)).

Run James as an unprivileged user, e.g. with a systemd unit:

```ini
[Unit]
Description=James voice bot
After=network-online.target

[Service]
User=james
WorkingDirectory=/opt/james
ExecStart=/opt/james/target/release/james --config /opt/james/james.toml
Restart=on-failure

[Install]
WantedBy=multi-user.target
```
