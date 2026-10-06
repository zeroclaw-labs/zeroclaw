import themesData from './themes.json';

export type ColorThemeId =
  | 'operator-dark' | 'operator-light'
  | 'calm-dark' | 'calm-light'
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
  /** Dark/light variants that should stay together when appearance mode changes. */
  family?: string;
  preview: [string, string, string, string, string];
  vars: Record<string, string>;
}

export const colorThemes: ColorThemeDef[] = themesData as unknown as ColorThemeDef[];

export const colorThemeMap: Record<ColorThemeId, ColorThemeDef> =
  Object.fromEntries(colorThemes.map(t => [t.id, t])) as Record<ColorThemeId, ColorThemeDef>;

// Match master’s Operator Console defaults; Calm remains an opt-in palette.
export const DEFAULT_DARK_THEME: ColorThemeId = 'operator-dark';
export const DEFAULT_LIGHT_THEME: ColorThemeId = 'operator-light';

export function themeForScheme(id: ColorThemeId, scheme: 'dark' | 'light'): ColorThemeId {
  const current = colorThemeMap[id];
  if (current.scheme === scheme) return id;
  return colorThemes.find(theme => current.family && theme.family === current.family && theme.scheme === scheme)?.id
    ?? (scheme === 'light' ? DEFAULT_LIGHT_THEME : DEFAULT_DARK_THEME);
}
