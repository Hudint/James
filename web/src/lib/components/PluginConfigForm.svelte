<script lang="ts">
  // Form generated from a plugin's config fields (SinusBot `vars` schema).
  import { t } from '$lib/i18n/index.svelte';
  import type { ConfigField } from '$lib/types';

  let {
    fields,
    values = $bindable(),
    valid = $bindable(true),
    idPrefix,
  }: { fields: ConfigField[]; values: Record<string, unknown>; valid?: boolean; idPrefix: string } = $props();

  function visible(field: ConfigField): boolean {
      // Loose comparison like SinusBot (select indices may be stored as strings).
    return (field.conditions ?? []).every((c) => values[c.field] == c.value);
  }

  function value(field: ConfigField): unknown {
    return values[field.name] ?? field.default ?? '';
  }

  function set(field: ConfigField, v: unknown) {
    values = { ...values, [field.name]: v };
  }

  function lines(v: unknown): string {
    return Array.isArray(v) ? v.join('\n') : '';
  }

  function json(v: unknown): string {
    return JSON.stringify(v ?? [], null, 2);
  }

  let invalid = $state<Record<string, boolean>>({});

  function markInvalid(field: ConfigField, bad: boolean) {
    invalid = { ...invalid, [field.name]: bad };
  }

  // Only fields that are shown count (a hidden invalid field cannot be fixed).
  $effect(() => {
    valid = !fields.some((f) => invalid[f.name] && visible(f));
  });

  /** Text of `array` fields as typed (reformatted only when the field loses focus). */
  let drafts = $state<Record<string, string>>({});

  function setJson(field: ConfigField, text: string) {
    drafts = { ...drafts, [field.name]: text };
    try {
      const parsed = JSON.parse(text);
      if (!Array.isArray(parsed)) throw new Error();
      set(field, parsed);
      markInvalid(field, false);
    } catch {
      markInvalid(field, true);
    }
  }

  function setNumber(field: ConfigField, input: HTMLInputElement) {
    // An empty field means "not set" (the default applies), never NaN.
    const bad = input.validity.badInput || (input.value !== '' && Number.isNaN(input.valueAsNumber));
    if (!bad) set(field, input.value === '' ? undefined : input.valueAsNumber);
    markInvalid(field, bad);
  }
</script>

{#if fields.length === 0}
  <p class="muted small">{t('plugins.noSettings')}</p>
{:else}
  <div class="fields">
    {#each fields.filter(visible) as field (field.name)}
      {@const id = `${idPrefix}-${field.name}`}
      <div class="field" style:margin-left="{(field.indent ?? 0) * 16}px">
        {#if field.type === 'checkbox'}
          <label class="checkbox" for={id}>
            <input {id} type="checkbox" checked={!!value(field)} onchange={(e) => set(field, e.currentTarget.checked)} />
            {field.title ?? field.name}
          </label>
        {:else}
          <label for={id}>{field.title ?? field.name}</label>
          {#if field.type === 'multiline'}
            <textarea {id} placeholder={field.placeholder} value={String(value(field))} oninput={(e) => set(field, e.currentTarget.value)}></textarea>
          {:else if field.type === 'number'}
            <input {id} type="number" placeholder={field.placeholder} value={value(field)} aria-invalid={invalid[field.name] ? 'true' : undefined} aria-describedby={invalid[field.name] ? `${id}-error` : undefined} oninput={(e) => setNumber(field, e.currentTarget)} />
            {#if invalid[field.name]}<span class="error-text" id="{id}-error">{t('plugins.invalidNumber')}</span>{/if}
          {:else if field.type === 'password'}
            <input {id} type="password" autocomplete="new-password" value={String(value(field))} oninput={(e) => set(field, e.currentTarget.value)} />
          {:else if field.type === 'select'}
            <select {id} value={String(value(field))} onchange={(e) => set(field, Number(e.currentTarget.value))}>
              {#each field.options ?? [] as option, index (index)}
                <option value={String(index)}>{option}</option>
              {/each}
            </select>
          {:else if field.type === 'strings'}
            <textarea {id} aria-describedby="{id}-hint" value={lines(value(field))} oninput={(e) => set(field, e.currentTarget.value.split('\n').filter((l) => l.trim()))}></textarea>
            <span class="hint" id="{id}-hint">{t('plugins.onePerLine')}</span>
          {:else if field.type === 'array'}
            <textarea {id} aria-describedby="{id}-hint{invalid[field.name] ? ` ${id}-error` : ''}" aria-invalid={invalid[field.name] ? 'true' : undefined} value={drafts[field.name] ?? json(value(field))} oninput={(e) => setJson(field, e.currentTarget.value)} onblur={() => { if (!invalid[field.name]) { const { [field.name]: _, ...rest } = drafts; drafts = rest; } }}></textarea>
            <span class="hint" id="{id}-hint">
              {t('plugins.arrayHint', { fields: (field.vars ?? []).map((v) => v.name).join(', ') })}
            </span>
            {#if invalid[field.name]}<span class="error-text" id="{id}-error">{t('plugins.invalidJson')}</span>{/if}
          {:else}
            <input {id} type="text" placeholder={field.placeholder} value={String(value(field))} oninput={(e) => set(field, e.currentTarget.value)} />
            {#if field.type === 'channel'}<span class="hint">{t('plugins.channelHint')}</span>{/if}
          {/if}
        {/if}
      </div>
    {/each}
  </div>
{/if}

<style>
  .fields {
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
  }
</style>
