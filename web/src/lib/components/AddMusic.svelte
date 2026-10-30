<script lang="ts">
  import { rise } from '$lib/motion';
  import { api } from '$lib/api';
  import { formatDuration, trackLabel } from '$lib/format';
  import { t } from '$lib/i18n/index.svelte';
  import { reportError, toast } from '$lib/session.svelte';
  import type { PlayMode, QueueItem, Track } from '$lib/types';
  import { mediaQuery } from '$lib/media.svelte';
  import ActionMenu from './ActionMenu.svelte';
  import Icon from './Icon.svelte';

  // Phones: one row (field, main action, menu); what people type there is mostly a search.
  const phone = mediaQuery('(max-width: 600px)');

  let { id }: { id: string } = $props();

  let input = $state('');
  let busy = $state<PlayMode | 'search' | null>(null);
  let results = $state<Track[] | null>(null);
  /** Results whose thumbnail failed to load (shown with the placeholder instead). */
  let broken = $state(new Set<string>());

  const isUrl = (value: string) => /^[a-z]+:/i.test(value);

  function added(items: QueueItem[]) {
    toast(
      items.length === 1 ? t('player.added', { title: items[0].track.title }) : t('player.addedMany', { count: items.length }),
      'success',
    );
  }

  async function add(mode: PlayMode) {
    const value = input.trim();
    if (!value || busy) return;
    busy = mode;
    try {
      added(await api.play(id, value, mode));
      input = '';
      results = null;
    } catch (e) {
      reportError(e);
    } finally {
      busy = null;
    }
  }

  // Results belong to one instance; switching instances clears them.
  $effect(() => {
    void id;
    results = null;
  });

  async function search() {
    const value = input.trim();
    if (!value || busy) return;
    busy = 'search';
    const forInstance = id;
    try {
      const found = await api.search(forInstance, value, 10);
      if (forInstance === id) results = found;
    } catch (e) {
      reportError(e);
    } finally {
      busy = null;
    }
  }

  async function addResult(track: Track, mode: PlayMode) {
    try {
      added(await api.addTracks(id, [track], mode));
    } catch (e) {
      reportError(e);
    }
  }

  // Enter adds to the queue: in a shared channel replacing the current song must be deliberate.
  // On phones Enter does what the main button shows (search for text, add for links).
  function submit(event: SubmitEvent) {
    event.preventDefault();
    if (phone.matches && !isUrl(input.trim())) void search();
    else void add('append');
  }
</script>

<section class="card add" aria-labelledby="add-label">
  <form class="stack-sm" onsubmit={submit}>
    <label id="add-label" for="add-input" class="label">{t('player.addLabel')}</label>
    {#if phone.matches}
      <div class="compact">
        <div class="input-row">
          <Icon name="search" size={18} />
          <input
            id="add-input"
            type="text"
            bind:value={input}
            placeholder={t('player.addPlaceholderShort')}
            autocomplete="off"
            enterkeyhint={isUrl(input.trim()) ? 'go' : 'search'}
            aria-busy={busy !== null}
          />
        </div>
        {#if isUrl(input.trim())}
          <button class="btn btn-primary" type="submit" disabled={busy !== null} aria-busy={busy !== null} aria-label={t('player.addToQueue')}>
            <span class:spin={busy !== null}><Icon name={busy !== null ? 'refresh' : 'plus'} size={18} /></span>
          </button>
        {:else}
          <button class="btn btn-primary" type="button" onclick={search} disabled={!input.trim() || busy !== null} aria-busy={busy === 'search'} aria-label={busy === 'search' ? t('player.searching') : t('common.search')}>
            <span class:spin={busy !== null}><Icon name={busy !== null ? 'refresh' : 'search'} size={18} /></span>
          </button>
        {/if}
        <ActionMenu
          label={t('common.moreActions')}
          title={input.trim() || t('player.addLabel')}
          items={[
            { label: t('player.addToQueue'), icon: 'plus', disabled: !input.trim() || busy !== null, onclick: () => add('append') },
            { label: t('player.playNext'), icon: 'next', disabled: !input.trim() || busy !== null, onclick: () => add('next') },
            { label: t('player.playNow'), icon: 'play', disabled: !input.trim() || busy !== null, onclick: () => add('now') },
          ]}
        />
      </div>
    {:else}
    <div class="input-row">
      <Icon name="search" size={18} />
      <input
        id="add-input"
        type="text"
        bind:value={input}
        placeholder={t('player.addPlaceholder')}
        autocomplete="off"
        enterkeyhint="go"
        aria-describedby="add-hint"
      />
    </div>
    <p id="add-hint" class="hint">{t('player.searchHint')}</p>
    <div class="row">
      <button class="btn btn-primary btn-small" type="submit" disabled={!input.trim() || busy !== null}>
        <Icon name="plus" size={16} />{busy === 'append' ? t('common.loading') : t('player.addToQueue')}
      </button>
      {#if input.trim()}
        <span class="more-actions" in:rise={{ y: 4 }}>
          <button class="btn btn-small" type="button" onclick={() => add('next')} disabled={busy !== null}>
            {busy === 'next' ? t('common.loading') : t('player.playNext')}
          </button>
          <button class="btn btn-small" type="button" onclick={() => add('now')} disabled={busy !== null}>
            <Icon name="play" size={16} />{busy === 'now' ? t('common.loading') : t('player.playNow')}
          </button>
          {#if !isUrl(input.trim())}
            <button class="btn btn-small btn-ghost" type="button" onclick={search} disabled={busy !== null}>
              <Icon name="search" size={16} />{busy === 'search' ? t('player.searching') : t('common.search')}
            </button>
          {/if}
        </span>
      {/if}
    </div>
    {/if}
  </form>

  {#if results}
    <div class="results" aria-live="polite">
      <div class="results-head">
        <h3>{t('player.searchResults')}</h3>
        <button class="icon-btn small" onclick={() => (results = null)} aria-label={t('common.close')}><Icon name="x" size={16} /></button>
      </div>
      {#if results.length === 0}
        <p class="muted small">{t('player.noResults')}</p>
      {:else}
        <ul class="list">
          {#each results as track, index (`${index}:${track.locator}`)}
            <li>
              {#if track.thumbnail && !broken.has(track.locator)}
                <img
                  class="thumb"
                  src={track.thumbnail}
                  alt=""
                  loading="lazy"
                  referrerpolicy="no-referrer"
                  onerror={() => (broken = new Set([...broken, track.locator]))}
                />
              {:else}
                <span class="thumb placeholder" aria-hidden="true"><Icon name="music" size={18} /></span>
              {/if}
              <div class="meta">
                <span class="truncate" title={trackLabel(track)}>{track.title}</span>
                <span class="truncate muted small">
                  {#if phone.matches}{[track.artist, track.isStream ? t('player.live') : track.durationMs ? formatDuration(track.durationMs) : null].filter(Boolean).join(' · ')}{:else}{track.artist ?? ''}{/if}
                </span>
              </div>
              {#if phone.matches}
                <button class="icon-btn small" onclick={() => addResult(track, 'append')} aria-label="{t('player.addToQueue')}: {track.title}">
                  <Icon name="plus" size={18} />
                </button>
                <ActionMenu
                  small
                  label="{t('common.moreActions')}: {track.title}"
                  title={track.title}
                  items={[
                    { label: t('player.playNow'), icon: 'play', onclick: () => addResult(track, 'now') },
                    { label: t('player.playNext'), icon: 'next', onclick: () => addResult(track, 'next') },
                  ]}
                />
              {:else}
              <span class="muted small duration">{track.isStream ? t('player.live') : formatDuration(track.durationMs)}</span>
              <div class="actions">
                <button class="icon-btn small" onclick={() => addResult(track, 'now')} aria-label="{t('player.playNow')}: {track.title}" title={t('player.playNow')}>
                  <Icon name="play" size={16} />
                </button>
                <button class="icon-btn small" onclick={() => addResult(track, 'next')} aria-label="{t('player.playNext')}: {track.title}" title={t('player.playNext')}>
                  <Icon name="next" size={16} />
                </button>
                <button class="icon-btn small" onclick={() => addResult(track, 'append')} aria-label="{t('player.addToQueue')}: {track.title}" title={t('player.addToQueue')}>
                  <Icon name="plus" size={16} />
                </button>
              </div>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}
    </div>
  {/if}
</section>

<style>
  .more-actions {
    display: inline-flex;
    flex-wrap: wrap;
    gap: var(--space-2);
  }
  .add {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }
  .input-row {
    position: relative;
    display: flex;
    align-items: center;
  }
  .input-row :global(svg) {
    position: absolute;
    left: var(--space-3);
    color: var(--text-muted);
    pointer-events: none;
  }
  .input-row input {
    padding-left: 40px;
  }
  /* Phones: field, main action and menu in one row. */
  .compact {
    display: grid;
    grid-template-columns: 1fr auto auto;
    gap: var(--space-2);
    align-items: center;
  }
  .compact .btn {
    padding: 0 var(--space-3);
  }
  .spin {
    display: inline-grid;
    animation: spin 1s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .spin {
      animation: none;
    }
  }
  /* Phones: result titles get two lines before they are cut. */
  @media (max-width: 600px) {
    .results .meta > :first-child {
      display: -webkit-box;
      -webkit-line-clamp: 2;
      line-clamp: 2;
      -webkit-box-orient: vertical;
      white-space: normal;
      overflow: hidden;
      overflow-wrap: anywhere;
    }
  }
  .results {
    border-top: 1px solid var(--border);
    padding-top: var(--space-3);
  }
  .results-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: var(--space-2);
  }
  .thumb {
    width: 48px;
    height: 36px;
    border-radius: var(--radius-sm);
    object-fit: cover;
    flex-shrink: 0;
  }
  .placeholder {
    display: grid;
    place-items: center;
    background: var(--accent-soft);
    color: var(--accent);
  }
  .meta {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-width: 0;
  }
  .duration {
    font-variant-numeric: tabular-nums;
    flex-shrink: 0;
  }
  .actions {
    display: flex;
    flex-shrink: 0;
  }
</style>
