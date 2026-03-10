import type { Locale, LocaleDocumentTarget } from './types';
import { getLocaleDirection } from './languages';
import { translations } from './locales';

const LOCALE_PREFIX_MAP = new Map<string, Locale>([
  ['zh', 'zh-CN'],
  ['ja', 'ja'],
  ['ko', 'ko'],
  ['vi', 'vi'],
  ['tl', 'tl'],
  ['es', 'es'],
  ['pt', 'pt'],
  ['it', 'it'],
  ['de', 'de'],
  ['fr', 'fr'],
  ['ar', 'ar'],
  ['hi', 'hi'],
  ['ru', 'ru'],
  ['bn', 'bn'],
  ['iw', 'he'],
  ['he', 'he'],
  ['pl', 'pl'],
  ['cs', 'cs'],
  ['nl', 'nl'],
  ['tr', 'tr'],
  ['uk', 'uk'],
  ['id', 'id'],
  ['th', 'th'],
  ['ur', 'ur'],
  ['ro', 'ro'],
  ['sv', 'sv'],
  ['el', 'el'],
  ['hu', 'hu'],
  ['fi', 'fi'],
  ['da', 'da'],
  ['nb', 'nb'],
  ['no', 'nb'],
]);

export function coerceLocale(locale: string | undefined): Locale {
  if (!locale) return 'en';
  const prefix = locale.toLowerCase().split(/[-_]/)[0];
  return LOCALE_PREFIX_MAP.get(prefix) ?? 'en';
}

let currentLocale: Locale = 'en';

export function getLocale(): Locale {
  return currentLocale;
}

export function setLocale(locale: Locale): void {
  currentLocale = locale;
}

export function t(key: string): string {
  return translations[currentLocale]?.[key] ?? translations.en[key] ?? key;
}

export function tLocale(key: string, locale: Locale): string {
  return translations[locale]?.[key] ?? translations.en[key] ?? key;
}

export function applyLocaleToDocument(locale: Locale, target: LocaleDocumentTarget): void {
  const direction = getLocaleDirection(locale);

  if (target.documentElement) {
    target.documentElement.lang = locale;
    target.documentElement.dir = direction;
  }

  if (target.body) {
    target.body.dir = direction;
  }
}
