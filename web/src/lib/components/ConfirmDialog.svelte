<script lang="ts">
  import { closeConfirm, confirmState } from '$lib/confirm.svelte';
  import { t } from '$lib/i18n/index.svelte';

  let dialog: HTMLDialogElement;

  $effect(() => {
    if (confirmState.open && !dialog.open) dialog.showModal();
    if (!confirmState.open && dialog.open) dialog.close();
  });
</script>

<dialog bind:this={dialog} class="modal narrow" onclose={() => closeConfirm(false)} aria-labelledby="confirm-text">
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
