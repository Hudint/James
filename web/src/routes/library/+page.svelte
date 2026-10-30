<script lang="ts">
  import { onMount, tick } from 'svelte';
  import { api } from '$lib/api';
  import ActionMenu from '$lib/components/ActionMenu.svelte';
  import Icon from '$lib/components/Icon.svelte';
  import InstancePicker from '$lib/components/InstancePicker.svelte';
  import { formatDuration } from '$lib/format';
  import { mediaQuery } from '$lib/media.svelte';
  import { t } from '$lib/i18n/index.svelte';
  import { can, errorMessage, reportError, toast } from '$lib/session.svelte';
  import type { LibraryTrack } from '$lib/types';

  const PAGE = 100;
  // Phones: add-to-queue and editing share a menu, so the title keeps its width.
  const phone = mediaQuery('(max-width: 600px)');
  const compact = $derived(phone.matches && can('library.manage'));
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

  let editDialog = $state<HTMLDialogElement>();
  let editing = $state<LibraryTrack | null>(null);
  let names = $state({ title: '', artist: '', album: '' });
  let savingNames = $state(false);
  let nameError = $state('');
  let titleError = $state('');
  /** Where focus returns when the dialog closes. */
  let editOpener: HTMLElement | null = null;
  let titleInput = $state<HTMLInputElement>();

  const fileNames = $derived(
    editing ? { title: editing.tags.title, artist: editing.tags.artist ?? '', album: editing.tags.album ?? '' } : null,
  );
  const unchanged = $derived(
    !!editing &&
      names.title.trim() === editing.title &&
      names.artist.trim() === (editing.artist ?? '') &&
      names.album.trim() === (editing.album ?? ''),
  );
  /** The fields show the file's tags (saving clears the edited names). */
  const showsFileNames = $derived(
    !!fileNames &&
      names.title.trim() === fileNames.title &&
      names.artist.trim() === fileNames.artist &&
      names.album.trim() === fileNames.album,
  );

  async function edit(track: LibraryTrack, opener: Element | null) {
    editOpener = opener instanceof HTMLElement ? opener : null;
    editing = track;
    names = { title: track.title, artist: track.artist ?? '', album: track.album ?? '' };
    nameError = '';
    titleError = '';
    await tick();
    editDialog?.showModal();
    // On touch screens a focused field would open the keyboard over the dialog at once.
    if (matchMedia('(pointer: fine)').matches) titleInput?.select();
    else document.getElementById('rename-title')?.focus();
  }

  function useFileNames() {
    if (fileNames) names = { ...fileNames };
    titleInput?.focus();
  }

  async function saveNames(event: SubmitEvent) {
    event.preventDefault();
    if (!editing || savingNames) return;
    if (!names.title.trim()) {
      titleError = t('library.titleRequired');
      titleInput?.focus();
      return;
    }
    titleError = '';
    nameError = '';
    if (unchanged) {
      editDialog?.close();
      return;
    }
    savingNames = true;
    try {
      const updated = showsFileNames
        ? await api.resetTrackNames(editing.id)
        : await api.renameTrack(editing.id, {
            title: names.title.trim(),
            artist: names.artist.trim() || null,
            album: names.album.trim() || null,
          });
      tracks = tracks?.map((t) => (t.id === updated.id ? updated : t)) ?? null;
      toast(t(updated.edited ? 'library.renamed' : 'library.namesReset', { title: updated.title }), 'success');
      editDialog?.close();
    } catch (e) {
      // Toasts would sit behind the modal dialog.
      nameError = errorMessage(e);
    } finally {
      savingNames = false;
    }
  }

  function closedEdit() {
    editing = null;
    (editOpener?.isConnected ? editOpener : document.getElementById('library-search'))?.focus();
    editOpener = null;
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

  <div class="toolbar">
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
              <th scope="col" class="title-col">{t('library.columns.title')}</th>
              <th scope="col" class="hide-sm">{t('library.columns.artist')}</th>
              <th scope="col" class="hide-md">{t('library.columns.album')}</th>
              <th scope="col" class="num">{t('library.columns.duration')}</th>
              {#if compact}
                <th scope="col"><span class="visually-hidden">{t('common.moreActions')}</span></th>
              {:else}
                <th scope="col"><span class="visually-hidden">{t('player.addToQueue')}</span></th>
                {#if can('library.manage')}
                  <th scope="col"><span class="visually-hidden">{t('library.rename')}</span></th>
                {/if}
              {/if}
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
                  <span class="cell-title" title={track.path.split('/').pop()}>{track.title}{#if track.edited}{' '}<span class="edited">{t('library.edited')}</span>{/if}</span>
                  <span class="show-sm muted small">{track.artist ?? ''}</span>
                </td>
                <td class="hide-sm">{track.artist ?? '–'}</td>
                <td class="hide-md">{track.album ?? '–'}</td>
                <td class="num muted mono duration">{formatDuration(track.durationMs)}</td>
                {#if compact}
                  <td class="action">
                    <ActionMenu
                      small
                      label="{t('common.moreActions')}: {track.title}"
                      title={track.title}
                      items={[
                        { label: t('player.addToQueue'), icon: 'plus', disabled: !target, onclick: () => play(track, 'append') },
                        { label: t('library.rename'), icon: 'pencil', onclick: () => edit(track, document.activeElement) },
                      ]}
                    />
                  </td>
                {:else}
                  <td class="action">
                    <button class="icon-btn small" disabled={!target} onclick={() => play(track, 'append')} aria-label="{t('player.addToQueue')}: {track.title}">
                      <Icon name="plus" size={16} />
                    </button>
                  </td>
                  {#if can('library.manage')}
                    <td class="action">
                      <button class="icon-btn small" onclick={(e) => edit(track, e.currentTarget)} aria-label="{t('library.rename')}: {track.title}" title={t('library.rename')}>
                        <Icon name="pencil" size={16} />
                      </button>
                    </td>
                  {/if}
                {/if}
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

<dialog
  bind:this={editDialog}
  class="modal wide"
  aria-labelledby="rename-title"
  aria-describedby="rename-hint"
  oncancel={(e) => savingNames && e.preventDefault()}
  onclose={closedEdit}
>
  {#if editing && fileNames}
    <form onsubmit={saveNames} novalidate>
      <h2 id="rename-title" tabindex="-1">{t('library.renameTitle')}</h2>
      <p id="rename-hint" class="small muted">{t('library.renameHint')}</p>
      <div class="file">
        <p class="small muted">
          <Icon name="music" size={14} />
          <span class="visually-hidden">{t('library.file')}:</span>
          <span class="path">{editing.path.split('/').pop()}</span>
        </p>
        {#if !showsFileNames}
          <button type="button" class="link-btn small" onclick={useFileNames}>{t('library.resetNames')}</button>
        {/if}
      </div>
      {#each [{ key: 'title', label: t('library.columns.title') }, { key: 'artist', label: t('library.columns.artist') }, { key: 'album', label: t('library.columns.album') }] as const as field (field.key)}
        <div class="field">
          <label class="label" for="name-{field.key}">{field.label}</label>
          {#if field.key === 'title'}
            <input type="text" id="name-title" bind:this={titleInput} bind:value={names.title} maxlength="200" autocomplete="off" aria-describedby="file-title{titleError ? ' title-error' : ''}" aria-invalid={!!titleError} />
            {#if titleError}
              <span class="field-error" id="title-error"><Icon name="alert" size={14} />{titleError}</span>
            {/if}
          {:else}
            <input type="text" id="name-{field.key}" bind:value={names[field.key]} maxlength="200" autocomplete="off" aria-describedby="file-{field.key}" />
          {/if}
          <span class="hint" id="file-{field.key}">{t('library.fromFile')}: {fileNames[field.key] || '–'}</span>
        </div>
      {/each}
      <p class="hint none-hint">{t('library.emptyMeansNone')}</p>
      {#if editing.edited && showsFileNames}
        <p class="small restore-note">{t('library.restoreNote')}</p>
      {/if}
      {#if nameError}
        <p class="name-error" role="alert"><Icon name="alert" size={16} /><span>{nameError}</span></p>
      {/if}
      <div class="actions">
        <button type="button" class="btn" disabled={savingNames} onclick={() => editDialog?.close()}>{t('common.cancel')}</button>
        <button type="submit" class="btn btn-primary" disabled={savingNames}>
          {savingNames ? t('common.saving') : t('common.save')}
        </button>
      </div>
    </form>
  {/if}
</dialog>

<style>
  .title-col {
    width: 45%;
  }
  .duration {
    font-size: var(--text-sm);
  }
  dialog .field {
    margin-top: var(--space-3);
  }
  .field-error,
  .name-error {
    display: flex;
    align-items: flex-start;
    gap: var(--space-1);
    color: var(--danger);
    font-size: var(--text-sm);
  }
  .name-error {
    gap: var(--space-2);
    margin: var(--space-3) 0 0;
  }
  .none-hint {
    margin: var(--space-2) 0 0;
  }
  .restore-note {
    margin: var(--space-3) 0 0;
    color: var(--text);
  }
  .file {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    justify-content: space-between;
    gap: var(--space-1) var(--space-3);
    margin-top: var(--space-3);
  }
  .file p {
    margin: 0;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .file :global(svg) {
    vertical-align: -2px;
  }
  .link-btn {
    padding: 0;
    border: 0;
    background: none;
    color: var(--accent);
    font: inherit;
    font-weight: 600;
    cursor: pointer;
    text-decoration: underline;
    text-underline-offset: 2px;
  }
  .link-btn.small {
    font-size: var(--text-sm);
  }
  .cell-title {
    overflow-wrap: anywhere;
  }
  .edited {
    display: inline-block;
    padding: 0 6px;
    border-radius: 999px;
    background: var(--surface-2);
    color: var(--text-muted);
    font-size: var(--text-xs);
    font-weight: 500;
    vertical-align: 1px;
  }
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
