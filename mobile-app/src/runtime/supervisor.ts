import AsyncStorage from "@react-native-async-storage/async-storage";

import { config } from "../config";
import { addActivity } from "../state/activity";
import { loadIntegrationsConfig, loadSecurityConfig, type IntegrationsConfig, type SecurityConfig } from "../state/mobileclaw";

export type RuntimeSupervisorState = {
  status: "stopped" | "starting" | "healthy" | "degraded";
  startedAtMs: number | null;
  lastTransitionMs: number;
  restartCount: number;
  components: string[];
  missingConfig: string[];
  lastError: string | null;
  configHash: string;
};

const KEY = "mobileclaw:runtime-supervisor:v1";

const DEFAULT_STATE: RuntimeSupervisorState = {
  status: "stopped",
  startedAtMs: null,
  lastTransitionMs: Date.now(),
  restartCount: 0,
  components: [],
  missingConfig: [],
  lastError: null,
  configHash: "",
};

function signature(integrations: IntegrationsConfig, security: SecurityConfig): string {
  return JSON.stringify({
    integrations: {
      telegramEnabled: integrations.telegramEnabled,
      telegramBotToken: Boolean(integrations.telegramBotToken.trim()),
      telegramChatId: Boolean(integrations.telegramChatId.trim()),
      discordEnabled: integrations.discordEnabled,
      discordBotToken: Boolean(integrations.discordBotToken.trim()),
      slackEnabled: integrations.slackEnabled,
      slackBotToken: Boolean(integrations.slackBotToken.trim()),
      whatsappEnabled: integrations.whatsappEnabled,
      whatsappAccessToken: Boolean(integrations.whatsappAccessToken.trim()),
      composioEnabled: integrations.composioEnabled,
      composioApiKey: Boolean(integrations.composioApiKey.trim()),
    },
    hooks: {
      incomingCallHooks: security.incomingCallHooks,
      includeCallerNumber: security.includeCallerNumber,
    },
  });
}

function deriveComponents(integrations: IntegrationsConfig, security: SecurityConfig) {
  const components = ["daemon:zeroclaw"];
  const missing: string[] = [];

  if (integrations.telegramEnabled) {
    components.push("channel:telegram");
    if (!integrations.telegramBotToken.trim()) missing.push("telegram.bot_token");
    if (!integrations.telegramChatId.trim()) missing.push("telegram.chat_id");
  }
  if (integrations.discordEnabled) {
    components.push("channel:discord");
    if (!integrations.discordBotToken.trim()) missing.push("discord.bot_token");
  }
  if (integrations.slackEnabled) {
    components.push("channel:slack");
    if (!integrations.slackBotToken.trim()) missing.push("slack.bot_token");
  }
  if (integrations.whatsappEnabled) {
    components.push("channel:whatsapp");
    if (!integrations.whatsappAccessToken.trim()) missing.push("whatsapp.access_token");
  }
  if (integrations.composioEnabled) {
    components.push("tool:composio");
    if (!integrations.composioApiKey.trim()) missing.push("composio.api_key");
  }

  if (security.incomingCallHooks) {
    components.push("hook:incoming_call");
  }
  if (security.incomingSmsHooks) {
    components.push("hook:incoming_sms");
  }

  return { components, missing };
}

async function readState(): Promise<RuntimeSupervisorState> {
  const raw = await AsyncStorage.getItem(KEY);
  if (!raw) return DEFAULT_STATE;
  try {
    return { ...DEFAULT_STATE, ...(JSON.parse(raw) as Partial<RuntimeSupervisorState>) };
  } catch {
    return DEFAULT_STATE;
  }
}

async function writeState(state: RuntimeSupervisorState): Promise<void> {
  await AsyncStorage.setItem(KEY, JSON.stringify(state));
}

async function fetchHealthSnapshot(): Promise<{ ok: boolean; detail?: string }> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 4000);
  try {
    const res = await fetch(`${config.platformUrl}/health`, { signal: controller.signal });
    if (!res.ok) return { ok: false, detail: `health HTTP ${res.status}` };
    return { ok: true };
  } catch (error) {
    return {
      ok: false,
      detail: error instanceof Error ? error.message : "health request failed",
    };
  } finally {
    clearTimeout(timer);
  }
}

export async function getRuntimeSupervisorState(): Promise<RuntimeSupervisorState> {
  return readState();
}

export async function startRuntimeSupervisor(reason: string): Promise<RuntimeSupervisorState> {
  const state = await readState();
  if (state.status !== "stopped") {
    return applyRuntimeSupervisorConfig(`resume:${reason}`);
  }

  const next: RuntimeSupervisorState = {
    ...state,
    status: "starting",
    startedAtMs: Date.now(),
    lastTransitionMs: Date.now(),
    lastError: null,
  };
  await writeState(next);
  await addActivity({
    kind: "action",
    source: "runtime",
    title: "ZeroClaw runtime starting",
    detail: reason,
  });

  return applyRuntimeSupervisorConfig(`start:${reason}`);
}

export async function applyRuntimeSupervisorConfig(reason: string): Promise<RuntimeSupervisorState> {
  const [integrations, security, previous] = await Promise.all([
    loadIntegrationsConfig(),
    loadSecurityConfig(),
    readState(),
  ]);

  const { components, missing } = deriveComponents(integrations, security);
  const hash = signature(integrations, security);
  const health = await fetchHealthSnapshot();

  const status: RuntimeSupervisorState["status"] =
    missing.length > 0 || !health.ok ? "degraded" : "healthy";

  const next: RuntimeSupervisorState = {
    ...previous,
    status,
    components,
    missingConfig: missing,
    lastError: health.ok ? null : health.detail || "health check failed",
    lastTransitionMs: Date.now(),
    configHash: hash,
  };

  const changed = previous.configHash !== hash;
  const statusChanged = previous.status !== status;
  const componentsChanged = JSON.stringify(previous.components) !== JSON.stringify(components);

  if (changed || statusChanged || componentsChanged) {
    const detailParts = [
      `reason=${reason}`,
      `status=${status}`,
      `components=${components.join(", ") || "none"}`,
    ];
    if (missing.length) detailParts.push(`missing=${missing.join(", ")}`);
    if (!health.ok && health.detail) detailParts.push(`health=${health.detail}`);

    await addActivity({
      kind: "action",
      source: "runtime",
      title: status === "healthy" ? "ZeroClaw runtime healthy" : "ZeroClaw runtime degraded",
      detail: detailParts.join(" | "),
    });
  }

  await writeState(next);
  return next;
}

export async function stopRuntimeSupervisor(reason: string): Promise<RuntimeSupervisorState> {
  const previous = await readState();
  const next: RuntimeSupervisorState = {
    ...previous,
    status: "stopped",
    components: [],
    missingConfig: [],
    lastError: null,
    lastTransitionMs: Date.now(),
  };
  await writeState(next);
  await addActivity({
    kind: "action",
    source: "runtime",
    title: "ZeroClaw runtime stopped",
    detail: reason,
  });
  return next;
}

export async function reportRuntimeHookEvent(kind: "incoming_call" | "incoming_sms", detail: string): Promise<void> {
  await addActivity({
    kind: "action",
    source: "runtime",
    title: `Hook event: ${kind}`,
    detail,
  });
}
