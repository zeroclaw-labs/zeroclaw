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
import { applyRuntimeSupervisorConfig } from "../../runtime/supervisor";

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
  const [telegramLookupStatus, setTelegramLookupStatus] = useState("");
  const [telegramLookupBusy, setTelegramLookupBusy] = useState(false);
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
      "Create a bot via @BotFather, then paste your bot token below.",
      "Open Telegram chat with your bot and send any message (for example: /start).",
      "Tap Detect chat from Telegram updates to auto-fill Chat ID.",
      "Keep Telegram toggle ON. MobileClaw saves and applies runtime config automatically.",
    ],
    discord: [
      "Create a Discord app and bot in the Developer Portal.",
      "Enable Message Content intent and invite the bot to your server.",
      "Paste Bot token here and keep Discord toggle ON.",
      "Send a test message to verify the integration is working.",
    ],
    slack: [
      "Create a Slack app and add Bot token scopes.",
      "Install the app to workspace and copy Bot User OAuth token.",
      "Paste token here and keep Slack toggle ON.",
      "Invite the bot to a channel and send a quick test message.",
    ],
    whatsapp: [
      "Create a Meta app with WhatsApp Business integration.",
      "Set webhook endpoint to your ZeroClaw gateway URL.",
      "Paste access token here and keep WhatsApp toggle ON.",
      "Use Meta dashboard webhook test to confirm delivery.",
    ],
    composio: [
      "Create an account at app.composio.dev and generate API key.",
      "Paste key here and keep Composio toggle ON.",
      "Connect target SaaS tools in Composio dashboard.",
      "MobileClaw auto-saves and reloads runtime tool configuration.",
    ],
  };

  const detectTelegramChatId = async () => {
    const token = form.telegramBotToken.trim();
    if (!token) {
      setTelegramLookupStatus("Paste Bot token first.");
      return;
    }

    setTelegramLookupBusy(true);
    setTelegramLookupStatus("Checking latest Telegram updates...");

    try {
      const response = await fetch(`https://api.telegram.org/bot${token}/getUpdates`);
      const payload = (await response.json()) as {
        ok?: boolean;
        description?: string;
        result?: Array<{
          message?: { chat?: { id?: number | string } };
          edited_message?: { chat?: { id?: number | string } };
          channel_post?: { chat?: { id?: number | string } };
        }>;
      };

      if (!response.ok || !payload.ok) {
        const detail = payload.description ? `: ${payload.description}` : "";
        setTelegramLookupStatus(`Telegram API request failed${detail}`);
        return;
      }

      const updates = Array.isArray(payload.result) ? payload.result : [];
      const match = [...updates]
        .reverse()
        .map((update) => update.message?.chat?.id ?? update.edited_message?.chat?.id ?? update.channel_post?.chat?.id)
        .find((chatId) => chatId !== undefined && chatId !== null);

      if (match === undefined) {
        setTelegramLookupStatus("No chat found yet. Send any message to your bot and try again.");
        return;
      }

      const detectedChatId = String(match);
      setForm((prev) => ({ ...prev, telegramChatId: detectedChatId }));
      setTelegramLookupStatus(`Detected chat ID: ${detectedChatId}`);
      await addActivity({
        kind: "action",
        source: "integrations",
        title: "Telegram chat ID detected",
        detail: `chat_id=${detectedChatId}`,
      });
    } catch (error) {
      setTelegramLookupStatus(error instanceof Error ? `Failed to detect chat ID: ${error.message}` : "Failed to detect chat ID.");
    } finally {
      setTelegramLookupBusy(false);
    }
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
      void applyRuntimeSupervisorConfig("integrations_saved");
      void addActivity({
        kind: "action",
        source: "integrations",
        title: "Integrations updated",
        detail: "Runtime supervisor applied latest integration config",
      });
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
          <Pressable
            onPress={() => {
              void detectTelegramChatId();
            }}
            disabled={telegramLookupBusy}
            style={{
              paddingVertical: 10,
              borderRadius: theme.radii.lg,
              alignItems: "center",
              borderWidth: 1,
              borderColor: theme.colors.stroke.subtle,
              backgroundColor: telegramLookupBusy ? theme.colors.surface.panel : theme.colors.surface.raised,
            }}
          >
            <Text variant="bodyMedium">{telegramLookupBusy ? "Detecting chat..." : "Detect chat from Telegram updates"}</Text>
          </Pressable>
          {telegramLookupStatus ? (
            <Text variant="muted" style={{ marginTop: 2 }}>
              {telegramLookupStatus}
            </Text>
          ) : null}
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
