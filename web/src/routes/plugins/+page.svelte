<script lang="ts">
  import { onDestroy, onMount, tick } from 'svelte';
  import { beforeNavigate, goto } from '$app/navigation';
  import { api } from '$lib/api';
  import Icon from '$lib/components/Icon.svelte';
  import PluginConfigForm from '$lib/components/PluginConfigForm.svelte';
  import { confirmAction } from '$lib/confirm.svelte';
  import { i18n, locales, t } from '$lib/i18n/index.svelte';
  import { reportError, toast } from '$lib/session.svelte';
  import type { InstanceSummary, PluginInfo, PluginOverview, PluginPermission } from '$lib/types';

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

  // Settings panel (for the selected instance)
  let configuring = $state<string | null>(null);
  let configs = $state<Record<string, Record<string, unknown>>>({});
  let values = $state<Record<string, unknown>>({});
  let formValid = $state(true);
  let saving = $state(false);
  const dirty = $derived(configuring !== null && JSON.stringify(values) !== JSON.stringify(configs[selected] ?? {}));

  async function load() {
    try {
      [overview, instances] = await Promise.all([api.plugins(), api.instances()]);
      if (!instances.some((i) => i.id === selected)) pickInstance(rememberedInstance());
      loadFailed = false;
      stale = false;
    } catch {
      if (overview === null) loadFailed = true;
      else stale = true;
    }
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
      if (target.origin === location.origin) void goto(target);
      else location.href = target.href;
    });
  });

  let timer: ReturnType<typeof setInterval> | undefined;
  onMount(() => {
    void load();
    timer = setInterval(() => {
      if (!document.hidden && !dialog?.open) void load();
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
    if (configuring) values = { ...(configs[id] ?? {}) };
    formValid = true;
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

  async function focusCard(id: string) {
    await tick();
    document.getElementById(`plugin-${id}`)?.focus();
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

  async function toggleSettings(plugin: PluginInfo) {
    // Closing or switching to another plugin's settings discards edits: ask first.
    if (dirty && !(await confirmAction(t('plugins.discardChanges'), t('common.discard')))) return;
    if (configuring === plugin.manifest.id) {
      configuring = null;
      return;
    }
    try {
      configs = await api.pluginConfigs(plugin.manifest.id);
      configuring = plugin.manifest.id;
      values = { ...(configs[selected] ?? {}) };
      formValid = true;
    } catch (e) {
      reportError(e);
    }
  }

  async function saveConfig(event: SubmitEvent, plugin: PluginInfo) {
    event.preventDefault();
    if (!formValid) return;
    saving = true;
    try {
      await api.setPluginConfig(selected, plugin.manifest.id, values);
      configs = { ...configs, [selected]: values };
      toast(t('plugins.saved'), 'success');
    } catch (e) {
      reportError(e);
    } finally {
      saving = false;
    }
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
      {#if instances.length}
        <label class="instance-pick">
          <span class="label">{t('plugins.instance')}</span>
          <select value={selected} onchange={(e) => changeSelected(e.currentTarget)}>
            {#each instances as instance (instance.id)}<option value={instance.id}>{instance.name}</option>{/each}
          </select>
        </label>
      {/if}
      <button class="btn" onclick={reload} disabled={reloading || overview?.host.state !== 'running'}>
        <Icon name="refresh" size={16} />{reloading ? t('common.loading') : t('plugins.reload')}
      </button>
    </div>
  </header>

  {#if loadFailed}
    <div class="card empty" role="alert">
      <Icon name="alert" size={32} />
      <p>{t('plugins.loadFailed')}</p>
      <button class="btn" onclick={load}><Icon name="refresh" size={16} />{t('common.retry')}</button>
    </div>
  {:else if overview === null}
    <p class="muted" aria-busy="true">{t('common.loading')}</p>
  {:else}
    {#if stale}
      <div class="card notice warning" role="status"><Icon name="alert" /><span>{t('plugins.stale')}</span></div>
    {/if}
    {#if overview!.host.state === 'unavailable'}
      <div class="card notice danger" role="alert">
        <Icon name="alert" />
        <div>
          <strong>{t('plugins.hostUnavailable')}</strong>
          <p class="small">{overview.host.error}</p>
        </div>
      </div>
    {:else if overview!.host.state === 'starting'}
      <div class="card notice" role="status"><Icon name="refresh" /><span>{t('plugins.hostStarting')}</span></div>
    {/if}

    {#if overview.errors.length > 0}
      <div class="card notice warning" role="alert">
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
      <div class="card notice" role="status"><Icon name="server" /><span>{t('plugins.noInstances')}</span></div>
    {/if}

    {#if visiblePlugins.length === 0}
      <div class="card empty">
        <Icon name="puzzle" size={32} />
        <p>{t('plugins.empty')}</p>
        <p class="hint">{t('plugins.emptyHint')}</p>
        {#if overview.pluginsDir}<code class="path">{overview.pluginsDir}</code>{/if}
      </div>
    {:else}
      {#each [{ title: t('plugins.activeHere', { instance: instanceName(selected), count: activeList.length }), list: activeList }, { title: t('plugins.available', { count: availableList.length }), list: availableList }] as section (section.title)}
        {#if section.list.length || section.list === activeList}
          <h2 class="section-title">{section.title}</h2>
          {#if section.list.length}
            <ul class="plugins">
              {#each section.list as plugin (plugin.manifest.id)}
                {@render card(plugin)}
              {/each}
            </ul>
          {:else}
            <p class="muted small">{t('plugins.noneActive', { instance: instanceName(selected) })}</p>
          {/if}
        {/if}
      {/each}
    {/if}
  {/if}
</div>

{#snippet card(plugin: PluginInfo)}
  {@const m = plugin.manifest}
  {@const badge = statusBadge(plugin)}
  {@const added = newPermissions(plugin)}
  {@const here = activeHere(plugin)}
  {@const others = activatedOn(plugin).filter((other) => other.id !== selected)}
  <li class="card plugin">
    <div class="head">
      <div class="title">
        <h3 id="plugin-{m.id}" tabindex="-1">{m.name}</h3>
        <span class="muted small">{m.version ? `v${m.version}` : ''}{m.author ? ` · ${m.author}` : ''}{sameName(plugin) ? ` · ${m.id}` : ''}</span>
      </div>
      <div class="badges">
        {#if m.autorun}<span class="badge">{t('plugins.autorun')}</span>{/if}
        {#if m.kind === 'sinusbot'}<span class="badge accent">{t('plugins.sinusbot')}</span>{/if}
        {#if here || plugin.status.state === 'needsApproval'}<span class="badge {badge.tone}">{badge.label}</span>{/if}
      </div>
    </div>
    {#if m.description}<p class="description">{m.description}</p>{/if}

    {#if plugin.status.state === 'failed' && here}
      <div class="notice danger inline" role="alert">
        <Icon name="alert" size={18} />
        <div><strong>{t('plugins.failedTitle')}</strong><p class="small">{plugin.status.error}</p></div>
      </div>
    {:else if plugin.status.state === 'unavailable' && here}
      <p class="small muted">{t('plugins.unavailableHint')}</p>
    {:else if added.length}
      <div class="notice warning inline" role="status">
        <Icon name="alert" size={18} />
        <p class="small">{t('plugins.newPermissions', { count: added.length })}</p>
      </div>
    {/if}

    <dl class="facts small">
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
                {#if added.includes(id)}<span class="badge accent">{t('plugins.new')}</span>{/if}
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
        <dd>{plugin.commands.join(', ')}</dd>
      {/if}
    </dl>

    <div class="row actions">
      {#if plugin.status.state === 'failed' && here}
        <button class="btn btn-primary" onclick={() => retry(plugin)} disabled={busy === m.id || overview!.host.state !== 'running'}>
          <Icon name="refresh" size={16} />{t('plugins.retry')}
        </button>
      {/if}
      {#if plugin.status.state === 'needsApproval'}
        <button class="btn btn-primary" onclick={() => openDialog(plugin, 'review')} disabled={busy === m.id || overview!.host.state !== 'running'}>
          <Icon name="power" size={16} />{t('plugins.review')}
        </button>
      {:else if here}
        <button class="btn" onclick={() => deactivate(plugin)} disabled={busy === m.id || !selected} aria-busy={busy === m.id}>
          <Icon name="power" size={16} />{busy === m.id ? t('plugins.disabling') : m.autorun ? t('plugins.disableEverywhere') : t('plugins.disable')}
        </button>
      {:else}
        <button class="btn" onclick={() => activate(plugin)} disabled={busy === m.id || !selected || overview!.host.state !== 'running'} aria-busy={busy === m.id}>
          <Icon name="power" size={16} />{busy === m.id ? t('plugins.enabling') : m.autorun ? t('plugins.enableEverywhere') : t('plugins.enable')}
        </button>
      {/if}
      {#if m.config.length && selected}
        <button class="btn" onclick={() => toggleSettings(plugin)} aria-expanded={configuring === m.id} aria-controls="settings-{m.id}">
          <Icon name="settings" size={16} />{t('plugins.settings')}
        </button>
      {/if}
      {#if plugin.state.enabled && optionalOf(m).length && plugin.status.state !== 'needsApproval'}
        <button class="btn" onclick={() => openDialog(plugin, 'permissions')} disabled={busy === m.id || overview!.host.state !== 'running'}>
          <Icon name="key" size={16} />{t('plugins.permissionsButton')}
        </button>
      {/if}
      {#if m.web && plugin.status.state === 'running' && plugin.activeInstances.includes(selected)}
        <button class="btn" onclick={() => openPage(plugin, selected)}>
          <Icon name="external" size={16} />{t('plugins.openPage')}
        </button>
      {/if}
      {#if blockedPage?.plugin === m.id}
        <a id="blocked-{m.id}" class="btn btn-ghost" href={blockedPage.url} target="_blank" rel="noopener" onclick={() => (blockedPage = null)}>
          {t('plugins.popupBlocked')}
        </a>
      {/if}
    </div>

    <div id="settings-{m.id}">
      {#if configuring === m.id}
        <form class="settings" onsubmit={(e) => saveConfig(e, plugin)} novalidate>
          <h3 class="label">{t('plugins.settingsFor', { instance: instanceName(selected) })}</h3>
          {#if !here}<p class="small muted">{t('plugins.settingsInactive')}</p>{/if}
          {#key selected}
            <PluginConfigForm fields={m.config} bind:values bind:valid={formValid} idPrefix="cfg-{m.id}" />
          {/key}
          <div class="row">
            <button class="btn btn-primary" type="submit" disabled={saving || !selected || !formValid || !dirty}>
              {saving ? t('common.saving') : t('common.save')}
            </button>
            {#if here}<span class="hint">{t('plugins.restartHint')}</span>{/if}
          </div>
        </form>
      {/if}
    </div>
  </li>
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
    align-items: flex-end;
    gap: var(--space-3);
    flex-wrap: wrap;
  }
  .instance-pick {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
  }
  .instance-pick select {
    min-width: 12rem;
  }
  .section-title {
    font-size: var(--text-base);
    margin: var(--space-5) 0 var(--space-3);
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
    color: var(--accent);
    font: inherit;
    text-decoration: underline;
    cursor: pointer;
  }
  .settings h3 {
    margin: 0;
  }
  .plugins {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
  }
  .plugin {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }
  .head {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: var(--space-3);
    flex-wrap: wrap;
  }
  .title {
    display: flex;
    align-items: baseline;
    gap: var(--space-2);
    flex-wrap: wrap;
    min-width: 0;
  }
  .title h3 {
    font-size: var(--text-lg);
    margin: 0;
  }
  .badges {
    display: flex;
    gap: var(--space-2);
    flex-wrap: wrap;
  }
  .description {
    margin: 0;
    color: var(--text-muted);
  }
  .facts {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: var(--space-2) var(--space-4);
    margin: 0;
  }
  .facts dt {
    font-weight: 600;
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
    gap: var(--space-1);
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
  .actions {
    flex-wrap: wrap;
  }
  /* Phones: buttons in an even two-column grid instead of ragged rows. */
  @media (max-width: 600px) {
    .actions {
      display: grid;
      grid-template-columns: 1fr 1fr;
    }
    .actions > :global(*) {
      width: 100%;
      min-width: 0;
    }
    .actions > :global(:last-child:nth-child(odd)) {
      grid-column: 1 / -1;
    }
  }
  .settings {
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
    border-top: 1px solid var(--border);
    padding-top: var(--space-4);
  }
  .notice {
    display: flex;
    gap: var(--space-3);
    align-items: flex-start;
  }
  .notice.inline {
    padding: var(--space-3);
    border-radius: var(--radius);
    border: 1px solid var(--border);
  }
  .notice p,
  .notice ul {
    margin: var(--space-1) 0 0;
  }
  .notice.inline > p {
    margin: 0;
  }
  .notice.danger {
    border-color: var(--danger);
    color: var(--danger);
    background: var(--danger-soft);
  }
  .notice.warning {
    border-color: var(--warning);
    background: var(--warning-soft);
  }
  .path {
    font-size: var(--text-sm);
    word-break: break-all;
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
