<script lang="ts">
  import { api } from '$lib/api';
  import { formatDuration } from '$lib/format';
  import { i18n, t } from '$lib/i18n/index.svelte';
  import { positionOf } from '$lib/live.svelte';
  import { can, reportError } from '$lib/session.svelte';
  import type { InstanceStatus, RepeatMode } from '$lib/types';
  import Icon from './Icon.svelte';
  import { canSave, savable, saveToLibrary, saving } from '$lib/saving.svelte';

  let { id, status }: { id: string; status: InstanceStatus } = $props();

  let now = $state(performance.now());
  $effect(() => {
    const timer = setInterval(() => (now = performance.now()), 250);
    return () => clearInterval(timer);
  });

  let track = $derived(status.current?.track ?? null);
  let duration = $derived(track?.durationMs ?? null);
  let position = $derived(positionOf(id, now));
  let control = $derived(can('playback.control', id));
  let playing = $derived(!!status.current && !status.paused);
  const percent = (value: number) => new Intl.NumberFormat(i18n.locale, { style: 'percent' }).format(value / 100);
  let canVolume = $derived(can('playback.volume', id));
  let canShuffle = $derived(can('queue.manage', id));

  // While dragging, the slider shows the drag position instead of the live one.
  let seeking = $state<number | null>(null);
  let volume = $state(0);
  let volumeDirty = false;
  $effect(() => {
    if (!volumeDirty) volume = status.volume;
  });

  async function run(action: () => Promise<unknown>) {
    try {
      await action();
    } catch (e) {
      reportError(e);
    }
  }

  let volumeTimer: ReturnType<typeof setTimeout> | undefined;
  function onVolume() {
    volumeDirty = true;
    clearTimeout(volumeTimer);
    volumeTimer = setTimeout(async () => {
      await run(() => api.volume(id, volume));
      volumeDirty = false;
    }, 120);
  }

  function commitSeek() {
    if (seeking !== null) {
      const target = seeking;
      seeking = null;
      void run(() => api.seek(id, target));
    }
  }

  const nextRepeat: Record<RepeatMode, RepeatMode> = { off: 'all', all: 'one', one: 'off' };
  let repeatLabel = $derived(
    status.repeat === 'one' ? t('player.repeatOne') : status.repeat === 'all' ? t('player.repeatAll') : t('player.repeatOff'),
  );
</script>

<section class="card player" aria-labelledby="now-playing-title">
  <div class="art" aria-hidden="true">
    {#if track?.thumbnail}
      <img src={track.thumbnail} alt="" referrerpolicy="no-referrer" />
    {:else if track}
      <span class="meter big" class:still={!playing}><i></i><i></i><i></i></span>
    {:else}
      <Icon name="music" size={36} />
    {/if}
  </div>

  <div class="info">
    <h2 id="now-playing-title" class="label muted">{t('player.nowPlaying')}</h2>
    {#if track}
      <p class="title clamp" title={track.title}>{track.title}</p>
      {#if track.artist}<p class="artist truncate muted">{track.artist}</p>{/if}
      {#if status.loading || status.current?.requestedBy}
        <p class="requested muted small" aria-live="polite">
          {#if status.loading}<span class="badge">{t('player.loading')}</span>{/if}
          {#if status.current?.requestedBy}<span class="truncate">{t('player.requestedBy', { name: status.current.requestedBy.name })}</span>{/if}
        </p>
      {/if}
      {#if savable(track) && canSave()}
        {@const busy = saving.inputs.includes(track.locator)}
        {@const done = saving.done.includes(track.locator)}
        <!-- aria-disabled, not disabled: keyboard focus stays on the button meanwhile. -->
        <button
          class="btn btn-small btn-ghost flush save"
          onclick={() => !busy && !done && saveToLibrary(track.locator)}
          aria-disabled={busy || done}
          aria-busy={busy}
        >
          <Icon name={done ? 'check' : 'download'} size={16} />{done ? t('library.inLibrary') : busy ? t('library.savingShort') : t('library.save')}
        </button>
      {/if}
    {:else}
      <p class="title">{t('player.nothing')}</p>
      <p class="muted small">{t('player.nothingHint')}</p>
    {/if}
  </div>

  <div class="progress">
    {#if status.loading && !track}
      <p class="muted small" aria-live="polite">{t('player.loading')}</p>
    {:else if track && track.isStream}
      <span class="badge danger"><span class="dot"></span>{t('player.live')}</span>
      <span class="muted small">{formatDuration(position)}</span>
    {:else if track}
      <span class="time">{formatDuration(seeking ?? position)}</span>
      <input
        type="range"
        min="0"
        max={duration ?? 0}
        step="1000"
        value={seeking ?? position}
        style:--p="{duration ? Math.min(100, ((seeking ?? Math.round(position / 1000) * 1000) / duration) * 100) : 0}%"
        disabled={!control || !duration}
        aria-label={t('player.seek')}
        aria-valuetext={formatDuration(seeking ?? position)}
        oninput={(e) => (seeking = Number((e.target as HTMLInputElement).value))}
        onchange={commitSeek}
      />
      <span class="time">{formatDuration(duration)}</span>
    {/if}
  </div>

  <div class="controls">
    <button
      class="icon-btn"
      class:active={status.shuffle}
      disabled={!canShuffle}
      onclick={() => run(() => api.shuffle(id, !status.shuffle))}
      aria-label={t('player.shuffle')}
      aria-pressed={status.shuffle}
      title={t('player.shuffle')}
    >
      <Icon name="shuffle" />
    </button>
    <button class="icon-btn" disabled={!control} onclick={() => run(() => api.previous(id))} aria-label={t('player.previous')} title={t('player.previous')}>
      <Icon name="previous" />
    </button>
    <!-- One toggle button (not two swapping ones) so keyboard focus stays on it. -->
    <button
      class="icon-btn large"
      disabled={!control || !status.current}
      onclick={() => run(() => (playing ? api.pause(id) : api.resume(id)))}
      aria-label={playing ? t('player.pause') : t('player.resume')}
      title={playing ? t('player.pause') : t('player.resume')}
    >
      <Icon name={playing ? 'pause' : 'play'} size={26} />
    </button>
    <button class="icon-btn" disabled={!control || (!status.current && status.queueLength === 0)} onclick={() => run(() => api.skip(id))} aria-label={t('player.next')} title={t('player.next')}>
      <Icon name="next" />
    </button>
    <button
      class="icon-btn"
      class:active={status.repeat !== 'off'}
      disabled={!control}
      onclick={() => run(() => api.repeat(id, nextRepeat[status.repeat]))}
      aria-label={repeatLabel}
      title={repeatLabel}
    >
      <Icon name={status.repeat === 'one' ? 'repeatOne' : 'repeat'} />
    </button>
    <button class="icon-btn stop" disabled={!control || !status.current} onclick={() => run(() => api.stopPlayback(id))} aria-label={t('player.stop')} title={t('player.stop')}>
      <Icon name="stop" />
    </button>
  </div>

  <div class="volume">
    <Icon name={volume === 0 ? 'volumeMute' : 'volume'} size={18} />
    <input
      type="range"
      min="0"
      max="100"
      bind:value={volume}
      style:--p="{volume}%"
      oninput={onVolume}
      disabled={!canVolume}
      aria-label={t('player.volume')}
      aria-valuetext={percent(volume)}
    />
    <span class="small muted vol-value">{percent(volume)}</span>
  </div>
</section>

<style>
  .player {
    display: grid;
    grid-template-columns: 96px 1fr;
    grid-template-areas:
      'art info'
      'progress progress'
      'controls controls'
      'volume volume';
    gap: var(--space-4);
    align-items: center;
  }
  .art {
    grid-area: art;
    width: 96px;
    height: 96px;
    border-radius: var(--radius-lg);
    background: var(--side-bg);
    color: var(--side-muted);
    display: grid;
    place-items: center;
    overflow: hidden;
  }
  .meter.big {
    width: 40px;
    height: 36px;
    gap: 5px;
  }
  .art img {
    width: 100%;
    height: 100%;
    object-fit: cover;
  }
  .info {
    grid-area: info;
    min-width: 0;
  }
  .label {
    font-size: var(--text-sm);
    font-weight: 500;
    margin-bottom: var(--space-1);
  }
  .title {
    font-size: var(--text-xl);
    font-weight: 650;
  }
  /* Long titles get two lines before they are cut. */
  .clamp {
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
    overflow-wrap: anywhere;
  }
  .save {
    margin-top: var(--space-2);
  }
  .requested {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-1) var(--space-2);
    min-width: 0;
  }
  .requested .truncate {
    min-width: 0;
  }
  .progress {
    grid-area: progress;
    display: flex;
    align-items: center;
    gap: var(--space-3);
    min-height: 24px;
  }
  .time {
    font-family: var(--mono);
    font-variant-numeric: tabular-nums;
    font-size: var(--text-sm);
    color: var(--text-muted);
    min-width: 4em;
    text-align: center;
  }
  .controls {
    grid-area: controls;
    display: flex;
    justify-content: center;
    align-items: center;
    gap: var(--space-2);
  }
  .volume {
    grid-area: volume;
    display: flex;
    align-items: center;
    gap: var(--space-3);
    color: var(--text-muted);
    max-width: 320px;
    width: 100%;
    justify-self: center;
  }
  .vol-value {
    min-width: 3.5em;
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  /* Phones: smaller cover, controls spread across the width within thumb reach. */
  @media (max-width: 600px) {
    .player {
      grid-template-columns: 72px 1fr;
      gap: var(--space-3);
    }
    .art {
      width: 72px;
      height: 72px;
    }
    .title {
      font-size: var(--text-lg);
    }
    .controls {
      justify-content: space-between;
      gap: 0;
    }
    .controls :global(.icon-btn:not(.large)) {
      width: clamp(40px, 12vw, 48px);
      height: clamp(40px, 12vw, 48px);
    }
    .volume {
      max-width: none;
    }
  }
  @media (min-width: 720px) {
    .player {
      grid-template-columns: 128px 1fr;
    }
    .art {
      width: 128px;
      height: 128px;
    }
  }
</style>
