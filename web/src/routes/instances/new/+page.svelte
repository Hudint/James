<script lang="ts">
  import { goto } from '$app/navigation';
  import { api } from '$lib/api';
  import InstanceForm from '$lib/components/InstanceForm.svelte';
  import { t } from '$lib/i18n/index.svelte';
  import { reportError, toast } from '$lib/session.svelte';
  import type { InstanceConfig } from '$lib/types';

  let config = $state<InstanceConfig>({
    id: 'music',
    name: 'Music',
    platform: 'teamspeak',
    settings: { address: '', nickname: 'James' },
    audio: { volume: 50, bitrate: null, target_lufs: -18 },
    command_prefix: '!',
    now_playing_description: true,
    autostart: true,
  });
  let busy = $state(false);

  async function create() {
    busy = true;
    try {
      await api.createInstance($state.snapshot(config));
      toast(t('config.created'), 'success');
      await goto(`/instances/${config.id}`);
    } catch (e) {
      reportError(e);
    } finally {
      busy = false;
    }
  }
</script>

<svelte:head><title>{t('instance.new')} · James</title></svelte:head>

<div class="page narrow">
  <header class="page-header"><h1>{t('instance.new')}</h1></header>
  <InstanceForm bind:config isNew={true} {busy} onsubmit={create} />
</div>

<style>
  .narrow {
    max-width: 880px;
  }
</style>
