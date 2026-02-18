import AsyncStorage from "@react-native-async-storage/async-storage";

export type ChatMessage = {
  id: string;
  role: "user" | "assistant";
  text: string;
  ts: number;
  imageUri?: string;
  meta?: {
    snapshot_id?: string;
  };
};

const KEY = "mobileclaw:chat:v1";

function sanitizeAssistantArtifacts(text: string): string {
  const value = String(text || "");
  return value
    .replace(/<[^>]*system\s*[-_ ]?reminder[^>]*>[\s\S]*?<\/[^>]*system\s*[-_ ]?reminder\s*>/gi, "")
    .replace(/<[^>]*system\s*[-_ ]?reminder[^>]*>[\s\S]*$/gi, "")
    .replace(/<\s*system-reminder\b[^>]*>[\s\S]*?<\s*\/\s*system-reminder\s*>/gi, "")
    .replace(/<\s*system-reminder\b[^>]*>[\s\S]*$/gi, "")
    .trim();
}

function sanitizeMessage(message: ChatMessage): ChatMessage {
  if (message.role !== "assistant") return message;
  return {
    ...message,
    text: sanitizeAssistantArtifacts(message.text),
  };
}

export async function loadChat(_legacyProjectId?: string): Promise<ChatMessage[]> {
  const raw = await AsyncStorage.getItem(KEY);
  if (!raw) return [];
  const data = JSON.parse(raw) as ChatMessage[];
  return Array.isArray(data) ? data.map(sanitizeMessage) : [];
}

export async function appendChat(messageOrProjectId: ChatMessage | string, maybeMessage?: ChatMessage) {
  const message = typeof messageOrProjectId === "string" ? maybeMessage : messageOrProjectId;
  if (!message) return;
  const existing = await loadChat();
  const next = [...existing, sanitizeMessage(message)].slice(-200);
  await AsyncStorage.setItem(KEY, JSON.stringify(next));
}

export async function clearChat() {
  await AsyncStorage.removeItem(KEY);
}
