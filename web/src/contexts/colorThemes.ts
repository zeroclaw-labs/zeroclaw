import themesData from './themes.json';

export type ColorThemeId =
  | 'operator-dark' | 'operator-light'
  | 'default-dark' | 'default-light' | 'oled-black'
  | 'icy-blue'
  | 'nord-dark' | 'nord-light'
  | 'dracula' | 'monokai'
  | 'solarized-dark' | 'solarized-light'
  | 'kanagawa-wave' | 'kanagawa-dragon' | 'kanagawa-lotus'
  | 'rose-pine' | 'rose-pine-moon' | 'rose-pine-dawn'
  | 'night-owl'
  | 'everforest-dark' | 'everforest-light'
  | 'cobalt2'
  | 'flexoki-dark' | 'flexoki-light'
  | 'hacker-green'
  | 'material-dark' | 'material-light';

export interface ColorThemeDef {
  id: ColorThemeId;
  name: string;
  scheme: 'dark' | 'light';
  preview: [string, string, string, string, string];
  vars: Record<string, string>;
}

export const colorThemes: ColorThemeDef[] = themesData as unknown as ColorThemeDef[];

export const colorThemeMap: Record<ColorThemeId, ColorThemeDef> =
  Object.fromEntries(colorThemes.map(t => [t.id, t])) as Record<ColorThemeId, ColorThemeDef>;

// "Operator Console" is the new default palette (Wave 1 redesign). The
// existing themes remain selectable; only the fallback default changes when no
// preference is saved in localStorage.
export const DEFAULT_DARK_THEME: ColorThemeId = 'operator-dark';
export const DEFAULT_LIGHT_THEME: ColorThemeId = 'operator-light';
