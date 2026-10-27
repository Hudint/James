<script lang="ts">
  // Select for a running instance the user may play on; remembers the last choice.
  import { onMount } from 'svelte';
  import { api } from '$lib/api';
  import { t } from '$lib/i18n/index.svelte';
  import { live } from '$lib/live.svelte';
  import { can } from '$lib/session.svelte';
  import type { InstanceSummary } from '$lib/types';

  let { value = $bindable(''), id = 'instance-picker' }: { value?: string; id?: string } = $props();
  let instances = $state<InstanceSummary[]>([]);
  const KEY = 'james.playTarget';

  async function load() {
    instances = (await api.instances().catch(() => [])).filter((i) => i.running && can('playback.play', i.id));
  }

  // Instances started or stopped later show up without a reload.
  $effect(() => {
    if (live.version > 0) void load();
  });

  onMount(async () => {
    await load();
    let stored = '';
    try {
      stored = localStorage.getItem(KEY) ?? '';
    } catch {
      // ignore
    }
    value = instances.some((i) => i.id === stored) ? stored : (instances[0]?.id ?? '');
  });

  $effect(() => {
    try {
      if (value) localStorage.setItem(KEY, value);
    } catch {
      // ignore
    }
  });
</script>

<label class="visually-hidden" for={id}>{t('library.playOn')}</label>
<select {id} bind:value disabled={instances.length === 0}>
  {#if instances.length === 0}
    <option value="">{t('library.noInstance')}</option>
  {/if}
  {#each instances as instance (instance.id)}
    <option value={instance.id}>{instance.name}</option>
  {/each}
</select>
