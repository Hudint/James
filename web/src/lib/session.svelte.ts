// Current user, permissions and global UI state (toasts, theme).

import { api, ApiError } from './api';
import { t } from './i18n/index.svelte';
import type { Grant, Me } from './types';

export const session = $state<{ me: Me | null; loaded: boolean }>({ me: null, loaded: false });

export async function loadSession(): Promise<Me | null> {
  try {
    session.me = await api.me();
  } catch {
    session.me = null;
  }
  session.loaded = true;
  return session.me;
}

function matches(granted: string, wanted: string): boolean {
  if (granted === '*' || granted === wanted) return true;
  if (granted.endsWith('.*')) {
    const prefix = granted.slice(0, -2);
    return wanted.startsWith(prefix + '.');
  }
  return false;
}

/** Mirrors the server's permission check (crates/james-db/src/permissions.rs). */
export function can(permission: string, instance?: string): boolean {
  const grants: Grant[] = session.me?.grants ?? [];
  return grants.some(
    (g) => matches(g.permission, permission) && (g.instance === null || g.instance === (instance ?? null)),
  );
}

// ---- toasts ---------------------------------------------------------------------------

export interface Toast {
  id: number;
  kind: 'info' | 'success' | 'error';
  text: string;
}

export const toasts = $state<Toast[]>([]);
let nextToast = 1;

export function toast(text: string, kind: Toast['kind'] = 'info') {
  const id = nextToast++;
  toasts.push({ id, kind, text });
  // Errors stay until dismissed, so they can be read at any pace.
  if (kind !== 'error') setTimeout(() => dismissToast(id), 4000);
}

export function dismissToast(id: number) {
  const index = toasts.findIndex((t) => t.id === id);
  if (index >= 0) toasts.splice(index, 1);
}

/** Shows an error toast for a failed action, in the UI language where possible. */
export function reportError(error: unknown) {
  toast(errorMessage(error), 'error');
}

export function errorMessage(error: unknown): string {
  if (error instanceof ApiError) {
    if (error.status === 0) return t('errors.network');
    if (error.status >= 500) return t('errors.server');
    if (error.status === 403) return t('errors.forbidden');
    if (error.status === 404) return t('errors.notFoundItem');
    if (error.status === 429) return t('errors.rateLimited');
    // 4xx validation messages from the server are specific and shown as they are.
    return error.message;
  }
  return error instanceof Error ? error.message : String(error);
}

// ---- theme ----------------------------------------------------------------------------

export type Theme = 'system' | 'light' | 'dark';
const THEME_KEY = 'james.theme';

function initialTheme(): Theme {
  try {
    const stored = localStorage.getItem(THEME_KEY);
    if (stored === 'light' || stored === 'dark' || stored === 'system') return stored;
  } catch {
    // ignore
  }
  return 'system';
}

export const ui = $state({ theme: initialTheme() });

export function applyTheme(theme: Theme) {
  ui.theme = theme;
  if (theme === 'system') delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = theme;
  try {
    localStorage.setItem(THEME_KEY, theme);
  } catch {
    // ignore
  }
}
