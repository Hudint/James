# Plugin host protocol

For contributors. Plugin authors do not need this; see [README.md](README.md).

James runs `node plugin-host/src/main.ts` as

- a **discovery process** (short-lived, may only read the plugin directory, cannot call
  anything), and
- one **plugin process per running plugin**, with Node's permission model limiting file access
  to the plugin's files and data directory (unless it holds `fs`) and child processes and native
  addons (unless it holds `process`).

Each process gets one end of a Unix socket pair as file descriptor 3. There is no socket file, so
no other process can connect, and James binds the plugin id to the connection: a plugin can only
act as itself. Output goes to the James log. A plugin process that exits is restarted after 1, 5
and 15 seconds; after three crashes within five minutes the plugin is marked as failed.

## Framing

Each frame is a 4-byte big-endian length followed by a UTF-8 JSON object of at most 16 MiB:

| Frame | Shape |
|---|---|
| Request | `{"t":"req","id":<n>,"method":"…","params":…}`, answered by exactly one response |
| Response | `{"t":"res","id":<n>,"result":…}` or `{"t":"res","id":<n>,"error":"…"}` |
| Event | `{"t":"evt","name":"…","data":…}` |

Both sides send requests and events. Ids are per sender. The first request is `init`; its
`protocol` field is the protocol version (currently 1), and the host refuses other versions.

## James → host

| Request | Params | Result |
|---|---|---|
| `init` | `protocol`, `version` (James) | `{ protocol, node }` |
| `discover` (discovery process) | `pluginsDir` | `{ plugins: Manifest[], errors: [{ id, error }] }`; manifests include `paths` (`dir`, `entry`, `readable`) and `webDir`, which James checks |
| `plugin.load` | `id`, `permissions` (granted), `manifest`, `dir`, `entry`, `dataDir` | – (native plugins are imported now) |
| `plugin.unload` | `id` | – (runs `unload` handlers) |
| `context.start` | `instance`, `platform`, `config` | – (after `setup(bot)` finished) |
| `context.stop` | `instance` | – |
| `command.run` | `instance`, `name`, `args`, `mode`, `invoker` | reply text or `null` |
| `http.event` | `instance`, `request` (`HttpEvent`) | handler result |

| Event | Data |
|---|---|
| `instance.event` | an `EventEnvelope` (instance events, including platform events) plus `sensitive` |
| `ws.open` | `instance`, `connection`, `user` |
| `ws.message` | `instance`, `connection`, `data` |
| `ws.close` | `instance`, `connection` |
| `plugin.event` | `instance` (or `null` = broadcast), `name`, `data`, `from` (custom events of plugins) |
| `store.changed` | `scope` (`global`), `key`, `value`, `deleted` (another plugin changed the global store) |

James is the source of truth for what runs: it loads enabled plugins whose requested permissions
are granted, and starts one context per running instance the plugin is enabled for. Events are
only forwarded for instances that have contexts.

## Host → James

| Request | Params |
|---|---|
| `call` | `instance` (or `null`), `method`, `params` |

`call` methods are implemented in `crates/james-plugins/src/bridge.rs`, grouped by prefix:
`instance.*` / `instances.*`, `chat.*`, `client.*`, `bot.*`, `channel.*`, `media.*`, `store.*`,
`playlists.*`, `library.*`, `users.*`, `commands.*`, `web.*` and `engine.*`. At most 16 calls of
a plugin run at the same time. Each checks that the plugin is
running, that it is active on the instance (unless it holds `instances`) and that it holds the
permission the method requires.

| Event | Data |
|---|---|
| `log` | `instance`, `level`, `message` (control characters are replaced) |
| `plugin.failed` | `error` (workers crashed repeatedly; James stops the process and marks it as failed) |
| `ws.send` | `instance`, `connection` (or `null` = all), `data`, `close` |
| `emit` | `instance`, `name`, `data`, `broadcast` (custom events, routed to all plugins) |

## Inside the host

- `src/main.ts` runs the IPC connection, the plugin's worker threads and the authoritative copy
  of its stores (writes are sent to James one after another).
- `src/worker/` holds the worker runtime: the per-instance state mirror (`state.ts`), the native
  API (`api.ts`) and the module guard (`guard.ts`).
- `src/compat/` contains the SinusBot layer.

Synchronous calls (SinusBot semantics) block the worker with `Atomics.wait` on a shared buffer.
The main thread answers through a dedicated `MessagePort`, which the worker reads with
`receiveMessageOnPort`.
