import type { LanguageOption, Locale, LocaleDirection } from './types';

export const LANGUAGE_OPTIONS: ReadonlyArray<LanguageOption> = [
  { value: 'en', label: 'English', flag: '🇺🇸', direction: 'ltr' },
  { value: 'zh-CN', label: '简体中文', flag: '🇨🇳', direction: 'ltr' },
  { value: 'ja', label: '日本語', flag: '🇯🇵', direction: 'ltr' },
  { value: 'ko', label: '한국어', flag: '🇰🇷', direction: 'ltr' },
  { value: 'vi', label: 'Tiếng Việt', flag: '🇻🇳', direction: 'ltr' },
  { value: 'tl', label: 'Tagalog', flag: '🇵🇭', direction: 'ltr' },
  { value: 'es', label: 'Español', flag: '🇪🇸', direction: 'ltr' },
  { value: 'pt', label: 'Português', flag: '🇵🇹', direction: 'ltr' },
  { value: 'it', label: 'Italiano', flag: '🇮🇹', direction: 'ltr' },
  { value: 'de', label: 'Deutsch', flag: '🇩🇪', direction: 'ltr' },
  { value: 'fr', label: 'Français', flag: '🇫🇷', direction: 'ltr' },
  { value: 'ar', label: 'العربية', flag: '🇸🇦', direction: 'rtl' },
  { value: 'hi', label: 'हिन्दी', flag: '🇮🇳', direction: 'ltr' },
  { value: 'ru', label: 'Русский', flag: '🇷🇺', direction: 'ltr' },
  { value: 'bn', label: 'বাংলা', flag: '🇧🇩', direction: 'ltr' },
  { value: 'he', label: 'עברית', flag: '🇮🇱', direction: 'rtl' },
  { value: 'pl', label: 'Polski', flag: '🇵🇱', direction: 'ltr' },
  { value: 'cs', label: 'Čeština', flag: '🇨🇿', direction: 'ltr' },
  { value: 'nl', label: 'Nederlands', flag: '🇳🇱', direction: 'ltr' },
  { value: 'tr', label: 'Türkçe', flag: '🇹🇷', direction: 'ltr' },
  { value: 'uk', label: 'Українська', flag: '🇺🇦', direction: 'ltr' },
  { value: 'id', label: 'Bahasa Indonesia', flag: '🇮🇩', direction: 'ltr' },
  { value: 'th', label: 'ไทย', flag: '🇹🇭', direction: 'ltr' },
  { value: 'ur', label: 'اردو', flag: '🇵🇰', direction: 'rtl' },
  { value: 'ro', label: 'Română', flag: '🇷🇴', direction: 'ltr' },
  { value: 'sv', label: 'Svenska', flag: '🇸🇪', direction: 'ltr' },
  { value: 'el', label: 'Ελληνικά', flag: '🇬🇷', direction: 'ltr' },
  { value: 'hu', label: 'Magyar', flag: '🇭🇺', direction: 'ltr' },
  { value: 'fi', label: 'Suomi', flag: '🇫🇮', direction: 'ltr' },
  { value: 'da', label: 'Dansk', flag: '🇩🇰', direction: 'ltr' },
  { value: 'nb', label: 'Norsk Bokmål', flag: '🇳🇴', direction: 'ltr' },
];

export const LANGUAGE_SWITCH_ORDER: ReadonlyArray<Locale> =
  LANGUAGE_OPTIONS.map((option) => option.value);

const RTL_LOCALES = new Set<Locale>(['ar', 'he', 'ur']);

export function getLocaleDirection(locale: Locale): LocaleDirection {
  return RTL_LOCALES.has(locale) ? 'rtl' : 'ltr';
}

export function getLanguageOption(locale: Locale): LanguageOption {
  const matched = LANGUAGE_OPTIONS.find((option) => option.value === locale);
  if (matched) {
    return matched;
  }

  const fallback = LANGUAGE_OPTIONS.find((option) => option.value === 'en');
  if (!fallback) {
    throw new Error('English locale metadata is missing.');
  }

  return fallback;
}
