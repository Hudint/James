<script lang="ts">
  import { onMount } from 'svelte';
  import { api } from '$lib/api';
  import { confirmAction } from '$lib/confirm.svelte';
  import Icon from '$lib/components/Icon.svelte';
  import { i18n, localeNames, setLocale, t, type Locale } from '$lib/i18n/index.svelte';
  import { applyTheme, reportError, session, toast, ui, type Theme } from '$lib/session.svelte';
  import type { Identity, TokenInfo } from '$lib/types';

  let currentPassword = $state('');
  let newPassword = $state('');
  let identities = $state<Identity[]>([]);
  let tokens = $state<TokenInfo[]>([]);
  let linkCode = $state<string | null>(null);
  let tokenName = $state('');
  let tokenDays = $state('90');
  let createdToken = $state<string | null>(null);

  async function load() {
    try {
      [identities, tokens] = await Promise.all([api.identities(), api.tokens()]);
    } catch (e) {
      reportError(e);
    }
  }

  onMount(load);

  async function changePassword(event: SubmitEvent) {
    event.preventDefault();
    try {
      await api.changePassword(session.me?.user.hasPassword ? currentPassword : null, newPassword);
      currentPassword = '';
      newPassword = '';
      toast(t('account.passwordChanged'), 'success');
    } catch (e) {
      reportError(e);
    }
  }

  async function startLink() {
    try {
      linkCode = (await api.linkCode()).code;
    } catch (e) {
      reportError(e);
    }
  }

  async function unlink(identity: Identity) {
    if (await confirmAction(`${identity.name || identity.uid}?`, t('common.delete'))) {
      await api.unlink(identity.platform, identity.uid).catch(reportError);
      await load();
    }
  }

  async function createToken(event: SubmitEvent) {
    event.preventDefault();
    try {
      createdToken = (await api.createToken(tokenName.trim() || 'token', tokenDays ? Number(tokenDays) : null)).token;
      tokenName = '';
      await load();
    } catch (e) {
      reportError(e);
    }
  }

  async function revoke(token: TokenInfo) {
    if (await confirmAction(`${token.name || token.id}?`, t('common.delete'))) {
      await api.revokeToken(token.id).catch(reportError);
      await load();
    }
  }

  async function copy(text: string) {
    try {
      await navigator.clipboard.writeText(text);
      toast(t('common.copied'), 'success');
    } catch {
      // clipboard unavailable (insecure context): the text stays selectable
    }
  }

  const date = (secs: number) => new Date(secs * 1000).toLocaleDateString(i18n.locale);
</script>

<svelte:head><title>{t('account.title')} · James</title></svelte:head>

<div class="page narrow">
  <header class="page-header"><h1>{t('account.title')}</h1></header>

  <section class="card stack">
    <div class="form-grid">
      <div class="field">
        <label for="language">{t('account.language')}</label>
        <select id="language" value={i18n.locale} onchange={(e) => setLocale(e.currentTarget.value as Locale)}>
          {#each Object.entries(localeNames) as [code, name] (code)}
            <option value={code}>{name}</option>
          {/each}
        </select>
      </div>
      <div class="field">
        <label for="theme">{t('account.theme')}</label>
        <select id="theme" value={ui.theme} onchange={(e) => applyTheme(e.currentTarget.value as Theme)}>
          <option value="system">{t('account.themeSystem')}</option>
          <option value="light">{t('account.themeLight')}</option>
          <option value="dark">{t('account.themeDark')}</option>
        </select>
      </div>
    </div>
  </section>

  <form class="card stack" onsubmit={changePassword}>
    <h2>{t('account.password')}</h2>
    <input type="text" class="visually-hidden" autocomplete="username" value={session.me?.user.username ?? ''} readonly tabindex="-1" aria-hidden="true" />
    <div class="form-grid">
      {#if session.me?.user.hasPassword}
        <div class="field">
          <label for="current-password">{t('account.currentPassword')}</label>
          <input id="current-password" type="password" required autocomplete="current-password" bind:value={currentPassword} />
        </div>
      {/if}
      <div class="field">
        <label for="new-password">{t('account.newPassword')}</label>
        <input id="new-password" type="password" required minlength="8" autocomplete="new-password" bind:value={newPassword} aria-describedby="pw-hint" />
        <span id="pw-hint" class="hint">{t('auth.passwordHint')}</span>
      </div>
    </div>
    <div><button class="btn btn-primary" type="submit">{t('common.save')}</button></div>
  </form>

  <section class="card stack">
    <h2>{t('account.identities')}</h2>
    <p class="muted small">{t('account.identitiesHint')}</p>
    {#if identities.length === 0}
      <p class="muted">{t('account.noIdentities')}</p>
    {:else}
      <ul class="list">
        {#each identities as identity (identity.platform + identity.uid)}
          <li>
            <Icon name="link" />
            <span class="grow truncate">{identity.name || identity.uid} <span class="muted small">· {identity.platform}</span></span>
            <button class="icon-btn small" onclick={() => unlink(identity)} aria-label="{t('common.delete')}: {identity.name}"><Icon name="trash" size={16} /></button>
          </li>
        {/each}
      </ul>
    {/if}
    {#if linkCode}
      <div class="callout" role="status">
        <p>{t('account.linkInstructions')}</p>
        <p class="small muted">{t('account.linkPrefixHint')}</p>
        <div class="row">
          <code class="code">!link {linkCode}</code>
          <button class="btn btn-small" onclick={() => copy(`!link ${linkCode}`)}>{t('common.copy')}</button>
        </div>
      </div>
    {:else}
      <div><button class="btn" onclick={startLink}><Icon name="link" size={16} />{t('account.linkStart')}</button></div>
    {/if}
  </section>

  <section class="card stack">
    <h2>{t('account.tokens')}</h2>
    <p class="muted small">{t('account.tokensHint')}</p>
    {#if createdToken}
      <div class="callout" role="status">
        <p>{t('account.tokenCreated')}</p>
        <div class="row">
          <code class="code token">{createdToken}</code>
          <button class="btn btn-small" onclick={() => copy(createdToken ?? '')}>{t('common.copy')}</button>
        </div>
      </div>
    {/if}
    {#if tokens.filter((tk) => tk.kind === 'api').length === 0}
      <p class="muted">{t('account.noTokens')}</p>
    {:else}
      <ul class="list">
        {#each tokens.filter((tk) => tk.kind === 'api') as token (token.id)}
          <li>
            <Icon name="key" />
            <span class="grow truncate">{token.name || token.id} <span class="muted small">· {date(token.createdAt)}</span></span>
            <button class="icon-btn small" onclick={() => revoke(token)} aria-label="{t('common.delete')}: {token.name}"><Icon name="trash" size={16} /></button>
          </li>
        {/each}
      </ul>
    {/if}
    <form class="row" onsubmit={createToken}>
      <label class="visually-hidden" for="token-name">{t('account.tokenName')}</label>
      <input id="token-name" class="grow" type="text" maxlength="64" placeholder={t('account.tokenName')} bind:value={tokenName} />
      <label class="visually-hidden" for="token-days">{t('account.tokenLifetime')}</label>
      <select id="token-days" class="lifetime" bind:value={tokenDays}>
        {#each ['30', '90', '365'] as days (days)}
          <option value={days}>{t('account.days', { count: Number(days) })}</option>
        {/each}
        <option value="">{t('account.never')}</option>
      </select>
      <button class="btn" type="submit"><Icon name="plus" size={16} />{t('common.create')}</button>
    </form>
  </section>
</div>

<style>
  .narrow {
    max-width: 880px;
  }
  .grow {
    flex: 1;
    min-width: 0;
  }
  form.row {
    flex-wrap: nowrap;
  }
  .callout {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    padding: var(--space-3);
    border-radius: var(--radius);
    background: var(--accent-soft);
  }
  .code {
    font-size: var(--text-base);
    padding: var(--space-1) var(--space-2);
    user-select: all;
  }
  .lifetime {
    width: auto;
  }
  .token {
    word-break: break-all;
    font-size: var(--text-sm);
  }
</style>
