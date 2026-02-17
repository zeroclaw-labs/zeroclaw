import React, { useEffect, useMemo, useRef, useState } from "react";
import { ScrollView, TextInput, View, Pressable, Modal } from "react-native";
import { useNavigation } from "@react-navigation/native";
import { Ionicons } from "@expo/vector-icons";

import { Screen } from "../../../ui/primitives/Screen";
import { Text } from "../../../ui/primitives/Text";
import { theme } from "../../../ui/theme";
import { addActivity } from "../../state/activity";
import { fetchOpenRouterModels } from "../../api/mobileclaw";
import {
  type AgentRuntimeConfig,
  type ProviderId,
  loadAgentConfig,
  saveAgentConfig,
  DEFAULT_AGENT_CONFIG,
} from "../../state/mobileclaw";

type ProviderPreset = {
  id: ProviderId;
  title: string;
  endpoint: string;
  model: string;
  supportsOauthToken: boolean;
  docsHint: string;
};

const MODELS_BY_PROVIDER: Record<ProviderId, string[]> = {
  ollama: ["gpt-oss:20b", "qwen2.5-coder:14b", "llama3.1:8b"],
  openrouter: ["minimax/minimax-m2.5"],
  openai: ["gpt-4.1-mini", "gpt-4.1", "gpt-4o-mini"],
  anthropic: ["claude-3-5-sonnet-latest", "claude-3-5-haiku-latest", "claude-3-opus-latest"],
  gemini: ["gemini-1.5-pro", "gemini-1.5-flash", "gemini-2.0-flash-exp"],
  copilot: ["gpt-4o-mini", "gpt-4.1", "claude-3-5-sonnet"],
};

const PROVIDERS: ProviderPreset[] = [
  { id: "ollama", title: "Ollama (local)", endpoint: "http://10.0.2.2:11434", model: "gpt-oss:20b", supportsOauthToken: false, docsHint: "Local Ollama on host machine." },
  { id: "openrouter", title: "OpenRouter", endpoint: "https://openrouter.ai/api/v1", model: "minimax/minimax-m2.5", supportsOauthToken: false, docsHint: "Use OpenRouter API key." },
  { id: "openai", title: "OpenAI", endpoint: "https://api.openai.com/v1", model: "gpt-4.1-mini", supportsOauthToken: true, docsHint: "API key or OAuth access token." },
  { id: "anthropic", title: "Anthropic", endpoint: "https://api.anthropic.com/v1", model: "claude-3-5-sonnet-latest", supportsOauthToken: true, docsHint: "Anthropic key or supported OAuth token." },
  { id: "gemini", title: "Google Gemini", endpoint: "https://generativelanguage.googleapis.com/v1beta", model: "gemini-1.5-pro", supportsOauthToken: true, docsHint: "Gemini API key or OAuth token." },
  { id: "copilot", title: "GitHub Copilot", endpoint: "https://api.githubcopilot.com", model: "gpt-4o-mini", supportsOauthToken: true, docsHint: "Token for Copilot-enabled account." },
];

function GlassCard({ children }: { children: React.ReactNode }) {
  return (
    <View style={{ padding: theme.spacing.lg, borderRadius: theme.radii.xl, backgroundColor: theme.colors.surface.raised, borderWidth: 1, borderColor: theme.colors.stroke.subtle, gap: theme.spacing.sm }}>
      {children}
    </View>
  );
}

function LabeledInput(props: {
  label: string;
  value: string;
  onChangeText: (value: string) => void;
  secureTextEntry?: boolean;
  testID?: string;
  placeholder?: string;
}) {
  return (
    <View style={{ gap: 6 }}>
      <Text variant="label">{props.label}</Text>
      <TextInput
        testID={props.testID}
        value={props.value}
        onChangeText={props.onChangeText}
        secureTextEntry={props.secureTextEntry}
        placeholder={props.placeholder}
        placeholderTextColor={theme.colors.alpha.textPlaceholder}
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

export function SettingsScreen() {
  const navigation = useNavigation<any>();
  const [form, setForm] = useState<AgentRuntimeConfig>(DEFAULT_AGENT_CONFIG);
  const [saveStatus, setSaveStatus] = useState("Loading...");
  const [providerPickerOpen, setProviderPickerOpen] = useState(false);
  const [modelPickerOpen, setModelPickerOpen] = useState(false);
  const [modelSearch, setModelSearch] = useState("");
  const [openRouterModels, setOpenRouterModels] = useState<string[]>(MODELS_BY_PROVIDER.openrouter);
  const [openRouterLoading, setOpenRouterLoading] = useState(false);
  const hydratedRef = useRef(false);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      const loaded = await loadAgentConfig();
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

  const selected = useMemo(() => PROVIDERS.find((p) => p.id === form.provider) || PROVIDERS[0], [form.provider]);
  const providerModels = useMemo(() => {
    if (form.provider === "openrouter") return openRouterModels;
    return MODELS_BY_PROVIDER[form.provider] || [];
  }, [form.provider, openRouterModels]);
  const filteredModels = useMemo(() => {
    const query = modelSearch.trim().toLowerCase();
    if (!query) return providerModels;
    return providerModels.filter((m) => m.toLowerCase().includes(query));
  }, [providerModels, modelSearch]);
  const onProvider = (provider: ProviderId) => {
    const preset = PROVIDERS.find((p) => p.id === provider) || PROVIDERS[0];
    const defaultModel = MODELS_BY_PROVIDER[provider]?.[0] || preset.model;
    setForm((prev) => ({ ...prev, provider, apiUrl: preset.endpoint, model: defaultModel, authMode: "api_key" }));
    void addActivity({ kind: "action", source: "settings", title: "Provider changed", detail: provider });
    setProviderPickerOpen(false);
  };

  useEffect(() => {
    if (!hydratedRef.current) return;
    if (form.provider !== "openrouter") return;
    let cancelled = false;
    setOpenRouterLoading(true);

    (async () => {
      try {
        const token = form.apiKey || form.oauthAccessToken;
        const models = await fetchOpenRouterModels(token);
        if (!cancelled && models.length > 0) {
          setOpenRouterModels(models);
        }
      } catch (error) {
        if (!cancelled) {
          void addActivity({
            kind: "log",
            source: "settings",
            title: "OpenRouter model list sync failed",
            detail: error instanceof Error ? error.message : "Unknown error",
          });
        }
      } finally {
        if (!cancelled) setOpenRouterLoading(false);
      }
    })();

    return () => {
      cancelled = true;
    };
  }, [form.provider, form.apiKey, form.oauthAccessToken]);

  useEffect(() => {
    if (!hydratedRef.current) return;
    const timer = setTimeout(() => {
      const normalized = {
        ...form,
        temperature: Math.max(0, Math.min(2, Number(form.temperature) || 0.1)),
      };
      void saveAgentConfig(normalized);
      setSaveStatus("Saved locally");
    }, 300);
    return () => clearTimeout(timer);
  }, [form]);

  return (
    <Screen>
      <ScrollView contentContainerStyle={{ paddingHorizontal: theme.spacing.lg, paddingTop: theme.spacing.xl, paddingBottom: 140, gap: theme.spacing.lg }}>
        <View>
          <Text testID="screen-settings" variant="display">Settings</Text>
          <Text variant="muted" style={{ marginTop: theme.spacing.xs }}>
            Provider, credentials, model, temperature, and voice key.
          </Text>
          <Text variant="mono" style={{ marginTop: theme.spacing.sm, color: theme.colors.base.textMuted }}>
            {saveStatus}
          </Text>
        </View>

        <GlassCard>
          <Text variant="title">Provider</Text>
          <Text variant="muted">Dropdown list - tap to choose provider.</Text>
          <Pressable
            testID="provider-dropdown"
            onPress={() => setProviderPickerOpen(true)}
            style={{
              paddingVertical: 12,
              paddingHorizontal: 14,
              borderRadius: theme.radii.lg,
              borderWidth: 1,
              borderColor: theme.colors.stroke.subtle,
              backgroundColor: theme.colors.surface.panel,
            }}
          >
            <View style={{ flexDirection: "row", alignItems: "center", justifyContent: "space-between" }}>
              <Text variant="bodyMedium">{selected.title}</Text>
              <Ionicons name="chevron-down" size={18} color={theme.colors.base.textMuted} />
            </View>
          </Pressable>

          <Pressable
            testID="model-dropdown"
            onPress={() => setModelPickerOpen(true)}
            style={{
              paddingVertical: 12,
              paddingHorizontal: 14,
              borderRadius: theme.radii.lg,
              borderWidth: 1,
              borderColor: theme.colors.stroke.subtle,
              backgroundColor: theme.colors.surface.panel,
            }}
          >
            <Text variant="label">Model</Text>
            <Text variant="muted" style={{ marginTop: 4 }}>
              Searchable dropdown list
            </Text>
            <Text variant="bodyMedium" style={{ marginTop: 6 }}>
              {form.model}
            </Text>
            <View style={{ position: "absolute", right: 12, top: 12 }}>
              <Ionicons name="chevron-down" size={18} color={theme.colors.base.textMuted} />
            </View>
          </Pressable>
          <LabeledInput testID="settings-endpoint" label="Endpoint" value={form.apiUrl} onChangeText={(value) => setForm((prev) => ({ ...prev, apiUrl: value }))} />
          <LabeledInput
            testID="settings-api-key"
            label={form.provider === "openrouter" ? "OpenRouter API key" : "API key"}
            placeholder="Enter API key"
            value={form.apiKey}
            onChangeText={(value) => setForm((prev) => ({ ...prev, apiKey: value, authMode: "api_key" }))}
            secureTextEntry
          />
          {selected.supportsOauthToken ? (
            <>
              <LabeledInput testID="settings-oauth-access-token" label="OAuth access token (optional)" value={form.oauthAccessToken} onChangeText={(value) => setForm((prev) => ({ ...prev, oauthAccessToken: value, authMode: value.trim() ? "oauth_token" : "api_key" }))} secureTextEntry />
              <LabeledInput label="OAuth refresh token (optional)" value={form.oauthRefreshToken} onChangeText={(value) => setForm((prev) => ({ ...prev, oauthRefreshToken: value }))} secureTextEntry />
              <LabeledInput label="OAuth expires at (epoch ms)" value={String(form.oauthExpiresAtMs || "")} onChangeText={(value) => setForm((prev) => ({ ...prev, oauthExpiresAtMs: Number(value) || 0 }))} />
              {form.provider === "openai" ? <LabeledInput label="Account id (optional)" value={form.accountId} onChangeText={(value) => setForm((prev) => ({ ...prev, accountId: value }))} /> : null}
              {form.provider === "copilot" ? <LabeledInput label="Enterprise URL (optional)" value={form.enterpriseUrl} onChangeText={(value) => setForm((prev) => ({ ...prev, enterpriseUrl: value }))} /> : null}
            </>
          ) : null}
          <LabeledInput label="Temperature (0-2)" value={String(form.temperature)} onChangeText={(value) => setForm((prev) => ({ ...prev, temperature: Number(value) || 0.1 }))} />
          {form.provider === "openrouter" ? (
            <Text variant="muted">
              {openRouterLoading
                ? "Refreshing OpenRouter models..."
                : `OpenRouter model catalog loaded: ${openRouterModels.length} models`}
            </Text>
          ) : null}
          <Text variant="muted">{selected.docsHint}</Text>
        </GlassCard>

        <GlassCard>
          <Text variant="title">Advanced</Text>
          <Pressable
            testID="open-security-screen"
            onPress={() => {
              const root = navigation.getParent("root-stack");
              if (root) {
                root.navigate("Security");
                return;
              }
              navigation.navigate("Security");
            }}
            style={{
              paddingVertical: 12,
              paddingHorizontal: 14,
              borderRadius: theme.radii.lg,
              borderWidth: 1,
              borderColor: theme.colors.stroke.subtle,
              backgroundColor: theme.colors.surface.panel,
            }}
          >
            <Text variant="bodyMedium">Open security controls</Text>
          </Pressable>
        </GlassCard>

        <GlassCard>
          <Text variant="title">Voice Mode</Text>
          <Text variant="muted">Deepgram API key is required for chat voice transcription.</Text>
          <LabeledInput label="Deepgram API key" testID="deepgram-key-input" value={form.deepgramApiKey} onChangeText={(value) => setForm((prev) => ({ ...prev, deepgramApiKey: value }))} secureTextEntry />
        </GlassCard>

        <Modal animationType="slide" transparent visible={providerPickerOpen} onRequestClose={() => setProviderPickerOpen(false)}>
          <View style={{ flex: 1, justifyContent: "flex-end", backgroundColor: theme.colors.alpha.scrim }}>
            <View style={{ maxHeight: "70%", padding: theme.spacing.lg, borderTopLeftRadius: theme.radii.xl, borderTopRightRadius: theme.radii.xl, backgroundColor: theme.colors.base.background, gap: theme.spacing.sm }}>
              <Text variant="title">Select provider</Text>
              <ScrollView contentContainerStyle={{ gap: theme.spacing.sm }}>
                {PROVIDERS.map((provider) => (
                  <Pressable
                    key={provider.id}
                    testID={`provider-option-${provider.id}`}
                    onPress={() => onProvider(provider.id)}
                    style={{
                      paddingVertical: 12,
                      paddingHorizontal: 14,
                      borderRadius: theme.radii.lg,
                      borderWidth: 1,
                      borderColor: form.provider === provider.id ? theme.colors.base.primary : theme.colors.stroke.subtle,
                      backgroundColor: form.provider === provider.id ? theme.colors.alpha.userBubbleBg : theme.colors.surface.panel,
                    }}
                  >
                    <Text variant="bodyMedium">{provider.title}</Text>
                  </Pressable>
                ))}
              </ScrollView>
              <Pressable onPress={() => setProviderPickerOpen(false)} style={{ paddingVertical: 12, borderRadius: theme.radii.lg, alignItems: "center", backgroundColor: theme.colors.surface.panel }}>
                <Text variant="bodyMedium">Close</Text>
              </Pressable>
            </View>
          </View>
        </Modal>

        <Modal animationType="slide" transparent visible={modelPickerOpen} onRequestClose={() => setModelPickerOpen(false)}>
          <View style={{ flex: 1, justifyContent: "flex-end", backgroundColor: theme.colors.alpha.scrim }}>
            <View style={{ maxHeight: "80%", padding: theme.spacing.lg, borderTopLeftRadius: theme.radii.xl, borderTopRightRadius: theme.radii.xl, backgroundColor: theme.colors.base.background, gap: theme.spacing.sm }}>
              <Text variant="title">Select model</Text>
              <TextInput
                testID="model-search-input"
                value={modelSearch}
                onChangeText={setModelSearch}
                placeholder="Search models"
                placeholderTextColor={theme.colors.alpha.textPlaceholder}
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
              <ScrollView contentContainerStyle={{ gap: theme.spacing.sm }}>
                {filteredModels.map((modelName) => (
                  <Pressable
                    key={modelName}
                    testID={`model-option-${modelName.replace(/[^a-zA-Z0-9]/g, "-")}`}
                    onPress={() => {
                      setForm((prev) => ({ ...prev, model: modelName }));
                      setModelPickerOpen(false);
                    }}
                    style={{
                      paddingVertical: 12,
                      paddingHorizontal: 14,
                      borderRadius: theme.radii.lg,
                      borderWidth: 1,
                      borderColor: form.model === modelName ? theme.colors.base.secondary : theme.colors.stroke.subtle,
                      backgroundColor: form.model === modelName ? theme.colors.surface.glass : theme.colors.surface.panel,
                    }}
                  >
                    <Text variant="bodyMedium">{modelName}</Text>
                  </Pressable>
                ))}
                {filteredModels.length === 0 ? <Text variant="muted">No matches</Text> : null}
              </ScrollView>
              <Pressable onPress={() => setModelPickerOpen(false)} style={{ paddingVertical: 12, borderRadius: theme.radii.lg, alignItems: "center", backgroundColor: theme.colors.surface.panel }}>
                <Text variant="bodyMedium">Close</Text>
              </Pressable>
            </View>
          </View>
        </Modal>
      </ScrollView>
    </Screen>
  );
}
