import AsyncStorage from "@react-native-async-storage/async-storage";

const keyFor = (projectId: string) => `guappa:pending-agent-start:v1:${projectId}`;

export async function setPendingAgentStart(projectId: string, prompt: string) {
  const trimmed = prompt.trim();
  if (!trimmed) return;
  await AsyncStorage.setItem(keyFor(projectId), trimmed);
}

export async function popPendingAgentStart(projectId: string): Promise<string> {
  const key = keyFor(projectId);
  const raw = await AsyncStorage.getItem(key);
  if (!raw) return "";
  await AsyncStorage.removeItem(key);
  return raw;
}
