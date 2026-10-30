# Writing plugins

James plugins are JavaScript or TypeScript modules. Every plugin runs in its own Node.js process
(the *plugin host*), so a crashing or busy plugin never blocks the bot or other plugins, and James
always knows which plugin is calling. Plugins can use what Node.js offers (`fetch`, npm packages,
timers, …), subject to the permissions an administrator grants.

- **Native plugins** use the typed API `@james/api` described here.
- **SinusBot scripts** run unchanged through the compatibility layer, see [sinusbot.md](sinusbot.md).
- The complete API reference is the [plugin API site](https://hudint.github.io/James/), generated
  from [`packages/api/index.d.ts`](../../packages/api/index.d.ts) (editors show the same
  documentation as you type).

Contents: [Quick start](#quick-start) · [Manifest](#manifest) · [Lifecycle](#lifecycle) ·
[Events](#events) · [Server state](#server-state) · [Chat and commands](#chat-and-commands) ·
[Playback](#playback) · [Store](#store) · [Web pages and HTTP](#web-pages-and-http) ·
[Permissions](#permissions) · [Accounts and instances](#accounts-and-instances) ·
[Logging and errors](#logging-and-errors) · [Development tips](#development-tips) ·
[Examples](#examples)

## Quick start

1. Create a file in the plugin folder (`<data_dir>/plugins/`, configurable with `plugins.dir`).
   Its name is the plugin id (lowercase letters, digits, `-`, `_`), e.g. `welcome.ts`:

   ```ts
   import { definePlugin } from '@james/api';

   export default definePlugin({
     name: 'Welcome',
     version: '1.0.0',
     description: 'Greets clients that join the server',
     config: [{ name: 'message', title: 'Greeting', type: 'string', default: 'Welcome, {name}!' }],
     setup(bot) {
       bot.on('clientJoin', async ({ client }) => {
         await client.chat(bot.config.message.replace('{name}', client.name)); // typed: string
       });
     },
   });
   ```

2. In the web interface open **Plugins**, choose the instance at the top, click **Rescan**, then
   **Enable**. Plugins are activated and configured per instance.

Everything besides `setup` describes the plugin (name, version, `permissions`, `config`, …, see
[Manifest](#manifest)). James reads it when it scans the plugins by running the file's top level
in a sandbox: imports do nothing there and `setup` does not run, so the settings may be built
with code:

```ts
const events = [{ name: 'clientMute', title: 'Microphone muted' }, { name: 'clientAway', title: 'Away' }] as const;

export default definePlugin({
  name: 'AFK mover',
  permissions: ['moderation'],
  config: events.flatMap(({ name, title }) => [
    { name: `${name}Active`, title: `${title}: active`, type: 'checkbox' },
    { name: `${name}Channel`, title: `${title}: channel`, type: 'channel' },
  ] as const),
  setup(bot) {
    bot.config.clientMuteActive; // boolean | undefined
  },
});
```

`bot.config` is typed from the fields (fields built in a callback need `as const` to keep their
names). The top level gets one second and little memory there, and cannot use `await`.

Every `.ts` file directly in the plugin folder is a plugin, and it can only use npm packages
from the folder's `node_modules` (not other files next to it). Larger plugins (several files, a
web page, their own npm packages) are a directory with a `package.json` whose `james` section
holds the same fields, and an entry module (`main`).

TypeScript runs directly (Node.js strips the types, no build step). Use only erasable syntax:
no `enum`, `namespace` or constructor parameter properties.

For type checking and editor support, add `@james/api` as a dev dependency. Every release from
0.5.2 on has it attached (with its SHA-256 in `SHA256SUMS`); use the version of your James and
commit the `package-lock.json`, which pins the file's hash:

```sh
npm i -D @james/api@https://github.com/Hudint/James/releases/download/v0.5.2/james-api-0.5.2.tgz
npm i -D typescript
```

and a `tsconfig.json`:

```json
{
  "compilerOptions": {
    "target": "ES2024",
    "module": "NodeNext",
    "moduleResolution": "NodeNext",
    "strict": true,
    "noEmit": true,
    "erasableSyntaxOnly": true,
    "verbatimModuleSyntax": true,
    "skipLibCheck": true
  },
  "include": ["**/*.ts"],
  "exclude": ["node_modules"]
}
```

Then `npx tsc -p .` checks every plugin, and editors type `bot`, events and clients. Without the
package, `@james/api` cannot be resolved and everything is `any` (e.g. *TS7006: Parameter 'bot'
implicitly has an 'any' type*).

## Manifest

The fields next to `setup` in a single-file plugin, or the `james` section of a directory
plugin's `package.json` (there `name`, `version`, `description` and `author` are the
package's own fields):

| Field | Type | Meaning |
|---|---|---|
| `name` | string | Display name (default: the package name). |
| `engine` | string | Required James version, e.g. `">=0.1.0"` (`>=`, `>`, `<=`, `<`, `=`, `^`, `~`). |
| `permissions` | string[] | Permissions the plugin needs ([list](#permissions)). An administrator confirms them. |
| `optionalPermissions` | string[] | Permissions the plugin can use but runs without. The administrator chooses which of them to grant; calls that need a missing one fail with a permission error. |
| `config` | field[] | Settings shown in the web interface, stored per instance ([fields](#settings)). |
| `platforms` | string[] | `teamspeak`, `discord`; default: all. |
| `autorun` | boolean | Active on every instance; cannot be restricted to some instances. |
| `hidden` | boolean | Not listed in the web interface. |
| `web` | boolean \| string | Serve static files from `web/` (or the given directory). Needs `web`. |

`version`, `description` and `author` are read from `package.json` itself. The entry module is
`main`, or `index.ts`/`index.js`/`index.mjs`.

### Settings

Each config field has a `name` (the key in `bot.config`), a `title` and a `type`:

| `type` | Input | Value |
|---|---|---|
| `string`, `password`, `multiline` | text | string |
| `number` | number | number |
| `checkbox` | checkbox | boolean |
| `select` | dropdown of `options` | index of the selected option |
| `strings` | one entry per line | string[] |
| `channel` | channel id | string |
| `track`, `tracks` | track reference | string |
| `array` | list of objects with the sub-fields in `vars` | object[] |

`default` is applied when no value is set, `placeholder` and `indent` affect the form, and
`conditions: [{ field, value }]` shows a field only while other fields have the given values.
This is the same schema as SinusBot's `vars`.

Settings are per instance. After saving, the plugin restarts on that instance. A plugin can
store its own settings with `bot.saveConfig(config)`.

To type `bot.config`, pass the settings' type to `definePlugin`. Fields without a `default` are
missing until the settings are saved, so declare them optional; a `select` holds the option's
index (a number):

```ts
type Config = {
  message: string;          // has a default
  poke: boolean;            // has a default
  channel?: string;         // no default
  mode?: number;            // select: index of the option
};

export default definePlugin<Config>({
  setup(bot) {
    bot.config.message; // string
  },
});
```

Helpers in other files take `Bot<Config>`. Declare the settings with `type`, not `interface`: an
interface is not assignable to the untyped `Bot`. The values are not checked against the type
at runtime.

## Lifecycle

`setup(bot)` runs once for every instance the plugin is active on, when the instance starts or the
plugin is enabled. The `bot` object is scoped to that instance. A plugin that should do something
once for all instances keeps that state at module level: all instances share one worker thread.
SinusBot scripts get one worker per instance.

The plugin stops on an instance when the instance stops, the plugin is disabled or its settings
change. Clean up with any of these:

- return a function from `setup`,
- register handlers with `bot.onStop(fn)`,
- listen to `bot.signal` (an `AbortSignal`), e.g. `fetch(url, { signal: bot.signal })`.

Event handlers, `bot.setTimeout`/`bot.setInterval` timers and chat commands are removed
automatically. Global `setTimeout` timers are not, so use `bot.setTimeout` instead.

Errors thrown in event handlers, timers and promises are logged and do not stop the plugin. If the
plugin's worker or process dies (for example `process.exit()` or running out of memory), it is
restarted after 1, 5 and 15 seconds. After the third crash within five minutes the plugin is marked
as *failed* and stays off until an administrator clicks **Try again**, enables it again or rescans.
A plugin whose module fails to load is marked as failed with the error.

## Events

```ts
const off = bot.on('clientMove', ({ client, from, to, reason }) => { … });
off(); // unsubscribe
bot.once('connect', () => …);
```

| Event | Payload | When |
|---|---|---|
| `connect` | `{ server }` | Connected to the server (also after reconnects). |
| `disconnect` | `{ reason }` | Connection lost or closed. |
| `message` | `{ text, mode, client, channel }` | Chat message (`mode`: `client`, `channel`, `server`). The bot's own messages are not delivered. |
| `poke` | `{ text, client }` | The bot was poked. |
| `clientJoin` | `{ client }` | A client connected. |
| `clientLeave` | `{ client, reason, message, invoker }` | A client left; `reason`: `disconnect`, `timeout`, `kicked`, `banned`, `serverShutdown`, `invisible`. |
| `clientMove` | `{ client, from, to, invoker, reason }` | Channel switch; `reason`: `switched`, `moved`, `kicked` (from the channel), `visible`. |
| `clientUpdate` | `{ client, previous }` | Any client property changed. |
| `clientNick` | `{ client, oldName }` | Nickname changed. |
| `clientAway` / `clientBack` | `{ client, message }` / `{ client }` | Away status. |
| `clientMute` / `clientUnmute` | `{ client }` | Microphone muted / unmuted. |
| `clientDeaf` / `clientUndeaf` | `{ client }` | Sound muted / unmuted. |
| `clientRecord` / `clientRecordStop` | `{ client }` | Recording started / stopped. |
| `clientDescription` | `{ client, previous }` | Description changed. |
| `clientInfo` | `{ client }` | Connection data or details arrived (after `client.requestInfo()`). |
| `serverGroupAdded` / `serverGroupRemoved` | `{ client, group }` | Server group membership changed. |
| `channelGroupChanged` | `{ client, group, previous }` | Channel group changed. |
| `channelCreate` / `channelUpdate` / `channelDelete` | `{ channel, invoker }` (`previous` on update) | Channel changes. |
| `serverUpdate` | `{ server }` | Server properties changed. |
| `groupsUpdate` | `{ serverGroups, channelGroups }` | Groups were added, renamed or removed. |
| `trackStart` | `{ item }` | A track started playing. |
| `trackEnd` | `{ item, error }` | A track ended (`error` is set when it failed). |
| `queueChange` | `{ queue }` | The queue changed. |
| `playbackChange` | `{ status }` | Pause, volume, repeat, position jumps, … |
| `speechStart` / `speechEnd` | `{ id, text }` / `{ id, error, stopped }` | Text to speech started / ended. |
| `speechRecognized` | `{ text, language, client }` | A client said something in the bot's channel (needs the `listen` permission and speech recognition switched on for the instance; other bots are not listened to). SinusBot scripts get it as `speech`. |
| `ws.connect` / `ws.message` / `ws.close` | `{ connection, user, data }` | WebSocket of the plugin's web page ([web](#web-pages-and-http)). |
| `custom:<name>` | `{ name, data, from }` | Custom event sent with `bot.emit(name, data)` by any plugin. |

`bot.emit(name, data)` reaches all plugins on the same instance, `bot.emit(name, data,
{ broadcast: true })` all plugins on every instance.

## Server state

Reads are synchronous: James keeps a mirror of the server in the plugin's worker.

```ts
bot.connected;            // boolean
bot.server;               // name, version, welcome message, banner, … (null while offline)
bot.self;                 // the bot's own client
bot.clients();            // all visible clients
bot.client(id); bot.clientByUid(uid); bot.clientByName(name);
bot.channels();           // in tree order
bot.channel(id); bot.channelByName(name); bot.channelsByName(name);
bot.serverGroups(); bot.serverGroup(id); bot.channelGroups(); bot.channelGroup(id);
```

`Client` and `Channel` objects always show the latest state, and the same id always returns the
same object, so `===` works. After a client leaves, its object keeps the last known data and
`client.online` is `false`. Fields include `uid`, `name`, `channel`, `serverGroups`, `databaseId`,
`away`, `inputMuted`, `outputMuted`, `recording`, `description`, `country`, `talkPower`, `info`
(version, platform, creation time, connection count) and `connection` (ping, packet loss, bytes,
idle time, IP address). TeamSpeak sends `info` and `connection` only on request (at most every
10 seconds per client and for 10 clients per second, to stay clear of the server's anti-flood
protection). The IP address is personal data: plugins only receive it with the `moderation`
permission, the web interface never, and the bot needs the TeamSpeak permission
`b_client_remoteaddress_view` to see it.

```ts
await client.requestInfo();
bot.once('clientInfo', ({ client }) => bot.log.info(client.connection?.pingMs));
```

Actions return promises and reject with a readable error, for example when the bot lacks
TeamSpeak permissions:

```ts
await client.poke('Hello');
await client.moveTo(channel);               // moderation
await client.kick('Bye'); await client.kickFromChannel();
await client.ban(3600, 'Spam');             // seconds, 0 = permanent
await client.addServerGroup(group); await client.removeServerGroup('7');
await client.setChannelGroup(group, channel);
await bot.move(channel, password);          // the bot itself
await bot.setNick('James'); await bot.setDescription('…'); await bot.setAway('brb'); await bot.setAway(null);
const created = await bot.createChannel({ name: 'Room', parent: lobby.id, maxClients: 5 });  // channels
await created.update({ topic: 'Hi', password: 'secret' });
await created.moveTo(null, '0');            // top level, first
await created.delete(true);                 // also when not empty
await channel.subscribe(false);             // stop seeing the clients inside (TeamSpeak; the bot
                                            // subscribes to all channels when it connects)
```

## Chat and commands

```ts
await bot.chatServer('Hello everyone');
await bot.chatChannel('Hello channel');   // the channel the bot is in
await client.chat('Hello you');

bot.commands.register({
  name: 'roll',
  aliases: ['dice'],
  usage: '[sides]',
  description: 'Rolls a die',
  permission: 'roll',              // users need plugin.<id>.roll (omit: everyone may use it)
  run: ({ argv, invoker, reply }) => {
    const sides = Number(argv[0] ?? 6);
    return `${invoker.name} rolled ${1 + Math.floor(Math.random() * sides)}`;
  },
});
```

Commands use the instance's prefix (`bot.commands.prefix`, default `!`) and show up in `!help` on
the instances the plugin is active on. Names are global: registering a name that another plugin or
a built-in command uses fails. `argv` splits the arguments at spaces, with double quotes grouping
words. A returned string is sent back where the command came from; `reply(text)` sends further
messages. Chat and command input comes from users, so validate it.

Command permissions are namespaced to the plugin and granted through roles like built-in
permissions (`plugin.<id>.<name>`, or `plugin.<id>.*` for all of a plugin's permissions).

## Playback

```ts
bot.player.status;     // { current, paused, volume, repeat, shuffle, positionMs, … }
bot.player.queue;      // QueueItem[]
await bot.player.play('https://www.youtube.com/watch?v=…');  // append (default)
await bot.player.play('yt: lofi beats', 'now');               // replace the current track
const [first] = await bot.player.search('daft punk');
await bot.player.play(first, 'next');
await bot.player.skip(); await bot.player.previous(); await bot.player.stop();
await bot.player.pause(); await bot.player.pause(false);
await bot.player.seek(90_000); await bot.player.setVolume(40);
await bot.player.setRepeat('all'); await bot.player.setShuffle(true);
await bot.player.remove(item.id); await bot.player.move(item.id, 0); await bot.player.jump(item.id);

await bot.playlists.play(3, { start: 2 });
const tracks = await bot.library.search('beatles', { limit: 10 });
```

Tracks a plugin adds are shown as requested by the plugin. Changing saved playlists needs the
`playlists` permission.

## Text to speech

```ts
await bot.say('Welcome!');                              // the instance's voice
await bot.say('Good evening', { language: 'en' });      // the best English voice
const voices = await bot.tts.voices();                  // [{ id, name, language, provider }]
await bot.say('Hallo', { voice: voices[0].id });
await bot.tts.stop();                                   // cancel the playing and queued speech
```

Speech plays over the music, which is lowered meanwhile, and is queued when several requests come
in. Texts are at most 500 characters. Which voices exist depends on the server: see
[Text to speech](../tts.md).

## Store

Persistent key-value storage with synchronous reads. Values must be JSON-serialisable.

```ts
const visits = bot.store.instance.get<number>('visits') ?? 0;
bot.store.instance.set('visits', visits + 1);
```

| Store | Shared by | Limits |
|---|---|---|
| `bot.store.instance` | this plugin on this instance | 10,000 keys, 64 MiB |
| `bot.store.plugin` | this plugin on all instances | 10,000 keys, 64 MiB |
| `bot.store.global` | all plugins on all instances | 1,000 keys, 4 MiB |

Keys are 1–256 bytes and a value is at most 1 MiB; `set` throws when these are exceeded. Writes
are saved in the background in the order they were made. When a store is full, the write is
rejected and logged. For larger data use files in `bot.dataDir` (the plugin's private directory)
or a SQLite database there (`db` permission, `node:sqlite`).

## Web pages and HTTP

With the `web` permission a plugin can serve static files and handle requests:

```ts
bot.web.handle('stats', (request) => ({ clients: bot.clients().length }));
bot.web.handle('skip', () => bot.player.skip(), { permission: 'control' }); // needs plugin.<id>.control
bot.web.handle('webhook', (request) => { … }, { public: true });

bot.on('ws.connect', ({ connection, user }) => bot.web.send({ hello: user?.username }, connection));
bot.on('ws.message', ({ connection, data }) => { … });
bot.web.send('to everyone');
```

| Endpoint | Access |
|---|---|
| `GET /plugins/<id>/…` | Static files from the plugin's `web/` directory (sandboxed, see below) |
| `POST /api/v1/instances/<instance>/plugins/<id>/events/<name>` | Signed-in users with the handler's `permission` (`plugin.<id>.<name>`) or, without one, `plugins.use` |
| `POST /api/v1/public/instances/<instance>/plugins/<id>/events/<name>` | Anyone, only for handlers registered with `public: true`; rate-limited, errors are not shown |
| `GET /api/v1/instances/<instance>/plugins/<id>/ws` | WebSocket for users with `plugins.use` |

The request body must be JSON (or empty), and the handler's return value is sent as JSON. The
request carries the signed-in `user` (`id`, `username`), the `query` parameters and the client's
`remoteAddr`. Public handlers receive requests from anyone on the internet, so treat their input
as hostile.

**Plugin pages are sandboxed.** They are served with `Content-Security-Policy: sandbox`, so they run
in an opaque origin and cannot use the viewer's session or call the James API. The web interface
opens a page for one instance with a short-lived token in the URL fragment, which is only valid for
that plugin's endpoints on that instance and only with the viewer's permissions. A small helper
makes this transparent:

```html
<script src="/plugins/_/james.js"></script>
<script>
  if (James.ready) {
    James.call('stats').then((stats) => console.log(stats));      // POST …/events/stats
    const socket = James.connect((data) => console.log(data));     // the plugin's WebSocket
  }
</script>
```

Pages opened directly (without the web interface) have no token: `James.ready` is `false`.

To keep a page from passing on what users type, it may only load files from James and send
requests to James: images, scripts, styles and fonts from other sites are blocked, as are
pop-ups and embedded frames. Ship assets in the `web/` directory, and fetch external data in
the plugin (`fetch` with the `http` permission) and hand it to the page through an endpoint.

## Permissions

| Permission | Allows | Enforced by |
|---|---|---|
| `moderation` | Kick, ban and move clients, change server and channel groups, see client IP addresses | James |
| `listen` | Receive what clients say in the bot's channel (speech recognition); a plugin may store or pass it on | James |
| `channels` | Create, edit, move and delete channels | James |
| `playlists` | Create, change and delete saved playlists | James |
| `instances` | Start, stop and inspect all instances (also act on instances the plugin is not active on) | James |
| `users` | Read and manage James accounts, except accounts with administrative permissions; cannot grant such permissions or take over identities linked to other accounts | James |
| `web` | Web pages, HTTP endpoints and WebSockets | James |
| `fs` | Read and write all files the bot user can access, including James' database, the bots' identities and the YouTube cookies; without it a plugin can only read its own files and read/write its data directory | process sandbox |
| `process` | `node:child_process`, `node:cluster`, native addons | process sandbox |
| `http` | `fetch()`, `WebSocket`, `node:http(s)` | module guard |
| `net` | `node:net`, `node:tls`, `node:dgram` | module guard |
| `db` | `node:sqlite` (files in the data directory unless `fs` is granted) | module guard |

Everything else needs no permission: reading the server state, chatting, poking, playback and the
plugin's own stores. Plugins receive all chat messages of their instances, private messages and
command lines included.

- **James** checks the permission on every call. Each plugin runs in its own process, and James
  knows which plugin a call comes from, so a plugin cannot act with another plugin's permissions.
- **Process sandbox** means Node's permission model, set when James starts the plugin's process.
- **Module guard** stops a plugin from loading these modules by accident, but determined code can
  get around it. `fetch` and Node's default HTTP agent go through James' network policy
  (`[network]` in [`james.example.toml`](../../james.example.toml)), which blocks private and
  internal addresses. Libraries that create their own HTTP agent or open sockets directly are not
  covered.

**Plugins are trusted code.** Only install plugins you trust, like any other software on your
server.

## Accounts and instances

With `users`:

```ts
const account = await client.user();          // the James account linked to this client
await bot.users.create('alice', 'password');
await bot.users.link(account.id, 'teamspeak', client.uid);
await bot.users.setRoles(account.id, ['dj']);
```

`bot.instances.list()` lists the instances the plugin is active on. With `instances` it lists all
of them and `start`, `stop` and `restart` work.

## Logging and errors

`bot.log.info(…)`, `warn`, `error` and `debug` (as well as `console.*`) write to the James log,
tagged with the plugin and instance, and to the instance's log in the web interface (Logs button on
the instance page; debug messages only when James' log level includes debug). Do not log secrets:
whoever may read an instance's log sees them. `bot.notify(message)` shows a message to the users of the web
interface.

## Development tips

- **Rescan** in the web interface (or `POST /api/v1/plugins/reload`) restarts all plugins after
  code changes.
- Native plugins can use npm packages: run `npm install` in the plugin directory.
- Plugin ids are lowercase letters, digits, `-` and `_`.
- A plugin directory may not contain symbolic links pointing outside of it, nor hard links:
  James refuses such plugins, as they would let the plugin read files it should not.
- Plugin processes run in the plugin's data directory (`bot.dataDir`).
- A plugin runs in a worker thread with a 256 MiB heap limit. Do not block the thread for long
  (CPU-heavy work belongs in a nested worker or child process).
- The plugin host itself has tests showing complete plugins: `plugin-host/test/`.

## Examples

| Example | Shows |
|---|---|
| [`examples/plugins/welcome`](https://github.com/Hudint/James/tree/main/examples/plugins/welcome) | Events, settings, per-instance store |
| [`examples/plugins/now-playing`](https://github.com/Hudint/James/tree/main/examples/plugins/now-playing) | Web page with a WebSocket and an HTTP endpoint, playback events |
| [`examples/plugins/afk-mover`](https://github.com/Hudint/James/tree/main/examples/plugins/afk-mover) | Moderation, timers, `requestInfo` |
| [`examples/plugins/sinusbot-greeter.js`](../../examples/plugins/sinusbot-greeter.js) | An unmodified SinusBot script |
