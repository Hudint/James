# SinusBot Scripting API — Complete Inventory

Purpose: reference for building a SinusBot-compatible plugin API for **James**.

## Sources

| Source | Notes |
|---|---|
| `github.com/SinusBot/scripting-docs` `master` @ `14eb970` (2021-10-07) — `api/*.js`, `api/interfaces/*.js`, `api/types/*.js` | Primary source. JSDoc stubs; the published HTML at https://sinusbot.github.io/scripting-docs/ is generated from this and matches it (anchors cross-checked). |
| `github.com/SinusBot/scripting-docs` `typescript` branch (2020-04-28) — `typings/**/*.d.ts` | Older hand-written typings (npm `sinusbot-scripting-engine`). Used for config-var schema and as a second opinion. Where it differs from JSDoc it is flagged with **[TS]**. |
| `github.com/SinusBot/docs` → `docs/scripts.md`, `docs/speechrecognition.md` | Manifest / `vars` guide. |
| `github.com/SinusBot/scripts` → `default-attached/*.js` (bundled scripts: welcome, followme, sinusbot-commands, …) | Real-world usage, used to settle ambiguities. |

Conventions in this doc:
- `?` after a param = optional. `T?` / `?T` return = may be `null`/`undefined`.
- **since** = first SinusBot version listed in the docs (none listed = pre-0.9.16 / original API).
- **perm** = string that must be present in manifest `requiredModules` for `require()` of that module to work.
- Almost every object accessor is a **method**, not a property (`client.name()`, not `client.name`). Exceptions are the plain-object event payload types (`Message`, `MoveInfo`, `ClientServerGroupEvent`, …) whose fields are **properties** (`ev.text`, `ev.client`).
- IDs are always **strings** (even when numeric on TS3).

---

## 0. Global scope (script runtime)

| Global | Signature | Notes |
|---|---|---|
| `registerPlugin` | `registerPlugin(manifest: Manifest, mainFunction: (_, config, meta) => void): void` | First and only top-level call a script should make. See §1. |
| `require` | `require(name: string): any` | Built-in modules (§2) by name: `engine, store, backend, media, audio, format, helpers, event, http, net, ws, db, fs, graphics, crypto`. Can also load **another script's exports** by script filename, e.g. `require('exportscript.js')` — must be done after the `load` event (see `engine.export`). Bundled 3rd-party lib is loaded via `require('command')` (Appendix A). |
| `module.exports` | `module.exports = {...}` | Since 1.0.0: standard way for a script to export an API to other scripts (replaces `engine.export`). |
| `setTimeout` | `(callback, timeout: number, ...args) => number` | **[TS]** typings only; standard timer. |
| `setInterval` | `(callback, interval: number, ...args) => number` | **[TS]** |
| `clearTimeout` | `(id: number) => void` | **[TS]** |
| `clearInterval` | `(id: number) => void` | **[TS]** |
| `console.log` | `(...args) => void` | **[TS]** declared as "a wrapper for engine.log" but commented out in `global.d.ts` — treat as unofficial. |
| Language | JavaScript ES5; "newer ES6 features" since 1.0 (arrow functions, template strings, `async`/`await`, `Promise` are used in bundled scripts). | |

---

## 1. Script manifest — `registerPlugin(manifest, mainFunction)`

### 1.1 `Manifest` fields

| Field | Type | Required | Meaning |
|---|---|---|---|
| `name` | `string` | yes | Short name of the script. |
| `version` | `string` | yes | Script version, e.g. `'1.0'`; increase each release. |
| `description` | `string` | yes | Longer description shown in web UI. |
| `author` | `string` | yes | Form `your name <your-email@example.com>`. |
| `autorun` | `boolean` | no | `true` = runs on every instance, user cannot disable it. |
| `backends` | `string[]` | no | Allowed backends. Default: **TS3 only**. Values: `"ts3"`, `"discord"`. Discord support must be declared explicitly: `backends: ["ts3", "discord"]`. |
| `enableWeb` | `boolean` | no | Script ships web content in `./scripts/<scriptname>/html/`; after restart the script title in the web UI becomes a link to that `index.html`. Pages can read the login token from `localStorage` (`window.localStorage.token`) and call the bot HTTP API (see `api:`/`public:` events, `ws`). **[TS]** spells it `enableweb` (lowercase) — JSDoc/docs say `enableWeb`. |
| `engine` | `string` | no | Required bot/engine version, semver range, e.g. `">= 0.9.16"`, `">= 1.0.0"`. |
| `hidden` | `boolean` | no | Hide script from settings page; use with `autorun`. Hidden scripts **cannot have `vars`**. |
| `requiredModules` | `string[]` | no | Protected modules the script needs. Mandatory for any protected module since 1.0. Script must be re-enabled by the user when this changes. Known values: `"http"`, `"net"`, `"ws"`, `"db"`, `"fs"`, `"graphics"`, plus `"discord-dangerous"` (needed for `ExtendedDiscord.rawCommand` in ≤ 1.0.0-beta.6). **[TS]** lists `"graphic"` (typo) instead of `"graphics"`. |
| `vars` | `ConfigEntry[]` | no | Config UI definition (§1.2). Values arrive in `config` param of `mainFunction`. |
| `voiceCommands` | `string[]` | no | Words/phrases for speech recognition (fires `speech` event). Licensed TS3 instances only; words must exist in `./speech/dict`. |

### 1.2 `vars` — config entry schema

Common keys (all entry types):

| Key | Type | Meaning |
|---|---|---|
| `name` | `string` | Key in the resulting `config` object. |
| `title` | `string` | Label shown in web UI. |
| `type` | `ConfigType` | One of the types below. |
| `default` | depends on type | Value used if nothing set. |
| `placeholder` | `string?` | Input placeholder text. |
| `indent` | `number?` | Visual indentation level in the UI **[TS]** (no further docs). |
| `conditions` | `{ field: string, value: any }[]?` | Show this field only when every referenced field `field` (by `name`) equals `value`. For `select` fields, `value` is the **option index**. Known bug (bundled scripts comment): conditions checking for `false` are buggy. |

Types:

| `type` | UI | Stored value / `default` type | Extra keys |
|---|---|---|---|
| `string` | single-line text input | `string` | — |
| `password` | like string, masked | `string` | — |
| `strings` | list of strings | `string[]` | — |
| `multiline` | textarea | `string` | — |
| `number` | numeric input | `number` | — |
| `track` | search/select one uploaded track | object `{ url: "track://uuid", title: "..." }` (docs; marked struck-through/uncertain in guide; **[TS]** says `string`) | — |
| `tracks` | select multiple tracks | array of track objects (assumed; **[TS]** says `string`) | — |
| `channel` | channel selector (when connected) | `string` channel ID | — |
| `select` | dropdown | **index** (number, may come as string) of selected option; **[TS]** types default as `string` | `options: string[]` (required) |
| `checkbox` | checkbox | `boolean` | — |
| `array` | user can add N items, each a sub-form | `object[]` (each item keyed by sub-var `name`) | `vars: ConfigEntry[]` (sub-fields) |

### 1.3 `mainFunction(_, config, meta)`

| Param | Type | Meaning |
|---|---|---|
| `_` | `object` / `null` | **Deprecated** (legacy `sinusbot` object). Ignore. |
| `config` | `object` | User-set values of `vars`, keyed by `name`. |
| `meta` | `Manifest` | The manifest passed to `registerPlugin`. |

Called when the script is activated (on load if enabled/autorun). Set up event listeners here.

---

## 2. Modules

Protected modules (need `requiredModules`): **http, net, ws, db, fs, graphics**. All others are freely usable.

### 2.1 `engine`

| Method | Signature → Return | Since | Semantics |
|---|---|---|---|
| `getInstanceID` | `() → string` | | Current instance's unique ID. |
| `getBotID` | `() → string` | | Current bot's unique ID. |
| `getBackend` | `() → string` | | Backend name: `"ts3"` or `"discord"`. |
| `setInstanceLogLevel` | `(level: number) → boolean` | | Set instance log level (0 none, 1 errors, 2 +warnings, 3 +info, 4 …, 10 most verbose, 11 + external backends). |
| `setBotLogLevel` | `(level: number) → boolean` | | Set bot log level (same scale). |
| `getInstanceLogLevel` | `() → number` | | |
| `getBotLogLevel` | `() → number` | | |
| `reloadScripts` | `() → boolean` | | Reload all scripts; requires corresponding config.ini setting. |
| `getNick` | `() → string` | | **Configured** nickname (actual: `backend.getNick()`). |
| `setNick` | `(nick: string) → boolean` | | Set nick and update on server. |
| `getDefaultChannelID` | `() → string` | | |
| `setDefaultChannelID` | `(channelID: string) → boolean` | | |
| `isRunning` | `() → boolean` | | Instance backend started. |
| `notify` | `(message: string) → void` | | Notification to all web-UI users (use for startup errors). |
| `saveConfig` | `(config: object) → boolean` | | Persist object as this script's config. (Stub signature has no param — param is documented.) |
| `log` | `(...something: any) → void` | | Log to stdout/instance log. Objects with method accessors print `{}`; Client prints `Client{ ID: <…>, Name: <…> }`; arrays of such print `[{},{}]`; numbers may print `<nil>` — stringify first. |
| `logf` | `(format: string, ...something: any) → void` | | Formatted log; format syntax undocumented (TODO in docs; presumably Go `fmt`). |
| `export` | `(obj: object) → void` | | Export object for other scripts (`require('<script>.js')` after `load`). Superseded by `module.exports` since 1.0.0. |
| `removeAvatar` | `() → boolean` | | Remove avatar. |
| `setAvatarFromTrack` | `(track: Track) → boolean` | | Avatar = track album art. |
| `setDefaultAvatar` | `() → boolean` | | Avatar = manually uploaded image. |
| `setAvatarFromBanner` | `(bannerName: string) → boolean` | 0.12.0 | Avatar = rendered banner template (see `graphics.setBanner`). |
| `setAvatarFromURL` | `(url: string) → boolean` | 0.14.0 | Avatar from image URL. |
| `getUsers` | `() → User[]` | 0.13.37 | All SinusBot (web) users. |
| `getUserById` | `(id: string) → User?` | 0.13.37 | **[TS]** spells it `getUserByID`. |
| `getUserByName` | `(name: string) → User?` | 0.13.37 | |
| `addUser` | `(username: string) → User?` | 1.0.0 | Create user; `undefined` if name exists. |
| `setCommandPrefix` | `(prefix: string) → void` | 0.14.0 | |
| `getCommandPrefix` | `() → string` | 0.14.0 | |
| `version` | `() → string` | 1.0.0 | e.g. `1.0.0-alpha.7-a20212b`. |
| `os` | `() → string` | 1.0.0 | e.g. `"linux"`. |
| `disableRegistration` | `() → void` | 1.0.0 | Disable `register` chat command. |
| `enableRegistration` | `() → void` | 1.0.0 | |
| `registrationEnabled` | `() → boolean` | 1.0.0-beta.6 | |
| `isSubscriptionMode` | `() → boolean` | | Is subscription mode on. |
| `getChannelSubscriptions` | `() → Channel[]` | | |
| `getClientSubscriptions` | `() → Client[]` | | |
| `setSubscriptionMode` | `(bool: boolean) → void` | 1.0.0-beta.8 | |
| `getIgnoreCommandsFromServerChat` | `() → boolean` | 1.0.0-beta.9 | Instance setting "ignore commands via server-chat". |
| `getIgnoreCommandsFromChannelChat` | `() → boolean` | 1.0.0-beta.9 | |
| `getIgnoreCommandsFromPrivateChat` | `() → boolean` | 1.0.0-beta.9 | |
| `setIgnoreCommandsFromServerChat` | `(bool: boolean) → void` | 1.0.0-beta.9 | |
| `setIgnoreCommandsFromChannelChat` | `(bool: boolean) → void` | 1.0.0-beta.9 | |
| `setIgnoreCommandsFromPrivateChat` | `(bool: boolean) → void` | 1.0.0-beta.9 | |
| `setChannelSubscription` | `(channel: Channel \| client: Client, bool: boolean) → void` | | **[TS] only**, not in JSDoc. |

`User.privileges()` bit flags (documented in User example): `LOGIN=1<<0, UPLOAD_FILES=1<<2, DELETE_FILES=1<<3, EDIT_FILES=1<<4, CREATE_AND_DELETE_PLAYLISTS=1<<5, EDIT_PLAYLISTS=1<<7, START_STOP=1<<8, EDIT_USERS=1<<9, PLAYBACK=1<<12, ENQUEUE=1<<13, SKIP_QUEUE=1<<14, ADMIN_QUEUE=1<<15, EDIT_BOT_SETTINGS=1<<16, EDIT_INSTANCES=1<<17`.

### 2.2 `store`

Three scopes, each JSON-serialisable values:
- **script** (default): this script only, **shared across instances**.
- **Global**: all scripts on all instances.
- **Instance**: this script on this instance only.

| Method | Signature → Return | Semantics |
|---|---|---|
| `set` | `(key: string, value: any) → boolean` | Script scope set. |
| `get` | `(key: string) → any` | `undefined` if missing. |
| `unset` | `(key: string) → void` | |
| `getKeys` | `() → string[]` | |
| `getAll` | `() → object` | key → value. |
| `setGlobal` | `(key, value) → boolean` | |
| `getGlobal` | `(key) → any` | |
| `unsetGlobal` | `(key) → void` | |
| `getKeysGlobal` | `() → string[]` | |
| `getAllGlobal` | `() → object` | |
| `setInstance` | `(key, value) → boolean` | |
| `getInstance` | `(key) → any` | |
| `unsetInstance` | `(key) → void` | |
| `getKeysInstance` | `() → string[]` | |
| `getAllInstance` | `() → object` | |

### 2.3 `backend`

| Method | Signature → Return | Since | Semantics |
|---|---|---|---|
| `connect` | `() → boolean` | | Connect to server. |
| `disconnect` | `() → boolean` | | Disconnect. |
| `isConnected` | `() → boolean` | | |
| `getBotClientID` | `() → string` | | Bot's client ID (on Discord it is a compound ID; bundled scripts use `getBotClientID().endsWith(user_id)`). |
| `getBotClient` | `() → Client` | | Bot's own Client. |
| `getNick` | `() → string` | | **Actual** nickname. |
| `getChannelByID` | `(id: string) → Channel` | | |
| `getCurrentChannel` | `() → Channel` | | (Primary) channel the bot is in. |
| `getChannelByName` | `(name: string) → Channel?` | | First match. |
| `getChannelsByName` | `(name: string) → Channel[]` | 0.14.0 | All matches. |
| `getChannelCount` | `() → number` | | |
| `getChannels` | `() → Channel[]` | | |
| `getClients` | `() → Client[]` | | All (visible) clients. |
| `getClientByID` | `(id: string) → Client` | | By temporary ID (changes on reconnect). |
| `getClientByName` | `(name: string) → Client` | | By nickname. |
| `getClientByNick` | `(name: string) → Client` | | Alias of `getClientByName`. |
| `getClientByUniqueID` | `(uniqueID: string) → Client` | | Online client by permanent UID. |
| `getClientByUID` | `(uniqueID: string) → Client` | | Alias of `getClientByUniqueID`. |
| `chat` | `(msg: string) → void` | | Message to **server** chat. |
| `createChannel` | `(channelParams: ChannelParams) → Channel` | 0.9.16.3 | Create channel. |
| `getServerGroupByID` | `(id: string) → ServerGroup` | | |
| `getChannelGroupByID` | `(id: string) → ChannelGroup` | | |
| `getServerGroups` | `() → ServerGroup[]` | | All known server groups (Discord: roles). |
| `getChannelGroups` | `() → ChannelGroup[]` | | |
| `extended` | `() → Extended` | 1.0.0-alpha.6 | Backend-specific API: `ExtendedTS3` or `ExtendedDiscord` (§3). |
| `setAway` | `(away: boolean, reason: string) → boolean` | | Set bot away (TeamSpeak). |

### 2.4 `media`

| Method | Signature → Return | Since | Semantics |
|---|---|---|---|
| `playURL` | `(url: string) → boolean` | | Stream URL or play internal `track://…` URL. |
| `getCurrentTrack` | `() → Track` | | |
| `getTrackByID` | `(id: string) → Track?` | | |
| `getTracks` | `() → Track[]` | 1.0.0-beta.3 | All tracks in library. |
| `search` | `(searchString: string) → Track[]` | | Max 20 results. |
| `enqueue` | `(url: string) → boolean` | | Append to queue. |
| `playAsNext` | `(url: string) → boolean` | 0.12.0 | Insert at queue head. |
| `playNext` | `() → boolean` | | Next track of queue/playlist. |
| `playPrevious` | `() → boolean` | | Previous track. |
| `stop` | `(trackID?: string) → boolean` | | Stop given track or all. |
| `getQueue` | `() → Track[]` | | |
| `getPlaylists` | `() → Playlist[]` | | |
| `getPlaylistByID` | `(id: string) → Playlist` | | |
| `getActivePlaylist` | `() → Playlist` | | (Stub has spurious `id` param.) |
| `removeFromQueue` | `(index: number) → boolean` | | 0-based. |
| `clearQueue` | `() → boolean` | | |
| `clearPlaylist` | `() → boolean` | | Unset current playlist so playback won't continue in it. |
| `yt` | `(url: string) → string` | | Play via youtube-dl; returns **jobId**. |
| `ytStream` | `(url: string) → boolean` | | Stream via youtube-dl. |
| `ytdl` | `(url: string, play: boolean) → string` | | Download via youtube-dl, optionally play; returns jobId. |
| `enqueueYt` | `(url: string) → string` | | Enqueue youtube-dl stream; jobId. |
| `enqueueYtdl` | `(url: string) → string` | | Download then enqueue; jobId. |
| `playQueueNext` | `() → void` | 1.0.0-beta.6 | Next queue track; resumes queue if stopped. |
| `resumeQueue` | `() → void` | 1.0.0-beta.6 | Alias of `playQueueNext`. |
| `clearIdleTrack` | `() → void` | 1.0.0-beta.6 | |
| `clearStartupTrack` | `() → void` | 1.0.0-beta.6 | |
| `setIdleTrackURL` | `(url: string) → void` | 1.0.0-beta.6 | |
| `setStartupTrackURL` | `(url: string) → void` | 1.0.0-beta.6 | |
| `playlistPlayByID` | `(playlist: Playlist, index: number) → void` | 1.0.0-beta.8 | Play playlist starting at track index. (Name says ByID but takes a Playlist object.) |

youtube-dl job results arrive via `ytdl.success` / `ytdl.error` events.

### 2.5 `audio`

| Method | Signature → Return | Since | Semantics |
|---|---|---|---|
| `setAudioFilter` | `(filter: string) → boolean` | | ffmpeg filter on output. **Documented as unsafe / crashes.** |
| `setAudioReturnChannel` | `(flags: number) → boolean` | 0.13.37 | Bitmask: `0x01` general return (recording), `0x02` separated audio (speech recognition), `0x03` both. |
| `getAudioReturnChannel` | `() → number` | 0.13.37 | |
| `startRecording` | `() → boolean` | | Record to file. |
| `stopRecording` | `() → boolean` | | |
| `streamToServer` | `(url: string, username: string, password: string) → boolean` | | Stream output to Icecast. |
| `stopStream` | `() → boolean` | | |
| `isRepeat` | `() → boolean` | | |
| `setRepeat` | `(val: boolean) → void` | | |
| `isShuffle` | `() → boolean` | | |
| `setShuffle` | `(val: boolean) → void` | | |
| `getVolume` | `() → number` | | 0–100. |
| `setVolume` | `(volume: number) → boolean` | | 0–100. |
| `getTrackPosition` | `() → number` | | Position of current track. Docs say **seconds**, but `Track.duration()` is ms — actual unit is very likely **milliseconds** (verify). |
| `seek` | `(pos: number) → void` | | Same unit caveat. |
| `isMute` | `() → boolean` | | |
| `setMute` | `(mute: boolean) → boolean` | | |
| `isPlaying` | `() → boolean` | | |
| `say` | `(text: string, locale?: string) → void` | | Text-to-speech playback. |
| `setStreamVolume` | `(streamID: string, volume: number) → boolean` | | Per-stream volume (by name/alias). |
| `setTTSURL` | `(url: string) → boolean` | 1.0.0-beta.6 | |
| `setTTSDefaultLocale` | `(locale: string) → boolean` | 1.0.0-beta.6 | |

### 2.6 `format`

Backend-aware text formatting (TS3 BBCode / Discord Markdown). All return `string`.

| Method | Signature |
|---|---|
| `color` | `(text: string, color: string /* hex, e.g. '#aa0000' */) → string` (no-op if backend unsupported, i.e. Discord) |
| `italic` | `(text: string) → string` |
| `bold` | `(text: string) → string` |
| `underline` | `(text: string) → string` |
| `code` | `(text: string) → string` |

### 2.7 `helpers`

| Method | Signature → Return | Since | Semantics |
|---|---|---|---|
| `getRandom` | `(max: number) → number` | | Random number in [0, max). |
| `getRandPerm` | `(max: number) → number[]` | | Random permutation of 0..max-1. |
| `newBytes` | `() → BytesWriter` | 1.0.0 | Empty writer. |
| `bytesFromString` | `(value: string) → BytesWriter` | 1.0.0 | |
| `bytesFromHex` | `(value: string) → BytesWriter` | 1.0.0 | |
| `bytesFromBase64` | `(value: string) → BytesWriter` | 1.0.0 | |
| `base64Encode` | `(input: string) → string` | | |
| `base64Decode` | `(input: string) → string` | | |
| `hexEncode` | `(input: string) → string` | | |
| `hexDecode` | `(input: string) → string` | | |
| `MD5Sum` | `(input: string) → string` | | hex. |
| `SHA1Sum` | `(input: string) → string` | | hex. |
| `SHA256Sum` | `(input: string) → string` | | hex. |
| `toString` | `(obj: any) → string` | | **Deprecated.** |
| `document` | `(name: string, obj: any) → string` | | Returns JSDoc string for an interface (introspection). |
| `typeOf` | `(obj: any) → string` | | Internal type name. **Private / deprecated.** |

### 2.8 `event`

| Method | Signature → Return | Semantics |
|---|---|---|
| `on` | `(eventName: string, callback: Function) → void` | Register listener (events in §4). For `api:`/`public:` the callback's return value is the HTTP response body. |
| `emit` | `(eventName: string, data: object) → void` | Emit custom event to **current instance** (other scripts' `on` receive it). |
| `broadcast` | `(eventName: string, data: object) → void` | Emit custom event to **all instances**. |

No `off`/`removeListener` exists.

### 2.9 `http` — perm `http` (since 0.14.2)

| Method | Signature → Return | Semantics |
|---|---|---|
| `simpleRequest` | `(config, callback: simpleRequestCallback) → void` | HTTP request. |

`config`: `{ method?: string /* GET, POST, … */, url: string, timeout?: number /* ms */, body?: string, headers?: object }`.
`simpleRequestCallback(error?: string, response?: { data: Bytes /* call .toString() */, headers: object, status: string, statusCode: number })`.

### 2.10 `net` — perm `net` (since 0.9.16)

| Method | Signature → Return | Semantics |
|---|---|---|
| `connect` | `(params: NetConnectParams, callback: netConnectCallback) → NetClient?` | Raw TCP/UDP/WebSocket client. `null` only for setup failure (bad params); connection errors go to callback. |

`NetConnectParams`: `{ host?: string, port?: number, url?: string /* WebSocket URL */, protocol?: "tcp" | "udp" | "ws" /* default "tcp" */ }`.
`netConnectCallback(error?: string)` — exactly one param on error.
`NetClient` interface: §3.

### 2.11 `ws` — perm `ws` (since 0.9.20)

WebSocket **server** exposed at `ws(s)://<host>/api/v1/b/<botId>/i/<instanceId>/ws`. Fires `ws.connect`, `ws.close`, `ws.error`, `ws.data` events (§4).

| Method | Signature → Return | Semantics |
|---|---|---|
| `write` | `(connectionId: string, messageType: number, message: string \| Bytes) → void` | Send to one connection. |
| `broadcast` | `(messageType: number, message: string \| Bytes) → void` | Send to all connections. |
| `close` | `(connectionId: string) → void` | Close connection. |

`messageType` is the WebSocket frame type (1 = text, 2 = binary per RFC 6455; inferred, not documented).

### 2.12 `db` — perm `db` (since 0.9.16.4)

| Method | Signature → Return | Semantics |
|---|---|---|
| `connect` | `(params: DBParams, callback: dbConnectCallback) → DBConn?` | `null` on failure. |

`DBParams`: `{ driver: "sqlite3" /* default; in-memory only */ | "mysql" | "postgres", host?: string /* required for mysql/postgres */, username?: string, password?: string, port?: number, database?: string /* used in example, not in typedef */ }`.
`dbConnectCallback(error?: string)`.

### 2.13 `fs` — perm `fs` (since 1.0.0)

| Method | Signature → Return | Semantics |
|---|---|---|
| `exists` | `(path: string) → boolean` | |
| `readFile` | `(path: string) → Bytes` | |
| `writeFile` | `(path: string, data: BytesWriter, mode: number) → boolean` | `mode` UNIX perms e.g. `0644`. |
| `stat` | `(path: string) → FileInfo` | |
| `readDir` | `(path: string) → FileInfo[]` | |
| `remove` | `(path: string) → boolean` | File or directory. |
| `mkDir` | `(path: string) → boolean` | |
| `mkDirAll` | `(path: string) → boolean` | Like `mkdir -p`. |
| `rename` | `(path: string, newPath: string) → boolean` | (Docs description wrongly says "Deletes".) |

### 2.14 `graphics` — perm `graphics` (since 0.14.2)

| Method | Signature → Return | Semantics |
|---|---|---|
| `removeBanner` | `() → boolean` | |
| `setBanner` | `(filename: string /* without extension */, bannerSettings: BannerSettings, callback: Function) → boolean` | Render banner image; then e.g. `engine.setAvatarFromBanner('banner.png')`. |

`BannerSettings`: `{ format: string /* e.g. "png" */, width: number, height: number, layers: BannerLayer[] }`. `BannerLayer` is **undocumented** (open issue SinusBot/scripting-docs#23).

### 2.15 `crypto` (since 1.0.0, not protected)

| Method | Signature → Return | Semantics |
|---|---|---|
| `generateKeypair` | `() → CryptoKeypair` | New keypair (likely Curve25519 / NaCl box). |
| `loadKeypair` | `(keypair: BytesWriter) → CryptoKeypair` | From exported bytes. |
| `seal` | `(secret: BytesWriter, message: BytesWriter) → BytesWriter` | Encrypt (likely NaCl secretbox). |
| `open` | `(secret: BytesWriter, message: BytesWriter) → BytesWriter` | Decrypt. |
| `pbkdf2` | `(hname: string /* hash name, e.g. "sha256" — undocumented */, password: BytesWriter, salt: BytesWriter, iterations: number, keylength: number /* bits */) → BytesWriter` | Key derivation. |
| `randomBytes` | `(number: number) → BytesWriter` | CSPRNG bytes. |

### 2.16 Modules that do NOT exist

There is **no** `interfaces` module, no `discord` module, no `console` module, no `Embed` type. Discord embeds are passed as raw Discord API JSON in `ExtendedDiscord.createMessage(channelId, { content, embed: {...} })` / `rawCommand`.

---

## 3. Interfaces / object types

All members are methods unless marked "prop".

### 3.1 `Client`
Note: if the client is invisible to the bot, some fields may be unavailable.

| Method | Signature → Return | Since | Semantics |
|---|---|---|---|
| `name` | `() → string` | | Nickname. |
| `nick` | `() → string` | | Alias of `name`. |
| `phoneticName` | `() → string` | | Phonetic nickname (TTS). |
| `id` | `() → string` | | Temporary ID. |
| `uid` | `() → string` | | Unique ID (TS3 identity / Discord user ID). |
| `uniqueId` | `() → string` | | **Deprecated** alias of `uid`. |
| `databaseID` | `() → string` | | TS3 database ID. |
| `databaseId` | `() → string` | | Alias. |
| `country` | `() → string` | | |
| `description` | `() → string` | | |
| `setDescription` | `(description: string) → boolean` | 0.9.19 | |
| `setDescriptionHex` | `(description: string) → boolean` | | Commented out in JSDoc; present in **[TS]**. |
| `isSelf` | `() → boolean` | | Is the bot itself. |
| `isRecording` | `() → string` (sic; effectively boolean) | | |
| `isMuted` | `() → boolean` | | Mic disabled. |
| `isDeaf` | `() → boolean` | | Speakers disabled. |
| `isAway` | `() → boolean` | | |
| `getServerGroups` | `() → ServerGroup[]` | | |
| `getChannelGroup` | `() → ChannelGroup` | | |
| `getAwayMessage` | `() → string` | | |
| `getPing` | `() → number` | | Latency. |
| `getIPAddress` | `() → string` | | If available. |
| `getOnlineTime` | `() → number` (ms) | | Needs special perms. |
| `getIdleTime` | `() → number` (ms) | | Needs special perms. |
| `getPacketLoss` | `() → number` | | Needs special perms. |
| `getBytesReceived` | `() → number` | | Needs special perms. |
| `getBytesSent` | `() → number` | | Needs special perms. |
| `getTotalConnections` | `() → number` | | TS3: requested lazily; ≤ 0 if not yet known. |
| `getCreationTime` | `() → number` | | First seen; TS3: lazily, ≤ 0 if unknown. |
| `getChannels` | `() → Channel[]` | | All channels client is in. |
| `getAudioChannel` | `() → Channel` | | Current audio channel. |
| `getURL` | `() → string` | | TS3 `client://0/uid~nickname`; Discord mention `<@uid>`. |
| `equals` | `(otherClient: Client) → boolean` | | |
| `chat` | `(msg: string) → boolean` | | Private message. |
| `poke` | `(msg: string) → void` | | Poke (TS3). |
| `ban` | `(time: number /* s, -1 permanent */, msg: string) → void` | | |
| `kick` | `(msg: string) → void` | | Kick from server. |
| `kickFromServer` | `(msg: string) → void` | | Same as `kick`. |
| `kickFromChannel` | `(msg: string) → void` | | |
| `addToServerGroup` | `(group: ServerGroup \| string \| number) → void` | | |
| `removeFromServerGroup` | `(group: ServerGroup \| string \| number) → void` | | |
| `moveTo` | `(target: Channel \| string, password?: string) → void` | | Move client. Discord: `backend.getBotClient().moveTo('')` leaves voice. |
| `subscribe` | `(val: boolean) → void` | | Subscription for this client (subscription mode). **[TS]** calls it `setSubscription(flag)`. |
| `getPlatform` | `() → string` | | Windows/Linux/MacOS. |
| `getVersion` | `() → string` | | Client version. |
| `requestConnectionInfoUpdate` | `() → boolean` | 1.0.0-beta.10 | Request fresh connection info → `clientConnectionInfoUpdate` event. |
| `type` | `() → string` | | Query = 0, Normal = 1. |
| `string` | `() → string` | | Private; `Client{ ID: <…>, Name: <…> }`. |

### 3.2 `Channel`

| Method | Signature → Return | Since | Semantics |
|---|---|---|---|
| `id` | `() → string` | | |
| `name` | `() → string` | | |
| `parent` | `() → Channel?` | | `null` if none. |
| `position` | `() → number` | 0.9.16.3 | TS3 sort order; change via `moveTo`. |
| `setPosition` | `(pos: number) → void` | | |
| `delete` | `() → boolean` | 0.9.17 | |
| `moveTo` | `(parent: string \| Channel, order: number) → void` | 0.9.16.3 | New parent + order. |
| `setName` | `(name: string) → void` | 0.9.16 | |
| `type` | `() → number` | | 0 = voice, 1 = text. |
| `topic` | `() → string` | | |
| `setTopic` | `(topic: string) → void` | 0.9.16 | |
| `description` | `() → string` | 0.9.19 | |
| `setDescription` | `(description: string) → void` | 0.9.16 | |
| `codec` | `() → number` | | |
| `setCodec` | `(codec: number) → void` | 0.9.16 | |
| `codecQuality` | `() → number` | | |
| `setCodecQuality` | `(quality: number) → void` | 0.9.16 | |
| `maxClients` | `() → number` | | -1 unlimited. |
| `setMaxClients` | `(maxClients: number) → void` | 0.9.16 | |
| `maxFamilyClients` | `() → number` | | |
| `setMaxFamilyClients` | `(maxFamilyClients: number) → void` | 0.9.16 | |
| `isPermanent` | `() → boolean` | | |
| `setPermanent` | `(permanent: boolean) → void` | 0.9.16 | |
| `isSemiPermanent` | `() → boolean` | | |
| `setSemiPermanent` | `(permanent: boolean) → void` | 0.9.16 | |
| `isDefault` | `() → boolean` | | |
| `setDefault` | `(bool: boolean) → void` | | |
| `isPassworded` | `() → boolean` | | |
| `setPassword` | `(password: string) → void` | | |
| `isEncrypted` | `() → boolean` | | |
| `setEncrypted` | `(encrypted: boolean) → void` | 0.9.16 | |
| `equals` | `(otherChannel: Channel) → boolean` | | |
| `chat` | `(msg: string) → boolean` | | Channel message. |
| `getClients` | `() → Client[]` | | |
| `getClientCount` | `() → number` | | |
| `subscribe` | `(val: boolean) → void` | | Subscription mode. **[TS]** `setSubscription(flag)`. |
| `update` | `(channelParams: ChannelParams) → void` | 0.9.16.3 | Bulk update; `parent` ignored. |
| `setChannelGroup` | `(client: Client, channelGroup: ChannelGroup) → void` | 0.9.18 | |
| `getPermissions` | `() → Permission[]` | 0.13.37 | Uncached server call. |
| `addPermission` | `(id: string) → Permission` | 0.13.37 | id or name (e.g. `i_channel_needed_join_power`); then setters + `save()`. |
| `getURL` | `() → string` | | Channel URL / Discord mention. |
| `getMessages` | `(params: object, callback: (error, messages) => void) → boolean` | 1.0.0-beta.2 | **Discord.** `params` per Discord "Get Channel Messages", e.g. `{ around: 'messageid', limit: '1' }`. |
| `string` | `() → string` | | Private; `Channel{ ID: <…>, Name: <…> }`. |

### 3.3 `User` (SinusBot web-interface user)

| Method | Signature → Return | Since |
|---|---|---|
| `id` | `() → string` | 0.13.37 |
| `name` | `() → string` | 0.13.37 |
| `privileges` | `() → number` (bitmask, see §2.1) | 0.13.37 |
| `instancePrivileges` | `() → number` | 1.0.0-beta.9 |
| `tsUid` | `() → string` — bound TS/Discord UID | 0.13.37 |
| `uid` | `() → string` — same as `tsUid` | 1.0.0-alpha.6 |
| `tsGroupId` | `() → string` — bound TS group / Discord role | 0.13.37 |
| `groupId` | `() → string` — same as `tsGroupId` | 1.0.0-alpha.6 |
| `isAdmin` | `() → boolean` | 0.13.37 |
| `setPassword` | `(password: string) → boolean` | 0.13.37 |
| `setTSUid` | `(uid: string) → boolean` | 0.13.37 |
| `setUid` | `(uid: string) → boolean` | 1.0.0-beta.9 |
| `setGroupId` | `(groupId: string) → boolean` | 1.0.0-alpha.6 |
| `setPrivileges` | `(privileges: number) → boolean` | 0.13.37 |
| `addPrivilege` | `(privilege: number) → boolean` | 0.13.37 |
| `removePrivilege` | `(privilege: number) → boolean` | 0.13.37 |
| `setInstancePrivileges` | `(privileges: number) → boolean` | 1.0.0-beta.9 |
| `addInstancePrivilege` | `(privilege: number) → boolean` | 1.0.0-beta.9 |
| `removeInstancePrivilege` | `(privilege: number) → boolean` | 1.0.0-beta.9 |
| `delete` | `() → boolean` | 0.13.37 |

### 3.4 `Permission` (since 0.13.37; mainly TS3; for channel, channel group, server group)

| Method | Signature → Return | Semantics |
|---|---|---|
| `id` | `() → string` | |
| `name` | `() → string` | |
| `value` | `() → number` | |
| `skip` | `() → boolean` | Server groups only. |
| `negated` | `() → boolean` | Server groups only. |
| `setNegated` | `(value: boolean) → boolean` | Needs `save()`. |
| `setSkip` | `(value: boolean) → boolean` | Needs `save()`. |
| `setValue` | `(value: number) → boolean` | Needs `save()`. (Docs descriptions of setNegated/setValue are swapped — names are authoritative.) |
| `save` | `() → boolean` | Apply. |
| `delete` | `() → boolean` | Remove permission. |

### 3.5 `ServerGroup`

| Method | Signature → Return | Since |
|---|---|---|
| `id` | `() → string` | |
| `name` | `() → string` | |
| `icon` | `() → string` (icon ID) | 0.12.0 |
| `addClientByDatabaseId` | `(client: Client \| string \| number) → boolean` | 0.13.37 |
| `removeClientByDatabaseId` | `(client: Client \| string \| number) → boolean` | 0.13.37 |
| `getPermissions` | `() → Permission[]` (uncached) | 0.13.37 |
| `addPermission` | `(id: string) → Permission` | 0.13.37 |

### 3.6 `ChannelGroup`

| Method | Signature → Return | Since |
|---|---|---|
| `id` | `() → string` | |
| `name` | `() → string` | |
| `icon` | `() → string` | 0.12.0 |
| `getPermissions` | `() → Permission[]` | 0.13.37 |
| `addPermission` | `(id: string) → Permission` | 0.13.37 |

### 3.7 `Track`

| Method | Signature → Return | Since | Semantics |
|---|---|---|---|
| `id` | `() → string` | | |
| `url` | `() → string` | | Unique internal URL (`track://…`). |
| `type` | `() → string` | | File type. |
| `title` | `() → string` | | |
| `artist` | `() → string` | | |
| `tempTitle` | `() → string` | | Temporary (radio stream) title. |
| `tempArtist` | `() → string` | | |
| `album` | `() → string` | | |
| `genre` | `() → string` | 0.9.16 | |
| `duration` | `() → number` (ms) | 0.9.16 | |
| `trackNumber` | `() → number` | 0.9.16 | |
| `thumbnail` | `() → string` | | Path to thumbnail. |
| `filename` | `() → string` | | Original filename. |
| `play` | `() → boolean` | | |
| `enqueue` | `() → boolean` | | |
| `addNext` | `() → boolean` | | Insert at queue head. |
| `setThumbnailFromURL` | `(url: string) → void` | | Download & store thumbnail. **[TS]** spells `setThumbnailFromUrl`. |
| `removeThumbnail` | `() → void` | | |
| `sourceURL` | `() → string` | 1.0.0-beta.2 | Original source URL. |
| `string` | `() → string` | | Private; `Track { <Artist> - <Title> }`. |

### 3.8 `Playlist`

| Method | Signature → Return | Semantics |
|---|---|---|
| `id` | `() → string` | |
| `name` | `() → string` | |
| `getTracks` | `() → PlaylistTrack[]` | |
| `setActive` | `() → boolean` | Continue playback from this playlist. |

### 3.9 `PlaylistTrack`

| Method | Signature → Return |
|---|---|
| `title` | `() → string` |
| `artist` | `() → string` |
| `album` | `() → string` |
| `url` | `() → string` (internal or external) |
| `play` | `() → boolean` |

(Commented out / not exposed in docs: `hasTitle`, `hasAlbum`, `hasArtist`, `enqueue`, `playNext`.)

### 3.10 `ApiEvent` (payload of `api:*` / `public:*`)

| Method | Signature → Return | Since |
|---|---|---|
| `name` | `() → string` | |
| `data` | `() → object` (parsed JSON body) | |
| `user` | `() → User?` (null for unauthenticated) | |
| `remoteAddr` | `() → string` | |
| `queryParams` | `() → object` (`{[k]: string}`) | 0.14 |

### 3.11 `Bytes` (reader, since 0.9.16)

Sequential reader with cursor.

| Method | Signature → Return |
|---|---|
| `bytes` | `() → number[]` (uint8 array) |
| `string` | `(length: number) → string` |
| `cString` | `() → string` (null-terminated) |
| `int16be` | `() → number` |
| `int16le` | `() → number` |
| `int32be` | `() → number` |
| `int32le` | `() → number` |
| `length` | `() → number` |
| `pos` | `() → number` |
| `seek` | `(pos: number) → boolean` |
| `toBase64` | `() → string` |
| `toHex` | `() → string` |
| `toString` | `() → string` |
| `uint8` | `() → number` |
| `uint16be` | `() → number` |
| `uint16le` | `() → number` |
| `uint32be` | `() → number` |
| `uint32le` | `() → number` |

### 3.12 `BytesWriter` (since 0.9.16)

| Method | Signature → Return |
|---|---|
| `append` | `(bytes: Bytes) → number` |
| `bytes` | `() → Bytes` |
| `length` | `() → number` |
| `newReader` | `() → Bytes` |
| `toBase64` | `() → string` |
| `toHex` | `() → string` |
| `toString` | `() → string` |
| `writeString` | `(str: string) → void` |
| `writeCString` | `(str: string) → void` |
| `writeInt8` | `(int8: number) → void` |
| `writeInt16be` / `writeInt16le` | `(int16: number) → void` |
| `writeInt32be` / `writeInt32le` | `(int32: number) → void` |
| `writeInt64be` / `writeInt64le` | `(int64: number) → void` |
| `writeUint8` | `(uint8: number) → void` |
| `writeUint16be` / `writeUint16le` | `(uint16: number) → void` |
| `writeUint32be` / `writeUint32le` | `(uint32: number) → void` |
| `writeUint64be` / `writeUint64le` | `(uint64: number) → void` |

(23 methods total.)

### 3.13 `NetClient` (from `net.connect`, since 0.9.16)

| Method | Signature → Return | Semantics |
|---|---|---|
| `write` | `(bytes: string \| Bytes \| number[], format?: "hex" \| "base64") → void` | Send; if `format` given, data is decoded first. |
| `on` | `(event: "data" \| "close" \| "error", callback) → void` | |
| `close` | `() → void` | |

NetClient events: `data` → `(data: Bytes)`; `close` → `()`; `error` → `(error: string)`.

### 3.14 `DBConn` (from `db.connect`, since 0.9.16.4)

| Method | Signature → Return | Semantics |
|---|---|---|
| `query` | `(queryString: string, ...parameters: any, callback: dbQueryCallback) → void` | Expect result set. Placeholders `?` (mysql). **Strings come back as byte arrays** (number[]) — must be converted. |
| `exec` | `(queryString: string, ...parameters: any, callback?: dbQueryCallback) → void` | No result expected. |

`dbQueryCallback(error?: string, result?: object[])` — mutually exclusive; rows are objects keyed by column name. Parameters are variadic (example: `dbc.exec("INSERT … VALUES (?, ?)", 'bar', 'bar2')`).

### 3.15 `FileInfo` (from `fs.stat`/`readDir`, since 1.0.0)

| Method | Signature → Return |
|---|---|
| `name` | `() → string` (base name) |
| `size` | `() → string` (sic; bytes — likely number in practice) |
| `mode` | `() → number` (UNIX mode) |
| `modTime` | `() → Date` |
| `isDir` | `() → boolean` |

### 3.16 `CryptoKeypair` (since 1.0.0)

| Method | Signature → Return | Semantics |
|---|---|---|
| `export` | `() → BytesWriter` | Keypair bytes (for `crypto.loadKeypair`). |
| `sharedKey` | `(something: BytesWriter) → BytesWriter` | Undocumented; presumably DH shared key with other party's public key. |

### 3.17 `DiscordMessage` (payload of `message` event; Discord only)

| Method | Signature → Return | Since | Semantics |
|---|---|---|---|
| `author` | `() → Client` | | |
| `authorID` | `() → string` | | |
| `channel` | `() → Channel` | | |
| `channelID` | `() → string` | | |
| `content` | `() → string` | | |
| `createReaction` | `(emoji: string, callback?: Function) → boolean` | | Emoji `name:id` or unicode. Needs READ_MESSAGE_HISTORY (+ADD_REACTIONS). |
| `delete` | `(callback?: Function) → boolean` | | |
| `deleteAllReactions` | `(callback?: Function) → boolean` | | Needs MANAGE_MESSAGES. |
| `deleteOwnReaction` | `(emoji: string, callback?: Function) → boolean` | | |
| `deleteUserReaction` | `(emoji: string, user: Client \| string, callback?: Function) → boolean` | 1.0.0-beta.2 | |
| `editContent` | `(content: string, callback?: Function) → boolean` | | |
| `guildID` | `() → string` | | |
| `ID` | `() → string` | | Note uppercase `ID`. |
| `isTTS` | `() → boolean` | | |
| `reply` | `(text: string, callback?: Function) → boolean` | | Post in same channel. |
| `timestamp` | `() → Date` | | |
| `timestampEdited` | `() → Date` | | Equals `timestamp()` if never edited. |

Callbacks are Node-style `(err, result)`.

### 3.18 `Extended` (from `backend.extended()`, since 1.0.0-alpha.6)
Either `ExtendedTS3` or `ExtendedDiscord`, depending on `engine.getBackend()`.

#### `ExtendedTS3`

| Method | Signature → Return | Semantics |
|---|---|---|
| `getServerInfo` | `() → TeamSpeakServerInfo` | |
| `requestExtendedServerInfo` | `() → boolean` | Async; result via `serverinfo_int` event. |

#### `ExtendedDiscord` (callbacks `(err, object)`; data = raw Discord REST JSON)

| Method | Signature → Return | Semantics |
|---|---|---|
| `getGuild` | `(guildId: string, callback) → void` | Guild info. |
| `modifyGuild` | `(guildId: string, guildObject: object, callback?) → void` | |
| `setStatus` | `(status: { game?: { name?: string, type?: 0\|1\|2 /* game/streaming/listening */, url?: string /* twitch only */ }, status: "online"\|"dnd"\|"idle"\|"invisible"\|"offline", afk: boolean, since?: number /* ms */ }) → void` | Presence update. |
| `getChannel` | `(channelId: string, callback) → void` | Raw channel object. |
| `modifyChannel` | `(channelId: string, data: object, callback?) → void` | Needs MANAGE_CHANNELS. |
| `deleteChannel` | `(channelId: string, callback?) → void` | Delete channel / close DM. |
| `getChannelMessages` | `(channelId: string, callback) → void` | |
| `getChannelMessage` | `(channelId: string, callback) → void` | Docs omit the message-ID param (likely `(channelId, messageId, callback)` in reality — verify). |
| `createMessage` | `(channelId: string, data: object /* {content, tts, embed, …} */, callback?) → void` | Post message (embeds go here). |
| `rawCommand` | `(method: string, path: string, data: object, callback?) → void` | Arbitrary Discord REST call. Required `requiredModules: ['discord-dangerous']` in ≤ 1.0.0-beta.6. |

### 3.19 `TeamSpeakServerInfo` (since 1.0.0-alpha.6)

| Method | Return | Notes |
|---|---|---|
| `name` | `string` | |
| `uid` | `string` | |
| `welcomeMessage` | `string` | |
| `platform` | `number` | |
| `version` | `number` | Broken: always 0. |
| `hostMessage` | `string` | |
| `hostBannerURL` | `string` | |
| `hostBannerGFXURL` | `string` | |
| `hostBannerGFXInterval` | `number` (s) | |
| `hostButtonURL` | `string` | |
| `hostButtonGFXURL` | `string` | |
| `namePhonetic` | `string` | |
| `serverID` | `number` | |
| `iconID` | `number` | |
| `reservedSlots` | `number` | |
| `defaultServerGroup` | `number` | |
| `defaultChannelGroup` | `number` | |
| `defaultChannelAdminGroup` | `number` | |
| `codecEncryptionMode` | `number` | perChannel=0, GlobalOff=1, GlobalOn=2. |
| `created` | `number` | Timestamp. |
| `asObject` | `object` | Raw JSON-able. |

### 3.20 `TeamSpeakExtendedServerInfo` (since 1.0.0-alpha.6; payload of `serverinfo_int`)

All methods `() → number` unless noted: `maxClients`, `clientsOnline`, `channelsOnline`, `uptime` (s), `complainAutobanCount`, `complainAutobanTime`, `complainRemoveTime`, `minClientsInChannelBeforeForcedSilence`, `antifloodPointsTickReduce`, `antifloodPointsNeededCommandBlock`, `antifloodPointsNeededIPBlock`, `clientConnections`, `queryClientConnections`, `queryClientsOnline`, `port`, `autostart`, `machineID` (→ `string`), `neededIdentitySecurityLevel`, `logClient`, `logQuery`, `logChannel`, `logPermissions`, `logServer`, `logFiletransfer`, `minClientVersion`, `maxDownloadTotalBandwidth`, `maxUploadTotalBandwidth`, `downloadQuota`, `uploadQuota`, `monthBytesDownloaded`, `monthBytesUploaded`, `totalBytesDownloaded`, `totalBytesUploaded`, `asObject` (→ `object`). (34 methods.)

### 3.21 Plain-object types (properties, not methods)

**`Message`** (payload of `chat`, `poke`):

| Prop | Type | Notes |
|---|---|---|
| `text` | `string` | |
| `channel` | `Channel` | if given (channel messages) |
| `client` | `Client` | sender (may be the bot itself!) |
| `mode` | `number` | 1 = private, 2 = channel, 3 = server |

(The bundled command library additionally reads `ev.message: DiscordMessage` on Discord — not in official docs; see §5.)

**`MoveInfo`** (payload of `clientMove`, `clientVisible`, `clientInvisible`, `clientKicked`, `clientKickedFromChannel`):

| Prop | Type | Notes |
|---|---|---|
| `fromChannel` | `Channel?` | undefined if client just connected / became visible |
| `toChannel` | `Channel?` | undefined if client disconnected / became invisible |
| `client` | `Client` | |
| `invoker` | `Client` | **[TS]**: optional |
| `message` | `string?` | move/ban/kick message (TS3; since 1.0.0-beta.6) |

**`ClientServerGroupEvent`**: `client: Client`, `invoker: Client`, `serverGroup: ServerGroup`.

**`ChannelParams`** (for `backend.createChannel`, `Channel.update`):

| Prop | Type | Notes |
|---|---|---|
| `name` | `string` | mandatory on create |
| `parent` | `Channel \| number \| string` | mandatory on create (TS3); ignored on update |
| `description` | `string` | |
| `topic` | `string` | |
| `password` | `string` | |
| `codec` | `number` | |
| `codecQuality` | `number` | |
| `encrypted` | `boolean` | default `true` |
| `permanent` | `boolean` | |
| `semiPermanent` | `boolean` | |
| `position` | `number` | |
| `maxClients` | `number` | -1 unlimited |
| `maxFamilyClients` | `number` | |
| `default` | `boolean` | |
| `neededTalkPower` | `number` | TS3 only |
| `deleteDelay` | `number` | TS3 only |
| `icon` | `number` | TS3 only |

**`NetConnectParams`**, **`DBParams`**, **`BannerSettings`**, **`BannerLayer`**: see §2.10, §2.12, §2.14.

---

## 4. Events (`event.on(name, cb)`)

Callback signature shown as positional parameters. "TS3"/"Discord" marks backend-specific events.

| Event | Callback | Since | Fires when |
|---|---|---|---|
| `api:<eventName>` | `(ev: ApiEvent) → any` | | Authenticated `POST {bot}/api/v1/bot/i/{instanceID}/event/{eventName}` with header `Authorization: bearer <token>` and JSON body. **Return value of the callback is sent back as the JSON response.** |
| `public:<eventName>` | `(ev: ApiEvent) → any` | | Same, **no auth**: `POST {bot}/api/v1/b/i/{instanceID}/event/{eventName}` (example in docs uses `/api/v1/b/{botId}/i/{instanceID}/event/{eventName}` — see §6). `ev.user()` is null. |
| `chat` | `(msg: Message)` | | Any chat message received (private/channel/server), **including the bot's own**. Both backends. |
| `message` | `(msg: DiscordMessage)` | | **Discord only**: a Discord message received. |
| `poke` | `(msg: Message)` | | Bot was poked (TS3). |
| `typing` | `(client: Client)` | | Client started typing in a chat with the bot. |
| `track` | `(track: Track)` | | New track starts. |
| `trackInfo` | `(track: Track)` | | Track metadata changes (e.g. radio). |
| `trackEnd` | `(track: Track, callback: string)` | | Track stopped. Second arg is an undocumented "callback string" (reason / origin). |
| `ytdl.success` | `(ev: { url: string, jobId: string, trackId?: string })` | | youtube-dl job succeeded (`trackId` only when downloaded). |
| `ytdl.error` | `(ev: { url: string, jobId: string }, message: string)` | | youtube-dl job failed. |
| `connect` | `()` | | Connection to server established. |
| `connectionFailed` | `(reason: string)` | | Unable to connect. |
| `disconnect` | `()` | | Connection closed. |
| `clientMove` | `(moveInfo: MoveInfo)` | | Client moves, joins (`fromChannel` undefined) or disconnects (`toChannel` undefined). |
| `clientNick` | `(client: Client, oldNick: string)` | | Nickname changed. **[TS]**: only `(client)`. |
| `clientVisible` | `(moveInfo: MoveInfo)` | | Client becomes visible to bot. |
| `clientInvisible` | `(moveInfo: MoveInfo)` | | Client becomes invisible. |
| `clientKicked` | `(moveInfo: MoveInfo)` | | Kicked from server. |
| `clientKickedFromChannel` | `(moveInfo: MoveInfo)` | | Kicked from channel. |
| `clientIPAddress` | `(client: Client)` | | IP changed / first fetched. |
| `clientAway` | `(client: Client)` | | Set away. |
| `clientBack` | `(client: Client)` | | Removed away. |
| `clientRecord` | `(client: Client)` | | Started recording. |
| `clientRecordStop` | `(client: Client)` | | Stopped recording. |
| `clientMute` | `(client: Client)` | | Muted mic. |
| `clientUnmute` | `(client: Client)` | | Unmuted mic. |
| `clientDeaf` | `(client: Client)` | 0.9.18 | Muted sound. |
| `clientUndeaf` | `(client: Client)` | 0.9.18 | Unmuted sound. |
| `clientBanned` | documented params: `message: string` (reason), `invoker: Client`, `fromChannel: Channel`, `client: Client`, `time: number` (s) | 1.0.0-beta.6 | Client banned from TS server (TS3). Whether positional or one object is ambiguous — see §6. |
| `serverGroupAdded` | `(ev: ClientServerGroupEvent)` | | Client added to server group. |
| `serverGroupRemoved` | `(ev: ClientServerGroupEvent)` | | Client removed from server group. |
| `channelCreate` | `(channel: Channel, invoker: Client)` | | Channel created. **[TS]**: `(ev: {channel, invoker})`. |
| `channelUpdate` | `(channel: Channel, invoker: Client)` | | Channel updated. **[TS]**: `(ev: {channel, invoker})`. |
| `channelDelete` | `(channel: Channel, invoker: Client)` | | Channel deleted. **[TS]**: invoker may be empty (temporary channel). |
| `clientConnectionInfoUpdate` | `(client: Client)` | 1.0.0-beta.10 | Client connection info updated (after `requestConnectionInfoUpdate`). |
| `speech` | `(ev: { client: Client, text: string })` | 0.13.37 | Registered `voiceCommands` word recognised. Requires speech recognition installed + enabled in config.ini, `voiceCommands` in manifest, `audio.setAudioReturnChannel(2)`. TS3 + licensed only. |
| `talkerCount` | `(number: number)` | | Number of users currently talking in bot's channel changed. |
| `unload` | `()` | | Script about to be unloaded/reloaded — clean up / persist. |
| `load` | `()` | | All scripts loaded (safe point to `require()` other scripts). |
| `discord:<EVENT_NAME>` | `(ev: object)` | | **Discord**: any raw gateway event, name UPPERCASE with spaces→underscores, e.g. `discord:GUILD_CREATE`, `discord:MESSAGE_REACTION_ADD` (payload = raw gateway `d` object, e.g. `ev.emoji.name`, `ev.user_id`, `ev.message_id`, `ev.channel_id`). |
| `ws.connect` | `(id: string)` | 0.9.20 | New WebSocket connection (ws module). |
| `ws.close` | `(id: string)` | 0.9.20 | Connection closed. |
| `ws.error` | `(id: string, error: string)` | 0.9.20 | **[TS]**: `(ev: {id, error})`. |
| `ws.data` | `(id: string, type: number, data: Bytes)` | 0.9.20 | Message received. **[TS]**: `(ev: {id, type, data})`. Bundled ws example uses positional form. |
| `serverinfo_int` | `(serverinfo: TeamSpeakExtendedServerInfo)` | 1.0.0-alpha.6 | Response to `backend.extended().requestExtendedServerInfo()` (TS3). |
| *custom* | `(data: object)` | | Any name passed to `event.emit` / `event.broadcast`. |

Total: 46 documented bot events (incl. the 3 pattern events `api:*`, `public:*`, `discord:*`) + 3 `NetClient` events (`data`, `close`, `error`).

### 4.1 Web exposure summary (`enableWeb` + `api:`/`public:` + `ws`)

- Static files: `./scripts/<scriptname>/html/` (served when `enableWeb: true`; script title links to `index.html`).
- Authenticated RPC: `POST /api/v1/bot/i/{instanceID}/event/{name}` → `api:{name}`; headers `Content-Type: application/json`, `Authorization: bearer <localStorage.token>`; response = handler return value.
- Public RPC: `POST /api/v1/b/i/{instanceID}/event/{name}` (or `/api/v1/b/{botId}/i/{instanceID}/event/{name}`) → `public:{name}`; no auth.
- Query string available via `ev.queryParams()`; caller IP via `ev.remoteAddr()`.
- WebSocket: `/api/v1/b/{botId}/i/{instanceId}/ws` (needs `ws` module) → `ws.*` events.
- Login/instance listing HTTP API is the general SinusBot API (https://www.sinusbot.com/api/), out of scope here.

---

## 5. Platform notes (TeamSpeak 3 vs Discord) and deprecations

### 5.1 Backend selection
- `manifest.backends` defaults to `["ts3"]`; Discord must be opted in.
- `engine.getBackend()` → `"ts3"` | `"discord"`; `backend.extended()` returns the matching Extended API.

### 5.2 Discord-only
- Event `message` (`DiscordMessage`), events `discord:<GATEWAY_EVENT>`.
- `ExtendedDiscord` (guild/channel/message REST, `setStatus`, `rawCommand`).
- `Channel.getMessages(params, cb)`.
- `Client.getURL()` returns `<@uid>` mention; `Channel.type()` 1 = text.
- `backend.getBotClient().moveTo('')` disconnects from voice.
- `User.uid/groupId` bind to Discord user ID / role ID.
- `format.color` is a no-op (no color in Discord markdown).
- Bundled scripts read `ev.message` (a `DiscordMessage`) on `chat`-derived command events and `ev.mode` may be `0` (command lib type `0|1|2|3`) — undocumented.
- Discord message timestamps are `Date`.

### 5.3 TS3-only (or TS3-meaningful)
- Events `poke`, `serverinfo_int`, `clientBanned`, `clientKicked*`, `clientRecord*`, `clientAway/Back`, `clientMute/Unmute/Deaf/Undeaf`, `talkerCount`, `speech`, `clientIPAddress`, `clientConnectionInfoUpdate`.
- `ExtendedTS3`, `TeamSpeakServerInfo`, `TeamSpeakExtendedServerInfo`.
- `Client.poke`, `databaseID`, `getChannelGroup`, connection stats (`getPing`, `getPacketLoss`, `getBytes*`, `getOnlineTime`, `getIdleTime`, `getTotalConnections`, `getCreationTime` — the last two lazily requested; ≤0 until known), `getPlatform`, `getVersion`, `type()` (query/normal), `phoneticName`, `country`.
- Channel: codecs, codecQuality, talk power, deleteDelay, icon, semi-permanent, encryption, permissions (`Permission` mainly TS3).
- `MoveInfo.message` (TS3, ≥1.0.0-beta.6).
- `backend.setAway`, `Client.getURL()` → `client://0/uid~nickname`.
- Subscription mode (`engine.*Subscription*`, `Client/Channel.subscribe`).
- `voiceCommands` / speech recognition: TS3 + licensed instance only.

### 5.4 Deprecated / discouraged / broken
| Item | Status |
|---|---|
| `mainFunction` first param `_` (legacy `sinusbot` object) | Deprecated. |
| `Client.uniqueId()` | Deprecated → `uid()`. |
| `helpers.toString()` | Deprecated. |
| `helpers.typeOf()` | Private, deprecated. |
| `engine.export()` | Superseded by `module.exports` (1.0.0). |
| `User.tsUid / setTSUid / tsGroupId` | Old names; `uid / setUid / groupId` preferred (still documented). |
| `audio.setAudioFilter()` | Documented as unsafe, leads to crashes. |
| `TeamSpeakServerInfo.version()` | Broken, always 0. |
| `requiredModules: ['discord-dangerous']` | Needed only ≤ 1.0.0-beta.6 for `rawCommand`. |
| `Client.setDescriptionHex`, `PlaylistTrack.hasTitle/hasAlbum/hasArtist/enqueue/playNext` | Commented out in JSDoc (not officially exposed). |
| `var conditions` on `false` values | Known buggy. |

---

## 6. Ambiguities / inconsistencies to decide on for James

1. **Positional vs. object payloads** — JSDoc and TS typings disagree for `channelCreate`/`channelUpdate` (positional `(channel, invoker)` vs `{channel, invoker}`), `ws.error`, `ws.data`, `clientNick` (oldNick arg). Recommendation: deliver positional args **and** make the first arg also carry `.channel/.invoker` etc. where feasible, or follow JSDoc (newer, matches bundled examples for `ws.data`).
2. **`clientBanned`** — 5 `@param`s (`message, invoker, fromChannel, client, time`); unclear whether positional or a single object (most other TS3 move-like events use a single `MoveInfo`-style object). Supporting a single object with those fields that is also the first positional arg is the safest compatibility choice.
3. **`trackEnd` second param** "callback: string" — undocumented semantics.
4. **`public:` URL** — description says `/api/v1/b/i/{instanceID}/event/{name}`, example says `/api/v1/b/{botId}/i/{instanceID}/event/{name}`; serve both.
5. **ws example** listens to `ws.disconnect`, which is not a documented event (`ws.close` is). Consider aliasing.
6. **Units** — `audio.getTrackPosition()/seek()` documented in seconds, while `Track.duration()` is ms. Needs verification against a real SinusBot (community scripts generally treat position as ms).
7. **Return types that look wrong**: `Client.isRecording() → string`, `Client.type() → string` ("0"/"1"), `FileInfo.size() → string`, `select` config value is an index (number vs string).
8. **Missing parameters in docs**: `engine.saveConfig(config)` stub has no param; `audio.setMute(mute)` stub has no param; `ExtendedDiscord.getChannelMessage` lacks message ID; `media.getActivePlaylist` has a spurious `id`.
9. **Naming variants between JSDoc and [TS]** — implement both as aliases: `getUserById`/`getUserByID`, `setThumbnailFromURL`/`setThumbnailFromUrl`, `subscribe`/`setSubscription`, `enableWeb`/`enableweb`, `graphics`/`graphic`, plus `[TS]`-only `engine.setChannelSubscription(channelOrClient, bool)`.
10. **Undocumented internals**: `engine.logf` format syntax, `BannerLayer` schema, `crypto` algorithms and `pbkdf2` `hname`, `CryptoKeypair.sharedKey`, `ws` message type numbers, `indent` semantics, `track`/`tracks` config value shape.
11. **`DBParams.database`** used in example but missing from typedef; `DBConn.query/exec` parameters are variadic between query string and callback.
12. **Discord `chat` event** — the bundled command library expects `ev.message` (DiscordMessage) to be present on Discord chat events; not in official docs.
13. Timers (`setTimeout`/`setInterval`/`clear*`) and `console` are only declared in [TS] typings, but scripts rely on timers heavily — must be provided.

---

## Appendix A — `require('command')` (bundled 3rd-party Command library)

Not part of the engine, but shipped with SinusBot ≥ 1.0.0-alpha and used by many scripts (docs: https://multivit4min.github.io/Sinusbot-Command/). Surface per `typings/external/command.d.ts`:

- Top level: `createCommand(cmd, overrides?)`, `createArgument("client"|"number"|"string"|"rest")`, `createCommandGroup(name)`, `createGroupedArgument("and"|"or")`, `getCommandPrefix()`, `createThrottle()`, `getVersion()`, `collector: CommandCollector`.
- Errors: `CommandDisabledError`, `ThrottleError`, `ParseError`, `TooManyArguments`, `SubCommandNotFound`, `PermissionError`.
- `MessageEvent`: `{ text, client, channel, mode: 0|1|2|3, message: DiscordMessage }`; exec callback `(client, args, reply(msg), event)`.
- `CommandCollector`: `getAvailableCommandsWithPrefix`, `isPossibleCommand`, `getPrefixes`, `registerCommand`, `getAvailableCommands`, `getCommandByName`.
- `Throttle`: `tickRate`, `penaltyPerCommand`, `restorePerTick`, `initialPoints`, `throttle`, `isThrottled`, `timeTillNextCommand`.
- `Argument<T>`: `optional(fallback?, displayDefault?)`, `getDefault`, `hasDefault`, `getManual`, `isOptional`, `setName(name, display?)`, `getName`; `StringArgument`: `match, max, min, forceUpperCase, forceLowerCase, whitelist`; `NumberArgument`: `match, max, min, integer, positive, negative`; `GroupArgument.addArgument(...)`; `ClientArgument`, `RestArgument`.
- `Command`: `getCommandName, getFullCommandName, forcePrefix, getPrefix, help, hasHelp, getHelp, throttle, disable, enable, isEnabled, exec, dispatchCommand, manual, hasManual, getManual, getUsage, checkPermission, isAllowed, run, validate, validateArgs, addArgument, getArguments`; `CommandGroup` adds `addCommand, findSubCommandByName, getAvailableSubCommands`; `SubCommand`.

Since this is plain JS running on top of the engine, the simplest compatibility path is to ship the original `command.js` unmodified once the core engine API is compatible.

---

## Counts

| Category | Count |
|---|---|
| Modules (`require`-able built-ins) | 15 (engine, store, backend, media, audio, format, helpers, event, http, net, ws, db, fs, graphics, crypto) |
| Module methods | 183 documented in JSDoc (engine 44 — plus 1 [TS]-only `setChannelSubscription`; store 15; backend 26; media 29; audio 22; format 5; helpers 16; event 3; http 1; net 1; ws 3; db 1; fs 9; graphics 2; crypto 6) |
| Interfaces (method-based) | 22 (Client, Channel, User, Permission, ChannelGroup, ServerGroup, Bytes, BytesWriter, PlaylistTrack, Track, ApiEvent, Playlist, NetClient, DBConn, FileInfo, DiscordMessage, Extended, ExtendedDiscord, ExtendedTS3, TeamSpeakServerInfo, TeamSpeakExtendedServerInfo, CryptoKeypair) |
| Interface methods | 303 (Client 47, Channel 43, User 20, Permission 10, ChannelGroup 5, ServerGroup 7, Bytes 18, BytesWriter 23, PlaylistTrack 5, Track 20, ApiEvent 5, Playlist 4, NetClient 3, DBConn 2, FileInfo 5, DiscordMessage 17, ExtendedDiscord 10, ExtendedTS3 2, TeamSpeakServerInfo 21, TeamSpeakExtendedServerInfo 34, CryptoKeypair 2) — incl. 3 private `string()` |
| Plain types | 9 (Manifest, Message, MoveInfo, ChannelParams, ClientServerGroupEvent, NetConnectParams, DBParams, BannerSettings, BannerLayer) |
| Callback types | 5 (mainFunction, simpleRequestCallback, netConnectCallback, dbConnectCallback, dbQueryCallback) |
| Events | 46 bot events + 3 NetClient events |
| Config var types | 11 (string, strings, password, multiline, number, track, tracks, channel, select, checkbox, array) |
| Manifest fields | 12 |
