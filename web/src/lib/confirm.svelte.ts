// Promise-based confirmation dialog rendered by ConfirmDialog.svelte in the root layout.

export const confirmState = $state<{
  open: boolean;
  text: string;
  action: string;
  danger: boolean;
  resolve: ((ok: boolean) => void) | null;
}>({ open: false, text: '', action: '', danger: true, resolve: null });

export function confirmAction(text: string, action: string, danger = true): Promise<boolean> {
  confirmState.resolve?.(false);
  return new Promise((resolve) => {
    Object.assign(confirmState, { open: true, text, action, danger, resolve });
  });
}

export function closeConfirm(ok: boolean) {
  confirmState.resolve?.(ok);
  Object.assign(confirmState, { open: false, resolve: null });
}
