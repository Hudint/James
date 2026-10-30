<script lang="ts">
  import type { Snippet } from 'svelte';
  import { i18n, localeNames, setLocale, t, type Locale } from '$lib/i18n/index.svelte';

  let { title, subtitle, children }: { title: string; subtitle?: string; children: Snippet } = $props();
</script>

<div class="wrap">
  <aside class="stage" aria-hidden="true">
    <span class="brand"><span class="tally"></span>James</span>
    <div class="levels">
      {#each Array.from({ length: 24 }, (_, i) => i) as i (i)}<i style:animation-delay="{-((i * 137) % 900)}ms" style:--peak={0.35 + ((i * 53) % 60) / 100}></i>{/each}
    </div>
    <p class="claim">{t('app.claim')}</p>
  </aside>
  <main class="side">
    <label class="language">
      <span class="visually-hidden">{t('common.language')}</span>
      <select value={i18n.locale} onchange={(e) => setLocale(e.currentTarget.value as Locale)}>
        {#each Object.entries(localeNames) as [code, name] (code)}
          <option value={code}>{name}</option>
        {/each}
      </select>
    </label>
    <div class="auth">
      <span class="brand mobile-only" aria-hidden="true"><span class="tally"></span>James</span>
      <h1>{title}</h1>
      {#if subtitle}<p class="muted">{subtitle}</p>{/if}
      {@render children()}
    </div>
  </main>
</div>

<style>
  .wrap {
    min-height: 100vh;
    display: grid;
    grid-template-columns: 42% minmax(0, 1fr);
  }
  .stage {
    position: relative;
    display: flex;
    flex-direction: column;
    justify-content: space-between;
    padding: var(--space-6);
    background: var(--side-bg);
    color: var(--side-text);
    overflow: hidden;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 10px;
    font-weight: 650;
    font-size: 1.25rem;
    letter-spacing: -0.02em;
  }
  .tally {
    width: 12px;
    height: 12px;
    border-radius: 3px;
    background: var(--tally);
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--tally) 22%, transparent);
  }
  .levels {
    display: flex;
    align-items: flex-end;
    gap: 6px;
    height: 180px;
  }
  .levels i {
    flex: 1;
    height: 100%;
    border-radius: 2px;
    background: var(--tally);
    transform-origin: bottom;
    transform: scaleY(var(--peak));
    animation: level 1.6s ease-in-out infinite alternate;
  }
  @keyframes level {
    0% {
      transform: scaleY(calc(var(--peak) * 0.25));
    }
    100% {
      transform: scaleY(var(--peak));
    }
  }
  .claim {
    max-width: 22rem;
    font-size: var(--text-xl);
    font-weight: 500;
    letter-spacing: -0.015em;
    color: var(--side-muted);
  }
  .side {
    position: relative;
    display: grid;
    place-items: center;
    padding: var(--space-6) var(--space-5);
  }
  .language {
    position: absolute;
    top: var(--space-4);
    right: var(--space-4);
  }
  .language select {
    width: auto;
    min-height: 34px;
    font-size: var(--text-sm);
  }
  .auth {
    width: min(360px, 100%);
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
  }
  .auth :global(.btn) {
    min-height: 40px;
  }
  .auth h1 {
    font-size: var(--text-2xl);
  }
  .auth .muted {
    margin-top: calc(-1 * var(--space-2));
  }
  .mobile-only {
    display: none;
  }
  @media (max-width: 800px) {
    .wrap {
      grid-template-columns: 1fr;
    }
    .stage {
      display: none;
    }
    .side {
      align-items: start;
      padding-top: calc(var(--space-6) * 2);
    }
    .mobile-only {
      display: flex;
      margin-bottom: var(--space-4);
    }
  }
</style>
