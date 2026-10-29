# Configuration

James reads `james.toml` (path: `--config` or `JAMES_CONFIG`). Every setting can also be set with an
environment variable: `JAMES_` + the path in upper case, with `__` between levels, e.g.
`JAMES_HTTP__BIND=0.0.0.0:8087` or `JAMES_TTS__PIPER__PYTHON=/opt/piper/bin/python`. Lists are
written as JSON: `JAMES_HTTP__ALLOWED_HOSTS='["james.example.com"]'`.

Instances (bots) are managed in the web interface and stored in the database; `[[instances]]` in the
file are imported once on the first start. [`james.example.toml`](../james.example.toml) shows a
complete file.

## General

| Key | Default | Meaning |
|---|---|---|
| `data_dir` | `data` | Database, uploads, plugins, plugin data, voices (relative paths below are relative to it where noted) |

## `[http]`

| Key | Default | Meaning |
|---|---|---|
| `bind` | `127.0.0.1:8087` | Address of the web interface and API |
| `web_root` | – | Directory of the built web interface (`web/build`); without it only the API is served |
| `secure_cookies` | `false` | Mark session cookies `Secure` (set when served via HTTPS) |
| `allowed_hosts` | `[]` | Host names the interface is reached under; others are refused (protects against DNS rebinding). Set it when James is reachable from other machines |
| `trusted_proxies` | `[]` | Reverse proxies whose `X-Forwarded-For` header is trusted (rate limits use the real client address) |

## `[log]`

| Key | Default | Meaning |
|---|---|---|
| `level` | `info` | Filter, e.g. `info,james_teamspeak=debug` |
| `format` | `pretty` | `pretty` or `json` |

## `[tools]`

| Key | Default | Meaning |
|---|---|---|
| `ffmpeg`, `ffprobe` | `ffmpeg`, `ffprobe` | Programs (name in `PATH` or path) |
| `max_concurrent` | `8` | yt-dlp/ffprobe processes running at once |
| `audio_threads` | `2` | Threads of the audio engine (one handles many bots) |

## `[media]`

| Key | Default | Meaning |
|---|---|---|
| `dirs` | `[]` | Additional music directories for the library; `<data_dir>/media` (uploads) is always included |

## `[ytdlp]`

| Key | Default | Meaning |
|---|---|---|
| `binary` | `yt-dlp` | yt-dlp program |
| `cookies` | – | Netscape cookie file; takes precedence over the cookies uploaded under **Settings** (see [Installation](installation.md#youtube-on-server-ips)) |
| `proxy` | – | Proxy for YouTube & co., e.g. `socks5://user:pass@host:1080` |
| `pot_provider` | – | URL of a bgutil PO token provider, e.g. `http://bgutil:4416` |
| `extra_args` | `[]` | Additional yt-dlp arguments |
| `format` | opus/webm audio first | yt-dlp format selection |
| `playlist_limit` | `500` | Entries taken from one playlist URL |
| `timeout_secs` | `60` | Limit for resolving one input |
| `search_fallback` | `true` | Treat plain text as a YouTube search |

## `[network]`

| Key | Default | Meaning |
|---|---|---|
| `allow_private_networks` | `false` | Allow streams and plugin requests to private/internal addresses. Off by default: users could otherwise make the bot fetch internal services (SSRF) |

## `[tts]`

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `true` | Text to speech on/off |
| `default_voice` | first voice | Voice used when an instance has none set |
| `piper.python` | `python3` | Python with the `piper-tts` package |
| `piper.voices_dir` | `voices` | Voice directory (relative to `data_dir`) |
| `piper.max_workers` | `3` | Voices kept loaded and texts synthesized at once (each voice needs ~100–200 MB of memory; the default voice may come on top) |
| `piper.idle_timeout_secs` | `600` | Unused voices are unloaded after this time (the default voice stays loaded) |

## `[plugins]`

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `true` | Plugins on/off |
| `node` | `node` | Node.js 24+ |
| `host` | `plugin-host/src/main.ts` | Entry of the plugin host (its directory must not contain the data directory) |
| `dir` | `<data_dir>/plugins` | Plugin directory |

## `[auth.oidc]`

Single sign-on, see [Single sign-on](sso.md).

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `false` | Show "Sign in with …" on the login page |
| `issuer` | – | Issuer URL of the identity provider |
| `client_id`, `client_secret` | – | Client registered at the provider |
| `redirect_url` | – | `https://<your domain>/api/v1/auth/oidc/callback`, registered at the provider |
| `scopes` | `["profile", "email"]` | Scopes besides `openid` |
| `display_name` | `SSO` | Name on the login button |
| `username_claim` | `preferred_username` | Claim for the user name of new accounts (falls back to e-mail and name) |
| `create_users` | `false` | Create accounts on the first login; otherwise only accounts linked by an administrator can sign in |
| `default_roles` | `[]` | Roles of accounts created by SSO (must exist) |
| `groups_claim` | `groups` | Claim with the user's groups |
| `allowed_groups` | `[]` | Only members of these groups (or of `admin_groups`) may sign in; empty = everybody at the provider |
| `admin_groups` | `[]` | Groups that make an account administrator (updated on every login; empty = not managed by SSO) |
| `allow_insecure_http` | `false` | Accept an `http://` issuer that is not on this machine (trusted networks only) |

## Instance settings

Set in the web interface (**Instance → Settings**) or via the API:

| Setting | Meaning |
|---|---|
| Address, nickname, server/channel password, default channel | TeamSpeak connection (TeamSpeak 3 and 6). While the bot is connected, the default channel is picked from a list; changing it (or the nickname) applies at once without reconnecting |
| Identity | The bot's TeamSpeak identity: created on the first start, or an existing one imported (keeps its server groups); administrators can show and download it (with its private key) to move the bot |
| Start volume, bitrate, loudness target | Audio: every track is normalised to the loudness target (EBU R128) |
| Command prefix | Chat command prefix (`!`) |
| Voice | Voice for text to speech |
| Now playing in description | Shows the current track in the bot's description |
| Start automatically | Connects when James starts |

On TeamSpeak the bot subscribes to every channel it may when it connects, as the TeamSpeak client
does, so the web interface and plugins see the clients in all of them (not only in the bot's own
channel). Channels whose occupants must stay hidden need a subscribe power the bot's server groups
do not have (`i_channel_needed_subscribe_power`).
