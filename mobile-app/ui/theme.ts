import { config } from "../src/config";

const normalizeHex = (hex: string) => {
  const h = hex.trim();
  if (!h.startsWith("#")) return h;
  if (h.length === 4) {
    const r = h[1];
    const g = h[2];
    const b = h[3];
    return `#${r}${r}${g}${g}${b}${b}`;
  }
  return h;
};

const hexToRgba = (hex: string, alpha: number) => {
  const h = normalizeHex(hex);
  if (!h.startsWith("#") || h.length !== 7) return h;
  const r = parseInt(h.slice(1, 3), 16);
  const g = parseInt(h.slice(3, 5), 16);
  const b = parseInt(h.slice(5, 7), 16);
  const a = Math.max(0, Math.min(1, alpha));
  return `rgba(${r}, ${g}, ${b}, ${a})`;
};

export const theme = {
  radii: {
    xs: 10,
    sm: 14,
    md: 18,
    lg: 26,
    xl: 34
  },
  spacing: {
    xs: 8,
    sm: 12,
    md: 16,
    lg: 24,
    xl: 32
  },
  typography: {
    display: "SpaceGrotesk_600SemiBold",
    body: "Inter_400Regular",
    bodyMedium: "Inter_500Medium",
    mono: "JetBrainsMono_500Medium"
  },
  colors: {
    base: {
      background: config.theme.background,
      text: config.theme.text,
      textMuted: config.theme.textMuted,
      border: config.theme.border,
      primary: config.theme.primary,
      secondary: config.theme.secondary,
      accent: config.theme.accent
    },
    surface: {
      // Keep the dock more glassy; make the rest more opaque.
      dock: hexToRgba(config.theme.text, 0.08),
      raised: hexToRgba(config.theme.text, 0.16),
      panel: hexToRgba(config.theme.text, 0.20),
      glass: hexToRgba(config.theme.text, 0.12)
    },
    stroke: {
      subtle: hexToRgba(config.theme.text, 0.12),
      stronger: hexToRgba(config.theme.text, 0.18)
    },
    shadow: {
      soft: hexToRgba(config.theme.background, 0.7),
      glowViolet: hexToRgba(config.theme.secondary, 0.45),
      glowLime: hexToRgba(config.theme.primary, 0.4),
      glowPink: hexToRgba(config.theme.accent, 0.35)
    },
    alpha: {
      transparent: hexToRgba(config.theme.background, 0),
      textSubtle: hexToRgba(config.theme.text, 0.7),
      textPlaceholder: hexToRgba(config.theme.text, 0.45),
      textFaint: hexToRgba(config.theme.text, 0.15),
      borderFaint: hexToRgba(config.theme.border, 0.14),
      surfaceFaint: hexToRgba(config.theme.border, 0.06),
      scrim: hexToRgba(config.theme.background, 0.55),
      userBubbleBg: hexToRgba(config.theme.primary, 0.14),
      userBubbleBorder: hexToRgba(config.theme.primary, 0.25),
      buttonSecondaryTop: hexToRgba(config.theme.text, 0.06),
      buttonSecondaryBottom: hexToRgba(config.theme.text, 0.02)
    },
    overlay: {
      cardGradient: [
        hexToRgba(config.theme.secondary, 0.28),
        hexToRgba(config.theme.accent, 0.18),
        hexToRgba(config.theme.primary, 0.1)
      ] as const,
      dockStroke: [hexToRgba(config.theme.text, 0.16), hexToRgba(config.theme.text, 0.06)] as const,
      dockIconIdle: hexToRgba(config.theme.text, 0.65),
      iconOnGradient: hexToRgba(config.theme.text, 0.95)
    },
    gradient: {
      holographic: [config.theme.secondary, config.theme.accent, config.theme.primary] as const
    }
  }
};
