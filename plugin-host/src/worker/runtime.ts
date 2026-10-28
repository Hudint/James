// Worker runtime: one per plugin worker. Holds the plugin's contexts (one per instance),
// dispatches events, and performs API calls through the main thread.

import { inspect } from 'node:util';
import { parentPort, receiveMessageOnPort } from 'node:worker_threads';

import type { Manifest } from '../discover.ts';
import type { ContextInit, FromWorker, ToWorker } from '../protocol.ts';
import { InstanceState, type Emitted } from './state.ts';

export type Handler = (payload: any) => unknown;

const SYNC_TIMEOUT = 20_000;

export class Runtime {
  readonly manifest: Manifest;
  readonly permissions: Set<string>;
  readonly dataDir: string;
  readonly contexts = new Map<string, Context>();
  #nextId = 1;
  #pending = new Map<number, { resolve: (v: any) => void; reject: (e: Error) => void }>();
  #syncState: Int32Array;
  #syncPort: MessagePort;

  /** Creates the plugin's per-instance object (modern: Bot, SinusBot: module set). */
  onContextStart: ((context: Context) => Promise<void>) | null = null;
  /** Global store shared with all plugins (kept in sync by the main thread). */
  readonly globalStore = new Map<string, unknown>();
  readonly pluginStore = new Map<string, unknown>();

  constructor(init: Extract<ToWorker, { type: 'init' }>) {
    this.manifest = init.manifest;
    this.permissions = new Set(init.permissions);
    this.dataDir = init.dataDir;
    this.#syncState = new Int32Array(init.syncBuffer);
    this.#syncPort = init.syncPort;
  }

  post(message: FromWorker): void {
    parentPort!.postMessage(message);
  }

  /** Asynchronous API call. */
  call(instance: string | null, method: string, params: unknown = null): Promise<any> {
    const id = this.#nextId++;
    return new Promise((resolve, reject) => {
      this.#pending.set(id, { resolve, reject });
      this.post({ type: 'call', id, instance, method, params });
    });
  }

  /** Synchronous API call: blocks this worker until James answers (SinusBot semantics). */
  callSync(instance: string | null, method: string, params: unknown = null): any {
    const id = this.#nextId++;
    Atomics.store(this.#syncState, 0, 0);
    this.post({ type: 'call', id, instance, method, params, sync: true });
    const deadline = Date.now() + SYNC_TIMEOUT;
    let reply: { id: number; result?: unknown; error?: string } | undefined;
    for (;;) {
      // Answers of earlier calls that timed out may arrive first: skip them and wait on.
      for (let next = receiveMessageOnPort(this.#syncPort as any); next; next = receiveMessageOnPort(this.#syncPort as any)) {
        if ((next.message as { id: number }).id === id) reply = next.message as typeof reply;
      }
      if (reply) break;
      const left = deadline - Date.now();
      if (left <= 0) throw new Error(`${method}: James did not answer in time`);
      Atomics.wait(this.#syncState, 0, 0, left);
      Atomics.store(this.#syncState, 0, 0);
    }
    if (reply.error !== undefined) throw new Error(reply.error);
    return reply.result;
  }

  log(level: 'debug' | 'info' | 'warn' | 'error', instance: string | null, args: unknown[]): void {
    const message = args.map((a) => (typeof a === 'string' ? a : inspect(a, { depth: 4, breakLength: Infinity }))).join(' ');
    this.post({ type: 'log', level, instance, message });
  }

  has(permission: string): boolean {
    return this.permissions.has(permission);
  }

  async handle(message: ToWorker): Promise<void> {
    switch (message.type) {
      case 'reply': {
        const pending = this.#pending.get(message.id);
        this.#pending.delete(message.id);
        if (message.error !== undefined) pending?.reject(new Error(message.error));
        else pending?.resolve(message.result ?? null);
        return;
      }
      case 'context.start':
        await this.#start(message.context);
        return;
      case 'context.stop':
        await this.#stop(message.instance);
        return;
      case 'event': {
        const context = this.contexts.get(message.instance);
        if (context) await context.receive(message.event);
        return;
      }
      case 'request': {
        const context = this.contexts.get(message.instance);
        try {
          if (!context) throw new Error('the plugin is not active on this instance');
          const result = message.kind === 'command' ? await context.runCommand(message.payload) : await context.handleHttp(message.payload);
          this.post({ type: 'reply', id: message.id, result: result ?? null });
        } catch (error: any) {
          this.post({ type: 'reply', id: message.id, error: error?.message ?? String(error) });
        }
        return;
      }
      case 'ws': {
        const context = this.contexts.get(message.instance);
        if (!context) return;
        const name = message.kind === 'open' ? 'ws.connect' : message.kind === 'message' ? 'ws.message' : 'ws.close';
        context.dispatch(name, { connection: message.connection, data: message.data, user: message.user ?? null });
        return;
      }
      case 'store': {
        const map = message.scope === 'global' ? this.globalStore : this.pluginStore;
        if (message.deleted) map.delete(message.key);
        else map.set(message.key, message.value);
        return;
      }
      case 'pluginEvent': {
        const targets = message.instance ? [this.contexts.get(message.instance)] : [...this.contexts.values()];
        for (const context of targets) context?.dispatch(`custom:${message.name}`, { name: message.name, data: message.data, from: message.from });
        return;
      }
    }
  }

  async #start(init: ContextInit): Promise<void> {
    // A second start for the same instance replaces the first one cleanly.
    if (this.contexts.has(init.instance)) await this.#stop(init.instance);
    // Shared scopes are loaded once; later contexts keep the live copy.
    if (this.contexts.size === 0) {
      this.pluginStore.clear();
      for (const [k, v] of Object.entries(init.store.plugin)) this.pluginStore.set(k, v);
      this.globalStore.clear();
      for (const [k, v] of Object.entries(init.store.global)) this.globalStore.set(k, v);
    }
    const context = new Context(this, init);
    this.contexts.set(init.instance, context);
    try {
      await this.onContextStart?.(context);
      this.post({ type: 'started', instance: init.instance });
    } catch (error: any) {
      this.log('error', init.instance, [`setup failed: ${error?.stack ?? error}`]);
      await context.stop();
      this.contexts.delete(init.instance);
      this.post({ type: 'started', instance: init.instance, error: error?.message ?? String(error) });
    }
  }

  async #stop(instance: string): Promise<void> {
    const context = this.contexts.get(instance);
    this.contexts.delete(instance);
    await context?.stop();
    this.post({ type: 'stopped', instance });
  }
}

export interface CommandSpec {
  name: string;
  aliases: string[];
  run: (context: any) => unknown;
}

/** A plugin on one instance. */
export class Context {
  readonly runtime: Runtime;
  readonly instance: string;
  readonly platform: string;
  readonly info: ContextInit['info'];
  readonly config: Record<string, unknown>;
  readonly state: InstanceState;
  readonly instanceStore: Map<string, unknown>;
  readonly abort = new AbortController();
  #handlers = new Map<string, Set<Handler>>();
  #stopHandlers: (() => unknown)[] = [];
  #timers = new Set<NodeJS.Timeout>();
  readonly commands = new Map<string, CommandSpec>();
  readonly http = new Map<string, { handler: (request: any) => unknown; public: boolean }>();
  stopped = false;
  /** Objects the API layer attaches (wrappers cache etc.). */
  readonly extra: Record<string, any> = {};

  constructor(runtime: Runtime, init: ContextInit) {
    this.runtime = runtime;
    this.instance = init.instance;
    this.platform = init.platform;
    this.info = init.info;
    this.config = withDefaults(runtime.manifest.config as any[], init.config);
    this.state = new InstanceState(init.snapshot, init.status, init.queue);
    this.instanceStore = new Map(Object.entries(init.store.instance));
  }

  call(method: string, params: unknown = null): Promise<any> {
    return this.runtime.call(this.instance, method, params);
  }

  callSync(method: string, params: unknown = null): any {
    return this.runtime.callSync(this.instance, method, params);
  }

  log(level: 'debug' | 'info' | 'warn' | 'error', ...args: unknown[]): void {
    this.runtime.log(level, this.instance, args);
  }

  on(event: string, handler: Handler): () => void {
    let set = this.#handlers.get(event);
    if (!set) this.#handlers.set(event, (set = new Set()));
    set.add(handler);
    return () => set!.delete(handler);
  }

  hasListeners(event: string): boolean {
    return (this.#handlers.get(event)?.size ?? 0) > 0;
  }

  /** Calls the handlers of an event; failures are logged, never propagated. */
  dispatch(event: string, payload: unknown): void {
    if (this.stopped) return;
    const handlers = this.#handlers.get(event);
    if (!handlers) return;
    for (const handler of [...handlers]) {
      try {
        const result = handler(payload);
        if (result && typeof (result as Promise<unknown>).catch === 'function') {
          (result as Promise<unknown>).catch((error) => this.log('error', `${event} handler failed:`, error));
        }
      } catch (error) {
        this.log('error', `${event} handler failed:`, error);
      }
    }
  }

  /** Raw (mirror-level) events are re-emitted under `raw:<name>` for the API layers. */
  async receive(envelope: any): Promise<void> {
    const emitted: Emitted[] = this.state.apply(envelope);
    for (const [name, payload] of emitted) {
      if (name === 'queueChange') {
        try {
          this.state.queue = await this.call('instance.queue');
        } catch {
          // keep the old queue; the next change refreshes it
        }
        payload.queue = this.state.queue;
      }
      this.dispatch(`raw:${name}`, payload);
    }
  }

  onStop(handler: () => unknown): void {
    this.#stopHandlers.push(handler);
  }

  setTimeout(handler: () => void, ms: number): () => void {
    const timer = setTimeout(() => {
      this.#timers.delete(timer);
      this.guard('timer', handler);
    }, ms);
    this.#timers.add(timer);
    return () => {
      clearTimeout(timer);
      this.#timers.delete(timer);
    };
  }

  setInterval(handler: () => void, ms: number): () => void {
    const timer = setInterval(() => this.guard('interval', handler), Math.max(ms, 10));
    this.#timers.add(timer);
    return () => {
      clearInterval(timer);
      this.#timers.delete(timer);
    };
  }

  guard(what: string, fn: () => unknown): void {
    if (this.stopped) return;
    try {
      const result = fn();
      if (result && typeof (result as Promise<unknown>).catch === 'function') {
        (result as Promise<unknown>).catch((error) => this.log('error', `${what} failed:`, error));
      }
    } catch (error) {
      this.log('error', `${what} failed:`, error);
    }
  }

  async stop(): Promise<void> {
    if (this.stopped) return;
    this.dispatch('unload', {});
    this.stopped = true;
    this.abort.abort();
    for (const timer of this.#timers) clearTimeout(timer);
    this.#timers.clear();
    for (const handler of this.#stopHandlers.reverse()) {
      try {
        await Promise.race([handler(), new Promise((r) => setTimeout(r, 3_000))]);
      } catch (error) {
        this.log('error', 'cleanup failed:', error);
      }
    }
    for (const name of this.commands.keys()) {
      await this.call('commands.unregister', { name }).catch(() => {});
    }
    this.#handlers.clear();
  }

  async registerCommand(spec: CommandSpec & { usage?: string; description?: string; permission?: string }): Promise<void> {
    const name = spec.name.toLowerCase();
    await this.call('commands.register', {
      name,
      aliases: spec.aliases,
      usage: spec.usage ?? '',
      description: spec.description ?? '',
      permission: spec.permission ?? null,
    });
    this.commands.set(name, { ...spec, name });
  }

  async unregisterCommand(name: string): Promise<void> {
    const key = name.toLowerCase();
    if (!this.commands.delete(key)) return;
    await this.call('commands.unregister', { name: key });
  }

  async runCommand(payload: { name: string; args: string; mode: string; invoker: any }): Promise<unknown> {
    const name = payload.name.toLowerCase();
    const spec = this.commands.get(name) ?? [...this.commands.values()].find((c) => c.aliases.some((a) => a.toLowerCase() === name));
    if (!spec) return 'This command is not available here.';
    return this.extra.runCommand(spec, payload);
  }

  async handleHttp(request: { name: string; public: boolean }): Promise<unknown> {
    const entry = this.http.get(request.name);
    if (!entry || (request.public && !entry.public)) throw new Error(`no handler for '${request.name}'`);
    return entry.handler(request);
  }

  /** Persists a store change (and keeps the local copy current). */
  storeChanged(scope: 'plugin' | 'instance' | 'global', key: string, value: unknown, deleted: boolean): void {
    this.runtime.post({ type: 'store', scope, instance: scope === 'instance' ? this.instance : null, key, value, deleted });
  }
}

/** Applies the manifest's defaults to missing config values. */
export function withDefaults(fields: any[] | undefined, config: Record<string, unknown>): Record<string, unknown> {
  const result: Record<string, unknown> = { ...config };
  for (const field of fields ?? []) {
    if (field && typeof field.name === 'string' && result[field.name] === undefined && field.default !== undefined) {
      result[field.name] = field.default;
    }
  }
  return result;
}

/** Splits command arguments at whitespace; double quotes group words. */
export function splitArgs(args: string): string[] {
  const out: string[] = [];
  const re = /"((?:[^"\\]|\\.)*)"|(\S+)/g;
  for (const m of args.matchAll(re)) out.push(m[1] !== undefined ? m[1].replace(/\\(.)/g, '$1') : m[2]);
  return out;
}
