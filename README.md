# James

A modern, plugin-extensible voice bot for TeamSpeak 3 and TeamSpeak 6, inspired by SinusBot
(Discord is planned). One James process runs any number of bots; you control them from a web
interface, with chat commands in TeamSpeak, or through an HTTP API.

> **Status:** version 0.6. See [PLAN.md](PLAN.md) for scope, architecture and milestones.

## Features

- **Music:** YouTube, SoundCloud, Bandcamp and everything else [yt-dlp](https://github.com/yt-dlp/yt-dlp)
  supports, radio streams and a local library with uploads. Every track is normalised to the same
  loudness (EBU R128), so nobody reaches for the volume between songs.
- **Queue and playlists:** play now / next / later, reorder, shuffle, repeat, saved playlists.
- **Text to speech:** the bot reads texts out over the music, which is turned down meanwhile.
  [Piper](https://github.com/OHF-Voice/piper1-gpl) runs locally, so no cloud service is needed.
- **Plugins** in TypeScript/JavaScript with an API that covers everything SinusBot scripts can do,
  and SinusBot scripts run unchanged ([plugin guide](docs/plugins/README.md),
  [SinusBot compatibility](docs/plugins/sinusbot.md)).
- **Web interface** (English and German, light and dark, works on phones), **chat commands**, a
  **REST + WebSocket API**.
- **Accounts and roles** with permissions per bot. TeamSpeak users can link their identity, and
  TeamSpeak server groups can be mapped to roles. **Single sign-on** works with OpenID Connect
  (Keycloak, Authentik, Entra ID, …).
- **One Docker image** with everything included: ffmpeg, yt-dlp, Node.js, Piper and a German and
  an English voice.

## Getting started (Docker Compose)

You need a Linux server (amd64) with Docker and the Compose plugin, and a TeamSpeak 3 or 6 server
the bot may join.

**1. Create a directory and the compose file.**

```sh
mkdir james && cd james
```

Save this as `docker-compose.yml`. It is the same as [the one in this repository](docker-compose.yml):

```yaml
services:
  james:
    image: ghcr.io/hudint/james:latest
    restart: unless-stopped
    ports:
      - "8087:8087"
    volumes:
      - ./data:/data
    environment:
      JAMES_YTDLP__POT_PROVIDER: http://bgutil:4416
    depends_on:
      - bgutil
    # The start script only needs these to hand ./data to the james user and drop to it.
    cap_drop: [ALL]
    cap_add: [CHOWN, SETUID, SETGID, SETPCAP]
    security_opt: ["no-new-privileges:true"]
    logging:
      options: { max-size: 10m, max-file: "3" }

  # Generates the "proof of origin" tokens YouTube requires from most server IP addresses.
  bgutil:
    image: brainicism/bgutil-ytdlp-pot-provider:2.0.2
    restart: unless-stopped
```

> While the repository is private, log in to the registry first:
> `docker login ghcr.io -u <github user>` with a token that has the `read:packages` scope.
> You can also build the image yourself: clone the repository and run `docker compose build`.

**2. Start it and get the setup token.**

```sh
docker compose up -d
docker compose logs james | grep setup_token
```

**3. Create the administrator account.** Open `http://<server>:8087`, enter the setup token from the
log and choose a user name and password. The token works once and is not needed afterwards.

**4. Add a bot.** In the sidebar, click **+** next to *Instances* and enter:

- **Address** of the TeamSpeak server, e.g. `ts.example.com` or `ts.example.com:9987`
- **Nickname** of the bot
- optionally the **server password**, a **default channel** (`Lobby/Music` or its id) and the
  channel password

Save and start the instance. The bot joins the server; its TeamSpeak identity is created on the
first start. To keep the server groups of an existing bot (e.g. from SinusBot), paste its identity
under **Settings → Identity** of the instance.

**5. Play something.** Paste a YouTube link into *Add music*, or write to the bot in TeamSpeak:

```
!play https://www.youtube.com/watch?v=dQw4w9WgXcQ
!add never gonna give you up
!say Hello everyone!
!help
```

All commands and permissions are listed in [Using James](docs/usage.md).

## Configuration

Most settings (bots, users, roles, plugins, voices) are made in the web interface. Server-wide
settings come from **environment variables** in the compose file, or from **`data/james.toml`**
(optional; the image reads it if it exists). Both use the same names: `JAMES_` plus the setting's
path in upper case, with `__` between levels. Lists are written as JSON.

| Setting | Environment variable | Example |
|---|---|---|
| Host names of the web interface | `JAMES_HTTP__ALLOWED_HOSTS` | `'["james.example.com"]'` |
| Secure cookies (behind HTTPS) | `JAMES_HTTP__SECURE_COOKIES` | `"true"` |
| Trusted reverse proxies | `JAMES_HTTP__TRUSTED_PROXIES` | `'["172.18.0.1"]'` |
| Proxy for YouTube & co. | `JAMES_YTDLP__PROXY` | `socks5://host:1080` (credentials better in `james.toml`) |
| Default voice | `JAMES_TTS__DEFAULT_VOICE` | `en_US-lessac-medium` |
| Log level | `JAMES_LOG__LEVEL` | `info,james_teamspeak=debug` |

The same settings in `data/james.toml`:

```toml
[http]
allowed_hosts = ["james.example.com"]
secure_cookies = true

[tts]
default_voice = "en_US-lessac-medium"
```

`data/` belongs to the container's user (uid 1001) and is private, so create files there with
`sudo`, for example `sudo install -o 1001 -g 1001 -m 600 james.toml data/james.toml`. Restart
after changes: `docker compose up -d` (environment) or `docker compose restart james` (file). All settings are described in [Configuration](docs/configuration.md);
[`james.example.toml`](james.example.toml) is a complete example.

### HTTPS with a reverse proxy

Put James behind a reverse proxy with HTTPS before you use it over the internet. Then publish the
port on the host only (`"127.0.0.1:8087:8087"`), and set `allowed_hosts` and `secure_cookies` as
above. With [Caddy](https://caddyserver.com/), which obtains the certificate itself:

```
james.example.com {
    reverse_proxy 127.0.0.1:8087
}
```

With nginx, WebSockets must be passed through:

```nginx
location / {
    proxy_pass http://127.0.0.1:8087;
    proxy_http_version 1.1;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection "upgrade";
    proxy_set_header Host $host;
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    client_max_body_size 512m;   # library uploads
}
```

**Tell James the proxy's address.** Through Docker, all connections from a proxy on the host
arrive from the gateway of the compose network. Without this setting, James treats every visitor
as that one address, and a few wrong passwords from anyone would block logins for everybody.
Look the address up and set it:

```sh
docker inspect $(docker compose ps -q james) \
    --format '{{range .NetworkSettings.Networks}}{{.Gateway}}{{end}}'   # e.g. 172.18.0.1
```

```yaml
    ports:
      - "127.0.0.1:8087:8087"   # only the proxy may connect, it is trusted
    environment:
      JAMES_HTTP__TRUSTED_PROXIES: '["172.18.0.1"]'
```

Trust the gateway only together with the `127.0.0.1` port binding. Otherwise anybody reaching the
port directly could pretend to be any address. Docker may assign a different subnet when the
network is recreated; to keep the address fixed, give the network a subnet that is free on your
host (`networks: default: ipam: config: [{subnet: 172.30.87.0/24}]`, gateway `172.30.87.1`).
If the proxy runs in Docker itself, use the proxy container's address on the shared network
instead (give it a fixed `ipv4_address`).

### YouTube on servers

YouTube often blocks data-centre addresses ("Sign in to confirm you're not a bot"). The PO token
provider in the compose file is often not enough on its own. Then export the cookies of a separate
YouTube account in Netscape format (e.g. with the browser extension "Get cookies.txt LOCALLY") and
upload them under **Settings → YouTube cookies** (administrators); they apply from the next track
on. The file grants access to that account, so keep
it private. See [Installation](docs/installation.md#youtube-on-server-ips) for more options.

### More voices

The image contains `de_DE-thorsten-medium` and `en_US-lessac-medium`. Each bot can use its own
voice (**Instance → Settings → Voice**). [Text to speech](docs/tts.md) explains how to add any of
the 30+ other languages.

### Single sign-on

Add an `[auth.oidc]` section to `data/james.toml` and the login page shows **Sign in with …**.
[Single sign-on](docs/sso.md) has the steps and the security notes.

## Updating and backups

```sh
docker compose pull && docker compose up -d
```

Database migrations run automatically. The release image is rebuilt every week with the newest
yt-dlp (YouTube changes often), so update regularly, e.g. from a cron job.

James runs as user 1001 inside the container; on the first start it takes over the empty `data/`
directory Docker created. For commands inside the container use that user too:
`docker compose exec -u james james …`.
Everything worth keeping lives in `data/`: the database (accounts, bots, playlists), uploaded
music, plugins and the bots' TeamSpeak identities. Back up this directory; the identities keep
the bots' server groups on the TeamSpeak server.

## Without Docker

James also runs directly on Linux. You need Rust, Node.js 24+, ffmpeg, yt-dlp and, for text to
speech, Python with `piper-tts`. See [Installation](docs/installation.md#without-docker), which
includes a systemd unit.

## Documentation

- [Installation](docs/installation.md): Docker, reverse proxy, YouTube, without Docker
- [Configuration](docs/configuration.md): every setting
- [Using James](docs/usage.md): web interface, chat commands, roles and permissions
- [Text to speech](docs/tts.md): voices, limits
- [Single sign-on](docs/sso.md): OpenID Connect
- [HTTP API](docs/api.md): endpoints and events, for scripts and integrations
- [Plugins](docs/plugins/README.md): writing plugins, [SinusBot compatibility](docs/plugins/sinusbot.md)
- [Plugin API reference](https://hudint.github.io/James/): every type, method and event

## Development

Requirements: Rust (stable), `cmake`, `libopus-dev`, `pkg-config`, `ffmpeg`, Node.js 24+ (plugin
host and web interface).

```sh
(cd web && npm ci && npm run build)
JAMES_HTTP__WEB_ROOT=web/build cargo run -p james -- --config james.example.toml
```

Checks (CI runs the same):

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
(cd plugin-host && npm ci && npm run check && npm test)
(cd web && npm ci && npm run check && npm run build)
```

The live tests against real TeamSpeak servers use the official server images:
`JAMES_TS_TEST_ADDR=localhost:19987 cargo test -p james-teamspeak --test live`. The architecture
and the reasoning behind it are in [PLAN.md](PLAN.md).

## License

James is licensed under the [GNU AGPL v3](LICENSE) with a
[plugin exception](LICENSE-PLUGIN-EXCEPTION): plugins that only use the public plugin API may be
licensed freely. The plugin API package `@james/api` is MIT-licensed.
