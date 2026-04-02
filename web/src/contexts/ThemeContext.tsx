import { useState, useEffect, useCallback, type ReactNode } from 'react';
import { ThemeContext, type ThemeContextValue } from './ThemeContextDef';
import { loadStored, STORAGE_KEY } from './themeStorage';
import type { ThemeMode, AccentColor, UiFont, MonoFont } from './ThemeContextDef';
import { uiFontStacks, monoFontStacks } from './ThemeContextDef';
import { loadUiFont, loadMonoFont } from './fontLoader';
import { colorThemeMap, DEFAULT_DARK_THEME, DEFAULT_LIGHT_THEME, type ColorThemeId } from './colorThemes';

/** Accent-only overrides (applied on top of color theme when user picks a custom accent). */
const accents: Record<AccentColor, Record<string, string>> = {
  cyan: {
    '--pc-accent': '#22d3ee',
    '--pc-accent-light': '#67e8f9',
    '--pc-accent-dim': 'rgba(34,211,238,0.3)',
    '--pc-accent-glow': 'rgba(34,211,238,0.1)',
    '--pc-accent-rgb': '34,211,238',
  },
  violet: {
    '--pc-accent': '#8b5cf6',
    '--pc-accent-light': '#a78bfa',
    '--pc-accent-dim': 'rgba(139,92,246,0.3)',
    '--pc-accent-glow': 'rgba(139,92,246,0.1)',
    '--pc-accent-rgb': '139,92,246',
  },
  emerald: {
    '--pc-accent': '#10b981',
    '--pc-accent-light': '#34d399',
    '--pc-accent-dim': 'rgba(16,185,129,0.3)',
    '--pc-accent-glow': 'rgba(16,185,129,0.1)',
    '--pc-accent-rgb': '16,185,129',
  },
  amber: {
    '--pc-accent': '#f59e0b',
    '--pc-accent-light': '#fbbf24',
    '--pc-accent-dim': 'rgba(245,158,11,0.3)',
    '--pc-accent-glow': 'rgba(245,158,11,0.1)',
    '--pc-accent-rgb': '245,158,11',
  },
  rose: {
    '--pc-accent': '#f43f5e',
    '--pc-accent-light': '#fb7185',
    '--pc-accent-dim': 'rgba(244,63,94,0.3)',
    '--pc-accent-glow': 'rgba(244,63,94,0.1)',
    '--pc-accent-rgb': '244,63,94',
  },
  blue: {
    '--pc-accent': '#3b82f6',
    '--pc-accent-light': '#60a5fa',
    '--pc-accent-dim': 'rgba(59,130,246,0.3)',
    '--pc-accent-glow': 'rgba(59,130,246,0.1)',
    '--pc-accent-rgb': '59,130,246',
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

/** Resolve which color theme to use based on the mode. */
function resolveColorTheme(mode: ThemeMode, colorTheme: ColorThemeId): ColorThemeId {
  if (mode === 'system') {
    const preferLight = window.matchMedia('(prefers-color-scheme: light)').matches;
    const ct = colorThemeMap[colorTheme];
    // If the selected theme matches system preference, use it; otherwise pick the right default
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

interface ThemeSettings {
  theme: ThemeMode;
  accent: AccentColor;
  colorTheme: ColorThemeId;
  uiFont: UiFont;
  monoFont: MonoFont;
  uiFontSize: number;
  monoFontSize: number;
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

  const persist = useCallback((s: ThemeSettings) => {
    localStorage.setItem(STORAGE_KEY, JSON.stringify({
      theme: s.theme,
      accent: s.accent,
      colorTheme: s.colorTheme,
      uiFont: s.uiFont,
      monoFont: s.monoFont,
      uiFontSize: s.uiFontSize,
      monoFontSize: s.monoFontSize,
    }));
  }, []);

  const applyAll = useCallback((s: ThemeSettings) => {
    const resolvedId = resolveColorTheme(s.theme, s.colorTheme);
    const ct = colorThemeMap[resolvedId];
    const themeVars = ct?.vars ?? colorThemeMap[DEFAULT_DARK_THEME].vars;
    // Color theme provides base + its own accent. User accent overrides on top.
    applyVars({
      ...themeVars,
      ...accents[s.accent],
      ...fontVars(s.uiFont, s.monoFont, s.uiFontSize, s.monoFontSize),
    });
  }, []);

  const setTheme = useCallback((t: ThemeMode) => {
    setThemeState(t);
    // Sync colorTheme to match the new mode when switching between light/dark/oled
    let newColorTheme = colorTheme;
    if (t !== 'system') {
      const ct = colorThemeMap[colorTheme];
      const modeScheme = t === 'oled' ? 'dark' : t;
      if (t === 'oled') {
        newColorTheme = 'oled-black';
      } else if (!ct || ct.scheme !== modeScheme) {
        newColorTheme = t === 'light' ? DEFAULT_LIGHT_THEME : DEFAULT_DARK_THEME;
      }
      if (newColorTheme !== colorTheme) setColorThemeState(newColorTheme);
    }
    const next: ThemeSettings = { theme: t, accent, colorTheme: newColorTheme, uiFont, monoFont, uiFontSize, monoFontSize };
    applyAll(next);
    persist(next);
  }, [accent, colorTheme, uiFont, monoFont, uiFontSize, monoFontSize, applyAll, persist]);

  const setAccent = useCallback((a: AccentColor) => {
    setAccentState(a);
    const next: ThemeSettings = { theme, accent: a, colorTheme, uiFont, monoFont, uiFontSize, monoFontSize };
    applyAll(next);
    persist(next);
  }, [theme, colorTheme, uiFont, monoFont, uiFontSize, monoFontSize, applyAll, persist]);

  const setColorTheme = useCallback((c: ColorThemeId) => {
    setColorThemeState(c);
    // Auto-adjust theme mode to match the color theme's scheme
    const ct = colorThemeMap[c];
    let newMode = theme;
    if (ct && theme !== 'system') {
      if (c === 'oled-black') {
        newMode = 'oled';
      } else {
        newMode = ct.scheme;
      }
      setThemeState(newMode);
    }
    const next: ThemeSettings = { theme: newMode, accent, colorTheme: c, uiFont, monoFont, uiFontSize, monoFontSize };
    applyAll(next);
    persist(next);
  }, [theme, accent, uiFont, monoFont, uiFontSize, monoFontSize, applyAll, persist]);

  const setUiFont = useCallback((f: UiFont) => {
    setUiFontState(f);
    loadUiFont(f);
    const next: ThemeSettings = { theme, accent, colorTheme, uiFont: f, monoFont, uiFontSize, monoFontSize };
    applyAll(next);
    persist(next);
  }, [theme, accent, colorTheme, applyAll, persist, monoFont, uiFontSize, monoFontSize]);

  const setMonoFont = useCallback((f: MonoFont) => {
    setMonoFontState(f);
    loadMonoFont(f);
    const next: ThemeSettings = { theme, accent, colorTheme, uiFont, monoFont: f, uiFontSize, monoFontSize };
    applyAll(next);
    persist(next);
  }, [theme, accent, colorTheme, applyAll, persist, uiFont, uiFontSize, monoFontSize]);

  const setUiFontSize = useCallback((size: number) => {
    const clamped = Math.min(20, Math.max(12, size));
    setUiFontSizeState(clamped);
    const next: ThemeSettings = { theme, accent, colorTheme, uiFont, monoFont, uiFontSize: clamped, monoFontSize };
    applyAll(next);
    persist(next);
  }, [theme, accent, colorTheme, applyAll, persist, uiFont, monoFont, monoFontSize]);

  const setMonoFontSize = useCallback((size: number) => {
    const clamped = Math.min(20, Math.max(12, size));
    setMonoFontSizeState(clamped);
    const next: ThemeSettings = { theme, accent, colorTheme, uiFont, monoFont, uiFontSize, monoFontSize: clamped };
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
