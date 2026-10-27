// Types mirroring the James API (crates/james-api).

export interface User {
  id: number;
  username: string;
  isAdmin: boolean;
  disabled: boolean;
  hasPassword: boolean;
  oidcSubject: string | null;
  createdAt: number;
  roles: string[];
}

export interface Grant {
  permission: string;
  instance: string | null;
}

export interface Me {
  user: User;
  grants: Grant[];
}

export interface Track {
  source: string;
  locator: string;
  title: string;
  artist: string | null;
  album: string | null;
  durationMs?: number | null;
  thumbnail: string | null;
  isStream: boolean;
}

export interface Requester {
  uid: string;
  name: string;
}

export interface QueueItem {
  id: number;
  track: Track;
  requestedBy: Requester | null;
}

export type RepeatMode = 'off' | 'one' | 'all';
export type PlayMode = 'now' | 'next' | 'append';

export interface InstanceStatus {
  id: string;
  name: string;
  platform: string;
  connected: boolean;
  current: QueueItem | null;
  loading: boolean;
  positionMs: number;
  paused: boolean;
  volume: number;
  repeat: RepeatMode;
  shuffle: boolean;
  queueLength: number;
  /** Why the last connection attempt failed (while not connected). */
  lastError: string | null;
}

export interface AudioSettings {
  volume: number;
  bitrate: number | null;
  target_lufs: number;
}

export interface InstanceConfig {
  id: string;
  name: string;
  platform: string;
  settings: Record<string, unknown>;
  audio: AudioSettings;
  command_prefix: string;
  now_playing_description: boolean;
  autostart: boolean;
}

export interface InstanceSummary {
  id: string;
  name: string;
  platform: string;
  autostart: boolean;
  running: boolean;
  status: InstanceStatus | null;
  config: InstanceConfig | null;
}

export interface PlatformClient {
  id: string;
  uid: string;
  name: string;
  channel: string | null;
  serverGroups: string[];
  isSelf: boolean;
}

export interface PlatformChannel {
  id: string;
  parent: string | null;
  name: string;
  topic: string;
  description: string;
  codecQuality: number | null;
  order: number;
}

export interface Snapshot {
  server: { name: string; version: string; platform: string; welcomeMessage: string } | null;
  ownClient: string | null;
  clients: PlatformClient[];
  channels: PlatformChannel[];
  serverGroups: { id: string; name: string }[];
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

export interface Playlist {
  id: number;
  name: string;
  ownerId: number | null;
  trackCount: number;
  createdAt: number;
  updatedAt: number;
}

export interface Role {
  id: number;
  name: string;
  description: string;
  builtin: boolean;
  grants: Grant[];
}

export interface TokenInfo {
  id: string;
  kind: 'session' | 'api';
  name: string;
  createdAt: number;
  expiresAt: number | null;
  lastUsed: number | null;
}

export interface Identity {
  platform: string;
  uid: string;
  userId: number;
  name: string;
}

/** Events from `GET /api/v1/ws`. */
export type InstanceEvent =
  | { instance: string; type: 'started' }
  | { instance: string; type: 'stopped' }
  | { instance: string; type: 'connected'; snapshot: Snapshot }
  | { instance: string; type: 'disconnected'; reason: string }
  | { instance: string; type: 'platform'; event: { type: string; [key: string]: unknown } }
  | { instance: string; type: 'trackStarted'; item: QueueItem }
  | { instance: string; type: 'trackEnded'; item: QueueItem; error: string | null }
  | { instance: string; type: 'queueChanged' }
  | { instance: string; type: 'playbackChanged'; status: InstanceStatus };

export type ServerEvent = InstanceEvent | { type: 'lagged'; skipped: number };
