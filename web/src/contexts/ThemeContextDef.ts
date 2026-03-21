import { createContext } from 'react';

export type ThemeName = 'system' | 'dark' | 'light' | 'oled';
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
  theme: ThemeName;
  accent: AccentColor;
  uiFont: UiFont;
  monoFont: MonoFont;
  uiFontSize: number;
  monoFontSize: number;
  resolvedTheme: 'dark' | 'light' | 'oled';
  setTheme: (t: ThemeName) => void;
  setAccent: (a: AccentColor) => void;
  setUiFont: (f: UiFont) => void;
  setMonoFont: (f: MonoFont) => void;
  setUiFontSize: (size: number) => void;
  setMonoFontSize: (size: number) => void;
}

export const ThemeContext = createContext<ThemeContextValue>({
  theme: 'dark',
  accent: 'cyan',
  uiFont: 'system',
  monoFont: 'jetbrains',
  uiFontSize: 15,
  monoFontSize: 14,
  resolvedTheme: 'dark',
  setTheme: () => {},
  setAccent: () => {},
  setUiFont: () => {},
  setMonoFont: () => {},
  setUiFontSize: () => {},
  setMonoFontSize: () => {},
});
