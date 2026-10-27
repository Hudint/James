<script lang="ts">
  import { api } from '$lib/api';
  import { t } from '$lib/i18n/index.svelte';
  import { can, reportError, toast } from '$lib/session.svelte';
  import type { PlatformChannel, PlatformClient, Snapshot } from '$lib/types';
  import Icon from './Icon.svelte';

  let { id, snapshot }: { id: string; snapshot: Snapshot } = $props();

  let canMove = $derived(can('server.move', id));
  let botChannel = $derived(snapshot.clients.find((c) => c.isSelf)?.channel ?? null);

  async function moveHere(channel: PlatformChannel) {
    try {
      await api.moveBot(id, channel.id);
      toast(t('server.moved', { name: channel.name }), 'success');
    } catch (e) {
      reportError(e);
    }
  }

  interface Node {
    channel: PlatformChannel;
    depth: number;
    clients: PlatformClient[];
  }

  // Channels arrive in tree order (depth first); compute depths from the parent links.
  let nodes = $derived.by((): Node[] => {
    const depth = new Map<string, number>();
    return snapshot.channels.map((channel) => {
      const d = channel.parent ? (depth.get(channel.parent) ?? 0) + 1 : 0;
      depth.set(channel.id, d);
      const clients = snapshot.clients
        .filter((c) => c.channel === channel.id)
        .sort((a, b) => a.name.localeCompare(b.name));
      return { channel, depth: d, clients };
    });
  });
</script>

<section class="card" aria-labelledby="server-title">
  <div class="card-header">
    <h2 id="server-title" class="truncate">{snapshot.server?.name ?? t('server.title')}</h2>
    <span class="badge">{t('server.clients', { count: snapshot.clients.length })}</span>
  </div>
  {#if snapshot.server}
    <p class="muted small version">{snapshot.server.platform} · {snapshot.server.version}</p>
  {/if}
  <ul class="tree">
    {#each nodes as node (node.channel.id)}
      <li style:--depth={node.depth}>
        <div class="channel">
          <Icon name="folder" size={16} />
          <span class="truncate">{node.channel.name}</span>
          {#if canMove && node.channel.id !== botChannel}
            <button class="btn btn-small btn-ghost move" onclick={() => moveHere(node.channel)} aria-label="{t('server.moveHere')}: {node.channel.name}">
              {t('server.moveHere')}
            </button>
          {/if}
        </div>
        {#if node.clients.length > 0}
          <ul class="clients">
            {#each node.clients as client (client.id)}
              <li class:self={client.isSelf}>
                <Icon name="user" size={14} />
                <span class="truncate">{client.name}</span>
                {#if client.isSelf}<span class="badge accent">{t('server.you')}</span>{/if}
              </li>
            {/each}
          </ul>
        {/if}
      </li>
    {/each}
  </ul>
</section>

<style>
  .version {
    margin: calc(-1 * var(--space-2)) 0 var(--space-3);
  }
  .tree,
  .clients {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .tree > li {
    padding-left: calc(var(--depth) * 18px);
  }
  .channel,
  .clients li {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    min-height: 30px;
    font-size: var(--text-sm);
  }
  .channel {
    font-weight: 600;
  }
  .channel .truncate {
    flex: 1;
  }
  .move {
    min-height: 26px;
    padding: 0 var(--space-2);
    font-size: var(--text-xs);
    opacity: 0;
  }
  .channel:hover .move,
  .move:focus-visible {
    opacity: 1;
  }
  @media (hover: none) {
    .move {
      opacity: 1;
    }
  }
  .channel :global(svg) {
    color: var(--text-muted);
    flex-shrink: 0;
  }
  .clients li {
    padding-left: 22px;
    color: var(--text-muted);
  }
  .clients li.self {
    color: var(--accent);
  }
</style>
