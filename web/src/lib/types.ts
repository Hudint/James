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

/** The YouTube cookie file (never its content). */
export interface CookieStatus {
  /** `ytdlp.cookies` is set in the configuration and takes precedence. */
  configured: boolean;
  uploaded: { cookies: number; changedAt: number | null } | null;
}

/** What is shown of a bot's identity (never the private key). */
export interface BotIdentity {
  /** Unique id as TeamSpeak 3 shows it. */
  uid: string;
  /** Unique id as TeamSpeak 6 shows it (SHA-256 of the same key). */
  uidTs6?: string;
  securityLevel?: number;
}

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
  /** Texts being spoken or waiting to be. */
  speech: number;
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
  tts_voice?: string | null;
}

export interface TtsVoice {
  id: string;
  name: string;
  language: string;
  provider: string;
}

export interface ConfigField {
  name: string;
  title?: string;
  type: string;
  default?: unknown;
  placeholder?: string;
  options?: string[];
  vars?: ConfigField[];
  conditions?: { field: string; value: unknown }[];
  indent?: number;
}

export interface PluginManifest {
  id: string;
  kind: 'james' | 'sinusbot';
  name: string;
  version: string;
  description: string;
  author: string;
  permissions: string[];
  /** Permissions the plugin can use when granted but runs without. */
  optionalPermissions?: string[];
  config: ConfigField[];
  platforms: string[];
  autorun: boolean;
  hidden: boolean;
  web: boolean;
}

export type PluginStatus =
  | { state: 'disabled' }
  | { state: 'needsApproval'; missing: string[] }
  | { state: 'running' }
  | { state: 'starting' }
  | { state: 'failed'; error: string }
  | { state: 'unavailable' };

export interface PluginInfo {
  manifest: PluginManifest;
  state: { id: string; enabled: boolean; grantedPermissions: string[]; instances: string[] | null };
  status: PluginStatus;
  activeInstances: string[];
  commands: string[];
}

export interface PluginPermission {
  id: string;
  enforced: boolean;
  risk: 'low' | 'medium' | 'high';
  description: string;
}

export interface PluginOverview {
  host: { state: 'starting' } | { state: 'running' } | { state: 'unavailable'; error: string };
  plugins: PluginInfo[];
  errors: { id: string; error: string }[];
  pluginsDir?: string;
  permissions: PluginPermission[];
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
  /** Neither flag: a temporary channel (gone when empty). */
  permanent?: boolean;
  semiPermanent?: boolean;
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
  /** The names were edited in James (the file is unchanged). */
  edited: boolean;
  /** Names from the file's tags. */
  tags: TrackNames;
}

export interface TrackNames {
  title: string;
  artist: string | null;
  album: string | null;
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
  | { instance: string; type: 'playbackChanged'; status: InstanceStatus }
  | { instance: string; type: 'speechStarted'; id: number; text: string }
  | { instance: string; type: 'speechEnded'; id: number; error: string | null; stopped: boolean };

export type ServerEvent = InstanceEvent | { type: 'lagged'; skipped: number };

export type LogLevel = 'debug' | 'info' | 'warn' | 'error';

export interface LogEntry {
  seq: number;
  /** Unix milliseconds. */
  time: number;
  level: LogLevel;
  /** The plugin that wrote it; `null` for James itself. */
  plugin: string | null;
  message: string;
}
