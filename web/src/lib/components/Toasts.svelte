<script lang="ts">
  import { dismissToast, toasts } from '$lib/session.svelte';
  import { t } from '$lib/i18n/index.svelte';
  import Icon from './Icon.svelte';
</script>

<div class="toasts">
  {#each toasts as toast (toast.id)}
    <div class="toast {toast.kind}" role={toast.kind === 'error' ? 'alert' : 'status'}>
      <span class="text">{toast.text}</span>
      <button class="icon-btn small" onclick={() => dismissToast(toast.id)} aria-label={t('common.close')}>
        <Icon name="x" size={16} />
      </button>
    </div>
  {/each}
</div>

<style>
  .toasts {
    position: fixed;
    right: var(--space-4);
    /* Above the mini player (its height includes the safe area), else above the safe area. */
    bottom: calc(var(--space-4) + var(--bottom-bar, env(safe-area-inset-bottom)));
    left: var(--space-4);
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: var(--space-2);
    z-index: 100;
    pointer-events: none;
    transition: bottom 0.15s;
  }
  .toast {
    pointer-events: auto;
    display: flex;
    align-items: center;
    gap: var(--space-2);
    max-width: 420px;
    padding: var(--space-2) var(--space-2) var(--space-2) var(--space-4);
    border-radius: var(--radius);
    background: var(--text);
    color: var(--bg);
    box-shadow: var(--shadow);
    font-size: var(--text-sm);
  }
  .toast .icon-btn {
    color: inherit;
  }
  /* --accent-contrast is white in light and near-black in dark mode, so both stay readable. */
  .toast.error {
    background: var(--danger);
    color: var(--accent-contrast);
  }
  .toast.success {
    background: var(--success);
    color: var(--accent-contrast);
  }
  .text {
    flex: 1;
  }
  /* Phones: full width, easy to read and to dismiss. */
  @media (max-width: 600px) {
    .toasts {
      align-items: stretch;
    }
    .toast {
      max-width: none;
    }
  }
</style>
