/**
 * Plugin API of James.
 *
 * A plugin is a directory with a `package.json` containing a `"james"` section and an entry
 * module whose default export is created with {@link definePlugin}:
 *
 * ```ts
 * import { definePlugin } from '@james/api';
 *
 * export default definePlugin({
 *   setup(bot) {
 *     bot.on('clientJoin', ({ client }) => client.chat(`Welcome, ${client.name}!`));
 *     bot.commands.register({
 *       name: 'hello',
 *       description: 'Says hello',
 *       run: ({ invoker }) => `Hello ${invoker.name}!`,
 *     });
 *   },
 * });
 * ```
 *
 * `setup` runs once per instance the plugin is active on. Reads (clients, channels, queue,
 * store) are synchronous and served from a local mirror; actions return promises.
 *
 * @packageDocumentation
 * @license MIT
 */

// ---- plugin definition ----------------------------------------------------------------------

/** A plugin's settings by name (see {@link ConfigField}); the type when none is declared. */
export type PluginConfig = Record<string, unknown>;

/** What a plugin module exports (see {@link definePlugin}). */
export interface PluginDefinition<Config extends object = PluginConfig> {
  /**
   * Called for every instance the plugin is activated on. May return a cleanup function
   * (also see {@link Bot.onStop}).
   */
  setup(bot: Bot<Config>): void | (() => void | Promise<void>) | Promise<void | (() => void | Promise<void>)>;
}

/**
 * What a plugin in a single file (`plugins/<id>.ts`) declares about itself, next to `setup`.
 * James reads it when it scans the plugins, by running the file's top level in a sandbox where
 * imports do nothing (so `config` may be built with code, e.g. `events.flatMap(…)`); `setup`
 * only runs once the plugin is activated. Plugins in a directory declare this in the
 * `package.json` instead (see {@link Manifest}).
 */
export interface PluginInfo {
  name?: string;
  version?: string;
  description?: string;
  author?: string;
  /** Required James version, e.g. `">=0.6.0"`. */
  engine?: string;
  /** Permissions the plugin needs; an administrator confirms them when activating it. */
  permissions?: readonly Permission[];
  /** Permissions the plugin can use when granted, but runs without. */
  optionalPermissions?: readonly Permission[];
  /** Platforms the plugin supports (default: all). */
  platforms?: readonly Platform[];
  /** Always active on every instance. */
  autorun?: boolean;
  /** Hidden from the plugin list (only sensible with `autorun`). */
  hidden?: boolean;
}

/** The value a setting of `Field`'s type holds. */
export type ConfigFieldValue<Field> = Field extends { type: 'checkbox' }
  ? boolean
  : Field extends { type: 'number' | 'select' }
    ? number
    : Field extends { type: 'strings' | 'tracks' }
      ? string[]
      : Field extends { type: 'array'; vars: infer Vars extends readonly ConfigField[] }
        ? ConfigValues<Vars>[]
        : Field extends { type: 'array' }
          ? Record<string, unknown>[]
          : string;

/**
 * The settings of a list of fields by name: fields with a `default` are always set, the
 * others are missing until an administrator saves them.
 */
export type ConfigValues<Fields extends readonly ConfigField[]> = {
  [F in Fields[number] as F extends { default: unknown } ? F['name'] : never]: ConfigFieldValue<F>;
} & {
  [F in Fields[number] as F extends { default: unknown } ? never : F['name']]?: ConfigFieldValue<F>;
};

/**
 * Declares a plugin (identity function that gives `setup` its types).
 *
 * With `config`, {@link Bot.config} is typed from the fields: a `checkbox` is a boolean, a
 * `number` or `select` (the option's index) a number, and so on. Fields built in a callback
 * (e.g. `flatMap`) need `as const` there to keep their names. In a single-file plugin the
 * other {@link PluginInfo} fields declare the plugin. The values are not checked at runtime.
 */
export function definePlugin<const Fields extends readonly ConfigField[]>(
  definition: PluginInfo & { config: Fields } & PluginDefinition<ConfigValues<Fields>>,
): PluginDefinition<ConfigValues<Fields>>;
/**
 * Declares a plugin without settings, or with settings typed by hand (`definePlugin<Config>`,
 * e.g. for a plugin whose fields are in its package.json).
 */
export function definePlugin<Config extends object = PluginConfig>(
  definition: PluginInfo & PluginDefinition<Config>,
): PluginDefinition<Config>;

/** Keeps config fields defined apart from {@link definePlugin} typed (names and types). */
export function defineConfig<const Fields extends readonly ConfigField[]>(fields: Fields): Fields;

// ---- manifest (package.json "james" section) ------------------------------------------------

/** The `"james"` section of a plugin's `package.json`. */
export interface Manifest {
  /** Display name (defaults to the package name). */
  name?: string;
  /** Required James version, e.g. `">=0.1.0"`. */
  engine?: string;
  /** Permissions the plugin needs; an administrator confirms them when enabling the plugin. */
  permissions?: Permission[];
  /**
   * Permissions the plugin can use when granted but runs without; the administrator picks them
   * when enabling the plugin. Check before use, e.g. by catching the permission error.
   */
  optionalPermissions?: Permission[];
  /** Settings shown in the web interface, per instance. */
  config?: ConfigField[];
  /** Platforms the plugin supports (default: all). */
  platforms?: Platform[];
  /** Always active on every instance. */
  autorun?: boolean;
  /** Hidden from the plugin list (only sensible with `autorun`). */
  hidden?: boolean;
  /** Serve static files from this directory (default `web`) under `/plugins/<id>/`; needs `web`. */
  web?: boolean | string;
}

/** Voice platforms (Discord is planned). */
export type Platform = 'teamspeak' | 'discord';

/**
 * Permissions. `moderation`, `listen`, `channels`, `playlists`, `instances`, `users` and `web` are
 * enforced by James; `fs` and `process` by the plugin's process sandbox (Node's permission
 * model). `http`, `net` and `db` only guard the corresponding Node.js modules; plugins are
 * trusted code, so these are a safety net, not a sandbox.
 */
export type Permission = 'moderation' | 'listen' | 'channels' | 'playlists' | 'instances' | 'users' | 'web' | 'http' | 'net' | 'fs' | 'db' | 'process';

/** A setting in the web interface (same schema as SinusBot's `vars`). */
export interface ConfigField {
  /** Key in {@link Bot.config}. */
  name: string;
  /** Label in the web interface. */
  title: string;
  /**
   * Input kind. `select` stores the option index, `strings` an array of lines, `array` a list
   * of objects (see `vars`), `checkbox` a boolean.
   */
  type: 'string' | 'strings' | 'password' | 'multiline' | 'number' | 'checkbox' | 'select' | 'channel' | 'track' | 'tracks' | 'array';
  /** Value used while none is set. */
  default?: unknown;
  /** Hint shown in empty text and number inputs. */
  placeholder?: string;
  /** `select`: the options; the stored value is the index. */
  options?: readonly string[];
  /** `array`: fields of each item. */
  vars?: readonly ConfigField[];
  /** Only shown when every condition matches another field's value. */
  conditions?: readonly {
    /** `name` of the other field. */
    field: string;
    /** Value the other field must have (compared loosely, so `1` matches `"1"`). */
    value: unknown;
  }[];
  /** Indentation level in the form (e.g. for fields that depend on the one above). */
  indent?: number;
}

// ---- the bot ----------------------------------------------------------------------------------

/** One instance as seen by a plugin. Everything is scoped to this instance. */
export interface Bot<Config extends object = PluginConfig> {
  /** The instance this plugin runs on. */
  readonly instance: InstanceInfo;
  /** This instance's values of the manifest's config fields (defaults applied). */
  readonly config: Readonly<Config>;
  /** Writes to the James log. */
  readonly log: Logger;
  /** Private directory for the plugin's files (shared by all instances). */
  readonly dataDir: string;
  /** Aborted when the plugin stops on this instance. */
  readonly signal: AbortSignal;

  /** Subscribes to an event; returns a function that unsubscribes. */
  on<E extends keyof Events>(event: E, handler: (payload: Events[E]) => void | Promise<void>): () => void;
  /** Custom events from plugins (see {@link Bot.emit}). */
  on(event: `custom:${string}`, handler: (payload: CustomEvent) => void | Promise<void>): () => void;
  /** Like {@link Bot.on}, but unsubscribes after the first event; returns a function that unsubscribes early. */
  once<E extends keyof Events>(event: E, handler: (payload: Events[E]) => void | Promise<void>): () => void;
  /** Sends a custom event (`custom:<name>`) to all plugins on this instance or, with `broadcast`, on every instance. */
  emit(name: string, data?: unknown, options?: { broadcast?: boolean }): void;
  /** Registers cleanup run when the plugin stops on this instance. */
  onStop(handler: () => void | Promise<void>): void;
  /** Timers that are cleared automatically when the plugin stops. */
  setTimeout(handler: () => void, ms: number): () => void;
  /** Runs `handler` every `ms` milliseconds (at least 10); returns a function that cancels it. */
  setInterval(handler: () => void, ms: number): () => void;

  // server state (synchronous)
  /** `true` while connected to the voice server. */
  readonly connected: boolean;
  /** The voice server; `null` while disconnected. */
  readonly server: ServerInfo | null;
  /** The bot's own client while connected. */
  readonly self: Client | null;
  /** All clients the bot currently sees. */
  clients(): Client[];
  /** The client with this session id; `null` if it is not on the server. */
  client(id: string): Client | null;
  /** The first client with this identity (a user may be connected more than once); `null` if none. */
  clientByUid(uid: string): Client | null;
  /** The first client with exactly this nickname; `null` if none. */
  clientByName(name: string): Client | null;
  /** All channels in tree order. */
  channels(): Channel[];
  /** The channel with this id; `null` if it does not exist. */
  channel(id: string): Channel | null;
  /** The first channel (in tree order) with exactly this name; `null` if none. */
  channelByName(name: string): Channel | null;
  /** All channels with exactly this name, in tree order. */
  channelsByName(name: string): Channel[];
  /** All server groups. */
  serverGroups(): Group[];
  /** The server group with this id; `null` if unknown. */
  serverGroup(id: string): Group | null;
  /** All channel groups. */
  channelGroups(): Group[];
  /** The channel group with this id; `null` if unknown. */
  channelGroup(id: string): Group | null;

  // chat
  /** Server-wide message. */
  chatServer(text: string): Promise<void>;
  /** Message to the bot's current channel. */
  chatChannel(text: string): Promise<void>;

  // the bot itself
  /** Moves the bot to a channel; `password` for protected channels. */
  move(channel: Channel | string, password?: string): Promise<void>;
  /** Changes the bot's nickname; rejects unless it is 1–30 characters. */
  setNick(name: string): Promise<void>;
  /** Sets the bot's description (at most 8 KiB). */
  setDescription(text: string): Promise<void>;
  /** Sets the bot away (`null` = back). */
  setAway(message: string | null): Promise<void>;
  /** Creates a channel and returns it (`channels`). */
  createChannel(params: ChannelParams & { name: string }): Promise<Channel>;

  /** Playback and the queue. */
  readonly player: Player;
  /** Saved playlists. */
  readonly playlists: Playlists;
  /** The local music library. */
  readonly library: Library;
  /** Persistent key-value stores in three scopes (see {@link Store} for the limits). */
  readonly store: {
    /** Shared by all instances the plugin runs on (at most 10,000 keys and 64 MiB). */
    readonly plugin: Store;
    /** This plugin on this instance only (at most 10,000 keys and 64 MiB). */
    readonly instance: Store;
    /** Shared by all plugins (at most 1000 keys and 4 MiB in total). */
    readonly global: Store;
  };
  /** Chat commands of this plugin. */
  readonly commands: Commands;
  /** HTTP endpoints and WebSocket connections of the plugin's web page (`web`). */
  readonly web: Web;
  /** James user accounts (`users`). */
  readonly users: Users;
  /** All bot instances (`instances` for anything but listing the plugin's own). */
  readonly instances: Instances;

  /**
   * Speaks `text` over the music (which is ducked meanwhile), with the instance's voice or the
   * best voice for `language` (e.g. `"de"`, `"en-US"`). Resolves with the speech id once it is
   * accepted (rejects for an unknown voice, empty or too long text, or a full queue);
   * `speechStart` / `speechEnd` follow (`speechEnd` carries the error if synthesis failed).
   */
  say(text: string, options?: { voice?: string; language?: string }): Promise<number>;
  /** Text-to-speech voices and control (see {@link Bot.say}). */
  readonly tts: {
    /** Installed voices. */
    voices(): Promise<TtsVoice[]>;
    /** Cancels the playing and queued speech. */
    stop(): Promise<void>;
  };

  /**
   * Shows a message to the users of the web interface (cut to 500 characters). Rejects above
   * 6 messages per minute per plugin (all instances together).
   */
  notify(message: string): Promise<void>;
  /** Replaces this instance's configuration (persisted; {@link Bot.config} keeps the old values until restart). */
  saveConfig(config: Config): Promise<void>;
  /** Version of James. */
  readonly version: string;
}

/** The instance (bot) a plugin runs on. */
export interface InstanceInfo {
  /** Instance id (as used by {@link Instances}). */
  id: string;
  /** Display name. */
  name: string;
  /** Voice platform the instance connects to. */
  platform: Platform | string;
  /** Prefix of chat commands, e.g. `!`. */
  commandPrefix: string;
}

/** Writes to the James log, tagged with the plugin and instance. */
export interface Logger {
  /** Logs at debug level; non-string arguments are formatted like `console.log`. */
  debug(...args: unknown[]): void;
  /** Logs at info level. */
  info(...args: unknown[]): void;
  /** Logs a warning. */
  warn(...args: unknown[]): void;
  /** Logs an error. */
  error(...args: unknown[]): void;
}

// ---- events -------------------------------------------------------------------------------------

/** Where a chat message was written: privately, in the bot's channel or in the server chat. */
export type MessageMode = 'client' | 'channel' | 'server';

/** All events with their payloads, for {@link Bot.on} and {@link Bot.once}. */
export interface Events {
  /** Connected to the server (also after a reconnect). */
  connect: {
    /** The server; `null` if not known yet. */
    server: ServerInfo | null;
  };
  /** Lost the connection to the server (the bot reconnects on its own). */
  disconnect: {
    /** Why the connection was lost (may be empty). */
    reason: string;
  };
  /** A chat message (not the bot's own). Command lines are delivered as well. */
  message: {
    /** The message text. */
    text: string;
    /** Where it was written. */
    mode: MessageMode;
    /** The sender. */
    client: Client;
    /** The bot's channel for `channel` messages, otherwise `null`. */
    channel: Channel | null;
  };
  /** A client poked the bot. */
  poke: {
    /** The poke message. */
    text: string;
    /** Who poked the bot. */
    client: Client;
  };
  /** A client connected to the server. */
  clientJoin: {
    /** The client who joined. */
    client: Client;
  };
  /** A client left the server. */
  clientLeave: {
    /** The client who left (its last known data stays readable). */
    client: Client;
    /** Why it left. */
    reason: LeaveReason;
    /** The leave, kick or ban message; may be empty. */
    message: string;
    /** Who kicked or banned it; `null` if nobody did. */
    invoker: Client | null;
  };
  /** A client switched channels; `from`/`to` are `null` when it became visible/invisible. */
  clientMove: {
    /** The client who moved. */
    client: Client;
    /** The previous channel. */
    from: Channel | null;
    /** The new channel. */
    to: Channel | null;
    /** Who caused the move (may be the client itself); `null` if unknown. */
    invoker: Client | null;
    /** Why it moved. */
    reason: MoveReason;
  };
  /** Any property of a client changed. */
  clientUpdate: {
    /** The client with its new data. */
    client: Client;
    /** Its data before the change (the current data when the client was not known before). */
    previous: ClientData;
  };
  /** A client changed its nickname. */
  clientNick: {
    /** The client with its new nickname. */
    client: Client;
    /** The previous nickname. */
    oldName: string;
  };
  /** A client set itself away (`message` may be empty). */
  clientAway: {
    /** The client who is away. */
    client: Client;
    /** The away message. */
    message: string;
  };
  /** A client is no longer away. */
  clientBack: {
    /** The client who is back. */
    client: Client;
  };
  /** A client muted its microphone. */
  clientMute: {
    /** The client. */
    client: Client;
  };
  /** A client unmuted its microphone. */
  clientUnmute: {
    /** The client. */
    client: Client;
  };
  /** A client muted its speakers. */
  clientDeaf: {
    /** The client. */
    client: Client;
  };
  /** A client unmuted its speakers. */
  clientUndeaf: {
    /** The client. */
    client: Client;
  };
  /** A client started recording. */
  clientRecord: {
    /** The client. */
    client: Client;
  };
  /** A client stopped recording. */
  clientRecordStop: {
    /** The client. */
    client: Client;
  };
  /** A client's description changed. */
  clientDescription: {
    /** The client with its new description. */
    client: Client;
    /** The previous description. */
    previous: string;
  };
  /** Fresh connection data or client details arrived (after {@link Client.requestInfo}). */
  clientInfo: {
    /** The client; see {@link ClientData.info} and {@link ClientData.connection}. */
    client: Client;
  };
  /** A client was added to a server group. */
  serverGroupAdded: {
    /** The client. */
    client: Client;
    /** The group (with an empty name if the group is unknown). */
    group: Group;
  };
  /** A client was removed from a server group. */
  serverGroupRemoved: {
    /** The client. */
    client: Client;
    /** The group (with an empty name if the group is unknown). */
    group: Group;
  };
  /** A client's channel group changed. */
  channelGroupChanged: {
    /** The client. */
    client: Client;
    /** The new channel group; `null` if none. */
    group: Group | null;
    /** The previous channel group; `null` if none. */
    previous: Group | null;
  };
  /** A channel was created. */
  channelCreate: {
    /** The new channel. */
    channel: Channel;
    /** Who created it; `null` if unknown. */
    invoker: Client | null;
  };
  /** A channel was edited (name, topic, order, …). */
  channelUpdate: {
    /** The channel with its new data. */
    channel: Channel;
    /** Its data before the change. */
    previous: ChannelData;
    /** Who edited it; `null` if unknown. */
    invoker: Client | null;
  };
  /** A channel was deleted; `channel` is its last known data. */
  channelDelete: {
    /** The channel's last known data. */
    channel: ChannelData;
    /** Who deleted it; `null` if unknown. */
    invoker: Client | null;
  };
  /** The server's settings (name, banner, …) changed. */
  serverUpdate: {
    /** The new server settings. */
    server: ServerInfo;
  };
  /** Server or channel groups were created, renamed or deleted. */
  groupsUpdate: {
    /** All server groups after the change. */
    serverGroups: Group[];
    /** All channel groups after the change. */
    channelGroups: Group[];
  };
  /** A track started playing. */
  trackStart: {
    /** The track that started. */
    item: QueueItem;
  };
  /** A track ended; `error` is set when it failed. */
  trackEnd: {
    /** The track that ended. */
    item: QueueItem;
    /** Why playback failed; `null` if it ended normally. */
    error: string | null;
  };
  /** The queue changed (added, removed, moved, shuffled, cleared). */
  queueChange: {
    /** The new queue (upcoming tracks, without the current one). */
    queue: QueueItem[];
  };
  /** Pause, volume, repeat, shuffle or the position (seek) changed. */
  playbackChange: {
    /** The new status. */
    status: PlayerStatus;
  };
  /** Speech (from `bot.say`, `!say`, the web interface or another plugin) started. */
  speechStart: {
    /** Speech id (as returned by {@link Bot.say}). */
    id: number;
    /** The text being spoken. */
    text: string;
  };
  /** Speech ended; `error` if synthesis or playback failed, `stopped` if it was cancelled. */
  speechEnd: {
    /** Speech id (as returned by {@link Bot.say}). */
    id: number;
    /** Why it failed; `null` on success. */
    error: string | null;
    /** `true` if it was cancelled before it finished. */
    stopped: boolean;
  };
  /**
   * A client said something in the bot's channel (`listen`; the instance must have speech
   * recognition switched on). Utterances end at a pause; recognition takes about as long as
   * the utterance. Other bots are not listened to.
   */
  speechRecognized: {
    /** What was recognised. */
    text: string;
    /** Detected or configured language (ISO 639-1, e.g. `de`); `null` if unknown. */
    language: string | null;
    /** The speaking client. */
    client: Client;
  };
  /** A WebSocket connection of the plugin's web page opened (`web`). */
  'ws.connect': {
    /** Connection id, for {@link Web.send} and {@link Web.close}. */
    connection: string;
    /** The signed-in web interface account that opened it. */
    user: WebUser | null;
  };
  /** A message from a WebSocket of the plugin's web page. */
  'ws.message': {
    /** Connection id. */
    connection: string;
    /** The message text, as sent by the browser. */
    data: string;
  };
  /** A WebSocket connection of the plugin's web page closed. */
  'ws.close': {
    /** Connection id. */
    connection: string;
  };
}

/** Why a client left the server. */
export type LeaveReason = 'disconnect' | 'timeout' | 'kicked' | 'banned' | 'serverShutdown' | 'invisible';
/** Why a client changed channels. */
export type MoveReason = 'switched' | 'moved' | 'kicked' | 'visible';

/** An event sent by a plugin with {@link Bot.emit}, received as `custom:<name>`. */
export interface CustomEvent {
  /** Event name, without the `custom:` prefix. */
  name: string;
  /** The data passed to {@link Bot.emit}; `null` if none. */
  data: unknown;
  /** Id of the plugin that sent it. */
  from: string;
}

// ---- server objects -------------------------------------------------------------------------

/** Plain data of a client (as delivered by the platform). */
export interface ClientData {
  /** Session id on the server; changes when the client reconnects (see `uid`). */
  id: string;
  /** Stable identity (TeamSpeak UID). */
  uid: string;
  /** Nickname. */
  name: string;
  /** Id of the current channel; `null` if unknown. */
  channel: string | null;
  /** Ids of the client's server groups (see {@link Client.serverGroupObjects}). */
  serverGroups: string[];
  /** `true` for the bot itself. */
  isSelf: boolean;
  /** Permanent database id (TeamSpeak); `null` if unknown. */
  databaseId: string | null;
  /** Id of the channel group in the current channel; `null` if none. */
  channelGroup: string | null;
  /** The client's description. */
  description: string;
  /** Phonetic nickname. */
  phoneticName: string;
  /** Away message; `null` when not away. */
  away: string | null;
  /** Microphone muted. */
  inputMuted: boolean;
  /** Speakers muted. */
  outputMuted: boolean;
  /** Recording the conversation. */
  recording: boolean;
  /** Talking right now. */
  talking: boolean;
  /** ISO country code; empty if the server does not report it. */
  country: string;
  /** A server query client, not a normal user. */
  query: boolean;
  /** The client's talk power. */
  talkPower: number;
  /** Is a priority speaker. */
  prioritySpeaker: boolean;
  /** Is channel commander. */
  channelCommander: boolean;
  /** Client software details; `null` until loaded (see {@link Client.requestInfo}). */
  info: {
    /** Client version. */
    version: string;
    /** Client operating system. */
    platform: string;
    /** First connection to the server, Unix seconds. */
    created: number;
    /** Last connection, Unix seconds. */
    lastConnected: number;
    /** Number of connections to the server. */
    totalConnections: number;
  } | null;
  /** Network details; `null` until loaded (see {@link Client.requestInfo}). */
  connection: ConnectionInfo | null;
}

/** Network details of a client (needs {@link Client.requestInfo} first). */
export interface ConnectionInfo {
  /** Ping in milliseconds; `null` if not reported. */
  pingMs: number | null;
  /** Packet loss from 0 to 1; `null` if not reported. */
  packetLoss: number | null;
  /** Bytes sent over the connection; `null` if not reported. */
  bytesSent: number | null;
  /** Bytes received over the connection; `null` if not reported. */
  bytesReceived: number | null;
  /** Time connected, in milliseconds; `null` if not reported. */
  connectedMs: number | null;
  /** Time since the client's last activity, in milliseconds; `null` if not reported. */
  idleMs: number | null;
  /**
   * IP address; `null` unless the plugin has the `moderation` permission and the bot the server
   * permission to see it (`b_client_remoteaddress_view` on TeamSpeak).
   */
  address: string | null;
}

/** A client on the server. Properties always reflect the latest known state. */
export interface Client extends Readonly<ClientData> {
  /** The client's current channel. */
  channelObject(): Channel | null;
  /** The client's server groups (unknown groups have an empty name). */
  serverGroupObjects(): Group[];
  /** `true` while the client is still on the server. */
  readonly online: boolean;
  /** `true` if `other` (a client or session id) is this client. */
  equals(other: Client | string): boolean;
  /** Sends a private message (at most 8 KiB). */
  chat(text: string): Promise<void>;
  /** Pokes the client with a message (the platform's limit applies: about 100 characters on TeamSpeak). */
  poke(text: string): Promise<void>;
  /**
   * Asks the server for connection data and details; a `clientInfo` event follows. Rejects with
   * an error starting with `throttled` when the client's info was requested in the last 10 seconds
   * (by any plugin) or too many clients were requested this second.
   */
  requestInfo(): Promise<void>;
  /** `moderation` */
  kick(reason?: string): Promise<void>;
  /** `moderation` */
  kickFromChannel(reason?: string): Promise<void>;
  /** `moderation`; `seconds` = 0 bans permanently. */
  ban(seconds: number, reason?: string): Promise<void>;
  /** `moderation` */
  moveTo(channel: Channel | string): Promise<void>;
  /** `moderation` */
  addServerGroup(group: Group | string): Promise<void>;
  /** `moderation` */
  removeServerGroup(group: Group | string): Promise<void>;
  /** `moderation` */
  setChannelGroup(group: Group | string, channel?: Channel | string): Promise<void>;
  /** The James account linked to this client's identity (`users`). */
  user(): Promise<UserAccount | null>;
}

/** The data of a channel (also kept for deleted channels). */
export interface ChannelData {
  /** Channel id. */
  id: string;
  /** Id of the parent channel; `null` at the top level. */
  parent: string | null;
  /** Channel name. */
  name: string;
  /** Channel topic. */
  topic: string;
  /** Channel description. */
  description: string;
  /** Codec id (TeamSpeak: 4 = Opus Voice, 5 = Opus Music); `null` if unknown. */
  codec: number | null;
  /** Codec quality; `null` if unknown. */
  codecQuality: number | null;
  /** Position in tree order (depth first); {@link Bot.channels} is sorted by it. */
  order: number;
  /** Maximum number of clients; `null` = unlimited. */
  maxClients: number | null;
  /** Maximum number of clients in the channel and its sub-channels; `null` = unlimited. */
  maxFamilyClients: number | null;
  /** Permanent channel. */
  permanent: boolean;
  /** Semi-permanent channel (kept until the server restarts). */
  semiPermanent: boolean;
  /** The server's default channel. */
  default: boolean;
  /** Protected by a password. */
  hasPassword: boolean;
  /** Voice data is encrypted. */
  encrypted: boolean;
  /** Talk power needed to speak in the channel. */
  neededTalkPower: number;
  /** Seconds before an empty temporary channel is deleted. */
  deleteDelay: number;
  /** Phonetic name. */
  phoneticName: string;
  /** Icon id; 0 = none. */
  icon: number;
  /** Text-only channel (Discord); TeamSpeak channels are always voice channels. */
  text: boolean;
  /** The bot is subscribed and sees the clients inside (see {@link Channel.subscribe}). */
  subscribed: boolean;
}

/** A channel on the server, with actions (needs the `channels` permission to change). */
export interface Channel extends Readonly<ChannelData> {
  /** The parent channel; `null` at the top level. */
  parentChannel(): Channel | null;
  /** Direct sub-channels, in tree order. */
  children(): Channel[];
  /** The clients in this channel that the bot sees. */
  clients(): Client[];
  /** `false` once the channel was deleted. */
  readonly exists: boolean;
  /** `true` if `other` (a channel or channel id) is this channel. */
  equals(other: Channel | string): boolean;
  /** Writes into the channel (only the channel the bot is in). */
  chat(text: string): Promise<void>;
  /** `channels` */
  update(params: ChannelParams): Promise<void>;
  /** `channels`; `order` = id of the sibling above ("0" = first). */
  moveTo(parent: Channel | string | null, order?: string): Promise<void>;
  /** `channels` */
  delete(force?: boolean): Promise<void>;
  /**
   * Sees the clients inside (TeamSpeak subscription), or no longer (`false`). On TeamSpeak the
   * bot subscribes to every channel it may when it (re)connects, as the TeamSpeak client does;
   * an unsubscribe lasts until the next reconnect.
   */
  subscribe(subscribe?: boolean): Promise<void>;
}

/** Settings of a new or edited channel. */
export interface ChannelParams {
  /** Channel name (required to create one). */
  name?: string;
  /** Parent channel id (`null` = top level); only used when creating, see {@link Channel.moveTo}. */
  parent?: string | null;
  /** Channel topic. */
  topic?: string;
  /** Channel description. */
  description?: string;
  /** Empty string removes the password. */
  password?: string;
  /** Codec id (TeamSpeak: 4 = Opus Voice, 5 = Opus Music). */
  codec?: number;
  /** Codec quality. */
  codecQuality?: number;
  /** Encrypt voice data. */
  encrypted?: boolean;
  /** Make the channel permanent. */
  permanent?: boolean;
  /** Make the channel semi-permanent (kept until the server restarts). */
  semiPermanent?: boolean;
  /** Make it the server's default channel. */
  default?: boolean;
  /** Id of the sibling above. */
  position?: string;
  /** -1 = unlimited. */
  maxClients?: number;
  /** Maximum number of clients in the channel and its sub-channels; -1 = unlimited. */
  maxFamilyClients?: number;
  /** Talk power needed to speak in the channel. */
  neededTalkPower?: number;
  /** Seconds before an empty temporary channel is deleted. */
  deleteDelay?: number;
  /** Phonetic name. */
  phoneticName?: string;
}

/** An installed text-to-speech voice. */
export interface TtsVoice {
  /** Voice id, e.g. `de_DE-thorsten-medium` (for the `voice` option of {@link Bot.say}). */
  id: string;
  /** Display name. */
  name: string;
  /** BCP 47 tag, e.g. `de-DE`. */
  language: string;
  /** Id of the TTS provider, e.g. `piper`. */
  provider: string;
}

/** A server or channel group. */
export interface Group {
  /** Group id. */
  id: string;
  /** Group name. */
  name: string;
  /** Icon id; 0 = none. */
  icon: number;
}

/** The voice server the bot is connected to. */
export interface ServerInfo {
  /** Server id (TeamSpeak: virtual server id). */
  id: string;
  /** Unique server identifier. */
  uid: string;
  /** Server name. */
  name: string;
  /** Server software version. */
  version: string;
  /** Operating system of the server. */
  platform: string;
  /** Welcome message shown to connecting clients. */
  welcomeMessage: string;
  /** Phonetic name. */
  phoneticName: string;
  /** Host message. */
  hostMessage: string;
  /** Link of the host banner. */
  hostBannerUrl: string;
  /** Image URL of the host banner. */
  hostBannerGfxUrl: string;
  /** Reload interval of the banner image, in seconds. */
  hostBannerGfxInterval: number;
  /** Link of the host button. */
  hostButtonUrl: string;
  /** Image URL of the host button. */
  hostButtonGfxUrl: string;
  /** Icon id; 0 = none. */
  icon: number;
  /** Maximum number of clients. */
  maxClients: number;
  /** Id of the group new clients get; `null` if unknown. */
  defaultServerGroup: string | null;
  /** Id of the channel group clients get by default; `null` if unknown. */
  defaultChannelGroup: string | null;
  /** TeamSpeak codec encryption: 0 = per channel, 1 = forced off, 2 = forced on. */
  codecEncryptionMode: number;
  /** Creation time, Unix seconds. */
  created: number;
}

// ---- playback -----------------------------------------------------------------------------------

/** A playable track: a URL, stream or library file with its metadata. */
export interface Track {
  /** Source id (`ytdlp`, `stream`, `local`, …). */
  source: string;
  /** Source-specific locator (URL, file path, …). */
  locator: string;
  /** Track title. */
  title: string;
  /** Artist; `null` if unknown. */
  artist?: string | null;
  /** Album; `null` if unknown. */
  album?: string | null;
  /** Duration in milliseconds; `null` if unknown. */
  durationMs?: number | null;
  /** Thumbnail image URL; `null` if none. */
  thumbnail?: string | null;
  /** Live stream: no end and no seeking. */
  isStream?: boolean;
}

/** A track in the queue, with who added it. */
export interface QueueItem {
  /** Unique within the instance and stable while the item exists (for remove, move, jump). */
  id: number;
  /** The track. */
  track: Track;
  /** Who added it; `null` if unknown. */
  requestedBy: {
    /** Identity of the requester: a platform UID, `user:<account id>` (web interface) or `plugin:<id>`. */
    uid: string;
    /** Display name of the requester. */
    name: string;
  } | null;
}

/** Where new tracks go: play now, after the current track, or at the end. */
export type PlayMode = 'now' | 'next' | 'append';
/** Repeat nothing, the current track or the whole queue. */
export type RepeatMode = 'off' | 'one' | 'all';

/** The state of the player. */
export interface PlayerStatus {
  /** Connected to the voice server. */
  connected: boolean;
  /** The current track; `null` when idle. */
  current: QueueItem | null;
  /** The current track is still being opened and not yet audible. */
  loading: boolean;
  /** Position in the current track, in milliseconds. */
  positionMs: number;
  /** Playback is paused. */
  paused: boolean;
  /** Volume from 0 to 100. */
  volume: number;
  /** Repeat mode. */
  repeat: RepeatMode;
  /** The next track is picked at random from the queue. */
  shuffle: boolean;
  /** Number of upcoming tracks in the queue. */
  queueLength: number;
}

/** Playback and the queue of this instance. */
export interface Player {
  /** Latest status (position is extrapolated while playing). */
  readonly status: PlayerStatus;
  /** The current track; `null` when idle. */
  readonly current: QueueItem | null;
  /** Upcoming tracks, without the current one. */
  readonly queue: readonly QueueItem[];
  /** Plays a URL, `yt:` search or library path, or tracks returned by `search`/`resolve`. */
  play(input: string | Track | Track[], mode?: PlayMode): Promise<QueueItem[]>;
  /** Resolves a URL, search or path to tracks without queueing them. */
  resolve(input: string): Promise<Track[]>;
  /** Searches with the search-capable source (yt-dlp); `limit` defaults to 10, at most 50. */
  search(query: string, limit?: number): Promise<Track[]>;
  /** Skips to the next track (a random one with shuffle). */
  skip(): Promise<void>;
  /** Plays the previous track again; the current one goes back to the front of the queue. */
  previous(): Promise<void>;
  /** Stops playback; the queue is kept. */
  stop(): Promise<void>;
  /** Pauses (`true`, the default) or resumes (`false`). */
  pause(paused?: boolean): Promise<void>;
  /** Position in milliseconds. */
  seek(positionMs: number): Promise<void>;
  /** Sets the volume from 0 to 100 (clamped). */
  setVolume(volume: number): Promise<void>;
  /** Sets the repeat mode. */
  setRepeat(mode: RepeatMode): Promise<void>;
  /** Picks the next track at random (the queue itself is not reordered). */
  setShuffle(shuffle: boolean): Promise<void>;
  /** Shuffles the queue once. */
  shuffleQueue(): Promise<void>;
  /** Removes all upcoming tracks; the current track keeps playing. */
  clear(): Promise<void>;
  /** Removes a queued item by {@link QueueItem.id}; rejects if it is no longer queued. */
  remove(itemId: number): Promise<QueueItem>;
  /** Removes the item at a 0-based queue position; rejects if there is none. */
  removeAt(index: number): Promise<QueueItem>;
  /** Moves a queued item to a 0-based position (clamped); rejects if it is no longer queued. */
  move(itemId: number, to: number): Promise<void>;
  /** Plays a queued item immediately; rejects if it is no longer queued. */
  jump(itemId: number): Promise<void>;
}

/** A saved playlist. */
export interface Playlist {
  /** Playlist id. */
  id: number;
  /** Playlist name. */
  name: string;
  /** Id of the owning web interface account; `null` if none. */
  ownerId: number | null;
  /** Number of tracks. */
  trackCount: number;
  /** Creation time, Unix seconds. */
  createdAt: number;
  /** Last change, Unix seconds. */
  updatedAt: number;
}

/** Saved playlists (needs the `playlists` permission to change). */
export interface Playlists {
  /** All saved playlists. */
  list(): Promise<Playlist[]>;
  /** A playlist with its tracks; rejects for an unknown id. */
  get(id: number): Promise<Playlist & {
    /** The playlist's tracks, in order. */
    tracks: Track[];
  }>;
  /** Plays a playlist from `start` (default: replace the current track). */
  play(id: number, options?: { start?: number; mode?: PlayMode }): Promise<QueueItem[]>;
  /** `playlists` */
  create(name: string): Promise<Playlist>;
  /** `playlists` */
  rename(id: number, name: string): Promise<void>;
  /** `playlists` */
  delete(id: number): Promise<void>;
  /** `playlists` */
  setTracks(id: number, tracks: Track[]): Promise<void>;
}

/** A file in the local music library. */
export interface LibraryTrack {
  /** Library id. */
  id: number;
  /** File path (playable with {@link Player.play}). */
  path: string;
  /** Title as renamed in James, else from the tags, or the file name. */
  title: string;
  /** Artist; `null` if unknown. */
  artist: string | null;
  /** Album; `null` if unknown. */
  album: string | null;
  /** Duration in milliseconds; `null` if unknown. */
  durationMs: number | null;
  /** File size in bytes. */
  size: number;
  /** File modification time, Unix seconds. */
  mtime: number;
  /** When the file was added to the library, Unix seconds. */
  addedAt: number;
  /** Renamed in the web interface (title, artist and album above are the new names). */
  edited: boolean;
  /** Names from the file's tags (the title falls back to the file name). */
  tags: { title: string; artist: string | null; album: string | null };
}

/** The local music library. */
export interface Library {
  /**
   * Finds tracks whose title, artist, album or path contains `query` (empty: all), sorted by
   * artist and album. `limit` is 1–200 (default 20).
   */
  search(query?: string, options?: { limit?: number; offset?: number }): Promise<LibraryTrack[]>;
  /** A track by its id; rejects for an unknown id. */
  get(id: number): Promise<LibraryTrack>;
}

// ---- store, commands, web ---------------------------------------------------------------------

/** Persistent key-value store with synchronous reads. Values must be JSON-serialisable. */
export interface Store {
  /** A copy of the value; `undefined` if the key is not set. */
  get<T = unknown>(key: string): T | undefined;
  /**
   * Stores a value (`undefined` deletes the key). Throws for keys outside 1–256 bytes or values
   * over 1 MiB. Saved in the background: a write beyond the scope's quota (see {@link Bot.store})
   * is dropped by James and only logged, so keep well below it.
   */
  set(key: string, value: unknown): void;
  /** Removes a key. */
  delete(key: string): void;
  /** `true` if the key is set. */
  has(key: string): boolean;
  /** All keys. */
  keys(): string[];
  /** All keys with (copies of) their values. */
  entries(): [string, unknown][];
}

/** A chat command a plugin registers (see {@link Commands.register}). */
export interface CommandDefinition {
  /** Name without the prefix: 1–32 letters, digits, `-` or `_`; case-insensitive. */
  name: string;
  /** Up to 8 other names for the command. */
  aliases?: string[];
  /** Argument syntax shown in help, e.g. `<name> [message]`. */
  usage?: string;
  /** Shown in help (at most 500 bytes). */
  description?: string;
  /**
   * Permission users need, namespaced to the plugin: `"use"` becomes `plugin.<id>.use`
   * and is granted through roles like built-in permissions.
   */
  permission?: string;
  /** Runs the command; a returned string is sent back to the invoker. */
  run(context: CommandContext): string | void | null | Promise<string | void | null>;
}

/** What a command handler receives. */
export interface CommandContext {
  /** Everything after the command name, trimmed. */
  args: string;
  /** Arguments split at whitespace (quotes group words). */
  argv: string[];
  /** The client who ran the command. */
  invoker: Client;
  /** Where the command was written. */
  mode: MessageMode;
  /** Answers where the command was written (privately, in the channel or in the server chat). */
  reply(text: string): Promise<void>;
}

/** Chat commands of this plugin; James handles the prefix, help and permissions. */
export interface Commands {
  /** The instance's command prefix (e.g. `!`). */
  readonly prefix: string;
  /** Registers a chat command (names are global across plugins; conflicts are rejected). */
  register(command: CommandDefinition): Promise<void>;
  /** Removes a command of this plugin (does nothing for unknown names); all are removed when the plugin stops. */
  unregister(name: string): Promise<void>;
}

/** The web interface account behind a request or WebSocket. */
export interface WebUser {
  /** Account id (see {@link Users.get}). */
  id: number;
  /** Username. */
  username: string;
}

/** A request to one of the plugin's HTTP endpoints. */
export interface WebRequest {
  /** Endpoint name (see {@link Web.handle}). */
  name: string;
  /** Received through the public URL (no signed-in user). */
  public: boolean;
  /** HTTP method (currently always `POST`). */
  method: string;
  /** The parsed JSON body; `null` if empty. */
  body: unknown;
  /** Query string parameters. */
  query: Record<string, string>;
  /** The caller's IP address; empty if unknown. */
  remoteAddr: string;
  /** The signed-in caller; `null` for public requests. */
  user: WebUser | null;
}

/** The plugin's web page, HTTP endpoints and WebSockets (needs the `web` permission). */
export interface Web {
  /**
   * Handles `POST /api/v1/instances/<instance>/plugins/<id>/events/<name>` or, with `public`,
   * `/api/v1/public/instances/<instance>/plugins/<id>/events/<name>` (anyone, rate-limited).
   * Signed-in callers need `permission` (namespaced: `"stats"` becomes `plugin.<id>.stats`)
   * or, without it, `plugins.use`. The returned value is sent as JSON.
   */
  handle(name: string, handler: (request: WebRequest) => unknown, options?: { public?: boolean; permission?: string }): () => void;
  /** Sends to one WebSocket connection, or to all of them. */
  send(data: string | object, connection?: string): void;
  /** Closes a WebSocket connection. */
  close(connection: string): void;
}

// ---- accounts and instances ---------------------------------------------------------------------

/** A web interface account (needs the `users` permission). */
export interface UserAccount {
  /** Account id. */
  id: number;
  /** Username. */
  username: string;
  /** Administrator (plugins cannot change such accounts). */
  isAdmin: boolean;
  /** The account is disabled and cannot sign in. */
  disabled: boolean;
  /** A password is set. */
  hasPassword: boolean;
  /** Names of the account's roles. */
  roles: string[];
  /** Linked platform identities (not included by {@link Users.list}). */
  identities?: {
    /** Platform, e.g. `teamspeak`. */
    platform: string;
    /** Identity on the platform (TeamSpeak UID). */
    uid: string;
    /** Name stored with the link. */
    name: string;
  }[];
}

/** Web interface accounts and the identities linked to them (needs the `users` permission). */
export interface Users {
  /** All accounts (without `identities`). */
  list(): Promise<UserAccount[]>;
  /** An account by id; rejects if there is none. */
  get(id: number): Promise<UserAccount>;
  /** An account by username; rejects if there is none. */
  byName(username: string): Promise<UserAccount>;
  /** The account linked to a platform identity; `null` if none. */
  byIdentity(platform: string, uid: string): Promise<UserAccount | null>;
  /** Creates a (non-administrator) account; a password needs at least 8 characters. */
  create(username: string, password?: string): Promise<UserAccount>;
  /** Sets or (with `null`) removes the password and signs the account out everywhere. */
  setPassword(id: number, password: string | null): Promise<void>;
  /**
   * Replaces the roles of an account. Rejects for accounts and roles with administrative rights
   * (`users.manage`, `plugins.manage`, `instance.manage`, `server.chat.read`); the same applies
   * to {@link Users.setPassword}, {@link Users.link} and {@link Users.delete}.
   */
  setRoles(id: number, roles: string[]): Promise<void>;
  /** Links a platform identity to an account; rejects if it is linked to another account. */
  link(id: number, platform: string, uid: string, name?: string): Promise<void>;
  /** Deletes an account (not administrative ones). */
  delete(id: number): Promise<void>;
}

/** Other instances (bots) of this James (needs the `instances` permission). */
export interface Instances {
  /** Instances; without `instances`, only those the plugin is active on. */
  list(): Promise<{
    /** Instance id. */
    id: string;
    /** Display name. */
    name: string;
    /** Voice platform. */
    platform: string;
    /** The instance is running. */
    running: boolean;
  }[]>;
  /** Starts an instance (`instances`). */
  start(id: string): Promise<void>;
  /** Stops an instance (`instances`). */
  stop(id: string): Promise<void>;
  /** Restarts an instance (`instances`). */
  restart(id: string): Promise<void>;
}
