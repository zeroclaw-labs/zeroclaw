/**
 * Color theme palettes for the ZeroClaw dashboard.
 *
 * Each theme defines the full set of --pc-* CSS variables.
 * Themes are grouped by scheme ('dark' | 'light') so the system
 * preference resolver can pick the right default.
 */

export type ColorThemeId =
  | 'default-dark' | 'default-light' | 'oled-black'
  | 'nord-dark' | 'nord-light'
  | 'dracula'
  | 'monokai'
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
  /** Preview colors for the settings card [bg, bar1, bar2, bar3, text] */
  preview: [string, string, string, string, string];
  vars: Record<string, string>;
}

function darkBase(
  bgBase: string, bgSurface: string, bgElevated: string,
  bgInput: string, bgCode: string,
  textPrimary: string, textSecondary: string, textMuted: string, textFaint: string,
  accent: string, accentLight: string,
): Record<string, string> {
  const r = parseInt(accent.slice(1, 3), 16);
  const g = parseInt(accent.slice(3, 5), 16);
  const b = parseInt(accent.slice(5, 7), 16);
  return {
    '--pc-bg-base': bgBase,
    '--color-scheme': 'dark',
    '--pc-bg-surface': bgSurface,
    '--pc-bg-elevated': bgElevated,
    '--pc-bg-input': bgInput,
    '--pc-bg-sidebar': `${bgBase}f2`,
    '--pc-bg-code': bgCode,
    '--pc-border': 'rgba(255,255,255,0.08)',
    '--pc-border-strong': 'rgba(255,255,255,0.12)',
    '--pc-text-primary': textPrimary,
    '--pc-text-secondary': textSecondary,
    '--pc-text-muted': textMuted,
    '--pc-text-faint': textFaint,
    '--pc-scrollbar-thumb': textFaint,
    '--pc-scrollbar-track': bgSurface,
    '--pc-scrollbar-thumb-hover': textMuted,
    '--pc-hover': 'rgba(255,255,255,0.05)',
    '--pc-hover-strong': 'rgba(255,255,255,0.08)',
    '--pc-separator': 'rgba(255,255,255,0.05)',
    '--pc-accent': accent,
    '--pc-accent-light': accentLight,
    '--pc-accent-dim': `rgba(${r},${g},${b},0.3)`,
    '--pc-accent-glow': `rgba(${r},${g},${b},0.1)`,
    '--pc-accent-rgb': `${r},${g},${b}`,
  };
}

function lightBase(
  bgBase: string, bgSurface: string, bgElevated: string,
  bgInput: string, bgCode: string,
  textPrimary: string, textSecondary: string, textMuted: string, textFaint: string,
  accent: string, accentLight: string,
): Record<string, string> {
  const r = parseInt(accent.slice(1, 3), 16);
  const g = parseInt(accent.slice(3, 5), 16);
  const b = parseInt(accent.slice(5, 7), 16);
  return {
    '--pc-bg-base': bgBase,
    '--color-scheme': 'light',
    '--pc-bg-surface': bgSurface,
    '--pc-bg-elevated': bgElevated,
    '--pc-bg-input': bgInput,
    '--pc-bg-sidebar': `${bgSurface}f2`,
    '--pc-bg-code': bgCode,
    '--pc-border': 'rgba(0,0,0,0.08)',
    '--pc-border-strong': 'rgba(0,0,0,0.12)',
    '--pc-text-primary': textPrimary,
    '--pc-text-secondary': textSecondary,
    '--pc-text-muted': textMuted,
    '--pc-text-faint': textFaint,
    '--pc-scrollbar-thumb': textFaint,
    '--pc-scrollbar-track': bgElevated,
    '--pc-scrollbar-thumb-hover': textMuted,
    '--pc-hover': 'rgba(0,0,0,0.04)',
    '--pc-hover-strong': 'rgba(0,0,0,0.07)',
    '--pc-separator': 'rgba(0,0,0,0.06)',
    '--pc-accent': accent,
    '--pc-accent-light': accentLight,
    '--pc-accent-dim': `rgba(${r},${g},${b},0.25)`,
    '--pc-accent-glow': `rgba(${r},${g},${b},0.08)`,
    '--pc-accent-rgb': `${r},${g},${b}`,
  };
}

export const colorThemes: ColorThemeDef[] = [
  // ── Defaults ────────────────────────────────────────────────
  {
    id: 'default-dark', name: 'Default Dark', scheme: 'dark',
    preview: ['#1e1e24', '#22d3ee', '#a78bfa', '#f59e0b', '#d4d4d8'],
    vars: darkBase('#1e1e24', '#232329', '#27272a', '#1a1a20', '#1a1a20',
      '#d4d4d8', '#a1a1aa', '#71717a', '#52525b', '#22d3ee', '#67e8f9'),
  },
  {
    id: 'default-light', name: 'Default Light', scheme: 'light',
    preview: ['#f4f4f5', '#22d3ee', '#8b5cf6', '#f59e0b', '#18181b'],
    vars: lightBase('#f4f4f5', '#ffffff', '#e4e4e7', '#ffffff', '#f4f4f5',
      '#18181b', '#3f3f46', '#71717a', '#a1a1aa', '#0891b2', '#06b6d4'),
  },
  {
    id: 'oled-black', name: 'OLED Black', scheme: 'dark',
    preview: ['#000000', '#22d3ee', '#8b5cf6', '#10b981', '#d4d4d8'],
    vars: darkBase('#000000', '#0a0a0a', '#141414', '#0a0a0a', '#0a0a0a',
      '#d4d4d8', '#a1a1aa', '#71717a', '#3f3f46', '#22d3ee', '#67e8f9'),
  },

  // ── Nord ────────────────────────────────────────────────────
  {
    id: 'nord-dark', name: 'Nord Dark', scheme: 'dark',
    preview: ['#2e3440', '#88c0d0', '#81a1c1', '#a3be8c', '#eceff4'],
    vars: darkBase('#2e3440', '#3b4252', '#434c5e', '#2e3440', '#2e3440',
      '#eceff4', '#d8dee9', '#7b88a1', '#4c566a', '#88c0d0', '#8fbcbb'),
  },
  {
    id: 'nord-light', name: 'Nord Light', scheme: 'light',
    preview: ['#eceff4', '#5e81ac', '#88c0d0', '#a3be8c', '#2e3440'],
    vars: lightBase('#eceff4', '#e5e9f0', '#d8dee9', '#e5e9f0', '#e5e9f0',
      '#2e3440', '#3b4252', '#4c566a', '#7b88a1', '#5e81ac', '#81a1c1'),
  },

  // ── Dracula ─────────────────────────────────────────────────
  {
    id: 'dracula', name: 'Dracula', scheme: 'dark',
    preview: ['#282a36', '#bd93f9', '#ff79c6', '#50fa7b', '#f8f8f2'],
    vars: darkBase('#282a36', '#21222c', '#343746', '#1e1f29', '#1e1f29',
      '#f8f8f2', '#c0c0d0', '#6272a4', '#44475a', '#bd93f9', '#caa9fa'),
  },

  // ── Monokai ─────────────────────────────────────────────────
  {
    id: 'monokai', name: 'Monokai', scheme: 'dark',
    preview: ['#272822', '#f92672', '#a6e22e', '#e6db74', '#f8f8f2'],
    vars: darkBase('#272822', '#2d2e27', '#3e3d32', '#1e1f1c', '#1e1f1c',
      '#f8f8f2', '#c0c0b0', '#75715e', '#49483e', '#f92672', '#fd5fa0'),
  },

  // ── Solarized ───────────────────────────────────────────────
  {
    id: 'solarized-dark', name: 'Solarized Dark', scheme: 'dark',
    preview: ['#002b36', '#268bd2', '#2aa198', '#b58900', '#839496'],
    vars: darkBase('#002b36', '#073642', '#0a4050', '#002028', '#002028',
      '#839496', '#93a1a1', '#657b83', '#586e75', '#268bd2', '#6cb6e8'),
  },
  {
    id: 'solarized-light', name: 'Solarized Light', scheme: 'light',
    preview: ['#fdf6e3', '#268bd2', '#2aa198', '#b58900', '#073642'],
    vars: lightBase('#fdf6e3', '#eee8d5', '#ddd6c1', '#fdf6e3', '#eee8d5',
      '#073642', '#586e75', '#657b83', '#93a1a1', '#268bd2', '#2aa198'),
  },

  // ── Kanagawa ────────────────────────────────────────────────
  {
    id: 'kanagawa-wave', name: 'Kanagawa Wave', scheme: 'dark',
    preview: ['#1f1f28', '#7e9cd8', '#957fb8', '#e6c384', '#dcd7ba'],
    vars: darkBase('#1f1f28', '#2a2a37', '#363646', '#16161d', '#16161d',
      '#dcd7ba', '#c8c093', '#727169', '#54546d', '#7e9cd8', '#7fb4ca'),
  },
  {
    id: 'kanagawa-dragon', name: 'Kanagawa Dragon', scheme: 'dark',
    preview: ['#181616', '#8ba4b0', '#a292a3', '#c4b28a', '#c5c9c5'],
    vars: darkBase('#181616', '#201d1d', '#2d2a2a', '#12120f', '#12120f',
      '#c5c9c5', '#a6a69c', '#737c73', '#625e5a', '#8ba4b0', '#9cabba'),
  },
  {
    id: 'kanagawa-lotus', name: 'Kanagawa Lotus', scheme: 'light',
    preview: ['#f2ecbc', '#4d699b', '#b35b79', '#836f4a', '#1f1f28'],
    vars: lightBase('#f2ecbc', '#e7dba0', '#d5cea3', '#f2ecbc', '#e7dba0',
      '#1f1f28', '#545464', '#716e61', '#8a8980', '#4d699b', '#6693bf'),
  },

  // ── Ros\u00e9 Pine ──────────────────────────────────────────────
  {
    id: 'rose-pine', name: 'Ros\u00e9 Pine', scheme: 'dark',
    preview: ['#191724', '#ebbcba', '#c4a7e7', '#f6c177', '#e0def4'],
    vars: darkBase('#191724', '#1f1d2e', '#26233a', '#13111e', '#13111e',
      '#e0def4', '#908caa', '#6e6a86', '#524f67', '#ebbcba', '#f2d5ce'),
  },
  {
    id: 'rose-pine-moon', name: 'Ros\u00e9 Pine Moon', scheme: 'dark',
    preview: ['#232136', '#ea9a97', '#c4a7e7', '#f6c177', '#e0def4'],
    vars: darkBase('#232136', '#2a273f', '#393552', '#1b1930', '#1b1930',
      '#e0def4', '#908caa', '#6e6a86', '#44415a', '#ea9a97', '#f0b8b6'),
  },
  {
    id: 'rose-pine-dawn', name: 'Ros\u00e9 Pine Dawn', scheme: 'light',
    preview: ['#faf4ed', '#d7827e', '#907aa9', '#ea9d34', '#575279'],
    vars: lightBase('#faf4ed', '#fffaf3', '#f2e9de', '#fffaf3', '#f2e9de',
      '#575279', '#797593', '#9893a5', '#cecacd', '#d7827e', '#b4637a'),
  },

  // ── Night Owl ───────────────────────────────────────────────
  {
    id: 'night-owl', name: 'Night Owl', scheme: 'dark',
    preview: ['#011627', '#82aaff', '#c792ea', '#addb67', '#d6deeb'],
    vars: darkBase('#011627', '#0b2942', '#122d42', '#010e1a', '#010e1a',
      '#d6deeb', '#a7bbc7', '#5f7e97', '#37536b', '#82aaff', '#a0c4ff'),
  },

  // ── Everforest ──────────────────────────────────────────────
  {
    id: 'everforest-dark', name: 'Everforest Dark', scheme: 'dark',
    preview: ['#2d353b', '#a7c080', '#83c092', '#dbbc7f', '#d3c6aa'],
    vars: darkBase('#2d353b', '#343f44', '#3d484d', '#272e33', '#272e33',
      '#d3c6aa', '#9da9a0', '#7a8478', '#56635f', '#a7c080', '#83c092'),
  },
  {
    id: 'everforest-light', name: 'Everforest Light', scheme: 'light',
    preview: ['#fdf6e3', '#8da101', '#35a77c', '#dfa000', '#5c6a72'],
    vars: lightBase('#fdf6e3', '#f3ead3', '#e9dfc4', '#f3ead3', '#eee8d5',
      '#5c6a72', '#708089', '#829181', '#a6b0a0', '#8da101', '#93b259'),
  },

  // ── Cobalt2 ─────────────────────────────────────────────────
  {
    id: 'cobalt2', name: 'Cobalt2', scheme: 'dark',
    preview: ['#193549', '#ffc600', '#ff9d00', '#80ffbb', '#ffffff'],
    vars: darkBase('#193549', '#1f4662', '#234d6e', '#0d2b3e', '#0d2b3e',
      '#ffffff', '#a0c4d8', '#507a8f', '#305a6f', '#ffc600', '#ffd740'),
  },

  // ── Flexoki ─────────────────────────────────────────────────
  {
    id: 'flexoki-dark', name: 'Flexoki Dark', scheme: 'dark',
    preview: ['#100f0f', '#ce5d97', '#879a39', '#da702c', '#cecdc3'],
    vars: darkBase('#100f0f', '#1c1b1a', '#282726', '#100f0f', '#1c1b1a',
      '#cecdc3', '#b7b5ac', '#878580', '#575653', '#ce5d97', '#d68fb2'),
  },
  {
    id: 'flexoki-light', name: 'Flexoki Light', scheme: 'light',
    preview: ['#fffcf0', '#ce5d97', '#879a39', '#da702c', '#100f0f'],
    vars: lightBase('#fffcf0', '#f2f0e5', '#e6e4d9', '#fffcf0', '#f2f0e5',
      '#100f0f', '#343331', '#575653', '#878580', '#ce5d97', '#a02f6f'),
  },

  // ── Hacker Green ────────────────────────────────────────────
  {
    id: 'hacker-green', name: 'Hacker Green', scheme: 'dark',
    preview: ['#0a0e0a', '#00ff41', '#00cc33', '#008f11', '#33ff66'],
    vars: darkBase('#0a0e0a', '#0d120d', '#121a12', '#080c08', '#080c08',
      '#00ff41', '#00cc33', '#008f11', '#005a0a', '#00ff41', '#33ff66'),
  },

  // ── Material ────────────────────────────────────────────────
  {
    id: 'material-dark', name: 'Material Dark', scheme: 'dark',
    preview: ['#212121', '#89ddff', '#c792ea', '#ffcb6b', '#eeffff'],
    vars: darkBase('#212121', '#292929', '#333333', '#1a1a1a', '#1a1a1a',
      '#eeffff', '#b0bec5', '#616161', '#424242', '#89ddff', '#80cbc4'),
  },
  {
    id: 'material-light', name: 'Material Light', scheme: 'light',
    preview: ['#fafafa', '#6182b8', '#7c4dff', '#f76d47', '#212121'],
    vars: lightBase('#fafafa', '#ffffff', '#eaeaea', '#ffffff', '#f5f5f5',
      '#212121', '#424242', '#757575', '#bdbdbd', '#6182b8', '#7c4dff'),
  },
];

/** Lookup map for O(1) access by id. */
export const colorThemeMap: Record<ColorThemeId, ColorThemeDef> =
  Object.fromEntries(colorThemes.map(t => [t.id, t])) as Record<ColorThemeId, ColorThemeDef>;

/** Default theme ids for system preference resolution. */
export const DEFAULT_DARK_THEME: ColorThemeId = 'default-dark';
export const DEFAULT_LIGHT_THEME: ColorThemeId = 'default-light';
