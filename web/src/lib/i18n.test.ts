import { describe, expect, it } from 'vitest';
import { coerceLocale, LANGUAGE_SWITCH_ORDER } from './i18n';

describe('i18n locale support', () => {
  it('normalizes locale hints for the supported language set', () => {
    expect(coerceLocale('en-US')).toBe('en');
    expect(coerceLocale('zh')).toBe('zh-CN');
    expect(coerceLocale('zh-HK')).toBe('zh-CN');
    expect(coerceLocale('ja-JP')).toBe('ja');
    expect(coerceLocale('ru-RU')).toBe('ru');
    expect(coerceLocale('fr-CA')).toBe('fr');
    expect(coerceLocale('vi-VN')).toBe('vi');
    expect(coerceLocale('el-GR')).toBe('el');
  });

  it('falls back to English for unknown locales', () => {
    expect(coerceLocale('es-ES')).toBe('en');
    expect(coerceLocale('pt-BR')).toBe('en');
    expect(coerceLocale(undefined)).toBe('en');
  });

  it('uses the expected language switch order', () => {
    expect(LANGUAGE_SWITCH_ORDER).toEqual([
      'en',
      'zh-CN',
      'ja',
      'ru',
      'fr',
      'vi',
      'el',
    ]);
  });
});
