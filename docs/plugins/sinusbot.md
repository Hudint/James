# SinusBot scripts

James runs scripts written for the [SinusBot scripting engine](https://sinusbot.github.io/scripting-docs/)
without changes. Copy the `.js` file into the plugin folder (`<data_dir>/plugins/`), click
**Rescan** under **Plugins** in the web interface, and enable it. Its `vars` appear as settings
per instance. A script with `enableWeb` serves `plugins/<name>/html/` as its page.

How it works: each script runs once per instance in its own worker thread, like on SinusBot.
`require('engine')` and the other modules are provided by a compatibility layer on top of the
[native plugin API](README.md). Synchronous SinusBot calls are synchronous here too: reads come
from a local mirror of the server, and the few calls that need data from James (users, playlists,
the library) block the script's worker until the answer arrives. Actions such as `client.kick()`
return `true` right away and run in the background; failures are written to the log.

Scripts can also `require()` Node.js built-ins and npm packages installed next to them, subject to
the same [permissions](README.md#permissions) as native plugins.

## Manifest

| Field | Support |
|---|---|
| `name`, `version`, `description`, `author` | ✓ |
| `vars` (all 11 types, `conditions`, `indent`, `placeholder`) | ✓ shown as settings per instance |
| `autorun`, `hidden` | ✓ |
| `backends` | ✓ `ts3` → TeamSpeak (3 and 6), `discord` → Discord; default `ts3` |
| `requiredModules` | ✓ mapped to permissions: `http` → `http`, `net` → `net`, `ws` → `web`, `db` → `db`, `fs` → `fs`; `graphics` and `discord-dangerous` need none |
| `enableWeb` | ✓ requests `web`; files from `<script name>/html/` |
| `engine` | ignored (refers to SinusBot versions) |
| `voiceCommands` | ✗ speech recognition is not available yet |

`registerPlugin(manifest, main)` calls `main(_, config, meta)` once per instance; `_` is
`undefined` (the legacy `sinusbot` object is deprecated in SinusBot as well).

## Differences

- **Unsupported functions** (marked ✗ below) do nothing, return `false`/`undefined`/`[]`, and log
  a warning the first time a script calls them.
- **Avatars, banners (`graphics`), recording, Icecast streaming, audio filters, idle/startup
  tracks, subscription mode** are not available.
- **`audio.say` (text to speech)** arrives with James' TTS support.
- **Positions** (`audio.getTrackPosition`, `audio.seek`) are in milliseconds.
- **Track ids** are `<source>:<locator>`. `media.getTrackByID` and `media.search` work on the local
  library; `track.url()` returns something `media.playURL` accepts.
- **Users** are James accounts. SinusBot's privilege bitmasks are replaced by roles: `privileges()`
  reports all bits for administrators and none otherwise, and the `set*Privilege*` methods do
  nothing. User methods need the `users` permission; administrators cannot be changed.
- **`audio.setMute`** is emulated through the volume.
- **`engine.setNick`** changes the nickname until the next reconnect; the permanent nickname is
  part of the instance settings, as are the command prefix and the default channel.
- **`db.connect`** supports SQLite (`driver: 'sqlite3'`, file in the plugin's data directory).
  For MySQL or PostgreSQL install an npm driver and use it directly.
- **`fs`** is limited to the plugin's data directory.
- **`crypto.seal`/`open`** use ChaCha20-Poly1305 and keypairs are X25519; data encrypted by
  SinusBot cannot be decrypted (and the other way round).
- **`chat` events** do not include the bot's own messages.
- **Requiring other scripts** (`require('<script>.js')` / `engine.export`) works within one
  script only. Use `event.emit` / `event.broadcast` to talk to other scripts.
- **Timers** (`setTimeout`, `setInterval`) are cleared when the script stops.
- **HTTP endpoints**: `api:<name>` handlers answer
  `POST /api/v1/instances/<instance>/plugins/<id>/events/<name>` and `public:<name>` handlers
  `POST /api/v1/public/instances/<instance>/plugins/<id>/events/<name>` (SinusBot's
  `/api/v1/bot/i/<instance>/event/<name>` URLs are not served).
- **Web pages** (`enableWeb`) are sandboxed and cannot use the session or `localStorage` tokens of
  the web interface. Include `<script src="/plugins/_/james.js"></script>` and use
  `James.call(name, data)` instead of `fetch` with a token (see
  [Web pages](README.md#web-pages-and-http)); pages written for SinusBot need this small change.
- **`ws`** connections come from `GET /api/v1/instances/<instance>/plugins/<id>/ws`. Messages are
  text; the `messageType` argument is ignored and `ws.data` reports type 1.

## Coverage

Legend: ✓ supported · ◐ supported with the noted difference · ✗ not supported.

### Events

| Event | | Notes |
|---|---|---|
| `chat` | ✓ | `mode` 1 private, 2 channel, 3 server |
| `poke` | ✓ | |
| `connect`, `disconnect` | ✓ | |
| `connectionFailed` | ✓ | for failed connects before the first successful one |
| `clientMove` | ✓ | joins (`fromChannel` undefined), leaves (`toChannel` undefined), switches |
| `clientVisible`, `clientInvisible` | ✓ | |
| `clientKicked`, `clientKickedFromChannel` | ✓ | |
| `clientBanned` | ◐ | `time` is 0 (TeamSpeak does not announce the duration) |
| `clientNick` | ✓ | `(client, oldNick)` |
| `clientAway`, `clientBack` | ✓ | |
| `clientMute`, `clientUnmute`, `clientDeaf`, `clientUndeaf` | ✓ | |
| `clientRecord`, `clientRecordStop` | ✓ | |
| `clientIPAddress`, `clientConnectionInfoUpdate` | ✓ | after `requestConnectionInfoUpdate()` |
| `serverGroupAdded`, `serverGroupRemoved` | ◐ | `invoker` is undefined |
| `channelCreate`, `channelUpdate`, `channelDelete` | ✓ | `(channel, invoker)` |
| `track`, `trackEnd` | ✓ | `trackEnd` second argument: `'error'` when the track failed, else `''` |
| `trackInfo` | ✗ | stream titles are part of the track |
| `ytdl.success`, `ytdl.error` | ✓ | for `media.yt*` jobs |
| `api:<name>`, `public:<name>` | ◐ | see the URLs above |
| `ws.connect`, `ws.close` (`ws.disconnect`), `ws.data` | ✓ | |
| `ws.error` | ✗ | |
| `load`, `unload` | ✓ | |
| custom (`event.emit`, `event.broadcast`) | ✓ | across scripts and native plugins |
| `talkerCount`, `typing`, `speech`, `serverinfo_int` | ✗ | |
| `message`, `discord:*` | ✗ | Discord support is not available yet |

### Modules

| Module | Supported | Not supported (✗) / differences (◐) |
|---|---|---|
| `engine` | `getInstanceID`, `getBotID`, `getBackend`, `get/setInstanceLogLevel`, `get/setBotLogLevel`, `getNick`, `setNick` ◐, `getDefaultChannelID`, `isRunning`, `notify`, `saveConfig`, `log`, `logf` ◐ (Go verbs mapped to `util.format`), `export`, `getUsers`, `getUserById`/`getUserByID`, `getUserByName`, `addUser`, `getCommandPrefix`, `version`, `os`, `isSubscriptionMode`, `getChannelSubscriptions`, `getClientSubscriptions`, `getIgnoreCommandsFrom*`, `registrationEnabled`, `enable/disableRegistration` (no-ops) | ✗ `reloadScripts`, `setDefaultChannelID`, `setCommandPrefix`, avatar functions, `setSubscriptionMode`, `setChannelSubscription`, `setIgnoreCommandsFrom*` |
| `store` | all 15 functions (script, instance and global scope) | |
| `backend` | all 26 functions; `connect`/`disconnect` need `instances` | `extended()` returns the TeamSpeak extension |
| `media` | `playURL`, `getCurrentTrack`, `getTrackByID` ◐, `getTracks` ◐ (first 200), `search` ◐ (library), `enqueue`, `playAsNext`, `playNext`, `playPrevious`, `stop`, `getQueue`, `getPlaylists`, `getPlaylistByID`, `removeFromQueue`, `clearQueue`, `clearPlaylist`, `yt`, `ytStream`, `ytdl`, `enqueueYt`, `enqueueYtdl`, `playQueueNext`, `resumeQueue`, `playlistPlayByID` | ◐ `getActivePlaylist` returns undefined; ✗ idle and startup tracks |
| `audio` | `isRepeat`, `setRepeat`, `isShuffle`, `setShuffle`, `getVolume`, `setVolume`, `getTrackPosition`, `seek`, `isMute`, `setMute` ◐, `isPlaying`, `getAudioReturnChannel` | ✗ `setAudioFilter`, `setAudioReturnChannel`, recording, `streamToServer`, `stopStream`, `say` (TTS pending), `setStreamVolume`, `setTTSURL`, `setTTSDefaultLocale` |
| `format` | all 5 (`code` uses `[noparse]` on TeamSpeak) | |
| `helpers` | all 16 | |
| `event` | `on`, `emit`, `broadcast` | |
| `http` | `simpleRequest` (`method`, `url`, `headers`, `body`, `timeout`, `maxSize`) | |
| `net` | `connect` with `tcp`, `udp`, `ws` | |
| `ws` | `write`, `broadcast`, `close` | |
| `db` | `connect` ◐ (SQLite only), `query`, `exec` | |
| `fs` | all 9 ◐ (plugin data directory only) | |
| `graphics` | | ✗ `setBanner`, `removeBanner` |
| `crypto` | all 6 ◐ (different algorithms, see above) | |

### Objects

| Object | Supported | Not supported (✗) / differences (◐) |
|---|---|---|
| `Client` | all identity, state and statistic getters, `setDescription` ◐ (own client only), `setDescriptionHex`, `getServerGroups`, `getChannelGroup`, `getChannels`, `getAudioChannel`, `getURL`, `equals`, `chat`, `poke`, `ban`, `kick`, `kickFromServer`, `kickFromChannel`, `addToServerGroup`, `removeFromServerGroup`, `moveTo`, `getPlatform`, `getVersion`, `requestConnectionInfoUpdate`, `type`, `string` | ✗ `subscribe`/`setSubscription`; moving the bot to `''` (leave voice, Discord) |
| `Channel` | all getters and setters, `delete`, `moveTo`, `setPosition` ◐ (index among siblings), `chat` (bot's own channel), `getClients`, `getClientCount`, `subscribe`, `update`, `setChannelGroup`, `getURL`, `equals`, `string` | ✗ `getPermissions`, `addPermission`, `getMessages` |
| `ServerGroup`, `ChannelGroup` | `id`, `name`, `icon`, `addClientByDatabaseId` ◐, `removeClientByDatabaseId` ◐ (online clients) | ✗ `getPermissions`, `addPermission` |
| `Permission` | | ✗ (permissions cannot be read or edited) |
| `User` | `id`, `name`, `privileges` ◐, `instancePrivileges` ◐, `uid`/`tsUid`, `groupId`/`tsGroupId` (empty), `isAdmin`, `setPassword`, `setUid`/`setTSUid`, `delete` | ✗ `setGroupId`, privilege setters |
| `Track` | all getters, `play`, `enqueue`, `addNext` | ✗ `setThumbnailFromURL`, `removeThumbnail`; `genre`/`trackNumber` are empty |
| `Playlist`, `PlaylistTrack` | `id`, `name`, `getTracks`; `title`, `artist`, `album`, `url`, `play` | ✗ `Playlist.setActive` |
| `ApiEvent` | all 5 | |
| `Bytes`, `BytesWriter` | all methods | |
| `NetClient`, `DBConn`, `FileInfo`, `CryptoKeypair` | all methods | |
| `ExtendedTS3` / `TeamSpeakServerInfo` | `getServerInfo` with all fields | ✗ `requestExtendedServerInfo`; `reservedSlots` and `defaultChannelAdminGroup` are 0 |
| `DiscordMessage`, `ExtendedDiscord` | | ✗ Discord support is not available yet |

### The Command library

SinusBot ships the third-party [Command library](https://multivit4min.github.io/Sinusbot-Command/)
as `require('command')`. It is not bundled with James. Scripts that depend on it can include
`command.js` themselves. Native plugins register commands with
[`bot.commands.register`](README.md#chat-and-commands).
