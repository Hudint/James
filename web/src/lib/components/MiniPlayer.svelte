<script lang="ts">
  // Phones: a bar at the bottom with the current track and the main controls, shown while
  // the full player is scrolled out of view (e.g. while working on the queue).
  import { api } from '$lib/api';
  import { t } from '$lib/i18n/index.svelte';
  import { can, reportError } from '$lib/session.svelte';
  import type { InstanceStatus } from '$lib/types';
  import Icon from './Icon.svelte';

  let {
    id,
    status,
    player,
    always = false,
    onshowplayer,
  }: {
    id: string;
    status: InstanceStatus;
    player: HTMLElement | null;
    /** Shown regardless of the player (it is on another tab). */
    always?: boolean;
    /** Brings the full player into view (e.g. switches the tab). */
    onshowplayer?: () => void;
  } = $props();

  let hidden = $state(false);
  $effect(() => {
    // Not there yet (first render) or on another tab (`always` decides then).
    if (!player) return;
    // The sticky top bar (56 px) covers the top of the page.
    const observer = new IntersectionObserver(([entry]) => (hidden = !entry.isIntersecting), {
      threshold: 0,
      rootMargin: '-56px 0px 0px 0px',
    });
    observer.observe(player);
    return () => observer.disconnect();
  });

  let track = $derived(status.current?.track ?? null);
  let playing = $derived(!!status.current && !status.paused);
  let control = $derived(can('playback.control', id));
  let visible = $derived((always || hidden) && track !== null);

  // The page and toasts keep clear of the bar: its real height (incl. the safe area).
  let bar = $state<HTMLElement | null>(null);
  $effect(() => {
    const root = document.documentElement.style;
    if (!bar) {
      root.removeProperty('--bottom-bar');
      return;
    }
    const element = bar;
    const observer = new ResizeObserver(() => root.setProperty('--bottom-bar', `${element.offsetHeight}px`));
    observer.observe(element);
    return () => {
      observer.disconnect();
      root.removeProperty('--bottom-bar');
    };
  });

  function showPlayer() {
    // The page takes over (e.g. switches the tab and moves focus there).
    if (onshowplayer) return onshowplayer();
    const smooth = !matchMedia('(prefers-reduced-motion: reduce)').matches;
    player?.scrollIntoView({ behavior: smooth ? 'smooth' : 'auto', block: 'start' });
    // The bar disappears: keep keyboard focus on the player.
    player?.querySelector<HTMLElement>('button:not(:disabled)')?.focus({ preventScroll: true });
  }

  async function run(action: () => Promise<unknown>) {
    try {
      await action();
    } catch (e) {
      reportError(e);
    }
  }
</script>

{#if visible && track}
  <div class="mini" role="region" aria-label={t('player.nowPlaying')} bind:this={bar}>
    {#if track.thumbnail}
      <img src={track.thumbnail} alt="" referrerpolicy="no-referrer" />
    {:else}
      <span class="art" aria-hidden="true"><Icon name={track.isStream ? 'radio' : 'music'} size={20} /></span>
    {/if}
    <button class="meta" type="button" onclick={showPlayer}>
      <span class="truncate">{track.title}</span>
      {#if track.artist}<span class="truncate muted small">{track.artist}</span>{/if}
    </button>
    {#if control}
      <button class="icon-btn" onclick={() => run(() => (playing ? api.pause(id) : api.resume(id)))} aria-label={playing ? t('player.pause') : t('player.resume')}>
        <Icon name={playing ? 'pause' : 'play'} />
      </button>
      <button class="icon-btn" onclick={() => run(() => api.skip(id))} aria-label={t('player.next')}>
        <Icon name="next" />
      </button>
    {/if}
  </div>
{/if}

<style>
  .mini {
    position: fixed;
    left: 0;
    right: 0;
    bottom: 0;
    z-index: 20;
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-2) max(var(--space-3), env(safe-area-inset-right)) calc(var(--space-2) + env(safe-area-inset-bottom))
      max(var(--space-3), env(safe-area-inset-left));
    background: var(--surface);
    border-top: 1px solid var(--border);
    box-shadow: var(--shadow);
  }
  img,
  .art {
    width: 40px;
    height: 40px;
    border-radius: var(--radius-sm);
    object-fit: cover;
    flex-shrink: 0;
  }
  .art {
    display: grid;
    place-items: center;
    background: var(--accent-soft);
    color: var(--accent);
  }
  .meta {
    display: flex;
    flex-direction: column;
    min-width: 0;
    flex: 1;
    border: none;
    background: none;
    color: inherit;
    font: inherit;
    text-align: left;
    padding: 0;
    cursor: pointer;
  }
</style>
