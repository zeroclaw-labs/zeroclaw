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

function normalizeConfigPath(path: string): string {
  return path.replace(/_/g, '-');
}

function humanizeConfigLeaf(path: string): string {
  const leaf = path.split('.').pop() ?? path;
  return leaf.replace(/[-_]/g, ' ');
}

function configPathCandidateKeys(namespace: string, path: string): string[] {
  const normalized = normalizeConfigPath(path);
  const segments = normalized.split('.');
  const paths = [normalized];
  const add = (candidate: string) => {
    if (!paths.includes(candidate)) paths.push(candidate);
  };

  if (path !== normalized) {
    add(path);
  }

  for (let i = 0; i < segments.length; i += 1) {
    const wildcard = [...segments];
    wildcard[i] = '*';
    add(wildcard.join('.'));
  }

  // Map keys can contain dots (for example custom provider URLs). Prefer
  // stable wildcard keys for known map-shaped config areas so descriptions
  // do not need one translation per user-supplied key.
  const leaf = segments[segments.length - 1];
  if (leaf && normalized.startsWith('providers.models.')) {
    add(`providers.models.*.${leaf}`);
  }
  if (leaf && normalized.startsWith('channels.')) {
    add(`channels.*.${leaf}`);
  }
  if (leaf && normalized.startsWith('tunnel.') && segments.length > 2) {
    add(`tunnel.*.${leaf}`);
  }

  return paths.map((candidate) => `config.${namespace}.${candidate}`);
}

export function tWithFallback(key: string, fallback: string): string {
  return translations[currentLocale]?.[key] ?? translations.en[key] ?? fallback;
}

export function tConfigDescription(path: string, fallback: string | null | undefined): string | null {
  for (const key of configPathCandidateKeys('description', path)) {
    const translated = translations[currentLocale]?.[key] ?? translations.en[key];
    if (translated) return translated;
  }
  return fallback ?? null;
}

export function tConfigLabel(path: string, fallback?: string | null): string {
  for (const key of configPathCandidateKeys('label', path)) {
    const translated = translations[currentLocale]?.[key] ?? translations.en[key];
    if (translated) return translated;
  }
  const leaf = normalizeConfigPath(path).split('.').pop();
  if (leaf) {
    const leafKey = `config.label.leaf.${leaf}`;
    const translated = translations[currentLocale]?.[leafKey] ?? translations.en[leafKey];
    if (translated) return translated;
  }
  return fallback || humanizeConfigLeaf(path);
}

export function tConfigPlaceholder(path: string, fallback: string): string {
  for (const key of configPathCandidateKeys('placeholder', path)) {
    const translated = translations[currentLocale]?.[key] ?? translations.en[key];
    if (translated) return translated;
  }
  return fallback;
}

export function tConfigSectionLabel(sectionKey: string, fallback: string): string {
  return tWithFallback(`config.section.${sectionKey}.label`, fallback);
}

export function tConfigGroupLabel(groupName: string): string {
  return tWithFallback(`config.group.${groupName.toLowerCase().replace(/\s+/g, '-')}`, groupName);
}

export function tConfigPickerItemLabel(sectionKey: string, itemKey: string, fallback: string): string {
  const translated = translations[currentLocale]?.[`config.picker.${sectionKey}.${itemKey}.label`]
    ?? translations.en[`config.picker.${sectionKey}.${itemKey}.label`];
  return translated ?? tConfigLabel(`${sectionKey}.${itemKey}`, fallback);
}

export function tConfigPickerItemDescription(
  sectionKey: string,
  itemKey: string,
  fallback: string | null | undefined,
): string | null {
  const translated = translations[currentLocale]?.[`config.picker.${sectionKey}.${itemKey}.description`]
    ?? translations.en[`config.picker.${sectionKey}.${itemKey}.description`];
  return translated ?? tConfigDescription(`${sectionKey}.${itemKey}`, fallback);
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
