import { NativeModules, Platform } from "react-native";

type NativeBridge = {
  executeToolAction(action: string, payload: Record<string, unknown>): Promise<string>;
  consumePendingEvents(): Promise<string[]>;
  configureRuntimeBridge(config: Record<string, unknown>): Promise<string>;
  getRuntimeBridgeStatus(): Promise<string>;
};

const moduleRef = (NativeModules.AndroidAgentTools || null) as NativeBridge | null;

export async function executeAndroidToolAction(action: string, payload: Record<string, unknown>): Promise<unknown> {
  if (Platform.OS !== "android") {
    throw new Error("Android tool execution is available on Android only.");
  }
  if (!moduleRef?.executeToolAction) {
    throw new Error("Android native tool bridge is not available.");
  }

  const raw = await moduleRef.executeToolAction(action, payload);
  if (!raw) return null;
  try {
    return JSON.parse(String(raw)) as unknown;
  } catch {
    return raw;
  }
}

export async function consumePendingAndroidAgentEvents(): Promise<string[]> {
  if (Platform.OS !== "android") return [];
  if (!moduleRef?.consumePendingEvents) return [];
  const events = await moduleRef.consumePendingEvents();
  return Array.isArray(events) ? events.map((entry) => String(entry)) : [];
}

export async function configureAndroidRuntimeBridge(config: {
  telegramEnabled: boolean;
  telegramBotToken: string;
  alwaysOnMode: boolean;
  incomingCallHooks: boolean;
  incomingSmsHooks: boolean;
  enabledToolIds: string[];
  runtimeProvider: string;
  runtimeModel: string;
  runtimeApiUrl: string;
  runtimeApiKey: string;
  runtimeTemperature: number;
}): Promise<void> {
  if (Platform.OS !== "android") return;
  if (!moduleRef?.configureRuntimeBridge) return;
  await moduleRef.configureRuntimeBridge(config);
}

export async function getAndroidRuntimeBridgeStatus(): Promise<{
  queueSize: number;
  alwaysOn: boolean;
  runtimeReady: boolean;
  daemonUp: boolean;
  telegramSeenCount: number;
  webhookSuccessCount: number;
  webhookFailCount: number;
  lastEventNote: string;
} | null> {
  if (Platform.OS !== "android") return null;
  if (!moduleRef?.getRuntimeBridgeStatus) return null;

  const raw = await moduleRef.getRuntimeBridgeStatus();
  try {
    const parsed = JSON.parse(String(raw)) as {
      queue_size?: number;
      always_on?: boolean;
      runtime_ready?: boolean;
      daemon_up?: boolean;
      telegram_seen_count?: number;
      webhook_success_count?: number;
      webhook_fail_count?: number;
      last_event_note?: string;
    };
    return {
      queueSize: Number(parsed.queue_size || 0),
      alwaysOn: Boolean(parsed.always_on),
      runtimeReady: Boolean(parsed.runtime_ready),
      daemonUp: Boolean(parsed.daemon_up),
      telegramSeenCount: Number(parsed.telegram_seen_count || 0),
      webhookSuccessCount: Number(parsed.webhook_success_count || 0),
      webhookFailCount: Number(parsed.webhook_fail_count || 0),
      lastEventNote: String(parsed.last_event_note || ""),
    };
  } catch {
    return null;
  }
}
