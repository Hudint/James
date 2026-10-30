<script lang="ts">
  // One plugin's settings for one instance (several can be open at once on the plugins page).
  import { onDestroy, onMount, untrack } from 'svelte';
  import { api } from '$lib/api';
  import { t } from '$lib/i18n/index.svelte';
  import { reportError, toast } from '$lib/session.svelte';
  import type { PlatformChannel, PluginInfo } from '$lib/types';
  import Icon from './Icon.svelte';
  import PluginConfigForm from './PluginConfigForm.svelte';
  import { rise } from '$lib/motion';

  let {
    plugin,
    instance,
    instanceName,
    here,
    channels,
    ondirty,
  }: {
    plugin: PluginInfo;
    instance: string;
    instanceName: string;
    /** Active on `instance` (saving restarts it there). */
    here: boolean;
    channels: PlatformChannel[] | null;
    ondirty: (dirty: boolean) => void;
  } = $props();

  let saved = $state<Record<string, unknown> | null>(null);
  let values = $state<Record<string, unknown>>({});
  let valid = $state(true);
  let saving = $state(false);
  let failed = $state(false);

  const id = $derived(plugin.manifest.id);
  const dirty = $derived(saved !== null && JSON.stringify(values) !== JSON.stringify(saved));
  $effect(() => {
    const now = dirty;
    untrack(() => ondirty(now));
  });
  onDestroy(() => ondirty(false));

  async function load() {
    failed = false;
    try {
      const configs = await api.pluginConfigs(id);
      saved = configs[instance] ?? {};
      values = { ...saved };
    } catch (e) {
      failed = true;
      reportError(e);
    }
  }
  onMount(load);

  async function save(event: SubmitEvent) {
    event.preventDefault();
    if (!valid || !dirty) return;
    saving = true;
    try {
      const sent = values;
      await api.setPluginConfig(instance, id, sent);
      saved = sent;
      // The save bar goes away with the edits: keep keyboard focus on the plugin.
      document.getElementById(`plugin-${id}`)?.focus();
      toast(t('plugins.savedNamed', { name: plugin.manifest.name }), 'success');
    } catch (e) {
      reportError(e);
    } finally {
      saving = false;
    }
  }
</script>

{#if failed}
  <div class="state">
    <p class="small">{t('plugins.settingsLoadFailed')}</p>
    <button class="btn btn-small" type="button" onclick={load}><Icon name="refresh" size={16} />{t('common.retry')}</button>
  </div>
{:else if saved === null}
  <div class="state" aria-busy="true">
    <span class="skeleton"></span>
    <span class="skeleton short"></span>
  </div>
{:else}
  <form class="settings" onsubmit={save} novalidate>
    <p class="for small">
      {t('plugins.settingsFor', { instance: instanceName })}
      {#if !here}<span class="muted">· {t('plugins.settingsInactive')}</span>{/if}
    </p>
    <PluginConfigForm fields={plugin.manifest.config} bind:values bind:valid idPrefix="cfg-{id}" {channels} />
    {#if dirty}
      <div class="save-bar" transition:rise={{ y: 12 }}>
        <button class="btn btn-primary" type="submit" disabled={saving || !valid}>
          {saving ? t('common.saving') : t('common.save')}
        </button>
        <button class="btn btn-ghost" type="button" onclick={() => (values = { ...saved })}>{t('plugins.revert')}</button>
        {#if here}<span class="hint">{t('plugins.restartHint')}</span>{/if}
      </div>
    {/if}
  </form>
{/if}

<style>
  .settings {
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
  }
  .for {
    margin: 0;
    font-weight: 600;
  }
  .for .muted {
    font-weight: 400;
  }
  .state {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: var(--space-3);
  }
  .skeleton {
    display: block;
    width: 100%;
    height: 40px;
    border-radius: var(--radius);
    background: linear-gradient(90deg, var(--surface-2), var(--surface-3), var(--surface-2));
    background-size: 200% 100%;
    animation: shimmer 1.2s linear infinite;
  }
  .skeleton.short {
    width: 40%;
  }
  @keyframes shimmer {
    to {
      background-position: -200% 0;
    }
  }
  /* Only while there are edits; it stays in reach on long forms. */
  .save-bar {
    position: sticky;
    bottom: calc(var(--bottom-bar, 0px) + var(--space-2));
    z-index: 2;
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--space-2) var(--space-3);
    padding: var(--space-2);
    border-radius: var(--radius-lg);
    background: var(--surface-raised);
    box-shadow: var(--shadow-lg);
  }
  .save-bar .hint {
    flex: 1 1 12rem;
  }
</style>
