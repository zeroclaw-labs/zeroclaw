import AsyncStorage from "@react-native-async-storage/async-storage";
import React, { createContext, useCallback, useContext, useMemo, useState } from "react";

export type ActivityItem = {
  id: string;
  ts: number;
  kind: "action" | "message" | "log";
  source: "chat" | "settings" | "integrations" | "device" | "security";
  title: string;
  detail?: string;
};

const KEY = "mobileclaw:activity:v1";

async function readAll(): Promise<ActivityItem[]> {
  const raw = await AsyncStorage.getItem(KEY);
  if (!raw) return [];
  const data = JSON.parse(raw) as ActivityItem[];
  return Array.isArray(data) ? data : [];
}

async function writeAll(items: ActivityItem[]) {
  await AsyncStorage.setItem(KEY, JSON.stringify(items.slice(0, 200)));
}

export async function addActivity(input: {
  kind?: ActivityItem["kind"];
  source?: ActivityItem["source"];
  title: string;
  detail?: string;
}) {
  const next: ActivityItem = {
    id: `act_${Date.now()}_${Math.floor(Math.random() * 10000)}`,
    ts: Date.now(),
    kind: input.kind ?? "log",
    source: input.source ?? "chat",
    title: input.title,
    detail: input.detail
  };
  const existing = await readAll();
  await writeAll([next, ...existing]);
}

type ActivityApi = {
  items: ActivityItem[];
  refresh: () => Promise<void>;
};

const ActivityContext = createContext<ActivityApi | null>(null);

export function ActivityProvider({ children }: { children: React.ReactNode }) {
  const [items, setItems] = useState<ActivityItem[]>([]);

  const refresh = useCallback(async () => {
    const all = await readAll();
    setItems(all);
  }, []);

  const value = useMemo(() => ({ items, refresh }), [items, refresh]);
  return <ActivityContext.Provider value={value}>{children}</ActivityContext.Provider>;
}

export function useActivity(): ActivityApi {
  const ctx = useContext(ActivityContext);
  if (!ctx) {
    return {
      items: [],
      refresh: async () => {}
    };
  }
  return ctx;
}
