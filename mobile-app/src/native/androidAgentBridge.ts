import { NativeModules, Platform } from "react-native";

type NativeBridge = {
  executeToolAction(action: string, payload: Record<string, unknown>): Promise<string>;
  consumePendingEvents(): Promise<string[]>;
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
