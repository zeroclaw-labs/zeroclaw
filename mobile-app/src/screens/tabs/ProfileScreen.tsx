import React, { useCallback, useState } from "react";
import { View, TextInput } from "react-native";

import { Screen } from "../../../ui/primitives/Screen";
import { Text } from "../../../ui/primitives/Text";
import { LiquidButton } from "../../../ui/primitives/LiquidButton";
import { theme } from "../../../ui/theme";
import { config } from "../../config";
import { useToast } from "../../state/toast";
import { joinWaitlist } from "../../api/platform";

export function ProfileScreen() {
  const toast = useToast();
  const [email, setEmail] = useState("");
  const [busy, setBusy] = useState(false);

  const onJoin = useCallback(async () => {
    setBusy(true);
    try {
      await joinWaitlist(email);
      toast.show("You're on the list.");
      setEmail("");
    } catch {
      toast.show("Couldn't add that email.");
    } finally {
      setBusy(false);
    }
  }, [email, toast]);

  return (
    <Screen>
      <View style={{ paddingHorizontal: theme.spacing.lg, paddingTop: theme.spacing.xl, paddingBottom: 140, gap: theme.spacing.lg }}>
        <View>
          <Text variant="display">Profile</Text>
          <Text variant="muted" style={{ marginTop: theme.spacing.xs }}>
            Settings, tokens, and exports will live here.
          </Text>
        </View>

        <View style={{ padding: theme.spacing.lg, borderRadius: theme.radii.xl, backgroundColor: theme.colors.surface.raised, borderWidth: 1, borderColor: theme.colors.stroke.subtle }}>
          <Text variant="title">Environment</Text>
          <Text variant="mono" style={{ marginTop: theme.spacing.sm }}>
            PLATFORM_URL
          </Text>
          <Text variant="muted" style={{ marginTop: 4 }}>
            {config.platformUrl}
          </Text>
        </View>

        <View style={{ padding: theme.spacing.lg, borderRadius: theme.radii.xl, backgroundColor: theme.colors.surface.raised, borderWidth: 1, borderColor: theme.colors.stroke.subtle }}>
          <Text variant="title">Early Access</Text>
          <Text variant="muted" style={{ marginTop: theme.spacing.xs }}>
            Leave an email to get updates.
          </Text>
          <TextInput
            value={email}
            onChangeText={setEmail}
            placeholder="you@domain.com"
            placeholderTextColor={theme.colors.alpha.textPlaceholder}
            autoCapitalize="none"
            keyboardType="email-address"
            style={{
              marginTop: theme.spacing.md,
              borderRadius: theme.radii.lg,
              padding: theme.spacing.md,
              backgroundColor: theme.colors.surface.panel,
              borderWidth: 1,
              borderColor: theme.colors.stroke.subtle,
              color: theme.colors.base.text,
              fontFamily: theme.typography.body
            }}
          />
          <View style={{ marginTop: theme.spacing.md }}>
            <LiquidButton label={busy ? "Sending…" : "Join waitlist"} onPress={onJoin} disabled={busy || !email.trim()} />
          </View>
        </View>
      </View>
    </Screen>
  );
}
