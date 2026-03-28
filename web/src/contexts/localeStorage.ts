
export const LOCALE_STORAGE_KEY = 'zeroclaw-locale';

const DEFAULT_LOCALE = 'en';

export function loadLocale(): string {
  const locale = localStorage.getItem(LOCALE_STORAGE_KEY);
  if (locale) {
    return locale;
  } 
  return DEFAULT_LOCALE;
}

export function saveLocale(locale: string) {
  console.log('saveLocale', locale);
  localStorage.setItem(LOCALE_STORAGE_KEY, locale);
}
