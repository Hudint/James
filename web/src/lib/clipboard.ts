// Copying to the clipboard, with a fallback where the browser does not allow it (plain HTTP).

import { t } from './i18n/index.svelte';
import { toast } from './session.svelte';

/** Copies `text`; if the clipboard is unavailable, selects `element` and asks to press Ctrl+C. */
export async function copyText(text: string, element?: HTMLElement | null): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
    toast(t('common.copied'), 'success');
  } catch {
    if (element) {
      const range = document.createRange();
      range.selectNodeContents(element);
      const selection = window.getSelection();
      selection?.removeAllRanges();
      selection?.addRange(range);
    }
    toast(t('common.copyManually'), 'info');
  }
}
