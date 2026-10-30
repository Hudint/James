<script lang="ts">
  import { channelTree as buildChannelTree, indent } from '$lib/channels';
  import { onMount } from 'svelte';
  import { api } from '$lib/api';
  import { i18n, t } from '$lib/i18n/index.svelte';
  import { can } from '$lib/session.svelte';
  import Icon from './Icon.svelte';
  import type { InstanceConfig, PlatformChannel, SpeechSettings, TtsVoice } from '$lib/types';

  let {
    config = $bindable(),
    isNew,
    busy,
    dirty = true,
    channels = null,
    onchannelsneeded,
    onsubmit,
  }: {
    config: InstanceConfig;
    isNew: boolean;
    busy: boolean;
    dirty?: boolean;
    /** Channels of the server the bot is connected to (offers a choice instead of typing). */
    channels?: PlatformChannel[] | null;
    /** Asks for a fresh channel list (the field got focus). */
    onchannelsneeded?: () => void;
    onsubmit: () => void;
  } = $props();

  // TeamSpeak settings are stored as an untyped map; expose them as typed fields.
  const settings = $derived(config.settings as Record<string, string | null | undefined>);
  function setSetting(key: string, value: string) {
    config.settings = { ...config.settings, [key]: value.trim() === '' ? null : value };
  }

  /** Channels depth first with depth and path (`Lobby/Music`). */
  let channelTree = $derived(buildChannelTree(channels));
  /** The configured channel (id or path) as an id of the list; `null` if not on the server. */
  let selectedChannel = $derived.by(() => {
    const value = settings.channel?.trim();
    if (!value) return '';
    // Paths are compared like the server resolves them: parts trimmed, outer slashes ignored.
    const normalized = value
      .split('/')
      .map((p) => p.trim())
      .filter(Boolean)
      .join('/');
    const listed = channelTree.find((c) => c.id === value || c.path === normalized);
    if (listed) return listed.id;
    // On the server but not offered (temporary or spacer): keep it as it is, not "missing".
    if ((channels ?? []).some((c) => c.id === value)) return value;
    return null;
  });

  const idPattern = '[a-z0-9_-]{1,32}';
  let bitrateKbps = $derived(config.audio.bitrate ? String(config.audio.bitrate / 1000) : '');

  /** `null` until loaded. */
  let voices = $state<TtsVoice[] | null>(null);
  /** `null` while asking the server, `undefined` if that failed. */
  let sttAvailable = $state<boolean | null | undefined>(null);
  $effect(() => {
    api.stt().then((r) => (sttAvailable = r.available), () => (sttAvailable = undefined));
  });
  const speech = $derived(config.speech ?? { enabled: false, language: null, wake_word: null });
  function setSpeech(change: Partial<SpeechSettings>) {
    config.speech = { ...speech, ...change };
  }
  /** Same rule as the server: 1-3 words of letters, digits and hyphens. */
  const WAKE_PATTERN = '[\\p{L}\\p{N}\\-]+( [\\p{L}\\p{N}\\-]+){0,2}';
  const SPEECH_LANGUAGES = ['de', 'en', 'fr', 'es', 'it', 'nl', 'pl', 'tr', 'pt', 'sv', 'da', 'cs'];
  const speechLanguages = $derived.by(() => {
    const names = new Intl.DisplayNames([i18n.locale], { type: 'language' });
    const codes = [...SPEECH_LANGUAGES, ...(speech.language && !SPEECH_LANGUAGES.includes(speech.language) ? [speech.language] : [])];
    return codes.map((code) => [code, names.of(code) ?? code] as const).sort((a, b) => a[1].localeCompare(b[1], i18n.locale));
  });
  /** Changing speech recognition needs its own permission. */
  const mayListen = $derived(can('instance.speech', config.id || undefined));
  const WAKE_REGEX = new RegExp(`^(?:${WAKE_PATTERN})$`, 'v');
  const validWake = (word: string) => word.length <= 40 && WAKE_REGEX.test(word);
  const wakeValid = $derived(!speech.wake_word || validWake(speech.wake_word));
  /** The name people hear: the bot's nickname. */
  const botName = $derived(typeof config.settings?.nickname === 'string' ? (config.settings.nickname as string).trim() : '');
  onMount(() => {
    api.voices().then((v) => (voices = v), () => (voices = []));
  });
  /** Voices grouped by language, sorted. */
  let voiceGroups = $derived(
    Object.entries(
      (voices ?? []).reduce<Record<string, TtsVoice[]>>((groups, voice) => {
        (groups[voice.language] ??= []).push(voice);
        return groups;
      }, {}),
    )
      .sort(([a], [b]) => a.localeCompare(b))
      .map(([language, list]) => [language, list.sort((a, b) => a.name.localeCompare(b.name))] as const),
  );
  /** The saved voice is no longer installed. */
  let voiceMissing = $derived(
    voices !== null && config.tts_voice !== null && !voices.some((v) => v.id === config.tts_voice),
  );

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
        {#if channels && channels.length}
          <select id="cfg-channel" value={selectedChannel ?? settings.channel ?? ''} onfocus={() => onchannelsneeded?.()} onchange={(e) => setSetting('channel', e.currentTarget.value)} aria-describedby="cfg-channel-hint">
            <option value="">{t('config.channelDefault')}</option>
            {#if selectedChannel !== null && selectedChannel !== '' && !channelTree.some((c) => c.id === selectedChannel)}
              <option value={selectedChannel}>{(channels ?? []).find((c) => c.id === selectedChannel)?.name ?? selectedChannel}</option>
            {/if}
            {#if selectedChannel === null}
              <option value={settings.channel ?? ''}>{t('config.channelMissing', { channel: settings.channel ?? '' })}</option>
            {/if}
            {#each channelTree as channel (channel.id)}
              <option value={channel.id}>{indent(channel.depth)}{channel.name}</option>
            {/each}
          </select>
          <span id="cfg-channel-hint" class="hint">{t('config.channelPickHint')}</span>
        {:else}
          <input id="cfg-channel" type="text" value={settings.channel ?? ''} onfocus={() => onchannelsneeded?.()} oninput={(e) => setSetting('channel', e.currentTarget.value)} aria-describedby="cfg-channel-hint" />
          <span id="cfg-channel-hint" class="hint">{t('config.channelHint')}</span>
        {/if}
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
        <input id="cfg-volume" type="range" min="0" max="100" bind:value={config.audio.volume} style:--p="{config.audio.volume}%" />
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
          {#each [64, 96, 128, 160, 192] as kbps (kbps)}
            <option value={String(kbps)}>{kbps} kbit/s</option>
          {/each}
          {#if bitrateKbps && ![64, 96, 128, 160, 192].includes(Number(bitrateKbps))}
            <!-- An older setting: shown as it is, used capped to what the platform takes. -->
            <option value={bitrateKbps}>{t('config.bitrateUnsupported', { kbps: bitrateKbps })}</option>
          {/if}
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
      {#if voices !== null}
        <div class="field">
          <label for="cfg-voice">{t('config.voice')}</label>
          {#if voices.length || voiceMissing}
            <select id="cfg-voice" value={config.tts_voice ?? ''} onchange={(e) => (config.tts_voice = e.currentTarget.value || null)} aria-describedby="cfg-voice-hint">
              <option value="">{t('config.voiceDefault')}</option>
              {#if voiceMissing}<option value={config.tts_voice} disabled>{t('config.voiceMissing', { id: config.tts_voice ?? '' })}</option>{/if}
              {#each voiceGroups as [language, list] (language)}
                <optgroup label={language}>
                  {#each list as voice (voice.id)}<option value={voice.id}>{voice.name}</option>{/each}
                </optgroup>
              {/each}
            </select>
            <span id="cfg-voice-hint" class="hint">{t('config.voiceHint')}</span>
          {:else}
            <span class="hint">{t('config.voiceNone')}</span>
          {/if}
        </div>
      {/if}
    </div>
    <label class="checkbox"><input type="checkbox" bind:checked={config.now_playing_description} />{t('config.description')}</label>
    <label class="checkbox"><input type="checkbox" bind:checked={config.autostart} />{t('config.autostart')}</label>
  </section>

  <section class="card stack" aria-labelledby="cfg-speech-title">
    <h2 id="cfg-speech-title">{t('config.speech')}</h2>
    <div class="privacy" role="note">
      <Icon name="alert" size={18} />
      <p>{t('config.speechHint')}</p>
    </div>
    {#if !mayListen}
      <p id="cfg-speech-state" class="hint">{t('config.speechNoPermission')}</p>
    {:else if sttAvailable === null}
      <p id="cfg-speech-state" class="hint">{t('config.speechChecking')}</p>
    {:else if sttAvailable === false}
      <p id="cfg-speech-state" class="hint">{t('config.speechUnavailable')}</p>
    {:else if sttAvailable === undefined}
      <p id="cfg-speech-state" class="hint">{t('config.speechUnknown')}</p>
    {/if}
    <label class="checkbox top">
      <input
        type="checkbox"
        checked={speech.enabled}
        disabled={!mayListen || sttAvailable === null || (sttAvailable !== true && !speech.enabled)}
        onchange={(e) => setSpeech({ enabled: e.currentTarget.checked })}
        aria-describedby={mayListen && sttAvailable === true ? 'cfg-speech-reconnect' : 'cfg-speech-state cfg-speech-reconnect'}
      />
      {t('config.speechEnabled')}
    </label>
    <p id="cfg-speech-reconnect" class="hint">{t('config.speechReconnect')}</p>
    {#if speech.enabled}
      <fieldset class="form-grid plain" disabled={!mayListen} aria-labelledby="cfg-speech-title">
        <div class="field">
          <label for="cfg-speech-language">{t('config.speechLanguage')}</label>
          <select id="cfg-speech-language" value={speech.language ?? ''} onchange={(e) => setSpeech({ language: e.currentTarget.value || null })}>
            <option value="">{t('config.speechLanguageAuto')}</option>
            {#each speechLanguages as [code, label] (code)}<option value={code}>{label}</option>{/each}
          </select>
        </div>
        <div class="field">
          <label for="cfg-wake">{t('config.wakeWord')}</label>
          <input
            id="cfg-wake"
            type="text"
            maxlength="40"
            pattern={WAKE_PATTERN}
            title={t('config.wakeWordRule')}
            value={speech.wake_word ?? ''}
            oninput={(e) => setSpeech({ wake_word: e.currentTarget.value.trim() || null })}
            aria-invalid={!wakeValid}
            aria-describedby="cfg-wake-hint"
          />
          <span id="cfg-wake-hint" class="hint" class:error={!wakeValid}>
            {#if !wakeValid}
              {t('config.wakeWordRule')}
            {:else if speech.wake_word}
              {t('config.wakeWordHint', { wake: speech.wake_word, prefix: config.command_prefix })}
            {:else}
              {t('config.wakeWordOff')}
            {/if}
            {#if botName && botName !== speech.wake_word && validWake(botName)}
              <button type="button" class="link-btn" onclick={() => setSpeech({ wake_word: botName })}>{t('config.wakeWordUseName', { name: botName })}</button>
            {/if}
          </span>
        </div>
      </fieldset>
    {/if}
  </section>

  <div class="savebar">
    {#if !isNew && dirty}<span class="hint">{t('config.restartHint')}</span>{/if}
    <button class="btn btn-primary" type="submit" disabled={busy || !dirty}>{busy ? t('common.saving') : isNew ? t('common.create') : t('common.save')}</button>
  </div>
</form>

<style>
  .privacy {
    display: flex;
    gap: var(--space-2);
    align-items: flex-start;
    padding: var(--space-3);
    border-radius: var(--radius);
    background: var(--warning-soft);
    color: var(--text);
    font-size: var(--text-sm);
  }
  .privacy :global(svg) {
    color: var(--warning);
    flex-shrink: 0;
    margin-top: 2px;
  }
  .privacy p {
    margin: 0;
  }
  fieldset.plain {
    border: 0;
    margin: 0;
    padding: 0;
    min-width: 0;
  }
  .checkbox.top {
    align-items: flex-start;
  }
  .checkbox.top input {
    margin-top: 3px;
  }
  #cfg-wake[aria-invalid='true'] {
    border-color: var(--danger);
  }
  .hint.error {
    color: var(--danger);
  }
  .link-btn {
    display: inline;
    margin-left: var(--space-1);
    padding: 0;
    border: 0;
    background: none;
    color: var(--accent);
    font: inherit;
    font-weight: 600;
    text-decoration: underline;
    text-underline-offset: 2px;
    cursor: pointer;
  }
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
    padding: var(--space-3) var(--space-4) calc(var(--space-3) + env(safe-area-inset-bottom));
    margin: 0 calc(-1 * var(--page-pad, var(--space-4)));
    background: color-mix(in srgb, var(--bg) 92%, transparent);
    backdrop-filter: blur(6px);
    border-top: 1px solid var(--border);
  }
  /* Phones: the main action across the width, within thumb reach. */
  @media (max-width: 600px) {
    .savebar .btn {
      flex: 1 1 100%;
    }
  }
  /* Wider screens: the bar ends flush with the page bottom, as before. */
  @media (min-width: 601px) {
    .savebar {
      margin-bottom: calc(-1 * var(--space-6));
    }
  }
</style>
