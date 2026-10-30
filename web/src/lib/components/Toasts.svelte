<script lang="ts">
  import { dismissToast, toasts } from '$lib/session.svelte';
  import { t } from '$lib/i18n/index.svelte';
  import Icon from './Icon.svelte';
</script>

<!-- One live region that always exists: messages inserted into it are announced. -->
<div class="toasts" role="status" aria-live="polite">
  {#each toasts as toast (toast.id)}
    <div class="toast {toast.kind}" class:busy={toast.sticky} role={toast.kind === 'error' ? 'alert' : undefined}>
      {#if toast.sticky}
        <span class="meter" aria-hidden="true"><i></i><i></i><i></i></span>
      {:else if toast.kind !== 'info'}
        <span class="kind" aria-hidden="true"><Icon name={toast.kind === 'error' ? 'alert' : 'check'} size={16} /></span>
      {/if}
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
  /* Navy slips with a coloured edge, sliding in from below. */
  .toast {
    pointer-events: auto;
    display: flex;
    align-items: center;
    gap: var(--space-3);
    max-width: 440px;
    padding: var(--space-2) var(--space-2) var(--space-2) var(--space-4);
    border-radius: var(--radius-lg);
    border-left: 3px solid var(--side-muted);
    background: var(--side-bg);
    color: var(--side-text);
    box-shadow: var(--shadow-lg);
    font-size: var(--text-sm);
    animation: toast-in var(--dur-3) var(--ease-out);
  }
  @keyframes toast-in {
    from {
      opacity: 0;
      transform: translateY(12px);
    }
  }
  .toast .icon-btn {
    color: var(--side-muted);
  }
  .toast .icon-btn:hover {
    background: var(--side-surface);
    color: var(--side-text);
  }
  .toast.error {
    border-left-color: #ff7a8a;
  }
  .toast.success {
    border-left-color: #4fd99a;
  }
  .toast.busy {
    border-left-color: var(--tally);
  }
  .kind {
    display: grid;
    color: #4fd99a;
  }
  .error .kind {
    color: #ff7a8a;
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
