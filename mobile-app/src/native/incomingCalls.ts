import { DeviceEventEmitter, Platform } from "react-native";

import { consumePendingAndroidAgentEvents } from "./androidAgentBridge";

export type IncomingCallEvent = {
  event: "incoming_call";
  state: string;
  phone: string;
  ts: number;
};

function parseIncomingCallEvent(raw: string): IncomingCallEvent | null {
  try {
    const parsed = JSON.parse(raw) as Partial<IncomingCallEvent>;
    if (parsed?.event !== "incoming_call") return null;
    return {
      event: "incoming_call",
      state: String(parsed.state || "unknown"),
      phone: String(parsed.phone || ""),
      ts: Number(parsed.ts || Date.now()),
    };
  } catch {
    return null;
  }
}

export function subscribeIncomingCalls(onEvent: (event: IncomingCallEvent) => void): () => void {
  if (Platform.OS !== "android") {
    return () => {};
  }

  const sub = DeviceEventEmitter.addListener("mobileclaw_incoming_call", (raw: string) => {
    const parsed = parseIncomingCallEvent(String(raw || ""));
    if (parsed) onEvent(parsed);
  });

  void consumePendingAndroidAgentEvents().then((events) => {
    for (const raw of events) {
      const parsed = parseIncomingCallEvent(raw);
      if (parsed) onEvent(parsed);
    }
  });

  return () => sub.remove();
}
