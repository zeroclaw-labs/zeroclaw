import AsyncStorage from "@react-native-async-storage/async-storage";

export type ChatMessage = {
  id: string;
  role: "user" | "assistant";
  text: string;
  ts: number;
  meta?: {
    snapshot_id?: string;
  };
};

const KEY = "mobileclaw:chat:v1";

export async function loadChat(_legacyProjectId?: string): Promise<ChatMessage[]> {
  const raw = await AsyncStorage.getItem(KEY);
  if (!raw) return [];
  const data = JSON.parse(raw) as ChatMessage[];
  return Array.isArray(data) ? data : [];
}

export async function appendChat(messageOrProjectId: ChatMessage | string, maybeMessage?: ChatMessage) {
  const message = typeof messageOrProjectId === "string" ? maybeMessage : messageOrProjectId;
  if (!message) return;
  const existing = await loadChat();
  const next = [...existing, message].slice(-200);
  await AsyncStorage.setItem(KEY, JSON.stringify(next));
}

export async function clearChat() {
  await AsyncStorage.removeItem(KEY);
}
