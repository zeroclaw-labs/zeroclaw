/**
 * Provider list query (M4a, US-004).
 *
 * Reads `GET /api/providers` so the slot settings drawer's Advanced
 * tab can render a real provider dropdown rather than hardcoding the
 * provider zoo. ZeroClaw ships ~15 first-class providers; the gateway
 * answers with the subset the user has actually configured under
 * `[providers.models.*]`.
 *
 * Wire shape mirrors `crates/zeroclaw-gateway/src/api_providers.rs`:
 *   GET /api/providers -> ProviderListResponse
 *
 * Cached for 30s; the configured-provider set changes only when the
 * user edits config.toml or hits `/api/config/*`, so a longer
 * staleTime than the default 5s is justified.
 */
import { useQuery } from "@tanstack/react-query";
import { apiFetch } from "@/lib/apiFetch";

export interface ProviderInfo {
  id: string;
  display_name: string;
  model?: string;
  is_fallback: boolean;
}

export interface ProviderListResponse {
  providers: ProviderInfo[];
}

export const PROVIDERS_QUERY_KEY = ["providers"] as const;

async function fetchProviders(): Promise<ProviderListResponse> {
  return apiFetch<ProviderListResponse>("/api/providers");
}

export function useProviders() {
  return useQuery({
    queryKey: PROVIDERS_QUERY_KEY,
    queryFn: fetchProviders,
    staleTime: 30_000,
  });
}
