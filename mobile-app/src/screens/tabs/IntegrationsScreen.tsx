import React, { useEffect, useRef, useState } from "react";
import { ScrollView, Switch, TextInput, View } from "react-native";

import { Screen } from "../../../ui/primitives/Screen";
import { Text } from "../../../ui/primitives/Text";
import { theme } from "../../../ui/theme";
import { addActivity } from "../../state/activity";
import {
  DEFAULT_INTEGRATIONS,
  type IntegrationsConfig,
  loadIntegrationsConfig,
  saveIntegrationsConfig,
} from "../../state/mobileclaw";

function IntegrationCard(props: {
  name: string;
  enabled: boolean;
  onToggle: (next: boolean) => void;
  children: React.ReactNode;
}) {
  return (
    <View
      style={{
        padding: theme.spacing.lg,
        borderRadius: theme.radii.xl,
        backgroundColor: theme.colors.surface.raised,
        borderWidth: 1,
        borderColor: theme.colors.stroke.subtle,
        gap: theme.spacing.sm,
      }}
    >
      <View style={{ flexDirection: "row", justifyContent: "space-between", alignItems: "center" }}>
        <Text variant="title">{props.name}</Text>
        <Switch value={props.enabled} onValueChange={props.onToggle} />
      </View>
      {props.enabled ? props.children : <Text variant="muted">Disabled</Text>}
    </View>
  );
}

function SecretField(props: { label: string; value: string; onChangeText: (value: string) => void }) {
  return (
    <View style={{ gap: 6 }}>
      <Text variant="label">{props.label}</Text>
      <TextInput
        value={props.value}
        onChangeText={props.onChangeText}
        secureTextEntry
        style={{
          borderRadius: theme.radii.lg,
          padding: theme.spacing.md,
          backgroundColor: theme.colors.surface.panel,
          borderWidth: 1,
          borderColor: theme.colors.stroke.subtle,
          color: theme.colors.base.text,
          fontFamily: theme.typography.body,
        }}
      />
    </View>
  );
}

export function IntegrationsScreen() {
  const [form, setForm] = useState<IntegrationsConfig>(DEFAULT_INTEGRATIONS);
  const [saveStatus, setSaveStatus] = useState("Loading...");
  const hydratedRef = useRef(false);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      const loaded = await loadIntegrationsConfig();
      if (!cancelled) {
        setForm(loaded);
        hydratedRef.current = true;
        setSaveStatus("Autosave enabled");
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    if (!hydratedRef.current) return;
    const timer = setTimeout(() => {
      void saveIntegrationsConfig(form);
      setSaveStatus("Saved locally");
    }, 300);
    return () => clearTimeout(timer);
  }, [form]);

  return (
    <Screen>
      <ScrollView contentContainerStyle={{ paddingHorizontal: theme.spacing.lg, paddingTop: theme.spacing.xl, paddingBottom: 140, gap: theme.spacing.lg }}>
        <View>
          <Text testID="screen-integrations" variant="display">Integrations</Text>
          <Text variant="muted" style={{ marginTop: theme.spacing.xs }}>
            Configure Telegram, Discord, Slack, WhatsApp, and Composio.
          </Text>
          <Text variant="mono" style={{ marginTop: theme.spacing.sm, color: theme.colors.base.textMuted }}>
            {saveStatus}
          </Text>
        </View>

        <IntegrationCard name="Telegram" enabled={form.telegramEnabled} onToggle={(next) => setForm((prev) => ({ ...prev, telegramEnabled: next }))}>
          <SecretField label="Bot token" value={form.telegramBotToken} onChangeText={(value) => setForm((prev) => ({ ...prev, telegramBotToken: value }))} />
          <SecretField label="Chat ID" value={form.telegramChatId} onChangeText={(value) => setForm((prev) => ({ ...prev, telegramChatId: value }))} />
        </IntegrationCard>

        <IntegrationCard name="Discord" enabled={form.discordEnabled} onToggle={(next) => setForm((prev) => ({ ...prev, discordEnabled: next }))}>
          <SecretField label="Bot token" value={form.discordBotToken} onChangeText={(value) => setForm((prev) => ({ ...prev, discordBotToken: value }))} />
        </IntegrationCard>

        <IntegrationCard name="Slack" enabled={form.slackEnabled} onToggle={(next) => setForm((prev) => ({ ...prev, slackEnabled: next }))}>
          <SecretField label="Bot token" value={form.slackBotToken} onChangeText={(value) => setForm((prev) => ({ ...prev, slackBotToken: value }))} />
        </IntegrationCard>

        <IntegrationCard name="WhatsApp" enabled={form.whatsappEnabled} onToggle={(next) => setForm((prev) => ({ ...prev, whatsappEnabled: next }))}>
          <SecretField label="Access token" value={form.whatsappAccessToken} onChangeText={(value) => setForm((prev) => ({ ...prev, whatsappAccessToken: value }))} />
        </IntegrationCard>

        <IntegrationCard name="Composio" enabled={form.composioEnabled} onToggle={(next) => setForm((prev) => ({ ...prev, composioEnabled: next }))}>
          <SecretField label="API key" value={form.composioApiKey} onChangeText={(value) => setForm((prev) => ({ ...prev, composioApiKey: value }))} />
        </IntegrationCard>
      </ScrollView>
    </Screen>
  );
}
