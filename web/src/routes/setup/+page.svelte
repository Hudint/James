<script lang="ts">
  import { goto } from '$app/navigation';
  import { api } from '$lib/api';
  import AuthCard from '$lib/components/AuthCard.svelte';
  import { t } from '$lib/i18n/index.svelte';
  import { connectLive } from '$lib/live.svelte';
  import { loadSession } from '$lib/session.svelte';

  let token = $state('');
  let username = $state('admin');
  let password = $state('');
  let repeat = $state('');
  let error = $state('');
  let busy = $state(false);
  let mismatch = $derived(repeat.length > 0 && password !== repeat);

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    if (password !== repeat) return;
    busy = true;
    error = '';
    try {
      await api.setup(username, password, token);
      await loadSession();
      connectLive();
      await goto('/instances/new', { replaceState: true });
    } catch (e) {
      error = (e as Error).message;
    } finally {
      busy = false;
    }
  }
</script>

<svelte:head><title>{t('auth.setupTitle')}</title></svelte:head>

<AuthCard title={t('auth.setupTitle')} subtitle={t('auth.setupText')}>
  <form class="stack" onsubmit={submit}>
    <div class="field">
      <label for="setup-token">{t('auth.setupToken')}</label>
      <input id="setup-token" type="text" required autocomplete="off" spellcheck="false" bind:value={token} aria-describedby="token-hint" />
      <span id="token-hint" class="hint">{t('auth.setupTokenHint')}</span>
    </div>
    <div class="field">
      <label for="username">{t('auth.username')}</label>
      <input id="username" type="text" autocomplete="username" required bind:value={username} />
    </div>
    <div class="field">
      <label for="password">{t('auth.password')}</label>
      <input
        id="password"
        type="password"
        autocomplete="new-password"
        minlength="8"
        required
        bind:value={password}
        aria-describedby="password-hint"
      />
      <span id="password-hint" class="hint">{t('auth.passwordHint')}</span>
    </div>
    <div class="field">
      <label for="repeat">{t('auth.passwordRepeat')}</label>
      <input
        id="repeat"
        type="password"
        autocomplete="new-password"
        required
        bind:value={repeat}
        aria-invalid={mismatch ? 'true' : undefined}
        aria-describedby={mismatch ? 'repeat-error' : undefined}
      />
      {#if mismatch}<span id="repeat-error" class="error-text">{t('auth.passwordMismatch')}</span>{/if}
    </div>
    {#if error}<p class="error-text" role="alert">{error}</p>{/if}
    <button class="btn btn-primary" type="submit" disabled={busy || mismatch}>{t('auth.setupSubmit')}</button>
  </form>
</AuthCard>
