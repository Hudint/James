<script lang="ts">
  // Form generated from a plugin's config fields (SinusBot `vars` schema).
  import { tick, untrack } from 'svelte';
  import { channelTree, indent } from '$lib/channels';
  import { t } from '$lib/i18n/index.svelte';
  import type { ConfigField, PlatformChannel } from '$lib/types';
  import Icon from './Icon.svelte';
  import PluginConfigForm from './PluginConfigForm.svelte';

  let {
    fields,
    values = $bindable(),
    valid = $bindable(true),
    idPrefix,
    channels = null,
    onchange,
    onvalidity,
  }: {
    fields: ConfigField[];
    values: Record<string, unknown>;
    valid?: boolean;
    idPrefix: string;
    /** The instance's channels while it is connected: `channel` fields become a choice. */
    channels?: PlatformChannel[] | null;
    /** Called with the new values on every change (for nested forms of list items). */
    onchange?: (values: Record<string, unknown>) => void;
    /** Called when the validity changes (for nested forms of list items). */
    onvalidity?: (valid: boolean) => void;
  } = $props();

  const tree = $derived(channelTree(channels));

  function visible(field: ConfigField): boolean {
      // Loose comparison like SinusBot (select indices may be stored as strings).
    return (field.conditions ?? []).every((c) => values[c.field] == c.value);
  }

  /** The stored value (own keys only: `__proto__` is just a name) or the default. */
  function stored(field: ConfigField): unknown {
    return Object.hasOwn(values, field.name) ? values[field.name] : undefined;
  }

  function value(field: ConfigField): unknown {
    return stored(field) ?? field.default ?? '';
  }

  function set(field: ConfigField, v: unknown) {
    values = { ...values, [field.name]: v };
    onchange?.(values);
  }

  const isObject = (v: unknown): v is Record<string, unknown> => !!v && typeof v === 'object' && !Array.isArray(v);

  /** Items of an `array` field (objects of its `vars`); `null` if the value is not such a list. */
  function items(field: ConfigField): Record<string, unknown>[] | null {
    const v = stored(field) ?? field.default ?? [];
    return Array.isArray(v) && v.every(isObject) ? v : null;
  }

  /**
   * A stable id per list entry, so an entry's form keeps its own state when others are removed.
   * Not reactive state: it changes together with the values, which re-render anyway.
   */
  let nextId = 0;
  const itemIds: Record<string, number[]> = {};
  function ids(field: ConfigField, count: number): number[] {
    const known = itemIds[field.name] ?? [];
    if (known.length === count) return known;
    const fixed = [...known.slice(0, count), ...Array.from({ length: Math.max(0, count - known.length) }, () => nextId++)];
    itemIds[field.name] = fixed;
    return fixed;
  }

  /** Short description of an entry for its heading (its first channel or text). */
  function summary(field: ConfigField, item: Record<string, unknown>): string {
    for (const v of field.vars ?? []) {
      const value = item[v.name];
      if (value === undefined || value === null || value === '') continue;
      if (v.type === 'channel') return tree.find((c) => c.id === String(value))?.name ?? String(value);
      if (v.type === 'string' || v.type === 'track') return String(value).slice(0, 60);
    }
    return '';
  }

  /** An entry whose `active` checkbox (as shown, default included) is off. */
  function inactive(field: ConfigField, item: Record<string, unknown>): boolean {
    const active = field.vars?.find((v) => v.name === 'active' && v.type === 'checkbox');
    return !!active && !(item.active ?? active.default);
  }

  let itemsRoot = $state<Record<string, HTMLElement | null>>({});
  let announcement = $state('');

  function setItem(field: ConfigField, index: number, item: Record<string, unknown>) {
    set(field, (items(field) ?? []).map((old, i) => (i === index ? item : old)));
  }

  function addItem(field: ConfigField) {
    const list = items(field) ?? [];
    // Shown defaults are saved too (James applies defaults to top-level settings only).
    const fresh = Object.fromEntries((field.vars ?? []).filter((v) => v.default !== undefined).map((v) => [v.name, v.default]));
    itemIds[field.name] = [...ids(field, list.length), nextId++];
    set(field, [...list, fresh]);
    // Focus the new entry's first control.
    void tick().then(() => {
      const entries = itemsRoot[field.name]?.querySelectorAll<HTMLElement>(':scope > fieldset');
      entries?.[entries.length - 1]?.querySelector<HTMLElement>('input, select, textarea')?.focus();
    });
  }

  function removeItem(field: ConfigField, index: number) {
    const list = items(field) ?? [];
    const kept = ids(field, list.length).filter((_, i) => i !== index);
    const removed = ids(field, list.length)[index];
    itemIds[field.name] = kept;
    if (removed !== undefined) {
      const { [removed]: _, ...rest } = itemValid[field.name] ?? {};
      itemValid = { ...itemValid, [field.name]: rest };
    }
    set(field, list.filter((_, i) => i !== index));
    announcement = t('plugins.itemRemoved', { n: index + 1 });
    // Focus the entry that took its place (or the one before, or "Add entry").
    void tick().then(() => {
      const root = itemsRoot[field.name];
      const buttons = root?.querySelectorAll<HTMLElement>(':scope > fieldset > .remove');
      (buttons?.[Math.min(index, (buttons?.length ?? 1) - 1)] ?? root?.querySelector<HTMLElement>('.add'))?.focus();
    });
  }

  /** Validity of list entries' forms, per field and entry id. */
  let itemValid = $state<Record<string, Record<number, boolean>>>({});

  function setItemValid(field: ConfigField, id: number, ok: boolean) {
    if (itemValid[field.name]?.[id] === ok) return;
    itemValid = { ...itemValid, [field.name]: { ...itemValid[field.name], [id]: ok } };
  }

  /** The channel a `channel` field holds is not among the server's (deleted, or another server). */
  function unknownChannel(field: ConfigField): string | null {
    const v = String(value(field) ?? '');
    return v && !tree.some((c) => c.id === v) ? v : null;
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
    const ok = !fields.some(
      (f) => visible(f) && (invalid[f.name] || Object.values(itemValid[f.name] ?? {}).some((v) => !v)),
    );
    valid = ok;
    untrack(() => onvalidity?.(ok));
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

<p class="visually-hidden" role="status">{announcement}</p>
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
          {#if field.type === 'array' && Array.isArray(field.vars) && field.vars.length && items(field)}
            <span class="label" id="{id}-label">{field.title ?? field.name}</span>
          {:else}
            <label for={id}>{field.title ?? field.name}</label>
          {/if}
          {#if field.type === 'multiline'}
            <textarea {id} placeholder={field.placeholder} value={String(value(field))} oninput={(e) => set(field, e.currentTarget.value)}></textarea>
          {:else if field.type === 'number'}
            <input {id} type="number" placeholder={field.placeholder ?? (field.default !== undefined ? String(field.default) : undefined)} value={stored(field) ?? ''} aria-invalid={invalid[field.name] ? 'true' : undefined} aria-describedby={invalid[field.name] ? `${id}-error` : undefined} oninput={(e) => setNumber(field, e.currentTarget)} />
            {#if invalid[field.name]}<span class="error-text" id="{id}-error">{t('plugins.invalidNumber')}</span>{/if}
          {:else if field.type === 'password'}
            <input {id} type="password" autocomplete="new-password" value={String(value(field))} oninput={(e) => set(field, e.currentTarget.value)} />
          {:else if field.type === 'select'}
            <select {id} value={String(value(field))} onchange={(e) => set(field, Number(e.currentTarget.value))}>
              {#if value(field) === ''}<option value="" disabled>{t('plugins.noChannel')}</option>{/if}
              {#each field.options ?? [] as option, index (index)}
                <option value={String(index)}>{option}</option>
              {/each}
            </select>
          {:else if field.type === 'strings'}
            <textarea {id} aria-describedby="{id}-hint" value={lines(value(field))} oninput={(e) => set(field, e.currentTarget.value.split('\n').filter((l) => l.trim()))}></textarea>
            <span class="hint" id="{id}-hint">{t('plugins.onePerLine')}</span>
          {:else if field.type === 'array' && Array.isArray(field.vars) && field.vars.length && items(field)}
            <div class="items" role="group" aria-labelledby="{id}-label" bind:this={itemsRoot[field.name]}>
              {#each items(field) ?? [] as item, index (ids(field, (items(field) ?? []).length)[index] ?? index)}
                {@const key = ids(field, (items(field) ?? []).length)[index] ?? -1}
                {@const about = summary(field, item)}
                <fieldset class="item" class:off={inactive(field, item)}>
                  <legend class="visually-hidden">{t('plugins.item', { n: index + 1 })}{about ? ` · ${about}` : ''}</legend>
                  <div class="item-head" aria-hidden="true">
                    <span>{t('plugins.item', { n: index + 1 })}{#if about}<span class="muted about">· {about}</span>{/if}</span>
                  </div>
                  <button class="icon-btn small remove" type="button" onclick={() => removeItem(field, index)} aria-label={t('plugins.removeItem', { n: index + 1 })} title={t('plugins.removeItem', { n: index + 1 })}>
                    <Icon name="trash" size={16} />
                  </button>
                  <PluginConfigForm
                    fields={field.vars ?? []}
                    values={item}
                    onvalidity={(ok) => setItemValid(field, key, ok)}
                    idPrefix="{id}-{key}"
                    {channels}
                    onchange={(v) => setItem(field, index, v)}
                  />
                </fieldset>
              {:else}
                <p class="muted small">{t('plugins.noItems')}</p>
              {/each}
              <div>
                <button class="btn add" type="button" onclick={() => addItem(field)}>
                  <Icon name="plus" size={16} />{t('plugins.addItem')}
                </button>
              </div>
            </div>
          {:else if field.type === 'channel' && tree.length}
            <select {id} value={String(value(field) ?? '')} onchange={(e) => set(field, e.currentTarget.value === '' ? undefined : e.currentTarget.value)}>
              {#if field.default === undefined}<option value="">{t('plugins.noChannel')}</option>{/if}
              {#if unknownChannel(field)}
                <option value={unknownChannel(field)}>{t('plugins.unknownChannel', { id: unknownChannel(field) ?? '' })}</option>
              {/if}
              {#each tree as channel (channel.id)}
                <option value={channel.id}>{indent(channel.depth)}{channel.name}</option>
              {/each}
            </select>
          {:else if field.type === 'array'}
            <textarea {id} aria-describedby="{id}-hint{invalid[field.name] ? ` ${id}-error` : ''}" aria-invalid={invalid[field.name] ? 'true' : undefined} value={drafts[field.name] ?? json(value(field))} oninput={(e) => setJson(field, e.currentTarget.value)} onblur={() => { if (!invalid[field.name]) { const { [field.name]: _, ...rest } = drafts; drafts = rest; } }}></textarea>
            <span class="hint" id="{id}-hint">
              {t('plugins.arrayHint', { fields: (field.vars ?? []).map((v) => v.name).join(', ') })}
            </span>
            {#if invalid[field.name]}<span class="error-text" id="{id}-error">{t('plugins.invalidJson')}</span>{/if}
          {:else}
            <input {id} type="text" placeholder={field.placeholder} value={String(value(field))} oninput={(e) => set(field, e.currentTarget.value)} />
            {#if field.type === 'channel'}<span class="hint">{t('plugins.channelOffline')}</span>{/if}
            {#if field.type === 'track' || field.type === 'tracks'}<span class="hint">{t('plugins.trackHint')}</span>{/if}
          {/if}
        {/if}
      </div>
    {/each}
  </div>
{/if}

<style>
  .items {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }
  .item {
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: var(--space-3);
    margin: 0;
    min-width: 0;
  }
  .item {
    position: relative;
  }
  .item.off {
    border-style: dashed;
  }
  .item-head {
    font-weight: 600;
    font-size: var(--text-sm);
    margin: 0 var(--space-6) var(--space-3) 0;
    overflow-wrap: anywhere;
  }
  .about {
    margin-left: 0.3em;
  }
  .remove {
    position: absolute;
    top: var(--space-2);
    right: var(--space-2);
  }
  .fields {
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
  }
</style>
