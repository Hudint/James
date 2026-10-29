<script lang="ts">
  import { onMount } from 'svelte';
  import { api } from '$lib/api';
  import Icon from '$lib/components/Icon.svelte';
  import InstancePicker from '$lib/components/InstancePicker.svelte';
  import { formatDuration } from '$lib/format';
  import { t } from '$lib/i18n/index.svelte';
  import { can, reportError, toast } from '$lib/session.svelte';
  import type { LibraryTrack } from '$lib/types';

  const PAGE = 100;
  let query = $state('');
  let tracks = $state<LibraryTrack[] | null>(null);
  let total = $state(0);
  let hasMore = $state(false);
  let target = $state('');
  let uploading = $state(false);
  let scanning = $state(false);
  let fileInput = $state<HTMLInputElement>();

  let searchId = 0;
  async function search(append = false) {
    const request = ++searchId;
    try {
      const offset = append ? (tracks?.length ?? 0) : 0;
      const result = await api.library(query, PAGE, offset);
      if (request !== searchId) return; // a newer search is running
      tracks = append ? [...(tracks ?? []), ...result.tracks] : result.tracks;
      total = result.total;
      hasMore = result.tracks.length === PAGE;
    } catch (e) {
      reportError(e);
      tracks ??= [];
    }
  }

  onMount(() => search());

  let timer: ReturnType<typeof setTimeout> | undefined;
  function onQuery() {
    clearTimeout(timer);
    timer = setTimeout(() => search(), 250);
  }

  async function play(track: LibraryTrack, mode: 'now' | 'append') {
    if (!target) return;
    try {
      await api.play(target, track.path, mode);
      toast(t('player.added', { title: track.title }), 'success');
    } catch (e) {
      reportError(e);
    }
  }

  let dragging = $state(false);

  function onDrop(event: DragEvent) {
    event.preventDefault();
    dragging = false;
    if (event.dataTransfer?.files.length) void upload(event.dataTransfer.files);
  }

  async function upload(dropped?: FileList) {
    const files = dropped ?? fileInput?.files;
    if (!files || files.length === 0) return;
    uploading = true;
    try {
      const result = await api.upload(files);
      toast(t('library.uploaded', { count: result.files.length }), 'success');
      await search();
    } catch (e) {
      reportError(e);
    } finally {
      uploading = false;
      if (fileInput) fileInput.value = "";
    }
  }

  async function scan() {
    scanning = true;
    try {
      await api.scanLibrary();
      toast(t('library.scanStarted'), 'success');
      // Pick up the first results; the scan continues in the background.
      setTimeout(() => void search(), 3000);
    } catch (e) {
      reportError(e);
    } finally {
      scanning = false;
    }
  }
</script>

<svelte:head><title>{t('library.title')} · James</title></svelte:head>

<svelte:window
  ondragover={(e) => {
    if (can('library.manage') && e.dataTransfer?.types.includes('Files')) {
      e.preventDefault();
      dragging = true;
    }
  }}
  ondragleave={(e) => {
    if (!e.relatedTarget) dragging = false;
  }}
  ondrop={(e) => can('library.manage') && onDrop(e)}
/>

{#if dragging}
  <div class="dropzone" aria-hidden="true"><Icon name="upload" size={40} />{t('library.dropHint')}</div>
{/if}

<div class="page">
  <header class="page-header">
    <h1>{t('library.title')}</h1>
    {#if can('library.manage')}
      <input bind:this={fileInput} type="file" accept="audio/*" multiple class="visually-hidden" id="upload" onchange={() => upload()} />
      <label for="upload" class="btn" class:disabled={uploading}><Icon name="upload" size={16} />{uploading ? t('library.uploading') : t('library.upload')}</label>
      <button class="btn rescan" onclick={scan} disabled={scanning} aria-label={t('library.scan')} title={t('library.scan')}>
        <Icon name="refresh" size={16} /><span class="label-text">{t('library.scan')}</span>
      </button>
    {/if}
  </header>

  <div class="card toolbar">
    <div class="search">
      <Icon name="search" size={18} />
      <label class="visually-hidden" for="library-search">{t('common.search')}</label>
      <input id="library-search" type="search" placeholder={t('library.search')} bind:value={query} oninput={onQuery} />
    </div>
    <div class="target">
      <span class="small muted nowrap">{t('library.playOn')}</span>
      <InstancePicker bind:value={target} />
    </div>
  </div>

  {#if tracks === null}
    <p class="muted" aria-busy="true">{t('common.loading')}</p>
  {:else if tracks.length === 0}
    <div class="card empty">
      <Icon name="library" size={32} />
      <p>{t('library.empty')}</p>
      <p class="small">{t('library.emptyHint')}</p>
    </div>
  {:else}
    <div class="card">
      <p class="muted small count">{t('library.total', { count: total })}</p>
      <div class="table-wrap">
        <table>
          <thead>
            <tr>
              <th scope="col"><span class="visually-hidden">{t('common.play')}</span></th>
              <th scope="col">{t('library.columns.title')}</th>
              <th scope="col" class="hide-sm">{t('library.columns.artist')}</th>
              <th scope="col" class="hide-md">{t('library.columns.album')}</th>
              <th scope="col" class="num">{t('library.columns.duration')}</th>
              <th scope="col"><span class="visually-hidden">{t('player.addToQueue')}</span></th>
            </tr>
          </thead>
          <tbody>
            {#each tracks as track (track.id)}
              <tr>
                <td class="action">
                  <button class="icon-btn small" disabled={!target} onclick={() => play(track, 'now')} aria-label="{t('player.playNow')}: {track.title}">
                    <Icon name="play" size={16} />
                  </button>
                </td>
                <td>
                  <span class="cell-title">{track.title}</span>
                  <span class="show-sm muted small">{track.artist ?? ''}</span>
                </td>
                <td class="hide-sm">{track.artist ?? '–'}</td>
                <td class="hide-md">{track.album ?? '–'}</td>
                <td class="num muted">{formatDuration(track.durationMs)}</td>
                <td class="action">
                  <button class="icon-btn small" disabled={!target} onclick={() => play(track, 'append')} aria-label="{t('player.addToQueue')}: {track.title}">
                    <Icon name="plus" size={16} />
                  </button>
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
      {#if hasMore}
        <button class="btn more" onclick={() => search(true)}>{t('common.loadMore')}</button>
      {/if}
    </div>
  {/if}
</div>

<style>
  .toolbar {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-3);
    align-items: center;
  }
  .search {
    position: relative;
    flex: 1 1 260px;
    display: flex;
    align-items: center;
  }
  .search :global(svg) {
    position: absolute;
    left: var(--space-3);
    color: var(--text-muted);
    pointer-events: none;
  }
  .search input {
    padding-left: 40px;
  }
  .target {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    flex: 0 1 280px;
  }
  .count {
    margin-bottom: var(--space-2);
  }
  .action {
    width: 40px;
    padding: 0 var(--space-1);
  }
  .num {
    text-align: right;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .cell-title {
    display: block;
    font-weight: 550;
  }
  .show-sm {
    display: none;
  }
  .more {
    margin-top: var(--space-3);
  }
  .dropzone {
    position: fixed;
    inset: 12px;
    z-index: 90;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: var(--space-3);
    border: 3px dashed var(--accent);
    border-radius: var(--radius-lg);
    background: color-mix(in srgb, var(--accent-soft) 90%, transparent);
    color: var(--accent);
    font-size: var(--text-lg);
    font-weight: 600;
    pointer-events: none;
  }
  label.disabled {
    opacity: 0.55;
    pointer-events: none;
  }
  @media (max-width: 900px) {
    .hide-md {
      display: none;
    }
  }
  @media (max-width: 600px) {
    .hide-sm {
      display: none;
    }
    .show-sm {
      display: block;
    }
  }
  /* Phones: upload and rescan stay in one row (rescan as an icon). */
  @media (max-width: 600px) {
    .rescan .label-text {
      display: none;
    }
    .rescan {
      padding: 0 var(--space-3);
    }
  }
</style>
