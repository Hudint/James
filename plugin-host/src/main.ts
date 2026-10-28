// Plugin host process. James starts one process per plugin (and a short-lived one for
// discovery) and talks to it over file descriptor 3. The plugin runs in worker threads:
// one shared worker for native plugins, one worker per instance for SinusBot scripts (they
// keep per-instance globals).

import { MessageChannel, Worker } from 'node:worker_threads';

import { Connection, PROTOCOL_VERSION, errorMessage } from './ipc.ts';
import { discover, type Discovered, type Manifest } from './discover.ts';
import type { ContextInit, FromWorker, ToWorker } from './protocol.ts';

const WORKER_ENTRY = new URL('./worker/entry.ts', import.meta.url);
/** Worker crashes within RESTART_WINDOW after which the plugin is reported as failed. */
const MAX_RESTARTS = 3;
const RESTART_WINDOW = 5 * 60_000;
// Tests shorten the delays.
const RESTART_DELAYS = process.env.JAMES_FAST_RESTART ? [10, 10, 10] : [1_000, 5_000, 15_000];
const SYNC_TIMEOUT = 15_000;
const CONTEXT_TIMEOUT = process.env.JAMES_FAST_RESTART ? 500 : 25_000;
const WORKER_MEMORY_MB = 256;
/** Messages kept for a context that is still starting. */
const MAX_PENDING = 1000;

class SetupTimeout extends Error {
  constructor() {
    super('setup did not finish in time');
  }
}

type Scope = 'plugin' | 'instance' | 'global';
type ContextBase = Omit<ContextInit, 'store' | 'snapshot' | 'status' | 'queue' | 'info'>;

let version = '';

// ---- store cache (authoritative copy for this plugin's workers) -----------------------------

const store = new Map<string, Map<string, unknown>>();
/** Store writes are sent one after another, so they reach the database in order. */
let storeQueue: Promise<void> = Promise.resolve();

function scopeKey(scope: Scope, instance: string | null): string {
  return scope === 'instance' ? `instance:${instance}` : scope;
}

async function loadScope(scope: Scope, instance: string | null): Promise<Record<string, unknown>> {
  const key = scopeKey(scope, instance);
  let entries = store.get(key);
  if (!entries) {
    const loaded = await james.request('call', { instance, method: 'store.load', params: { scope } });
    entries = new Map(Object.entries(loaded ?? {}));
    store.set(key, entries);
  }
  return Object.fromEntries(entries);
}

function storeChanged(from: PluginWorker | null, message: Extract<FromWorker, { type: 'store' }>): void {
  const { scope, instance, key, value, deleted } = message;
  const entries = store.get(scopeKey(scope, instance));
  if (deleted) entries?.delete(key);
  else entries?.set(key, value);
  // Other workers sharing the scope (SinusBot: one per instance) see the change.
  if (scope !== 'instance' && plugin) {
    for (const w of plugin.workers()) {
      if (w !== from) w.post({ type: 'store', scope, key, value, deleted });
    }
  }
}

function persistStore(from: PluginWorker, message: Extract<FromWorker, { type: 'store' }>): void {
  storeChanged(from, message);
  const { scope, instance, key, value, deleted } = message;
  storeQueue = storeQueue.then(async () => {
    try {
      const method = deleted ? 'store.unset' : 'store.set';
      await james.request('call', { instance, method, params: { scope, key, value } });
    } catch (error) {
      log(instance, 'warn', `store: ${errorMessage(error)}`);
    }
  });
}

// ---- workers ------------------------------------------------------------------------------

/** One worker thread; `instance` is set for per-instance (SinusBot) workers. */
class PluginWorker {
  readonly plugin: LoadedPlugin;
  readonly instance: string | null;
  worker: Worker | null = null;
  #requests = new Map<number, { resolve: (v: unknown) => void; reject: (e: Error) => void }>();
  #starts = new Map<string, { resolve: () => void; reject: (e: Error) => void }>();
  #stops = new Map<string, (() => void)[]>();
  #nextId = 1;
  #syncState: Int32Array | null = null;
  #syncPort: MessagePort | null = null;
  #stopping = false;

  constructor(plugin: LoadedPlugin, instance: string | null) {
    this.plugin = plugin;
    this.instance = instance;
  }

  start(): Promise<void> {
    const { discovered: d, permissions } = this.plugin;
    const buffer = new SharedArrayBuffer(4);
    const channel = new MessageChannel();
    this.#syncState = new Int32Array(buffer);
    this.#syncPort = channel.port1 as unknown as MessagePort;
    this.#stopping = false;
    const worker = new Worker(WORKER_ENTRY, {
      name: `plugin:${d.manifest.id}${this.instance ? `@${this.instance}` : ''}`,
      resourceLimits: { maxOldGenerationSizeMb: WORKER_MEMORY_MB },
      stdout: false,
      stderr: false,
    });
    this.worker = worker;
    const ready = new Promise<void>((resolve, reject) => {
      const onMessage = (message: FromWorker) => {
        if (message.type === 'ready') {
          worker.off('message', onMessage);
          resolve();
        } else if (message.type === 'loadFailed') {
          worker.off('message', onMessage);
          reject(new Error(message.error.split('\n')[0]));
        }
      };
      worker.on('message', onMessage);
      worker.once('exit', (code) => reject(new Error(`the plugin exited while loading (code ${code})`)));
      worker.once('error', (error) => reject(error));
    });
    worker.on('message', (message: FromWorker) => this.#onMessage(message));
    worker.on('error', (error) => log(this.instance, 'error', `crashed: ${error?.stack ?? error}`));
    worker.on('exit', (code) => this.#onExit(worker, code));
    const init: ToWorker = {
      type: 'init',
      manifest: d.manifest,
      dir: d.dir,
      entry: d.entry,
      permissions,
      dataDir: this.plugin.dataDir,
      version,
      syncBuffer: buffer,
      syncPort: channel.port2 as unknown as MessagePort,
    };
    worker.postMessage(init, [channel.port2 as any]);
    return ready.catch(async (error) => {
      await this.terminate();
      throw error;
    });
  }

  post(message: ToWorker): boolean {
    if (!this.worker) return false;
    this.worker.postMessage(message);
    return true;
  }

  request(kind: 'command' | 'http', instance: string, payload: unknown, timeoutMs = 30_000): Promise<unknown> {
    const id = this.#nextId++;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.#requests.delete(id);
        reject(new Error('the plugin did not answer in time'));
      }, timeoutMs);
      this.#requests.set(id, {
        resolve: (v) => (clearTimeout(timer), resolve(v)),
        reject: (e) => (clearTimeout(timer), reject(e)),
      });
      if (!this.post({ type: 'request', id, kind, instance, payload })) {
        this.#requests.get(id)?.reject(new Error('the plugin is restarting'));
        this.#requests.delete(id);
      }
    });
  }

  startContext(context: ContextInit): Promise<void> {
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.#starts.delete(context.instance);
        reject(new SetupTimeout());
      }, CONTEXT_TIMEOUT);
      this.#starts.set(context.instance, {
        resolve: () => (clearTimeout(timer), resolve()),
        reject: (e) => (clearTimeout(timer), reject(e)),
      });
      if (!this.post({ type: 'context.start', context })) {
        this.#starts.get(context.instance)?.reject(new Error('the plugin is restarting'));
        this.#starts.delete(context.instance);
      }
    });
  }

  /** Stops a context; waits (bounded) for its cleanup handlers. */
  stopContext(instance: string): Promise<void> {
    return new Promise((resolve) => {
      const timer = setTimeout(done, 5_000);
      function done() {
        clearTimeout(timer);
        resolve();
      }
      this.#stops.set(instance, [...(this.#stops.get(instance) ?? []), done]);
      if (!this.post({ type: 'context.stop', instance })) done();
    });
  }

  async terminate(): Promise<void> {
    this.#stopping = true;
    const worker = this.worker;
    this.worker = null;
    await worker?.terminate();
  }

  #onMessage(message: FromWorker): void {
    switch (message.type) {
      case 'started': {
        const start = this.#starts.get(message.instance);
        this.#starts.delete(message.instance);
        if (message.error) start?.reject(new Error(message.error));
        else start?.resolve();
        return;
      }
      case 'stopped':
        for (const done of this.#stops.get(message.instance) ?? []) done();
        this.#stops.delete(message.instance);
        return;
      case 'call':
        this.#call(message);
        return;
      case 'reply': {
        const request = this.#requests.get(message.id);
        this.#requests.delete(message.id);
        if (message.error !== undefined) request?.reject(new Error(message.error));
        else request?.resolve(message.result);
        return;
      }
      case 'log':
        log(message.instance, message.level, message.message);
        return;
      case 'ws.send':
        james.notify('ws.send', { instance: message.instance, connection: message.connection, data: message.data, close: message.close ?? false });
        return;
      case 'store':
        persistStore(this, message);
        return;
      case 'emit':
        james.notify('emit', { instance: message.instance, name: message.name, data: message.data, broadcast: message.broadcast });
        return;
    }
  }

  #call(message: Extract<FromWorker, { type: 'call' }>): void {
    const { id, instance, method, params, sync } = message;
    if ((method === 'commands.register' || method === 'commands.unregister') && instance) {
      this.plugin.command(method, instance, params).then(
        (result) => this.#answer(id, sync, { result }),
        (error) => this.#answer(id, sync, { error: errorMessage(error) }),
      );
      return;
    }
    james.request('call', { instance, method, params }, sync ? SYNC_TIMEOUT : 60_000).then(
      (result) => this.#answer(id, sync, { result }),
      (error) => this.#answer(id, sync, { error: errorMessage(error) }),
    );
  }

  #answer(id: number, sync: boolean | undefined, answer: { result?: unknown; error?: string }): void {
    if (sync && this.#syncPort && this.#syncState) {
      this.#syncPort.postMessage({ id, ...answer });
      Atomics.store(this.#syncState, 0, 1);
      Atomics.notify(this.#syncState, 0);
    } else {
      this.post({ type: 'reply', id, ...answer });
    }
  }

  #onExit(worker: Worker, code: number): void {
    if (this.worker === worker) this.worker = null;
    for (const r of this.#requests.values()) r.reject(new Error('the plugin stopped'));
    this.#requests.clear();
    for (const s of this.#starts.values()) s.reject(new Error('the plugin stopped'));
    this.#starts.clear();
    for (const list of this.#stops.values()) for (const s of list) s();
    this.#stops.clear();
    if (this.#stopping) return;
    log(this.instance, 'error', `worker exited unexpectedly (code ${code})`);
    void this.plugin.restart(this);
  }
}

class LoadedPlugin {
  readonly id: string;
  readonly discovered: Discovered;
  readonly permissions: string[];
  readonly dataDir: string;
  /** Contexts by instance, including ones that are still starting. */
  readonly contexts = new Map<string, ContextBase>();
  /** Messages for contexts whose start was not sent to the worker yet. */
  readonly #pending = new Map<string, ToWorker[]>();
  shared: PluginWorker | null = null;
  readonly perInstance = new Map<string, PluginWorker>();
  #crashes: number[] = [];
  #failed = false;
  /** Restarts in progress, one per worker. */
  readonly #restarting = new Map<PluginWorker, Promise<void>>();
  /** Command name → instances whose context registered it. */
  readonly #commandOwners = new Map<string, Set<string>>();
  /** Command registrations are handled one after another (no interleaving). */
  #commandQueue: Promise<unknown> = Promise.resolve();

  constructor(discovered: Discovered, permissions: string[], dataDir: string) {
    this.id = discovered.manifest.id;
    this.discovered = discovered;
    this.permissions = permissions;
    this.dataDir = dataDir;
  }

  get sinusbot(): boolean {
    return this.discovered.manifest.kind === 'sinusbot';
  }

  async load(): Promise<void> {
    if (this.sinusbot) return; // workers are created per instance
    this.shared = new PluginWorker(this, null);
    await this.shared.start();
  }

  /** Delivers a message to the context on `instance` (buffered while it starts). */
  deliver(instance: string, message: ToWorker): void {
    const pending = this.#pending.get(instance);
    if (!pending) this.workerFor(instance)?.post(message);
    else if (pending.length < MAX_PENDING) pending.push(message);
  }

  workerFor(instance: string): PluginWorker | undefined {
    return this.sinusbot ? this.perInstance.get(instance) : (this.shared ?? undefined);
  }

  workers(): PluginWorker[] {
    return this.sinusbot ? [...this.perInstance.values()] : this.shared ? [this.shared] : [];
  }

  async startContext(base: ContextBase): Promise<void> {
    if (this.contexts.has(base.instance)) return;
    const { instance } = base;
    this.contexts.set(instance, base);
    this.#pending.set(instance, []);
    // `context.stop` may arrive while this start waits: it replaces the entry.
    const cancelled = () => this.contexts.get(instance) !== base;
    let worker: PluginWorker | undefined;
    try {
      const context = await initialState(base);
      if (cancelled()) throw new Error('the context was stopped while starting');
      worker = this.workerFor(instance);
      if (!worker) {
        if (!this.sinusbot) throw new Error('the plugin is restarting');
        worker = new PluginWorker(this, instance);
        this.perInstance.set(instance, worker);
        await worker.start();
        if (cancelled()) throw new Error('the context was stopped while starting');
      }
      const started = worker.startContext(context);
      // Events that arrived meanwhile follow the start message (the worker keeps the order).
      for (const message of this.#pending.get(instance) ?? []) worker.post(message);
      this.#pending.delete(instance);
      try {
        await started;
      } catch (error) {
        if (error instanceof SetupTimeout && !this.sinusbot) {
          // A hung setup blocks the shared worker for every instance: start it afresh.
          void this.restart(worker, true);
        }
        throw error;
      }
      if (cancelled()) {
        // Stopped while setup ran: undo the start.
        await worker.stopContext(instance);
        throw new Error('the context was stopped while starting');
      }
    } catch (error) {
      if (this.contexts.get(instance) === base) {
        this.#pending.delete(instance);
        this.contexts.delete(instance);
        this.#releaseCommands([instance]);
      }
      if (this.sinusbot && worker && this.perInstance.get(instance) === worker && !this.contexts.has(instance)) {
        // A script that failed to start leaves a dirty worker behind.
        this.perInstance.delete(instance);
        await worker.terminate();
      }
      throw error;
    }
  }

  async stopContext(instance: string): Promise<void> {
    const worker = this.workerFor(instance);
    const starting = this.#pending.has(instance);
    if (!this.contexts.delete(instance)) return;
    this.#pending.delete(instance);
    this.#releaseCommands([instance]);
    // A start in progress notices the removal and cleans up after itself.
    if (starting) return;
    if (!worker) return;
    await worker.stopContext(instance);
    if (this.sinusbot) {
      this.perInstance.delete(instance);
      await worker.terminate();
    }
  }

  async unload(): Promise<void> {
    await Promise.all([...this.contexts.keys()].map((i) => this.stopContext(i).catch(() => {})));
    await Promise.all(this.workers().map((w) => w.terminate()));
    this.shared = null;
    this.perInstance.clear();
  }

  /** Commands are global in James but registered per context: counts registrations. */
  command(method: string, instance: string, params: any): Promise<unknown> {
    const name = String(params?.name ?? '').toLowerCase();
    const run = async () => {
      const users = this.#commandOwners.get(name) ?? new Set<string>();
      if (method === 'commands.register') {
        if (users.size === 0) await james.request('call', { instance, method, params });
        users.add(instance);
        this.#commandOwners.set(name, users);
      } else if (users.delete(instance) && users.size === 0) {
        this.#commandOwners.delete(name);
        await james.request('call', { instance, method, params: { name } });
      }
      return null;
    };
    const result = this.#commandQueue.then(run);
    this.#commandQueue = result.catch(() => {});
    return result;
  }

  /** Forgets command registrations of contexts that are gone. */
  #releaseCommands(instances: string[]): void {
    for (const [name, users] of [...this.#commandOwners]) {
      for (const instance of instances) {
        if (users.has(instance)) void this.command('commands.unregister', instance, { name }).catch(() => {});
      }
    }
  }

  /** Restarts a crashed worker and its contexts; reports the plugin as failed when it
   * crashes too often (also when restarting itself keeps failing). */
  restart(worker: PluginWorker, hung = false): Promise<void> {
    // Several hung setups or crash reports for one worker trigger one restart.
    let running = this.#restarting.get(worker);
    if (!running) {
      running = this.#restart(worker, hung).finally(() => this.#restarting.delete(worker));
      this.#restarting.set(worker, running);
    }
    return running;
  }

  /** The worker is still the one serving its instances (not stopped or replaced). */
  #current(worker: PluginWorker): boolean {
    return worker.instance ? this.perInstance.get(worker.instance) === worker : this.shared === worker;
  }

  async #restart(worker: PluginWorker, hung: boolean): Promise<void> {
    if (hung) await worker.terminate();
    const instances = worker.instance ? [worker.instance] : [...this.contexts.keys()];
    // The crashed contexts register their commands again when they start.
    this.#releaseCommands(instances);
    for (;;) {
      const now = Date.now();
      this.#crashes = this.#crashes.filter((t) => now - t < RESTART_WINDOW);
      if (this.#failed || this.#crashes.length >= MAX_RESTARTS) {
        this.#fail('the plugin crashed repeatedly');
        return;
      }
      const delay = RESTART_DELAYS[this.#crashes.length] ?? 15_000;
      this.#crashes.push(now);
      await new Promise((r) => setTimeout(r, delay));
      // Unloaded, or the instance stopped during the delay.
      if (plugin !== this || !this.#current(worker)) return;
      try {
        await worker.terminate(); // never two threads for one worker
        await worker.start();
      } catch (error) {
        log(worker.instance, 'error', `restart failed: ${errorMessage(error)}`);
        continue;
      }
      // A context whose setup fails is that context's problem, not a crash of the plugin: it
      // is dropped and James starts it again later.
      for (const instance of instances) {
        const base = this.contexts.get(instance);
        if (!base) continue;
        try {
          const context = await initialState(base);
          // Stopped while the state was fetched: do not bring it back.
          if (this.contexts.get(instance) !== base) continue;
          await worker.startContext(context);
          if (this.contexts.get(instance) !== base) await worker.stopContext(instance);
        } catch (error) {
          log(instance, 'error', `setup failed after restart: ${errorMessage(error)}`);
          // A context stopped (and maybe started again) meanwhile is not this one.
          if (this.contexts.get(instance) !== base) continue;
          this.contexts.delete(instance);
          this.#releaseCommands([instance]);
          james.notify('context.failed', { instance });
          if (error instanceof SetupTimeout && !this.sinusbot) {
            // Still hung: the next restart round handles it.
            setImmediate(() => void this.restart(worker, true));
            return;
          }
        }
      }
      log(worker.instance, 'info', 'restarted after a crash');
      return;
    }
  }

  #fail(error: string): void {
    if (this.#failed) return;
    this.#failed = true;
    james.notify('plugin.failed', { error });
  }
}

let plugin: LoadedPlugin | null = null;

async function initialState(base: ContextBase): Promise<ContextInit> {
  const call = (method: string) => james.request('call', { instance: base.instance, method, params: null });
  const [info, status, queue, snapshot, pluginStore, instanceStore, globalStore] = await Promise.all([
    call('instance.info'),
    call('instance.status'),
    call('instance.queue'),
    call('instance.snapshot').catch(() => null), // not connected yet
    loadScope('plugin', null),
    loadScope('instance', base.instance),
    loadScope('global', null),
  ]);
  return { ...base, info, status, queue, snapshot, store: { plugin: pluginStore, instance: instanceStore, global: globalStore } };
}

function log(instance: string | null, level: string, message: string): void {
  james.notify('log', { instance, level, message: message.slice(0, 8000) });
}

function loaded(): LoadedPlugin {
  if (!plugin) throw new Error('no plugin is loaded');
  return plugin;
}

// ---- requests from James ------------------------------------------------------------------

async function onRequest(method: string, params: any): Promise<unknown> {
  switch (method) {
    case 'init':
      if (params.protocol !== PROTOCOL_VERSION) throw new Error(`unsupported protocol ${params.protocol}`);
      version = String(params.version ?? '');
      return { protocol: PROTOCOL_VERSION, node: process.version };
    case 'discover': {
      const result = discover(String(params?.pluginsDir ?? ''), version);
      return {
        plugins: result.plugins.map((d) => ({ ...d.manifest, webDir: d.webDir, paths: { dir: d.dir, entry: d.entry, readable: d.readable } })),
        errors: result.errors,
      };
    }
    case 'plugin.load': {
      if (plugin) throw new Error('a plugin is already loaded');
      const manifest = params.manifest as Manifest;
      const discovered: Discovered = { manifest, dir: params.dir, entry: params.entry, webDir: null, readable: [] };
      const p = new LoadedPlugin(discovered, params.permissions ?? [], params.dataDir);
      plugin = p;
      try {
        await p.load();
      } catch (error) {
        plugin = null;
        await p.unload();
        throw error;
      }
      return null;
    }
    case 'plugin.unload': {
      const p = plugin;
      plugin = null;
      await p?.unload();
      await storeQueue;
      return null;
    }
    case 'context.start':
      await loaded().startContext({ instance: params.instance, platform: params.platform, config: params.config ?? {} });
      return null;
    case 'context.stop':
      await plugin?.stopContext(params.instance);
      return null;
    case 'command.run':
    case 'http.event': {
      const p = loaded();
      const worker = p.workerFor(params.instance);
      if (!worker || !p.contexts.has(params.instance)) throw new Error('the plugin is not active on this instance');
      const kind = method === 'command.run' ? 'command' : 'http';
      return worker.request(kind, params.instance, method === 'command.run' ? params : params.request);
    }
    default:
      throw new Error(`unknown method '${method}'`);
  }
}

function onEvent(name: string, data: any): void {
  const p = plugin;
  if (!p) return;
  switch (name) {
    case 'instance.event':
      // Also for contexts that are still starting: the worker queues events behind the start.
      if (p.contexts.has(data.instance)) {
        p.deliver(data.instance, { type: 'event', instance: data.instance, event: data, sensitive: !!data.sensitive });
      }
      return;
    case 'ws.open':
    case 'ws.message':
    case 'ws.close': {
      const kind = name.slice(3) as 'open' | 'message' | 'close';
      p.deliver(data.instance, { type: 'ws', kind, instance: data.instance, connection: data.connection, data: data.data, user: data.user });
      return;
    }
    case 'plugin.event':
      for (const [instance] of p.contexts) {
        if (data.instance && instance !== data.instance) continue;
        p.deliver(instance, { type: 'pluginEvent', instance, name: data.name, data: data.data, from: data.from });
      }
      return;
    case 'store.changed':
      storeChanged(null, { type: 'store', scope: 'global', instance: null, key: data.key, value: data.value, deleted: !!data.deleted });
      return;
  }
}

const fd = Number(process.env.JAMES_IPC_FD ?? 3);
const james = Connection.fromFd(fd, onRequest, onEvent, () => {
  // James went away: nothing left to do.
  process.exit(0);
});

process.on('uncaughtException', (error) => {
  console.error('plugin host error:', error);
});
process.on('unhandledRejection', (error) => {
  console.error('plugin host unhandled rejection:', error);
});
