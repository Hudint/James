<script lang="ts">
  // A track setting: a URL typed in, or a track chosen from the library (stored as `library:<id>`).
  import { tick } from 'svelte';
  import { api, ApiError } from '$lib/api';
  import { formatDuration } from '$lib/format';
  import { t } from '$lib/i18n/index.svelte';
  import { can, errorMessage } from '$lib/session.svelte';
  import { trackTitles } from '$lib/trackTitles.svelte';
  import type { LibraryTrack } from '$lib/types';
  import Icon from './Icon.svelte';

  const LIMIT = 50;

  let {
    id,
    value,
    placeholder,
    onchange,
  }: {
    /** Id of the input; the field's label is `<id>-label`. */
    id: string;
    value: string;
    placeholder?: string;
    onchange: (value: string) => void;
  } = $props();

  const libraryId = $derived(/^library:([1-9]\d*)$/i.exec(value)?.[1] ?? null);
  /** The chosen library track: `null` while loading, `'missing'` / `'failed'` if it cannot be shown. */
  let chosen = $state<LibraryTrack | null | 'missing' | 'failed'>(null);
  function load(wanted: string) {
    chosen = null;
    api.libraryTrack(Number(wanted)).then(
      (track) => {
        if (libraryId !== wanted) return;
        chosen = track;
        trackTitles[wanted] = track.artist ? `${track.title} · ${track.artist}` : track.title;
      },
      (e) => libraryId === wanted && (chosen = e instanceof ApiError && e.status === 404 ? 'missing' : 'failed'),
    );
  }
  $effect(() => {
    if (libraryId) load(libraryId);
  });

  let dialog = $state<HTMLDialogElement>();
  let search = $state<HTMLInputElement>();
  let input = $state<HTMLInputElement>();
  let query = $state('');
  let results = $state<LibraryTrack[] | null>(null);
  let total = $state(0);
  let searching = $state(false);
  let failed = $state('');
  /** Announced to screen readers: the chosen track (outside the dialog) and result counts
   * (inside it: everything outside a modal dialog is inert). */
  let status = $state('');
  let found = $state('');
  let opener: HTMLElement | null = null;
  let request = 0;

  async function find() {
    const mine = ++request;
    searching = true;
    try {
      const found = await api.library(query, LIMIT, 0);
      if (mine !== request) return;
      results = found.tracks;
      total = found.total;
      failed = '';
      foundStatus(found.total === 0 ? t('player.noResults') : t('library.total', { count: found.total }));
    } catch (e) {
      if (mine !== request) return;
      results = [];
      failed = errorMessage(e);
      foundStatus(failed);
    } finally {
      if (mine === request) searching = false;
    }
  }

  function foundStatus(text: string) {
    found = text;
  }

  let timer: ReturnType<typeof setTimeout> | undefined;
  function onQuery() {
    clearTimeout(timer);
    searching = true;
    timer = setTimeout(() => void find(), 250);
  }

  async function open(event: MouseEvent) {
    opener = event.currentTarget as HTMLElement;
    query = '';
    results = null;
    failed = '';
    status = '';
    found = '';
    dialog?.showModal();
    await tick();
    search?.focus();
    void find();
  }

  /** The dialog sits inside the settings form: Enter must not submit (save) it. */
  function onSearchKey(event: KeyboardEvent) {
    if (event.key !== 'Enter') return;
    event.preventDefault();
    clearTimeout(timer);
    if (results?.length === 1 && !searching) choose(results[0]!);
    else void find();
  }

  function closed() {
    clearTimeout(timer);
    request++;
    opener?.focus();
  }

  function choose(track: LibraryTrack) {
    trackTitles[String(track.id)] = track.artist ? `${track.title} · ${track.artist}` : track.title;
    onchange(`library:${track.id}`);
    status = t('plugins.track.selected', { title: track.title });
    dialog?.close();
  }

  async function clear() {
    onchange('');
    await tick();
    input?.focus();
  }
</script>

<div class="picker">
  {#if libraryId}
    <div class="chosen" id={id} role="group" aria-labelledby="{id}-label {id}-chosen" aria-describedby="{id}-hint">
      <Icon name="music" size={16} />
      <span class="title" id="{id}-chosen">
        {#if chosen === 'missing'}
          <span class="error-text">{t('plugins.track.missing', { id: libraryId })}</span>
        {:else if chosen === 'failed'}
          <span class="error-text">{t('plugins.track.failed')}</span>
        {:else if chosen}
          <span class="name">{chosen.title}</span>{#if chosen.artist}<span class="muted artist">{chosen.artist}</span>{/if}
        {:else}
          <span class="muted">{t('common.loading')}</span>
        {/if}
      </span>
      {#if chosen === 'failed'}
        <button type="button" class="link-btn" onclick={() => libraryId && load(libraryId)}>{t('common.retry')}</button>
      {/if}
      <button type="button" class="icon-btn small" onclick={clear} aria-label={t('plugins.track.clear')} title={t('plugins.track.clear')}>
        <Icon name="x" size={16} />
      </button>
    </div>
  {:else}
    <input {id} bind:this={input} type="text" {placeholder} {value} oninput={(e) => onchange(e.currentTarget.value)} aria-describedby="{id}-hint" />
  {/if}
  {#if can('library.read')}
    <button type="button" class="btn choose" onclick={open} aria-describedby="{id}-hint"><Icon name="library" size={16} />{t('plugins.track.choose')}</button>
  {/if}
</div>
<span class="hint" id="{id}-hint">{t('plugins.trackHint')}</span>
<p class="visually-hidden" role="status">{status}</p>

<dialog bind:this={dialog} class="modal wide" aria-labelledby="{id}-dialog-title" onclose={closed}>
  <h2 id="{id}-dialog-title">{t('plugins.track.choose')}</h2>
  <label class="visually-hidden" for="{id}-search">{t('common.search')}</label>
  <input
    id="{id}-search"
    bind:this={search}
    type="search"
    placeholder={t('library.search')}
    bind:value={query}
    oninput={onQuery}
    onkeydown={onSearchKey}
    aria-controls="{id}-results"
  />
  <div class="results" id="{id}-results" aria-busy={searching} class:stale={searching && results !== null}>
    {#if failed}
      <p class="error-text">{failed}</p>
    {:else if results === null}
      <p class="muted">{t('player.searching')}</p>
    {:else if results.length === 0}
      <p class="muted">{query.trim() ? t('player.noResults') : t('library.emptyHint')}</p>
    {:else}
      <ul>
        {#each results as track (track.id)}
          {@const current = String(track.id) === libraryId}
          <li>
            <button type="button" class="result" class:current aria-current={current ? 'true' : undefined} onclick={() => choose(track)} title={track.path.split('/').pop()}>
              <span class="title">{#if current}<Icon name="check" size={14} />{/if}{track.title}</span>
              <span class="muted small">{[track.artist, track.album, track.path.split('/').pop()].filter(Boolean).join(' · ')}</span>
              <span class="muted small duration">{formatDuration(track.durationMs)}</span>
            </button>
          </li>
        {/each}
      </ul>
      {#if total > results.length}
        <p class="muted small more">{t('plugins.track.more', { shown: results.length, total })}</p>
      {/if}
    {/if}
  </div>
  <p class="visually-hidden" role="status">{found}</p>
  <div class="actions">
    <button type="button" class="btn" onclick={() => dialog?.close()}>{t('common.cancel')}</button>
  </div>
</dialog>

<style>
  .picker {
    display: flex;
    gap: var(--space-2);
    flex-wrap: wrap;
  }
  .picker > input,
  .chosen {
    flex: 1 1 220px;
    min-width: 0;
  }
  .chosen {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-1) var(--space-2) var(--space-1) var(--space-3);
    min-height: 40px;
    border: 1px solid var(--input-border);
    border-radius: var(--radius);
    background: var(--surface);
  }
  .chosen .title {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-wrap: wrap;
    column-gap: var(--space-2);
    overflow-wrap: anywhere;
  }
  .link-btn {
    padding: 0;
    border: 0;
    background: none;
    color: var(--accent);
    font: inherit;
    font-weight: 600;
    text-decoration: underline;
    cursor: pointer;
  }
  .results {
    margin-top: var(--space-3);
    padding: 2px;
    max-height: min(50dvh, 420px);
    overflow-y: auto;
  }
  .results.stale {
    opacity: 0.6;
  }
  .results ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .result {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    gap: 0 var(--space-3);
    width: 100%;
    padding: var(--space-2) var(--space-3);
    border: 0;
    border-left: 3px solid transparent;
    border-radius: var(--radius);
    background: none;
    color: inherit;
    font: inherit;
    text-align: left;
    cursor: pointer;
  }
  .result:hover,
  .result:focus-visible {
    background: color-mix(in srgb, var(--accent-soft) 60%, transparent);
  }
  .result.current {
    border-left-color: var(--accent);
  }
  .result .title {
    display: inline-flex;
    align-items: center;
    gap: var(--space-1);
    font-weight: 600;
    overflow-wrap: anywhere;
  }
  .result .duration {
    grid-row: 1 / span 2;
    grid-column: 2;
    align-self: center;
    font-variant-numeric: tabular-nums;
  }
  .more {
    margin: var(--space-2) var(--space-3) 0;
  }
  @media (max-width: 430px) {
    .choose {
      width: 100%;
      justify-content: center;
    }
  }
</style>
