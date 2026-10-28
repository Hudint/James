<script lang="ts">
  import { beforeNavigate, goto } from '$app/navigation';
  import { page } from '$app/state';
  import { api } from '$lib/api';
  import { confirmAction } from '$lib/confirm.svelte';
  import Icon from '$lib/components/Icon.svelte';
  import IdentityCard from '$lib/components/IdentityCard.svelte';
  import InstanceForm from '$lib/components/InstanceForm.svelte';
  import { t } from '$lib/i18n/index.svelte';
  import { reportError, toast } from '$lib/session.svelte';
  import type { InstanceConfig } from '$lib/types';

  let id = $derived(page.params.id ?? '');
  let config = $state<InstanceConfig | null>(null);
  let saved = $state('');
  let dirty = $derived(config !== null && JSON.stringify(config) !== saved);
  let error = $state('');
  let busy = $state(false);

  $effect(() => {
    api
      .instance(id)
      .then((s) => {
        if (s.config) {
          config = s.config;
          saved = JSON.stringify(s.config);
        }
        else error = t('errors.forbidden');
      })
      .catch((e) => (error = (e as Error).message));
  });

  async function save() {
    if (!config) return;
    busy = true;
    try {
      await api.updateInstance(id, $state.snapshot(config));
      saved = JSON.stringify(config);
      toast(t('config.saved'), 'success');
      await goto(`/instances/${id}`);
    } catch (e) {
      reportError(e);
    } finally {
      busy = false;
    }
  }

  let leaving = false;
  beforeNavigate((navigation) => {
    if (!dirty || leaving || !navigation.to) return;
    // Ask with the app's dialog, then repeat the navigation if confirmed.
    navigation.cancel();
    const target = navigation.to.url;
    void confirmAction(t('common.unsavedChanges'), t('common.close')).then((ok) => {
      if (ok) {
        leaving = true;
        void goto(target);
      }
    });
  });

  async function remove() {
    if (!config) return;
    if (!(await confirmAction(t('instance.deleteConfirm', { name: config.name }), t('common.delete')))) return;
    try {
      await api.deleteInstance(id);
      saved = JSON.stringify(config);
      await goto('/');
    } catch (e) {
      reportError(e);
    }
  }
</script>

<svelte:head><title>{t('instance.settings')} · James</title></svelte:head>

<div class="page narrow">
  <header class="page-header">
    <div class="title">
      <a class="icon-btn" href="/instances/{id}" aria-label={t('common.back')}><Icon name="left" /></a>
      <h1>{t('instance.settings')}</h1>
    </div>
  </header>
  {#if error}
    <p class="error-text" role="alert">{error}</p>
  {:else if config}
    <InstanceForm bind:config isNew={false} {busy} {dirty} onsubmit={save} />
    {#if config.platform === 'teamspeak'}<IdentityCard {id} />{/if}
    <section class="card danger stack">
      <h2>{t('config.dangerZone')}</h2>
      <p class="muted small">{t('config.deleteHint')}</p>
      <div><button class="btn btn-danger" type="button" onclick={remove}><Icon name="trash" size={16} />{t('common.delete')}</button></div>
    </section>
  {:else}
    <p class="muted" aria-busy="true">{t('common.loading')}</p>
  {/if}
</div>

<style>
  .narrow {
    max-width: 880px;
  }
  .danger {
    border-color: color-mix(in srgb, var(--danger) 40%, var(--border));
  }
</style>
