import React from "react";
import { Text as RNText, type TextProps, StyleSheet } from "react-native";

import { theme } from "../theme";

type Variant = "display" | "title" | "body" | "bodyMedium" | "muted" | "label" | "mono";

export function Text({ style, variant = "body", ...props }: TextProps & { variant?: Variant }) {
  return <RNText {...props} style={[styles.base, styles[variant], style]} />;
}

const styles = StyleSheet.create({
  base: {
    color: theme.colors.base.text
  },
  display: {
    fontFamily: theme.typography.display,
    fontSize: 30,
    letterSpacing: -0.6
  },
  title: {
    fontFamily: theme.typography.bodyMedium,
    fontSize: 18,
    letterSpacing: -0.2
  },
  body: {
    fontFamily: theme.typography.body,
    fontSize: 15
  },
  bodyMedium: {
    fontFamily: theme.typography.bodyMedium,
    fontSize: 15
  },
  muted: {
    fontFamily: theme.typography.body,
    fontSize: 14,
    color: theme.colors.base.textMuted
  },
  label: {
    fontFamily: theme.typography.bodyMedium,
    fontSize: 13,
    color: theme.colors.base.textMuted,
    letterSpacing: 0.2,
    textTransform: "uppercase"
  },
  mono: {
    fontFamily: theme.typography.mono,
    fontSize: 13
  }
});
