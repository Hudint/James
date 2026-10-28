// The native plugin API (`@james/api`): Bot, Client, Channel, Player, … for one context.

import type { Context } from './runtime.ts';
import { splitArgs } from './runtime.ts';
import type { ChannelData, ClientData } from './state.ts';

const CLIENT_FIELDS = [
  'id', 'uid', 'name', 'channel', 'serverGroups', 'isSelf', 'databaseId', 'channelGroup', 'description', 'phoneticName',
  'away', 'inputMuted', 'outputMuted', 'recording', 'talking', 'country', 'query', 'talkPower', 'prioritySpeaker',
  'channelCommander', 'info', 'connection',
] as const;

const CHANNEL_FIELDS = [
  'id', 'parent', 'name', 'topic', 'description', 'codec', 'codecQuality', 'order', 'maxClients', 'maxFamilyClients',
  'permanent', 'semiPermanent', 'default', 'hasPassword', 'encrypted', 'neededTalkPower', 'deleteDelay', 'phoneticName',
  'icon', 'text', 'subscribed',
] as const;

function idOf(value: { id: string } | string): string {
  return typeof value === 'string' ? value : value.id;
}

// Field getters are defined on the prototype below.
export interface ClientRef extends Readonly<Omit<ClientData, 'id'>> {}
// eslint-disable-next-line @typescript-eslint/no-unsafe-declaration-merging
export class ClientRef {
  #ctx: Context;
  #last: ClientData;
  declare readonly id: string;

  constructor(ctx: Context, data: ClientData) {
    this.#ctx = ctx;
    this.#last = data;
  }

  /** Latest data (falls back to the last known state once the client is gone). */
  data(): ClientData {
    const current = this.#ctx.state.clients.get(this.#last.id);
    if (current) this.#last = current;
    return this.#last;
  }

  get online(): boolean {
    return this.#ctx.state.clients.has(this.#last.id);
  }
  channelObject() {
    return wrapChannel(this.#ctx, this.data().channel);
  }
  serverGroupObjects() {
    return this.data().serverGroups.map((id) => this.#ctx.state.serverGroups.get(id) ?? { id, name: '', icon: 0 });
  }
  equals(other: ClientRef | string): boolean {
    return idOf(other) === this.#last.id;
  }
  chat(text: string) {
    return this.#ctx.call('chat.send', { target: 'client', client: this.#last.id, text: String(text) });
  }
  poke(text: string) {
    return this.#ctx.call('client.poke', { client: this.#last.id, text: String(text) });
  }
  requestInfo() {
    return this.#ctx.call('client.requestInfo', { client: this.#last.id });
  }
  kick(reason = '') {
    return this.#ctx.call('client.kick', { client: this.#last.id, fromServer: true, reason });
  }
  kickFromChannel(reason = '') {
    return this.#ctx.call('client.kick', { client: this.#last.id, fromServer: false, reason });
  }
  ban(seconds: number, reason = '') {
    return this.#ctx.call('client.ban', { client: this.#last.id, seconds: Math.max(0, Math.floor(seconds)), reason });
  }
  moveTo(channel: { id: string } | string) {
    return this.#ctx.call('client.move', { client: this.#last.id, channel: idOf(channel) });
  }
  addServerGroup(group: { id: string } | string) {
    return this.#ctx.call('client.addServerGroup', { client: this.#last.id, group: idOf(group) });
  }
  removeServerGroup(group: { id: string } | string) {
    return this.#ctx.call('client.removeServerGroup', { client: this.#last.id, group: idOf(group) });
  }
  setChannelGroup(group: { id: string } | string, channel?: { id: string } | string) {
    const target = channel ? idOf(channel) : this.data().channel;
    return this.#ctx.call('client.setChannelGroup', { client: this.#last.id, channel: target, group: idOf(group) });
  }
  async user() {
    return this.#ctx.call('users.byIdentity', { platform: this.#ctx.platform, uid: this.data().uid });
  }
  toJSON() {
    return this.data();
  }
  [Symbol.for('nodejs.util.inspect.custom')]() {
    return `Client{ id: ${this.#last.id}, name: ${this.data().name} }`;
  }
}
for (const field of CLIENT_FIELDS) {
  Object.defineProperty(ClientRef.prototype, field, {
    get(this: ClientRef) {
      return this.data()[field];
    },
    enumerable: true,
  });
}

export interface ChannelRef extends Readonly<Record<(typeof CHANNEL_FIELDS)[number], any>> {}
export class ChannelRef {
  #ctx: Context;
  #last: ChannelData;
  constructor(ctx: Context, data: ChannelData) {
    this.#ctx = ctx;
    this.#last = data;
  }

  data(): ChannelData {
    const current = this.#ctx.state.channels.get(this.#last.id);
    if (current) this.#last = current;
    return this.#last;
  }

  get exists(): boolean {
    return this.#ctx.state.channels.has(this.#last.id);
  }
  parentChannel() {
    return wrapChannel(this.#ctx, this.data().parent);
  }
  children() {
    return sortedChannels(this.#ctx).filter((c) => c.data().parent === this.#last.id);
  }
  clients() {
    return [...this.#ctx.state.clients.values()].filter((c) => c.channel === this.#last.id).map((c) => wrapClient(this.#ctx, c));
  }
  equals(other: ChannelRef | string): boolean {
    return idOf(other) === this.#last.id;
  }
  chat(text: string) {
    return this.#ctx.call('channel.chat', { channel: this.#last.id, text: String(text) });
  }
  update(params: Record<string, unknown>) {
    return this.#ctx.call('channel.edit', { channel: this.#last.id, params });
  }
  moveTo(parent: { id: string } | string | null, order?: string) {
    return this.#ctx.call('channel.move', { channel: this.#last.id, parent: parent === null ? null : idOf(parent), order: order ?? null });
  }
  delete(force = false) {
    return this.#ctx.call('channel.delete', { channel: this.#last.id, force });
  }
  subscribe(subscribe = true) {
    return this.#ctx.call('channel.subscribe', { channel: this.#last.id, subscribe });
  }
  toJSON() {
    return this.data();
  }
  [Symbol.for('nodejs.util.inspect.custom')]() {
    return `Channel{ id: ${this.#last.id}, name: ${this.data().name} }`;
  }
}
for (const field of CHANNEL_FIELDS) {
  Object.defineProperty(ChannelRef.prototype, field, {
    get(this: ChannelRef) {
      return this.data()[field];
    },
    enumerable: true,
  });
}

/** One wrapper per id and context, so `===` and identity-based maps work. */
function cache(ctx: Context, kind: 'clients' | 'channels'): Map<string, any> {
  return (ctx.extra[`${kind}Cache`] ??= new Map());
}

export function wrapClient(ctx: Context, data: ClientData | string | null | undefined): ClientRef | null {
  const resolved = typeof data === 'string' ? ctx.state.client(data) : data;
  if (!resolved) return null;
  const map = cache(ctx, 'clients');
  let ref = map.get(resolved.id);
  if (!ref) {
    ref = new ClientRef(ctx, resolved);
    map.set(resolved.id, ref);
    if (map.size > 5000) map.delete(map.keys().next().value!);
  }
  return ref;
}

export function wrapChannel(ctx: Context, data: ChannelData | string | null | undefined): ChannelRef | null {
  const resolved = typeof data === 'string' ? ctx.state.channel(data) : data;
  if (!resolved) return null;
  const map = cache(ctx, 'channels');
  let ref = map.get(resolved.id);
  if (!ref) {
    ref = new ChannelRef(ctx, resolved);
    map.set(resolved.id, ref);
  }
  return ref;
}

export function sortedChannels(ctx: Context): ChannelRef[] {
  return [...ctx.state.channels.values()].sort((a, b) => Number(a.order) - Number(b.order)).map((c) => wrapChannel(ctx, c)!);
}

function group(ctx: Context, kind: 'server' | 'channel', id: string | null) {
  if (!id) return null;
  const map = kind === 'server' ? ctx.state.serverGroups : ctx.state.channelGroups;
  return map.get(id) ?? { id, name: '', icon: 0 };
}

/** Converts mirror-level events (`raw:*`, ids) into API events (wrapped objects). */
export function installEvents(ctx: Context): void {
  const client = (id: string | null) => wrapClient(ctx, id);
  const channel = (id: string | null) => wrapChannel(ctx, id);
  const map: Record<string, (p: any) => Record<string, unknown>> = {
    connect: (p) => p,
    disconnect: (p) => p,
    message: (p) => ({
      text: p.text,
      mode: p.mode,
      client: wrapClient(ctx, ctx.state.clients.get(p.clientId) ?? p.client),
      channel: p.mode === 'channel' ? channel(ctx.state.client(ctx.state.ownId)?.channel ?? null) : null,
    }),
    poke: (p) => ({ text: p.text, client: wrapClient(ctx, ctx.state.client(p.clientId) ?? p.client) }),
    clientJoin: (p) => ({ client: client(p.clientId) }),
    clientLeave: (p) => ({ client: client(p.clientId), reason: p.reason, message: p.message, invoker: p.invoker ? wrapClient(ctx, ctx.state.client(p.invokerId) ?? p.invoker) : null }),
    clientMove: (p) => ({
      client: client(p.clientId),
      from: channel(p.fromId),
      to: channel(p.toId),
      invoker: p.invoker ? wrapClient(ctx, ctx.state.client(p.invokerId) ?? p.invoker) : null,
      reason: p.reason,
    }),
    clientUpdate: (p) => ({ client: client(p.clientId), previous: p.previous }),
    clientNick: (p) => ({ client: client(p.clientId), oldName: p.oldName }),
    clientAway: (p) => ({ client: client(p.clientId), message: p.message }),
    clientDescription: (p) => ({ client: client(p.clientId), previous: p.previous }),
    clientInfo: (p) => ({ client: client(p.clientId) }),
    serverGroupAdded: (p) => ({ client: client(p.clientId), group: group(ctx, 'server', p.groupId) }),
    serverGroupRemoved: (p) => ({ client: client(p.clientId), group: group(ctx, 'server', p.groupId) }),
    channelGroupChanged: (p) => ({ client: client(p.clientId), group: group(ctx, 'channel', p.groupId), previous: group(ctx, 'channel', p.previousId) }),
    channelCreate: (p) => ({ channel: channel(p.channelId), invoker: p.invoker ? wrapClient(ctx, ctx.state.client(p.invoker.id) ?? p.invoker) : null }),
    channelUpdate: (p) => ({ channel: channel(p.channelId), previous: p.previous, invoker: p.invoker ? wrapClient(ctx, ctx.state.client(p.invoker.id) ?? p.invoker) : null }),
    channelDelete: (p) => ({ channel: p.channel, invoker: p.invoker ? wrapClient(ctx, ctx.state.client(p.invoker.id) ?? p.invoker) : null }),
    serverUpdate: (p) => p,
    groupsUpdate: (p) => p,
    trackStart: (p) => p,
    trackEnd: (p) => p,
    queueChange: (p) => p,
    playbackChange: (p) => p,
    speechStart: (p) => p,
    speechEnd: (p) => p,
  };
  for (const name of ['clientBack', 'clientMute', 'clientUnmute', 'clientDeaf', 'clientUndeaf', 'clientRecord', 'clientRecordStop']) {
    map[name] = (p) => ({ client: client(p.clientId) });
  }
  for (const [name, convert] of Object.entries(map)) {
    ctx.on(`raw:${name}`, (payload) => {
      if (ctx.hasListeners(name)) ctx.dispatch(name, convert(payload));
    });
  }
}

const MAX_KEY_BYTES = 256;
const MAX_VALUE_BYTES = 1 << 20;

class StoreImpl {
  #ctx: Context;
  #scope: 'plugin' | 'instance' | 'global';
  #map: Map<string, unknown>;

  constructor(ctx: Context, scope: 'plugin' | 'instance' | 'global', map: Map<string, unknown>) {
    this.#ctx = ctx;
    this.#scope = scope;
    this.#map = map;
  }
  get(key: string) {
    const value = this.#map.get(String(key));
    // Callers must not mutate the cached copy.
    return value !== null && typeof value === 'object' ? structuredClone(value) : value;
  }
  set(key: string, value: unknown) {
    if (value === undefined) return this.delete(key);
    key = String(key);
    // Same limits as James enforces, checked here so the caller learns about them.
    if (key.length === 0 || Buffer.byteLength(key) > MAX_KEY_BYTES) throw new RangeError(`store keys are 1-${MAX_KEY_BYTES} bytes`);
    const json = JSON.stringify(value);
    if (json === undefined) throw new TypeError('store values must be JSON-serialisable');
    if (Buffer.byteLength(json) > MAX_VALUE_BYTES) throw new RangeError('store values are at most 1 MiB');
    const copy = JSON.parse(json);
    this.#map.set(String(key), copy);
    this.#ctx.storeChanged(this.#scope, String(key), copy, false);
  }
  delete(key: string) {
    if (this.#map.delete(String(key))) this.#ctx.storeChanged(this.#scope, String(key), null, true);
  }
  has(key: string) {
    return this.#map.has(String(key));
  }
  keys() {
    return [...this.#map.keys()];
  }
  entries(): [string, unknown][] {
    return this.keys().map((k) => [k, this.get(k)]);
  }
}

function tracksOf(input: unknown): { input?: string; tracks?: unknown[] } {
  if (typeof input === 'string') return { input };
  if (Array.isArray(input)) return { tracks: input };
  if (input && typeof input === 'object') return { tracks: [input] };
  throw new TypeError('play() takes a string, a track or a list of tracks');
}

export function createBot(ctx: Context) {
  installEvents(ctx);
  const call = (method: string, params: unknown = null) => ctx.call(method, params);
  const state = ctx.state;

  const player = {
    get status(): Record<string, any> {
      return { ...state.status, positionMs: state.position() };
    },
    get current() {
      return state.status.current ?? null;
    },
    get queue() {
      return state.queue;
    },
    play: (input: unknown, mode = 'append') => call('media.play', { ...tracksOf(input), mode }),
    resolve: (input: string) => call('media.resolve', { input }),
    search: (query: string, limit = 10) => call('media.search', { query, limit }),
    skip: () => call('media.skip'),
    previous: () => call('media.previous'),
    stop: () => call('media.stop'),
    pause: (paused = true) => call('media.pause', { value: paused }),
    seek: (positionMs: number) => call('media.seek', { value: positionMs }),
    setVolume: (volume: number) => call('media.volume', { value: volume }),
    setRepeat: (mode: string) => call('media.repeat', { value: mode }),
    setShuffle: (shuffle: boolean) => call('media.shuffle', { value: shuffle }),
    shuffleQueue: () => call('media.shuffleQueue'),
    clear: () => call('media.clear'),
    remove: (item: number) => call('media.remove', { item }),
    removeAt: (index: number) => call('media.removeAt', { index }),
    move: (item: number, to: number) => call('media.move', { item, to }),
    jump: (item: number) => call('media.jump', { item }),
  };

  const runtime = ctx.runtime;
  const bot = {
    instance: ctx.info,
    config: Object.freeze({ ...ctx.config }),
    log: {
      debug: (...a: unknown[]) => ctx.log('debug', ...a),
      info: (...a: unknown[]) => ctx.log('info', ...a),
      warn: (...a: unknown[]) => ctx.log('warn', ...a),
      error: (...a: unknown[]) => ctx.log('error', ...a),
    },
    signal: ctx.abort.signal,
    dataDir: runtime.dataDir,
    version: '',
    on: (event: string, handler: (p: any) => unknown) => ctx.on(event, handler),
    once: (event: string, handler: (p: any) => unknown) => {
      const off = ctx.on(event, (p) => {
        off();
        return handler(p);
      });
      return off;
    },
    emit: (name: string, data?: unknown, options?: { broadcast?: boolean }) =>
      runtime.post({ type: 'emit', instance: ctx.instance, name: String(name), data: data ?? null, broadcast: !!options?.broadcast }),
    onStop: (handler: () => unknown) => ctx.onStop(handler),
    setTimeout: (handler: () => void, ms: number) => ctx.setTimeout(handler, ms),
    setInterval: (handler: () => void, ms: number) => ctx.setInterval(handler, ms),

    get connected() {
      return state.connected;
    },
    get server() {
      return state.connected ? state.server : null;
    },
    get self() {
      return state.connected ? wrapClient(ctx, state.ownId) : null;
    },
    clients: () => [...state.clients.values()].map((c) => wrapClient(ctx, c)!),
    client: (id: string) => (state.clients.has(id) ? wrapClient(ctx, id) : null),
    clientByUid: (uid: string) => wrapClient(ctx, [...state.clients.values()].find((c) => c.uid === uid)),
    clientByName: (name: string) => wrapClient(ctx, [...state.clients.values()].find((c) => c.name === name)),
    channels: () => sortedChannels(ctx),
    channel: (id: string) => (state.channels.has(id) ? wrapChannel(ctx, id) : null),
    channelByName: (name: string) => sortedChannels(ctx).find((c) => c.data().name === name) ?? null,
    channelsByName: (name: string) => sortedChannels(ctx).filter((c) => c.data().name === name),
    serverGroups: () => [...state.serverGroups.values()],
    serverGroup: (id: string) => state.serverGroups.get(id) ?? null,
    channelGroups: () => [...state.channelGroups.values()],
    channelGroup: (id: string) => state.channelGroups.get(id) ?? null,

    chatServer: (text: string) => call('chat.send', { target: 'server', text: String(text) }),
    chatChannel: (text: string) => call('chat.send', { target: 'channel', text: String(text) }),
    move: (channel: { id: string } | string, password?: string) => call('bot.move', { channel: idOf(channel), password: password ?? null }),
    setNick: (name: string) => call('bot.setNick', { text: name }),
    setDescription: (text: string) => call('bot.setDescription', { text }),
    setAway: (message: string | null) => call('bot.setAway', { message }),
    createChannel: async (params: Record<string, unknown>) => {
      const id: string = await call('channel.create', params);
      return wrapChannel(ctx, state.channels.get(id) ?? { id, parent: (params.parent as string) ?? null, name: String(params.name) });
    },

    player,
    playlists: {
      list: () => call('playlists.list'),
      get: (id: number) => call('playlists.get', { id }),
      play: (id: number, options: { start?: number; mode?: string } = {}) => call('playlists.play', { id, start: options.start ?? 0, mode: options.mode ?? null }),
      create: (name: string) => call('playlists.create', { name }),
      rename: (id: number, name: string) => call('playlists.rename', { id, name }),
      delete: (id: number) => call('playlists.delete', { id }),
      setTracks: (id: number, tracks: unknown[]) => call('playlists.setTracks', { id, tracks }),
    },
    library: {
      search: (query = '', options: { limit?: number; offset?: number } = {}) => call('library.search', { query, limit: options.limit ?? 20, offset: options.offset ?? 0 }),
      get: (id: number) => call('library.get', { id }),
    },
    store: {
      plugin: new StoreImpl(ctx, 'plugin', runtime.pluginStore),
      instance: new StoreImpl(ctx, 'instance', ctx.instanceStore),
      global: new StoreImpl(ctx, 'global', runtime.globalStore),
    },
    commands: {
      get prefix() {
        return ctx.info.commandPrefix;
      },
      register: (definition: any) => {
        if (!definition || typeof definition.name !== 'string' || typeof definition.run !== 'function') {
          return Promise.reject(new TypeError('a command needs a name and a run function'));
        }
        return ctx.registerCommand({
          name: definition.name,
          aliases: Array.isArray(definition.aliases) ? definition.aliases.map(String) : [],
          usage: definition.usage,
          description: definition.description,
          permission: definition.permission,
          run: definition.run,
        });
      },
      unregister: (name: string) => ctx.unregisterCommand(name),
    },
    web: {
      handle: (name: string, handler: (request: any) => unknown, options: { public?: boolean; permission?: string } = {}) => {
        if (!runtime.has('web')) throw new Error("the plugin lacks the permission 'web'");
        name = String(name);
        ctx.http.set(name, { handler, public: !!options.public });
        // James checks access (public, permission) before a request reaches the plugin.
        call('web.register', { name, public: !!options.public, permission: options.permission ?? null }).catch((error: Error) =>
          ctx.log('error', `web.handle('${name}'):`, error.message),
        );
        return () => ctx.http.delete(name);
      },
      send: (data: unknown, connection?: string) =>
        runtime.post({ type: 'ws.send', instance: ctx.instance, connection: connection ?? null, data: typeof data === 'string' ? data : JSON.stringify(data) }),
      close: (connection: string) => runtime.post({ type: 'ws.send', instance: ctx.instance, connection, data: '', close: true }),
    },
    users: {
      list: () => call('users.list'),
      get: (id: number) => call('users.get', { id }),
      byName: (username: string) => call('users.byName', { username }),
      byIdentity: (platform: string, uid: string) => call('users.byIdentity', { platform, uid }),
      create: (username: string, password?: string) => call('users.create', { username, password: password ?? null }),
      setPassword: (id: number, password: string | null) => call('users.setPassword', { id, password }),
      setRoles: (id: number, roles: string[]) => call('users.setRoles', { id, roles }),
      link: (id: number, platform: string, uid: string, name = '') => call('users.link', { id, platform, uid, name }),
      delete: (id: number) => call('users.delete', { id }),
    },
    instances: {
      list: () => call('instances.list'),
      start: (id: string) => call('instances.start', { id }),
      stop: (id: string) => call('instances.stop', { id }),
      restart: (id: string) => call('instances.restart', { id }),
    },
    say: (text: string, options: { voice?: string; language?: string } = {}) =>
      call('tts.say', { text: String(text), voice: options.voice ?? null, language: options.language ?? null }),
    tts: {
      voices: () => call('tts.voices'),
      stop: () => call('tts.stop'),
    },
    notify: (message: string) => call('engine.notify', { message: String(message) }),
    saveConfig: (config: Record<string, unknown>) => call('engine.saveConfig', { config }),
  };

  ctx.extra.runCommand = async (spec: { run: (c: any) => unknown }, payload: any) => {
    const invoker = wrapClient(ctx, ctx.state.client(payload.invoker.id) ?? payload.invoker)!;
    const mode = payload.mode as 'client' | 'channel' | 'server';
    const reply = (text: string) =>
      call('chat.send', { target: mode, client: mode === 'client' ? invoker.id : null, text: String(text) });
    const result = await spec.run({ args: payload.args, argv: splitArgs(payload.args), invoker, mode, reply });
    return typeof result === 'string' ? result : null;
  };
  return bot;
}
