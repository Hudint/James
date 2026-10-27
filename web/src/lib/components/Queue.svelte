<script lang="ts">
  import { api } from '$lib/api';
  import { confirmAction } from '$lib/confirm.svelte';
  import { formatDuration, trackLabel } from '$lib/format';
  import { t } from '$lib/i18n/index.svelte';
  import { live } from '$lib/live.svelte';
  import { can, reportError, toast } from '$lib/session.svelte';
  import type { QueueItem } from '$lib/types';
  import { tick } from 'svelte';
  import Icon from './Icon.svelte';

  let { id, items }: { id: string; items: QueueItem[] } = $props();

  let manage = $derived(can('queue.manage', id));
  let control = $derived(can('playback.control', id));
  // Undo re-adds the track, which needs `playback.play`; while idle it would start playback.
  let canUndo = $derived(can('playback.play', id) && (live.instances[id]?.status?.current ?? null) !== null);
  let total = $derived(items.reduce((sum, i) => sum + (i.track.durationMs ?? 0), 0));
  /** The last removed item and its position, for undo. */
  let removed = $state<{ item: QueueItem; index: number } | null>(null);
  let undoTimer: ReturnType<typeof setTimeout> | undefined;

  async function run(action: () => Promise<unknown>) {
    try {
      await action();
    } catch (e) {
      reportError(e);
    }
  }

  async function clear() {
    if (await confirmAction(t('queue.clearConfirm', { count: items.length }), t('queue.clear'))) {
      await run(() => api.clearQueue(id));
    }
  }

  let list = $state<HTMLElement>();
  let undoButton = $state<HTMLButtonElement>();

  /**
   * Keeps keyboard focus on the moved item: rows are keyed by item id, so after the update
   * the same button exists at the new position (or its sibling if that one became disabled).
   */
  async function move(item: QueueItem, to: number, direction: 'up' | 'down') {
    try {
      await api.moveQueueItem(id, item.id, to);
    } catch (e) {
      reportError(e);
      return;
    }
    // Wait for the live queue update to re-render the rows.
    for (let i = 0; i < 20 && items.findIndex((x) => x.id === item.id) !== to; i++) {
      await new Promise((r) => setTimeout(r, 25));
    }
    await tick();
    const row = list?.querySelector<HTMLElement>(`[data-item="${item.id}"]`);
    const preferred = row?.querySelector<HTMLButtonElement>(`[data-action="${direction}"]`);
    const fallback = row?.querySelector<HTMLButtonElement>(`[data-action="${direction === 'up' ? 'down' : 'up'}"]`);
    (preferred && !preferred.disabled ? preferred : fallback)?.focus();
  }

  async function remove(item: QueueItem, index: number) {
    try {
      await api.removeQueueItem(id, item.id);
      removed = { item, index };
      clearTimeout(undoTimer);
      undoTimer = setTimeout(() => (removed = null), 10000);
      // The focused button disappears with its row: continue at the Undo button.
      await tick();
      if (canUndo) undoButton?.focus();
      else list?.querySelector<HTMLButtonElement>('button:not(:disabled)')?.focus();
    } catch (e) {
      reportError(e);
    }
  }

  async function undo() {
    if (!removed) return;
    const { item, index } = removed;
    removed = null;
    await run(async () => {
      const [restored] = await api.addTracks(id, [item.track], 'append');
      await api.moveQueueItem(id, restored.id, index);
    });
  }

  async function playNow(item: QueueItem) {
    await run(async () => {
      await api.playQueueItem(id, item.id);
      toast(t('queue.nowPlaying', { title: item.track.title }), 'success');
    });
  }
</script>

<section class="card" aria-labelledby="queue-title">
  <div class="card-header">
    <h2 id="queue-title">{t('queue.title')}</h2>
    {#if items.length > 0}
      <span class="muted small summary">{t('queue.count', { count: items.length, duration: formatDuration(total) })}</span>
    {/if}
    {#if manage && items.length > 1}
      <button class="icon-btn small" onclick={() => run(() => api.shuffleQueue(id))} aria-label={t('queue.shuffle')} title={t('queue.shuffle')}>
        <Icon name="shuffle" size={16} />
      </button>
    {/if}
    {#if manage && items.length > 0}
      <button class="btn btn-small btn-danger" onclick={clear}>{t('queue.clear')}</button>
    {/if}
  </div>

  {#if removed && canUndo}
    <div class="undo" role="status">
      <span class="truncate">{t('queue.removed', { title: removed.item.track.title })}</span>
      <button class="btn btn-small" onclick={undo} bind:this={undoButton}>{t('common.undo')}</button>
    </div>
  {/if}

  {#if items.length === 0}
    <p class="muted empty-queue">{t('queue.empty')}</p>
  {:else}
    <ol class="list queue" bind:this={list}>
      {#each items as item, index (item.id)}
        <li data-item={item.id}>
          <span class="position muted" aria-hidden="true">{index + 1}</span>
          <div class="meta">
            <span class="truncate" title={trackLabel(item.track)}>{item.track.title}</span>
            <span class="truncate muted small">
              {[item.track.artist, item.requestedBy?.name].filter(Boolean).join(' · ')}
            </span>
          </div>
          <span class="muted small duration">{item.track.isStream ? t('player.live') : formatDuration(item.track.durationMs)}</span>
          <div class="actions">
            {#if control}
              <button class="icon-btn small" onclick={() => playNow(item)} aria-label="{t('queue.playThis')}: {item.track.title}" title={t('queue.playThis')}>
                <Icon name="play" size={16} />
              </button>
            {/if}
            {#if manage}
              <button
                class="icon-btn small"
                data-action="up"
                disabled={index === 0}
                onclick={() => move(item, index - 1, 'up')}
                aria-label="{t('queue.moveUp')}: {item.track.title}"
                title={t('queue.moveUp')}
              >
                <Icon name="up" size={16} />
              </button>
              <button
                class="icon-btn small"
                data-action="down"
                disabled={index === items.length - 1}
                onclick={() => move(item, index + 1, 'down')}
                aria-label="{t('queue.moveDown')}: {item.track.title}"
                title={t('queue.moveDown')}
              >
                <Icon name="down" size={16} />
              </button>
              <button class="icon-btn small" onclick={() => remove(item, index)} aria-label="{t('queue.remove')}: {item.track.title}" title={t('queue.remove')}>
                <Icon name="trash" size={16} />
              </button>
            {/if}
          </div>
        </li>
      {/each}
    </ol>
  {/if}
</section>

<style>
  .queue {
    max-height: 560px;
    overflow-y: auto;
  }
  .position {
    width: 2em;
    text-align: right;
    font-variant-numeric: tabular-nums;
    font-size: var(--text-sm);
    flex-shrink: 0;
  }
  .meta {
    display: flex;
    flex-direction: column;
    min-width: 0;
    flex: 1;
  }
  .duration {
    font-variant-numeric: tabular-nums;
    flex-shrink: 0;
  }
  .actions {
    display: flex;
    flex-shrink: 0;
  }
  .empty-queue {
    padding: var(--space-4) 0;
    text-align: center;
  }
  .undo {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    padding: var(--space-2) var(--space-3);
    margin-bottom: var(--space-2);
    border-radius: var(--radius);
    background: var(--surface-2);
    font-size: var(--text-sm);
  }
  .undo span {
    flex: 1;
  }
  .summary {
    white-space: nowrap;
  }
  .card-header {
    flex-wrap: wrap;
  }
  /* Phones: actions get their own line so the title keeps the full width. */
  @media (max-width: 600px) {
    .queue li {
      flex-wrap: wrap;
    }
    .meta {
      flex-basis: calc(100% - 7em);
    }
    .actions {
      width: 100%;
      justify-content: flex-end;
      margin-top: calc(-1 * var(--space-1));
    }
  }
</style>
