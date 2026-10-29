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

  // Enable / scope dialog
  let dialog: HTMLDialogElement | undefined = $state();
  let enabling = $state<PluginInfo | null>(null);
  /** Only the instance scope changes (permissions are already granted). */
  let scopeOnly = $state(false);
  let scope = $state<'all' | 'some'>('all');
  let chosen = $state<string[]>([]);
  /** Optional permissions ticked in the dialog. */
  let chosenOptional = $state<string[]>([]);

  // Settings panel
  let configuring = $state<string | null>(null);
  let configInstance = $state('');
  let configs = $state<Record<string, Record<string, unknown>>>({});
  let values = $state<Record<string, unknown>>({});
  let formValid = $state(true);
  let saving = $state(false);
  const dirty = $derived(configuring !== null && JSON.stringify(values) !== JSON.stringify(configs[configInstance] ?? {}));

  async function load() {
    try {
      [overview, instances] = await Promise.all([api.plugins(), api.instances()]);
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
      .sort((a, b) => ORDER[a.status.state] - ORDER[b.status.state] || a.manifest.name.localeCompare(b.manifest.name)),
  );

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

  async function openDialog(plugin: PluginInfo, onlyScope: boolean) {
    enabling = plugin;
    scopeOnly = onlyScope;
    scope = plugin.state.instances === null ? 'all' : 'some';
    chosen = plugin.state.instances ?? instances.map((i) => i.id);
    chosenOptional = optionalOf(plugin.manifest).filter((p) => plugin.state.grantedPermissions.includes(p));
    // The content must exist before showModal() so focus lands on Cancel.
    await tick();
    dialog?.showModal();
  }

  async function focusCard(id: string) {
    await tick();
    document.getElementById(`plugin-${id}`)?.focus();
  }

  async function confirmDialog() {
    if (!enabling) return;
    const plugin = enabling;
    dialog?.close();
    busy = plugin.manifest.id;
    try {
      const updated = await api.configurePlugin(plugin.manifest.id, {
        enabled: true,
        permissions: [...plugin.manifest.permissions, ...chosenOptional],
        instances: plugin.manifest.autorun || scope === 'all' ? null : chosen,
      });
      replace(updated);
      if (updated.status.state === 'failed') toast(t('plugins.startFailed', { error: updated.status.error }), 'error');
      else toast(t(scopeOnly ? 'plugins.scopeSaved' : 'plugins.enabled', { name: plugin.manifest.name }), 'success');
    } catch (e) {
      reportError(e);
    } finally {
      busy = null;
      void focusCard(plugin.manifest.id);
    }
  }

  async function disable(plugin: PluginInfo) {
    if (!(await confirmAction(t('plugins.disableConfirm', { name: plugin.manifest.name }), t('plugins.disable')))) return;
    busy = plugin.manifest.id;
    try {
      replace(await api.configurePlugin(plugin.manifest.id, { enabled: false, permissions: [], instances: plugin.state.instances }));
      toast(t('plugins.disabled', { name: plugin.manifest.name }), 'success');
    } catch (e) {
      reportError(e);
    } finally {
      busy = null;
      void focusCard(plugin.manifest.id);
    }
  }

  /** Re-applies the current configuration, which reloads a failed plugin. */
  async function retry(plugin: PluginInfo) {
    busy = plugin.manifest.id;
    try {
      const updated = await api.configurePlugin(plugin.manifest.id, {
        enabled: true,
        permissions: plugin.state.grantedPermissions,
        instances: plugin.state.instances,
      });
      replace(updated);
      if (updated.status.state === 'failed') toast(t('plugins.startFailed', { error: updated.status.error }), 'error');
      else toast(t('plugins.restarted', { name: plugin.manifest.name }), 'success');
    } catch (e) {
      reportError(e);
    } finally {
      busy = null;
      void focusCard(plugin.manifest.id);
    }
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
      selectInstance(plugin.activeInstances[0] ?? instances[0]?.id ?? '');
    } catch (e) {
      reportError(e);
    }
  }

  function selectInstance(id: string) {
    configInstance = id;
    values = { ...(configs[id] ?? {}) };
    formValid = true;
  }

  async function changeInstance(select: HTMLSelectElement) {
    const next = select.value;
    if (dirty && !(await confirmAction(t('plugins.discardChanges'), t('common.discard')))) {
      select.value = configInstance;
      return;
    }
    selectInstance(next);
  }

  async function saveConfig(event: SubmitEvent, plugin: PluginInfo) {
    event.preventDefault();
    if (!formValid) return;
    saving = true;
    try {
      await api.setPluginConfig(configInstance, plugin.manifest.id, values);
      configs = { ...configs, [configInstance]: values };
      toast(t('plugins.saved'), 'success');
    } catch (e) {
      reportError(e);
    } finally {
      saving = false;
    }
  }

  function statusBadge(plugin: PluginInfo): { label: string; tone: string } {
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

  let pageInstance = $state<Record<string, string>>({});

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
    <button class="btn" onclick={reload} disabled={reloading || overview?.host.state !== 'running'}>
      <Icon name="refresh" size={16} />{reloading ? t('common.loading') : t('plugins.reload')}
    </button>
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
    {#if overview.host.state === 'unavailable'}
      <div class="card notice danger" role="alert">
        <Icon name="alert" />
        <div>
          <strong>{t('plugins.hostUnavailable')}</strong>
          <p class="small">{overview.host.error}</p>
        </div>
      </div>
    {:else if overview.host.state === 'starting'}
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

    {#if visiblePlugins.length === 0}
      <div class="card empty">
        <Icon name="puzzle" size={32} />
        <p>{t('plugins.empty')}</p>
        <p class="hint">{t('plugins.emptyHint')}</p>
        {#if overview.pluginsDir}<code class="path">{overview.pluginsDir}</code>{/if}
      </div>
    {:else}
      <ul class="plugins">
        {#each visiblePlugins as plugin (plugin.manifest.id)}
          {@const m = plugin.manifest}
          {@const badge = statusBadge(plugin)}
          {@const added = newPermissions(plugin)}
          <li class="card plugin">
            <div class="head">
              <div class="title">
                <h2 id="plugin-{m.id}" tabindex="-1">{m.name}</h2>
                <span class="muted small">{m.version ? `v${m.version}` : ''}{m.author ? ` · ${m.author}` : ''}</span>
              </div>
              <div class="badges">
                {#if m.autorun}<span class="badge">{t('plugins.autorun')}</span>{/if}
                {#if m.kind === 'sinusbot'}<span class="badge accent">{t('plugins.sinusbot')}</span>{/if}
                <span class="badge {badge.tone}">{badge.label}</span>
              </div>
            </div>
            {#if m.description}<p class="description">{m.description}</p>{/if}

            {#if plugin.status.state === 'failed'}
              <div class="notice danger inline" role="alert">
                <Icon name="alert" size={18} />
                <div><strong>{t('plugins.failedTitle')}</strong><p class="small">{plugin.status.error}</p></div>
              </div>
            {:else if plugin.status.state === 'unavailable'}
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
              {#if plugin.state.enabled}
                <dt>{t('plugins.activeOn')}</dt>
                <dd>
                  {#if plugin.activeInstances.length}
                    {plugin.activeInstances.map(instanceName).join(', ')}
                  {:else}
                    <span class="muted">{t('plugins.noActiveInstances')}</span>
                  {/if}
                </dd>
              {/if}
              {#if plugin.commands.length}
                <dt>{t('plugins.commands')}</dt>
                <dd>{plugin.commands.join(', ')}</dd>
              {/if}
            </dl>

            <div class="row actions">
              {#if plugin.status.state === 'failed'}
                <button class="btn btn-primary" onclick={() => retry(plugin)} disabled={busy === m.id || overview.host.state !== 'running'}>
                  <Icon name="refresh" size={16} />{t('plugins.retry')}
                </button>
              {/if}
              {#if plugin.state.enabled && plugin.status.state !== 'needsApproval'}
                {#if !m.autorun}
                  <button class="btn" onclick={() => openDialog(plugin, true)} disabled={busy === m.id || overview.host.state !== 'running'}>
                    <Icon name="server" size={16} />{t('plugins.instances')}
                  </button>
                {/if}
                <button class="btn" onclick={() => disable(plugin)} disabled={busy === m.id}>
                  <Icon name="power" size={16} />{t('plugins.disable')}
                </button>
              {:else}
                <button class="btn btn-primary" onclick={() => openDialog(plugin, false)} disabled={busy === m.id || overview.host.state !== 'running'}>
                  <Icon name="power" size={16} />{plugin.status.state === 'needsApproval' ? t('plugins.review') : t('plugins.enable')}
                </button>
              {/if}
              {#if m.config.length}
                <button class="btn" onclick={() => toggleSettings(plugin)} aria-expanded={configuring === m.id} aria-controls="settings-{m.id}">
                  <Icon name="settings" size={16} />{t('plugins.settings')}
                </button>
              {/if}
              {#if m.web && plugin.status.state === 'running' && plugin.activeInstances.length}
                {#if plugin.activeInstances.length > 1}
                  <span class="open-page">
                    <label class="small muted" for="page-instance-{m.id}">{t('plugins.pageFor')}</label>
                    <select id="page-instance-{m.id}" bind:value={pageInstance[m.id]}>
                      {#each plugin.activeInstances as instance (instance)}<option value={instance}>{instanceName(instance)}</option>{/each}
                    </select>
                    <button class="btn" onclick={() => openPage(plugin, pageInstance[m.id] ?? plugin.activeInstances[0])}>
                      <Icon name="external" size={16} />{t('plugins.openPage')}
                    </button>
                  </span>
                {:else}
                  <button class="btn" onclick={() => openPage(plugin, plugin.activeInstances[0])}>
                    <Icon name="external" size={16} />{t('plugins.openPage')}
                  </button>
                {/if}
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
                  <div class="field">
                    <label for="instance-{m.id}">{t('plugins.instance')}</label>
                    <select id="instance-{m.id}" value={configInstance} onchange={(e) => changeInstance(e.currentTarget)}>
                      {#each instances as instance (instance.id)}
                        <option value={instance.id}>
                          {instance.name}{plugin.activeInstances.includes(instance.id) ? '' : ` (${t('plugins.inactive')})`}
                        </option>
                      {/each}
                    </select>
                  </div>
                  {#key configInstance}
                    <PluginConfigForm fields={m.config} bind:values bind:valid={formValid} idPrefix="cfg-{m.id}" />
                  {/key}
                  <div class="row">
                    <button class="btn btn-primary" type="submit" disabled={saving || !configInstance || !formValid || !dirty}>
                      {saving ? t('common.saving') : t('common.save')}
                    </button>
                    <span class="hint">{t('plugins.restartHint')}</span>
                  </div>
                </form>
              {/if}
            </div>
          </li>
        {/each}
      </ul>
    {/if}
  {/if}
</div>

<dialog bind:this={dialog} aria-labelledby="enable-title" aria-describedby="enable-summary" onclose={() => (enabling = null)}>
  {#if enabling}
    {@const m = enabling.manifest}
    <h2 id="enable-title">{t(scopeOnly ? 'plugins.scopeTitle' : 'plugins.enableTitle', { name: m.name })}</h2>
    {#if !scopeOnly}
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
      <p id="enable-summary">{t(dialogOptional.length ? 'plugins.scopeOptionalHint' : 'plugins.scopeHint')}</p>
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

    {#if m.autorun}
      <p class="small muted">{t('plugins.autorunHint')}</p>
    {:else if instances.length === 0}
      <p class="small muted">{t('plugins.noInstances')}</p>
    {:else}
      <fieldset class="scope">
        <legend class="label">{t('plugins.activeOn')}</legend>
        <label class="checkbox"><input type="radio" name="scope" value="all" bind:group={scope} aria-describedby="all-hint" />{t('plugins.allInstances')}</label>
        <span class="hint indent" id="all-hint">{t('plugins.allInstancesHint')}</span>
        <label class="checkbox"><input type="radio" name="scope" value="some" bind:group={scope} />{t('plugins.someInstances')}</label>
        {#if scope === 'some'}
          <div class="instance-list">
            {#each instances as instance (instance.id)}
              <label class="checkbox"><input type="checkbox" value={instance.id} bind:group={chosen} />{instance.name}</label>
            {/each}
          </div>
          <span class="hint indent" id="choose-hint" aria-live="polite">{chosen.length === 0 ? t('plugins.chooseInstance') : ''}</span>
        {/if}
      </fieldset>
    {/if}

    {#if scopeOnly && newlyGranted.some((id) => permission(id)?.risk === 'high')}
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
      <button class="btn btn-primary" onclick={confirmDialog} disabled={scope === 'some' && chosen.length === 0 && !m.autorun} aria-describedby={scope === 'some' && chosen.length === 0 ? 'choose-hint' : undefined}>
        {#if scopeOnly}{newlyGranted.length ? t('plugins.grantAndSave') : t('common.save')}{:else}{m.permissions.length || chosenOptional.length ? t('plugins.grantAndEnable') : t('plugins.enable')}{/if}
      </button>
    </div>
  {/if}
</dialog>

<style>
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
  .title h2 {
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
    .actions .open-page,
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
  .scope {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    margin-top: var(--space-4);
  }
  .scope .checkbox {
    min-height: 28px;
  }
  .scope .checkbox input {
    margin: 0;
  }
  .open-page {
    display: inline-flex;
    gap: var(--space-2);
  }
  .open-page {
    align-items: center;
  }
  .open-page select {
    width: auto;
    min-height: 36px;
    font-size: var(--text-sm);
    font-weight: 600;
  }
  .indent {
    padding-left: 26px;
  }
  .instance-list {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    padding-left: var(--space-5);
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
