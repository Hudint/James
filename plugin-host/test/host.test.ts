// End-to-end tests of the plugin host: each test plays James on the other end of fd 3 and
// runs the host with the same Node permission flags James uses.

import assert from 'node:assert/strict';
import { spawn, type ChildProcess } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import type net from 'node:net';
import { after, test } from 'node:test';

import { Connection } from '../src/ipc.ts';

const hostDir = path.resolve(new URL('..', import.meta.url).pathname);
const fixtures = path.join(hostDir, 'test/fixtures/plugins');
const examples = path.resolve(hostDir, '../examples/plugins');
const tmp = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'james-host-')));
after(() => fs.rmSync(tmp, { recursive: true, force: true }));

const alice = { id: '5', uid: 'uid-alice', name: 'Alice', channel: '1', serverGroups: ['8'], isSelf: false, databaseId: '12', channelGroup: '5',
  description: '', phoneticName: '', away: null, inputMuted: false, outputMuted: false, recording: false, talking: false, country: 'DE',
  query: false, talkPower: 0, prioritySpeaker: false, channelCommander: false, info: null, connection: null };
const self = { ...alice, id: '1', uid: 'uid-bot', name: 'James', isSelf: true };
const snapshot = {
  server: { name: 'Test', version: '3', platform: 'Linux', welcomeMessage: '' },
  ownClient: '1',
  clients: [self],
  channels: [{ id: '1', parent: null, name: 'Lobby', order: 0 }],
  serverGroups: [{ id: '8', name: 'Guest', icon: 0 }],
  channelGroups: [],
};

interface Call { instance: string | null; method: string; params: any; done?: boolean }

/** A host process with a fake James. */
class Host {
  readonly calls: Call[] = [];
  readonly logs: { level: string; message: string }[] = [];
  readonly events: { name: string; data: any }[] = [];
  readonly stores = new Map<string, Record<string, unknown>>();
  readonly process: ChildProcess;
  readonly conn: Connection;
  /** Delay of store.set answers (to observe ordering). */
  storeDelay = 0;
  /** Delay of instance.info answers (a context start in progress). */
  infoDelay = 0;
  /** Delay of media.play answers (James resolving a URL). */
  playDelay = 0;

  constructor(flags: string[], env: Record<string, string> = {}, cwd?: string) {
    this.process = spawn(process.execPath, ['--disable-warning=ExperimentalWarning', '--disable-warning=SecurityWarning', ...flags, path.join(hostDir, 'src/main.ts')], {
      cwd,
      stdio: ['ignore', 'inherit', 'inherit', 'pipe'],
      env: { ...process.env, JAMES_IPC_FD: '3', ...env },
    });
    this.conn = new Connection(this.process.stdio[3] as net.Socket, (method, params) => this.#call(method, params), (name, data) => {
      if (name === 'log') this.logs.push(data);
      else this.events.push({ name, data });
    }, () => {});
  }

  /** Runs one plugin process with James' permission flags. */
  static async plugin(dir: string, id: string, permissions: string[], env: Record<string, string> = {}): Promise<Host> {
    const discovery = await Host.discover(dir);
    const manifest = discovery.plugins.find((p: any) => p.id === id);
    assert.ok(manifest, `plugin ${id} not discovered: ${JSON.stringify(discovery.errors)}`);
    const dataDir = path.join(tmp, `data-${id}-${Math.random().toString(36).slice(2)}`);
    fs.mkdirSync(dataDir);
    const readable = [hostDir, dataDir, ...manifest.paths.readable].map((p) => `--allow-fs-read=${p}`);
    const flags = ['--permission', '--allow-worker', ...readable, `--allow-fs-write=${dataDir}`];
    // Like James: the working directory is the plugin's data directory.
    const host = new Host(flags, env, dataDir);
    try {
      await host.conn.request('init', { protocol: 1, version: '0.1.0' });
      await host.conn.request('plugin.load', { id, permissions, manifest, dir: manifest.paths.dir, entry: manifest.paths.entry, dataDir });
    } catch (error) {
      host.kill();
      throw error;
    }
    return host;
  }

  static async discover(dir: string): Promise<any> {
    const host = new Host(['--permission', `--allow-fs-read=${hostDir}`, `--allow-fs-read=${dir}`]);
    try {
      await host.conn.request('init', { protocol: 1, version: '0.1.0' });
      return await host.conn.request('discover', { pluginsDir: dir });
    } finally {
      host.kill();
    }
  }

  async #call(method: string, params: any): Promise<unknown> {
    assert.equal(method, 'call');
    const call: Call = { ...params };
    this.calls.push(call);
    const key = `${params.params?.scope}|${params.instance}`;
    try {
      switch (params.method) {
        case 'instance.info':
          await new Promise((r) => setTimeout(r, this.infoDelay));
          return { id: params.instance, name: 'Music', platform: 'teamspeak', commandPrefix: '!' };
        case 'instance.status':
          return { connected: true, current: null, positionMs: 0, paused: false, volume: 50, repeat: 'off', shuffle: false, queueLength: 0 };
        case 'instance.queue':
          return [];
        case 'instance.snapshot':
          return snapshot;
        case 'store.load':
          return this.stores.get(key) ?? {};
        case 'store.set':
        case 'store.unset':
          await new Promise((r) => setTimeout(r, this.storeDelay));
          return null;
        case 'media.play': {
          await new Promise((r) => setTimeout(r, this.playDelay));
          const input: string | undefined = params.params.input;
          // Like the YouTube resolver: the track's locator is the canonical URL.
          const locator = input?.replace('https://youtu.be/', 'https://www.youtube.com/watch?v=');
          return input ? [{ id: 1, track: { source: 'youtube', locator, title: 'Video', isStream: false } }] : [];
        }
        case 'playlists.list':
          return [{ id: 1, name: 'Chill', trackCount: 0 }, { id: 2, name: 'Rock', trackCount: 3 }];
        case 'client.kick':
          throw new Error("the plugin lacks the permission 'moderation'");
        default:
          return null;
      }
    } finally {
      call.done = true;
    }
  }

  find(method: string): Call | undefined {
    return this.calls.find((c) => c.method === method);
  }

  wait<T>(what: string, check: () => T | undefined | false, ms = 5000): Promise<T> {
    return new Promise((resolve, reject) => {
      const started = Date.now();
      const tick = () => {
        const value = check();
        if (value !== undefined && value !== false) return resolve(value as T);
        if (Date.now() - started > ms) return reject(new Error(`timed out waiting for ${what}; logs: ${JSON.stringify(this.logs)}`));
        setTimeout(tick, 10);
      };
      tick();
    });
  }

  event(type: string, event: object, instance = 'music'): void {
    this.conn.notify('instance.event', { instance, type, ...event });
  }

  kill(): void {
    this.process.kill();
  }
}

function plugin(dir: string, id: string, files: Record<string, string>): string {
  const root = path.join(dir, id);
  fs.mkdirSync(root, { recursive: true });
  for (const [name, content] of Object.entries(files)) fs.writeFileSync(path.join(root, name), content);
  return root;
}

test('discovers native plugins and SinusBot scripts', async () => {
  const result = await Host.discover(fixtures);
  const byId = Object.fromEntries(result.plugins.map((p: any) => [p.id, p]));
  assert.deepEqual(result.errors, []);
  assert.equal(byId.greeter.kind, 'james');
  assert.deepEqual(byId.greeter.permissions, ['web']);
  assert.deepEqual(byId.greeter.paths.readable, [path.join(fixtures, 'greeter')]);
  assert.equal(byId.legacy.kind, 'sinusbot');
  assert.deepEqual(byId.legacy.permissions, ['http']);
  // SinusBot has no permissions for these, so scripts offer them all as optional.
  assert.deepEqual(byId.legacy.optionalPermissions, ['moderation', 'channels', 'playlists']);
  assert.deepEqual(byId.greeter.optionalPermissions, []);
  assert.deepEqual(byId.legacy.platforms, ['teamspeak']);
  assert.equal(byId.legacy.config[0].name, 'prefix');
});

test('SinusBot scripts can require James permissions', async () => {
  const dir = path.join(tmp, 'sb-permissions');
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(path.join(dir, 'mover.js'), "registerPlugin({ name: 'Mover', requiredModules: ['http'], permissions: ['moderation', 'bogus'] }, function () {});");
  const result = await Host.discover(dir);
  const mover = result.plugins.find((p: any) => p.id === 'mover');
  assert.deepEqual([...mover.permissions].sort(), ['bogus', 'http', 'moderation']);
  assert.deepEqual(mover.optionalPermissions, ['channels', 'playlists']);
});

test('discovery rejects duplicate ids and scripts escaping the probe', async () => {
  const dir = path.join(tmp, 'dupes');
  plugin(dir, 'twin', { 'package.json': JSON.stringify({ name: 'twin', main: 'index.js', james: {} }), 'index.js': '' });
  fs.writeFileSync(path.join(dir, 'twin.js'), "registerPlugin({ name: 'Twin' }, function () {});");
  fs.writeFileSync(path.join(dir, 'escape.js'), "registerPlugin.constructor('return process')().exit(1);");
  fs.writeFileSync(path.join(dir, 'escape2.js'), "this.constructor.constructor('return process')().exit(1);");
  fs.writeFileSync(path.join(dir, 'getter.js'), "throw { get message() { for (;;) {} } };");
  fs.writeFileSync(path.join(dir, 'proxy.js'), "throw new Proxy({}, { get: function () { for (;;) {} }, getPrototypeOf: function () { for (;;) {} } });");
  fs.writeFileSync(path.join(dir, 'spin.js'), 'Promise.resolve().then(function loop() { return Promise.resolve().then(loop); }); registerPlugin({ name: "Spin" }, function () {});');
  const result = await Host.discover(dir);
  assert.deepEqual(result.plugins, []);
  const errors = Object.fromEntries(result.errors.map((e: any) => [e.id, e.error]));
  assert.match(errors.twin, /two plugins/);
  assert.ok(errors.escape, 'escape attempt fails');
  assert.ok(errors.escape2, 'escape through the global object fails');
  assert.equal(errors.getter, 'the script failed while being read');
  assert.equal(errors.proxy, 'the script failed while being read');
  assert.ok(errors.spin, 'microtask loop is stopped');
});

test('native plugin: events, commands, store, web', async () => {
  const host = await Host.plugin(fixtures, 'greeter', ['web']);
  try {
    await host.conn.request('context.start', { instance: 'music', platform: 'teamspeak', config: {} });
    const register = await host.wait('command registration', () => host.find('commands.register'));
    assert.equal(register.params.name, 'hello');
    assert.deepEqual(host.find('web.register')!.params, { name: 'ping', public: false, permission: null });

    host.event('platform', { event: { type: 'clientEnter', client: alice } });
    const greeting = await host.wait('greeting', () => host.find('chat.send'));
    assert.deepEqual(greeting.params, { target: 'client', client: '5', text: 'Welcome, Alice!' });

    host.event('platform', { event: { type: 'clientUpdate', client: { ...alice, name: 'Alicia' } } });
    const nick = await host.wait('nick announcement', () => host.calls.find((c) => c.method === 'chat.send' && c.params.target === 'server'));
    assert.equal(nick.params.text, 'Alice is now Alicia');

    const reply = await host.conn.request('command.run', { instance: 'music', name: 'hi', args: 'a "b c"', mode: 'client', invoker: alice });
    assert.equal(reply, 'Hello Alicia (1) a|b c');
    await host.wait('store write', () => host.find('store.set'));
    assert.deepEqual(host.find('store.set')!.params, { scope: 'instance', key: 'count', value: 1 });

    const pong = await host.conn.request('http.event', { instance: 'music', request: { name: 'ping', public: false, body: 42 } });
    assert.deepEqual(pong, { pong: 42, clients: 2 });

    // Custom events from other plugins (routed by James).
    host.conn.notify('plugin.event', { instance: 'music', name: 'hello', data: { x: 1 }, from: 'other' });
    await host.wait('custom event', () => host.logs.find((l) => l.message.startsWith('custom event')));

    await host.conn.request('context.stop', { instance: 'music' });
    await host.wait('cleanup log', () => host.logs.find((l) => l.message === 'stopped'));
    await host.wait('command unregistration', () => host.find('commands.unregister'));
  } finally {
    host.kill();
  }
});

test('commands registered by several instances stay until the last one stops', async () => {
  const host = await Host.plugin(fixtures, 'greeter', ['web']);
  try {
    await host.conn.request('context.start', { instance: 'a', platform: 'teamspeak', config: {} });
    await host.conn.request('context.start', { instance: 'b', platform: 'teamspeak', config: {} });
    await host.wait('registration', () => host.find('commands.register'));
    assert.equal(host.calls.filter((c) => c.method === 'commands.register' && c.params.name === 'hello').length, 1, 'registered once');
    await host.conn.request('context.stop', { instance: 'a' });
    await new Promise((r) => setTimeout(r, 100));
    assert.equal(host.find('commands.unregister'), undefined, 'still used by b');
    await host.conn.request('context.stop', { instance: 'b' });
    await host.wait('unregistration', () => host.find('commands.unregister'));
  } finally {
    host.kill();
  }
});

test('SinusBot script: compat modules, sync calls, events', async () => {
  const host = await Host.plugin(fixtures, 'legacy', ['http']);
  try {
    await host.conn.request('context.start', { instance: 'music', platform: 'teamspeak', config: { prefix: 'echo' } });
    await host.wait('load event', () => host.logs.find((l) => l.message.startsWith('loaded on music')));
    assert.equal(host.logs.find((l) => l.message.startsWith('loaded on'))!.message, 'loaded on music backend=ts3 clients=1');
    await host.wait('timer', () => host.logs.find((l) => l.message === 'timer fired'));

    host.event('platform', { event: { type: 'clientEnter', client: alice } });
    await host.wait('clientMove on join', () => host.logs.find((l) => l.message === 'joined Alice into Lobby'));

    host.event('platform', { event: { type: 'message', mode: 'client', from: alice, text: 'hi' } });
    const echo = await host.wait('echo', () => host.find('chat.send'));
    assert.equal(echo.params.text, 'echo hi 9dd4e461268c8034f5c8564e155c67a6 mode=1');

    host.event('platform', { event: { type: 'message', mode: 'client', from: alice, text: 'playlists' } });
    const list = await host.wait('sync playlists', () => host.calls.find((c) => c.method === 'chat.send' && c.params.text.startsWith('playlists')));
    assert.equal(list.params.text, 'playlists: Chill,Rock');
    await host.wait('plugin store write', () => host.find('store.set'));
    assert.equal(host.find('store.set')!.params.scope, 'plugin');

    // Scripts store URLs where SinusBot stored song ids.
    host.event('platform', { event: { type: 'message', mode: 'client', from: alice, text: 'track https://youtu.be/abc' } });
    const url = await host.wait('URL track', () => host.calls.find((c) => c.method === 'chat.send' && c.params.text.startsWith('track https')));
    assert.equal(url.params.text, 'track https://youtu.be/abc url');
    assert.deepEqual(host.find('media.play')!.params, { input: 'https://youtu.be/abc', mode: 'now' });
    host.event('platform', { event: { type: 'message', mode: 'client', from: alice, text: 'track local:/music/a.mp3' } });
    const id = await host.wait('track id', () => host.calls.find((c) => c.method === 'chat.send' && c.params.text.startsWith('track local')));
    assert.equal(id.params.text, 'track local:/music/a.mp3 local');
    const played = host.calls.filter((c) => c.method === 'media.play')[1];
    assert.deepEqual(played.params.tracks[0], { source: 'local', locator: '/music/a.mp3', title: 'a.mp3', artist: null, album: null, durationMs: null, thumbnail: null, isStream: false });
    // Other sources use URLs as ids.
    host.event('platform', { event: { type: 'message', mode: 'client', from: alice, text: 'track ytdlp:abc' } });
    await host.wait('unknown source', () => host.calls.find((c) => c.method === 'chat.send' && c.params.text === 'no track'));
    // A track counts as playing while James resolves it, and keeps the URL the script used as id.
    host.playDelay = 300;
    host.event('platform', { event: { type: 'message', mode: 'client', from: alice, text: 'track https://youtu.be/def' } });
    host.event('platform', { event: { type: 'message', mode: 'client', from: alice, text: 'id https://example.com/x' } });
    const pending = await host.wait('pending play', () => host.calls.find((c) => c.method === 'chat.send' && c.params.text.startsWith('id https://example')));
    assert.equal(pending.params.text, 'id https://example.com/x playing=true');
    await host.wait('play answered', () => host.calls.filter((c) => c.method === 'media.play' && c.done).length === 3);
    host.event('platform', { event: { type: 'message', mode: 'client', from: alice, text: 'id https://www.youtube.com/watch?v=def' } });
    const alias = await host.wait('alias', () => host.calls.find((c) => c.method === 'chat.send' && c.params.text.startsWith('id https://youtu.be/def')));
    // Still "playing" shortly after the answer (the status event may come later).
    assert.equal(alias.params.text, 'id https://youtu.be/def playing=true');
    host.event('platform', { event: { type: 'message', mode: 'client', from: alice, text: 'track nonsense' } });
    await host.wait('unknown id', () => host.calls.filter((c) => c.method === 'chat.send' && c.params.text === 'no track').length === 2);

    // Connection data is requested on first use (once, not per getter call).
    host.event('platform', { event: { type: 'message', mode: 'client', from: alice, text: 'ip' } });
    const ip = await host.wait('ip', () => host.calls.find((c) => c.method === 'chat.send' && c.params.text.startsWith('ip=')));
    assert.equal(ip.params.text, 'ip= again=');
    assert.deepEqual(host.calls.filter((c) => c.method === 'client.requestInfo').map((c) => c.params), [{ client: '5' }]);

    host.event('platform', { event: { type: 'message', mode: 'client', from: alice, text: 'say good morning' } });
    const say = await host.wait('audio.say', () => host.find('tts.say'));
    assert.deepEqual(say.params, { text: 'good morning', voice: null, language: 'en' });
  } finally {
    host.kill();
  }
});

test('store writes reach James in order', async () => {
  const dir = path.join(tmp, 'store');
  plugin(dir, 'writer', {
    'package.json': JSON.stringify({ name: 'writer', type: 'module', main: 'index.js', james: {} }),
    'index.js': `export default { setup(bot) {
      bot.store.plugin.set('k', 1); bot.store.plugin.delete('k'); bot.store.plugin.set('j', 2);
      let error = '';
      try { bot.store.plugin.set('big', 'x'.repeat(1 << 21)); } catch (e) { error = e.message; }
      bot.log.info('limit: ' + error);
    } };`,
  });
  const host = await Host.plugin(dir, 'writer', []);
  host.storeDelay = 30;
  try {
    await host.conn.request('context.start', { instance: 'music', platform: 'teamspeak', config: {} });
    await host.wait('three writes', () => host.calls.filter((c) => c.method.startsWith('store.') && c.method !== 'store.load').length === 3 && host.calls.every((c) => c.done));
    const writes = host.calls.filter((c) => c.method === 'store.set' || c.method === 'store.unset').map((c) => `${c.method}:${c.params.key}`);
    assert.deepEqual(writes, ['store.set:k', 'store.unset:k', 'store.set:j']);
    assert.match(host.logs.find((l) => l.message.startsWith('limit:'))!.message, /at most 1 MiB/);
  } finally {
    host.kill();
  }
});

test('plugins cannot read files outside their directories', async () => {
  const dir = path.join(tmp, 'fs');
  plugin(dir, 'snoop', {
    'package.json': JSON.stringify({ name: 'snoop', type: 'module', main: 'index.js', james: {} }),
    'index.js': `import fs from 'node:fs';
      export default { setup(bot) {
        fs.writeFileSync(bot.dataDir + '/own.txt', 'ok');
        bot.log.info('own: ' + fs.readFileSync(bot.dataDir + '/own.txt', 'utf8'));
        try { fs.readFileSync('${hostDir}/../Cargo.toml'); bot.log.info('outside: read'); } catch (e) { bot.log.info('outside: ' + e.code); }
        try { fs.readdirSync(process.cwd() + '/..'); bot.log.info('parent: read'); } catch (e) { bot.log.info('parent: ' + e.code); }
      } };`,
  });
  const host = await Host.plugin(dir, 'snoop', []);
  try {
    await host.conn.request('context.start', { instance: 'music', platform: 'teamspeak', config: {} });
    await host.wait('own file', () => host.logs.find((l) => l.message === 'own: ok'));
    await host.wait('outside', () => host.logs.find((l) => l.message.startsWith('outside:')));
    assert.equal(host.logs.find((l) => l.message.startsWith('outside:'))!.message, 'outside: ERR_ACCESS_DENIED');
    await host.wait('parent', () => host.logs.find((l) => l.message.startsWith('parent:')));
    assert.equal(host.logs.find((l) => l.message.startsWith('parent:'))!.message, 'parent: ERR_ACCESS_DENIED');
  } finally {
    host.kill();
  }
});

test('a context stopped while starting never runs setup', async () => {
  const dir = path.join(tmp, 'cancel');
  plugin(dir, 'slow', {
    'package.json': JSON.stringify({ name: 'slow', type: 'module', main: 'index.js', james: {} }),
    'index.js': "export default { setup(bot) { bot.log.info('setup ran'); } };",
  });
  const host = await Host.plugin(dir, 'slow', []);
  host.infoDelay = 300;
  try {
    const start = host.conn.request('context.start', { instance: 'music', platform: 'teamspeak', config: {} });
    await host.conn.request('context.stop', { instance: 'music' });
    await assert.rejects(start, /stopped while starting/);
    await new Promise((r) => setTimeout(r, 300));
    assert.equal(host.logs.find((l) => l.message === 'setup ran'), undefined);
    // A later start works normally.
    host.infoDelay = 0;
    await host.conn.request('context.start', { instance: 'music', platform: 'teamspeak', config: {} });
    await host.wait('setup', () => host.logs.find((l) => l.message === 'setup ran'));
  } finally {
    host.kill();
  }
});

test('setups that hang restart the worker once, without failing the plugin', async () => {
  const dir = path.join(tmp, 'hang');
  plugin(dir, 'hangs', {
    'package.json': JSON.stringify({ name: 'hangs', type: 'module', main: 'index.js', james: {} }),
    'index.js': `let first = true;
      export default { setup(bot) { if (first) { first = false; const end = Date.now() + 1500; while (Date.now() < end) {} } bot.log.info('setup ' + bot.instance.id); } };`,
  });
  const host = await Host.plugin(dir, 'hangs', [], { JAMES_FAST_RESTART: '1' });
  try {
    const starts = ['a', 'b', 'c', 'd'].map((instance) =>
      host.conn.request('context.start', { instance, platform: 'teamspeak', config: {} }).catch(() => 'failed'),
    );
    await Promise.all(starts);
    await host.wait('restart', () => host.logs.find((l) => l.message === 'restarted after a crash'), 10_000);
    await new Promise((r) => setTimeout(r, 500));
    assert.equal(host.events.find((e) => e.name === 'plugin.failed'), undefined, 'not failed');
    assert.equal(host.logs.filter((l) => l.message === 'restarted after a crash').length, 1, 'one restart');
  } finally {
    host.kill();
  }
});

test('guard blocks modules without permission', async () => {
  const dir = path.join(tmp, 'guard');
  plugin(dir, 'netty', {
    'package.json': JSON.stringify({ name: 'netty', type: 'module', main: 'index.js', james: {} }),
    'index.js': "import net from 'node:net';\nexport default { setup() {} };\n",
  });
  await assert.rejects(Host.plugin(dir, 'netty', []), /permission 'net'/);
});

test('a plugin whose worker keeps crashing is reported as failed', async () => {
  const dir = path.join(tmp, 'crash');
  plugin(dir, 'crashy', {
    'package.json': JSON.stringify({ name: 'crashy', type: 'module', main: 'index.js', james: {} }),
    'index.js': 'export default { setup() { setTimeout(() => process.exit(3), 20); } };',
  });
  const host = await Host.plugin(dir, 'crashy', [], { JAMES_FAST_RESTART: '1' });
  try {
    await host.conn.request('context.start', { instance: 'music', platform: 'teamspeak', config: {} });
    const failed = await host.wait('plugin.failed', () => host.events.find((e) => e.name === 'plugin.failed'), 10_000);
    assert.match(failed.data.error, /crashed repeatedly/);
  } finally {
    host.kill();
  }
});

test('the bundled examples load', async () => {
  const result = await Host.discover(examples);
  assert.deepEqual(result.errors, []);
  assert.deepEqual(result.plugins.map((p: any) => p.id).sort(), ['afk-mover', 'now-playing', 'sinusbot-greeter', 'welcome']);
  for (const p of result.plugins) {
    const host = await Host.plugin(examples, p.id, p.permissions);
    try {
      await host.conn.request('context.start', { instance: 'music', platform: 'teamspeak', config: { channel: '1' } });
      await host.conn.request('plugin.unload', { id: p.id });
    } finally {
      host.kill();
    }
  }
});
