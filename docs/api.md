# HTTP API

Everything the web interface does goes through this API under `/api/v1`, so scripts and
integrations can do the same. Requests and responses are JSON.

## Authentication

Create a token under **Account → API tokens** and send it as `Authorization: Bearer <token>`. The
token has the permissions of its account (see [permissions](usage.md#users-roles-and-permissions)).
The web interface uses a session cookie instead; requests with that cookie must come from the same
site (cross-site requests are refused).

Errors are answered with a status code and `{"error": "…"}`. Expensive actions (resolving URLs,
searching) are rate-limited per account (`429`).

## Endpoints

| Method and path | Permission | Purpose |
|---|---|---|
| `GET /health` | – | Liveness (`{"status":"ok"}`) |
| `GET /setup`, `POST /setup` | setup token | First-run administrator account |
| `POST /auth/login`, `POST /auth/logout` | – | Password login (session cookie) / logout |
| `GET /auth/providers`, `GET /auth/oidc/login`, `GET /auth/oidc/callback` | – | [Single sign-on](sso.md) (browser redirects; failures return to `/login?error=<code>`) |
| `GET /me`, `POST /me/password` | account | Own account and permissions; password |
| `GET/POST /me/tokens`, `DELETE /me/tokens/{id}` | account | API tokens |
| `GET /me/identities`, `POST /me/identities/code`, `DELETE /me/identities/{platform}/{uid}` | account | Linked TeamSpeak identities |
| `GET /instances` | `instance.view` | Instances with their status |
| `POST /instances`, `PUT/DELETE /instances/{id}` | `instance.manage` | Create, change, delete |
| `POST /instances/{id}/start\|stop\|restart` | `instance.manage` | Connection |
| `GET /instances/{id}/identity` / `PUT …` `{identity}` | `instance.manage` / administrators | The bot's identity (unique id; never the key) / use an existing one (TeamSpeak `123V…` or base64 key); a running bot reconnects |
| `GET/PUT/DELETE /settings/youtube-cookies` (`PUT` `{content}`) | administrators | YouTube cookie file (status only, never the content) |
| `GET /instances/{id}/status` | `instance.view` | Player status (incl. `speech`: texts being spoken or waiting) |
| `POST /instances/{id}/play` `{input, mode}` | `playback.play` | Play a URL/search/path (`mode`: `now`, `next`, `append`) |
| `GET /instances/{id}/search?q=&limit=` | `playback.play` | Search (YouTube & co.) |
| `GET/POST/DELETE /instances/{id}/queue` | `playback.play` / `queue.manage` | Queue; add tracks; clear |
| `POST /instances/{id}/queue/move`, `DELETE /instances/{id}/queue/{item}`, `POST …/queue/{item}/play`, `POST …/queue/shuffle` | `queue.manage` / `playback.control` | Edit the queue |
| `POST /instances/{id}/skip\|previous\|stop-playback\|pause\|resume\|seek` | `playback.control` | Control playback |
| `PUT /instances/{id}/volume\|repeat\|shuffle` | `playback.volume` / `playback.control` | Settings |
| `GET /instances/{id}/server` | `instance.view` | Channels and clients |
| `POST /instances/{id}/move` `{channel}` | `server.move` | Move the bot |
| `POST /instances/{id}/message` | `server.message` | Chat as the bot |
| `POST /instances/{id}/say` `{text, voice?, language?}`, `POST …/say/stop` | `tts.say` | [Text to speech](tts.md): `202 {"id"}`, then `speechStarted`/`speechEnded` events; `429` when 16 texts wait |
| `GET /tts/voices` | account | Installed voices |
| `GET /library?q=&limit=&offset=`, `POST /library/upload`, `POST /library/scan` | `library.read` / `library.manage` | Local library |
| `GET/POST /playlists`, `GET/PATCH/DELETE /playlists/{id}`, `GET/PUT/POST /playlists/{id}/tracks`, `POST /playlists/{id}/play` | `playlist.read` / `playlist.manage` | Playlists |
| `GET/POST /users`, `GET/PATCH/DELETE /users/{id}`, `GET /users/{id}/identities` | `users.manage` | Accounts (`oidcSubject` and `isAdmin` only by administrators) |
| `GET/POST /roles`, `PUT/DELETE /roles/{name}`, `GET/PUT /group-roles/{platform}[/{group}]` | `users.manage` | Roles and server group mapping |
| `GET /plugins`, `PUT /plugins/{id}`, `POST /plugins/reload`, `GET /plugins/{id}/config`, `PUT /instances/{i}/plugins/{id}/config` | `plugins.manage` | [Plugins](plugins/README.md) |
| `POST /instances/{i}/plugins/{id}/token` | `plugins.use` | Token for a plugin's web page |
| `POST /instances/{i}/plugins/{id}/events/{name}`, `GET …/ws` | handler's permission / `plugins.use` | Plugin endpoints |
| `POST /public/instances/{i}/plugins/{id}/events/{name}` | – | Public plugin endpoints |

## Events (WebSocket)

`GET /api/v1/ws` (session cookie or bearer token) streams the events of all instances the account
may see, one JSON object per message:

```json
{ "instance": "music", "type": "trackStarted", "item": { "id": 3, "track": { "title": "…" } } }
```

| `type` | Data |
|---|---|
| `started`, `stopped` | Instance task started/stopped |
| `connected` / `disconnected` | `snapshot` (server, channels, clients) / `reason` |
| `platform` | `event`: a server event (`message`, `clientEnter`, `clientMove`, …); private messages need `server.chat.read` |
| `trackStarted`, `trackEnded` | `item` (and `error`) |
| `queueChanged` | – (fetch the queue) |
| `playbackChanged` | `status` |
| `speechStarted`, `speechEnded` | `id`, `text` / `error`, `stopped` |
| `notice` | `source`, `message` (e.g. from plugins) |

Example: skip the current track with `curl`:

```sh
curl -X POST -H "Authorization: Bearer $TOKEN" https://james.example.com/api/v1/instances/music/skip
```
