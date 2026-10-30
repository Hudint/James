<script lang="ts">
  import { onDestroy, onMount, tick } from 'svelte';
  import { flip } from 'svelte/animate';
  import { beforeNavigate, goto } from '$app/navigation';
  import { api } from '$lib/api';
  import Icon from '$lib/components/Icon.svelte';
  import PluginSettings from '$lib/components/PluginSettings.svelte';
  import { confirmAction } from '$lib/confirm.svelte';
  import { i18n, locales, t } from '$lib/i18n/index.svelte';
  import { rise, reducedMotion } from '$lib/motion';
  import { reportError, toast } from '$lib/session.svelte';
  import type { InstanceSummary, PlatformChannel, PluginInfo, PluginOverview, PluginPermission } from '$lib/types';

  /** Status refresh while the page is open (crashes, instances starting). */
  const POLL_MS = 10_000;

  let overview = $state<PluginOverview | null>(null);
  let loadFailed = $state(false);
  /** A refresh failed after the page had loaded (shown inline, not as repeated toasts). */
  let stale = $state(false);
  let instances = $state<InstanceSummary[]>([]);
  let reloading = $state(false);
  let busy = $state<string | null>(null);

  /** The instance the page shows: plugins are activated and configured per instance. */
  const INSTANCE_KEY = 'james.plugins.instance';
  let selected = $state('');

  // Permission dialog: activating on the selected instance, an update's new permissions, or
  // the optional permissions of an active plugin.
  let dialog: HTMLDialogElement | undefined = $state();
  let enabling = $state<PluginInfo | null>(null);
  let mode = $state<'activate' | 'review' | 'permissions'>('activate');
  /** Optional permissions ticked in the dialog. */
  let chosenOptional = $state<string[]>([]);

  // Settings panels (for the selected instance); several can be open at once.
  let open = $state<string[]>([]);
  /** Panels opened at least once: they stay mounted (and keep their edits) while collapsed. */
  let mounted = $state<string[]>([]);
  /** Panels with unsaved edits. */
  let dirtyPanels = $state<Record<string, boolean>>({});
  /** Channels of the selected instance while it is connected (for `channel` settings). */
  let channels = $state<PlatformChannel[] | null>(null);
  let channelRequest = 0;
  async function loadChannels() {
    const request = ++channelRequest;
    const instance = selected;
    // Not the previous instance's channels meanwhile.
    channels = null;
    const loaded = instance ? await api.server(instance).then((s) => s.channels, () => null) : null;
    // Only the latest answer counts (the instance may have changed meanwhile).
    if (request === channelRequest) channels = loaded;
  }
  /** Updates the channel list without clearing it first (no flicker). */
  async function refreshChannels() {
    const request = ++channelRequest;
    // A failed poll keeps the list (the controls do not flip while someone edits).
    const loaded = selected ? await api.server(selected).then((s) => s.channels, () => undefined) : null;
    if (request === channelRequest && loaded !== undefined) channels = loaded;
  }
  const dirtyIds = $derived(mounted.filter((id) => dirtyPanels[id]));
  const dirty = $derived(dirtyIds.length > 0);
  /** Unsaved edits in collapsed plugins (open ones show their own save bar). */
  const hiddenDirty = $derived(dirtyIds.filter((id) => !open.includes(id)));

  async function load() {
    try {
      [overview, instances] = await Promise.all([api.plugins(), api.instances()]);
      if (!instances.some((i) => i.id === selected)) pickInstance(rememberedInstance());
      prune();
      loadFailed = false;
      stale = false;
    } catch {
      if (overview === null) loadFailed = true;
      else stale = true;
    }
  }

  /** Forgets panels of plugins that are gone (e.g. removed on reload). */
  function prune() {
    const ids = new Set((overview?.plugins ?? []).map((p) => p.manifest.id));
    mounted = mounted.filter((id) => ids.has(id));
    open = open.filter((id) => ids.has(id));
    dirtyPanels = Object.fromEntries(Object.entries(dirtyPanels).filter(([id]) => ids.has(id)));
  }

  let leaving = false;
  beforeNavigate((navigation) => {
    if (!dirty || leaving) return;
    // Reload or closing the tab: only the browser's own prompt is possible.
    if (navigation.type === 'leave' || !navigation.to) {
      navigation.cancel();
      return;
    }
    navigation.cancel();
    const target = navigation.to.url;
    void confirmAction(t('plugins.discardChanges'), t('common.discard')).then((ok) => {
      if (!ok) return;
      leaving = true;
      if (target.origin === location.origin) void goto(target).finally(() => (leaving = false));
      else location.href = target.href;
    });
  });

  let timer: ReturnType<typeof setInterval> | undefined;
  onMount(() => {
    void load();
    timer = setInterval(() => {
      if (!document.hidden && !dialog?.open) void load();
      // The instance may have connected or disconnected meanwhile.
      if (!document.hidden && mounted.length) void refreshChannels();
    }, POLL_MS);
  });
  onDestroy(() => clearInterval(timer));

  const ORDER: Record<string, number> = { failed: 0, needsApproval: 1, unavailable: 2, starting: 3, running: 4, disabled: 5 };
  const visiblePlugins = $derived(
    (overview?.plugins ?? [])
      .filter((p) => !p.manifest.hidden)
      .sort(
        (a, b) =>
          ORDER[a.status.state] - ORDER[b.status.state] ||
          a.manifest.name.localeCompare(b.manifest.name),
      ),
  );
  const activeList = $derived(visiblePlugins.filter((p) => activeHere(p)));
  const availableList = $derived(visiblePlugins.filter((p) => !activeHere(p)));

  function rememberedInstance(): string {
    const fromUrl = new URLSearchParams(location.search).get('instance');
    let stored: string | null = null;
    try {
      stored = localStorage.getItem(INSTANCE_KEY);
    } catch {
      // Storage blocked: the first instance.
    }
    return [fromUrl, stored].find((id) => id && instances.some((i) => i.id === id)) ?? instances[0]?.id ?? '';
  }

  function pickInstance(id: string) {
    selected = id;
    const url = new URL(location.href);
    if (id && url.searchParams.get('instance') !== id) {
      url.searchParams.set('instance', id);
      history.replaceState(history.state, '', url);
    }
    try {
      localStorage.setItem(INSTANCE_KEY, id);
    } catch {
      // Only a convenience.
    }
    // Open panels reload for the new instance (they are keyed by it).
    dirtyPanels = {};
    if (mounted.length) void loadChannels();
  }

  async function changeSelected(select: HTMLSelectElement) {
    const next = select.value;
    if (dirty && !(await confirmAction(t('plugins.discardChanges'), t('common.discard')))) {
      select.value = selected;
      return;
    }
    pickInstance(next);
  }

  /** Another plugin has the same name (the id tells them apart). */
  function sameName(plugin: PluginInfo): boolean {
    const name = plugin.manifest.name.toLowerCase();
    return visiblePlugins.some((p) => p !== plugin && p.manifest.name.toLowerCase() === name);
  }

  /** Activated on the selected instance (whether or not that instance runs); `autorun` plugins run on all. */
  function activeHere(plugin: PluginInfo): boolean {
    if (!plugin.state.enabled) return false;
    return plugin.manifest.autorun || plugin.state.instances === null || plugin.state.instances.includes(selected);
  }

  /** Instances the plugin is activated on. */
  function activatedOn(plugin: PluginInfo): { id: string; name: string }[] {
    if (!plugin.state.enabled) return [];
    const ids = plugin.state.instances ?? instances.map((i) => i.id);
    return ids.filter((id) => instances.some((i) => i.id === id)).map((id) => ({ id, name: instanceName(id) }));
  }

  /** Shows another instance (asks first if settings are unsaved). */
  async function switchTo(id: string) {
    if (dirty && !(await confirmAction(t('plugins.discardChanges'), t('common.discard')))) return;
    pickInstance(id);
  }

  const RISK: Record<string, number> = { high: 0, medium: 1, low: 2 };

  function permission(id: string): PluginPermission | undefined {
    return overview?.permissions.find((p) => p.id === id);
  }

  function byRisk(ids: string[]): string[] {
    return [...ids].sort((a, b) => (RISK[permission(a)?.risk ?? 'high'] ?? 0) - (RISK[permission(b)?.risk ?? 'high'] ?? 0));
  }

  /** Localised description of a permission (falls back to the server's English text). */
  function describe(id: string): string {
    const texts = locales[i18n.locale].plugins.perm as Record<string, string>;
    return texts[id] ?? permission(id)?.description ?? t('plugins.unknownPermission');
  }

  function permName(id: string): string {
    return (locales[i18n.locale].plugins.permName as Record<string, string>)[id] ?? id;
  }

  function riskTone(id: string): string {
    const risk = permission(id)?.risk ?? 'high';
    return risk === 'high' ? 'danger' : risk === 'medium' ? 'warning' : '';
  }

  function riskLabel(id: string): string {
    return t(`plugins.risk.${permission(id)?.risk ?? 'high'}` as 'plugins.risk.high');
  }

  function instanceName(id: string): string {
    return instances.find((i) => i.id === id)?.name ?? id;
  }

  async function reload() {
    reloading = true;
    try {
      await api.reloadPlugins();
      await load();
      toast(t('plugins.reloaded'), 'success');
    } catch (e) {
      reportError(e);
    } finally {
      reloading = false;
    }
  }

  async function openDialog(plugin: PluginInfo, kind: typeof mode) {
    enabling = plugin;
    mode = kind;
    chosenOptional = optionalOf(plugin.manifest).filter((p) => plugin.state.grantedPermissions.includes(p));
    // The content must exist before showModal() so focus lands on Cancel.
    await tick();
    dialog?.showModal();
  }

  /** Back on the plugin's switch (its row may have moved between the lists). */
  async function focusCard(id: string) {
    await tick();
    (document.getElementById(`switch-${id}`) ?? document.getElementById(`plugin-${id}`))?.focus();
  }

  /** Runs a change of `plugin` and shows its outcome. */
  async function change(plugin: PluginInfo, run: () => Promise<PluginInfo>, success: string) {
    busy = plugin.manifest.id;
    try {
      const updated = await run();
      replace(updated);
      if (updated.status.state === 'failed') toast(t('plugins.startFailed', { error: updated.status.error }), 'error');
      else toast(success, 'success');
    } catch (e) {
      reportError(e);
    } finally {
      busy = null;
      void focusCard(plugin.manifest.id);
    }
  }

  /** Whether activating asks first: permissions not granted yet, or optional ones never chosen. */
  function needsDialog(plugin: PluginInfo): boolean {
    const granted = plugin.state.grantedPermissions;
    if (plugin.manifest.permissions.some((p) => !granted.includes(p))) return true;
    return optionalOf(plugin.manifest).length > 0 && !plugin.state.enabled && granted.length === 0;
  }

  async function activate(plugin: PluginInfo) {
    if (needsDialog(plugin) || plugin.manifest.autorun) return openDialog(plugin, 'activate');
    const name = plugin.manifest.name;
    const instance = instanceName(selected);
    const granted = plugin.state.grantedPermissions.map(permName).join(', ');
    await change(
      plugin,
      () => api.setPluginActive(selected, plugin.manifest.id, { active: true }),
      granted ? t('plugins.enabledWith', { name, instance, permissions: granted }) : t('plugins.enabled', { name, instance }),
    );
  }

  async function confirmDialog() {
    if (!enabling) return;
    const plugin = enabling;
    const permissions = [...plugin.manifest.permissions, ...chosenOptional];
    const name = plugin.manifest.name;
    dialog?.close();
    if (mode === 'activate' && plugin.manifest.autorun) {
      await change(plugin, () => api.configurePlugin(plugin.manifest.id, { enabled: true, permissions, instances: null }), t('plugins.enabledEverywhere', { name }));
    } else if (mode === 'activate') {
      await change(plugin, () => api.setPluginActive(selected, plugin.manifest.id, { active: true, permissions }), t('plugins.enabled', { name, instance: instanceName(selected) }));
    } else {
      // Review and permissions keep where the plugin runs.
      await change(
        plugin,
        () => api.setPluginPermissions(plugin.manifest.id, permissions, mode === 'review'),
        t(mode === 'review' ? 'plugins.restarted' : 'plugins.permissionsSaved', { name }),
      );
    }
  }

  async function deactivate(plugin: PluginInfo) {
    const name = plugin.manifest.name;
    if (plugin.manifest.autorun) {
      if (!(await confirmAction(t('plugins.disableEverywhereConfirm', { name }), t('plugins.disable')))) return;
      await change(plugin, () => api.configurePlugin(plugin.manifest.id, { enabled: false, permissions: plugin.state.grantedPermissions, instances: null }), t('plugins.disabledEverywhere', { name }));
      return;
    }
    const instance = instanceName(selected);
    if (!(await confirmAction(t('plugins.disableConfirm', { name, instance }), t('plugins.disable')))) return;
    await change(plugin, () => api.setPluginActive(selected, plugin.manifest.id, { active: false }), t('plugins.disabled', { name, instance }));
  }

  /** Re-applies the current configuration, which reloads a failed plugin. */
  async function retry(plugin: PluginInfo) {
    await change(
      plugin,
      () => api.setPluginPermissions(plugin.manifest.id, plugin.state.grantedPermissions, true),
      t('plugins.restarted', { name: plugin.manifest.name }),
    );
  }

  function replace(updated: PluginInfo) {
    if (!overview) return;
    overview.plugins = overview.plugins.map((p) => (p.manifest.id === updated.manifest.id ? updated : p));
  }

  /** Expands or collapses a plugin (several can be open; collapsed ones keep their edits). */
  function toggle(plugin: PluginInfo) {
    const id = plugin.manifest.id;
    if (open.includes(id)) {
      open = open.filter((o) => o !== id);
      return;
    }
    if (mounted.length === 0) void loadChannels();
    if (!mounted.includes(id)) mounted = [...mounted, id];
    open = [...open, id];
  }

  /** Opens the plugins with unsaved settings and moves to the first. */
  async function showUnsaved() {
    if (!dirtyIds.length) return;
    filter = 'all';
    query = '';
    const first = hiddenDirty[0] ?? dirtyIds[0];
    open = [...new Set([...open, ...dirtyIds])];
    await tick();
    const row = first ? document.getElementById(`row-${first}`) : null;
    row?.scrollIntoView({ behavior: reducedMotion() ? 'auto' : 'smooth', block: 'start' });
    document.getElementById(`plugin-${first}`)?.focus({ preventScroll: true });
  }

  /** The switch: activates or deactivates on the selected instance (it may ask first). */
  async function flipActive(plugin: PluginInfo, input: HTMLInputElement, blocked: boolean) {
    // The switch shows the real state until the change went through.
    input.checked = activeHere(plugin);
    // Not `disabled`: the switch stays focusable, so its reason (described) can be heard.
    if (blocked) return;
    if (activeHere(plugin)) await deactivate(plugin);
    else await activate(plugin);
  }

  type Filter = 'all' | 'active' | 'available' | 'problems';
  let filter = $state<Filter>('all');
  let query = $state('');

  function problem(plugin: PluginInfo): boolean {
    const state = plugin.status.state;
    return state === 'needsApproval' || ((state === 'failed' || state === 'unavailable') && activeHere(plugin));
  }

  const counts = $derived({
    all: visiblePlugins.length,
    active: activeList.length,
    available: availableList.length,
    problems: visiblePlugins.filter(problem).length,
  });

  /** Every plugin stays rendered (collapsed panels keep their edits); the filter only hides rows. */
  const ordered = $derived([...activeList, ...availableList]);
  const shown = $derived.by(() => {
    const words = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
    const base =
      filter === 'active' ? activeList
      : filter === 'available' ? availableList
      : filter === 'problems' ? visiblePlugins.filter(problem)
      : [...activeList, ...availableList];
    if (!words.length) return base;
    return base.filter((p) => {
      const text = `${p.manifest.name} ${p.manifest.description} ${p.manifest.author} ${p.manifest.id}`.toLowerCase();
      return words.every((w) => text.includes(w));
    });
  });

  // The last problem went away (or another instance has none): its button is gone too.
  $effect(() => {
    if (filter === 'problems' && !counts.problems) filter = 'all';
  });
  const shownIds = $derived(new Set(shown.map((p) => p.manifest.id)));

  const FILTERS: { id: Filter; label: 'plugins.filterAll' | 'plugins.filterActive' | 'plugins.filterAvailable' | 'plugins.filterProblems' }[] = [
    { id: 'all', label: 'plugins.filterAll' },
    { id: 'active', label: 'plugins.filterActive' },
    { id: 'available', label: 'plugins.filterAvailable' },
    { id: 'problems', label: 'plugins.filterProblems' },
  ];

  /** The most important permission plus how many more (row summary). */
  function permSummary(m: PluginInfo['manifest']): { name: string; more: number; high: boolean } | null {
    const all = byRisk([...m.permissions, ...optionalOf(m)]);
    if (!all.length) return null;
    return { name: permName(all[0]), more: all.length - 1, high: permission(all[0])?.risk === 'high' };
  }

  /** Rows move to their new place; a row that was hidden just appears (it has no old place). */
  function move(node: Element, rects: { from: DOMRect; to: DOMRect }) {
    const still = reducedMotion() || !rects.from.width || !rects.to.width;
    return flip(node, rects, { duration: still ? 0 : 260 });
  }

  /** The plugin's state on the selected instance. */
  function statusBadge(plugin: PluginInfo): { label: string; tone: string } {
    if (plugin.status.state !== 'needsApproval' && !activeHere(plugin)) return { label: t('plugins.status.inactive'), tone: '' };
    if (plugin.status.state === 'running' && !plugin.activeInstances.includes(selected)) {
      return { label: t('plugins.status.instanceStopped'), tone: 'warning' };
    }
    switch (plugin.status.state) {
      case 'running':
        return { label: t('plugins.status.running'), tone: 'success' };
      case 'starting':
        return { label: t('plugins.status.starting'), tone: 'accent' };
      case 'needsApproval':
        return { label: t('plugins.status.needsApproval'), tone: 'warning' };
      case 'failed':
        return { label: t('plugins.status.failed'), tone: 'danger' };
      case 'unavailable':
        return { label: t('plugins.status.unavailable'), tone: 'warning' };
      default:
        return { label: t('plugins.status.disabled'), tone: '' };
    }
  }

  /** A page link the popup blocker stopped (shown for the user to click). */
  let blockedPage = $state<{ plugin: string; url: string } | null>(null);

  /** Opens a plugin page (sandboxed) with a token for one instance in the URL fragment. */
  async function openPage(plugin: PluginInfo, instance: string) {
    // Opened right away (popup blockers), the address follows once the token is there.
    const tab = window.open('about:blank', '_blank');
    try {
      const { token } = await api.pluginToken(instance, plugin.manifest.id);
      const url = `/plugins/${encodeURIComponent(plugin.manifest.id)}/#instance=${encodeURIComponent(instance)}&token=${encodeURIComponent(token)}`;
      if (tab) {
        tab.opener = null;
        tab.location.href = url;
      } else {
        blockedPage = { plugin: plugin.manifest.id, url };
        await tick();
        document.getElementById(`blocked-${plugin.manifest.id}`)?.focus();
        // The token in the link expires; do not keep it around.
        setTimeout(() => {
          if (blockedPage?.url === url) blockedPage = null;
        }, 60_000);
      }
    } catch (e) {
      tab?.close();
      reportError(e);
    }
  }

  /** Permissions requested but not granted yet (after an update). */
  function newPermissions(plugin: PluginInfo): string[] {
    return plugin.status.state === 'needsApproval' ? plugin.status.missing : [];
  }

  function optionalOf(manifest: PluginInfo['manifest']): string[] {
    return (manifest.optionalPermissions ?? []).filter((p) => !manifest.permissions.includes(p));
  }

  const dialogPermissions = $derived(enabling ? byRisk(enabling.manifest.permissions) : []);
  const dialogOptional = $derived(enabling ? byRisk(optionalOf(enabling.manifest)) : []);
  const enforced = $derived(dialogPermissions.filter((id) => permission(id)?.enforced));
  const unenforced = $derived(dialogPermissions.filter((id) => permission(id) && !permission(id)!.enforced));
  /** Optional permissions granted in the dialog that are not granted yet. */
  const newlyGranted = $derived(enabling ? chosenOptional.filter((p) => !enabling!.state.grantedPermissions.includes(p)) : []);
  /** Optional permissions unticked in the dialog that are granted now. */
  const revoked = $derived(enabling ? dialogOptional.filter((p) => enabling!.state.grantedPermissions.includes(p) && !chosenOptional.includes(p)) : []);
  const highRisk = $derived([...dialogPermissions, ...newlyGranted].some((id) => permission(id)?.risk === 'high'));

</script>

<svelte:head><title>{t('plugins.title')} · James</title></svelte:head>

<div class="page">
  <header class="page-header">
    <h1>{t('plugins.title')}</h1>
    <div class="header-actions">
      <button class="btn" onclick={reload} disabled={reloading || overview?.host.state !== 'running'}>
        <Icon name="refresh" size={16} />{reloading ? t('common.loading') : t('plugins.reload')}
      </button>
    </div>
    {#if overview && selected}
      <p class="lede">{t('plugins.lede', { active: activeList.length, instance: instanceName(selected), count: visiblePlugins.length })}</p>
    {/if}
  </header>

  {#if loadFailed}
    <div class="card empty" role="alert">
      <Icon name="alert" size={32} />
      <p>{t('plugins.loadFailed')}</p>
      <button class="btn" onclick={load}><Icon name="refresh" size={16} />{t('common.retry')}</button>
    </div>
  {:else if overview === null}
    <div class="card list-panel" aria-busy="true">
      {#each [0, 1, 2, 3] as i (i)}<div class="skeleton-row"></div>{/each}
    </div>
  {:else}
    {#if stale}
      <div class="notice warning" role="status"><Icon name="alert" /><span>{t('plugins.stale')}</span></div>
    {/if}
    {#if overview!.host.state === 'unavailable'}
      <div class="notice danger" role="alert">
        <Icon name="alert" />
        <div>
          <strong>{t('plugins.hostUnavailable')}</strong>
          <p class="small">{overview.host.error}</p>
        </div>
      </div>
    {:else if overview!.host.state === 'starting'}
      <div class="notice" role="status"><Icon name="refresh" /><span>{t('plugins.hostStarting')}</span></div>
    {/if}

    {#if overview.errors.length > 0}
      <div class="notice warning" role="alert">
        <Icon name="alert" />
        <div>
          <strong>{t('plugins.loadErrors')}</strong>
          <ul class="small">
            {#each overview.errors as error (error.id)}<li><code>{error.id}</code>: {error.error}</li>{/each}
          </ul>
        </div>
      </div>
    {/if}

    {#if instances.length === 0}
      <div class="notice" role="status"><Icon name="server" /><span>{t('plugins.noInstances')}</span></div>
    {/if}

    {#if visiblePlugins.length === 0}
      <div class="card empty">
        <Icon name="puzzle" size={32} />
        <p>{t('plugins.empty')}</p>
        <p class="hint">{t('plugins.emptyHint')}</p>
        {#if overview.pluginsDir}<code class="path">{overview.pluginsDir}</code>{/if}
      </div>
    {:else}
      <div class="toolbar">
        {#if instances.length}
          <label class="instance-pick">
            <span class="pick-label">{t('plugins.instance')}</span>
            <select value={selected} onchange={(e) => changeSelected(e.currentTarget)}>
              {#each instances as instance (instance.id)}<option value={instance.id}>{instance.name}</option>{/each}
            </select>
          </label>
        {/if}
        <label class="search">
          <Icon name="search" size={16} />
          <span class="visually-hidden">{t('plugins.search')}</span>
          <input type="search" placeholder={t('plugins.search')} bind:value={query} />
        </label>
        <div class="segmented" role="group" aria-label={t('plugins.filter')}>
          {#each FILTERS as f (f.id)}
            {#if f.id !== 'problems' || counts.problems}
              <button type="button" aria-pressed={filter === f.id} onclick={() => (filter = f.id)}>
                {#if f.id === 'problems'}<span class="dot problem-dot" aria-hidden="true"></span>{/if}
                {t(f.label)} <span class="count">{counts[f.id]}</span>
              </button>
            {/if}
          {/each}
        </div>
        {#if open.length > 1}
          <button class="btn btn-ghost btn-small collapse-all" type="button" onclick={() => (open = [])}>{t('plugins.collapseAll')}</button>
        {/if}
      </div>

      <ul class="card list-panel plugins" hidden={!shown.length}>
        {#each ordered as plugin (plugin.manifest.id)}
          <li
            class="plugin"
            class:expanded={open.includes(plugin.manifest.id)}
            class:on={activeHere(plugin)}
            id="row-{plugin.manifest.id}"
            hidden={!shownIds.has(plugin.manifest.id)}
            animate:move
            in:rise
          >
            {@render row(plugin)}
          </li>
        {/each}
      </ul>
      {#if !shown.length}
        <div class="card empty">
          <Icon name="search" size={28} />
          <p>{filter === 'active' && !query ? t('plugins.noneActive', { instance: instanceName(selected) }) : t('plugins.noMatch')}</p>
          {#if query || filter !== 'all'}
            <button class="btn" type="button" onclick={() => { query = ''; filter = 'all'; }}>{t('plugins.showAll')}</button>
          {/if}
        </div>
      {/if}
    {/if}
  {/if}
</div>

<p class="visually-hidden" role="status">{hiddenDirty.length ? t('plugins.unsavedCount', { count: hiddenDirty.length }) : ''}</p>
{#if hiddenDirty.length}
  <div class="dock" transition:rise={{ y: 24 }}>
    <span class="dot" aria-hidden="true"></span>
    <span aria-hidden="true">{t('plugins.unsavedCount', { count: hiddenDirty.length })}</span>
    <button class="btn btn-small" type="button" onclick={showUnsaved}>{t('plugins.showUnsaved')}</button>
  </div>
{/if}

{#snippet row(plugin: PluginInfo)}
  {@const m = plugin.manifest}
  {@const badge = statusBadge(plugin)}
  {@const added = newPermissions(plugin)}
  {@const here = activeHere(plugin)}
  {@const others = activatedOn(plugin).filter((other) => other.id !== selected)}
  {@const isOpen = open.includes(m.id)}
  {@const perms = permSummary(m)}
  {@const label = sameName(plugin) ? `${m.name} v${m.version} (${m.kind === 'sinusbot' ? t('plugins.sinusbot') : t('plugins.native')})` : m.name}
  {@const blocked = plugin.status.state === 'needsApproval' ? t('plugins.status.needsApproval') : overview!.host.state !== 'running' && !here ? t('plugins.hostUnavailable') : !selected ? t('plugins.noInstances') : ''}
  <div class="head">
    <input
      type="checkbox"
      class="switch"
      role="switch"
      id="switch-{m.id}"
      checked={here}
      aria-disabled={busy === m.id || !!blocked}
      aria-busy={busy === m.id}
      title={blocked || undefined}
      aria-label={m.autorun ? t('plugins.switchEverywhere', { name: label }) : t('plugins.switchLabel', { name: label, instance: instanceName(selected) })}
      aria-describedby={blocked ? `blocked-reason-${m.id}` : undefined}
      onchange={(e) => flipActive(plugin, e.currentTarget, busy === m.id || !!blocked)}
    />
    {#if blocked}<span class="visually-hidden" id="blocked-reason-{m.id}">{blocked}</span>{/if}
    <div class="text">
      <div class="name-line">
        <h3><button class="toggle" id="plugin-{m.id}" type="button" aria-label={label === m.name ? undefined : label} aria-expanded={isOpen} aria-controls="body-{m.id}" onclick={() => toggle(plugin)}>{m.name}</button></h3>
        {#if m.version}<span class="mono muted version">v{m.version}</span>{/if}
        {#if m.kind === 'sinusbot'}<span class="badge accent">{t('plugins.sinusbot')}</span>{:else if sameName(plugin)}<span class="badge">{t('plugins.native')}</span>{/if}
        {#if m.autorun}<span class="badge">{t('plugins.autorun')}</span>{/if}
        {#if dirtyPanels[m.id]}<span class="badge live">{t('plugins.unsaved')}</span>{/if}
      </div>
      {#if m.description}<p class="description truncate">{m.description}</p>{/if}
    </div>
    <div class="meta">
      {#if perms}
        <span class="perm-summary">{perms.name}{#if perms.more}<span class="more">+{perms.more}</span>{/if}</span>
      {/if}
      {#if blocked && plugin.status.state !== 'needsApproval'}
        <span class="status warning"><span class="dot"></span>{blocked}</span>
      {:else if here || plugin.status.state === 'needsApproval'}
        <span class="status {badge.tone === 'success' ? 'success' : badge.tone === 'accent' ? 'live' : badge.tone}"><span class="dot"></span>{badge.label}</span>
      {/if}
    </div>
    <span class="chevron" aria-hidden="true"><Icon name="down" size={18} /></span>
  </div>

  {#if plugin.status.state === 'failed' && here}
    <div class="notice danger inline" role="alert">
      <Icon name="alert" size={18} />
      <div><strong>{t('plugins.failedTitle')}</strong><p class="small">{plugin.status.error}</p></div>
      <button class="btn btn-small" onclick={() => retry(plugin)} disabled={busy === m.id || overview!.host.state !== 'running'}>
        <Icon name="refresh" size={16} />{t('plugins.retry')}
      </button>
    </div>
  {:else if added.length}
    <div class="notice warning inline" role="status">
      <Icon name="alert" size={18} />
      <p class="small">{t('plugins.newPermissions', { count: added.length })}</p>
      <button class="btn btn-small btn-primary" onclick={() => openDialog(plugin, 'review')} disabled={busy === m.id || overview!.host.state !== 'running'}>
        {t('plugins.review')}
      </button>
    </div>
  {/if}

  <div class="collapse" class:open={isOpen} id="body-{m.id}" inert={!isOpen}>
    <div>
      {#if mounted.includes(m.id)}
        <div class="body">
          {#if plugin.status.state === 'unavailable' && here}
            <p class="small muted">{t('plugins.unavailableHint')}</p>
          {/if}
          <dl class="facts small">
            {#if m.author}
              <dt>{t('plugins.author')}</dt>
              <dd>{m.author}</dd>
            {/if}
            <dt>{t('plugins.id')}</dt>
            <dd><code>{m.id}</code></dd>
            {#if m.permissions.length || optionalOf(m).length}
              <dt>{t('plugins.permissions')}</dt>
              <dd>
                <ul class="perm-list">
                  {#each byRisk([...m.permissions, ...optionalOf(m)]) as id (id)}
                    {@const optional = !m.permissions.includes(id)}
                    {@const notGranted = optional && plugin.state.enabled && !plugin.state.grantedPermissions.includes(id)}
                    {@const status = optional ? (notGranted ? `${t('common.optional')}, ${t('plugins.notGranted')}` : t('common.optional')) : ''}
                    <li>
                      <span class="badge {riskTone(id)}" class:not-granted={notGranted} title={riskLabel(id)}>
                        {#if permission(id)?.risk === 'high'}<Icon name="alert" size={12} />{/if}{permName(id)}<span class="visually-hidden">, {riskLabel(id)}{status ? `, ${status}` : ''}</span>
                      </span>
                      {#if added.includes(id)}<span class="badge live">{t('plugins.new')}</span>{/if}
                      {#if optional}<span class="small muted" aria-hidden="true">{status}</span>{/if}
                    </li>
                  {/each}
                </ul>
              </dd>
            {/if}
            {#if others.length && !m.autorun}
              <dt>{t('plugins.alsoActiveOn')}</dt>
              <dd class="others">
                {#each others as other (other.id)}
                  <button class="link" type="button" onclick={() => switchTo(other.id)}>{other.name}</button>
                {/each}
              </dd>
            {/if}
            {#if plugin.commands.length}
              <dt>{t('plugins.commands')}</dt>
              <dd class="mono">{plugin.commands.join(', ')}</dd>
            {/if}
          </dl>

          {#if (plugin.state.enabled && optionalOf(m).length && plugin.status.state !== 'needsApproval') || (m.web && plugin.status.state === 'running' && plugin.activeInstances.includes(selected)) || blockedPage?.plugin === m.id}
            <div class="row actions">
              {#if plugin.state.enabled && optionalOf(m).length && plugin.status.state !== 'needsApproval'}
                <button class="btn btn-small" onclick={() => openDialog(plugin, 'permissions')} disabled={busy === m.id || overview!.host.state !== 'running'}>
                  <Icon name="key" size={16} />{t('plugins.permissionsButton')}
                </button>
              {/if}
              {#if m.web && plugin.status.state === 'running' && plugin.activeInstances.includes(selected)}
                <button class="btn btn-small" onclick={() => openPage(plugin, selected)}>
                  <Icon name="external" size={16} />{t('plugins.openPage')}
                </button>
              {/if}
              {#if blockedPage?.plugin === m.id}
                <a id="blocked-{m.id}" class="btn btn-small btn-ghost" href={blockedPage.url} target="_blank" rel="noopener" onclick={() => (blockedPage = null)}>
                  {t('plugins.popupBlocked')}
                </a>
              {/if}
            </div>
          {/if}

          {#if m.config.length && selected}
            <section class="settings-panel" aria-label={t('plugins.settings')}>
              {#key selected}
                <PluginSettings
                  {plugin}
                  instance={selected}
                  instanceName={instanceName(selected)}
                  {here}
                  {channels}
                  ondirty={(d) => {
                    if (!!dirtyPanels[m.id] !== d) dirtyPanels = { ...dirtyPanels, [m.id]: d };
                  }}
                />
              {/key}
            </section>
          {/if}
        </div>
      {/if}
    </div>
  </div>
{/snippet}

<dialog bind:this={dialog} aria-labelledby="enable-title" aria-describedby="enable-summary" onclose={() => (enabling = null)}>
  {#if enabling}
    {@const m = enabling.manifest}
    <h2 id="enable-title">
      {mode === 'activate'
        ? m.autorun
          ? t('plugins.enableEverywhereTitle', { name: m.name })
          : t('plugins.enableTitle', { name: m.name, instance: instanceName(selected) })
        : mode === 'review'
          ? t('plugins.reviewTitle', { name: m.name })
          : t('plugins.permissionsTitle', { name: m.name })}
    </h2>
    {#if mode !== 'permissions'}
      <div id="enable-summary">
        {#if m.permissions.length === 0 && dialogOptional.length === 0}
          <p>{t('plugins.noPermissions')}</p>
        {/if}
        {#if enforced.length}
          <h3 class="label">{t('plugins.enforcedTitle')}</h3>
          <ul class="grant-list">
            {#each enforced as id (id)}
              <li>
                <span class="badge {riskTone(id)}">{permName(id)}</span>
                <span>{describe(id)} <span class="risk {riskTone(id)}">({riskLabel(id)})</span></span>
              </li>
            {/each}
          </ul>
        {/if}
        {#if unenforced.length}
          <h3 class="label">{t('plugins.unenforcedTitle')}</h3>
          <p class="small muted">{t('plugins.unenforcedHint')}</p>
          <ul class="grant-list">
            {#each unenforced as id (id)}
              <li>
                <span class="badge {riskTone(id)}">{permName(id)}</span>
                <span>{describe(id)} <span class="risk {riskTone(id)}">({riskLabel(id)})</span></span>
              </li>
            {/each}
          </ul>
        {/if}
        <div class="notice inline {highRisk ? 'danger' : 'warning'}" role="note">
          <Icon name="alert" size={18} />
          <p class="small">{t('plugins.trustedCode')}</p>
        </div>
      </div>
    {:else}
      <p id="enable-summary">{t('plugins.permissionsHint')}</p>
    {/if}

    {#if dialogOptional.length}
      <fieldset class="optional" aria-describedby="optional-hint">
        <legend class="label">{t('plugins.optionalTitle')}</legend>
        <p class="small muted" id="optional-hint">{t('plugins.optionalHint')}</p>
        {#each dialogOptional as id (id)}
          <label class="checkbox optional-perm">
            <input type="checkbox" value={id} bind:group={chosenOptional} />
            <span class="badge {riskTone(id)}">{#if permission(id)?.risk === 'high'}<Icon name="alert" size={12} />{/if}{permName(id)}</span>
            <span>{describe(id)} <span class="risk {riskTone(id)}">({riskLabel(id)})</span></span>
          </label>
        {/each}
      </fieldset>
    {/if}

    {#if mode !== 'permissions'}
      <p class="small muted">{t('plugins.grantsEverywhere')}</p>
    {/if}

    {#if mode === 'permissions' && newlyGranted.some((id) => permission(id)?.risk === 'high')}
      <div class="notice inline danger" role="note">
        <Icon name="alert" size={18} />
        <p class="small">{t('plugins.trustedCode')}</p>
      </div>
    {/if}
    {#if revoked.length}
      <p class="small muted" aria-live="polite">{t('plugins.revokes', { names: revoked.map(permName).join(', ') })}</p>
    {/if}

    <div class="dialog-actions">
      <!-- svelte-ignore a11y_autofocus -->
      <button class="btn" onclick={() => dialog?.close()} autofocus>{t('common.cancel')}</button>
      <button class="btn btn-primary" onclick={confirmDialog}>
        {#if mode === 'permissions'}
          {newlyGranted.length ? t('plugins.grantAndSave') : t('common.save')}
        {:else if mode === 'review'}
          {t('plugins.grantAndRestart')}
        {:else}
          {m.permissions.length || chosenOptional.length ? t('plugins.grantAndEnable') : t('plugins.enable')}
        {/if}
      </button>
    </div>
  {/if}
</dialog>

<style>
  .header-actions {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    flex-wrap: wrap;
  }
  .instance-pick {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }
  .pick-label {
    font-size: var(--text-sm);
    color: var(--text-muted);
  }
  .instance-pick select {
    min-width: 10rem;
    font-weight: 600;
  }
  .toolbar {
    position: sticky;
    top: 0;
    z-index: 5;
    display: flex;
    align-items: center;
    gap: var(--space-3);
    flex-wrap: wrap;
    padding: var(--space-2) 0;
    margin: calc(-1 * var(--space-2)) 0;
    background: var(--bg);
  }
  .search {
    position: relative;
    flex: 1 1 16rem;
    max-width: 24rem;
    color: var(--text-muted);
  }
  .search :global(svg) {
    position: absolute;
    left: 11px;
    top: 50%;
    transform: translateY(-50%);
    pointer-events: none;
  }
  .search input {
    padding-left: 34px;
  }
  .problem-dot {
    background: var(--danger);
    width: 6px;
    height: 6px;
  }
  .collapse-all {
    margin-left: auto;
  }
  .list-panel {
    padding: 0;
    /* clip, not hidden: no scroll container, so the save bar inside can stick to the page. */
    overflow: clip;
  }
  .skeleton-row {
    height: 64px;
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
  .plugins {
    list-style: none;
    margin: 0;
  }
  /* Hairline above every row; the panel clips the first one (rows may be hidden by the filter). */
  .plugin {
    position: relative;
    box-shadow: 0 -1px 0 var(--border);
    background: var(--surface);
    transition: background var(--dur-2) var(--ease-out);
  }
  /* The left edge: orange (tally) while it is active here, navy while it is only open. */
  .plugin::before {
    content: '';
    position: absolute;
    left: 0;
    top: 0;
    bottom: 0;
    width: 3px;
    background: var(--tally);
    transform: scaleY(0);
    transform-origin: top;
    transition:
      transform var(--dur-3) var(--ease-out),
      background var(--dur-2) var(--ease-out);
  }
  .plugin.on::before {
    transform: scaleY(1);
  }
  .plugin.expanded:not(.on)::before {
    width: 2px;
    background: var(--navy);
    transform: scaleY(1);
  }
  .plugin.expanded {
    background: color-mix(in srgb, var(--surface-2) 45%, var(--surface));
  }
  .head {
    position: relative;
    display: flex;
    align-items: flex-start;
    gap: var(--space-3);
    min-height: 64px;
    padding: var(--space-3) var(--space-3) var(--space-3) var(--space-5);
    cursor: pointer;
  }
  /* Interactive or selectable parts of the head sit above the row's stretched toggle. */
  .head > .switch {
    position: relative;
    z-index: 1;
    margin-top: 2px;
  }
  .text {
    flex: 1 1 auto;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .name-line {
    display: flex;
    align-items: center;
    min-height: 28px;
    gap: var(--space-2);
    flex-wrap: wrap;
    min-width: 0;
  }
  .name-line h3 {
    font-size: var(--text-base);
    font-weight: 600;
  }
  .toggle {
    padding: 0;
    border: none;
    background: none;
    color: inherit;
    font: inherit;
    text-align: left;
    cursor: pointer;
  }
  /* The whole row opens the plugin (the switch stays on top). */
  .toggle::after {
    content: '';
    position: absolute;
    inset: 0;
  }
  .toggle:focus-visible {
    outline: none;
  }
  .toggle:focus-visible::after {
    outline: 2px solid var(--focus);
    outline-offset: -2px;
  }
  .version {
    font-size: var(--text-xs);
  }
  .description {
    max-width: 100%;
    color: var(--text-muted);
    font-size: var(--text-sm);
  }
  .meta {
    position: relative;
    z-index: 1;
    display: flex;
    align-items: center;
    gap: var(--space-4);
    flex-shrink: 0;
    min-height: 28px;
  }
  .perm-summary {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: var(--text-sm);
    color: var(--text-muted);
  }
  .perm-summary .more {
    margin-left: 4px;
    font-family: var(--mono);
    font-size: var(--text-xs);
  }
  .plugin.expanded .chevron {
    transform: rotate(180deg);
  }
  .chevron {
    display: grid;
    place-items: center;
    width: 32px;
    height: 32px;
    margin-top: -2px;
    flex-shrink: 0;
    border-radius: var(--radius);
    color: var(--text-muted);
    transition:
      transform var(--dur-2) var(--ease-out),
      background var(--dur-1) var(--ease-out);
  }
  @media (hover: hover) {
    .head:hover .chevron {
      background: var(--surface-2);
      color: var(--text);
    }
  }
  .body {
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
    padding: 0 var(--space-5) var(--space-5) calc(var(--space-5) + 36px + var(--space-3));
  }
  /* Open: the whole description instead of one line. */
  .plugin.expanded .description {
    white-space: normal;
    overflow: visible;
    display: block;
    -webkit-line-clamp: unset;
    line-clamp: unset;
  }
  .settings-panel {
    padding: var(--space-4);
    border-radius: var(--radius-lg);
    background: var(--surface);
    border: 1px solid var(--border);
  }
  .others {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-1) var(--space-3);
  }
  .link {
    border: 0;
    background: none;
    padding: 0;
    color: var(--navy);
    font: inherit;
    text-decoration: underline;
    cursor: pointer;
  }
  .facts {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: var(--space-2) var(--space-5);
    margin: 0;
  }
  .facts dt {
    color: var(--text-muted);
  }
  .facts dd {
    margin: 0;
    min-width: 0;
  }
  .perm-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    gap: var(--space-1) var(--space-2);
    flex-wrap: wrap;
  }
  .perm-list li {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-1);
  }
  .perm-list li > .muted {
    white-space: nowrap;
  }
  .notice {
    display: flex;
    gap: var(--space-3);
    align-items: flex-start;
    padding: var(--space-3) var(--space-4);
    border-radius: var(--radius-lg);
    border: 1px solid var(--border);
    background: var(--surface);
  }
  .notice.inline {
    align-items: center;
    margin: 0 var(--space-5) var(--space-3) calc(var(--space-5) + 36px + var(--space-3));
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius);
  }
  .notice.inline > :global(:nth-child(2)) {
    flex: 1;
  }
  .notice p,
  .notice ul {
    margin: var(--space-1) 0 0;
  }
  .notice.inline p {
    margin: 0;
  }
  .notice.danger {
    border-color: color-mix(in srgb, var(--danger) 45%, transparent);
    color: var(--danger);
    background: var(--danger-soft);
  }
  .notice.warning {
    border-color: color-mix(in srgb, var(--warning) 45%, transparent);
    background: var(--warning-soft);
  }
  .path {
    font-size: var(--text-sm);
    word-break: break-all;
  }
  /* Unsaved settings anywhere on the page (also in collapsed plugins). */
  .dock {
    position: fixed;
    z-index: 40;
    bottom: calc(var(--bottom-bar, 0px) + var(--space-4) + env(safe-area-inset-bottom));
    left: 50%;
    translate: -50% 0;
    display: flex;
    align-items: center;
    gap: var(--space-3);
    padding: var(--space-2) var(--space-2) var(--space-2) var(--space-4);
    border-radius: var(--radius-lg);
    background: var(--side-bg);
    color: var(--side-text);
    box-shadow: var(--shadow-lg);
    font-size: var(--text-sm);
    font-weight: 500;
    max-width: calc(100vw - 32px);
  }
  .dock .dot {
    background: var(--tally);
  }
  @media (min-width: 900px) {
    .dock {
      left: calc(50% + var(--sidebar-width) / 2);
    }
  }
  @media (max-width: 899px) {
    .toolbar {
      top: 56px;
    }
  }
  /* Phones: the toolbar would cover too much of the screen while sticky. */
  @media (max-width: 600px) {
    .toolbar {
      position: static;
      margin: 0;
      padding: 0;
    }
    .collapse-all {
      display: none;
    }
  }
  @media (max-width: 720px) {
    .meta .perm-summary {
      display: none;
    }
  }
  @media (max-width: 600px) {
    .head {
      padding-left: var(--space-3);
      gap: var(--space-2);
      align-items: flex-start;
    }
    .settings-panel {
      border: 0;
      padding: 0;
      background: none;
    }
    .meta {
      display: none;
    }
    .description {
      white-space: normal;
      display: -webkit-box;
      -webkit-line-clamp: 2;
      line-clamp: 2;
      -webkit-box-orient: vertical;
    }
    .body {
      padding: 0 var(--space-3) var(--space-4);
    }
    .notice.inline {
      margin: 0 var(--space-3) var(--space-3);
      flex-wrap: wrap;
    }
    .facts {
      grid-template-columns: 1fr;
      gap: 2px;
    }
    .facts dd {
      margin-bottom: var(--space-2);
    }
    .search {
      max-width: none;
      flex-basis: 100%;
    }
    .segmented {
      width: 100%;
      overflow-x: auto;
    }
    .instance-pick {
      flex-basis: 100%;
    }
    .instance-pick select {
      flex: 1;
      min-width: 0;
    }
  }
  dialog {
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--surface);
    color: var(--text);
    padding: var(--space-5);
    width: min(560px, calc(100vw - 32px));
    max-height: calc(100dvh - 32px);
    overflow-y: auto;
    box-shadow: var(--shadow);
  }
  dialog::backdrop {
    background: rgb(0 0 0 / 45%);
  }
  dialog h2 {
    margin: 0 0 var(--space-3);
    font-size: var(--text-lg);
  }
  dialog h3 {
    margin: var(--space-4) 0 var(--space-2);
  }
  .grant-list {
    list-style: none;
    padding: 0;
    margin: 0 0 var(--space-3);
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }
  .grant-list li {
    display: flex;
    gap: var(--space-2);
    align-items: baseline;
  }
  .risk {
    font-size: var(--text-xs);
    font-weight: 600;
  }
  .risk.danger {
    color: var(--danger);
  }
  .risk.warning {
    color: var(--warning);
  }
  /* Not granted: outlined instead of filled (keeps the text contrast). */
  .badge.not-granted {
    background: transparent;
    border: 1px dashed currentColor;
  }
  .optional {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    margin: var(--space-3) 0;
  }
  /* Checkbox, badge and description in columns; the checkbox lines up with the badge. */
  .optional .optional-perm {
    display: grid;
    grid-template-columns: auto auto 1fr;
    align-items: start;
    gap: var(--space-2);
    font-size: var(--text-base);
  }
  .optional .optional-perm input {
    margin: 2px 0 0;
  }
  @media (max-width: 480px) {
    /* Badge above the description. */
    .optional .optional-perm {
      grid-template-columns: auto 1fr;
    }
    .optional .optional-perm > .badge {
      justify-self: start;
    }
    .optional .optional-perm > span:last-child {
      grid-column: 2;
    }
  }
  .dialog-actions {
    display: flex;
    flex-wrap: wrap;
    justify-content: flex-end;
    gap: var(--space-2);
    margin-top: var(--space-5);
  }
  @media (max-width: 480px) {
    .grant-list li {
      flex-wrap: wrap;
    }
    .dialog-actions {
      flex-direction: column-reverse;
    }
    .dialog-actions .btn {
      width: 100%;
      justify-content: center;
    }
  }
</style>
