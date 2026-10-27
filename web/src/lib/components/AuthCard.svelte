<script lang="ts">
  import type { Snippet } from 'svelte';
  import { i18n, localeNames, setLocale, t, type Locale } from '$lib/i18n/index.svelte';

  let { title, subtitle, children }: { title: string; subtitle?: string; children: Snippet } = $props();
</script>

<div class="wrap">
  <div class="card auth">
    <img src="/favicon.svg" alt="" width="48" height="48" />
    <h1>{title}</h1>
    {#if subtitle}<p class="muted">{subtitle}</p>{/if}
    {@render children()}
  </div>
  <label class="language">
    <span class="visually-hidden">{t('common.language')}</span>
    <select value={i18n.locale} onchange={(e) => setLocale(e.currentTarget.value as Locale)}>
      {#each Object.entries(localeNames) as [code, name] (code)}
        <option value={code}>{name}</option>
      {/each}
    </select>
  </label>
</div>

<style>
  .wrap {
    min-height: 100vh;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: var(--space-4);
    padding: var(--space-4);
  }
  .language select {
    width: auto;
    min-height: 36px;
    font-size: var(--text-sm);
  }
  .auth {
    width: min(400px, 100%);
    padding: var(--space-6) var(--space-5);
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
    text-align: center;
  }
  .auth img {
    margin: 0 auto;
  }
  .auth :global(form) {
    text-align: left;
  }
</style>
