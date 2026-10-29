// The channels of a server as a tree for choosing one (instance settings, plugin settings).

import type { PlatformChannel } from './types';

export interface ChannelChoice {
  id: string;
  name: string;
  /** Nesting level (0 = top). */
  depth: number;
  /** `Lobby/Music`. */
  path: string;
}

/** TeamSpeak spacers (`[cspacer]---`) are decoration, not channels to join. */
export const isSpacer = (name: string) => /^\[[lcr*]?spacer[^\]]*\]/i.test(name);

/**
 * Channels depth first (by their order) with depth and path, without spacers and temporary
 * channels (they disappear when empty: no use in a setting).
 */
export function channelTree(channels: readonly PlatformChannel[] | null | undefined): ChannelChoice[] {
  const known = new Set((channels ?? []).map((c) => c.id));
  const children = new Map<string, PlatformChannel[]>();
  for (const c of channels ?? []) {
    // A channel whose parent is not in the list is shown at the top.
    const key = c.parent && known.has(c.parent) ? c.parent : '';
    const list = children.get(key);
    if (list) list.push(c);
    else children.set(key, [c]);
  }
  const out: ChannelChoice[] = [];
  // Defends against a parent cycle in the data (each channel is listed once).
  const seen = new Set<string>();
  const walk = (parent: string, depth: number, prefix: string) => {
    for (const c of (children.get(parent) ?? []).sort((a, b) => a.order - b.order)) {
      if (seen.has(c.id)) continue;
      seen.add(c.id);
      const path = prefix ? `${prefix}/${c.name}` : c.name;
      const temporary = c.permanent === false && c.semiPermanent === false;
      if (!isSpacer(c.name) && !temporary) out.push({ id: c.id, name: c.name, depth, path });
      walk(c.id, depth + 1, path);
    }
  };
  walk('', 0, '');
  return out;
}

/** Indentation of a nested channel in a `<select>` (options cannot be styled). */
export const indent = (depth: number) => '\u00a0\u00a0\u00a0'.repeat(depth);
