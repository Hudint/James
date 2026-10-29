// SinusBot compatibility layer: runs scripts written for the SinusBot scripting engine
// (`registerPlugin(manifest, main)` + `require('engine')` etc.) on top of the James API.
// Coverage and deviations are listed in docs/plugins/sinusbot.md.

import fs from 'node:fs';
import { createRequire } from 'node:module';
import nodeCrypto from 'node:crypto';
import path from 'node:path';
import util from 'node:util';
import vm from 'node:vm';

import { ChannelRef, ClientRef, createBot, sortedChannels, wrapChannel, wrapClient } from '../worker/api.ts';
import type { Context, Runtime } from '../worker/runtime.ts';
import { Bytes, BytesWriter, toBuffer } from './bytes.ts';

type Bot = ReturnType<typeof createBot>;

/** Fire-and-forget for SinusBot's synchronous action methods (errors go to the log). */
function fire(ctx: Context, what: string, promise: Promise<unknown>): boolean {
  promise.catch((error) => ctx.log('warn', `${what}: ${error?.message ?? error}`));
  return true;
}

function unsupported(ctx: Context, what: string): false {
  if (!ctx.extra.warned) ctx.extra.warned = new Set<string>();
  if (!ctx.extra.warned.has(what)) {
    ctx.extra.warned.add(what);
    ctx.log('warn', `${what} is not supported by James`);
  }
  return false;
}

const MODES: Record<string, number> = { client: 1, channel: 2, server: 3 };

// ---- wrappers ---------------------------------------------------------------------------------

/** How long connection data of a client stays fresh before a getter asks TeamSpeak again. */
const CONNECTION_INFO_MS = 30_000;
/** Most clients whose connection data a script requests per second (James limits it too). */
const CONNECTION_INFO_BURST = 10;

class SinusbotClient {
  #ctx: Context;
  #bot: Bot;
  readonly ref: ClientRef;
  constructor(ctx: Context, bot: Bot, ref: ClientRef) {
    this.#ctx = ctx;
    this.#bot = bot;
    this.ref = ref;
  }
  #d() { return this.ref.data(); }
  /**
   * SinusBot keeps connection data (IP, idle time, …) loaded; James loads it on request. Reading it
   * asks for fresh data at most every 30 s per client, so polling scripts see it from the next poll.
   * The IP address is only passed to scripts with the `moderation` permission.
   */
  #c() {
    const data = this.#d();
    if (!data.isSelf) {
      const requested: Map<string, number> = (this.#ctx.extra.infoRequested ??= new Map());
      // Client ids are reused after a client left.
      const key = `${data.id}:${data.uid}`;
      const last = requested.get(key);
      const now = Date.now();
      // Clients beyond the burst are requested on a later read.
      const second = Math.floor(now / 1000);
      if (this.#ctx.extra.infoSecond !== second) [this.#ctx.extra.infoSecond, this.#ctx.extra.infoCount] = [second, 0];
      if ((last === undefined || now - last > CONNECTION_INFO_MS) && this.#ctx.extra.infoCount < CONNECTION_INFO_BURST) {
        this.#ctx.extra.infoCount++;
        requested.delete(key);
        requested.set(key, now);
        if (requested.size > 1000) requested.delete(requested.keys().next().value!);
        this.ref.requestInfo().catch((error: any) => {
          // Not asked (e.g. James throttled it): try again on a later read.
          if (requested.get(key) === now) requested.delete(key);
          this.#ctx.log('debug', `connection data of ${data.name}: ${error?.message ?? error}`);
        });
      }
    }
    return data.connection ?? {};
  }
  name() { return this.#d().name; }
  nick() { return this.#d().name; }
  phoneticName() { return this.#d().phoneticName; }
  id() { return this.#d().id; }
  uid() { return this.#d().uid; }
  uniqueId() { return this.#d().uid; }
  databaseID() { return this.#d().databaseId ?? ''; }
  databaseId() { return this.#d().databaseId ?? ''; }
  country() { return this.#d().country; }
  description() { return this.#d().description; }
  setDescription(description: string) {
    if (!this.#d().isSelf) return unsupported(this.#ctx, "setting another client's description");
    return fire(this.#ctx, 'setDescription', this.#bot.setDescription(String(description)));
  }
  setDescriptionHex(hex: string) { return this.setDescription(Buffer.from(String(hex), 'hex').toString('utf8')); }
  isSelf() { return this.#d().isSelf; }
  isRecording() { return this.#d().recording; }
  isMuted() { return this.#d().inputMuted; }
  isDeaf() { return this.#d().outputMuted; }
  isAway() { return this.#d().away !== null; }
  getAwayMessage() { return this.#d().away ?? ''; }
  getServerGroups() { return this.#d().serverGroups.map((id) => wrapGroup(this.#ctx, this.#bot, 'server', id)); }
  getChannelGroup() { return wrapGroup(this.#ctx, this.#bot, 'channel', this.#d().channelGroup); }
  getPing() { return this.#c().pingMs ?? 0; }
  getIPAddress() { return this.#c().address ?? ''; }
  getOnlineTime() { return this.#c().connectedMs ?? 0; }
  getIdleTime() { return this.#c().idleMs ?? 0; }
  getPacketLoss() { return this.#c().packetLoss ?? 0; }
  getBytesReceived() { return this.#c().bytesReceived ?? 0; }
  getBytesSent() { return this.#c().bytesSent ?? 0; }
  getTotalConnections() { return this.#d().info?.totalConnections ?? -1; }
  getCreationTime() { return (this.#d().info?.created ?? 0) * 1000 || -1; }
  getChannels() { const c = this.getAudioChannel(); return c ? [c] : []; }
  getAudioChannel() { return sChannel(this.#ctx, this.#bot, this.ref.channelObject()); }
  getURL() { const d = this.#d(); return `client://${d.id}/${d.uid}~${encodeURIComponent(d.name)}`; }
  getPlatform() { return this.#d().info?.platform ?? ''; }
  getVersion() { return this.#d().info?.version ?? ''; }
  type() { return this.#d().query ? 0 : 1; }
  equals(other: SinusbotClient) { return !!other && typeof other.id === 'function' && other.id() === this.id(); }
  chat(msg: string) { return fire(this.#ctx, 'chat', this.ref.chat(String(msg))); }
  poke(msg: string) { fire(this.#ctx, 'poke', this.ref.poke(String(msg))); }
  ban(time: number, msg: string) { fire(this.#ctx, 'ban', this.ref.ban(time < 0 ? 0 : time, msg ?? '')); }
  kick(msg: string) { fire(this.#ctx, 'kick', this.ref.kick(msg ?? '')); }
  kickFromServer(msg: string) { this.kick(msg); }
  kickFromChannel(msg: string) { fire(this.#ctx, 'kickFromChannel', this.ref.kickFromChannel(msg ?? '')); }
  addToServerGroup(group: unknown) { fire(this.#ctx, 'addToServerGroup', this.ref.addServerGroup(groupId(group))); }
  removeFromServerGroup(group: unknown) { fire(this.#ctx, 'removeFromServerGroup', this.ref.removeServerGroup(groupId(group))); }
  moveTo(target: unknown, password?: string) {
    const channel = channelId(target);
    if (this.#d().isSelf) {
      if (!channel) return unsupported(this.#ctx, 'leaving the voice channel');
      return fire(this.#ctx, 'moveTo', this.#bot.move(channel, password));
    }
    return fire(this.#ctx, 'moveTo', this.ref.moveTo(channel));
  }
  subscribe() { return unsupported(this.#ctx, 'client subscriptions'); }
  setSubscription() { return unsupported(this.#ctx, 'client subscriptions'); }
  requestConnectionInfoUpdate() { return fire(this.#ctx, 'requestConnectionInfoUpdate', this.ref.requestInfo()); }
  string() { return `Client{ ID: ${this.id()}, Name: ${this.name()} }`; }
  toString() { return this.string(); }
}

class SinusbotChannel {
  #ctx: Context;
  #bot: Bot;
  readonly ref: ChannelRef;
  constructor(ctx: Context, bot: Bot, ref: ChannelRef) {
    this.#ctx = ctx;
    this.#bot = bot;
    this.ref = ref;
  }
  #d() { return this.ref.data() as any; }
  #edit(what: string, params: Record<string, unknown>) { fire(this.#ctx, what, this.ref.update(params)); }
  id() { return this.#d().id; }
  name() { return this.#d().name; }
  parent() { return sChannel(this.#ctx, this.#bot, this.ref.parentChannel()); }
  position() { return this.#d().order; }
  setPosition(pos: number) {
    // SinusBot positions are indices among siblings; TeamSpeak orders by "sibling above".
    const siblings = (this.ref.parentChannel()?.children() ?? sortedChannels(this.#ctx).filter((c) => !c.data().parent)).filter((c) => c.id !== this.id());
    const above = pos <= 0 ? '0' : (siblings[Math.min(pos, siblings.length) - 1]?.id ?? '0');
    fire(this.#ctx, 'setPosition', this.ref.moveTo(this.#d().parent, above));
  }
  delete() { return fire(this.#ctx, 'delete', this.ref.delete(true)); }
  moveTo(parent: unknown, order?: number) {
    fire(this.#ctx, 'moveTo', this.ref.moveTo(channelId(parent) || null, order === undefined ? undefined : String(order)));
  }
  setName(name: string) { this.#edit('setName', { name }); }
  type() { return this.#d().text ? 1 : 0; }
  topic() { return this.#d().topic; }
  setTopic(topic: string) { this.#edit('setTopic', { topic }); }
  description() { return this.#d().description; }
  setDescription(description: string) { this.#edit('setDescription', { description }); }
  codec() { return this.#d().codec ?? 0; }
  setCodec(codec: number) { this.#edit('setCodec', { codec }); }
  codecQuality() { return this.#d().codecQuality ?? 0; }
  setCodecQuality(codecQuality: number) { this.#edit('setCodecQuality', { codecQuality }); }
  maxClients() { return this.#d().maxClients ?? -1; }
  setMaxClients(maxClients: number) { this.#edit('setMaxClients', { maxClients }); }
  maxFamilyClients() { return this.#d().maxFamilyClients ?? -1; }
  setMaxFamilyClients(maxFamilyClients: number) { this.#edit('setMaxFamilyClients', { maxFamilyClients }); }
  isPermanent() { return this.#d().permanent; }
  setPermanent(permanent: boolean) { this.#edit('setPermanent', { permanent }); }
  isSemiPermanent() { return this.#d().semiPermanent; }
  setSemiPermanent(semiPermanent: boolean) { this.#edit('setSemiPermanent', { semiPermanent }); }
  isDefault() { return this.#d().default; }
  setDefault(value: boolean) { this.#edit('setDefault', { default: value }); }
  isPassworded() { return this.#d().hasPassword; }
  setPassword(password: string) { this.#edit('setPassword', { password: password ?? '' }); }
  isEncrypted() { return this.#d().encrypted; }
  setEncrypted(encrypted: boolean) { this.#edit('setEncrypted', { encrypted }); }
  equals(other: SinusbotChannel) { return !!other && typeof other.id === 'function' && other.id() === this.id(); }
  chat(msg: string) { return fire(this.#ctx, 'chat', this.ref.chat(String(msg))); }
  getClients() { return this.ref.clients().map((c) => sClient(this.#ctx, this.#bot, c)); }
  getClientCount() { return this.ref.clients().length; }
  subscribe(value: boolean) { fire(this.#ctx, 'subscribe', this.ref.subscribe(value !== false)); }
  setSubscription(value: boolean) { this.subscribe(value); }
  update(params: Record<string, unknown>) { fire(this.#ctx, 'update', this.ref.update(channelParams(params))); }
  setChannelGroup(client: SinusbotClient, group: unknown) {
    fire(this.#ctx, 'setChannelGroup', client.ref.setChannelGroup(groupId(group), this.id()));
  }
  getPermissions() { unsupported(this.#ctx, 'channel permissions'); return []; }
  addPermission() { unsupported(this.#ctx, 'channel permissions'); return null; }
  getURL() { return `channel://${this.id()}/${encodeURIComponent(this.name())}`; }
  getMessages() { return unsupported(this.#ctx, 'Channel.getMessages (Discord)'); }
  string() { return `Channel{ ID: ${this.id()}, Name: ${this.name()} }`; }
  toString() { return this.string(); }
}

function groupId(group: unknown): string {
  if (group && typeof group === 'object' && typeof (group as any).id === 'function') return String((group as any).id());
  return String(group);
}

function channelId(channel: unknown): string {
  if (channel && typeof channel === 'object' && typeof (channel as any).id === 'function') return String((channel as any).id());
  return channel === undefined || channel === null ? '' : String(channel);
}

/** SinusBot ChannelParams → James ChannelParams. */
function channelParams(p: Record<string, any>): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const key of ['name', 'topic', 'description', 'password', 'codec', 'codecQuality', 'encrypted', 'permanent', 'semiPermanent', 'default', 'maxClients', 'maxFamilyClients', 'neededTalkPower', 'deleteDelay']) {
    if (p[key] !== undefined) out[key] = p[key];
  }
  if (p.parent !== undefined) out.parent = channelId(p.parent) || null;
  if (p.position !== undefined) out.position = String(p.position);
  return out;
}

function sClient(ctx: Context, bot: Bot, ref: ClientRef | null): SinusbotClient | undefined {
  if (!ref) return undefined;
  const cache: WeakMap<ClientRef, SinusbotClient> = (ctx.extra.sClients ??= new WeakMap());
  let c = cache.get(ref);
  if (!c) cache.set(ref, (c = new SinusbotClient(ctx, bot, ref)));
  return c;
}

function sChannel(ctx: Context, bot: Bot, ref: ChannelRef | null): SinusbotChannel | undefined {
  if (!ref) return undefined;
  const cache: WeakMap<ChannelRef, SinusbotChannel> = (ctx.extra.sChannels ??= new WeakMap());
  let c = cache.get(ref);
  if (!c) cache.set(ref, (c = new SinusbotChannel(ctx, bot, ref)));
  return c;
}

function wrapGroup(ctx: Context, bot: Bot, kind: 'server' | 'channel', id: string | null) {
  if (!id) return undefined;
  const data = (kind === 'server' ? ctx.state.serverGroups : ctx.state.channelGroups).get(id) ?? { id, name: '', icon: 0 };
  const byDbId = (client: unknown) => {
    if (client instanceof SinusbotClient) return client.ref;
    const dbid = String(client);
    return bot.clients().find((c) => c.databaseId === dbid) ?? null;
  };
  return {
    id: () => data.id,
    name: () => data.name,
    icon: () => String(data.icon),
    addClientByDatabaseId(client: unknown) {
      const ref = byDbId(client);
      if (!ref) return unsupported(ctx, 'adding offline clients to groups');
      return fire(ctx, 'addClientByDatabaseId', ref.addServerGroup(data.id));
    },
    removeClientByDatabaseId(client: unknown) {
      const ref = byDbId(client);
      if (!ref) return unsupported(ctx, 'removing offline clients from groups');
      return fire(ctx, 'removeClientByDatabaseId', ref.removeServerGroup(data.id));
    },
    getPermissions() { unsupported(ctx, 'group permissions'); return []; },
    addPermission() { unsupported(ctx, 'group permissions'); return null; },
  };
}

const isUrl = (value: string) => /^https?:\/\//i.test(value);

/** How long a started track counts as playing after James answered (see `starting`). */
const STARTING_GRACE_MS = 2_000;
/** Most URL aliases remembered per script (see `starting`). */
const MAX_ALIASES = 100;

/**
 * Plays `input` and counts it as playing until James answered: SinusBot starts tracks at once, and
 * scripts commonly poll `audio.isPlaying()` to start one, which would otherwise repeat the request
 * every poll while James resolves the URL. A single track resolved from a URL keeps that URL as its
 * id, so scripts recognise "their" track even when James normalises the URL.
 */
function starting(ctx: Context, what: string, play: Promise<unknown>, input?: string): boolean {
  ctx.extra.starting = (ctx.extra.starting ?? 0) + 1;
  const done = play.then((items: any) => {
    if (input === undefined || !Array.isArray(items) || items.length !== 1) return;
    const aliases: Map<string, string> = (ctx.extra.aliases ??= new Map());
    aliases.delete(items[0].track.locator);
    aliases.set(items[0].track.locator, input);
    if (aliases.size > MAX_ALIASES) aliases.delete(aliases.keys().next().value!);
  });
  // The playback status reaches the script separately from the answer: stay "playing" briefly.
  return fire(ctx, what, done.finally(() => setTimeout(() => ctx.extra.starting--, STARTING_GRACE_MS)));
}

function sTrack(ctx: Context, bot: Bot, track: any) {
  if (!track) return undefined;
  const url = track.locator;
  // A track known only by its URL is resolved by James when played.
  const play = (mode: string) => starting(ctx, 'play', bot.player.play(track.input ?? track, mode), track.input);
  return {
    // URLs are their own id (scripts keep ids in their settings, e.g. "play this song").
    id: () => {
      const locator = String(track.locator);
      return isUrl(locator) ? (ctx.extra.aliases?.get(locator) ?? locator) : `${track.source}:${locator}`;
    },
    url: () => url,
    type: () => track.source,
    title: () => track.title ?? '',
    artist: () => track.artist ?? '',
    tempTitle: () => (track.isStream ? (track.title ?? '') : ''),
    tempArtist: () => (track.isStream ? (track.artist ?? '') : ''),
    album: () => track.album ?? '',
    genre: () => '',
    duration: () => track.durationMs ?? 0,
    trackNumber: () => 0,
    thumbnail: () => track.thumbnail ?? '',
    filename: () => (track.source === 'local' ? path.basename(track.locator) : ''),
    sourceURL: () => (track.source === 'local' ? '' : track.locator),
    play: () => play('now'),
    enqueue: () => play('append'),
    addNext: () => play('next'),
    setThumbnailFromURL: () => unsupported(ctx, 'Track.setThumbnailFromURL'),
    setThumbnailFromUrl: () => unsupported(ctx, 'Track.setThumbnailFromURL'),
    removeThumbnail: () => unsupported(ctx, 'Track.removeThumbnail'),
    string: () => `Track { ${track.artist ?? ''} - ${track.title ?? ''} }`,
    toString() { return this.string(); },
  };
}

function libraryTrack(t: any) {
  return { source: 'local', locator: t.path, title: t.title, artist: t.artist, album: t.album, durationMs: t.durationMs };
}

function sPlaylist(ctx: Context, bot: Bot, playlist: any) {
  if (!playlist) return undefined;
  return {
    id: () => String(playlist.id),
    name: () => playlist.name,
    getTracks: () => {
      const full = ctx.callSync('playlists.get', { id: playlist.id });
      return (full.tracks ?? []).map((t: any, index: number) => ({
        title: () => t.title ?? '',
        artist: () => t.artist ?? '',
        album: () => t.album ?? '',
        url: () => t.locator,
        play: () => fire(ctx, 'play', bot.playlists.play(playlist.id, { start: index })),
      }));
    },
    setActive: () => unsupported(ctx, 'Playlist.setActive'),
  };
}

function sUser(ctx: Context, user: any) {
  if (!user) return undefined;
  const identity = () => (user.identities ?? [])[0];
  const set = (what: string, method: string, params: unknown) => fire(ctx, what, ctx.call(method, params));
  return {
    id: () => String(user.id),
    name: () => user.username,
    // SinusBot privilege bitmask: administrators have all bits.
    privileges: () => (user.isAdmin ? 0x7fffffff : 0),
    instancePrivileges: () => (user.isAdmin ? 0x7fffffff : 0),
    tsUid: () => identity()?.uid ?? '',
    uid: () => identity()?.uid ?? '',
    tsGroupId: () => '',
    groupId: () => '',
    isAdmin: () => !!user.isAdmin,
    setPassword: (password: string) => set('setPassword', 'users.setPassword', { id: user.id, password }),
    setTSUid: (uid: string) => set('setUid', 'users.link', { id: user.id, platform: ctx.platform, uid, name: user.username }),
    setUid: (uid: string) => set('setUid', 'users.link', { id: user.id, platform: ctx.platform, uid, name: user.username }),
    setGroupId: () => unsupported(ctx, 'User.setGroupId'),
    setPrivileges: () => unsupported(ctx, 'User privileges (use roles)'),
    addPrivilege: () => unsupported(ctx, 'User privileges (use roles)'),
    removePrivilege: () => unsupported(ctx, 'User privileges (use roles)'),
    setInstancePrivileges: () => unsupported(ctx, 'User privileges (use roles)'),
    addInstancePrivilege: () => unsupported(ctx, 'User privileges (use roles)'),
    removeInstancePrivilege: () => unsupported(ctx, 'User privileges (use roles)'),
    delete: () => set('delete', 'users.delete', { id: user.id }),
  };
}

// ---- modules ----------------------------------------------------------------------------------

function modules(ctx: Context, bot: Bot, rt: Runtime, scriptModule: { exports: unknown }, on: (name: string, cb: Function) => void) {
  const client = (ref: ClientRef | null) => sClient(ctx, bot, ref);
  const channel = (ref: ChannelRef | null) => sChannel(ctx, bot, ref);
  const info = ctx.info as any;
  const settings = { logLevel: 3 };

  const engine = {
    getInstanceID: () => ctx.instance,
    getBotID: () => ctx.instance,
    getBackend: () => (ctx.platform === 'teamspeak' ? 'ts3' : ctx.platform),
    setInstanceLogLevel: (level: number) => ((settings.logLevel = level), true),
    setBotLogLevel: () => true,
    getInstanceLogLevel: () => settings.logLevel,
    getBotLogLevel: () => settings.logLevel,
    reloadScripts: () => unsupported(ctx, 'engine.reloadScripts (use the web interface)'),
    getNick: () => info.nickname ?? '',
    setNick: (nick: string) => fire(ctx, 'setNick', bot.setNick(String(nick))),
    getDefaultChannelID: () => String(info.defaultChannel ?? ''),
    setDefaultChannelID: () => unsupported(ctx, 'engine.setDefaultChannelID (use the instance settings)'),
    isRunning: () => true,
    notify: (message: string) => void fire(ctx, 'notify', bot.notify(String(message))),
    saveConfig: (config: Record<string, unknown>) => fire(ctx, 'saveConfig', bot.saveConfig(config)),
    log: (...args: unknown[]) => ctx.log('info', ...args.map(plain)),
    logf: (format: string, ...args: unknown[]) => ctx.log('info', util.format(goFormat(format), ...args.map(plain))),
    export: (obj: unknown) => void (scriptModule.exports = obj),
    removeAvatar: () => unsupported(ctx, 'avatars'),
    setAvatarFromTrack: () => unsupported(ctx, 'avatars'),
    setDefaultAvatar: () => unsupported(ctx, 'avatars'),
    setAvatarFromBanner: () => unsupported(ctx, 'avatars'),
    setAvatarFromURL: () => unsupported(ctx, 'avatars'),
    getUsers: () => tryOr(ctx, 'getUsers', () => ctx.callSync('users.list').map((u: any) => sUser(ctx, u)), []),
    getUserById: (id: string) => tryOr(ctx, 'getUserById', () => sUser(ctx, ctx.callSync('users.get', { id: Number(id) })), undefined),
    getUserByID: (id: string) => engine.getUserById(id),
    getUserByName: (username: string) => tryOr(ctx, 'getUserByName', () => sUser(ctx, ctx.callSync('users.byName', { username })), undefined),
    addUser: (username: string) => tryOr(ctx, 'addUser', () => sUser(ctx, ctx.callSync('users.create', { username })), undefined),
    setCommandPrefix: () => unsupported(ctx, 'engine.setCommandPrefix (use the instance settings)'),
    getCommandPrefix: () => ctx.info.commandPrefix,
    version: () => `James ${(bot as any).version ?? ''}`.trim(),
    os: () => process.platform,
    disableRegistration: () => {},
    enableRegistration: () => {},
    registrationEnabled: () => false,
    isSubscriptionMode: () => false,
    setSubscriptionMode: () => unsupported(ctx, 'subscription mode'),
    getChannelSubscriptions: () => [],
    getClientSubscriptions: () => [],
    setChannelSubscription: () => unsupported(ctx, 'subscription mode'),
    getIgnoreCommandsFromServerChat: () => false,
    getIgnoreCommandsFromChannelChat: () => false,
    getIgnoreCommandsFromPrivateChat: () => false,
    setIgnoreCommandsFromServerChat: () => unsupported(ctx, 'ignoring command sources'),
    setIgnoreCommandsFromChannelChat: () => unsupported(ctx, 'ignoring command sources'),
    setIgnoreCommandsFromPrivateChat: () => unsupported(ctx, 'ignoring command sources'),
  };

  const scoped = (s: (typeof bot.store)['plugin']) => ({
    set: (key: string, value: unknown) => (s.set(key, value), true),
    get: (key: string) => s.get(key),
    unset: (key: string) => s.delete(key),
    getKeys: () => s.keys(),
    getAll: () => Object.fromEntries(s.entries()),
  });
  const plugin = scoped(bot.store.plugin);
  const global = scoped(bot.store.global);
  const instance = scoped(bot.store.instance);
  const store = {
    ...plugin,
    setGlobal: global.set, getGlobal: global.get, unsetGlobal: global.unset, getKeysGlobal: global.getKeys, getAllGlobal: global.getAll,
    setInstance: instance.set, getInstance: instance.get, unsetInstance: instance.unset, getKeysInstance: instance.getKeys, getAllInstance: instance.getAll,
  };

  const backend = {
    connect: () => fire(ctx, 'connect', bot.instances.start(ctx.instance)),
    disconnect: () => fire(ctx, 'disconnect', bot.instances.stop(ctx.instance)),
    isConnected: () => bot.connected,
    getBotClientID: () => bot.self?.id ?? '',
    getBotClient: () => client(bot.self),
    getNick: () => bot.self?.name ?? '',
    getChannelByID: (id: string) => channel(bot.channel(String(id))),
    getCurrentChannel: () => channel(bot.self?.channelObject() ?? null),
    getChannelByName: (name: string) => channel(bot.channelByName(name)),
    getChannelsByName: (name: string) => bot.channelsByName(name).map((c) => channel(c)),
    getChannelCount: () => bot.channels().length,
    getChannels: () => bot.channels().map((c) => channel(c)),
    getClients: () => bot.clients().map((c) => client(c)),
    getClientByID: (id: string) => client(bot.client(String(id))),
    getClientByName: (name: string) => client(bot.clientByName(name)),
    getClientByNick: (name: string) => client(bot.clientByName(name)),
    getClientByUniqueID: (uid: string) => client(bot.clientByUid(uid)),
    getClientByUID: (uid: string) => client(bot.clientByUid(uid)),
    chat: (msg: string) => void fire(ctx, 'chat', bot.chatServer(String(msg))),
    createChannel: (params: Record<string, unknown>) => {
      const id = tryOr(ctx, 'createChannel', () => ctx.callSync('channel.create', channelParams(params)), null);
      return id ? channel(wrapChannel(ctx, ctx.state.channels.get(id) ?? { id, parent: channelId(params.parent) || null, name: String(params.name) })) : undefined;
    },
    getServerGroupByID: (id: string) => (ctx.state.serverGroups.has(String(id)) ? wrapGroup(ctx, bot, 'server', String(id)) : undefined),
    getChannelGroupByID: (id: string) => (ctx.state.channelGroups.has(String(id)) ? wrapGroup(ctx, bot, 'channel', String(id)) : undefined),
    getServerGroups: () => bot.serverGroups().map((g) => wrapGroup(ctx, bot, 'server', g.id)),
    getChannelGroups: () => bot.channelGroups().map((g) => wrapGroup(ctx, bot, 'channel', g.id)),
    extended: () => (ctx.platform === 'teamspeak' ? extendedTs3(ctx, bot) : undefined),
    setAway: (away: boolean, reason?: string) => fire(ctx, 'setAway', bot.setAway(away ? (reason ?? '') : null)),
  };

  const currentTrack = () => sTrack(ctx, bot, bot.player.current?.track);
  const jobs = { next: 1 };
  /** youtube-dl style job: plays/enqueues and reports ytdl.success / ytdl.error. */
  const ytJob = (url: string, mode: string) => {
    const jobId = `job-${ctx.instance}-${jobs.next++}`;
    bot.player.play(String(url), mode).then(
      () => ctx.dispatch('compat:ytdl.success', { url, jobId }),
      (error) => ctx.dispatch('compat:ytdl.error', { ev: { url, jobId }, message: error?.message ?? String(error) }),
    );
    return jobId;
  };
  const media = {
    playURL: (url: string) => starting(ctx, 'playURL', bot.player.play(String(url), 'now'), String(url)),
    getCurrentTrack: () => currentTrack(),
    // A library id, a URL (YouTube, SoundCloud, a stream, …) or a local file's id (`local:<path>`).
    // SinusBot's own ids do not exist in James: settings holding them need a URL instead.
    getTrackByID: (id: string) => {
      const value = String(id ?? '').trim();
      if (/^\d+$/.test(value)) {
        return tryOr(ctx, 'getTrackByID', () => sTrack(ctx, bot, libraryTrack(ctx.callSync('library.get', { id: Number(value) }))), undefined);
      }
      if (isUrl(value)) return sTrack(ctx, bot, { source: 'url', locator: value, title: value, input: value });
      // Local files (other sources use URLs as ids).
      if (value.startsWith('local:') && value.length > 6) {
        const locator = value.slice(6);
        return sTrack(ctx, bot, { source: 'local', locator, title: path.basename(locator), artist: null, album: null, durationMs: null, thumbnail: null, isStream: false });
      }
      return undefined;
    },
    getTracks: () => tryOr(ctx, 'getTracks', () => ctx.callSync('library.search', { query: '', limit: 200 }).map((t: any) => sTrack(ctx, bot, libraryTrack(t))), []),
    search: (query: string) => tryOr(ctx, 'search', () => ctx.callSync('library.search', { query: String(query), limit: 20 }).map((t: any) => sTrack(ctx, bot, libraryTrack(t))), []),
    enqueue: (url: string) => starting(ctx, 'enqueue', bot.player.play(String(url), 'append'), String(url)),
    playAsNext: (url: string) => fire(ctx, 'playAsNext', bot.player.play(String(url), 'next')),
    playNext: () => fire(ctx, 'playNext', bot.player.skip()),
    playPrevious: () => fire(ctx, 'playPrevious', bot.player.previous()),
    stop: () => fire(ctx, 'stop', bot.player.stop()),
    getQueue: () => bot.player.queue.map((item: any) => sTrack(ctx, bot, item.track)),
    getPlaylists: () => tryOr(ctx, 'getPlaylists', () => ctx.callSync('playlists.list').map((p: any) => sPlaylist(ctx, bot, p)), []),
    getPlaylistByID: (id: string) => tryOr(ctx, 'getPlaylistByID', () => sPlaylist(ctx, bot, ctx.callSync('playlists.get', { id: Number(id) })), undefined),
    getActivePlaylist: () => undefined,
    removeFromQueue: (index: number) => fire(ctx, 'removeFromQueue', bot.player.removeAt(index)),
    clearQueue: () => fire(ctx, 'clearQueue', bot.player.clear()),
    clearPlaylist: () => true,
    yt: (url: string) => ytJob(url, 'now'),
    ytStream: (url: string) => fire(ctx, 'ytStream', bot.player.play(String(url), 'now')),
    ytdl: (url: string, play: boolean) => ytJob(url, play ? 'now' : 'append'),
    enqueueYt: (url: string) => ytJob(url, 'append'),
    enqueueYtdl: (url: string) => ytJob(url, 'append'),
    playQueueNext: () => void fire(ctx, 'playQueueNext', bot.player.skip()),
    resumeQueue: () => void fire(ctx, 'resumeQueue', bot.player.skip()),
    clearIdleTrack: () => unsupported(ctx, 'idle tracks'),
    clearStartupTrack: () => unsupported(ctx, 'startup tracks'),
    setIdleTrackURL: () => unsupported(ctx, 'idle tracks'),
    setStartupTrackURL: () => unsupported(ctx, 'startup tracks'),
    playlistPlayByID: (playlist: any, index: number) => void fire(ctx, 'playlistPlayByID', bot.playlists.play(Number(playlist?.id?.() ?? playlist), { start: index ?? 0 })),
  };

  const audio = {
    setAudioFilter: () => unsupported(ctx, 'audio filters'),
    setAudioReturnChannel: () => unsupported(ctx, 'audio return channels'),
    getAudioReturnChannel: () => 0,
    startRecording: () => unsupported(ctx, 'recording'),
    stopRecording: () => unsupported(ctx, 'recording'),
    streamToServer: () => unsupported(ctx, 'streaming to Icecast'),
    stopStream: () => unsupported(ctx, 'streaming to Icecast'),
    isRepeat: () => bot.player.status.repeat !== 'off',
    setRepeat: (value: boolean) => void fire(ctx, 'setRepeat', bot.player.setRepeat(value ? 'all' : 'off')),
    isShuffle: () => !!bot.player.status.shuffle,
    setShuffle: (value: boolean) => void fire(ctx, 'setShuffle', bot.player.setShuffle(!!value)),
    getVolume: () => (ctx.extra.mutedVolume !== undefined ? ctx.extra.mutedVolume : (bot.player.status.volume ?? 0)),
    setVolume: (volume: number) => {
      if (ctx.extra.mutedVolume !== undefined) ctx.extra.mutedVolume = volume;
      else fire(ctx, 'setVolume', bot.player.setVolume(volume));
      return true;
    },
    getTrackPosition: () => bot.player.status.positionMs,
    seek: (pos: number) => void fire(ctx, 'seek', bot.player.seek(pos)),
    isMute: () => ctx.extra.mutedVolume !== undefined,
    setMute: (mute: boolean) => {
      // Emulated through the volume.
      if (mute && ctx.extra.mutedVolume === undefined) {
        ctx.extra.mutedVolume = bot.player.status.volume ?? 0;
        return fire(ctx, 'setMute', bot.player.setVolume(0));
      }
      if (!mute && ctx.extra.mutedVolume !== undefined) {
        const volume = ctx.extra.mutedVolume;
        ctx.extra.mutedVolume = undefined;
        return fire(ctx, 'setMute', bot.player.setVolume(volume));
      }
      return true;
    },
    isPlaying: () => (ctx.extra.starting ?? 0) > 0 || (!!bot.player.current && !bot.player.status.paused),
    say: (text: string, locale?: string) =>
      void fire(ctx, 'say', bot.say(String(text), { language: locale ?? ctx.extra.ttsLocale ?? undefined })),
    setStreamVolume: () => unsupported(ctx, 'per-stream volume'),
    setTTSURL: () => unsupported(ctx, 'audio.setTTSURL (James uses its own voices)'),
    setTTSDefaultLocale: (locale: string) => ((ctx.extra.ttsLocale = String(locale)), true),
  };

  const teamspeak = ctx.platform === 'teamspeak';
  const format = {
    color: (text: string, color: string) => (teamspeak ? `[color=${color}]${text}[/color]` : String(text)),
    italic: (text: string) => (teamspeak ? `[i]${text}[/i]` : `*${text}*`),
    bold: (text: string) => (teamspeak ? `[b]${text}[/b]` : `**${text}**`),
    underline: (text: string) => (teamspeak ? `[u]${text}[/u]` : `__${text}__`),
    code: (text: string) => (teamspeak ? `[noparse]${text}[/noparse]` : `\`${text}\``),
  };

  const hash = (algorithm: string) => (input: unknown) => nodeCrypto.createHash(algorithm).update(toBuffer(input)).digest('hex');
  const helpers = {
    getRandom: (max: number) => nodeCrypto.randomInt(Math.max(1, Math.floor(max))),
    getRandPerm: (max: number) => {
      const out = [...Array(Math.max(0, Math.floor(max))).keys()];
      for (let i = out.length - 1; i > 0; i--) {
        const j = nodeCrypto.randomInt(i + 1);
        [out[i], out[j]] = [out[j], out[i]];
      }
      return out;
    },
    newBytes: () => new BytesWriter(),
    bytesFromString: (value: string) => new BytesWriter(Buffer.from(String(value), 'utf8')),
    bytesFromHex: (value: string) => new BytesWriter(Buffer.from(String(value), 'hex')),
    bytesFromBase64: (value: string) => new BytesWriter(Buffer.from(String(value), 'base64')),
    base64Encode: (input: unknown) => toBuffer(input).toString('base64'),
    base64Decode: (input: string) => Buffer.from(String(input), 'base64').toString('utf8'),
    hexEncode: (input: unknown) => toBuffer(input).toString('hex'),
    hexDecode: (input: string) => Buffer.from(String(input), 'hex').toString('utf8'),
    MD5Sum: hash('md5'),
    SHA1Sum: hash('sha1'),
    SHA256Sum: hash('sha256'),
    toString: (obj: unknown) => (obj instanceof Bytes || obj instanceof BytesWriter ? obj.toString() : typeof obj === 'string' ? obj : JSON.stringify(obj)),
    document: () => '',
    typeOf: (obj: unknown) => typeof obj,
  };

  const event = {
    on: (name: string, cb: Function) => on(String(name), cb),
    emit: (name: string, data: unknown) => bot.emit(String(name), data),
    broadcast: (name: string, data: unknown) => bot.emit(String(name), data, { broadcast: true }),
  };

  const http = {
    simpleRequest(config: any, callback: (error: string | undefined, response?: any) => void) {
      const done = (error: string | undefined, response?: unknown) => ctx.guard('http callback', () => callback?.(error, response));
      if (!rt.has('http')) {
        done("the plugin lacks the permission 'http'");
        return;
      }
      const controller = new AbortController();
      const timer = setTimeout(() => controller.abort(), Number(config?.timeout ?? 10_000));
      const maxSize = Number(config?.maxSize ?? 5 << 20);
      (async () => {
        const response = await fetch(String(config.url), {
          method: String(config.method ?? 'GET').toUpperCase(),
          headers: config.headers ?? {},
          body: config.body === undefined ? undefined : new Uint8Array(toBuffer(config.body)),
          signal: controller.signal,
          redirect: 'follow',
        });
        const reader = response.body?.getReader();
        const chunks: Buffer[] = [];
        let size = 0;
        while (reader) {
          const { done: end, value } = await reader.read();
          if (end) break;
          size += value.length;
          if (size > maxSize) {
            controller.abort();
            throw new Error('response too large');
          }
          chunks.push(Buffer.from(value));
        }
        const headers: Record<string, string> = {};
        response.headers.forEach((v, k) => (headers[k] = v));
        done(undefined, { statusCode: response.status, status: `${response.status} ${response.statusText}`, headers, data: new Bytes(Buffer.concat(chunks)) });
      })()
        .catch((error) => done(error?.name === 'AbortError' ? 'timeout' : (error?.message ?? String(error))))
        .finally(() => clearTimeout(timer));
    },
  };

  const net = {
    connect(params: any, callback?: (error?: string) => void) {
      if (!rt.has('net')) {
        ctx.log('warn', "net.connect: the plugin lacks the permission 'net'");
        return null;
      }
      return netConnect(ctx, params, callback);
    },
  };

  const ws = {
    write: (connection: string, _type: number, message: unknown) => bot.web.send(Bytes.of(message).toString(), String(connection)),
    broadcast: (_type: number, message: unknown) => bot.web.send(Bytes.of(message).toString()),
    close: (connection: string) => bot.web.close(String(connection)),
  };

  const db = {
    connect(params: any, callback?: (error?: string) => void) {
      if (!rt.has('db')) {
        ctx.log('warn', "db.connect: the plugin lacks the permission 'db'");
        return null;
      }
      return dbConnect(ctx, rt, params, callback);
    },
  };

  const files = fileModule(ctx, rt);
  const graphics = {
    removeBanner: () => unsupported(ctx, 'graphics (banners)'),
    setBanner: () => unsupported(ctx, 'graphics (banners)'),
  };

  return { engine, store, backend, media, audio, format, helpers, event, http, net, ws, db, fs: files, graphics, crypto: cryptoModule() };
}

function tryOr<T>(ctx: Context, what: string, fn: () => T, fallback: T): T {
  try {
    return fn();
  } catch (error: any) {
    ctx.log('warn', `${what}: ${error?.message ?? error}`);
    return fallback;
  }
}

/** Objects with SinusBot-style accessors print like SinusBot (`Client{ … }`). */
function plain(value: unknown): unknown {
  if (value && typeof value === 'object' && typeof (value as any).string === 'function') return (value as any).string();
  return value;
}

/** Go-style format verbs → util.format verbs. */
function goFormat(format: string): string {
  return String(format).replace(/%[vqtxXbeEfFgG]/g, (verb) => (verb === '%v' || verb === '%q' || verb === '%t' ? '%s' : verb === '%f' ? '%d' : '%s'));
}

function extendedTs3(ctx: Context, bot: Bot) {
  return {
    getServerInfo: () => {
      const s: any = bot.server ?? {};
      const values: Record<string, unknown> = {
        name: s.name ?? '', uid: s.uid ?? '', welcomeMessage: s.welcomeMessage ?? '', platform: s.platform ?? '', version: 0,
        hostMessage: s.hostMessage ?? '', hostBannerURL: s.hostBannerUrl ?? '', hostBannerGFXURL: s.hostBannerGfxUrl ?? '',
        hostBannerGFXInterval: s.hostBannerGfxInterval ?? 0, hostButtonURL: s.hostButtonUrl ?? '', hostButtonGFXURL: s.hostButtonGfxUrl ?? '',
        namePhonetic: s.phoneticName ?? '', serverID: Number(s.id ?? 0), iconID: s.icon ?? 0, reservedSlots: 0,
        defaultServerGroup: Number(s.defaultServerGroup ?? 0), defaultChannelGroup: Number(s.defaultChannelGroup ?? 0),
        defaultChannelAdminGroup: 0, codecEncryptionMode: s.codecEncryptionMode ?? 0, created: s.created ?? 0,
      };
      const info: Record<string, unknown> = { asObject: () => ({ ...values }) };
      for (const [k, v] of Object.entries(values)) info[k] = () => v;
      return info;
    },
    requestExtendedServerInfo: () => unsupported(ctx, 'requestExtendedServerInfo'),
  };
}

function netConnect(ctx: Context, params: any, callback?: (error?: string) => void) {
  const handlers: Record<string, Function[]> = { data: [], close: [], error: [] };
  const emit = (name: string, ...args: unknown[]) => {
    for (const h of handlers[name] ?? []) ctx.guard(`net ${name}`, () => h(...args));
  };
  const protocol = String(params?.protocol ?? 'tcp');
  let send: (data: Buffer) => void;
  let close: () => void;
  if (protocol === 'ws') {
    const socket = new WebSocket(String(params.url));
    socket.binaryType = 'arraybuffer';
    socket.onopen = () => ctx.guard('net connect', () => callback?.());
    socket.onmessage = (e) => emit('data', new Bytes(Buffer.from(typeof e.data === 'string' ? e.data : new Uint8Array(e.data))));
    socket.onerror = () => emit('error', 'websocket error');
    socket.onclose = () => emit('close');
    send = (data) => socket.send(data);
    close = () => socket.close();
  } else if (protocol === 'udp') {
    const dgram = process.getBuiltinModule('node:dgram');
    const socket = dgram.createSocket(params.host?.includes(':') ? 'udp6' : 'udp4');
    socket.on('message', (msg) => emit('data', new Bytes(msg)));
    socket.on('error', (e) => emit('error', e.message));
    socket.on('close', () => emit('close'));
    socket.connect(Number(params.port), String(params.host), () => ctx.guard('net connect', () => callback?.()));
    send = (data) => socket.send(data);
    close = () => socket.close();
  } else {
    const netModule = process.getBuiltinModule('node:net');
    const socket = netModule.connect({ host: String(params.host), port: Number(params.port) }, () => ctx.guard('net connect', () => callback?.()));
    socket.on('data', (data) => emit('data', new Bytes(data)));
    socket.on('error', (e) => {
      emit('error', e.message);
      if (!socket.remoteAddress) ctx.guard('net connect', () => callback?.(e.message));
    });
    socket.on('close', () => emit('close'));
    send = (data) => socket.write(data);
    close = () => socket.destroy();
  }
  ctx.onStop(() => close());
  return {
    write(data: unknown, format?: 'hex' | 'base64') {
      const buffer = typeof data === 'string' && format ? Buffer.from(data, format) : toBuffer(data);
      send(buffer);
    },
    on(name: string, cb: Function) {
      (handlers[name] ??= []).push(cb);
    },
    close: () => close(),
  };
}

function dbConnect(ctx: Context, rt: Runtime, params: any, callback?: (error?: string) => void) {
  const driver = String(params?.driver ?? '');
  if (driver !== 'sqlite3' && driver !== 'sqlite') {
    const message = `database driver '${driver}' is not built in (use SQLite or an npm driver in a native plugin)`;
    ctx.log('warn', `db.connect: ${message}`);
    setImmediate(() => ctx.guard('db connect', () => callback?.(message)));
    return null;
  }
  const { DatabaseSync } = process.getBuiltinModule('node:sqlite');
  const file = insideDataDir(rt, String(params.database ?? params.host ?? 'database.sqlite'));
  const database = new DatabaseSync(file);
  ctx.onStop(() => database.close());
  setImmediate(() => ctx.guard('db connect', () => callback?.()));
  const split = (args: unknown[]) => {
    const cb = typeof args[args.length - 1] === 'function' ? (args.pop() as Function) : undefined;
    return { cb, values: args.map((v) => (v instanceof Bytes || v instanceof BytesWriter ? v.toString() : v)) as any[] };
  };
  // SinusBot returns text columns as Bytes.
  const row = (r: Record<string, unknown>) => Object.fromEntries(Object.entries(r).map(([k, v]) => [k, typeof v === 'string' ? new Bytes(Buffer.from(v)) : v]));
  return {
    query(sql: string, ...args: unknown[]) {
      const { cb, values } = split(args);
      setImmediate(() => {
        let result: unknown[] | undefined;
        let error: string | undefined;
        try {
          result = database.prepare(String(sql)).all(...values).map((r) => row(r as Record<string, unknown>));
        } catch (e: any) {
          error = e?.message ?? String(e);
        }
        ctx.guard('db query callback', () => cb?.(error, result));
      });
    },
    exec(sql: string, ...args: unknown[]) {
      const { cb, values } = split(args);
      setImmediate(() => {
        let error: string | undefined;
        try {
          database.prepare(String(sql)).run(...values);
        } catch (e: any) {
          error = e?.message ?? String(e);
        }
        ctx.guard('db exec callback', () => cb?.(error, undefined));
      });
    },
  };
}

/** Resolves a path inside the plugin's data directory (no escaping it). */
function insideDataDir(rt: Runtime, file: string): string {
  const resolved = path.resolve(rt.dataDir, file);
  if (resolved !== rt.dataDir && !resolved.startsWith(rt.dataDir + path.sep)) {
    throw new Error(`'${file}' is outside the plugin's data directory`);
  }
  return resolved;
}

function fileModule(ctx: Context, rt: Runtime) {
  const allowed = () => {
    if (!rt.has('fs')) throw new Error("the plugin lacks the permission 'fs'");
  };
  const wrap = <T>(what: string, fallback: T, fn: () => T): T => {
    try {
      allowed();
      return fn();
    } catch (error: any) {
      ctx.log('warn', `fs.${what}: ${error?.message ?? error}`);
      return fallback;
    }
  };
  const info = (name: string, st: fs.Stats) => ({
    name: () => name,
    size: () => st.size,
    mode: () => st.mode,
    modTime: () => st.mtime,
    isDir: () => st.isDirectory(),
  });
  return {
    exists: (p: string) => wrap('exists', false, () => fs.existsSync(insideDataDir(rt, p))),
    readFile: (p: string) => wrap('readFile', undefined, () => new Bytes(fs.readFileSync(insideDataDir(rt, p)))),
    writeFile: (p: string, data: unknown, mode?: number) =>
      wrap('writeFile', false, () => (fs.writeFileSync(insideDataDir(rt, p), toBuffer(data), { mode: mode ?? 0o644 }), true)),
    stat: (p: string) => wrap('stat', undefined, () => info(path.basename(p), fs.statSync(insideDataDir(rt, p)))),
    readDir: (p: string) =>
      wrap('readDir', [] as any[], () => {
        const dir = insideDataDir(rt, p);
        return fs.readdirSync(dir).map((name) => info(name, fs.statSync(path.join(dir, name))));
      }),
    remove: (p: string) => wrap('remove', false, () => (fs.rmSync(insideDataDir(rt, p), { recursive: true, force: true }), true)),
    mkDir: (p: string) => wrap('mkDir', false, () => (fs.mkdirSync(insideDataDir(rt, p)), true)),
    mkDirAll: (p: string) => wrap('mkDirAll', false, () => (fs.mkdirSync(insideDataDir(rt, p), { recursive: true }), true)),
    rename: (p: string, to: string) => wrap('rename', false, () => (fs.renameSync(insideDataDir(rt, p), insideDataDir(rt, to)), true)),
  };
}

/**
 * crypto: keypairs are X25519, `seal`/`open` use XChaCha-free ChaCha20-Poly1305 with a
 * random nonce prepended (ciphertexts are not interchangeable with SinusBot's).
 */
function cryptoModule() {
  const keypair = (privateKey: nodeCrypto.KeyObject) => {
    const publicKey = nodeCrypto.createPublicKey(privateKey);
    return {
      export: () => new BytesWriter(Buffer.from(privateKey.export({ format: 'der', type: 'pkcs8' }))),
      publicKey: () => new BytesWriter(Buffer.from(publicKey.export({ format: 'der', type: 'spki' }))),
      sharedKey: (other: unknown) =>
        new BytesWriter(nodeCrypto.diffieHellman({ privateKey, publicKey: nodeCrypto.createPublicKey({ key: toBuffer(other), format: 'der', type: 'spki' }) })),
    };
  };
  const key = (secret: unknown) => nodeCrypto.createHash('sha256').update(toBuffer(secret)).digest();
  return {
    generateKeypair: () => keypair(nodeCrypto.generateKeyPairSync('x25519').privateKey),
    loadKeypair: (bytes: unknown) => keypair(nodeCrypto.createPrivateKey({ key: toBuffer(bytes), format: 'der', type: 'pkcs8' })),
    seal(secret: unknown, message: unknown) {
      const nonce = nodeCrypto.randomBytes(12);
      const cipher = nodeCrypto.createCipheriv('chacha20-poly1305', key(secret), nonce, { authTagLength: 16 });
      const body = Buffer.concat([cipher.update(toBuffer(message)), cipher.final()]);
      return new BytesWriter(Buffer.concat([nonce, cipher.getAuthTag(), body]));
    },
    open(secret: unknown, message: unknown) {
      const data = toBuffer(message);
      const decipher = nodeCrypto.createDecipheriv('chacha20-poly1305', key(secret), data.subarray(0, 12), { authTagLength: 16 });
      decipher.setAuthTag(data.subarray(12, 28));
      return new BytesWriter(Buffer.concat([decipher.update(data.subarray(28)), decipher.final()]));
    },
    pbkdf2: (hash: string, password: unknown, salt: unknown, iterations: number, keylength: number) =>
      new BytesWriter(nodeCrypto.pbkdf2Sync(toBuffer(password), toBuffer(salt), iterations, keylength, String(hash || 'sha256'))),
    randomBytes: (n: number) => new BytesWriter(nodeCrypto.randomBytes(Math.max(0, Math.floor(n)))),
  };
}

// ---- events -----------------------------------------------------------------------------------

/** Subscribes a SinusBot event handler to the corresponding James events. */
function subscribe(ctx: Context, bot: Bot, name: string, cb: Function): void {
  const client = (ref: ClientRef | null) => sClient(ctx, bot, ref);
  const channel = (ref: ChannelRef | null) => sChannel(ctx, bot, ref);
  const call = (...args: unknown[]) => cb(...args);
  const on = (event: string, convert: (p: any) => unknown[] | null) =>
    bot.on(event as any, (p: any) => {
      const args = convert(p);
      if (args) return call(...args);
    });
  const moveInfo = (p: any, from: ChannelRef | null, to: ChannelRef | null) => ({
    client: client(p.client),
    fromChannel: channel(from),
    toChannel: channel(to),
    invoker: client(p.invoker),
    message: p.message ?? '',
  });
  const simple = ['clientAway', 'clientBack', 'clientRecord', 'clientRecordStop', 'clientMute', 'clientUnmute', 'clientDeaf', 'clientUndeaf'];

  if (name.startsWith('api:') || name.startsWith('public:')) {
    const isPublic = name.startsWith('public:');
    const eventName = name.slice(name.indexOf(':') + 1);
    bot.web.handle(eventName, (request: any) => call(apiEvent(ctx, request)), { public: isPublic });
    return;
  }
  if (simple.includes(name)) {
    on(name, (p) => [client(p.client)]);
    return;
  }
  switch (name) {
    case 'chat':
      on('message', (p) => [{ text: p.text, client: client(p.client), channel: channel(p.channel), mode: MODES[p.mode] }]);
      return;
    case 'poke':
      on('poke', (p) => [{ text: p.text, client: client(p.client), mode: 1 }]);
      return;
    case 'connect':
      on('connect', () => []);
      return;
    case 'disconnect':
      on('disconnect', () => (ctx.extra.everConnected ? [] : null));
      return;
    case 'connectionFailed':
      on('disconnect', (p) => (ctx.extra.everConnected ? null : [p.reason]));
      return;
    case 'clientMove':
      on('clientJoin', (p) => [moveInfo(p, null, p.client.channelObject())]);
      on('clientMove', (p) => (p.reason === 'visible' ? null : [moveInfo(p, p.from, p.to)]));
      on('clientLeave', (p) => (p.reason === 'invisible' ? null : [moveInfo(p, wrapChannel(ctx, p.client.channel), null)]));
      return;
    case 'clientVisible':
      on('clientMove', (p) => (p.reason === 'visible' ? [moveInfo(p, null, p.to)] : null));
      return;
    case 'clientInvisible':
      on('clientLeave', (p) => (p.reason === 'invisible' ? [moveInfo(p, wrapChannel(ctx, p.client.channel), null)] : null));
      return;
    case 'clientKicked':
      on('clientLeave', (p) => (p.reason === 'kicked' ? [moveInfo(p, wrapChannel(ctx, p.client.channel), null)] : null));
      return;
    case 'clientBanned':
      on('clientLeave', (p) =>
        p.reason === 'banned' ? [{ client: client(p.client), invoker: client(p.invoker), fromChannel: channel(wrapChannel(ctx, p.client.channel)), message: p.message ?? '', time: 0 }] : null,
      );
      return;
    case 'clientKickedFromChannel':
      on('clientMove', (p) => (p.reason === 'kicked' ? [moveInfo(p, p.from, p.to)] : null));
      return;
    case 'clientNick':
      on('clientNick', (p) => [client(p.client), p.oldName]);
      return;
    case 'clientIPAddress':
      ctx.on('raw:clientInfo', (p: any) => (p.addressChanged ? call(client(wrapClient(ctx, p.clientId))) : undefined));
      return;
    case 'clientConnectionInfoUpdate':
      on('clientInfo', (p) => [client(p.client)]);
      return;
    case 'serverGroupAdded':
    case 'serverGroupRemoved':
      on(name, (p) => [{ client: client(p.client), invoker: undefined, serverGroup: wrapGroup(ctx, bot, 'server', p.group?.id) }]);
      return;
    case 'channelCreate':
    case 'channelUpdate':
      on(name, (p) => [channel(p.channel), client(p.invoker)]);
      return;
    case 'channelDelete':
      on('channelDelete', (p) => [channel(wrapChannel(ctx, p.channel)), client(p.invoker)]);
      return;
    case 'track':
      on('trackStart', (p) => [sTrack(ctx, bot, p.item.track)]);
      return;
    case 'trackEnd':
      on('trackEnd', (p) => [sTrack(ctx, bot, p.item.track), p.error ? 'error' : '']);
      return;
    case 'trackInfo':
      // Fires when a stream title changes; James reports stream titles with the track.
      return;
    case 'ytdl.success':
      ctx.on('compat:ytdl.success', (p) => call(p));
      return;
    case 'ytdl.error':
      ctx.on('compat:ytdl.error', (p: any) => call(p.ev, p.message));
      return;
    case 'ws.connect':
      on('ws.connect', (p) => [p.connection]);
      return;
    case 'ws.disconnect':
    case 'ws.close':
      on('ws.close', (p) => [p.connection]);
      return;
    case 'ws.data':
      on('ws.message', (p) => [p.connection, 1, new Bytes(Buffer.from(p.data ?? ''))]);
      return;
    case 'ws.error':
      return;
    case 'load':
      ctx.on('compat:load', () => call());
      return;
    case 'unload':
      ctx.on('unload', () => call());
      return;
    case 'talkerCount':
    case 'typing':
    case 'speech':
    case 'serverinfo_int':
    case 'message':
      unsupported(ctx, `the '${name}' event`);
      return;
    default:
      if (name.startsWith('discord:')) {
        unsupported(ctx, 'Discord events');
        return;
      }
      // Custom events between scripts (event.emit / event.broadcast).
      bot.on(`custom:${name}` as any, (p: any) => call(p.data));
  }
}

function apiEvent(ctx: Context, request: any) {
  return {
    name: () => request.name,
    data: () => request.body,
    user: () => (request.user ? sUser(ctx, { id: request.user.id, username: request.user.username, isAdmin: false, identities: [] }) : undefined),
    remoteAddr: () => request.remoteAddr,
    queryParams: () => ({ ...request.query }),
  };
}

// ---- script runner ----------------------------------------------------------------------------

export function setupSinusbot(rt: Runtime, entry: string): void {
  const code = fs.readFileSync(entry, 'utf8');
  const wrapper = new vm.Script(`(function (registerPlugin, require, module, exports, console, setTimeout, setInterval, clearTimeout, clearInterval) {${code}\n})`, { filename: entry });
  const run = wrapper.runInThisContext();
  const nodeRequire = createRequire(entry);

  rt.onContextStart = async (ctx) => {
    const bot = createBot(ctx);
    ctx.on('connect', () => (ctx.extra.everConnected = true));
    if (ctx.state.connected) ctx.extra.everConnected = true;
    const scriptModule = { exports: {} as unknown };
    const mods = modules(ctx, bot, rt, scriptModule, (name, cb) => subscribe(ctx, bot, name, cb));
    let registered = false;
    const registerPlugin = (manifest: Record<string, unknown>, main: Function) => {
      if (registered) throw new Error('registerPlugin() called twice');
      registered = true;
      main?.(undefined, sinusbotConfig(ctx), manifest);
    };
    const req = (name: string) => {
      if (Object.hasOwn(mods, name)) return (mods as Record<string, unknown>)[name];
      // Node.js built-ins and npm packages next to the script (guarded like native plugins).
      return nodeRequire(name);
    };
    const console = {
      log: (...a: unknown[]) => ctx.log('info', ...a.map(plain)),
      info: (...a: unknown[]) => ctx.log('info', ...a.map(plain)),
      debug: (...a: unknown[]) => ctx.log('debug', ...a.map(plain)),
      warn: (...a: unknown[]) => ctx.log('warn', ...a.map(plain)),
      error: (...a: unknown[]) => ctx.log('error', ...a.map(plain)),
    };
    // Timers belong to the context so they stop with it (SinusBot timers return ids).
    const cancels = new Map<number, () => void>();
    let nextTimer = 1;
    const setTimeoutCompat = (fn: Function, ms: number, ...args: unknown[]) => {
      const id = nextTimer++;
      cancels.set(id, ctx.setTimeout(() => {
        cancels.delete(id);
        fn(...args);
      }, ms));
      return id;
    };
    const setIntervalCompat = (fn: Function, ms: number, ...args: unknown[]) => {
      const id = nextTimer++;
      cancels.set(id, ctx.setInterval(() => fn(...args), ms));
      return id;
    };
    const clearCompat = (id: unknown) => {
      cancels.get(Number(id))?.();
      cancels.delete(Number(id));
    };
    run(registerPlugin, req, scriptModule, scriptModule.exports, console, setTimeoutCompat, setIntervalCompat, clearCompat, clearCompat);
    if (!registered) throw new Error('the script did not call registerPlugin()');
    setImmediate(() => ctx.dispatch('compat:load', {}));
  };
}

/** SinusBot passes `select` values as indices and checkboxes as booleans. */
function sinusbotConfig(ctx: Context): Record<string, unknown> {
  return { ...ctx.config };
}
