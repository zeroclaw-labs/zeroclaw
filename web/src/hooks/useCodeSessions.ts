import { useEffect, useState } from "react";
import { AcpWebSocketClient } from "@/lib/acp";
import type { Session } from "@/types/api";

export interface CodeSessionSummary extends Session {
  state: "running" | "idle";
  interaction_surface?: string;
  surface: "code";
}

/** Read-only observer of the daemon's canonical code-session list. */
export function useCodeSessions(enabled: boolean) {
  const [sessions, setSessions] = useState<CodeSessionSummary[]>([]);
  const [error, setError] = useState(false);
  const [loaded, setLoaded] = useState(false);
  useEffect(() => {
    if (!enabled) return;
    let stopped = false;
    let initialized = false;
    let pending = false;
    let reconnect: ReturnType<typeof setTimeout> | undefined;
    const refresh = async () => {
      if (stopped || !initialized || pending || document.hidden) return;
      pending = true;
      try {
        const result = await client.request<{
          sessions: Omit<CodeSessionSummary, "surface">[];
        }>("session/list-acp");
        if (!stopped) {
          setSessions(
            result.sessions
              .filter(
                (session) => session.interaction_surface === "zerocode_code",
              )
              .map((session) => ({ ...session, surface: "code" })),
          );
          setError(false);
          setLoaded(true);
        }
      } catch {
        if (!stopped) {
          setError(true);
          setLoaded(true);
        }
      } finally {
        pending = false;
      }
    };
    const client = new AcpWebSocketClient(
      {
        onOpen: () => {
          void client
            .request("initialize", { protocol_version: 1 })
            .then(() => {
              initialized = true;
              void refresh();
            })
            .catch(() => {
              if (!stopped) {
                setError(true);
                setLoaded(true);
              }
            });
        },
        onClose: () => {
          initialized = false;
          if (!stopped) {
            setError(true);
            reconnect = setTimeout(() => client.connect(), 3000);
          }
        },
      },
      "/ws/code",
    );
    client.connect();
    const interval = setInterval(() => void refresh(), 5000);
    document.addEventListener("visibilitychange", refresh);
    return () => {
      stopped = true;
      clearInterval(interval);
      clearTimeout(reconnect);
      document.removeEventListener("visibilitychange", refresh);
      client.disconnect();
    };
  }, [enabled]);
  return {
    sessions: enabled && !error ? sessions : [],
    error: enabled && error,
    loaded: !enabled || loaded,
  };
}
