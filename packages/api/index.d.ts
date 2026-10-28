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

export interface PluginDefinition {
  /**
   * Called for every instance the plugin is activated on. May return a cleanup function
   * (also see {@link Bot.onStop}).
   */
  setup(bot: Bot): void | (() => void | Promise<void>) | Promise<void | (() => void | Promise<void>)>;
}

/** Declares a plugin (identity function that gives `setup` its types). */
export function definePlugin(definition: PluginDefinition): PluginDefinition;

// ---- manifest (package.json "james" section) ------------------------------------------------

/** The `"james"` section of a plugin's `package.json`. */
export interface Manifest {
  /** Display name (defaults to the package name). */
  name?: string;
  /** Required James version, e.g. `">=0.1.0"`. */
  engine?: string;
  /** Permissions the plugin needs; an administrator confirms them when enabling the plugin. */
  permissions?: Permission[];
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

export type Platform = 'teamspeak' | 'discord';

/**
 * Permissions. `moderation`, `channels`, `playlists`, `instances`, `users` and `web` are
 * enforced by James; `fs` and `process` by the plugin's process sandbox (Node's permission
 * model). `http`, `net` and `db` only guard the corresponding Node.js modules; plugins are
 * trusted code, so these are a safety net, not a sandbox.
 */
export type Permission = 'moderation' | 'channels' | 'playlists' | 'instances' | 'users' | 'web' | 'http' | 'net' | 'fs' | 'db' | 'process';

/** A setting in the web interface (same schema as SinusBot's `vars`). */
export interface ConfigField {
  /** Key in {@link Bot.config}. */
  name: string;
  title: string;
  type: 'string' | 'strings' | 'password' | 'multiline' | 'number' | 'checkbox' | 'select' | 'channel' | 'track' | 'tracks' | 'array';
  default?: unknown;
  placeholder?: string;
  /** `select`: the options; the stored value is the index. */
  options?: string[];
  /** `array`: fields of each item. */
  vars?: ConfigField[];
  /** Only shown when every condition matches another field's value. */
  conditions?: { field: string; value: unknown }[];
  indent?: number;
}

// ---- the bot ----------------------------------------------------------------------------------

/** One instance as seen by a plugin. Everything is scoped to this instance. */
export interface Bot {
  readonly instance: InstanceInfo;
  /** This instance's values of the manifest's config fields (defaults applied). */
  readonly config: Readonly<Record<string, unknown>>;
  readonly log: Logger;
  /** Private directory for the plugin's files (shared by all instances). */
  readonly dataDir: string;
  /** Aborted when the plugin stops on this instance. */
  readonly signal: AbortSignal;

  /** Subscribes to an event; returns a function that unsubscribes. */
  on<E extends keyof Events>(event: E, handler: (payload: Events[E]) => void | Promise<void>): () => void;
  /** Custom events from plugins (see {@link Bot.emit}). */
  on(event: `custom:${string}`, handler: (payload: CustomEvent) => void | Promise<void>): () => void;
  once<E extends keyof Events>(event: E, handler: (payload: Events[E]) => void | Promise<void>): () => void;
  /** Sends a custom event (`custom:<name>`) to all plugins on this instance or, with `broadcast`, on every instance. */
  emit(name: string, data?: unknown, options?: { broadcast?: boolean }): void;
  /** Registers cleanup run when the plugin stops on this instance. */
  onStop(handler: () => void | Promise<void>): void;
  /** Timers that are cleared automatically when the plugin stops. */
  setTimeout(handler: () => void, ms: number): () => void;
  setInterval(handler: () => void, ms: number): () => void;

  // server state (synchronous)
  readonly connected: boolean;
  readonly server: ServerInfo | null;
  /** The bot's own client while connected. */
  readonly self: Client | null;
  clients(): Client[];
  client(id: string): Client | null;
  clientByUid(uid: string): Client | null;
  clientByName(name: string): Client | null;
  channels(): Channel[];
  channel(id: string): Channel | null;
  channelByName(name: string): Channel | null;
  channelsByName(name: string): Channel[];
  serverGroups(): Group[];
  serverGroup(id: string): Group | null;
  channelGroups(): Group[];
  channelGroup(id: string): Group | null;

  // chat
  /** Server-wide message. */
  chatServer(text: string): Promise<void>;
  /** Message to the bot's current channel. */
  chatChannel(text: string): Promise<void>;

  // the bot itself
  move(channel: Channel | string, password?: string): Promise<void>;
  setNick(name: string): Promise<void>;
  setDescription(text: string): Promise<void>;
  /** Sets the bot away (`null` = back). */
  setAway(message: string | null): Promise<void>;
  /** Creates a channel and returns it (`channels`). */
  createChannel(params: ChannelParams & { name: string }): Promise<Channel>;

  readonly player: Player;
  readonly playlists: Playlists;
  readonly library: Library;
  readonly store: { readonly plugin: Store; readonly instance: Store; readonly global: Store };
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
  readonly tts: {
    /** Installed voices. */
    voices(): Promise<TtsVoice[]>;
    /** Cancels the playing and queued speech. */
    stop(): Promise<void>;
  };

  /** Shows a message to the users of the web interface. */
  notify(message: string): Promise<void>;
  /** Replaces this instance's configuration (persisted; {@link Bot.config} keeps the old values until restart). */
  saveConfig(config: Record<string, unknown>): Promise<void>;
  /** Version of James. */
  readonly version: string;
}

export interface InstanceInfo {
  id: string;
  name: string;
  platform: Platform | string;
  commandPrefix: string;
}

export interface Logger {
  debug(...args: unknown[]): void;
  info(...args: unknown[]): void;
  warn(...args: unknown[]): void;
  error(...args: unknown[]): void;
}

// ---- events -------------------------------------------------------------------------------------

export type MessageMode = 'client' | 'channel' | 'server';

export interface Events {
  /** Connected to the server (also after a reconnect). */
  connect: { server: ServerInfo | null };
  disconnect: { reason: string };
  /** A chat message (not the bot's own). Command lines are delivered as well. */
  message: { text: string; mode: MessageMode; client: Client; channel: Channel | null };
  poke: { text: string; client: Client };
  /** A client connected to the server. */
  clientJoin: { client: Client };
  /** A client left the server. */
  clientLeave: { client: Client; reason: LeaveReason; message: string; invoker: Client | null };
  /** A client switched channels; `from`/`to` are `null` when it became visible/invisible. */
  clientMove: { client: Client; from: Channel | null; to: Channel | null; invoker: Client | null; reason: MoveReason };
  /** Any property of a client changed. */
  clientUpdate: { client: Client; previous: ClientData };
  clientNick: { client: Client; oldName: string };
  clientAway: { client: Client; message: string };
  clientBack: { client: Client };
  clientMute: { client: Client };
  clientUnmute: { client: Client };
  clientDeaf: { client: Client };
  clientUndeaf: { client: Client };
  clientRecord: { client: Client };
  clientRecordStop: { client: Client };
  clientDescription: { client: Client; previous: string };
  /** Fresh connection data or client details arrived (after {@link Client.requestInfo}). */
  clientInfo: { client: Client };
  serverGroupAdded: { client: Client; group: Group };
  serverGroupRemoved: { client: Client; group: Group };
  channelGroupChanged: { client: Client; group: Group | null; previous: Group | null };
  channelCreate: { channel: Channel; invoker: Client | null };
  channelUpdate: { channel: Channel; previous: ChannelData; invoker: Client | null };
  channelDelete: { channel: ChannelData; invoker: Client | null };
  serverUpdate: { server: ServerInfo };
  groupsUpdate: { serverGroups: Group[]; channelGroups: Group[] };
  trackStart: { item: QueueItem };
  /** A track ended; `error` is set when it failed. */
  trackEnd: { item: QueueItem; error: string | null };
  queueChange: { queue: QueueItem[] };
  playbackChange: { status: PlayerStatus };
  /** Speech (from `bot.say`, `!say`, the web interface or another plugin) started. */
  speechStart: { id: number; text: string };
  speechEnd: { id: number; error: string | null; stopped: boolean };
  /** A WebSocket connection of the plugin's web page opened (`web`). */
  'ws.connect': { connection: string; user: WebUser | null };
  'ws.message': { connection: string; data: string };
  'ws.close': { connection: string };
}

export type LeaveReason = 'disconnect' | 'timeout' | 'kicked' | 'banned' | 'serverShutdown' | 'invisible';
export type MoveReason = 'switched' | 'moved' | 'kicked' | 'visible';

export interface CustomEvent {
  name: string;
  data: unknown;
  /** Id of the plugin that sent it. */
  from: string;
}

// ---- server objects -------------------------------------------------------------------------

/** Plain data of a client (as delivered by the platform). */
export interface ClientData {
  id: string;
  /** Stable identity (TeamSpeak UID). */
  uid: string;
  name: string;
  channel: string | null;
  serverGroups: string[];
  isSelf: boolean;
  databaseId: string | null;
  channelGroup: string | null;
  description: string;
  phoneticName: string;
  /** Away message; `null` when not away. */
  away: string | null;
  inputMuted: boolean;
  outputMuted: boolean;
  recording: boolean;
  talking: boolean;
  country: string;
  query: boolean;
  talkPower: number;
  prioritySpeaker: boolean;
  channelCommander: boolean;
  info: { version: string; platform: string; created: number; lastConnected: number; totalConnections: number } | null;
  connection: ConnectionInfo | null;
}

export interface ConnectionInfo {
  pingMs: number | null;
  packetLoss: number | null;
  bytesSent: number | null;
  bytesReceived: number | null;
  connectedMs: number | null;
  idleMs: number | null;
  address: string | null;
}

/** A client on the server. Properties always reflect the latest known state. */
export interface Client extends Readonly<ClientData> {
  /** The client's current channel. */
  channelObject(): Channel | null;
  serverGroupObjects(): Group[];
  /** `true` while the client is still on the server. */
  readonly online: boolean;
  equals(other: Client | string): boolean;
  chat(text: string): Promise<void>;
  poke(text: string): Promise<void>;
  /** Asks the server for connection data and details; a `clientInfo` event follows. */
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

export interface ChannelData {
  id: string;
  parent: string | null;
  name: string;
  topic: string;
  description: string;
  codec: number | null;
  codecQuality: number | null;
  order: number;
  maxClients: number | null;
  maxFamilyClients: number | null;
  permanent: boolean;
  semiPermanent: boolean;
  default: boolean;
  hasPassword: boolean;
  encrypted: boolean;
  neededTalkPower: number;
  deleteDelay: number;
  phoneticName: string;
  icon: number;
  text: boolean;
  subscribed: boolean;
}

export interface Channel extends Readonly<ChannelData> {
  parentChannel(): Channel | null;
  children(): Channel[];
  clients(): Client[];
  readonly exists: boolean;
  equals(other: Channel | string): boolean;
  /** Writes into the channel (only the channel the bot is in). */
  chat(text: string): Promise<void>;
  /** `channels` */
  update(params: ChannelParams): Promise<void>;
  /** `channels`; `order` = id of the sibling above ("0" = first). */
  moveTo(parent: Channel | string | null, order?: string): Promise<void>;
  /** `channels` */
  delete(force?: boolean): Promise<void>;
  /** Sees the clients inside (TeamSpeak subscription). */
  subscribe(subscribe?: boolean): Promise<void>;
}

export interface ChannelParams {
  name?: string;
  parent?: string | null;
  topic?: string;
  description?: string;
  /** Empty string removes the password. */
  password?: string;
  codec?: number;
  codecQuality?: number;
  encrypted?: boolean;
  permanent?: boolean;
  semiPermanent?: boolean;
  default?: boolean;
  /** Id of the sibling above. */
  position?: string;
  /** -1 = unlimited. */
  maxClients?: number;
  maxFamilyClients?: number;
  neededTalkPower?: number;
  deleteDelay?: number;
  phoneticName?: string;
}

export interface TtsVoice {
  id: string;
  name: string;
  /** BCP 47 tag, e.g. `de-DE`. */
  language: string;
  provider: string;
}

export interface Group {
  id: string;
  name: string;
  icon: number;
}

export interface ServerInfo {
  id: string;
  uid: string;
  name: string;
  version: string;
  platform: string;
  welcomeMessage: string;
  phoneticName: string;
  hostMessage: string;
  hostBannerUrl: string;
  hostBannerGfxUrl: string;
  hostBannerGfxInterval: number;
  hostButtonUrl: string;
  hostButtonGfxUrl: string;
  icon: number;
  maxClients: number;
  defaultServerGroup: string | null;
  defaultChannelGroup: string | null;
  codecEncryptionMode: number;
  created: number;
}

// ---- playback -----------------------------------------------------------------------------------

export interface Track {
  /** Source id (`ytdlp`, `stream`, `local`, …). */
  source: string;
  locator: string;
  title: string;
  artist?: string | null;
  album?: string | null;
  durationMs?: number | null;
  thumbnail?: string | null;
  isStream?: boolean;
  url?: string | null;
}

export interface QueueItem {
  id: number;
  track: Track;
  requestedBy: { uid: string; name: string } | null;
}

export type PlayMode = 'now' | 'next' | 'append';
export type RepeatMode = 'off' | 'one' | 'all';

export interface PlayerStatus {
  connected: boolean;
  current: QueueItem | null;
  loading: boolean;
  positionMs: number;
  paused: boolean;
  volume: number;
  repeat: RepeatMode;
  shuffle: boolean;
  queueLength: number;
}

export interface Player {
  /** Latest status (position is extrapolated while playing). */
  readonly status: PlayerStatus;
  readonly current: QueueItem | null;
  readonly queue: readonly QueueItem[];
  /** Plays a URL, `yt:` search or library path, or tracks returned by `search`/`resolve`. */
  play(input: string | Track | Track[], mode?: PlayMode): Promise<QueueItem[]>;
  resolve(input: string): Promise<Track[]>;
  search(query: string, limit?: number): Promise<Track[]>;
  skip(): Promise<void>;
  previous(): Promise<void>;
  stop(): Promise<void>;
  pause(paused?: boolean): Promise<void>;
  /** Position in milliseconds. */
  seek(positionMs: number): Promise<void>;
  setVolume(volume: number): Promise<void>;
  setRepeat(mode: RepeatMode): Promise<void>;
  setShuffle(shuffle: boolean): Promise<void>;
  shuffleQueue(): Promise<void>;
  clear(): Promise<void>;
  remove(itemId: number): Promise<QueueItem>;
  removeAt(index: number): Promise<QueueItem>;
  move(itemId: number, to: number): Promise<void>;
  jump(itemId: number): Promise<void>;
}

export interface Playlist {
  id: number;
  name: string;
  ownerId: number | null;
  trackCount: number;
  createdAt: number;
  updatedAt: number;
}

export interface Playlists {
  list(): Promise<Playlist[]>;
  get(id: number): Promise<Playlist & { tracks: Track[] }>;
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

export interface LibraryTrack {
  id: number;
  path: string;
  title: string;
  artist: string | null;
  album: string | null;
  durationMs: number | null;
  size: number;
  mtime: number;
  addedAt: number;
}

export interface Library {
  search(query?: string, options?: { limit?: number; offset?: number }): Promise<LibraryTrack[]>;
  get(id: number): Promise<LibraryTrack>;
}

// ---- store, commands, web ---------------------------------------------------------------------

/** Persistent key-value store with synchronous reads. Values must be JSON-serialisable. */
export interface Store {
  get<T = unknown>(key: string): T | undefined;
  set(key: string, value: unknown): void;
  delete(key: string): void;
  has(key: string): boolean;
  keys(): string[];
  entries(): [string, unknown][];
}

export interface CommandDefinition {
  name: string;
  aliases?: string[];
  /** Argument syntax shown in help, e.g. `<name> [message]`. */
  usage?: string;
  description?: string;
  /**
   * Permission users need, namespaced to the plugin: `"use"` becomes `plugin.<id>.use`
   * and is granted through roles like built-in permissions.
   */
  permission?: string;
  /** Runs the command; a returned string is sent back to the invoker. */
  run(context: CommandContext): string | void | null | Promise<string | void | null>;
}

export interface CommandContext {
  /** Everything after the command name, trimmed. */
  args: string;
  /** Arguments split at whitespace (quotes group words). */
  argv: string[];
  invoker: Client;
  mode: MessageMode;
  reply(text: string): Promise<void>;
}

export interface Commands {
  /** The instance's command prefix (e.g. `!`). */
  readonly prefix: string;
  /** Registers a chat command (names are global across plugins; conflicts are rejected). */
  register(command: CommandDefinition): Promise<void>;
  unregister(name: string): Promise<void>;
}

export interface WebUser {
  id: number;
  username: string;
}

export interface WebRequest {
  name: string;
  public: boolean;
  method: string;
  body: unknown;
  query: Record<string, string>;
  remoteAddr: string;
  user: WebUser | null;
}

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
  close(connection: string): void;
}

// ---- accounts and instances ---------------------------------------------------------------------

export interface UserAccount {
  id: number;
  username: string;
  isAdmin: boolean;
  disabled: boolean;
  hasPassword: boolean;
  roles: string[];
  identities?: { platform: string; uid: string; name: string }[];
}

export interface Users {
  list(): Promise<UserAccount[]>;
  get(id: number): Promise<UserAccount>;
  byName(username: string): Promise<UserAccount>;
  byIdentity(platform: string, uid: string): Promise<UserAccount | null>;
  create(username: string, password?: string): Promise<UserAccount>;
  setPassword(id: number, password: string | null): Promise<void>;
  /** Administrator accounts and the `admin` role are off limits for plugins. */
  setRoles(id: number, roles: string[]): Promise<void>;
  link(id: number, platform: string, uid: string, name?: string): Promise<void>;
  delete(id: number): Promise<void>;
}

export interface Instances {
  list(): Promise<{ id: string; name: string; platform: string; running: boolean }[]>;
  start(id: string): Promise<void>;
  stop(id: string): Promise<void>;
  restart(id: string): Promise<void>;
}
