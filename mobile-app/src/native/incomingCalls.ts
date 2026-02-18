import { DeviceEventEmitter, Platform } from "react-native";

import { consumePendingAndroidAgentEvents } from "./androidAgentBridge";

export type IncomingCallEvent = {
  event: "incoming_call";
  state: string;
  phone: string;
  ts: number;
};

export type IncomingSmsEvent = {
  event: "incoming_sms";
  address: string;
  body: string;
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

function parseIncomingSmsEvent(raw: string): IncomingSmsEvent | null {
  try {
    const parsed = JSON.parse(raw) as Partial<IncomingSmsEvent>;
    if (parsed?.event !== "incoming_sms") return null;
    return {
      event: "incoming_sms",
      address: String(parsed.address || ""),
      body: String(parsed.body || ""),
      ts: Number(parsed.ts || Date.now()),
    };
  } catch {
    return null;
  }
}

export function subscribeIncomingDeviceEvents(onCall: (event: IncomingCallEvent) => void, onSms: (event: IncomingSmsEvent) => void): () => void {
  if (Platform.OS !== "android") {
    return () => {};
  }

  const callSub = DeviceEventEmitter.addListener("mobileclaw_incoming_call", (raw: string) => {
    const parsed = parseIncomingCallEvent(String(raw || ""));
    if (parsed) onCall(parsed);
  });

  const smsSub = DeviceEventEmitter.addListener("mobileclaw_incoming_sms", (raw: string) => {
    const parsed = parseIncomingSmsEvent(String(raw || ""));
    if (parsed) onSms(parsed);
  });

  void consumePendingAndroidAgentEvents().then((events) => {
    for (const raw of events) {
      const call = parseIncomingCallEvent(raw);
      if (call) {
        onCall(call);
        continue;
      }
      const sms = parseIncomingSmsEvent(raw);
      if (sms) {
        onSms(sms);
      }
    }
  });

  return () => {
    callSub.remove();
    smsSub.remove();
  };
}
