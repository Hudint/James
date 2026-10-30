<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import { api } from '$lib/api';
  import Icon from '$lib/components/Icon.svelte';
  import { formatDuration } from '$lib/format';
  import { i18n, t } from '$lib/i18n/index.svelte';
  import { live, loadSnapshot, positionOf, setStatus } from '$lib/live.svelte';
  import { can, reportError, toast } from '$lib/session.svelte';
  import type { InstanceSummary, LogEntry, PluginOverview } from '$lib/types';

  /** Refresh of the side panels (warnings, plugin host). */
  const POLL_MS = 30_000;

  let instances = $state<InstanceSummary[] | null>(null);
  let error = $state('');
  let plugins = $state<PluginOverview | null>(null);
  let warnings = $state<(LogEntry & { instance: string })[] | null>(null);

  async function load() {
    try {
      instances = await api.instances();
      for (const i of instances) {
        setStatus(i.id, i.status);
        if (i.running && !live.instances[i.id]?.snapshot) void loadSnapshot(i.id);
      }
      error = '';
    } catch (e) {
      error = (e as Error).message;
    }
  }

  /** Per instance: the log stream read so far and its latest warnings. */
  const cursors = new Map<string, { boot: number; seq: number; recent: (LogEntry & { instance: string })[] }>();
  let polling = false;

  async function warningsOf(id: string): Promise<(LogEntry & { instance: string })[]> {
    const known = cursors.get(id);
    let page = await api.logs(id, known?.seq ?? 0, 5, 'warn');
    // James restarted: its log starts over.
    if (known && page.boot !== known.boot) page = await api.logs(id, 0, 5, 'warn');
    const fresh = known && page.boot === known.boot ? known.recent : [];
    const found = page.entries.map((e) => ({ ...e, instance: id }));
    const recent = [...fresh, ...found].slice(-5);
    const seq = page.entries.at(-1)?.seq ?? (known && page.boot === known.boot ? known.seq : 0);
    cursors.set(id, { boot: page.boot, seq, recent });
    return recent;
  }

  async function loadSide() {
    // A slow round is not overtaken by the next one.
    if (polling) return;
    polling = true;
    try {
      plugins = can('plugins.manage') ? await api.plugins().catch(() => null) : null;
      // Stopped instances too: their last words are often the interesting ones.
      const readable = (instances ?? []).filter((i) => can('instance.logs', i.id));
      // Deleted instances (an id may come back as a new instance with a new log; one recreated
      // between two polls shows its predecessor's last warnings until newer ones replace them).
      for (const id of cursors.keys()) if (!readable.some((i) => i.id === id)) cursors.delete(id);
      if (!readable.length) {
        warnings = null;
        return;
      }
      const lists = await Promise.all(readable.map((i) => warningsOf(i.id).catch(() => cursors.get(i.id)?.recent ?? [])));
      warnings = lists
        .flat()
        .sort((a, b) => b.time - a.time)
        .slice(0, 5);
    } finally {
      polling = false;
    }
  }

  let timer: ReturnType<typeof setInterval> | undefined;
  onMount(async () => {
    await load();
    void loadSide();
    timer = setInterval(() => {
      if (!document.hidden) void loadSide();
    }, POLL_MS);
  });
  onDestroy(() => clearInterval(timer));

  // Instances started or stopped elsewhere.
  $effect(() => {
    if (live.version > 0) void load();
  });

  // Progress bars move while something plays.
  let now = $state(performance.now());
  const anyPlaying = $derived(Object.values(live.instances).some((e) => e.status?.current && !e.status.paused));
  $effect(() => {
    if (!anyPlaying) return;
    const tick = setInterval(() => (now = performance.now()), 500);
    return () => clearInterval(tick);
  });

  type State = 'stopped' | 'connecting' | 'ready' | 'playing' | 'paused';
  function stateOf(id: string): State {
    const status = live.instances[id]?.status;
    if (!status) return 'stopped';
    if (!status.connected) return 'connecting';
    if (!status.current) return 'ready';
    return status.paused ? 'paused' : 'playing';
  }

  /** The bot's channel and who else is in it. */
  function room(id: string): { channel: string; listeners: number } | null {
    const snapshot = live.instances[id]?.snapshot;
    if (!snapshot) return null;
    const self = snapshot.clients.find((c) => c.isSelf) ?? snapshot.clients.find((c) => c.id === snapshot.ownClient);
    if (!self?.channel) return null;
    const channel = snapshot.channels.find((c) => c.id === self.channel)?.name ?? '';
    const listeners = snapshot.clients.filter((c) => c.channel === self.channel && !c.isSelf && c.id !== self.id).length;
    return { channel, listeners };
  }

  const summary = $derived.by(() => {
    const states = (instances ?? []).map((i) => stateOf(i.id));
    return {
      total: states.length,
      playing: states.filter((s) => s === 'playing').length,
      ready: states.filter((s) => s === 'ready' || s === 'paused').length,
    };
  });

  async function toggle(id: string) {
    const status = live.instances[id]?.status;
    try {
      if (status?.paused) await api.resume(id);
      else await api.pause(id);
    } catch (e) {
      reportError(e);
    }
  }

  let drafts = $state<Record<string, string>>({});
  let sending = $state<string | null>(null);
  async function quickPlay(event: SubmitEvent, id: string) {
    event.preventDefault();
    const input = (drafts[id] ?? '').trim();
    if (!input) {
      document.getElementById(`quick-${id}`)?.focus();
      return;
    }
    sending = id;
    try {
      await api.play(id, input, 'append');
      drafts = { ...drafts, [id]: '' };
      // The field goes away once it plays: keep keyboard focus on the instance.
      document.getElementById(`name-${id}`)?.focus();
      toast(t('dashboard.added'), 'success');
    } catch (e) {
      reportError(e);
    } finally {
      sending = null;
    }
  }

  async function start(id: string) {
    try {
      await api.startInstance(id);
      await load();
    } catch (e) {
      reportError(e);
    }
  }

  const time = (ms: number) =>
    new Intl.DateTimeFormat(i18n.locale, { hour: '2-digit', minute: '2-digit' }).format(new Date(ms));
  const name = (id: string) => instances?.find((i) => i.id === id)?.name ?? id;
</script>

<svelte:head><title>{t('dashboard.title')} · James</title></svelte:head>

<div class="page">
  <header class="page-header">
    <h1>{t('dashboard.title')}</h1>
    {#if can('instance.manage')}
      <a class="btn btn-primary" href="/instances/new"><Icon name="plus" size={16} />{t('instance.new')}</a>
    {/if}
    {#if instances?.length}
      <p class="lede">
        {t('dashboard.summary', { count: summary.total })}
        {#if summary.playing}<span aria-hidden="true">·</span> <span class="on-air">{t('dashboard.summaryPlaying', { count: summary.playing })}</span>{/if}
        {#if summary.ready}<span aria-hidden="true">·</span> {t('dashboard.summaryReady', { count: summary.ready })}{/if}
      </p>
    {/if}
  </header>

  {#if error}
    <div class="card empty" role="alert">
      <Icon name="alert" size={28} />
      <p>{error}</p>
      <button class="btn" onclick={load}>{t('common.retry')}</button>
    </div>
  {:else if instances === null}
    <div class="card desk" aria-busy="true">
      {#each [0, 1] as i (i)}<div class="skeleton"></div>{/each}
    </div>
  {:else if instances.length === 0}
    <div class="card empty">
      <span class="big-meter meter still" aria-hidden="true"><i></i><i></i><i></i></span>
      <h2>{t('dashboard.empty')}</h2>
      <p>{t('dashboard.emptyHint')}</p>
      {#if can('instance.manage')}
        <a class="btn btn-primary" href="/instances/new">{t('dashboard.createFirst')}</a>
      {/if}
    </div>
  {:else}
    <div class="layout">
      <ul class="card desk" role="list">
        {#each instances as instance (instance.id)}
          {@const status = live.instances[instance.id]?.status}
          {@const state = stateOf(instance.id)}
          {@const where = room(instance.id)}
          {@const track = status?.current?.track}
          <li class="strip {state}">
            <div class="top">
              <span class="indicator" aria-hidden="true">
                {#if state === 'playing' || state === 'paused'}
                  <span class="meter" class:still={state === 'paused'}><i></i><i></i><i></i></span>
                {:else}
                  <span class="dot"></span>
                {/if}
              </span>
              <div class="who">
                <a class="name" id="name-{instance.id}" href="/instances/{instance.id}">{instance.name}</a>
                <span class="where small muted">
                  {#if state === 'stopped'}
                    {t('instance.stopped')}
                  {:else if state === 'connecting'}
                    {status?.lastError ? t('dashboard.retrying') : t('instance.connecting')}
                  {:else if where}
                    {t(state === 'playing' ? 'dashboard.playingIn' : state === 'paused' ? 'dashboard.pausedIn' : 'dashboard.readyIn', { channel: where.channel })}
                  {:else}
                    {t(state === 'playing' ? 'dashboard.playing' : state === 'paused' ? 'dashboard.paused' : 'instance.connected')}
                  {/if}
                </span>
              </div>
              <div class="figures small muted">
                {#if where}
                  <span title={t('dashboard.listeners', { count: where.listeners })}>
                    <Icon name="users" size={14} /><span>{where.listeners}</span><span class="visually-hidden">{t('dashboard.listeners', { count: where.listeners })}</span>
                  </span>
                {/if}
                {#if status && status.queueLength > 0}
                  <span title={t('dashboard.queued', { count: status.queueLength })}>
                    <Icon name="list" size={14} /><span>{status.queueLength}</span><span class="visually-hidden">{t('dashboard.queued', { count: status.queueLength })}</span>
                  </span>
                {/if}
                {#if status}
                  <span title={t('player.volume')}>
                    <Icon name={status.volume === 0 ? 'volumeMute' : 'volume'} size={14} /><span>{new Intl.NumberFormat(i18n.locale, { style: 'percent' }).format(status.volume / 100)}</span><span class="visually-hidden">{t('player.volume')}</span>
                  </span>
                {/if}
              </div>
              <a class="icon-btn open" href="/instances/{instance.id}" aria-label={t('dashboard.open', { name: instance.name })}>
                <Icon name="right" size={18} />
              </a>
            </div>

            {#if track && status}
              {@const duration = track.durationMs ?? null}
              {@const position = positionOf(instance.id, now)}
              <div class="now">
                <div class="art" aria-hidden="true">
                  {#if track.thumbnail}<img src={track.thumbnail} alt="" referrerpolicy="no-referrer" />{:else}<Icon name={track.isStream ? 'radio' : 'music'} size={20} />{/if}
                </div>
                <div class="track">
                  <span class="title truncate" title={track.title}>{track.title}</span>
                  {#if track.artist}<span class="artist truncate small muted">{track.artist}</span>{/if}
                  <div class="progress">
                    <span class="mono time">{formatDuration(position)}</span>
                    <span class="bar" class:stream={track.isStream || !duration}>
                      <span class="fill" style:transform="scaleX({duration ? Math.min(1, position / duration) : 1})"></span>
                    </span>
                    <span class="mono time">{track.isStream ? t('player.live') : formatDuration(duration)}</span>
                  </div>
                </div>
                {#if can('playback.control', instance.id)}
                  <div class="controls">
                    <button class="icon-btn play" onclick={() => toggle(instance.id)} aria-label={status.paused ? t('player.resume') : t('player.pause')}>
                      <Icon name={status.paused ? 'play' : 'pause'} size={18} />
                    </button>
                    <button class="icon-btn" onclick={() => api.skip(instance.id).catch(reportError)} aria-label={t('player.next')}>
                      <Icon name="next" size={18} />
                    </button>
                  </div>
                {/if}
              </div>
            {:else if state === 'ready' && can('playback.play', instance.id)}
              <form class="quick" onsubmit={(e) => quickPlay(e, instance.id)}>
                <label class="visually-hidden" for="quick-{instance.id}">{t('dashboard.quickLabel', { name: instance.name })}</label>
                <input
                  id="quick-{instance.id}"
                  type="text"
                  placeholder={t('dashboard.quickPlaceholder')}
                  value={drafts[instance.id] ?? ''}
                  oninput={(e) => (drafts = { ...drafts, [instance.id]: e.currentTarget.value })}
                />
                <button class="icon-btn small go" type="submit" disabled={sending === instance.id} aria-label={t('dashboard.quickPlay')} title={t('dashboard.quickPlay')}>
                  <Icon name="play" size={18} />
                </button>
              </form>
            {:else if state === 'stopped' && can('instance.manage', instance.id)}
              <div class="quick">
                <button class="btn btn-small" onclick={() => start(instance.id)}><Icon name="power" size={16} />{t('instance.start')}</button>
              </div>
            {:else if state === 'connecting' && status?.lastError}
              <p class="small error-line">{status.lastError}</p>
            {/if}
          </li>
        {/each}
      </ul>

      {#if plugins || warnings !== null}
        <aside class="side">
          {#if plugins}
            {@const failed = plugins.plugins.filter((p) => !p.manifest.hidden && p.state.enabled && (p.status.state === 'failed' || p.status.state === 'needsApproval'))}
            {@const running = plugins.plugins.filter((p) => !p.manifest.hidden && p.status.state === 'running').length}
            <section class="card panel">
              <h2>{t('dashboard.system')}</h2>
              <ul class="facts">
                <li>
                  <span class="status {plugins.host.state === 'running' ? 'success' : plugins.host.state === 'starting' ? 'warning' : 'danger'}"><span class="dot"></span>{t('dashboard.pluginHost')}</span>
                  <span class="small muted">{t(plugins.host.state === 'running' ? 'dashboard.hostRunning' : plugins.host.state === 'starting' ? 'dashboard.hostStarting' : 'dashboard.hostDown')}</span>
                </li>
                <li>
                  <span class="status {running ? 'success' : ''}"><span class="dot"></span>{t('nav.plugins')}</span>
                  <span class="small muted">{t('dashboard.pluginsRunning', { count: running })}</span>
                </li>
                {#if failed.length || plugins.errors.length}
                  <li>
                    <span class="status danger"><span class="dot"></span>{t('dashboard.attention')}</span>
                    <a class="small" href="/plugins">{t('dashboard.pluginProblems', { count: failed.length + plugins.errors.length })}</a>
                  </li>
                {/if}
              </ul>
            </section>
          {/if}
          {#if warnings !== null}
            <section class="card panel">
              <h2>{t('dashboard.warnings')}</h2>
              {#if warnings.length === 0}
                <p class="small muted calm"><Icon name="check" size={16} />{t('dashboard.noWarnings')}</p>
              {:else}
                <ul class="log">
                  {#each warnings as entry (`${entry.instance}-${entry.seq}`)}
                    <li class={entry.level}>
                      <span class="dot" aria-hidden="true"></span>
                      <div>
                        <p class="message">{entry.message}</p>
                        <p class="small muted">
                          <span class="mono">{time(entry.time)}</span> · <a href="/instances/{entry.instance}/logs">{name(entry.instance)}</a>{entry.plugin ? ` · ${entry.plugin}` : ''}
                        </p>
                      </div>
                    </li>
                  {/each}
                </ul>
              {/if}
            </section>
          {/if}
        </aside>
      {/if}
    </div>
  {/if}
</div>

<style>
  .on-air {
    color: var(--tally-text);
    font-weight: 550;
  }
  .layout {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: var(--space-5);
    align-items: start;
  }
  @media (min-width: 1100px) {
    .layout {
      grid-template-columns: minmax(0, 1fr) 320px;
    }
  }
  /* Wide screens: the desk becomes a grid of instance cards. */
  @media (min-width: 1800px) {
    .layout {
      grid-template-columns: minmax(0, 1fr) 360px;
    }
    .desk {
      display: grid;
      grid-template-columns: repeat(auto-fill, minmax(520px, 1fr));
      gap: var(--space-4);
      background: none;
      border: none;
      overflow: visible;
    }
    .strip {
      border: 1px solid var(--border);
      border-radius: var(--radius-lg);
      background: var(--surface);
      overflow: clip;
    }
    .strip:last-child {
      border-bottom: 1px solid var(--border);
    }
    .quick {
      max-width: none;
    }
  }
  .desk {
    list-style: none;
    margin: 0;
    padding: 0;
    overflow: hidden;
  }
  .skeleton {
    height: 88px;
    border-bottom: 1px solid var(--border);
    background: linear-gradient(90deg, var(--surface) 0%, var(--surface-2) 50%, var(--surface) 100%);
    background-size: 200% 100%;
    animation: shimmer 1.4s linear infinite;
  }
  @keyframes shimmer {
    to {
      background-position: -200% 0;
    }
  }
  .strip {
    position: relative;
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    padding: var(--space-4) var(--space-4) var(--space-4) var(--space-5);
    border-bottom: 1px solid var(--border);
    transition: background var(--dur-2) var(--ease-out);
  }
  .strip:last-child {
    border-bottom: none;
  }
  /* On air: the tally edge. */
  .strip::before {
    content: '';
    position: absolute;
    left: 0;
    top: 0;
    bottom: 0;
    width: 3px;
    background: var(--tally);
    transform: scaleY(0);
    transition: transform var(--dur-3) var(--ease-out);
  }
  .strip.playing::before {
    transform: scaleY(1);
  }
  .top {
    display: flex;
    align-items: center;
    gap: var(--space-3);
  }
  .indicator {
    display: grid;
    place-items: center;
    width: 16px;
    flex-shrink: 0;
  }
  .indicator .dot {
    background: var(--input-border);
  }
  .ready .indicator .dot {
    background: var(--success);
  }
  .connecting .indicator .dot {
    background: var(--warning);
    animation: pulse 1.4s ease-in-out infinite;
  }
  @keyframes pulse {
    50% {
      opacity: 0.3;
    }
  }
  .who {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .name {
    font-size: var(--text-lg);
    font-weight: 600;
    letter-spacing: -0.015em;
    color: var(--text);
  }
  .where {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .figures {
    display: flex;
    gap: var(--space-4);
  }
  .figures > span {
    display: inline-flex;
    align-items: center;
    gap: 5px;
  }
  .open {
    color: var(--text-muted);
  }
  .now {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    padding-left: calc(16px + var(--space-3));
    animation: rise var(--dur-3) var(--ease-out);
  }
  .art {
    width: 48px;
    height: 48px;
    flex-shrink: 0;
    border-radius: var(--radius);
    overflow: hidden;
    display: grid;
    place-items: center;
    background: var(--side-bg);
    border: 1px solid var(--side-border);
    color: var(--tally);
  }
  .art img {
    width: 100%;
    height: 100%;
    object-fit: cover;
  }
  .track {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .title {
    font-weight: 550;
  }
  .progress {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    margin-top: 6px;
  }
  .time {
    font-size: var(--text-xs);
    color: var(--text-muted);
    min-width: 3.2em;
  }
  .time:last-child {
    text-align: right;
  }
  .bar {
    flex: 1;
    height: 4px;
    border-radius: 2px;
    background: var(--surface-3);
    overflow: hidden;
  }
  .fill {
    display: block;
    height: 100%;
    background: var(--tally);
    transform-origin: left;
    transition: transform 0.5s linear;
  }
  .paused .fill {
    background: var(--text-muted);
  }
  .bar.stream .fill {
    opacity: 0.35;
  }
  .controls {
    display: flex;
    gap: var(--space-1);
  }
  .controls .play {
    background: var(--tally);
    color: var(--tally-ink);
    border-radius: 50%;
  }
  @media (hover: hover) {
    .controls .play:hover {
      background: var(--tally-hover);
    }
  }
  .quick {
    position: relative;
    display: flex;
    padding-left: calc(16px + var(--space-3));
    max-width: 560px;
  }
  .quick input {
    flex: 1;
    min-width: 0;
    padding-right: 44px;
  }
  .quick .go {
    position: absolute;
    right: 4px;
    top: 50%;
    translate: 0 -50%;
    color: var(--tally-text);
  }
  @media (hover: hover) {
    .quick .go:hover:not(:disabled) {
      background: var(--tally-soft);
    }
  }
  .error-line {
    padding-left: calc(16px + var(--space-3));
    color: var(--warning);
    overflow-wrap: anywhere;
  }
  .side {
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
  }
  .panel h2 {
    font-size: var(--text-base);
    margin-bottom: var(--space-3);
  }
  .facts {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }
  .facts li {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: var(--space-3);
  }
  .facts .status {
    color: var(--text);
  }
  .calm {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }
  .calm :global(svg) {
    color: var(--success);
  }
  .log {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }
  .log li {
    display: flex;
    gap: var(--space-2);
    align-items: baseline;
    min-width: 0;
  }
  .log li > div {
    min-width: 0;
  }
  .log .dot {
    background: var(--warning);
    transform: translateY(-1px);
  }
  .log .error .dot {
    background: var(--danger);
  }
  .message {
    font-size: var(--text-sm);
    overflow-wrap: anywhere;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .big-meter {
    width: 40px;
    height: 32px;
    gap: 5px;
  }
  @media (max-width: 600px) {
    .strip {
      padding: var(--space-3) var(--space-3) var(--space-3) var(--space-4);
    }
    .figures {
      display: none;
    }
    .now,
    .quick,
    .error-line {
      padding-left: 0;
    }

  }
</style>
