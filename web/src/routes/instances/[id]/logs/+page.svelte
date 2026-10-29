<script lang="ts">
  import { onDestroy, tick, untrack } from 'svelte';
  import { page } from '$app/state';
  import { api, ApiError } from '$lib/api';
  import Icon from '$lib/components/Icon.svelte';
  import { i18n, t } from '$lib/i18n/index.svelte';
  import { errorMessage } from '$lib/session.svelte';
  import type { LogEntry, LogLevel } from '$lib/types';

  const POLL_MS = 2000;
  const MAX_ENTRIES = 2000;
  const LEVELS: LogLevel[] = ['debug', 'info', 'warn', 'error'];
  const DEFAULT_LEVEL: LogLevel = 'info';
  /** Source filter value for James itself (no plugin id contains a space). */
  const SYSTEM = ' system';
  /** Distance from the end (px) that still counts as "at the newest message". */
  const NEAR_END = 40;

  let id = $derived(page.params.id ?? '');
  let name = $state('');
  let entries = $state<LogEntry[] | null>(null);
  let error = $state('');
  let minLevel = $state<LogLevel>(DEFAULT_LEVEL);
  /** '' = all, SYSTEM = James itself, else a plugin id. */
  let source = $state('');
  let query = $state('');
  let follow = $state(true);
  /** Sequence number of the newest message seen while following (for the "new" count). */
  let seenSeq = $state(0);
  let list = $state<HTMLElement>();

  const plugins = $derived(
    [...new Set([...(entries ?? []).flatMap((e) => (e.plugin ? [e.plugin] : [])), ...(source && source !== SYSTEM ? [source] : [])])].sort(),
  );
  const filtered = $derived(minLevel !== DEFAULT_LEVEL || source !== '' || query.trim() !== '');
  const shown = $derived(
    (entries ?? []).filter(
      (e) =>
        LEVELS.indexOf(e.level) >= LEVELS.indexOf(minLevel) &&
        (source === '' || (source === SYSTEM ? e.plugin === null : e.plugin === source)) &&
        (query.trim() === '' || `${e.plugin ?? ''} ${e.message}`.toLowerCase().includes(query.trim().toLowerCase())),
    ),
  );
  const unseen = $derived(follow ? 0 : shown.filter((e) => e.seq > seenSeq).length);

  const timeFormat = $derived(new Intl.DateTimeFormat(i18n.locale, { hour: '2-digit', minute: '2-digit', second: '2-digit', hourCycle: 'h23' }));
  const dayFormat = $derived(new Intl.DateTimeFormat(i18n.locale, { day: '2-digit', month: '2-digit' }));
  const dateFormat = $derived(new Intl.DateTimeFormat(i18n.locale, { dateStyle: 'medium', timeStyle: 'medium' }));
  let today = $state(new Date().toDateString());

  let timer: ReturnType<typeof setTimeout> | undefined;
  let loadId = 0;
  /** The request whose poll is running (one at a time per page view). */
  let polling = -1;
  /** No access or the instance is gone: polling stopped. */
  let stopped = false;
  /** James' start the entries are from (sequence numbers start over with every start). */
  let boot: number | null = null;

  async function poll(instance: string, request: number) {
    clearTimeout(timer);
    if (polling === request || stopped || request !== loadId) return;
    if (!document.hidden) {
      polling = request;
      try {
        let page = await api.logs(instance, entries?.at(-1)?.seq ?? 0, MAX_ENTRIES);
        if (request !== loadId) return;
        if (boot !== null && page.boot !== boot) {
          // James restarted: start over.
          boot = page.boot;
          entries = null;
          seenSeq = 0;
          follow = true;
          page = await api.logs(instance, 0, MAX_ENTRIES);
          if (request !== loadId) return;
        }
        boot = page.boot;
        error = '';
        today = new Date().toDateString();
        const last = entries?.at(-1)?.seq ?? 0;
        const fresh = page.entries.filter((e) => e.seq > last);
        if (fresh.length || entries === null) {
          // While reading older messages, the oldest dropping out must not move the text: the
          // first visible row keeps its place.
          const anchor = follow || !list ? undefined : [...list.children].find((row) => (row as HTMLElement).offsetTop + (row as HTMLElement).offsetHeight > list!.scrollTop) as HTMLElement | undefined;
          const anchorTop = anchor?.offsetTop ?? 0;
          entries = [...(entries ?? []), ...fresh].slice(-MAX_ENTRIES);
          await tick();
          if (follow) await toEnd();
          else if (list && anchor?.isConnected) list.scrollTop += anchor.offsetTop - anchorTop;
        }
      } catch (e) {
        if (request !== loadId) return;
        error = errorMessage(e);
        entries ??= [];
        // No access (any more) or the instance is gone: asking again does not help.
        if (e instanceof ApiError && (e.status === 403 || e.status === 404)) stopped = true;
      } finally {
        if (polling === request) polling = -1;
      }
    }
    if (request === loadId && !stopped) timer = setTimeout(() => void poll(instance, request), POLL_MS);
  }

  $effect(() => {
    const instance = id;
    // Only a change of the instance starts over (the poll reads the entries).
    untrack(() => {
      const request = ++loadId;
      clearTimeout(timer);
      stopped = false;
      boot = null;
      entries = null;
      name = '';
      source = page.url.searchParams.get('source') ?? '';
      follow = true;
      api.instance(instance).then(
        (s) => request === loadId && (name = s.name),
        () => {},
      );
      void poll(instance, request);
    });
  });
  onDestroy(() => {
    loadId++;
    clearTimeout(timer);
  });

  // A changed filter shows the newest matching messages (while following).
  $effect(() => {
    void [minLevel, source, query];
    if (untrack(() => follow)) void toEnd();
  });

  async function toEnd() {
    await tick();
    // Instantly: a smooth scroll would report positions above the end and stop following.
    list?.scrollTo({ top: list.scrollHeight });
    seenSeq = entries?.at(-1)?.seq ?? 0;
  }

  function onVisibility() {
    if (!document.hidden) void poll(id, loadId);
  }

  function jumpToEnd() {
    follow = true;
    void toEnd();
  }

  function onScroll() {
    if (!list) return;
    // Scrolling up stops following; back at the end it continues.
    const atEnd = list.scrollHeight - list.scrollTop - list.clientHeight < NEAR_END;
    if (atEnd && !follow) seenSeq = entries?.at(-1)?.seq ?? 0;
    follow = atEnd;
  }

  function resetFilters() {
    minLevel = DEFAULT_LEVEL;
    source = '';
    query = '';
    follow = true;
  }

  // A list that appears (again, e.g. after "no match") starts at the newest message.
  $effect(() => {
    if (list && untrack(() => follow)) void toEnd();
  });

  /** Announced to screen readers when new messages arrive while reading older ones. */
  const announcement = $derived(
    unseen === 0 ? '' : unseen < 10 ? t('logs.newMessages') : t('logs.newMessagesCount', { count: Math.floor(unseen / 10) * 10 }),
  );

  function sourceName(entry: LogEntry): string {
    return entry.plugin ?? t('logs.system');
  }

  function download() {
    const pad = (n: number) => String(n).padStart(2, '0');
    const offset = (d: Date) => {
      const minutes = -d.getTimezoneOffset();
      return `${minutes < 0 ? '-' : '+'}${pad(Math.floor(Math.abs(minutes) / 60))}:${pad(Math.abs(minutes) % 60)}`;
    };
    const stamp = (d: Date) =>
      `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
    const text = shown
      .map((e) => {
        const d = new Date(e.time);
        return `${stamp(d)}${offset(d)} ${e.level.toUpperCase().padEnd(5)} [${sourceName(e)}] ${e.message}`;
      })
      .join('\n');
    const now = new Date();
    const file = `james-${(name || id).replace(/[^\p{L}\p{N}_-]+/gu, '-')}-${stamp(now).replace(/[-: ]/g, '')}.txt`;
    const url = URL.createObjectURL(new Blob([text + '\n'], { type: 'text/plain' }));
    const link = document.createElement('a');
    link.href = url;
    link.download = file;
    link.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  }
</script>

<svelte:head><title>{t('logs.title')} · {name || id} · James</title></svelte:head>
<svelte:document onvisibilitychange={onVisibility} />

<div class="page">
  <header class="page-header">
    <div class="heading">
      <a class="back small" href="/instances/{id}"><Icon name="left" size={16} />{name || id}</a>
      <h1>{t('logs.title')}</h1>
    </div>
    <button
      class="btn download"
      onclick={download}
      disabled={!shown.length}
      aria-label={filtered && shown.length ? t('logs.downloadFiltered', { count: shown.length }) : t('logs.download')}
    >
      <Icon name="download" size={16} /><span class="label-text">{filtered && shown.length ? t('logs.downloadFiltered', { count: shown.length }) : t('logs.download')}</span>
    </button>
  </header>

  <div class="card toolbar">
    <div class="field">
      <label class="label" for="log-level">{t('logs.level')}</label>
      <select id="log-level" bind:value={minLevel}>
        <option value="debug">{t('logs.levels.all')}</option>
        <option value="info">{t('logs.levels.info')}</option>
        <option value="warn">{t('logs.levels.warn')}</option>
        <option value="error">{t('logs.levels.error')}</option>
      </select>
    </div>
    <div class="field">
      <label class="label" for="log-source">{t('logs.source')}</label>
      <select id="log-source" bind:value={source}>
        <option value="">{t('logs.allSources')}</option>
        <option value={SYSTEM}>{t('logs.system')}</option>
        {#if plugins.length}
          <optgroup label={t('nav.plugins')}>
            {#each plugins as plugin (plugin)}
              <option value={plugin}>{plugin}</option>
            {/each}
          </optgroup>
        {/if}
      </select>
    </div>
    <div class="field grow">
      <label class="label" for="log-search">{t('common.search')}</label>
      <input id="log-search" type="search" bind:value={query} placeholder={t('logs.searchHint')} />
    </div>
  </div>

  {#if error}
    <p class="load-error" role="alert"><Icon name="alert" size={18} /><span>{error}</span></p>
  {/if}

  <div class="card log-card">
    {#if entries === null}
      <p class="muted" role="status">{t('common.loading')}</p>
    {:else if shown.length === 0}
      <div class="empty">
        <Icon name="logs" size={32} />
        <p>{entries.length ? t('logs.noMatch') : t('logs.empty')}</p>
        {#if entries.length}
          <button class="btn" onclick={resetFilters}>{t('logs.resetFilters')}</button>
        {:else}
          <p class="small muted">{t('logs.emptyHint')}</p>
        {/if}
      </div>
    {:else}
      <!-- svelte-ignore a11y_no_noninteractive_tabindex (scrollable region, reachable by keyboard) -->
      <ol class="log" class:reading={!follow} bind:this={list} onscroll={onScroll} aria-label={t('logs.title')} tabindex="0">
        {#each shown as entry (entry.seq)}
          {@const date = new Date(entry.time)}
          <li class="entry {entry.level}">
            <time datetime={date.toISOString()} title={dateFormat.format(date)}
              >{date.toDateString() === today ? '' : `${dayFormat.format(date)} `}{timeFormat.format(date)}</time
            >
            <span class="level">{entry.level}</span>
            <span class="source" class:plugin={entry.plugin} title={sourceName(entry)}>{sourceName(entry)}</span>
            <span class="message">{entry.message}</span>
          </li>
        {/each}
      </ol>
      {#if !follow}
        <button class="btn jump" onclick={jumpToEnd}>
          <Icon name="down" size={16} />{unseen ? t('logs.jumpToEndCount', { count: unseen }) : t('logs.jumpToEnd')}
        </button>
      {/if}
    {/if}
  </div>
  <p class="visually-hidden" role="status">{announcement}</p>
  <p class="small muted note">{t('logs.note', { count: MAX_ENTRIES })}</p>
</div>

<style>
  .heading {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
    min-width: 0;
  }
  .download {
    margin-left: auto;
  }
  .back {
    display: inline-flex;
    align-items: center;
    gap: var(--space-1);
    color: var(--text-muted);
    text-decoration: none;
  }
  .back:hover {
    color: var(--text);
  }
  .toolbar {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-3);
    align-items: flex-end;
  }
  .toolbar .field {
    flex: 0 1 200px;
  }
  .toolbar .grow {
    flex: 1 1 240px;
  }
  .load-error {
    display: flex;
    gap: var(--space-2);
    align-items: flex-start;
    margin: 0 0 var(--space-3);
    padding: var(--space-3) var(--space-4);
    border-radius: var(--radius);
    background: var(--danger-soft);
    color: var(--danger);
  }
  .log-card {
    position: relative;
    padding: 0;
    overflow: hidden;
  }
  .log-card > p,
  .empty {
    padding: var(--space-5);
  }
  .empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--space-2);
    color: var(--text-muted);
    text-align: center;
  }
  .empty p {
    margin: 0;
  }
  .log {
    list-style: none;
    margin: 0;
    padding: var(--space-2) 0;
    max-height: min(65dvh, 720px);
    overflow-y: auto;
    font-family: var(--font-mono, ui-monospace, SFMono-Regular, Menlo, Consolas, monospace);
    font-size: var(--text-sm);
    line-height: 1.5;
  }
  /* Room for the "jump to newest" button below the last row. */
  .log.reading {
    padding-bottom: 64px;
  }
  .log:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }
  .entry {
    overflow: hidden;
    display: grid;
    grid-template-columns: auto 6ch minmax(0, 16ch) minmax(0, 1fr);
    gap: var(--space-3);
    padding: 2px var(--space-4);
    align-items: baseline;
  }
  .entry:hover {
    background: var(--surface-2);
  }
  time {
    color: var(--text-muted);
    font-variant-numeric: tabular-nums;
  }
  .level {
    font-weight: 700;
    font-size: var(--text-xs);
    text-transform: uppercase;
    color: var(--text-muted);
  }
  .warn {
    background: color-mix(in srgb, var(--warning-soft) 55%, transparent);
  }
  .warn .level {
    color: var(--warning);
  }
  .error {
    background: color-mix(in srgb, var(--danger-soft) 60%, transparent);
  }
  .error .level {
    color: var(--danger);
  }
  .debug .message {
    color: var(--text-muted);
  }
  .source {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--text-muted);
  }
  .source.plugin {
    color: var(--accent);
  }
  .message {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .jump {
    position: absolute;
    right: var(--space-4);
    bottom: var(--space-4);
    box-shadow: var(--shadow);
  }
  .note {
    margin-top: var(--space-2);
  }
  /* Phones: time, level and source above the message. */
  @media (max-width: 600px) {
    .entry {
      grid-template-columns: auto auto minmax(0, 1fr);
      gap: 0 var(--space-2);
      padding: var(--space-1) var(--space-3);
      border-bottom: 1px solid var(--border);
    }
    .message {
      grid-column: 1 / -1;
    }
    .toolbar .field {
      flex: 1 1 140px;
    }
    .log {
      max-height: 60dvh;
    }
    .download .label-text {
      display: none;
    }
  }
</style>
