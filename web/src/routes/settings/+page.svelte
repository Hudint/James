<script lang="ts">
  // Server settings for administrators: YouTube cookies.
  import { api } from '$lib/api';
  import { confirmAction } from '$lib/confirm.svelte';
  import Icon from '$lib/components/Icon.svelte';
  import { i18n, t } from '$lib/i18n/index.svelte';
  import { reportError, session, toast } from '$lib/session.svelte';
  import type { CookieStatus } from '$lib/types';

  let status = $state<CookieStatus | null>(null);
  let busy = $state(false);
  let fileInput = $state<HTMLInputElement | null>(null);

  async function load() {
    try {
      status = await api.cookieStatus();
    } catch (e) {
      reportError(e);
    }
  }

  $effect(() => {
    if (session.me?.user.isAdmin) void load();
  });

  async function upload(event: Event) {
    const file = (event.currentTarget as HTMLInputElement).files?.[0];
    if (!file || busy) return;
    busy = true;
    try {
      const { cookies, dropped } = await api.uploadCookies(await file.text());
      toast(t('settings.cookiesSaved', { count: cookies }), 'success');
      if (dropped > 0) toast(t('settings.cookiesDropped', { count: dropped }), 'info');
      await load();
    } catch (e) {
      reportError(e);
    } finally {
      busy = false;
      if (fileInput) fileInput.value = '';
    }
  }

  async function remove() {
    if (!(await confirmAction(t('settings.cookiesRemoveConfirm'), t('common.delete')))) return;
    try {
      await api.removeCookies();
      toast(t('settings.cookiesRemoved'), 'success');
      await load();
    } catch (e) {
      reportError(e);
    }
  }

  const date = (secs: number) =>
    new Date(secs * 1000).toLocaleString(i18n.locale, { dateStyle: 'medium', timeStyle: 'short' });
</script>

<svelte:head><title>{t('settings.title')} · James</title></svelte:head>

<div class="page narrow">
  <header class="page-header"><h1>{t('settings.title')}</h1></header>
  {#if !session.me?.user.isAdmin}
    <p class="error-text" role="alert">{t('errors.forbidden')}</p>
  {:else}
    <section class="card stack" aria-labelledby="cookies-title">
      <h2 id="cookies-title">{t('settings.cookiesTitle')}</h2>
      <p class="muted small">{t('settings.cookiesIntro')}</p>
      {#if status === null}
        <p class="muted small" aria-busy="true">{t('common.loading')}</p>
      {:else}
        {#if status.configured}
          <p class="notice small" role="note"><Icon name="alert" size={16} />{t('settings.cookiesConfigured')}</p>
        {/if}
        <p class="state" role="status">
          {#if status.uploaded}
            {#if !status.configured}<Icon name="check" size={16} />{/if}
            {t('settings.cookiesStored', { count: status.uploaded.cookies })}
            {#if status.configured}<span class="muted">({t('settings.cookiesInactive')})</span>{/if}
            {#if status.uploaded.changedAt}<span class="muted nowrap">{t('settings.cookiesUploadedAt', { date: date(status.uploaded.changedAt) })}</span>{/if}
          {:else}
            {t('settings.cookiesNone')}
          {/if}
        </p>
        <div class="row">
          <label class="btn btn-primary" class:disabled={busy} aria-disabled={busy}>
            <Icon name="upload" size={16} />{busy ? t('settings.cookiesUploading') : status.uploaded ? t('settings.cookiesReplace') : t('settings.cookiesUpload')}
            <input bind:this={fileInput} class="sr-only" type="file" accept=".txt,text/plain" onchange={upload} disabled={busy} />
          </label>
          {#if status.uploaded}
            <button class="btn btn-danger" type="button" onclick={remove}><Icon name="trash" size={16} />{t('common.delete')}</button>
          {/if}
        </div>
        <details class="howto">
          <summary>{t('settings.cookiesHowTo')}</summary>
          <ol class="small">
            <li>{t('settings.cookiesStep1')}</li>
            <li>{t('settings.cookiesStep2')}</li>
            <li>{t('settings.cookiesStep3')}</li>
          </ol>
        </details>
      {/if}
    </section>
  {/if}
</div>

<style>
  .narrow {
    max-width: 880px;
  }
  .state {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    flex-wrap: wrap;
    margin: 0;
  }
  .notice {
    display: flex;
    gap: var(--space-2);
    align-items: flex-start;
    margin: 0;
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius);
    background: var(--surface-2);
  }
  label.btn {
    cursor: pointer;
  }
  label.btn:has(input:focus-visible) {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  label.btn.disabled {
    opacity: 0.6;
    cursor: progress;
  }
  .nowrap {
    white-space: nowrap;
  }
  .howto summary {
    cursor: pointer;
    font-size: var(--text-sm);
  }
  .howto ol {
    margin: var(--space-2) 0 0;
    padding-left: 1.25em;
  }
</style>
