<script lang="ts" module>
  import type { IconName } from './Icon.svelte';

  export interface MenuItem {
    label: string;
    icon?: IconName;
    danger?: boolean;
    disabled?: boolean;
    /** A link (keeps link behaviour, e.g. opening in a new tab) instead of an action. */
    href?: string;
    onclick?: () => void;
  }
</script>

<script lang="ts">
  // "⋯" button with more actions: a bottom sheet on phones, a small dropdown elsewhere.
  import { flushSync } from 'svelte';
  import { t } from '$lib/i18n/index.svelte';
  import Icon from './Icon.svelte';

  let {
    items,
    label,
    title,
    small = false,
  }: {
    items: MenuItem[];
    /** Accessible name of the button, e.g. "More actions: Song title". */
    label: string;
    /** Heading of the sheet on phones (e.g. the track title). */
    title?: string;
    small?: boolean;
  } = $props();

  let dialog = $state<HTMLDialogElement | null>(null);
  let button = $state<HTMLButtonElement | null>(null);
  let position = $state('');
  /** The entries exist only while the menu is open (long lists have one menu per row). */
  let isOpen = $state(false);

  function open() {
    if (!dialog || !button) return;
    const sheet = matchMedia('(max-width: 600px)').matches;
    if (sheet) {
      position = '';
    } else {
      // Below the button, right-aligned, kept inside the window.
      const r = button.getBoundingClientRect();
      const height = items.length * 44 + (title ? 40 : 0) + 16;
      const top = Math.min(r.bottom + 4, innerHeight - 8 - height);
      position = `top:${Math.max(8, top)}px;right:${Math.max(8, innerWidth - r.right)}px`;
    }
    // Render the entries first, so the dialog focuses the first one.
    flushSync(() => (isOpen = true));
    dialog.showModal();
  }

  function choose(item: MenuItem) {
    dialog?.close();
    item.onclick?.();
  }

  // Taps outside the content (on the backdrop) close the menu.
  let content = $state<HTMLElement | null>(null);
  function backdrop(event: MouseEvent) {
    if (content && !content.contains(event.target as Node)) dialog?.close();
  }
</script>

<button
  bind:this={button}
  class="icon-btn"
  class:small
  type="button"
  data-action="menu"
  aria-haspopup="dialog"
  aria-label={label}
  title={label}
  onclick={open}
>
  <Icon name="more" size={small ? 16 : 20} />
</button>

<dialog bind:this={dialog} class="menu" style={position} onclick={backdrop} onclose={() => (isOpen = false)} aria-label={title ?? label}>
  {#if isOpen}
    <div bind:this={content}>
      {#if title}<p class="menu-title truncate">{title}</p>{/if}
      <ul>
        {#each items as item, index (index)}
          <li>
            {#if item.href && !item.disabled}
              <a class="entry" class:danger={item.danger} href={item.href} onclick={() => dialog?.close()}>
                {#if item.icon}<Icon name={item.icon} size={18} />{/if}{item.label}
              </a>
            {:else}
              <button class="entry" type="button" class:danger={item.danger} disabled={item.disabled} onclick={() => choose(item)}>
                {#if item.icon}<Icon name={item.icon} size={18} />{/if}{item.label}
              </button>
            {/if}
          </li>
        {/each}
      </ul>
      <button class="cancel btn" type="button" onclick={() => dialog?.close()}>{t('common.cancel')}</button>
    </div>
  {/if}
</dialog>

<style>
  .menu {
    position: fixed;
    margin: 0;
    inset: auto;
    min-width: 220px;
    max-width: min(320px, calc(100vw - 16px));
    padding: var(--space-1);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface);
    color: var(--text);
    box-shadow: var(--shadow);
  }
  .menu::backdrop {
    background: transparent;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  li .entry {
    text-decoration: none;
    display: flex;
    align-items: center;
    gap: var(--space-3);
    width: 100%;
    min-height: 40px;
    padding: 0 var(--space-3);
    border: none;
    border-radius: var(--radius-sm);
    background: none;
    color: inherit;
    font: inherit;
    font-size: var(--text-sm);
    text-align: left;
    cursor: pointer;
  }
  li .entry:hover:not(:disabled),
  li .entry:focus-visible {
    background: var(--surface-2);
  }
  li .entry:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }
  li .entry.danger {
    color: var(--danger);
  }
  .menu-title {
    margin: var(--space-2) var(--space-3) var(--space-1);
    font-weight: 650;
  }
  .cancel {
    display: none;
  }
  /* Phones: a sheet at the bottom, within thumb reach. */
  @media (max-width: 600px) {
    .menu {
      inset: auto 0 0 0;
      width: 100%;
      max-width: none;
      border-radius: var(--radius-lg) var(--radius-lg) 0 0;
      padding: var(--space-2) var(--space-2) calc(var(--space-2) + env(safe-area-inset-bottom));
    }
    .menu::backdrop {
      background: rgb(0 0 0 / 45%);
    }
    li .entry {
      min-height: 48px;
      font-size: var(--text-md, 1rem);
    }
    .cancel {
      display: flex;
      width: 100%;
      margin-top: var(--space-2);
    }
  }
</style>
