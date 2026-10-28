// Live state of all instances, kept up to date via the WebSocket event stream.

import { api } from './api';
import { t } from './i18n/index.svelte';
import { toast } from './session.svelte';
import type { InstanceStatus, QueueItem, ServerEvent, Snapshot } from './types';

interface LiveInstance {
  status: InstanceStatus | null;
  queue: QueueItem[] | null;
  snapshot: Snapshot | null;
  /** Client time when `status.positionMs` was reported, to interpolate progress. */
  statusAt: number;
}

export const live = $state<{ connected: boolean; instances: Record<string, LiveInstance>; version: number }>({
  connected: false,
  instances: {},
  // Bumped when instances start or stop, so lists can refetch.
  version: 0,
});

function entry(id: string): LiveInstance {
  live.instances[id] ??= { status: null, queue: null, snapshot: null, statusAt: 0 };
  return live.instances[id];
}

export function setStatus(id: string, status: InstanceStatus | null) {
  const e = entry(id);
  e.status = status;
  e.statusAt = performance.now();
}

/** Current playback position, interpolated between status updates. */
export function positionOf(id: string, now: number): number {
  const e = live.instances[id];
  const s = e?.status;
  if (!s?.current || s.loading) return 0;
  if (s.paused) return s.positionMs;
  const position = s.positionMs + (now - e.statusAt);
  const duration = s.current.track.durationMs;
  return duration ? Math.min(position, duration) : position;
}

const queueRefresh = new Map<string, ReturnType<typeof setTimeout>>();

export function refreshQueue(id: string) {
  clearTimeout(queueRefresh.get(id));
  // Debounce bursts of queueChanged events.
  queueRefresh.set(
    id,
    setTimeout(async () => {
      try {
        entry(id).queue = await api.queue(id);
      } catch {
        // instance stopped
      }
    }, 100),
  );
}

export async function refreshStatus(id: string) {
  try {
    setStatus(id, await api.status(id));
  } catch {
    setStatus(id, null);
  }
}

function handle(event: ServerEvent) {
  if (event.type === 'lagged') {
    for (const id of Object.keys(live.instances)) {
      void refreshStatus(id);
      refreshQueue(id);
    }
    return;
  }
  const id = event.instance;
  switch (event.type) {
    case 'playbackChanged':
      setStatus(id, event.status);
      break;
    case 'queueChanged':
      refreshQueue(id);
      break;
    case 'speechStarted':
    case 'speechEnded':
      void refreshStatus(id);
      if (event.type === 'speechEnded' && event.error) toast(t('speak.failed', { error: event.error }), 'error');
      break;
    case 'connected':
      entry(id).snapshot = event.snapshot;
      void refreshStatus(id);
      break;
    case 'disconnected':
      entry(id).snapshot = null;
      void refreshStatus(id);
      break;
    case 'started':
      void refreshStatus(id);
      live.version++;
      break;
    case 'stopped':
      setStatus(id, null);
      entry(id).queue = null;
      entry(id).snapshot = null;
      live.version++;
      break;
    case 'trackEnded':
      if (event.error) toast(t('player.failed', { title: event.item.track.title, error: event.error }), 'error');
      break;
    case 'platform':
      // Client/channel changes: refresh the server view lazily if it is shown.
      if (live.instances[id]?.snapshot) scheduleSnapshot(id);
      break;
    default:
      break;
  }
}

const snapshotRefresh = new Map<string, ReturnType<typeof setTimeout>>();

function scheduleSnapshot(id: string) {
  clearTimeout(snapshotRefresh.get(id));
  snapshotRefresh.set(
    id,
    setTimeout(async () => {
      try {
        entry(id).snapshot = await api.server(id);
      } catch {
        // not connected
      }
    }, 300),
  );
}

export async function loadSnapshot(id: string) {
  try {
    entry(id).snapshot = await api.server(id);
  } catch {
    entry(id).snapshot = null;
  }
}

let socket: WebSocket | null = null;
let retry = 0;
let stopped = false;
let retryTimer: ReturnType<typeof setTimeout> | undefined;

/** Opens the event stream (idempotent); reconnects with backoff. */
export function connectLive() {
  stopped = false;
  clearTimeout(retryTimer);
  if (socket) return;
  const url = `${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}/api/v1/ws`;
  const ws = new WebSocket(url);
  socket = ws;
  ws.onopen = () => {
    if (socket !== ws) return;
    live.connected = true;
    retry = 0;
    // Resynchronise everything that might have changed while disconnected.
    handle({ type: 'lagged', skipped: 0 });
  };
  ws.onmessage = (message) => {
    if (socket !== ws) return;
    try {
      handle(JSON.parse(message.data));
    } catch {
      // ignore malformed events
    }
  };
  ws.onclose = () => {
    // A late close of a replaced socket must not affect the current one.
    if (socket !== ws) return;
    live.connected = false;
    socket = null;
    if (stopped) return;
    const delay = Math.min(30_000, 1000 * 2 ** retry++);
    retryTimer = setTimeout(connectLive, delay);
  };
}

export function disconnectLive() {
  stopped = true;
  clearTimeout(retryTimer);
  const ws = socket;
  socket = null;
  live.connected = false;
  ws?.close();
}
