import AsyncStorage from "@react-native-async-storage/async-storage";

const keyFor = (projectId: string) => `guappa:pending-draft:v1:${projectId}`;

export async function setPendingChatDraft(projectId: string, text: string) {
  const trimmed = text.trim();
  if (!trimmed) return;
  await AsyncStorage.setItem(keyFor(projectId), trimmed);
}

export async function popPendingChatDraft(projectId: string): Promise<string> {
  const key = keyFor(projectId);
  const raw = await AsyncStorage.getItem(key);
  if (!raw) return "";
  await AsyncStorage.removeItem(key);
  return raw;
}
