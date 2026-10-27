<script lang="ts">
  import { i18n, t } from '$lib/i18n/index.svelte';
  import type { InstanceConfig } from '$lib/types';

  let {
    config = $bindable(),
    isNew,
    busy,
    dirty = true,
    onsubmit,
  }: { config: InstanceConfig; isNew: boolean; busy: boolean; dirty?: boolean; onsubmit: () => void } = $props();

  // TeamSpeak settings are stored as an untyped map; expose them as typed fields.
  const settings = $derived(config.settings as Record<string, string | null | undefined>);
  function setSetting(key: string, value: string) {
    config.settings = { ...config.settings, [key]: value.trim() === '' ? null : value };
  }

  const idPattern = '[a-z0-9_-]{1,32}';
  let bitrateKbps = $derived(config.audio.bitrate ? String(config.audio.bitrate / 1000) : '');

  function submit(event: SubmitEvent) {
    event.preventDefault();
    onsubmit();
  }
</script>

<form class="stack" onsubmit={submit}>
  <section class="card stack">
    <h2>{t('config.general')}</h2>
    <div class="form-grid">
      <div class="field">
        <label for="cfg-name">{t('config.name')}</label>
        <input id="cfg-name" type="text" required maxlength="64" bind:value={config.name} />
      </div>
      <div class="field">
        <label for="cfg-id">{t('config.id')}</label>
        <input id="cfg-id" type="text" required pattern={idPattern} disabled={!isNew} bind:value={config.id} aria-describedby="cfg-id-hint" />
        <span id="cfg-id-hint" class="hint">{t('config.idHint')}</span>
      </div>
    </div>
  </section>

  <section class="card stack">
    <h2>{t('config.connection')}</h2>
    <div class="form-grid">
      <div class="field">
        <label for="cfg-address">{t('config.address')}</label>
        <input id="cfg-address" type="text" required placeholder="ts.example.com" value={settings.address ?? ''} oninput={(e) => setSetting('address', e.currentTarget.value)} aria-describedby="cfg-address-hint" />
        <span id="cfg-address-hint" class="hint">{t('config.addressHint')}</span>
      </div>
      <div class="field">
        <label for="cfg-nickname">{t('config.nickname')}</label>
        <input id="cfg-nickname" type="text" required maxlength="30" value={settings.nickname ?? ''} oninput={(e) => setSetting('nickname', e.currentTarget.value)} />
      </div>
      <div class="field">
        <label for="cfg-server-password">{t('config.serverPassword')} <span class="muted">({t('common.optional')})</span></label>
        <input id="cfg-server-password" type="password" autocomplete="off" value={settings.server_password ?? ''} oninput={(e) => setSetting('server_password', e.currentTarget.value)} />
      </div>
      <div class="field">
        <label for="cfg-channel">{t('config.channel')} <span class="muted">({t('common.optional')})</span></label>
        <input id="cfg-channel" type="text" value={settings.channel ?? ''} oninput={(e) => setSetting('channel', e.currentTarget.value)} aria-describedby="cfg-channel-hint" />
        <span id="cfg-channel-hint" class="hint">{t('config.channelHint')}</span>
      </div>
      <div class="field">
        <label for="cfg-channel-password">{t('config.channelPassword')} <span class="muted">({t('common.optional')})</span></label>
        <input id="cfg-channel-password" type="password" autocomplete="off" value={settings.channel_password ?? ''} oninput={(e) => setSetting('channel_password', e.currentTarget.value)} />
      </div>
    </div>
  </section>

  <section class="card stack">
    <h2>{t('config.audio')}</h2>
    <div class="form-grid">
      <div class="field">
        <label for="cfg-volume">{t('config.volume')}: {new Intl.NumberFormat(i18n.locale, { style: 'percent' }).format(config.audio.volume / 100)}</label>
        <input id="cfg-volume" type="range" min="0" max="100" bind:value={config.audio.volume} />
      </div>
      <div class="field">
        <label for="cfg-bitrate">{t('config.bitrate')}</label>
        <select
          id="cfg-bitrate"
          value={bitrateKbps}
          onchange={(e) => {
            const v = e.currentTarget.value;
            config.audio.bitrate = v ? Number(v) * 1000 : null;
          }}
        >
          <option value="">{t('config.bitrateAuto')}</option>
          {#each [64, 96, 128, 160, 192, 256] as kbps (kbps)}
            <option value={String(kbps)}>{kbps} kbit/s</option>
          {/each}
        </select>
      </div>
      <div class="field">
        <label for="cfg-lufs">{t('config.loudness')}</label>
        <input id="cfg-lufs" type="number" min="-40" max="-5" step="1" bind:value={config.audio.target_lufs} aria-describedby="cfg-lufs-hint" />
        <span id="cfg-lufs-hint" class="hint">{t('config.loudnessHint')}</span>
      </div>
    </div>
  </section>

  <section class="card stack">
    <h2>{t('config.behaviour')}</h2>
    <div class="form-grid">
      <div class="field">
        <label for="cfg-prefix">{t('config.prefix')}</label>
        <input id="cfg-prefix" class="prefix" type="text" required maxlength="5" pattern="\S{'{1,5}'}" bind:value={config.command_prefix} />
      </div>
    </div>
    <label class="checkbox"><input type="checkbox" bind:checked={config.now_playing_description} />{t('config.description')}</label>
    <label class="checkbox"><input type="checkbox" bind:checked={config.autostart} />{t('config.autostart')}</label>
  </section>

  <div class="savebar">
    {#if !isNew && dirty}<span class="hint">{t('config.restartHint')}</span>{/if}
    <button class="btn btn-primary" type="submit" disabled={busy || !dirty}>{busy ? t('common.saving') : isNew ? t('common.create') : t('common.save')}</button>
  </div>
</form>

<style>
  .prefix {
    max-width: 6em;
  }
  /* Keeps the primary action reachable on long forms. */
  .savebar {
    position: sticky;
    bottom: 0;
    z-index: 5;
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: var(--space-3);
    flex-wrap: wrap;
    padding: var(--space-3) var(--space-4);
    margin: 0 calc(-1 * var(--space-4)) calc(-1 * var(--space-6));
    background: color-mix(in srgb, var(--bg) 92%, transparent);
    backdrop-filter: blur(6px);
    border-top: 1px solid var(--border);
  }
</style>
