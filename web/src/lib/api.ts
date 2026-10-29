// Thin typed client for the James API. Authentication uses the HttpOnly session cookie.

import type {
  Identity,
  InstanceConfig,
  InstanceStatus,
  InstanceSummary,
  LibraryTrack,
  Me,
  PlayMode,
  Playlist,
  PluginInfo,
  PluginOverview,
  TtsVoice,
  QueueItem,
  RepeatMode,
  Role,
  Grant,
  Snapshot,
  TokenInfo,
  Track,
  User,
  BotIdentity,
  CookieStatus,
} from "./types";

export class ApiError extends Error {
  constructor(
    public status: number,
    message: string,
  ) {
    super(message);
  }
}

async function request<T>(
  method: string,
  path: string,
  body?: unknown,
): Promise<T> {
  const init: RequestInit = { method, credentials: "same-origin", headers: {} };
  if (body instanceof FormData) {
    init.body = body;
  } else if (body !== undefined) {
    init.body = JSON.stringify(body);
    (init.headers as Record<string, string>)["content-type"] =
      "application/json";
  }
  let response: Response;
  try {
    response = await fetch(`/api/v1${path}`, init);
  } catch {
    throw new ApiError(0, "network");
  }
  if (response.status === 204) return undefined as T;
  const text = await response.text();
  let data: { error?: string } | undefined;
  try {
    data = text ? JSON.parse(text) : undefined;
  } catch {
    // Not JSON (e.g. a reverse proxy error page): fall back to the status text below.
    data = undefined;
  }
  if (!response.ok) {
    throw new ApiError(response.status, data?.error ?? response.statusText);
  }
  return data as T;
}

const enc = encodeURIComponent;

export const api = {
  setupRequired: () => request<{ required: boolean }>("GET", "/setup"),
  setup: (username: string, password: string, setupToken: string) =>
    request<{ user: User }>("POST", "/setup", {
      username,
      password,
      setupToken,
    }),
  login: (username: string, password: string) =>
    request<{ user: User }>("POST", "/auth/login", { username, password }),
  logout: () => request<void>("POST", "/auth/logout"),
  providers: () =>
    request<{ oidc: { name: string } | null }>("GET", "/auth/providers"),
  me: () => request<Me>("GET", "/me"),
  changePassword: (currentPassword: string | null, newPassword: string) =>
    request<void>("POST", "/me/password", { currentPassword, newPassword }),
  tokens: () => request<TokenInfo[]>("GET", "/me/tokens"),
  createToken: (name: string, days: number | null) =>
    request<{ token: string }>("POST", "/me/tokens", { name, days }),
  revokeToken: (id: string) => request<void>("DELETE", `/me/tokens/${enc(id)}`),
  identities: () => request<Identity[]>("GET", "/me/identities"),
  linkCode: () =>
    request<{ code: string; expiresInSecs: number }>(
      "POST",
      "/me/identities/code",
    ),
  unlink: (platform: string, uid: string) =>
    request<void>("DELETE", `/me/identities/${enc(platform)}/${enc(uid)}`),

  instances: () => request<InstanceSummary[]>("GET", "/instances"),
  instance: (id: string) =>
    request<InstanceSummary>("GET", `/instances/${enc(id)}`),
  createInstance: (config: InstanceConfig) =>
    request<InstanceSummary>("POST", "/instances", config),
  updateInstance: (id: string, config: InstanceConfig) =>
    request<InstanceSummary>("PUT", `/instances/${enc(id)}`, config),
  deleteInstance: (id: string) =>
    request<void>("DELETE", `/instances/${enc(id)}`),
  startInstance: (id: string) =>
    request<void>("POST", `/instances/${enc(id)}/start`),
  stopInstance: (id: string) =>
    request<void>("POST", `/instances/${enc(id)}/stop`),
  restartInstance: (id: string) =>
    request<void>("POST", `/instances/${enc(id)}/restart`),
  cookieStatus: () => request<CookieStatus>("GET", "/settings/youtube-cookies"),
  uploadCookies: (content: string) =>
    request<{ cookies: number; dropped: number }>(
      "PUT",
      "/settings/youtube-cookies",
      { content },
    ),
  removeCookies: () => request<void>("DELETE", "/settings/youtube-cookies"),
  identity: (id: string) =>
    request<{ identity: BotIdentity | null }>(
      "GET",
      `/instances/${enc(id)}/identity`,
    ),
  importIdentity: (id: string, identity: string) =>
    request<{ identity: BotIdentity | null }>(
      "PUT",
      `/instances/${enc(id)}/identity`,
      { identity },
    ),
  /** The private identity (secret): only after the user asked for it. */
  /** `password`: the account's own, proving it again. */
  exportIdentity: (id: string, password: string) =>
    request<{ identity: string }>(
      "POST",
      `/instances/${enc(id)}/identity/export`,
      { password },
    ),

  status: (id: string) =>
    request<InstanceStatus>("GET", `/instances/${enc(id)}/status`),
  play: (id: string, input: string, mode: PlayMode) =>
    request<QueueItem[]>("POST", `/instances/${enc(id)}/play`, { input, mode }),
  skip: (id: string) => request<void>("POST", `/instances/${enc(id)}/skip`),
  previous: (id: string) =>
    request<void>("POST", `/instances/${enc(id)}/previous`),
  stopPlayback: (id: string) =>
    request<void>("POST", `/instances/${enc(id)}/stop-playback`),
  pause: (id: string) => request<void>("POST", `/instances/${enc(id)}/pause`),
  resume: (id: string) => request<void>("POST", `/instances/${enc(id)}/resume`),
  volume: (id: string, volume: number) =>
    request<void>("PUT", `/instances/${enc(id)}/volume`, { volume }),
  seek: (id: string, positionMs: number) =>
    request<void>("POST", `/instances/${enc(id)}/seek`, { positionMs }),
  repeat: (id: string, mode: RepeatMode) =>
    request<void>("PUT", `/instances/${enc(id)}/repeat`, { mode }),
  shuffle: (id: string, enabled: boolean) =>
    request<void>("PUT", `/instances/${enc(id)}/shuffle`, { enabled }),
  queue: (id: string) =>
    request<QueueItem[]>("GET", `/instances/${enc(id)}/queue`),
  clearQueue: (id: string) =>
    request<void>("DELETE", `/instances/${enc(id)}/queue`),
  shuffleQueue: (id: string) =>
    request<void>("POST", `/instances/${enc(id)}/queue/shuffle`),
  // Queue items are addressed by id (stable while others change the queue).
  moveQueueItem: (id: string, item: number, to: number) =>
    request<void>("POST", `/instances/${enc(id)}/queue/move`, { item, to }),
  removeQueueItem: (id: string, item: number) =>
    request<QueueItem>("DELETE", `/instances/${enc(id)}/queue/${item}`),
  playQueueItem: (id: string, item: number) =>
    request<void>("POST", `/instances/${enc(id)}/queue/${item}/play`),
  addTracks: (id: string, tracks: Track[], mode: PlayMode) =>
    request<QueueItem[]>("POST", `/instances/${enc(id)}/queue`, {
      tracks,
      mode,
    }),
  search: (id: string, q: string, limit = 10) =>
    request<Track[]>(
      "GET",
      `/instances/${enc(id)}/search?q=${enc(q)}&limit=${limit}`,
    ),
  moveBot: (id: string, channel: string) =>
    request<void>("POST", `/instances/${enc(id)}/move`, { channel }),
  server: (id: string) =>
    request<Snapshot>("GET", `/instances/${enc(id)}/server`),

  library: (q: string, limit = 50, offset = 0) =>
    request<{ total: number; tracks: LibraryTrack[] }>(
      "GET",
      `/library?q=${enc(q)}&limit=${limit}&offset=${offset}`,
    ),
  scanLibrary: () => request<void>("POST", "/library/scan"),
  upload: (files: FileList | File[]) => {
    const form = new FormData();
    for (const file of Array.from(files)) form.append("file", file, file.name);
    return request<{ files: string[] }>("POST", "/library/upload", form);
  },

  playlists: () => request<Playlist[]>("GET", "/playlists"),
  playlist: (id: number) =>
    request<Playlist & { tracks: Track[] }>("GET", `/playlists/${id}`),
  createPlaylist: (name: string) =>
    request<Playlist>("POST", "/playlists", { name }),
  renamePlaylist: (id: number, name: string) =>
    request<Playlist>("PATCH", `/playlists/${id}`, { name }),
  deletePlaylist: (id: number) => request<void>("DELETE", `/playlists/${id}`),
  setPlaylistTracks: (id: number, tracks: Track[]) =>
    request<void>("PUT", `/playlists/${id}/tracks`, tracks),
  appendToPlaylist: (id: number, input: string) =>
    request<Track[]>("POST", `/playlists/${id}/tracks`, { input }),
  playPlaylist: (id: number, instance: string, mode: PlayMode) =>
    request<QueueItem[]>("POST", `/playlists/${id}/play`, { instance, mode }),

  voices: () => request<TtsVoice[]>("GET", "/tts/voices"),
  say: (id: string, text: string) =>
    request<{ id: number }>("POST", `/instances/${enc(id)}/say`, { text }),
  stopSpeech: (id: string) =>
    request<void>("POST", `/instances/${enc(id)}/say/stop`),
  plugins: () => request<PluginOverview>("GET", "/plugins"),
  reloadPlugins: () => request<void>("POST", "/plugins/reload"),
  configurePlugin: (
    id: string,
    body: {
      enabled: boolean;
      permissions: string[];
      instances: string[] | null;
    },
  ) => request<PluginInfo>("PUT", `/plugins/${enc(id)}`, body),
  pluginToken: (instance: string, id: string) =>
    request<{ token: string; expiresInSecs: number }>(
      "POST",
      `/instances/${enc(instance)}/plugins/${enc(id)}/token`,
    ),
  /** Activates or deactivates a plugin on one instance; `permissions` replaces the granted ones. */
  setPluginActive: (
    instance: string,
    id: string,
    body: { active: boolean; permissions?: string[] },
  ) =>
    request<PluginInfo>(
      "PUT",
      `/instances/${enc(instance)}/plugins/${enc(id)}`,
      body,
    ),
  /** Replaces the granted permissions, keeping where the plugin runs (`enable`: also enables it). */
  setPluginPermissions: (id: string, permissions: string[], enable = false) =>
    request<PluginInfo>("PUT", `/plugins/${enc(id)}/permissions`, { permissions, enable }),
  pluginConfigs: (id: string) =>
    request<Record<string, Record<string, unknown>>>(
      "GET",
      `/plugins/${enc(id)}/config`,
    ),
  setPluginConfig: (
    instance: string,
    id: string,
    config: Record<string, unknown>,
  ) =>
    request<void>(
      "PUT",
      `/instances/${enc(instance)}/plugins/${enc(id)}/config`,
      config,
    ),

  users: () => request<User[]>("GET", "/users"),
  createUser: (body: {
    username: string;
    password?: string;
    isAdmin?: boolean;
    roles?: string[];
  }) => request<User>("POST", "/users", body),
  updateUser: (
    id: number,
    body: {
      password?: string;
      isAdmin?: boolean;
      disabled?: boolean;
      roles?: string[];
    },
  ) => request<User>("PATCH", `/users/${id}`, body),
  deleteUser: (id: number) => request<void>("DELETE", `/users/${id}`),
  roles: () => request<Role[]>("GET", "/roles"),
  createRole: (name: string, description: string, grants: Grant[]) =>
    request<Role>("POST", "/roles", { name, description, grants }),
  setRoleGrants: (name: string, grants: Grant[]) =>
    request<Role>("PUT", `/roles/${enc(name)}`, grants),
  deleteRole: (name: string) => request<void>("DELETE", `/roles/${enc(name)}`),
};
