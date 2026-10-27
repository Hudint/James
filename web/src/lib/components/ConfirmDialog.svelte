<script lang="ts">
  import { closeConfirm, confirmState } from '$lib/confirm.svelte';
  import { t } from '$lib/i18n/index.svelte';

  let dialog: HTMLDialogElement;

  $effect(() => {
    if (confirmState.open && !dialog.open) dialog.showModal();
    if (!confirmState.open && dialog.open) dialog.close();
  });
</script>

<dialog bind:this={dialog} onclose={() => closeConfirm(false)} aria-labelledby="confirm-text">
  <p id="confirm-text">{confirmState.text}</p>
  <div class="actions">
    <button class="btn" onclick={() => closeConfirm(false)}>{t('common.cancel')}</button>
    <button
      class="btn {confirmState.danger ? 'btn-danger' : 'btn-primary'}"
      onclick={() => closeConfirm(true)}
    >
      {confirmState.action}
    </button>
  </div>
</dialog>

<style>
  dialog {
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--surface);
    color: var(--text);
    padding: var(--space-5);
    width: min(420px, calc(100vw - 32px));
    box-shadow: var(--shadow);
  }
  dialog::backdrop {
    background: rgb(0 0 0 / 45%);
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: var(--space-2);
    margin-top: var(--space-5);
  }
</style>
