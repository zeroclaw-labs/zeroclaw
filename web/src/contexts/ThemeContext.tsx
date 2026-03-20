import { useState, useEffect, useCallback, type ReactNode } from 'react';
import { ThemeContext, type ThemeContextValue } from './ThemeContextDef';
import { loadStored, STORAGE_KEY } from './themeStorage';
import type { ThemeName, AccentColor, UiFont, MonoFont } from './ThemeContextDef';
import { uiFontStacks, monoFontStacks } from './ThemeContextDef';

type ConcreteTheme = 'dark' | 'light' | 'oled';

const themes: Record<ConcreteTheme, Record<string, string>> = {
  dark: {
    '--pc-bg-base': '#1e1e24',
    '--color-scheme': 'dark',
    '--pc-bg-surface': '#232329',
    '--pc-bg-elevated': '#27272a',
    '--pc-bg-input': '#1a1a20',
    '--pc-bg-sidebar': 'rgba(30,30,36,0.95)',
    '--pc-bg-code': '#1a1a20',
    '--pc-border': 'rgba(255,255,255,0.08)',
    '--pc-border-strong': 'rgba(255,255,255,0.1)',
    '--pc-text-primary': '#d4d4d8',
    '--pc-text-secondary': '#a1a1aa',
    '--pc-text-muted': '#71717a',
    '--pc-text-faint': '#52525b',
    '--pc-scrollbar-thumb': '#52525b',
    '--pc-scrollbar-track': '#27272a',
    '--pc-scrollbar-thumb-hover': '#71717a',
    '--pc-hover': 'rgba(255,255,255,0.05)',
    '--pc-hover-strong': 'rgba(255,255,255,0.08)',
    '--pc-separator': 'rgba(255,255,255,0.05)',
  },
  light: {
    '--pc-bg-base': '#f4f4f5',
    '--color-scheme': 'light',
    '--pc-bg-surface': '#ffffff',
    '--pc-bg-elevated': '#e4e4e7',
    '--pc-bg-input': '#ffffff',
    '--pc-bg-sidebar': 'rgba(255,255,255,0.95)',
    '--pc-bg-code': '#f4f4f5',
    '--pc-border': 'rgba(0,0,0,0.08)',
    '--pc-border-strong': 'rgba(0,0,0,0.12)',
    '--pc-text-primary': '#18181b',
    '--pc-text-secondary': '#3f3f46',
    '--pc-text-muted': '#71717a',
    '--pc-text-faint': '#a1a1aa',
    '--pc-scrollbar-thumb': '#a1a1aa',
    '--pc-scrollbar-track': '#e4e4e7',
    '--pc-scrollbar-thumb-hover': '#71717a',
    '--pc-hover': 'rgba(0,0,0,0.05)',
    '--pc-hover-strong': 'rgba(0,0,0,0.08)',
    '--pc-separator': 'rgba(0,0,0,0.08)',
  },
  oled: {
    '--pc-bg-base': '#000000',
    '--color-scheme': 'dark',
    '--pc-bg-surface': '#0a0a0a',
    '--pc-bg-elevated': '#141414',
    '--pc-bg-input': '#0a0a0a',
    '--pc-bg-sidebar': 'rgba(0,0,0,0.95)',
    '--pc-bg-code': '#0a0a0a',
    '--pc-border': 'rgba(255,255,255,0.06)',
    '--pc-border-strong': 'rgba(255,255,255,0.08)',
    '--pc-text-primary': '#d4d4d8',
    '--pc-text-secondary': '#a1a1aa',
    '--pc-text-muted': '#71717a',
    '--pc-text-faint': '#3f3f46',
    '--pc-scrollbar-thumb': '#3f3f46',
    '--pc-scrollbar-track': '#0a0a0a',
    '--pc-scrollbar-thumb-hover': '#52525b',
    '--pc-hover': 'rgba(255,255,255,0.04)',
    '--pc-hover-strong': 'rgba(255,255,255,0.06)',
    '--pc-separator': 'rgba(255,255,255,0.04)',
  },
};

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

function resolveTheme(name: ThemeName): 'dark' | 'light' | 'oled' {
  if (name === 'system') {
    return window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
  }
  return name;
}

interface ThemeSettings {
  theme: ThemeName;
  accent: AccentColor;
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
  const [theme, setThemeState] = useState<ThemeName>(stored.theme);
  const [accent, setAccentState] = useState<AccentColor>(stored.accent);
  const [uiFont, setUiFontState] = useState<UiFont>(stored.uiFont);
  const [monoFont, setMonoFontState] = useState<MonoFont>(stored.monoFont);
  const [uiFontSize, setUiFontSizeState] = useState<number>(stored.uiFontSize);
  const [monoFontSize, setMonoFontSizeState] = useState<number>(stored.monoFontSize);

  const persist = useCallback((s: ThemeSettings) => {
    localStorage.setItem(STORAGE_KEY, JSON.stringify({
      theme: s.theme,
      accent: s.accent,
      uiFont: s.uiFont,
      monoFont: s.monoFont,
      uiFontSize: s.uiFontSize,
      monoFontSize: s.monoFontSize,
    }));
  }, []);

  const applyAll = useCallback((s: ThemeSettings) => {
    applyVars({
      ...themes[resolveTheme(s.theme)],
      ...accents[s.accent],
      ...fontVars(s.uiFont, s.monoFont, s.uiFontSize, s.monoFontSize),
    });
  }, []);

  const setTheme = useCallback((t: ThemeName) => {
    setThemeState(t);
    const next: ThemeSettings = { theme: t, accent, uiFont, monoFont, uiFontSize, monoFontSize };
    applyAll(next);
    persist(next);
  }, [accent, applyAll, persist, uiFont, monoFont, uiFontSize, monoFontSize]);

  const setAccent = useCallback((a: AccentColor) => {
    setAccentState(a);
    const next: ThemeSettings = { theme, accent: a, uiFont, monoFont, uiFontSize, monoFontSize };
    applyAll(next);
    persist(next);
  }, [theme, applyAll, persist, uiFont, monoFont, uiFontSize, monoFontSize]);

  const setUiFont = useCallback((f: UiFont) => {
    setUiFontState(f);
    const next: ThemeSettings = { theme, accent, uiFont: f, monoFont, uiFontSize, monoFontSize };
    applyAll(next);
    persist(next);
  }, [theme, accent, applyAll, persist, monoFont, uiFontSize, monoFontSize]);

  const setMonoFont = useCallback((f: MonoFont) => {
    setMonoFontState(f);
    const next: ThemeSettings = { theme, accent, uiFont, monoFont: f, uiFontSize, monoFontSize };
    applyAll(next);
    persist(next);
  }, [theme, accent, applyAll, persist, uiFont, uiFontSize, monoFontSize]);

  const setUiFontSize = useCallback((size: number) => {
    const clamped = Math.min(20, Math.max(12, size));
    setUiFontSizeState(clamped);
    const next: ThemeSettings = { theme, accent, uiFont, monoFont, uiFontSize: clamped, monoFontSize };
    applyAll(next);
    persist(next);
  }, [theme, accent, applyAll, persist, uiFont, monoFont, monoFontSize]);

  const setMonoFontSize = useCallback((size: number) => {
    const clamped = Math.min(20, Math.max(12, size));
    setMonoFontSizeState(clamped);
    const next: ThemeSettings = { theme, accent, uiFont, monoFont, uiFontSize, monoFontSize: clamped };
    applyAll(next);
    persist(next);
  }, [theme, accent, applyAll, persist, uiFont, monoFont, uiFontSize]);

  useEffect(() => {
    applyAll({ theme, accent, uiFont, monoFont, uiFontSize, monoFontSize });
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    if (theme !== 'system') return;
    const mq = window.matchMedia('(prefers-color-scheme: light)');
    const handler = () => applyAll({ theme: mq.matches ? 'light' : 'dark', accent, uiFont, monoFont, uiFontSize, monoFontSize });
    mq.addEventListener('change', handler);
    return () => mq.removeEventListener('change', handler);
  }, [theme, accent, applyAll, uiFont, monoFont, uiFontSize, monoFontSize]);

  const resolvedTheme = resolveTheme(theme);

  const value: ThemeContextValue = {
    theme, accent, uiFont, monoFont, uiFontSize, monoFontSize,
    resolvedTheme, setTheme, setAccent, setUiFont, setMonoFont, setUiFontSize, setMonoFontSize,
  };

  return <ThemeContext.Provider value={value}>{children}</ThemeContext.Provider>;
}
