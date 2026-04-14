import { createContext, useState, useEffect, useCallback, type ReactNode } from 'react';
import { colorThemeMap, DEFAULT_DARK_THEME, DEFAULT_LIGHT_THEME, type ColorThemeId } from './colorThemes';

// ── Types (was ThemeContextDef.ts) ───────────────────────────────────────────

export type ThemeMode = 'system' | 'dark' | 'light' | 'oled';
export type AccentColor = 'cyan' | 'violet' | 'emerald' | 'amber' | 'rose' | 'blue';
export type UiFont = 'system' | 'inter' | 'segoe' | 'sf';
export type MonoFont = 'jetbrains' | 'fira' | 'cascadia' | 'system-mono';

export const uiFontStacks: Record<UiFont, string> = {
  system: 'system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif',
  inter: '"Inter", system-ui, sans-serif',
  segoe: '"Segoe UI", system-ui, sans-serif',
  sf: '-apple-system, BlinkMacSystemFont, "SF Pro Text", sans-serif',
};

export const monoFontStacks: Record<MonoFont, string> = {
  jetbrains: '"JetBrains Mono", "Fira Code", "Cascadia Code", monospace',
  fira: '"Fira Code", "JetBrains Mono", "Cascadia Code", monospace',
  cascadia: '"Cascadia Code", "JetBrains Mono", "Fira Code", monospace',
  'system-mono': 'ui-monospace, "SF Mono", "Cascadia Code", "Fira Code", monospace',
};

export interface ThemeContextValue {
  theme: ThemeMode;
  accent: AccentColor;
  colorTheme: ColorThemeId;
  uiFont: UiFont;
  monoFont: MonoFont;
  uiFontSize: number;
  monoFontSize: number;
  resolvedTheme: 'dark' | 'light' | 'oled';
  setTheme: (t: ThemeMode) => void;
  setAccent: (a: AccentColor) => void;
  setColorTheme: (c: ColorThemeId) => void;
  setUiFont: (f: UiFont) => void;
  setMonoFont: (f: MonoFont) => void;
  setUiFontSize: (size: number) => void;
  setMonoFontSize: (size: number) => void;
}

export const ThemeContext = createContext<ThemeContextValue>({
  theme: 'dark',
  accent: 'cyan',
  colorTheme: 'default-dark',
  uiFont: 'system',
  monoFont: 'jetbrains',
  uiFontSize: 15,
  monoFontSize: 14,
  resolvedTheme: 'dark',
  setTheme: () => {},
  setAccent: () => {},
  setColorTheme: () => {},
  setUiFont: () => {},
  setMonoFont: () => {},
  setUiFontSize: () => {},
  setMonoFontSize: () => {},
});

// ── Font loader (was fontLoader.ts) ──────────────────────────────────────────

const loadedFonts: Set<string> = new Set();

function loadGoogleFont(family: string, weights: string = '400;500;600') {
  const id = `gfont-${family.replace(/\s+/g, '-').toLowerCase()}`;
  if (loadedFonts.has(id)) return;
  loadedFonts.add(id);
  const link = document.createElement('link');
  link.id = id;
  link.rel = 'stylesheet';
  link.href = `https://fonts.googleapis.com/css2?family=${encodeURIComponent(family)}:wght@${weights}&display=swap`;
  document.head.appendChild(link);
}

function loadUiFont(font: string) {
  if (font === 'inter') loadGoogleFont('Inter');
  if (font === 'segoe') loadGoogleFont('Segoe UI');
  if (font === 'sf') loadGoogleFont('SF Pro Text');
}

function loadMonoFont(font: string) {
  if (font === 'jetbrains') loadGoogleFont('JetBrains Mono');
  if (font === 'fira') loadGoogleFont('Fira Code');
  if (font === 'cascadia') loadGoogleFont('Cascadia Code');
}

// ── Locale storage (was localeStorage.ts) ────────────────────────────────────

export const LOCALE_STORAGE_KEY = 'zeroclaw-locale';

export function loadLocale(): string {
  return localStorage.getItem(LOCALE_STORAGE_KEY) ?? 'en';
}

export function saveLocale(locale: string) {
  localStorage.setItem(LOCALE_STORAGE_KEY, locale);
}

// ── Theme storage (was themeStorage.ts) ──────────────────────────────────────

const STORAGE_KEY = 'zeroclaw-theme';

interface StoredTheme {
  theme: ThemeMode;
  accent: AccentColor;
  colorTheme: ColorThemeId;
  uiFont: UiFont;
  monoFont: MonoFont;
  uiFontSize: number;
  monoFontSize: number;
}

const DEFAULTS: StoredTheme = {
  theme: 'dark',
  accent: 'cyan',
  colorTheme: 'default-dark',
  uiFont: 'system',
  monoFont: 'jetbrains',
  uiFontSize: 15,
  monoFontSize: 14,
};

const validThemes: ThemeMode[] = ['dark', 'light', 'oled', 'system'];
const validAccents: AccentColor[] = ['cyan', 'violet', 'emerald', 'amber', 'rose', 'blue'];

function migrateThemeToColorTheme(themeMode: ThemeMode): ColorThemeId {
  switch (themeMode) {
    case 'light': return 'default-light';
    case 'oled': return 'oled-black';
    default: return 'default-dark';
  }
}

function loadStored(): StoredTheme {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) {
      const parsed = JSON.parse(raw);
      const themeValid = validThemes.includes(parsed.theme);
      const accentValid = validAccents.includes(parsed.accent);
      const uiFont: UiFont = uiFontStacks[parsed.uiFont as UiFont] ? parsed.uiFont as UiFont : DEFAULTS.uiFont;
      const monoFont: MonoFont = monoFontStacks[parsed.monoFont as MonoFont] ? parsed.monoFont as MonoFont : DEFAULTS.monoFont;
      const uiFontSize = Number.isFinite(parsed.uiFontSize) ? Math.min(20, Math.max(12, Number(parsed.uiFontSize))) : DEFAULTS.uiFontSize;
      const monoFontSize = Number.isFinite(parsed.monoFontSize) ? Math.min(20, Math.max(12, Number(parsed.monoFontSize))) : DEFAULTS.monoFontSize;

      let colorTheme: ColorThemeId = DEFAULTS.colorTheme;
      if (parsed.colorTheme && colorThemeMap[parsed.colorTheme as ColorThemeId]) {
        colorTheme = parsed.colorTheme as ColorThemeId;
      } else if (themeValid) {
        colorTheme = migrateThemeToColorTheme(parsed.theme);
      }

      if (themeValid && accentValid) {
        return { theme: parsed.theme, accent: parsed.accent, colorTheme, uiFont, monoFont, uiFontSize, monoFontSize };
      }
    }
  } catch { /* ignore corrupt storage */ }
  return DEFAULTS;
}

// ── Provider ─────────────────────────────────────────────────────────────────

const accents: Record<AccentColor, Record<string, string>> = {
  cyan: {
    '--pc-accent': '#22d3ee', '--pc-accent-light': '#67e8f9',
    '--pc-accent-dim': 'rgba(34,211,238,0.3)', '--pc-accent-glow': 'rgba(34,211,238,0.1)', '--pc-accent-rgb': '34,211,238',
  },
  violet: {
    '--pc-accent': '#8b5cf6', '--pc-accent-light': '#a78bfa',
    '--pc-accent-dim': 'rgba(139,92,246,0.3)', '--pc-accent-glow': 'rgba(139,92,246,0.1)', '--pc-accent-rgb': '139,92,246',
  },
  emerald: {
    '--pc-accent': '#10b981', '--pc-accent-light': '#34d399',
    '--pc-accent-dim': 'rgba(16,185,129,0.3)', '--pc-accent-glow': 'rgba(16,185,129,0.1)', '--pc-accent-rgb': '16,185,129',
  },
  amber: {
    '--pc-accent': '#f59e0b', '--pc-accent-light': '#fbbf24',
    '--pc-accent-dim': 'rgba(245,158,11,0.3)', '--pc-accent-glow': 'rgba(245,158,11,0.1)', '--pc-accent-rgb': '245,158,11',
  },
  rose: {
    '--pc-accent': '#f43f5e', '--pc-accent-light': '#fb7185',
    '--pc-accent-dim': 'rgba(244,63,94,0.3)', '--pc-accent-glow': 'rgba(244,63,94,0.1)', '--pc-accent-rgb': '244,63,94',
  },
  blue: {
    '--pc-accent': '#3b82f6', '--pc-accent-light': '#60a5fa',
    '--pc-accent-dim': 'rgba(59,130,246,0.3)', '--pc-accent-glow': 'rgba(59,130,246,0.1)', '--pc-accent-rgb': '59,130,246',
  },
};

function applyVars(vars: Record<string, string>) {
  const root = document.documentElement;
  for (const [k, v] of Object.entries(vars)) {
    if (k === '--color-scheme') {
      root.style.colorScheme = v as 'light' | 'dark';
    } else {
      root.style.setProperty(k, v);
    }
  }
}

function resolveColorTheme(mode: ThemeMode, colorTheme: ColorThemeId): ColorThemeId {
  if (mode === 'system') {
    const preferLight = window.matchMedia('(prefers-color-scheme: light)').matches;
    const ct = colorThemeMap[colorTheme];
    if (ct && ((preferLight && ct.scheme === 'light') || (!preferLight && ct.scheme === 'dark'))) {
      return colorTheme;
    }
    return preferLight ? DEFAULT_LIGHT_THEME : DEFAULT_DARK_THEME;
  }
  if (mode === 'oled') return 'oled-black';
  return colorTheme;
}

function resolveThemeScheme(mode: ThemeMode, colorTheme: ColorThemeId): 'dark' | 'light' | 'oled' {
  if (mode === 'oled') return 'oled';
  const resolved = resolveColorTheme(mode, colorTheme);
  const ct = colorThemeMap[resolved];
  return ct?.scheme ?? 'dark';
}

function fontVars(uiFont: UiFont, monoFont: MonoFont, uiFontSize: number, monoFontSize: number) {
  return {
    '--pc-font-ui': uiFontStacks[uiFont],
    '--pc-font-mono': monoFontStacks[monoFont],
    '--pc-font-size': `${uiFontSize}px`,
    '--pc-font-size-mono': `${monoFontSize}px`,
  };
}

export function ThemeProvider({ children }: { children: ReactNode }) {
  const [stored] = useState(loadStored);
  const [theme, setThemeState] = useState<ThemeMode>(stored.theme);
  const [accent, setAccentState] = useState<AccentColor>(stored.accent);
  const [colorTheme, setColorThemeState] = useState<ColorThemeId>(stored.colorTheme);
  const [uiFont, setUiFontState] = useState<UiFont>(stored.uiFont);
  const [monoFont, setMonoFontState] = useState<MonoFont>(stored.monoFont);
  const [uiFontSize, setUiFontSizeState] = useState<number>(stored.uiFontSize);
  const [monoFontSize, setMonoFontSizeState] = useState<number>(stored.monoFontSize);

  const persist = useCallback((s: StoredTheme) => {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(s));
  }, []);

  const applyAll = useCallback((s: StoredTheme) => {
    const resolvedId = resolveColorTheme(s.theme, s.colorTheme);
    const ct = colorThemeMap[resolvedId];
    const themeVars = ct?.vars ?? colorThemeMap[DEFAULT_DARK_THEME].vars;
    applyVars({
      ...themeVars,
      ...accents[s.accent],
      ...fontVars(s.uiFont, s.monoFont, s.uiFontSize, s.monoFontSize),
    });
  }, []);

  const setTheme = useCallback((t: ThemeMode) => {
    // Auto-select a color theme matching the new scheme so explicit mode choices apply correctly.
    const currentCt = colorThemeMap[colorTheme];
    const targetScheme = t === 'oled' ? 'dark' : t;
    const newColorTheme: ColorThemeId =
      (currentCt && currentCt.scheme === targetScheme) ? colorTheme : (
        t === 'oled' ? 'oled-black' :
        t === 'light' ? DEFAULT_LIGHT_THEME :
        DEFAULT_DARK_THEME
      );
    setThemeState(t);
    setColorThemeState(newColorTheme);
    const next = { theme: t, accent, colorTheme: newColorTheme, uiFont, monoFont, uiFontSize, monoFontSize };
    applyAll(next);
    persist(next);
  }, [accent, colorTheme, uiFont, monoFont, uiFontSize, monoFontSize, applyAll, persist]);

  const setAccent = useCallback((a: AccentColor) => {
    setAccentState(a);
    const next: StoredTheme = { theme, accent: a, colorTheme, uiFont, monoFont, uiFontSize, monoFontSize };
    applyAll(next);
    persist(next);
  }, [theme, colorTheme, uiFont, monoFont, uiFontSize, monoFontSize, applyAll, persist]);

  const setColorTheme = useCallback((c: ColorThemeId) => {
    setColorThemeState(c);
    // Only update the color theme — do NOT override the user's explicit theme mode.
    // setTheme handles syncing colorTheme when the mode changes.
    const next: StoredTheme = { theme, accent, colorTheme: c, uiFont, monoFont, uiFontSize, monoFontSize };
    applyAll(next);
    persist(next);
  }, [theme, accent, uiFont, monoFont, uiFontSize, monoFontSize, applyAll, persist]);

  const setUiFont = useCallback((f: UiFont) => {
    setUiFontState(f);
    loadUiFont(f);
    const next: StoredTheme = { theme, accent, colorTheme, uiFont: f, monoFont, uiFontSize, monoFontSize };
    applyAll(next);
    persist(next);
  }, [theme, accent, colorTheme, applyAll, persist, monoFont, uiFontSize, monoFontSize]);

  const setMonoFont = useCallback((f: MonoFont) => {
    setMonoFontState(f);
    loadMonoFont(f);
    const next: StoredTheme = { theme, accent, colorTheme, uiFont, monoFont: f, uiFontSize, monoFontSize };
    applyAll(next);
    persist(next);
  }, [theme, accent, colorTheme, applyAll, persist, uiFont, uiFontSize, monoFontSize]);

  const setUiFontSize = useCallback((size: number) => {
    const clamped = Math.min(20, Math.max(12, size));
    setUiFontSizeState(clamped);
    const next: StoredTheme = { theme, accent, colorTheme, uiFont, monoFont, uiFontSize: clamped, monoFontSize };
    applyAll(next);
    persist(next);
  }, [theme, accent, colorTheme, applyAll, persist, uiFont, monoFont, monoFontSize]);

  const setMonoFontSize = useCallback((size: number) => {
    const clamped = Math.min(20, Math.max(12, size));
    setMonoFontSizeState(clamped);
    const next: StoredTheme = { theme, accent, colorTheme, uiFont, monoFont, uiFontSize, monoFontSize: clamped };
    applyAll(next);
    persist(next);
  }, [theme, accent, colorTheme, applyAll, persist, uiFont, monoFont, uiFontSize]);

  useEffect(() => {
    applyAll({ theme, accent, colorTheme, uiFont, monoFont, uiFontSize, monoFontSize });
    loadUiFont(uiFont);
    loadMonoFont(monoFont);
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    if (theme !== 'system') return;
    const mq = window.matchMedia('(prefers-color-scheme: light)');
    const handler = () => applyAll({ theme, accent, colorTheme, uiFont, monoFont, uiFontSize, monoFontSize });
    mq.addEventListener('change', handler);
    return () => mq.removeEventListener('change', handler);
  }, [theme, accent, colorTheme, applyAll, uiFont, monoFont, uiFontSize, monoFontSize]);

  const resolvedTheme = resolveThemeScheme(theme, colorTheme);

  const value: ThemeContextValue = {
    theme, accent, colorTheme, uiFont, monoFont, uiFontSize, monoFontSize,
    resolvedTheme, setTheme, setAccent, setColorTheme, setUiFont, setMonoFont, setUiFontSize, setMonoFontSize,
  };

  return <ThemeContext.Provider value={value}>{children}</ThemeContext.Provider>;
}
