// Messages between the host's main thread and plugin workers.

import type { Manifest } from './discover.ts';

export interface ContextInit {
  instance: string;
  platform: string;
  config: Record<string, unknown>;
  info: { id: string; name: string; platform: string; commandPrefix: string };
  /** Initial state: snapshot (null while disconnected), status and queue. */
  snapshot: any | null;
  status: any;
  queue: any[];
  store: { plugin: Record<string, unknown>; instance: Record<string, unknown>; global: Record<string, unknown> };
}

export type ToWorker =
  | { type: 'init'; manifest: Manifest; dir: string; entry: string; permissions: string[]; dataDir: string; version: string; syncBuffer: SharedArrayBuffer; syncPort: MessagePort }
  | { type: 'context.start'; context: ContextInit }
  | { type: 'context.stop'; instance: string }
  | { type: 'event'; instance: string; event: any; sensitive: boolean }
  | { type: 'request'; id: number; kind: 'command' | 'http'; instance: string; payload: any }
  | { type: 'ws'; kind: 'open' | 'message' | 'close'; instance: string; connection: string; data?: string; user?: unknown }
  | { type: 'reply'; id: number; result?: unknown; error?: string }
  | { type: 'store'; scope: 'plugin' | 'global'; instance?: string; key: string; value: unknown; deleted: boolean }
  | { type: 'pluginEvent'; instance: string | null; name: string; data: unknown; from: string };

export type FromWorker =
  | { type: 'ready' }
  | { type: 'loadFailed'; error: string }
  | { type: 'started'; instance: string; error?: string }
  | { type: 'stopped'; instance: string }
  | { type: 'call'; id: number; instance: string | null; method: string; params: unknown; sync?: boolean }
  | { type: 'reply'; id: number; result?: unknown; error?: string }
  | { type: 'log'; level: 'debug' | 'info' | 'warn' | 'error'; instance: string | null; message: string }
  | { type: 'ws.send'; instance: string; connection: string | null; data: string; close?: boolean }
  | { type: 'store'; scope: 'plugin' | 'instance' | 'global'; instance: string | null; key: string; value: unknown; deleted: boolean }
  | { type: 'emit'; instance: string | null; name: string; data: unknown; broadcast: boolean };

/** Layout of the shared buffer used for synchronous calls (see worker/sync.ts). */
export const SYNC_STATE = 0;
