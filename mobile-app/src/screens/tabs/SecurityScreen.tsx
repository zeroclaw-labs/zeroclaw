import React, { useEffect, useRef, useState } from "react";
import { Pressable, ScrollView, Switch, View } from "react-native";
import { useNavigation } from "@react-navigation/native";

import { Screen } from "../../../ui/primitives/Screen";
import { Text } from "../../../ui/primitives/Text";
import { theme } from "../../../ui/theme";
import { addActivity } from "../../state/activity";
import { DEFAULT_SECURITY, loadSecurityConfig, saveSecurityConfig } from "../../state/mobileclaw";

export function SecurityScreen() {
  const navigation = useNavigation<any>();
  const [requireApproval, setRequireApproval] = useState(DEFAULT_SECURITY.requireApproval);
  const [highRiskActions, setHighRiskActions] = useState(DEFAULT_SECURITY.highRiskActions);
  const [incomingCallHooks, setIncomingCallHooks] = useState(DEFAULT_SECURITY.incomingCallHooks);
  const [includeCallerNumber, setIncludeCallerNumber] = useState(DEFAULT_SECURITY.includeCallerNumber);
  const [saveStatus, setSaveStatus] = useState("Loading...");
  const hydratedRef = useRef(false);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      const loaded = await loadSecurityConfig();
      if (cancelled) return;
      setRequireApproval(loaded.requireApproval);
      setHighRiskActions(loaded.highRiskActions);
      setIncomingCallHooks(loaded.incomingCallHooks);
      setIncludeCallerNumber(loaded.includeCallerNumber);
      hydratedRef.current = true;
      setSaveStatus("Autosave enabled");
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    if (!hydratedRef.current) return;
    const timer = setTimeout(() => {
      void saveSecurityConfig({ requireApproval, highRiskActions, incomingCallHooks, includeCallerNumber });
      void addActivity({
        kind: "action",
        source: "security",
        title: "Security policy updated",
        detail: `approval=${requireApproval}, high_risk=${highRiskActions}, call_hooks=${incomingCallHooks}, caller_number=${includeCallerNumber}`,
      });
      setSaveStatus("Saved locally");
    }, 300);
    return () => clearTimeout(timer);
  }, [requireApproval, highRiskActions, incomingCallHooks, includeCallerNumber]);

  return (
    <Screen>
      <ScrollView contentContainerStyle={{ paddingHorizontal: theme.spacing.lg, paddingTop: theme.spacing.xl, paddingBottom: 140, gap: theme.spacing.lg }}>
        <View>
          <Pressable
            testID="security-back"
            onPress={() => navigation.goBack()}
            style={{ alignSelf: "flex-start", paddingVertical: 8, paddingHorizontal: 12, borderRadius: theme.radii.lg, borderWidth: 1, borderColor: theme.colors.stroke.subtle, backgroundColor: theme.colors.surface.panel, marginBottom: theme.spacing.sm }}
          >
            <Text variant="bodyMedium">Back</Text>
          </Pressable>
          <Text testID="screen-security" variant="display">Security</Text>
          <Text variant="muted" style={{ marginTop: theme.spacing.xs }}>
            Approval gates for sensitive and high-risk actions.
          </Text>
          <Text variant="mono" style={{ marginTop: theme.spacing.sm, color: theme.colors.base.textMuted }}>
            {saveStatus}
          </Text>
        </View>

        <View style={{ padding: theme.spacing.lg, borderRadius: theme.radii.xl, backgroundColor: theme.colors.surface.raised, borderWidth: 1, borderColor: theme.colors.stroke.subtle, gap: theme.spacing.md }}>
          <View style={{ flexDirection: "row", justifyContent: "space-between", alignItems: "center" }}>
            <Text variant="bodyMedium">Require approval for calls/SMS</Text>
            <Switch value={requireApproval} onValueChange={setRequireApproval} />
          </View>
          <View style={{ flexDirection: "row", justifyContent: "space-between", alignItems: "center" }}>
            <Text variant="bodyMedium">Enable high-risk actions</Text>
            <Switch value={highRiskActions} onValueChange={setHighRiskActions} />
          </View>
          <View style={{ flexDirection: "row", justifyContent: "space-between", alignItems: "center" }}>
            <Text variant="bodyMedium">Enable incoming call hooks</Text>
            <Switch value={incomingCallHooks} onValueChange={setIncomingCallHooks} />
          </View>
          <View style={{ flexDirection: "row", justifyContent: "space-between", alignItems: "center" }}>
            <Text variant="bodyMedium">Share caller number with agent</Text>
            <Switch value={includeCallerNumber} onValueChange={setIncludeCallerNumber} />
          </View>
        </View>

      </ScrollView>
    </Screen>
  );
}
