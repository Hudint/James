<script lang="ts" module>
  /** Unsent texts per instance: the card moves when the layout switches (narrow/wide). */
  const drafts = $state<Record<string, string>>({});
</script>

<script lang="ts">
  // Text to speech: the bot reads a text out over the music.
  import { api } from '$lib/api';
  import { t } from '$lib/i18n/index.svelte';
  import { refreshStatus } from '$lib/live.svelte';
  import { reportError, toast } from '$lib/session.svelte';
  import type { InstanceStatus } from '$lib/types';
  import Icon from './Icon.svelte';

  let { id, status }: { id: string; status: InstanceStatus } = $props();

  const MAX = 500;
  let text = $derived(drafts[id] ?? '');
  let busy = $state(false);
  // Characters as the server counts them (code points, not UTF-16 units like maxlength).
  let length = $derived([...text].length);
  let tooLong = $derived(length > MAX);
  let speaking = $derived(status.speech > 0);
  let offline = $derived(!status.connected);

  async function say(event?: SubmitEvent) {
    event?.preventDefault();
    const value = text.trim();
    if (!value || busy || offline || tooLong) return;
    busy = true;
    // The instance and text sent: the card may switch instance or get new text meanwhile.
    const target = id;
    const sent = text;
    try {
      await api.say(target, value);
      if (drafts[target] === sent) drafts[target] = '';
      toast(t('speak.queued'), 'success');
      void refreshStatus(target);
    } catch (e) {
      reportError(e);
    } finally {
      busy = false;
    }
  }

  function keydown(event: KeyboardEvent) {
    // Enter sends, Shift+Enter starts a new line (as in chat apps).
    if (event.key === 'Enter' && !event.shiftKey && !event.isComposing) {
      event.preventDefault();
      void say();
    }
  }

  async function stop() {
    // The stop button goes away once nothing is spoken: keep focus in the card.
    document.getElementById('speak-input')?.focus();
    try {
      await api.stopSpeech(id);
      void refreshStatus(id);
    } catch (e) {
      reportError(e);
    }
  }
</script>

<section class="card speak" aria-labelledby="speak-label">
  <form class="stack-sm" onsubmit={say}>
    <div class="head">
      <label id="speak-label" for="speak-input" class="label">{t('speak.label')}</label>
      {#if speaking}<span class="badge live" aria-hidden="true">{t('speak.speaking')}</span>{/if}
      <!-- Always present, so screen readers announce the change. -->
      <span class="sr-only" role="status">{speaking ? t('speak.speaking') : ''}</span>
    </div>
    <textarea
      id="speak-input"
      rows="2"
      enterkeyhint="send"
      value={text}
      oninput={(e) => (drafts[id] = e.currentTarget.value)}
      onkeydown={keydown}
      placeholder={t('speak.placeholder')}
      disabled={offline}
      aria-describedby="speak-hint"
      aria-invalid={tooLong ? 'true' : undefined}
    ></textarea>
    <div class="hint-row">
      <p id="speak-hint" class="hint">
        {#if offline}{t('speak.offline')}{:else}{t('speak.hint')}{' '}<span class="keys">{t('speak.keys')}</span>{/if}
      </p>
      {#if length > MAX * 0.8}
        <span class="hint count" class:over={tooLong} aria-live="polite">{t('speak.count', { count: length, max: MAX })}</span>
      {/if}
    </div>
    <div class="row">
      <button class="btn btn-primary" type="submit" disabled={!text.trim() || busy || offline || tooLong} aria-busy={busy}>
        <Icon name="volume" size={16} />{busy ? t('speak.sending') : t('speak.say')}
      </button>
      {#if speaking}
        <button class="btn" type="button" onclick={stop}>
          <Icon name="stop" size={16} />{t('speak.stop')}
        </button>
      {/if}
    </div>
  </form>
</section>

<style>
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2);
  }
  /* Prose, not code (the global textarea style is monospace). */
  textarea {
    resize: vertical;
    min-height: 4.5em;
    font-family: inherit;
    font-size: inherit;
  }
  .hint-row {
    display: flex;
    justify-content: space-between;
    gap: var(--space-2);
  }
  .count {
    flex-shrink: 0;
    font-variant-numeric: tabular-nums;
  }
  .count.over {
    color: var(--danger);
  }
  /* Touch keyboards have no Shift: Enter always sends there. */
  @media not (pointer: fine) {
    .keys {
      display: none;
    }
  }
</style>
