import type { AccentColor } from '@/hooks/use-settings'

export const ACCENT_HUES: Record<AccentColor, number> = {
  orange: 60,
  purple: 300,
  blue: 250,
  green: 150,
}

export function applyAccentColor(color: AccentColor) {
  if (typeof document === 'undefined') return
  document.documentElement.setAttribute('data-accent', color)
}
