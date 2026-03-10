import { describe, expect, it } from 'vitest';
import {
  applyLocaleToDocument,
  coerceLocale,
  getLanguageOption,
  getLocaleDirection,
  LANGUAGE_OPTIONS,
  LANGUAGE_SWITCH_ORDER,
} from '.';

describe('language metadata', () => {
  it('keeps language options aligned with switch order', () => {
    expect(LANGUAGE_OPTIONS.map((option) => option.value)).toEqual(LANGUAGE_SWITCH_ORDER);
    expect(new Set(LANGUAGE_OPTIONS.map((option) => option.value)).size).toBe(LANGUAGE_OPTIONS.length);
  });

  it('provides a flag-backed label for every locale', () => {
    for (const option of LANGUAGE_OPTIONS) {
      expect(getLanguageOption(option.value)).toEqual(option);
      expect(option.label.length).toBeGreaterThan(0);
      expect(option.flag.length).toBeGreaterThan(0);
    }
  });
});

describe('coerceLocale', () => {
  it('normalizes browser locale variants to supported locales', () => {
    expect(coerceLocale('ar-SA')).toBe('ar');
    expect(coerceLocale('he-IL')).toBe('he');
    expect(coerceLocale('iw-IL')).toBe('he');
    expect(coerceLocale('pt-BR')).toBe('pt');
    expect(coerceLocale('no-NO')).toBe('nb');
    expect(coerceLocale('zh-Hans')).toBe('zh-CN');
    expect(coerceLocale(undefined)).toBe('en');
  });
});

describe('locale direction', () => {
  it('returns rtl only for rtl languages', () => {
    expect(getLocaleDirection('ar')).toBe('rtl');
    expect(getLocaleDirection('he')).toBe('rtl');
    expect(getLocaleDirection('ur')).toBe('rtl');
    expect(getLocaleDirection('en')).toBe('ltr');
    expect(getLocaleDirection('ja')).toBe('ltr');
  });

  it('applies lang and dir to a document-like target', () => {
    const target = {
      documentElement: { lang: '', dir: '' },
      body: { dir: '' },
    };

    applyLocaleToDocument('ar', target);
    expect(target.documentElement.lang).toBe('ar');
    expect(target.documentElement.dir).toBe('rtl');
    expect(target.body.dir).toBe('rtl');

    applyLocaleToDocument('fr', target);
    expect(target.documentElement.lang).toBe('fr');
    expect(target.documentElement.dir).toBe('ltr');
    expect(target.body.dir).toBe('ltr');
  });
});
