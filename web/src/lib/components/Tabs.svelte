<script lang="ts" generics="T extends string">
  // Accessible tab list (WAI-ARIA tabs pattern): arrow keys move between tabs, only the
  // active tab is in the tab order. Panels use id `panel-<value>`, tabs `tab-<value>`.
  import type { IconName } from './Icon.svelte';
  import Icon from './Icon.svelte';

  let {
    tabs,
    value = $bindable(),
    label,
  }: { tabs: { value: T; label: string; icon?: IconName }[]; value: T; label: string } = $props();

  let list = $state<HTMLElement>();

  function onKeydown(event: KeyboardEvent) {
    const index = tabs.findIndex((tab) => tab.value === value);
    const next =
      event.key === 'ArrowRight' ? (index + 1) % tabs.length
      : event.key === 'ArrowLeft' ? (index - 1 + tabs.length) % tabs.length
      : event.key === 'Home' ? 0
      : event.key === 'End' ? tabs.length - 1
      : -1;
    if (next < 0) return;
    event.preventDefault();
    value = tabs[next].value;
    list?.querySelectorAll<HTMLElement>('[role="tab"]')[next]?.focus();
  }
</script>

<div class="tabs" role="tablist" aria-label={label} bind:this={list} onkeydown={onKeydown} tabindex="-1">
  {#each tabs as tab (tab.value)}
    <button
      role="tab"
      id="tab-{tab.value}"
      aria-selected={value === tab.value}
      aria-controls="panel-{tab.value}"
      tabindex={value === tab.value ? 0 : -1}
      class:active={value === tab.value}
      onclick={() => (value = tab.value)}
    >
      {#if tab.icon}<Icon name={tab.icon} size={16} />{/if}{tab.label}
    </button>
  {/each}
</div>

<style>
  .tabs {
    display: flex;
    gap: var(--space-1);
    padding: var(--space-1);
    background: var(--surface-2);
    border-radius: var(--radius);
    width: fit-content;
    max-width: 100%;
  }
  .tabs:focus {
    outline: none;
  }
  button {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    min-height: 36px;
    padding: 0 var(--space-4);
    border: none;
    border-radius: var(--radius-sm);
    background: none;
    color: var(--text-muted);
    font: inherit;
    font-size: var(--text-sm);
    font-weight: 600;
    cursor: pointer;
    white-space: nowrap;
  }
  button.active {
    background: var(--surface);
    color: var(--text);
    box-shadow: var(--shadow);
  }
  /* Phones: full width, big enough for a thumb. */
  @media (max-width: 600px) {
    .tabs {
      width: 100%;
    }
    button {
      flex: 1;
      justify-content: center;
      min-height: 44px;
    }
  }
</style>
