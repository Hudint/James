<script lang="ts">
  // The bot's TeamSpeak identity: shows its unique id and imports an existing identity (e.g.
  // from the TeamSpeak client or SinusBot), so the bot keeps its server groups.
  import { api, ApiError } from '$lib/api';
  import { confirmAction } from '$lib/confirm.svelte';
  import { t } from '$lib/i18n/index.svelte';
  import { copyText } from '$lib/clipboard';
  import { reportError, session, toast } from '$lib/session.svelte';
  import type { BotIdentity } from '$lib/types';
  import { tick, untrack } from 'svelte';
  import Icon from './Icon.svelte';

  let { id, nickname }: { id: string; nickname?: string } = $props();

  /** `undefined` while loading, `null` before the first connection. */
  let identity = $state<BotIdentity | null | undefined>(undefined);
  let input = $state('');
  let busy = $state(false);
  let loadError = $state('');
  let uidCode = $state<HTMLElement | null>(null);
  let uidTs6Code = $state<HTMLElement | null>(null);

  /** Minutes the private identity stays loaded after it was shown (or kept open). */
  const SECRET_MINUTES = 2;
  /**
   * The private identity: `confirm` asks for the warning to be read (and the password), `shown`
   * holds it, blurred until revealed.
   */
  let step = $state<'idle' | 'confirm' | 'shown'>('idle');
  let secret = $state<string | null>(null);
  let revealed = $state(false);
  let password = $state('');
  let secretBusy = $state(false);
  let secretError = $state('');
  /** When the shown identity closes by itself, and the clock showing how long is left. */
  let expiresAt = $state(0);
  let now = $state(Date.now());
  /** It closed by itself (said in the idle state, and announced). */
  let closedByTimer = $state(false);
  /** Requests started before the last close are ignored when they return. */
  let generation = 0;
  let showButton = $state<HTMLButtonElement | null>(null);
  let passwordInput = $state<HTMLInputElement | null>(null);
  let revealButton = $state<HTMLButtonElement | null>(null);
  let secretCode = $state<HTMLElement | null>(null);
  let secretSection = $state<HTMLElement | null>(null);

  let hasPassword = $derived(session.me?.user.hasPassword ?? false);

  // Also when the page is reused for another instance: nothing of the previous one stays.
  $effect(() => {
    const current = id;
    untrack(() => {
      identity = undefined;
      loadError = '';
      closeSecret(false);
      closedByTimer = false;
    });
    api.identity(current).then(
      (r) => current === id && (identity = r.identity),
      (e) => {
        if (current !== id) return;
        identity = null;
        loadError = (e as Error).message;
      },
    );
  });

  // Forgotten again after a while.
  $effect(() => {
    if (secret === null) return;
    const timer = setInterval(() => {
      now = Date.now();
      if (now < expiresAt || secret === null) return;
      closeSecret(false);
      closedByTimer = true;
    }, 1000);
    return () => clearInterval(timer);
  });
  let remaining = $derived(Math.max(0, Math.ceil((expiresAt - now) / 1000)));
  let remainingText = $derived(`${Math.floor(remaining / 60)}:${String(remaining % 60).padStart(2, '0')}`);

  function keepOpen() {
    now = Date.now();
    expiresAt = now + SECRET_MINUTES * 60_000;
  }

  // Blurred again when the tab is left (screen sharing, stepping away).
  $effect(() => {
    if (!revealed) return;
    const blur = () => {
      if (document.hidden) blurSecret(secretSection?.contains(document.activeElement) ?? false);
    };
    document.addEventListener('visibilitychange', blur);
    return () => document.removeEventListener('visibilitychange', blur);
  });

  async function startSecret() {
    step = 'confirm';
    secretError = '';
    closedByTimer = false;
    await tick();
    passwordInput?.focus();
  }

  async function loadSecret(event: SubmitEvent) {
    event.preventDefault();
    if (secretBusy) return;
    const started = generation;
    secretBusy = true;
    secretError = '';
    try {
      const exported = (await api.exportIdentity(id, password)).identity;
      // Cancelled meanwhile, or another instance.
      if (started !== generation) return;
      secret = exported;
      password = '';
      revealed = false;
      keepOpen();
      step = 'shown';
      await tick();
      revealButton?.focus();
    } catch (e) {
      if (started !== generation) return;
      secretError =
        e instanceof ApiError && e.message === 'the password is wrong'
          ? t('identity.secretWrongPassword')
          : (e as Error).message;
      await tick();
      passwordInput?.select();
    } finally {
      if (started === generation) secretBusy = false;
    }
  }

  async function reveal() {
    revealed = true;
    await tick();
    secretCode?.focus();
  }

  /** `focus`: to the reveal button that replaces the identity (where the focus was). */
  async function blurSecret(focus = true) {
    revealed = false;
    if (!focus) return;
    await tick();
    revealButton?.focus();
  }

  /** Forgets the private identity; `focus` returns to the button that shows it again. */
  async function closeSecret(focus = true) {
    const hadFocus = focus || secretSection?.contains(document.activeElement);
    generation++;
    secretBusy = false;
    secret = null;
    revealed = false;
    password = '';
    secretError = '';
    step = 'idle';
    if (!hadFocus) return;
    await tick();
    showButton?.focus();
  }

  async function copySecret() {
    if (secret === null) return;
    try {
      await navigator.clipboard.writeText(secret);
      toast(t('common.copied'), 'success');
    } catch {
      // No clipboard access (plain HTTP): it is selected for Ctrl+C, so it must be visible.
      await reveal();
      await copyText(secret, secretCode);
    }
  }

  /** As the TeamSpeak client exports it (Tools → Identities → Import reads this). */
  function downloadSecret() {
    if (secret === null) return;
    const name = (nickname || id).replace(/[\u0000-\u001f"=[\]]/g, ' ').trim() || id;
    const file = `[Identity]\r\nid=${name}\r\nidentity="${secret}"\r\nnickname=${name}\r\n`;
    const url = URL.createObjectURL(new Blob([file], { type: 'application/octet-stream' }));
    const link = document.createElement('a');
    link.href = url;
    link.download = `${name.replace(/[^\w.-]+/g, '_')}-identity.ini`;
    link.click();
    // Not at once: some browsers start the download only after the click returned.
    setTimeout(() => URL.revokeObjectURL(url), 1000);
    toast(t('identity.secretDownloaded', { file: link.download }), 'info');
  }

  /** Stand-in shown under the blur: the real value is not in the page until revealed. */
  const mask = (length: number) =>
    Array.from({ length }, (_, i) => 'Xk9QmZ2vR7tLw4Hb'[(i * 7) % 16]).join('');

  async function importIdentity(event: SubmitEvent) {
    event.preventDefault();
    if (!input.trim() || busy) return;
    if (!(await confirmAction(t('identity.confirm'), t('identity.import')))) return;
    busy = true;
    try {
      identity = (await api.importIdentity(id, input.trim())).identity;
      input = '';
      // The shown one is not the bot's any more.
      closeSecret(false);
      toast(t('identity.imported'), 'success');
    } catch (e) {
      reportError(e);
    } finally {
      busy = false;
    }
  }
</script>

<section class="card stack identity" aria-labelledby="identity-title">
  <h2 id="identity-title">{t('identity.title')}</h2>
  {#if loadError}
    <p class="error-text" role="alert">{loadError}</p>
  {:else if identity === undefined}
    <p class="muted small" aria-busy="true">{t('common.loading')}</p>
  {:else if identity === null}
    <p class="muted small">{t('identity.none')}</p>
  {:else}
    <dl class="facts">
      <dt>{t('identity.uid')}</dt>
      <dd class="uid">
        <code bind:this={uidCode}>{identity.uid}</code>
        <button class="icon-btn small" type="button" onclick={() => copyText(identity!.uid, uidCode)} aria-label="{t('common.copy')}: {t('identity.uid')}" title={t('common.copy')}>
          <Icon name="copy" size={14} />
        </button>
      </dd>
      {#if identity.uidTs6}
        <dt>{t('identity.uidTs6')}</dt>
        <dd class="uid">
          <code bind:this={uidTs6Code}>{identity.uidTs6}</code>
          <button class="icon-btn small" type="button" onclick={() => copyText(identity!.uidTs6!, uidTs6Code)} aria-label="{t('common.copy')}: {t('identity.uidTs6')}" title={t('common.copy')}>
            <Icon name="copy" size={14} />
          </button>
        </dd>
      {/if}
      {#if identity.securityLevel !== undefined}
        <dt>{t('identity.level')}</dt>
        <dd>{identity.securityLevel} <span class="muted">({t('identity.levelHint')})</span></dd>
      {/if}
    </dl>
  {/if}
  {#if session.me?.user.isAdmin && identity}
    <div class="secret stack-sm" role="group" aria-labelledby="secret-title" bind:this={secretSection}>
      <h3 id="secret-title" class="label">{t('identity.secretTitle')}</h3>
      {#if step === 'idle'}
        <p class="hint">{t('identity.secretHint')}</p>
        {#if closedByTimer}
          <p class="hint">{t('identity.secretClosed')}</p>
        {/if}
        {#if hasPassword}
          <div>
            <button class="btn" type="button" bind:this={showButton} onclick={startSecret}>
              <Icon name="eye" size={16} />{t('identity.secretShow')}
            </button>
          </div>
        {:else}
          <!-- Single sign-on can sign in again silently: only the password proves the admin. -->
          <p class="hint">{t('identity.secretNeedsPassword')}</p>
        {/if}
      {:else if step === 'confirm'}
        <form class="warning stack-sm" onsubmit={loadSecret}>
          <p class="warning-text">
            <Icon name="alert" size={18} />
            <span><strong>{t('identity.secretWarning')}</strong> {t('identity.secretWarningMore')}</span>
          </p>
          <label for="secret-password" class="label">{t('identity.secretPassword')}</label>
          <input
            id="secret-password"
            type="password"
            autocomplete="current-password"
            required
            bind:this={passwordInput}
            bind:value={password}
            aria-invalid={secretError ? 'true' : undefined}
            aria-describedby={secretError ? 'secret-error' : undefined}
            onkeydown={(e) => e.key === 'Escape' && closeSecret()}
          />
          {#if secretError}
            <p id="secret-error" class="error-text" role="alert">{secretError}</p>
          {/if}
          <div class="actions">
            <button class="btn btn-danger" type="submit" disabled={secretBusy || !password} aria-busy={secretBusy}>
              <Icon name="eye" size={16} />{secretBusy ? t('identity.secretLoading') : t('identity.secretShow')}
            </button>
            <button class="btn" type="button" onclick={() => closeSecret()}>{t('common.cancel')}</button>
          </div>
        </form>
      {:else if secret !== null}
        <div class="vault" class:revealed>
          {#if revealed}
            <!-- Takes the focus of the reveal button (it disappears). -->
            <code bind:this={secretCode} tabindex="-1">{secret}</code>
          {:else}
            <code aria-hidden="true">{mask(secret.length)}</code>
            <div class="veil">
              <button class="btn reveal" type="button" bind:this={revealButton} onclick={reveal} aria-describedby="secret-veil-hint">
                <Icon name="eye" size={16} />{t('identity.secretReveal')}
              </button>
              <span id="secret-veil-hint" class="visually-hidden">{t('identity.secretVeilHint')}</span>
            </div>
          {/if}
        </div>
        <div class="actions">
          <button class="btn" type="button" onclick={copySecret}>
            <Icon name="copy" size={16} />{t('common.copy')}
          </button>
          <button class="btn" type="button" onclick={downloadSecret}>
            <Icon name="download" size={16} />{t('identity.secretDownload')}
          </button>
          {#if revealed}
            <button class="btn" type="button" onclick={() => blurSecret()}>
              <Icon name="eye-off" size={16} />{t('identity.secretBlur')}
            </button>
          {/if}
          <!-- Phones: alone in its row while blurred, so it does not move under the thumb. -->
          <button class="btn close" class:wide={!revealed} type="button" onclick={() => closeSecret()}>
            <Icon name="x" size={16} />{t('identity.secretClose')}
          </button>
        </div>
        <p class="hint timer">
          {t('identity.secretAutoHide', { time: remainingText })}
          <button class="btn btn-small" type="button" onclick={keepOpen}>
            <Icon name="refresh" size={14} />{t('identity.secretKeepOpen')}
          </button>
        </p>
        <p class="hint">{t('identity.secretClipboard')}</p>
      {/if}
      <!-- Warned in time to keep it open (the ticking clock itself is not announced). -->
      <p class="visually-hidden" role="status">
        {#if closedByTimer}{t('identity.secretClosed')}{:else if secret !== null && remaining > 0 && remaining <= 30}{t('identity.secretClosingSoon')}{/if}
      </p>
    </div>
  {/if}
  {#if session.me?.user.isAdmin && !loadError}
  <form class="stack-sm" onsubmit={importIdentity}>
    <label for="identity-input" class="label">{t('identity.importLabel')}</label>
    <textarea
      id="identity-input"
      rows="3"
      bind:value={input}
      autocomplete="off"
      spellcheck="false"
      placeholder={'identity="123V…"'}
      aria-describedby="identity-hint"
    ></textarea>
    <p id="identity-hint" class="hint">{t('identity.hint')}</p>
    <div>
      <button class="btn" type="submit" disabled={!input.trim() || busy} aria-busy={busy}>
        <Icon name="key" size={16} />{busy ? t('identity.importing') : t('identity.import')}
      </button>
    </div>
  </form>
  {/if}
</section>

<style>
  /* Own section with its own button: clearly apart from the form's save bar above. */
  .identity {
    margin-top: var(--space-6);
  }
  .facts {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: var(--space-1) var(--space-3);
    margin: 0;
    font-size: var(--text-sm);
  }
  dt {
    color: var(--text-muted);
  }
  dd {
    margin: 0;
    min-width: 0;
  }
  .uid {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }
  .uid code {
    overflow-wrap: anywhere;
  }
  .secret {
    border-top: 1px solid var(--border);
    padding-top: var(--space-4);
  }
  .secret h3 {
    margin: 0;
  }
  /* The warning before loading it, and the box holding it: set apart from the import below. */
  .warning,
  .vault {
    border: 1px solid color-mix(in srgb, var(--danger) 35%, var(--border));
    border-radius: var(--radius);
    background: color-mix(in srgb, var(--danger-soft) 45%, var(--surface));
    padding: var(--space-3);
  }
  .warning-text {
    display: flex;
    gap: var(--space-2);
    margin: 0;
    font-size: var(--text-sm);
  }
  .warning-text :global(svg) {
    flex: none;
    color: var(--danger);
    margin-top: 1px;
  }
  .warning input {
    max-width: 20rem;
  }
  .vault {
    position: relative;
  }
  .vault:not(.revealed) {
    min-height: 5rem;
  }
  .vault code {
    display: block;
    padding: 0;
    border: 0;
    background: none;
    font-size: var(--text-sm);
    overflow-wrap: anywhere;
  }
  .vault code:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .vault:not(.revealed) code {
    filter: blur(6px);
    user-select: none;
  }
  /* Over the blurred stand-in: a solid button, readable whatever is under it. */
  .veil {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: var(--space-1);
  }
  .reveal {
    background: var(--surface);
    box-shadow: var(--shadow);
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
  }
  @media (prefers-reduced-motion: no-preference) {
    .vault code {
      transition: filter 0.2s;
    }
  }
  .timer {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-2);
  }
  /* Phones: the actions as two equal columns. */
  @media (max-width: 600px) {
    .vault ~ .actions {
      display: grid;
      grid-template-columns: 1fr 1fr;
    }
    .close.wide {
      grid-column: 1 / -1;
    }
  }
  /* Narrow screens: labels above the long ids. */
  @media (max-width: 600px) {
    .facts {
      grid-template-columns: 1fr;
    }
    dd + dt {
      margin-top: var(--space-2);
    }
  }
</style>
