import React, { useEffect } from "react";
import { View, ScrollView } from "react-native";
import { useFocusEffect } from "@react-navigation/native";

import { Screen } from "../../../ui/primitives/Screen";
import { Text } from "../../../ui/primitives/Text";
import { useActivity } from "../../state/activity";
import { theme } from "../../../ui/theme";

export function ActivityScreen() {
  const { items, refresh } = useActivity();

  useEffect(() => {
    refresh();
  }, [refresh]);

  useFocusEffect(
    React.useCallback(() => {
      void refresh();
    }, [refresh])
  );

  return (
    <Screen>
      <ScrollView contentContainerStyle={{ paddingHorizontal: theme.spacing.lg, paddingTop: theme.spacing.xl, paddingBottom: 140 }}>
        <Text testID="screen-activity" variant="display">Activity</Text>
        <Text variant="muted" style={{ marginTop: theme.spacing.xs }}>
          Agent actions, messages, and runtime logs.
        </Text>

        <View style={{ marginTop: theme.spacing.lg, gap: theme.spacing.sm }}>
          {items.length === 0 ? (
            <View style={{ padding: theme.spacing.lg, borderRadius: theme.radii.lg, backgroundColor: theme.colors.surface.raised }}>
              <Text variant="body">No activity yet.</Text>
              <Text variant="muted" style={{ marginTop: theme.spacing.xs }}>
                Open chat or settings to generate activity.
              </Text>
            </View>
          ) : (
            items.map((it) => (
              <View
                key={it.id}
                style={{
                  padding: theme.spacing.md,
                  borderRadius: theme.radii.lg,
                  backgroundColor: theme.colors.surface.raised,
                  borderWidth: 1,
                  borderColor: theme.colors.stroke.subtle
                }}
              >
                <Text variant="bodyMedium">{it.title}</Text>
                <Text variant="mono" style={{ marginTop: 6, color: theme.colors.base.textMuted }}>
                  {`${it.source} / ${it.kind}`}
                </Text>
                {it.detail ? (
                  <Text variant="muted" style={{ marginTop: 4 }}>
                    {it.detail}
                  </Text>
                ) : null}
                <Text variant="mono" style={{ marginTop: 10, color: theme.colors.base.textMuted }}>
                  {new Date(it.ts).toLocaleString()}
                </Text>
              </View>
            ))
          )}
        </View>
      </ScrollView>
    </Screen>
  );
}
