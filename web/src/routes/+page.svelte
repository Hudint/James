<script lang="ts">
  import { onMount } from 'svelte';
  import { api } from '$lib/api';
  import Icon from '$lib/components/Icon.svelte';
  import { formatDuration, trackLabel } from '$lib/format';
  import { t } from '$lib/i18n/index.svelte';
  import { live, setStatus } from '$lib/live.svelte';
  import { can, reportError } from '$lib/session.svelte';
  import type { InstanceSummary } from '$lib/types';

  let instances = $state<InstanceSummary[] | null>(null);
  let error = $state('');

  async function load() {
    try {
      instances = await api.instances();
      for (const i of instances) setStatus(i.id, i.status);
      error = '';
    } catch (e) {
      error = (e as Error).message;
    }
  }

  onMount(load);

  async function toggle(i: InstanceSummary) {
    const status = live.instances[i.id]?.status;
    try {
      if (status?.paused) await api.resume(i.id);
      else await api.pause(i.id);
    } catch (e) {
      reportError(e);
    }
  }

  async function skip(id: string) {
    await api.skip(id).catch(reportError);
  }
</script>

<svelte:head><title>{t('dashboard.title')} · James</title></svelte:head>

<div class="page">
  <header class="page-header">
    <h1>{t('dashboard.title')}</h1>
    {#if can('instance.manage')}
      <a class="btn btn-primary" href="/instances/new"><Icon name="plus" size={18} />{t('instance.new')}</a>
    {/if}
  </header>

  {#if error}
    <div class="card empty" role="alert">
      <Icon name="alert" size={28} />
      <p>{error}</p>
      <button class="btn" onclick={load}>{t('common.retry')}</button>
    </div>
  {:else if instances === null}
    <p class="muted" aria-busy="true">{t('common.loading')}</p>
  {:else if instances.length === 0}
    <div class="card empty">
      <Icon name="server" size={36} />
      <h2>{t('dashboard.empty')}</h2>
      <p>{t('dashboard.emptyHint')}</p>
      {#if can('instance.manage')}
        <a class="btn btn-primary" href="/instances/new">{t('dashboard.createFirst')}</a>
      {/if}
    </div>
  {:else}
    <ul class="grid" role="list">
      {#each instances as instance (instance.id)}
        {@const status = live.instances[instance.id]?.status}
        <li class="card instance">
          <a class="title" href="/instances/{instance.id}">
            <span class="truncate">{instance.name}</span>
            <Icon name="right" size={18} />
          </a>
          <div class="row">
            {#if !status}
              <span class="badge">{t('instance.stopped')}</span>
            {:else if !status.connected}
              <span class="badge warning"><span class="dot"></span>{t('instance.connecting')}</span>
            {:else if status.current && !status.paused}
              <span class="badge accent"><span class="dot"></span>{t('dashboard.playing')}</span>
            {:else if status.current}
              <span class="badge"><span class="dot"></span>{t('dashboard.paused')}</span>
            {:else}
              <span class="badge success"><span class="dot"></span>{t('instance.connected')}</span>
            {/if}
            {#if status && status.queueLength > 0}
              <span class="badge">{t('dashboard.queued', { count: status.queueLength })}</span>
            {/if}
          </div>
          <div class="now">
            <Icon name="music" size={18} />
            {#if status?.current}
              <span class="truncate" title={trackLabel(status.current.track)}>{trackLabel(status.current.track)}</span>
              {#if status.current.track.durationMs}
                <span class="muted small">{formatDuration(status.current.track.durationMs)}</span>
              {/if}
            {:else}
              <span class="muted">{t('dashboard.nothingPlaying')}</span>
            {/if}
          </div>
          {#if status?.current && can('playback.control', instance.id)}
            <div class="row controls">
              <button
                class="icon-btn"
                onclick={() => toggle(instance)}
                aria-label={status.paused ? t('player.resume') : t('player.pause')}
              >
                <Icon name={status.paused ? 'play' : 'pause'} />
              </button>
              <button class="icon-btn" onclick={() => skip(instance.id)} aria-label={t('player.next')}>
                <Icon name="next" />
              </button>
            </div>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}
</div>

<style>
  .grid {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
    gap: var(--space-4);
  }
  .instance {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }
  .title {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2);
    font-size: var(--text-lg);
    font-weight: 650;
    color: var(--text);
  }
  .now {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    min-height: 24px;
    color: var(--text);
  }
  .now :global(svg) {
    flex-shrink: 0;
    color: var(--text-muted);
  }
  .controls {
    margin-top: auto;
    border-top: 1px solid var(--border);
    padding-top: var(--space-2);
  }
</style>
