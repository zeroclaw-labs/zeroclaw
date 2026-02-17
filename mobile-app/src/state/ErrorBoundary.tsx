import React from "react";
import { View, ScrollView } from "react-native";

import { theme } from "../../ui/theme";
import { Text } from "../../ui/primitives/Text";

type State = {
  error: Error | null;
  componentStack?: string;
};

export class ErrorBoundary extends React.Component<{ children: React.ReactNode }, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  componentDidCatch(error: Error, info: React.ErrorInfo) {
    // eslint-disable-next-line no-console
    console.error("[mobile] uncaught error", error, info.componentStack);
    this.setState({ componentStack: info.componentStack });
  }

  render() {
    if (!this.state.error) return this.props.children;

    return (
      <View style={{ flex: 1, backgroundColor: theme.colors.base.background, padding: theme.spacing.lg }}>
        <Text variant="display">Something went wrong</Text>
        <Text variant="muted" style={{ marginTop: theme.spacing.sm }}>
          This is a dev-only crash screen.
        </Text>
        <ScrollView style={{ marginTop: theme.spacing.lg }}>
          <View style={{ padding: theme.spacing.md, borderRadius: theme.radii.lg, backgroundColor: theme.colors.surface.raised, borderWidth: 1, borderColor: theme.colors.stroke.subtle }}>
            <Text variant="mono">{String(this.state.error.message || this.state.error)}</Text>
            {this.state.componentStack ? (
              <Text variant="mono" style={{ marginTop: theme.spacing.md, color: theme.colors.base.textMuted }}>
                {this.state.componentStack}
              </Text>
            ) : null}
          </View>
        </ScrollView>
      </View>
    );
  }
}
