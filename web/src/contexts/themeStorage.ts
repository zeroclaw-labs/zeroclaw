import type { AccentColor, UiFont, MonoFont, ThemeMode } from './ThemeContextDef';
import { uiFontStacks, monoFontStacks } from './ThemeContextDef';
import type { ColorThemeId } from './colorThemes';
import { colorThemeMap } from './colorThemes';

export const STORAGE_KEY = 'zeroclaw-theme';

export interface StoredTheme {
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

/** Migrate old theme mode to a color theme id for backward compatibility. */
function migrateThemeToColorTheme(themeMode: ThemeMode): ColorThemeId {
  switch (themeMode) {
    case 'light': return 'default-light';
    case 'oled': return 'oled-black';
    default: return 'default-dark';
  }
}

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

      // Validate or migrate color theme
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
  } catch { /* ignore */ }
  return DEFAULTS;
}
