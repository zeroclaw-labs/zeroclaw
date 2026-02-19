import React, { useEffect } from "react";
import { View, ScrollView } from "react-native";
import { useFocusEffect } from "@react-navigation/native";

import { Screen } from "../../../ui/primitives/Screen";
import { Text } from "../../../ui/primitives/Text";
import { useActivity } from "../../state/activity";
import { theme } from "../../../ui/theme";
import { getRuntimeSupervisorState, type RuntimeSupervisorState } from "../../runtime/supervisor";
import { getAndroidRuntimeBridgeStatus } from "../../native/androidAgentBridge";

export function ActivityScreen() {
  const { items, refresh } = useActivity();
  const [runtimeState, setRuntimeState] = React.useState<RuntimeSupervisorState | null>(null);
  const [bridgeState, setBridgeState] = React.useState<{
    queueSize: number;
    alwaysOn: boolean;
    runtimeReady: boolean;
    daemonUp: boolean;
    telegramSeenCount: number;
    webhookSuccessCount: number;
    webhookFailCount: number;
    lastEventNote: string;
  } | null>(null);

  useEffect(() => {
    refresh();
    void getRuntimeSupervisorState().then(setRuntimeState);
    void getAndroidRuntimeBridgeStatus().then(setBridgeState);
  }, [refresh]);

  useFocusEffect(
    React.useCallback(() => {
      void refresh();
      void getRuntimeSupervisorState().then(setRuntimeState);
      void getAndroidRuntimeBridgeStatus().then(setBridgeState);
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
          <View
            style={{
              padding: theme.spacing.md,
              borderRadius: theme.radii.lg,
              backgroundColor: theme.colors.surface.raised,
              borderWidth: 1,
              borderColor: theme.colors.stroke.subtle,
            }}
          >
            <Text variant="bodyMedium">ZeroClaw Runtime</Text>
            <Text variant="mono" style={{ marginTop: 6, color: theme.colors.base.textMuted }}>
              {runtimeState
                ? `status=${runtimeState.status} | reason=${runtimeState.degradeReason} | restarts=${runtimeState.restartCount}`
                : "status=unknown"}
            </Text>
            {runtimeState?.components?.length ? (
              <Text variant="muted" style={{ marginTop: 4 }}>
                {runtimeState.components.join(", ")}
              </Text>
            ) : (
              <Text variant="muted" style={{ marginTop: 4 }}>
                No runtime components active.
              </Text>
            )}
            {runtimeState?.missingConfig?.length ? (
              <Text variant="muted" style={{ marginTop: 4 }}>
                Missing config: {runtimeState.missingConfig.join(", ")}
              </Text>
            ) : null}
            {bridgeState ? (
              <Text variant="muted" style={{ marginTop: 4 }}>
                Native bridge: queue={bridgeState.queueSize}, always_on={bridgeState.alwaysOn ? "on" : "off"}, runtime_ready={bridgeState.runtimeReady ? "yes" : "no"}, daemon_up={bridgeState.daemonUp ? "yes" : "no"}, telegram_seen={bridgeState.telegramSeenCount}, handled_ok={bridgeState.webhookSuccessCount}, handled_fail={bridgeState.webhookFailCount}
              </Text>
            ) : null}
            {bridgeState?.lastEventNote ? (
              <Text variant="muted" style={{ marginTop: 4 }}>
                Last bridge event: {bridgeState.lastEventNote}
              </Text>
            ) : null}
          </View>

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
