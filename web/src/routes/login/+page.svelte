<script lang="ts">
  import { goto } from '$app/navigation';
  import { api, ApiError } from '$lib/api';
  import AuthCard from '$lib/components/AuthCard.svelte';
  import { t } from '$lib/i18n/index.svelte';
  import { connectLive } from '$lib/live.svelte';
  import { loadSession } from '$lib/session.svelte';

  let username = $state('');
  let password = $state('');
  let error = $state('');
  let busy = $state(false);

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    busy = true;
    error = '';
    try {
      await api.login(username, password);
      await loadSession();
      connectLive();
      await goto('/', { replaceState: true });
    } catch (e) {
      error = e instanceof ApiError && e.status === 401 ? t('auth.invalid') : String((e as Error).message);
      password = '';
    } finally {
      busy = false;
    }
  }
</script>

<svelte:head><title>{t('auth.signIn')} · James</title></svelte:head>

<AuthCard title={t('auth.signIn')} subtitle={t('app.tagline')}>
  <form class="stack" onsubmit={submit}>
    <div class="field">
      <label for="username">{t('auth.username')}</label>
      <!-- svelte-ignore a11y_autofocus -->
      <input id="username" type="text" autocomplete="username" required autofocus bind:value={username} />
    </div>
    <div class="field">
      <label for="password">{t('auth.password')}</label>
      <input
        id="password"
        type="password"
        autocomplete="current-password"
        required
        bind:value={password}
        aria-invalid={error ? 'true' : undefined}
        aria-describedby={error ? 'login-error' : undefined}
      />
    </div>
    {#if error}<p id="login-error" class="error-text" role="alert">{error}</p>{/if}
    <button class="btn btn-primary" type="submit" disabled={busy}>
      {busy ? t('auth.signingIn') : t('auth.signIn')}
    </button>
  </form>
</AuthCard>
