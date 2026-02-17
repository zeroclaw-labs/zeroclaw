import AsyncStorage from "@react-native-async-storage/async-storage";
import { demoChats } from "./demoData";

const SEEDED_KEY = "guappa:demo:seeded:v1";
const ACTIVITY_KEY = "guappa:activity:v1";

const chatKeyFor = (projectId: string) => `guappa:chat:v1:${projectId}`;

export async function seedDemoDataOnce() {
  const seeded = await AsyncStorage.getItem(SEEDED_KEY);
  if (seeded === "true") return;

  const entries = Object.entries(demoChats);
  await Promise.all(entries.map(([projectId, messages]) => AsyncStorage.setItem(chatKeyFor(projectId), JSON.stringify(messages))));

  await AsyncStorage.setItem(
    ACTIVITY_KEY,
    JSON.stringify([
      {
        id: "act_demo_1",
        ts: Date.now() - 1000 * 60 * 12,
        title: "Generation complete",
        detail: "Shared Shopping List · snap_demo_002"
      },
      {
        id: "act_demo_2",
        ts: Date.now() - 1000 * 60 * 45,
        title: "Fork ready",
        detail: "Habit Streak"
      },
      {
        id: "act_demo_3",
        ts: Date.now() - 1000 * 60 * 140,
        title: "Preview updated",
        detail: "Studio Booking"
      }
    ])
  );

  await AsyncStorage.setItem(SEEDED_KEY, "true");
}
