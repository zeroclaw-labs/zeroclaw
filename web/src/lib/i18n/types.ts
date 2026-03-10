export type Locale =
  | 'en'
  | 'zh-CN'
  | 'ja'
  | 'ko'
  | 'vi'
  | 'tl'
  | 'es'
  | 'pt'
  | 'it'
  | 'de'
  | 'fr'
  | 'ar'
  | 'hi'
  | 'ru'
  | 'bn'
  | 'he'
  | 'pl'
  | 'cs'
  | 'nl'
  | 'tr'
  | 'uk'
  | 'id'
  | 'th'
  | 'ur'
  | 'ro'
  | 'sv'
  | 'el'
  | 'hu'
  | 'fi'
  | 'da'
  | 'nb';

export type LocaleDirection = 'ltr' | 'rtl';

export interface LanguageOption {
  value: Locale;
  label: string;
  flag: string;
  direction: LocaleDirection;
}

export interface LocaleDocumentTarget {
  documentElement?: { lang?: string; dir?: string };
  body?: { dir?: string } | null;
}
