import type { ThemeName, AccentColor, UiFont, MonoFont } from './ThemeContextDef';
import { uiFontStacks, monoFontStacks } from './ThemeContextDef';

export const STORAGE_KEY = 'zeroclaw-theme';

export interface StoredTheme {
  theme: ThemeName;
  accent: AccentColor;
  uiFont: UiFont;
  monoFont: MonoFont;
  uiFontSize: number;
  monoFontSize: number;
}

const DEFAULTS: StoredTheme = {
  theme: 'dark',
  accent: 'cyan',
  uiFont: 'system',
  monoFont: 'jetbrains',
  uiFontSize: 15,
  monoFontSize: 14,
};

const validThemes: ThemeName[] = ['dark', 'light', 'oled', 'system'];
const validAccents: AccentColor[] = ['cyan', 'violet', 'emerald', 'amber', 'rose', 'blue'];

export function loadStored(): StoredTheme {
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
      if (themeValid && accentValid) {
        return { theme: parsed.theme, accent: parsed.accent, uiFont, monoFont, uiFontSize, monoFontSize };
      }
    }
  } catch { /* ignore */ }
  return DEFAULTS;
}
