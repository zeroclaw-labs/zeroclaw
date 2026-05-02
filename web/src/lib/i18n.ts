import { useState, useEffect } from 'react';
import { getStatus } from './api';
import type { Locale } from './i18n/types';
import { translations } from './i18n/translations';

export type { Locale } from './i18n/types';

// ---------------------------------------------------------------------------
// Current locale state
// ---------------------------------------------------------------------------

let currentLocale: Locale = 'en';

export function getLocale(): Locale {
  return currentLocale;
}

export function setLocale(locale: Locale): void {
  currentLocale = locale;
}

// ---------------------------------------------------------------------------
// Translation function
// ---------------------------------------------------------------------------

/**
 * Translate a key using the current locale. Returns the key itself if no
 * translation is found.
 */
export function t(key: string): string {
  return translations[currentLocale]?.[key] ?? translations.en[key] ?? key;
}

/**
 * Get the translation for a specific locale. Falls back to English, then to the
 * raw key.
 */
export function tLocale(key: string, locale: Locale): string {
  return translations[locale]?.[key] ?? translations.en[key] ?? key;
}

function configDescriptionCandidateKeys(path: string): string[] {
  const segments = path.split('.');
  const paths = [path];
  const add = (candidate: string) => {
    if (!paths.includes(candidate)) paths.push(candidate);
  };

  for (let i = 0; i < segments.length; i += 1) {
    const wildcard = [...segments];
    wildcard[i] = '*';
    add(wildcard.join('.'));
  }

  // Map keys can contain dots (for example custom provider URLs). Prefer
  // stable wildcard keys for known map-shaped config areas so descriptions
  // do not need one translation per user-supplied key.
  const leaf = segments[segments.length - 1];
  if (leaf && path.startsWith('providers.models.')) {
    add(`providers.models.*.${leaf}`);
  }
  if (leaf && path.startsWith('channels.')) {
    add(`channels.*.${leaf}`);
  }
  if (leaf && path.startsWith('tunnel.') && segments.length > 2) {
    add(`tunnel.*.${leaf}`);
  }

  return paths.map((candidate) => `config.description.${candidate}`);
}

export function tWithFallback(key: string, fallback: string): string {
  return translations[currentLocale]?.[key] ?? translations.en[key] ?? fallback;
}

export function tConfigDescription(path: string, fallback: string | null | undefined): string | null {
  if (!fallback) return null;
  for (const key of configDescriptionCandidateKeys(path)) {
    const translated = translations[currentLocale]?.[key] ?? translations.en[key];
    if (translated) return translated;
  }
  return fallback;
}

// ---------------------------------------------------------------------------
// Locale metadata
// ---------------------------------------------------------------------------

export { SUPPORTED_LOCALES } from './i18n/supportedLocales';

// ---------------------------------------------------------------------------
// React hook
// ---------------------------------------------------------------------------

/**
 * React hook that fetches the locale from /api/status on mount and keeps the
 * i18n module in sync. Returns the current locale and a `t` helper bound to it.
 */
export function useLocale(): { locale: Locale; t: (key: string) => string } {
  const [locale, setLocaleState] = useState<Locale>(currentLocale);

  useEffect(() => {
    let cancelled = false;

    getStatus()
      .then((status) => {
        if (cancelled) return;
        const raw = (status.locale || 'en').toLowerCase().replace(/-.*/, '').replace(/_.*/, '');
        const detected: Locale = (raw in translations) ? (raw as Locale) : 'en';
        setLocale(detected);
        setLocaleState(detected);
      })
      .catch(() => {
        // Keep default locale on error
      });

    return () => {
      cancelled = true;
    };
  }, []);

  return {
    locale,
    t: (key: string) => tLocale(key, locale),
  };
}
