<script lang="ts">
  // The bot's TeamSpeak identity: shows its unique id and imports an existing identity (e.g.
  // from the TeamSpeak client or SinusBot), so the bot keeps its server groups.
  import { api } from '$lib/api';
  import { confirmAction } from '$lib/confirm.svelte';
  import { t } from '$lib/i18n/index.svelte';
  import { copyText } from '$lib/clipboard';
  import { reportError, session, toast } from '$lib/session.svelte';
  import type { BotIdentity } from '$lib/types';
  import Icon from './Icon.svelte';

  let { id }: { id: string } = $props();

  /** `undefined` while loading, `null` before the first connection. */
  let identity = $state<BotIdentity | null | undefined>(undefined);
  let input = $state('');
  let busy = $state(false);
  let loadError = $state('');
  let uidCode = $state<HTMLElement | null>(null);
  let uidTs6Code = $state<HTMLElement | null>(null);

  $effect(() => {
    api.identity(id).then(
      (r) => (identity = r.identity),
      (e) => {
        identity = null;
        loadError = (e as Error).message;
      },
    );
  });


  async function importIdentity(event: SubmitEvent) {
    event.preventDefault();
    if (!input.trim() || busy) return;
    if (!(await confirmAction(t('identity.confirm'), t('identity.import')))) return;
    busy = true;
    try {
      identity = (await api.importIdentity(id, input.trim())).identity;
      input = '';
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
