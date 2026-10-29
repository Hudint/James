<script lang="ts">
  import { tick } from 'svelte';
  import { goto } from '$app/navigation';
  import { page } from '$app/state';
  import { api } from '$lib/api';
  import { confirmAction } from '$lib/confirm.svelte';
  import ActionMenu from '$lib/components/ActionMenu.svelte';
  import AddMusic from '$lib/components/AddMusic.svelte';
  import Speak from '$lib/components/Speak.svelte';
  import Icon from '$lib/components/Icon.svelte';
  import MiniPlayer from '$lib/components/MiniPlayer.svelte';
  import Player from '$lib/components/Player.svelte';
  import Queue from '$lib/components/Queue.svelte';
  import ServerTree from '$lib/components/ServerTree.svelte';
  import Tabs from '$lib/components/Tabs.svelte';
  import { t } from '$lib/i18n/index.svelte';
  import { live, loadSnapshot, refreshQueue, refreshStatus, setStatus } from '$lib/live.svelte';
  import { mediaQuery } from '$lib/media.svelte';
  import { can, reportError } from '$lib/session.svelte';
  import type { InstanceSummary } from '$lib/types';

  let id = $derived(page.params.id ?? '');
  let summary = $state<InstanceSummary | null>(null);
  let error = $state('');
  let busy = $state(false);
  let tab = $state<'player' | 'server'>('player');
  // Tabs only exist on narrow screens; on wide screens both panels are shown side by side.
  const narrow = mediaQuery('(max-width: 1099px)');
  // Phones: the instance actions go into a menu next to the title.
  const phone = mediaQuery('(max-width: 600px)');
  let playerCard = $state<HTMLElement | null>(null);
  const manage = $derived(can('instance.manage', id));
  const logs = $derived(can('instance.logs', id));

  /** From the mini player on the server tab: to the player, with keyboard focus there. */
  async function showPlayerTab() {
    tab = 'player';
    await tick();
    playerCard?.querySelector<HTMLElement>('button:not(:disabled)')?.focus();
  }

  let loadId = 0;
  async function load(instanceId: string) {
    const request = ++loadId;
    try {
      const result = await api.instance(instanceId);
      // Navigating to another instance meanwhile makes this answer obsolete.
      if (request !== loadId) return;
      summary = result;
      setStatus(instanceId, summary.status);
      error = '';
      if (summary.running) {
        refreshQueue(instanceId);
        void loadSnapshot(instanceId);
      }
    } catch (e) {
      if (request !== loadId) return;
      summary = null;
      error = (e as Error).message;
    }
  }

  $effect(() => {
    void load(id);
  });

  let entry = $derived(live.instances[id]);
  let status = $derived(entry?.status ?? null);

  async function lifecycle(action: 'start' | 'stop' | 'restart') {
    // Stop and restart interrupt everyone in the channel.
    if (action !== 'start' && summary) {
      const text = t(action === 'stop' ? 'instance.stopConfirm' : 'instance.restartConfirm', { name: summary.name });
      const label = t(action === 'stop' ? 'instance.stop' : 'instance.restart');
      if (!(await confirmAction(text, label))) return;
    }
    busy = true;
    try {
      if (action === 'start') await api.startInstance(id);
      else if (action === 'stop') await api.stopInstance(id);
      else await api.restartInstance(id);
      await load(id);
      await refreshStatus(id);
    } catch (e) {
      reportError(e);
    } finally {
      busy = false;
    }
  }

  /** Space: play/pause, N: next, P: previous — not while typing. */
  function onKeydown(event: KeyboardEvent) {
    const target = event.target as HTMLElement;
    if (event.ctrlKey || event.metaKey || event.altKey || !status?.current || !can('playback.control', id)) return;
    if (target.closest('input, textarea, select, button, a, [contenteditable]')) return;
    const action =
      event.key === ' ' ? (status.paused ? api.resume : api.pause)
      : event.key.toLowerCase() === 'n' ? api.skip
      : event.key.toLowerCase() === 'p' ? api.previous
      : null;
    if (!action) return;
    event.preventDefault();
    action(id).catch(reportError);
  }
</script>

<svelte:head><title>{summary?.name ?? id} · James</title></svelte:head>
<svelte:window onkeydown={onKeydown} />

<div class="page">
  {#if error}
    <div class="card empty" role="alert">
      <Icon name="alert" size={28} />
      <p>{error}</p>
      <button class="btn" onclick={() => goto('/')}>{t('nav.dashboard')}</button>
    </div>
  {:else if !summary}
    <p class="muted" aria-busy="true">{t('common.loading')}</p>
  {:else}
    <header class="page-header">
      <div class="title">
        <h1 class="truncate">{summary.name}</h1>
        <span aria-live="polite">
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
        </span>
        {#if phone.matches && status && (manage || logs)}
          <ActionMenu
            label={t('common.moreActions')}
            title={summary.name}
            items={[
              ...(manage
                ? [
                    { label: t('instance.restart'), icon: 'refresh' as const, disabled: busy, onclick: () => lifecycle('restart') },
                    { label: t('instance.settings'), icon: 'settings' as const, href: `/instances/${id}/settings` },
                  ]
                : []),
              ...(logs ? [{ label: t('logs.title'), icon: 'logs' as const, href: `/instances/${id}/logs` }] : []),
              ...(manage ? [{ label: t('instance.stop'), icon: 'power' as const, danger: true, disabled: busy, onclick: () => lifecycle('stop') }] : []),
            ]}
          />
        {/if}
      </div>
      {#if (manage || logs) && !(phone.matches && status)}
        <div class="row actions">
          {#if manage}
            {#if status}
              <button class="btn" disabled={busy} onclick={() => lifecycle('restart')}><Icon name="refresh" size={16} />{t('instance.restart')}</button>
              <button class="btn" disabled={busy} onclick={() => lifecycle('stop')}><Icon name="power" size={16} />{t('instance.stop')}</button>
            {:else}
              <button class="btn btn-primary" disabled={busy} onclick={() => lifecycle('start')}><Icon name="power" size={16} />{t('instance.start')}</button>
            {/if}
          {/if}
          {#if logs}
            <a class="btn" href="/instances/{id}/logs"><Icon name="logs" size={16} />{t('logs.title')}</a>
          {/if}
          {#if manage}
            <a class="btn" href="/instances/{id}/settings"><Icon name="settings" size={16} />{t('instance.settings')}</a>
          {/if}
        </div>
      {/if}
    </header>

    {#if status && !status.connected && status.lastError}
      <div class="alert" role="alert">
        <Icon name="alert" size={18} />
        <span>{t('instance.connectionFailed', { reason: status.lastError })}</span>
      </div>
    {/if}

    {#if !status}
      <div class="card empty">
        <Icon name="power" size={32} />
        <p>{t('instance.notRunning')}</p>
      </div>
    {:else}
      {#if narrow.matches}
        <Tabs
          bind:value={tab}
          label={summary.name}
          tabs={[
            { value: 'player', label: t('instance.tabs.player'), icon: 'music' },
            { value: 'server', label: t('instance.tabs.server'), icon: 'server' },
          ]}
        />
      {/if}

      <!-- Phones: the main controls stay at hand below the player and on the server tab. -->
      {#if phone.matches}
        <MiniPlayer {id} {status} player={tab === 'player' ? playerCard : null} always={tab === 'server'} onshowplayer={tab === 'server' ? showPlayerTab : undefined} />
      {/if}
      <div class="layout">
        {#if !narrow.matches || tab === 'player'}
          <div
            id="panel-player"
            class="main"
            role={narrow.matches ? 'tabpanel' : undefined}
            aria-labelledby={narrow.matches ? 'tab-player' : undefined}
          >
            <div class="player-anchor" bind:this={playerCard}><Player {id} {status} /></div>
            {#if can('playback.play', id)}<AddMusic {id} />{/if}
            <Queue {id} items={entry?.queue ?? []} />
            {#if can('playback.control', id)}<p class="hint shortcuts">{t('player.shortcuts')}</p>{/if}
          </div>
        {/if}
        {#if !narrow.matches || tab === 'server'}
          <div
            id="panel-server"
            class="side"
            role={narrow.matches ? 'tabpanel' : undefined}
            aria-labelledby={narrow.matches ? 'tab-server' : undefined}
          >
            {#if entry?.snapshot}
              <ServerTree {id} snapshot={entry.snapshot} />
            {:else}
              <div class="card empty"><p>{t('server.noData')}</p></div>
            {/if}
            <!-- Speaking concerns the channel: next to the server view (its own tab on phones). -->
            {#if can('tts.say', id)}<Speak {id} {status} />{/if}
          </div>
        {/if}
      </div>
    {/if}
  {/if}
</div>

<style>
  .layout {
    display: grid;
    gap: var(--space-4);
  }
  .main,
  .side {
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
    min-width: 0;
  }
  .alert {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-3) var(--space-4);
    border-radius: var(--radius);
    background: var(--warning-soft);
    color: var(--warning);
    font-size: var(--text-sm);
  }
  .shortcuts {
    text-align: center;
  }
  @media (hover: none) {
    .shortcuts {
      display: none;
    }
  }
  @media (min-width: 1100px) {
    .layout {
      grid-template-columns: minmax(0, 1fr) 340px;
      align-items: start;
    }
  }
  /* Keyboard shortcuts mean nothing on touch screens and phones. */
  @media (hover: none), (max-width: 600px) {
    .shortcuts {
      display: none;
    }
  }
  /* Scrolled to from the mini player: not under the sticky top bar. */
  .player-anchor {
    scroll-margin-top: 64px;
  }
</style>
