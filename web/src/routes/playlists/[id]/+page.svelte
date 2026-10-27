<script lang="ts">
  import { goto } from '$app/navigation';
  import { page } from '$app/state';
  import { api } from '$lib/api';
  import { confirmAction } from '$lib/confirm.svelte';
  import Icon from '$lib/components/Icon.svelte';
  import InstancePicker from '$lib/components/InstancePicker.svelte';
  import { formatDuration } from '$lib/format';
  import { t } from '$lib/i18n/index.svelte';
  import { can, reportError, toast } from '$lib/session.svelte';
  import type { Playlist, Track } from '$lib/types';

  let id = $derived(Number(page.params.id));
  let playlist = $state<(Playlist & { tracks: Track[] }) | null>(null);
  let input = $state('');
  let adding = $state(false);
  let target = $state('');
  let manage = $derived(can('playlist.manage'));

  async function load() {
    try {
      playlist = await api.playlist(id);
    } catch (e) {
      reportError(e);
    }
  }

  $effect(() => {
    void id;
    void load();
  });

  async function add(event: SubmitEvent) {
    event.preventDefault();
    if (!input.trim()) return;
    adding = true;
    try {
      const tracks = await api.appendToPlaylist(id, input.trim());
      toast(tracks.length === 1 ? t('player.added', { title: tracks[0].title }) : t('player.addedMany', { count: tracks.length }), 'success');
      input = '';
      await load();
    } catch (e) {
      reportError(e);
    } finally {
      adding = false;
    }
  }

  async function removeTrack(index: number) {
    if (!playlist) return;
    const tracks = playlist.tracks.filter((_, i) => i !== index);
    try {
      await api.setPlaylistTracks(id, tracks);
      playlist.tracks = tracks;
    } catch (e) {
      reportError(e);
    }
  }

  async function play(mode: 'now' | 'append') {
    if (!target) return;
    try {
      const items = await api.playPlaylist(id, target, mode);
      toast(t('player.addedMany', { count: items.length }), 'success');
    } catch (e) {
      reportError(e);
    }
  }

  async function remove() {
    if (!playlist) return;
    if (!(await confirmAction(t('playlists.deleteConfirm', { name: playlist.name }), t('common.delete')))) return;
    try {
      await api.deletePlaylist(id);
      await goto('/playlists');
    } catch (e) {
      reportError(e);
    }
  }
</script>

<svelte:head><title>{playlist?.name ?? t('playlists.title')} · James</title></svelte:head>

<div class="page">
  {#if !playlist}
    <p class="muted" aria-busy="true">{t('common.loading')}</p>
  {:else}
    <header class="page-header">
      <div class="title">
        <a class="icon-btn" href="/playlists" aria-label={t('common.back')}><Icon name="left" /></a>
        <h1 class="truncate">{playlist.name}</h1>
      </div>
      {#if manage}
        <button class="btn btn-danger" onclick={remove}><Icon name="trash" size={16} />{t('common.delete')}</button>
      {/if}
    </header>

    <div class="card row">
      <span class="small muted nowrap">{t('playlists.playOn')}</span>
      <div class="picker"><InstancePicker bind:value={target} id="playlist-target" /></div>
      <button class="btn btn-primary" disabled={!target || playlist.tracks.length === 0} onclick={() => play('now')}>
        <Icon name="play" size={16} />{t('player.playNow')}
      </button>
      <button class="btn" disabled={!target || playlist.tracks.length === 0} onclick={() => play('append')}>
        <Icon name="plus" size={16} />{t('playlists.appendTo')}
      </button>
    </div>

    {#if manage}
      <form class="card row stack-narrow" onsubmit={add}>
        <label class="visually-hidden" for="playlist-add">{t('playlists.add')}</label>
        <input id="playlist-add" class="grow" type="text" placeholder={t('playlists.addPlaceholder')} bind:value={input} disabled={adding} />
        <button class="btn" type="submit" disabled={adding || !input.trim()}><Icon name="plus" size={16} />{adding ? t('common.loading') : t('playlists.add')}</button>
      </form>
    {/if}

    <section class="card">
      {#if playlist.tracks.length === 0}
        <p class="muted empty-list">{t('playlists.emptyList')}</p>
      {:else}
        <ol class="list">
          {#each playlist.tracks as track, index (index)}
            <li>
              <span class="muted small position">{index + 1}</span>
              <div class="grow meta">
                <span class="truncate">{track.title}</span>
                <span class="truncate muted small">{track.artist ?? ''}</span>
              </div>
              <span class="muted small">{track.isStream ? t('player.live') : formatDuration(track.durationMs)}</span>
              {#if manage}
                <button class="icon-btn small" onclick={() => removeTrack(index)} aria-label="{t('queue.remove')}: {track.title}">
                  <Icon name="trash" size={16} />
                </button>
              {/if}
            </li>
          {/each}
        </ol>
      {/if}
    </section>
  {/if}
</div>

<style>
  .grow {
    flex: 1;
    min-width: 0;
  }
  .picker {
    flex: 1 1 200px;
    max-width: 320px;
  }
  .meta {
    display: flex;
    flex-direction: column;
  }
  .position {
    width: 2em;
    text-align: right;
  }
  .empty-list {
    text-align: center;
    padding: var(--space-4);
  }
  form.row {
    flex-wrap: nowrap;
  }
</style>
