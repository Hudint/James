# Using James

## Web interface

- **Dashboard** – all bots at a glance: their channel, listeners, what they play (with progress
  and play/pause/skip), a field to play something on an idle bot, the plugin host's state and the
  latest warnings from the instance logs you may read.
- **Instance** – player (play/pause, skip, seek, volume, repeat, shuffle), queue (reorder, remove,
  jump), adding music (URL, search, library), speaking a text, and the server tree (move the bot by
  clicking a channel).
- **Logs** (button on an instance) – the instance's recent messages: connection events, playback
  errors, chat commands (without their arguments) and what plugins log there (`bot.log` / SinusBot's
  `engine.log`; debug messages only at log level debug), filterable by level, source and text, and
  downloadable. The last 2000 messages per instance since James started are kept in memory; floods
  from one client or plugin are cut short.
- **Library** – music on the server; upload files (MP3, FLAC, OGG, WAV, …) and edit a track's title,
  artist and album (pencil button). The file is not changed and a rescan keeps the edits; fields
  left as in the file follow the file's tags. Tracks already in a queue or playlist keep the name they
  had when they were added, and edits are lost when the file is moved or renamed on disk.
- **Playlists** – saved lists; play them on any instance.
- **Plugins** – for the instance chosen in the toolbar: search and filter (all, active, available,
  problems), switch plugins on and off, and open any number of them at once to see their details,
  permissions and settings ([Plugins](plugins/README.md)). Unsaved settings stay while a plugin is
  collapsed or filtered out; a note at the bottom leads back to them.
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
direct stream), `yt: <search>` (or plain text, which searches YouTube), a file path from the
library (`local:<path>` relative to a media folder), or `library:<id>` for a library track (what
plugin settings store when a song is chosen from the library). Library files can be played by
everyone allowed to play; `library.read` only guards browsing the library.

### Voice commands

With speech recognition and a wake word set for an instance (instance settings), commands can be
spoken in the bot's channel: the wake word, then the command, e.g. "James, skip" or "James, play
Thunderstruck". German words work for the basic commands: *spiel* (play), *weiter* / *nächstes*
(skip), *zurück* (prev), *stopp*, *anhalten* (pause), *fortsetzen* (resume), *lauter* / *leiser*.
The speaker needs the `voice.command` permission (not part of any role by default: anyone's
microphone may pick up a command someone else said or played) and the same permission as for the
chat command; answers come as a private message. Only people in the bot's channel are listened to.
Only James' playback commands can be spoken: play, add, skip, prev, stop, pause, resume, volume and
nowplaying (not plugin commands, so that plugins without the `listen` permission never receive what
people say, and nothing that speaks, removes or reorders, since a microphone may pick up someone
else's command). Each speaker is recognised for at most a minute of speech per minute.

Speech that only sounds like the wake word is ignored unless it is followed by a known command.
Recognition allows one wrong letter (two in long words) and must get the first letter right, so a
distinctive name works best; short words or names that sound like everyday words ("Jens", "Ja") are
often missed or confused.

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
| `voice.command` | Give commands by voice (with speech recognition and a wake word), in addition to the command's own permission |
| `instance.speech` | Switch speech recognition of an instance on or off and change its settings (also granted by `instance.*`) |
| `instance.logs` | Read an instance's log (may contain chat commands and client names; also granted by `instance.*`) |
| `library.read`, `library.manage` | Browse / upload, rename and rescan the library |
| `playlist.read`, `playlist.manage` | See / edit playlists |
| `plugins.use`, `plugins.manage` | Use plugin pages / install and configure plugins |
| `users.manage` | Manage users and roles |
| `plugin.<id>.<name>` | Permissions defined by plugins (e.g. for their commands) |

`*` grants everything, `playback.*` all permissions below `playback`.
