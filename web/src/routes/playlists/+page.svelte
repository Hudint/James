<script lang="ts">
  import { onMount } from 'svelte';
  import { api } from '$lib/api';
  import Icon from '$lib/components/Icon.svelte';
  import { t } from '$lib/i18n/index.svelte';
  import { can, reportError } from '$lib/session.svelte';
  import type { Playlist } from '$lib/types';

  let playlists = $state<Playlist[] | null>(null);
  let name = $state('');
  let creating = $state(false);

  async function load() {
    playlists = await api.playlists().catch((e) => {
      reportError(e);
      return [];
    });
  }

  onMount(load);

  async function create(event: SubmitEvent) {
    event.preventDefault();
    if (!name.trim()) return;
    creating = true;
    try {
      await api.createPlaylist(name);
      name = '';
      await load();
    } catch (e) {
      reportError(e);
    } finally {
      creating = false;
    }
  }
</script>

<svelte:head><title>{t('playlists.title')} · James</title></svelte:head>

<div class="page">
  <header class="page-header"><h1>{t('playlists.title')}</h1></header>

  {#if can('playlist.manage')}
    <form class="card row stack-narrow" onsubmit={create}>
      <label class="visually-hidden" for="playlist-name">{t('playlists.name')}</label>
      <input id="playlist-name" class="grow" type="text" maxlength="100" placeholder={t('playlists.new')} bind:value={name} />
      <button class="btn btn-primary" type="submit" disabled={creating || !name.trim()}><Icon name="plus" size={16} />{t('common.create')}</button>
    </form>
  {/if}

  {#if playlists === null}
    <p class="muted" aria-busy="true">{t('common.loading')}</p>
  {:else if playlists.length === 0}
    <div class="card empty"><Icon name="list" size={32} /><p>{t('playlists.empty')}</p></div>
  {:else}
    <ul class="card list">
      {#each playlists as playlist (playlist.id)}
        <li>
          <Icon name="list" />
          <a class="grow truncate" href="/playlists/{playlist.id}">{playlist.name}</a>
          <span class="muted small">{t('playlists.tracks', { count: playlist.trackCount })}</span>
        </li>
      {/each}
    </ul>
  {/if}
</div>

<style>
  .grow {
    flex: 1;
    min-width: 0;
  }
  form.row {
    flex-wrap: nowrap;
  }
  .list a {
    font-weight: 550;
    color: var(--text);
  }
</style>
