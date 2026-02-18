import React, { useEffect, useRef, useState } from "react";
import { Pressable, ScrollView, Switch, TextInput, View } from "react-native";
import { Ionicons } from "@expo/vector-icons";

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
  expanded: boolean;
  onExpandToggle: () => void;
  onToggle: (next: boolean) => void;
  instructions: string[];
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
      <Pressable onPress={props.onExpandToggle} style={{ flexDirection: "row", justifyContent: "space-between", alignItems: "center" }}>
        <View style={{ flexDirection: "row", alignItems: "center", gap: 8 }}>
          <Text variant="title">{props.name}</Text>
          {props.enabled ? <Ionicons name="checkmark-circle" size={16} color={theme.colors.base.secondary} /> : null}
        </View>
        <View style={{ flexDirection: "row", alignItems: "center", gap: 8 }}>
          <Switch value={props.enabled} onValueChange={props.onToggle} />
          <Ionicons name={props.expanded ? "chevron-up" : "chevron-down"} size={16} color={theme.colors.base.textMuted} />
        </View>
      </Pressable>
      {props.enabled ? (
        <>
          {props.children}
          {props.expanded ? (
            <View style={{ marginTop: 4, gap: 4 }}>
              <Text variant="label">Setup guide</Text>
              {props.instructions.map((step) => (
                <Text key={`${props.name}-${step}`} variant="muted">
                  - {step}
                </Text>
              ))}
            </View>
          ) : null}
        </>
      ) : (
        <Text variant="muted">Disabled</Text>
      )}
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
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  const hydratedRef = useRef(false);

  const toggleExpanded = (key: string) => {
    setExpanded((prev) => ({ ...prev, [key]: !prev[key] }));
  };

  const setEnabled = (key: string, next: boolean) => {
    if (next) {
      setExpanded((prev) => ({ ...prev, [key]: true }));
    }
  };

  const guides: Record<string, string[]> = {
    telegram: [
      "Create bot in Telegram via @BotFather and copy bot token.",
      "Start a chat with the bot and send /start.",
      "Read chat id from updates and paste it in Chat ID field.",
      "Run ZeroClaw daemon with telegram channel enabled in config.",
    ],
    discord: [
      "Create Discord application and bot in Developer Portal.",
      "Enable message content intent and invite bot to your server.",
      "Paste bot token and start ZeroClaw daemon.",
      "Restrict allowed users/channels in server policy.",
    ],
    slack: [
      "Create Slack app and add bot token scopes.",
      "Install app to workspace and copy Bot User OAuth token.",
      "Paste token and enable Slack channel in ZeroClaw config.",
      "Invite bot to target channels and test a message.",
    ],
    whatsapp: [
      "Create Meta app and WhatsApp Business integration.",
      "Set webhook endpoint to your ZeroClaw gateway URL.",
      "Add verify token/secret and paste access token in app.",
      "Confirm inbound webhook delivery from Meta dashboard.",
    ],
    composio: [
      "Create account at app.composio.dev and generate API key.",
      "Paste key and enable Composio in ZeroClaw config.",
      "Connect target SaaS tools in Composio dashboard.",
      "Restart daemon so tools are loaded in runtime.",
    ],
  };

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

        <IntegrationCard
          name="Telegram"
          enabled={form.telegramEnabled}
          expanded={!!expanded.telegram}
          onExpandToggle={() => toggleExpanded("telegram")}
          onToggle={(next) => {
            setForm((prev) => ({ ...prev, telegramEnabled: next }));
            setEnabled("telegram", next);
          }}
          instructions={guides.telegram}
        >
          <SecretField label="Bot token" value={form.telegramBotToken} onChangeText={(value) => setForm((prev) => ({ ...prev, telegramBotToken: value }))} />
          <SecretField label="Chat ID" value={form.telegramChatId} onChangeText={(value) => setForm((prev) => ({ ...prev, telegramChatId: value }))} />
        </IntegrationCard>

        <IntegrationCard
          name="Discord"
          enabled={form.discordEnabled}
          expanded={!!expanded.discord}
          onExpandToggle={() => toggleExpanded("discord")}
          onToggle={(next) => {
            setForm((prev) => ({ ...prev, discordEnabled: next }));
            setEnabled("discord", next);
          }}
          instructions={guides.discord}
        >
          <SecretField label="Bot token" value={form.discordBotToken} onChangeText={(value) => setForm((prev) => ({ ...prev, discordBotToken: value }))} />
        </IntegrationCard>

        <IntegrationCard
          name="Slack"
          enabled={form.slackEnabled}
          expanded={!!expanded.slack}
          onExpandToggle={() => toggleExpanded("slack")}
          onToggle={(next) => {
            setForm((prev) => ({ ...prev, slackEnabled: next }));
            setEnabled("slack", next);
          }}
          instructions={guides.slack}
        >
          <SecretField label="Bot token" value={form.slackBotToken} onChangeText={(value) => setForm((prev) => ({ ...prev, slackBotToken: value }))} />
        </IntegrationCard>

        <IntegrationCard
          name="WhatsApp"
          enabled={form.whatsappEnabled}
          expanded={!!expanded.whatsapp}
          onExpandToggle={() => toggleExpanded("whatsapp")}
          onToggle={(next) => {
            setForm((prev) => ({ ...prev, whatsappEnabled: next }));
            setEnabled("whatsapp", next);
          }}
          instructions={guides.whatsapp}
        >
          <SecretField label="Access token" value={form.whatsappAccessToken} onChangeText={(value) => setForm((prev) => ({ ...prev, whatsappAccessToken: value }))} />
        </IntegrationCard>

        <IntegrationCard
          name="Composio"
          enabled={form.composioEnabled}
          expanded={!!expanded.composio}
          onExpandToggle={() => toggleExpanded("composio")}
          onToggle={(next) => {
            setForm((prev) => ({ ...prev, composioEnabled: next }));
            setEnabled("composio", next);
          }}
          instructions={guides.composio}
        >
          <SecretField label="API key" value={form.composioApiKey} onChangeText={(value) => setForm((prev) => ({ ...prev, composioApiKey: value }))} />
        </IntegrationCard>
      </ScrollView>
    </Screen>
  );
}
