# Using James

## Web interface

- **Dashboard** – all bots with what they play.
- **Instance** – player (play/pause, skip, seek, volume, repeat, shuffle), queue (reorder, remove,
  jump), adding music (URL, search, library), speaking a text, and the server tree (move the bot by
  clicking a channel).
- **Library** – music on the server; upload files (MP3, FLAC, OGG, WAV, …) and edit a track's title,
  artist and album (pencil button). The file is not changed and a rescan keeps the edits; fields
  left as in the file follow the file's tags. Tracks already in a queue or playlist keep the name they
  had when they were added, and edits are lost when the file is moved or renamed on disk.
- **Playlists** – saved lists; play them on any instance.
- **Plugins** – for the instance chosen at the top: activate and deactivate plugins, their
  settings, and the permissions they get ([Plugins](plugins/README.md)).
- **Instance → Settings** – connection, audio, command prefix, voice, and the bot's **identity**:
  its unique ID, and an existing identity to use instead (exported from the TeamSpeak client via
  *Tools → Identities → Export*, or copied from SinusBot), so the bot keeps its server groups.
  Administrators can also show the bot's private identity (after a warning and their password,
  blurred until revealed) to copy it or download it as a file the TeamSpeak client imports.
- **Users & roles** – accounts, roles and what they may do, per instance if needed.
- **Settings** (administrators) – YouTube cookies.
- **Account** – password, linked TeamSpeak identities, API tokens.

## Chat commands

Write them to the bot in TeamSpeak: privately, in its channel or in the server chat. The prefix (`!`)
can be changed per instance. Plugins add more commands; `!help` lists the ones available.

| Command | Aliases | Does | Permission |
|---|---|---|---|
| `!help [command]` | `h`, `commands` | Lists commands / shows one | – |
| `!play <url\|search\|path>` | `p` | Plays immediately | `playback.play` |
| `!add <…>` | `a`, `enqueue` | Adds to the end of the queue | `playback.play` |
| `!playnext <…>` | `pn` | Plays after the current track | `playback.play` |
| `!skip` | `next`, `n`, `s` | Next track | `playback.control` |
| `!prev` | `previous`, `back` | Previous track | `playback.control` |
| `!stop`, `!pause`, `!resume` | `continue`, `unpause` | Stop / pause / resume | `playback.control` |
| `!seek <m:ss\|+s\|-s>` | | Jumps within the track | `playback.control` |
| `!volume [0-100\|+n\|-n]` | `vol`, `v` | Shows or sets the volume | `playback.volume` |
| `!nowplaying` | `np`, `song`, `current` | Shows the current track | – |
| `!queue [page]` | `q`, `list` | Shows the queue | – |
| `!clear`, `!remove <pos>`, `!move <from> <to>` | `rm`, `mv` | Edits the queue | `queue.manage` |
| `!shuffle [on\|off]`, `!repeat [off\|one\|all]` | `loop` | Shuffle / repeat | `playback.control` |
| `!say <text>` | `tts` | Speaks the text over the music | `tts.say` |
| `!hush` | `shutup` | Stops speaking | `tts.say` |
| `!link <code>` | | Links your TeamSpeak identity to your web account (private message) | – |

Inputs: a URL (YouTube, SoundCloud, Bandcamp, radio streams, … – everything yt-dlp supports, or a
direct stream), `yt: <search>` (or plain text, which searches YouTube), or a file path from the
library.

## Users, roles and permissions

Permissions are granted through roles, globally or for single instances. Built-in roles:

| Role | Permissions |
|---|---|
| `admin` | everything |
| `dj` | playback, queue, playlists, library (read), text to speech, plugin pages |
| `everyone` | `playback.play`; applies to every TeamSpeak client, linked or not |

Roles can also be given to TeamSpeak server groups (**Users & roles → Server groups**), so for
example everybody in the group "Music" may control the bot without a web account.

TeamSpeak users get the roles of their linked web account: in the web interface open **Account →
Linked identities → Link identity** and send the shown `!link` code to the bot.

| Permission | Allows |
|---|---|
| `playback.play` | Play and queue music |
| `playback.control` | Pause, skip, stop, seek, repeat, shuffle |
| `playback.volume` | Change the volume |
| `queue.manage` | Reorder, remove and clear the queue |
| `tts.say` | Text to speech |
| `server.move` | Move the bot between channels |
| `server.message` | Send chat messages as the bot |
| `server.chat.read` | Read private chat and commands in the web interface |
| `instance.view`, `instance.manage` | See / create, configure and delete instances |
| `library.read`, `library.manage` | Browse / upload, rename and rescan the library |
| `playlist.read`, `playlist.manage` | See / edit playlists |
| `plugins.use`, `plugins.manage` | Use plugin pages / install and configure plugins |
| `users.manage` | Manage users and roles |
| `plugin.<id>.<name>` | Permissions defined by plugins (e.g. for their commands) |

`*` grants everything, `playback.*` all permissions below `playback`.
