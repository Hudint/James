// Minimal type-checked i18n: `t('player.next')`, `t('dashboard.queued', { count: 3 })`.

import en, { type Messages } from './en';
import de from './de';

export const locales = { en, de } as const;
export type Locale = keyof typeof locales;
export const localeNames: Record<Locale, string> = { en: 'English', de: 'Deutsch' };

type Paths<T, P extends string = ''> = {
  [K in keyof T & string]: T[K] extends string ? `${P}${K}` : Paths<T[K], `${P}${K}.`>;
}[keyof T & string];
export type MessageKey = Paths<Messages>;

const STORAGE_KEY = 'james.locale';

function initialLocale(): Locale {
  try {
    const stored = localStorage.getItem(STORAGE_KEY);
    if (stored && stored in locales) return stored as Locale;
  } catch {
    // storage unavailable
  }
  return navigator.language.toLowerCase().startsWith('de') ? 'de' : 'en';
}

export const i18n = $state({ locale: initialLocale() });

export function setLocale(locale: Locale) {
  i18n.locale = locale;
  document.documentElement.lang = locale;
  try {
    localStorage.setItem(STORAGE_KEY, locale);
  } catch {
    // ignore
  }
}

function lookup(messages: unknown, key: string): string | undefined {
  let node = messages;
  for (const part of key.split('.')) {
    if (typeof node !== 'object' || node === null) return undefined;
    node = (node as Record<string, unknown>)[part];
  }
  return typeof node === 'string' ? node : undefined;
}

const plurals = new Map<string, Intl.PluralRules>();

/** Picks `key_one` for singular counts (if defined), otherwise `key`. */
function pluralKey(key: string, count: unknown): string {
  if (typeof count !== 'number') return key;
  let rules = plurals.get(i18n.locale);
  if (!rules) {
    rules = new Intl.PluralRules(i18n.locale);
    plurals.set(i18n.locale, rules);
  }
  return rules.select(count) === 'one' ? `${key}_one` : key;
}

export function t(key: MessageKey, params?: Record<string, string | number>): string {
  const plural = pluralKey(key, params?.count);
  const text =
    lookup(locales[i18n.locale], plural) ?? lookup(locales[i18n.locale], key) ?? lookup(en, key) ?? key;
  if (!params) return text;
  return text.replace(/\{(\w+)\}/g, (_, name) => {
    const value = params[name];
    return value === undefined ? `{${name}}` : typeof value === 'number' ? formatNumber(value) : value;
  });
}

/** Locale-aware number formatting. */
export function formatNumber(value: number): string {
  return new Intl.NumberFormat(i18n.locale).format(value);
}
