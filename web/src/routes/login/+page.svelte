<script lang="ts">
  import { onMount } from 'svelte';
  import { goto, replaceState } from '$app/navigation';
  import { page } from '$app/state';
  import { api, ApiError } from '$lib/api';
  import AuthCard from '$lib/components/AuthCard.svelte';
  import Icon from '$lib/components/Icon.svelte';
  import { t } from '$lib/i18n/index.svelte';
  import type { MessageKey } from '$lib/i18n/index.svelte';
  import { connectLive } from '$lib/live.svelte';
  import { loadSession } from '$lib/session.svelte';

  const SSO_ERRORS = ['not_configured', 'setup', 'unreachable', 'denied', 'expired', 'not_linked', 'not_allowed', 'disabled', 'failed'];

  let username = $state('');
  let password = $state('');
  let error = $state('');
  let busy = $state(false);
  /** Single sign-on provider, if configured. */
  let sso = $state<{ name: string } | null>(null);
  let redirecting = $state(false);
  /** Code of a failed single sign-on (translated when shown, so a language switch applies). */
  let ssoCode = $state<string | null>(null);

  onMount(() => {
    api.providers().then((p) => (sso = p.oidc), () => (sso = null));
    // A failed single sign-on comes back with an error code: show its message (never text
    // from the URL) and drop it from the address, so a reload does not show it again.
    const code = page.url.searchParams.get('error');
    if (code !== null) {
      ssoCode = SSO_ERRORS.includes(code) ? code : 'failed';
      replaceState('/login', {});
    }
  });

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    busy = true;
    error = '';
    ssoCode = null;
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

<!-- Back from the provider's page: the cached page must not stay in its redirecting state. -->
<svelte:window onpageshow={(e) => e.persisted && (redirecting = false)} />

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
    {#if error}
      <p id="login-error" class="error-text" role="alert">{error}</p>
    {:else if ssoCode}
      <!-- Not tied to a field: it is about the single sign-on, not the password. -->
      <p class="error-text" role="alert">{t(`auth.sso.${ssoCode}` as MessageKey)}</p>
    {/if}
    <button class="btn btn-primary" type="submit" disabled={busy || redirecting}>
      {busy ? t('auth.signingIn') : t('auth.signIn')}
    </button>
  </form>
  {#if sso}
    <div class="divider" aria-hidden="true"><span>{t('auth.or')}</span></div>
    <!-- A full page navigation: the provider's login page follows. -->
    <a
      class="btn sso"
      href="/api/v1/auth/oidc/login"
      data-sveltekit-reload
      aria-busy={redirecting}
      onclick={() => {
        redirecting = true;
        error = '';
        ssoCode = null;
      }}
    >
      <Icon name="external" size={16} />{redirecting ? t('auth.redirecting') : t('auth.signInWith', { name: sso.name })}
    </a>
  {/if}
</AuthCard>

<style>
  .divider {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    margin: var(--space-4) 0;
    color: var(--text-muted);
    font-size: var(--text-sm);
  }
  .divider::before,
  .divider::after {
    content: '';
    flex: 1;
    border-top: 1px solid var(--border);
  }
  .sso {
    width: 100%;
    justify-content: center;
    background: var(--surface-2);
  }
</style>
