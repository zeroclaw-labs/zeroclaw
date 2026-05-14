import { createContext, useContext, type ReactNode } from "react";
import { useQuery } from "@tanstack/react-query";
import { apiFetch } from "@/lib/apiFetch";

/**
 * Shape of `GET /api/control-ui/config` (M3).
 *
 * Mirrors the `ControlUiBootstrapConfig` pattern OpenClaw uses at
 * `src/gateway/control-ui.ts`. Consumers read theme list + assistant
 * identity + server version once on boot and cache; anything that
 * changes at runtime (slot list, cost, health) goes through its own
 * React Query hook.
 */
export interface ControlUiBootstrapConfig {
  server_version: string;
  assistant_identity: {
    name: string;
    description?: string;
  };
  themes: {
    default_theme: "default" | "monochrome" | "contrast";
    default_mode: "light" | "dark";
  };
  max_chat_width_ch?: number;
}

const BootstrapContext = createContext<ControlUiBootstrapConfig | null>(null);

async function fetchBootstrap(): Promise<ControlUiBootstrapConfig> {
  return apiFetch<ControlUiBootstrapConfig>("/api/control-ui/config");
}

export function ControlUiBootstrapProvider({ children }: { children: ReactNode }) {
  const { data, isLoading, error } = useQuery({
    queryKey: ["control-ui-config"],
    queryFn: fetchBootstrap,
    // Bootstrap data only changes on gateway restart, so we can be
    // aggressive about staleness. Refetch on mount is still useful
    // across tab switches to detect a daemon redeploy.
    staleTime: 60 * 60 * 1000,
  });

  if (isLoading) {
    return (
      <div className="flex min-h-full items-center justify-center text-sm opacity-60">
        Loading…
      </div>
    );
  }
  if (error || !data) {
    return (
      <div className="flex min-h-full items-center justify-center text-sm text-red-600">
        Failed to load dashboard config: {String(error ?? "unknown error")}
      </div>
    );
  }

  return <BootstrapContext.Provider value={data}>{children}</BootstrapContext.Provider>;
}

/** Read the bootstrap snapshot. Must be called inside the provider. */
export function useControlUiBootstrap(): ControlUiBootstrapConfig {
  const ctx = useContext(BootstrapContext);
  if (!ctx) {
    throw new Error("useControlUiBootstrap called outside ControlUiBootstrapProvider");
  }
  return ctx;
}
