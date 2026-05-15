/**
 * React Query hooks for the Overview page (M5.0, US-002).
 *
 * Each hook hits one existing gateway endpoint. The Overview cards
 * need counts plus one headline value (next-run, most-recent), so the
 * types here include only the fields the cards read — when M5.1 ships
 * the Memory deep page it will widen `MemoryEntry` for its own needs.
 *
 * Backend contracts:
 *   GET /api/memory       → { entries: MemoryEntry[] }
 *   GET /api/cron         → { jobs: CronJob[] }
 *   GET /api/integrations → { integrations: IntegrationEntry[] }
 *   GET /api/tools        → { tools: ToolSpec[] }
 */
import { useQuery } from "@tanstack/react-query";
import { apiFetch } from "@/lib/apiFetch";

// ── Memory ─────────────────────────────────────────────────────────

export interface MemoryEntry {
  key: string;
  content: string;
  timestamp: string;
}

export interface MemoryListResponse {
  entries: MemoryEntry[];
}

export const MEMORY_OVERVIEW_KEY = ["overview", "memory"] as const;

export function useMemoryOverview() {
  return useQuery({
    queryKey: MEMORY_OVERVIEW_KEY,
    queryFn: () => apiFetch<MemoryListResponse>("/api/memory"),
    // Memory rarely changes between Overview visits.
    staleTime: 30_000,
  });
}

// ── Cron ───────────────────────────────────────────────────────────

export interface CronJob {
  id: string;
  name?: string | null;
  expression: string;
  next_run: string;
  enabled: boolean;
  last_status?: string | null;
}

export interface CronListResponse {
  jobs: CronJob[];
}

export const CRONS_OVERVIEW_KEY = ["overview", "crons"] as const;

export function useCronsOverview() {
  return useQuery({
    queryKey: CRONS_OVERVIEW_KEY,
    queryFn: () => apiFetch<CronListResponse>("/api/cron"),
    staleTime: 15_000,
  });
}

// ── Integrations (the "MCP" section in plan §5) ────────────────────

export type IntegrationStatus = "Available" | "Active";

export interface IntegrationEntry {
  name: string;
  description: string;
  category: string;
  status: IntegrationStatus;
}

export interface IntegrationsListResponse {
  integrations: IntegrationEntry[];
}

export const INTEGRATIONS_OVERVIEW_KEY = ["overview", "integrations"] as const;

export function useIntegrationsOverview() {
  return useQuery({
    queryKey: INTEGRATIONS_OVERVIEW_KEY,
    queryFn: () => apiFetch<IntegrationsListResponse>("/api/integrations"),
    staleTime: 30_000,
  });
}

// ── Tools (the "Skills" section in plan §5) ────────────────────────

export interface ToolSpec {
  name: string;
  description: string;
}

export interface ToolsListResponse {
  tools: ToolSpec[];
}

export const TOOLS_OVERVIEW_KEY = ["overview", "tools"] as const;

export function useToolsOverview() {
  return useQuery({
    queryKey: TOOLS_OVERVIEW_KEY,
    queryFn: () => apiFetch<ToolsListResponse>("/api/tools"),
    // Tools registry is built at gateway start and never changes
    // mid-process.
    staleTime: 5 * 60_000,
  });
}

// ── Helpers ────────────────────────────────────────────────────────

/**
 * Returns the soonest upcoming enabled cron run, or null when no jobs
 * are enabled (or when every `next_run` is unparseable, which would
 * indicate a backend contract regression worth noticing).
 */
export function pickNextCronRun(jobs: CronJob[]): CronJob | null {
  let best: { job: CronJob; ts: number } | null = null;
  for (const job of jobs) {
    if (!job.enabled) continue;
    const ts = Date.parse(job.next_run);
    if (Number.isNaN(ts)) continue;
    if (!best || ts < best.ts) {
      best = { job, ts };
    }
  }
  return best?.job ?? null;
}

/**
 * Returns the most recent memory entry by `timestamp`. Backend
 * guarantees ISO 8601 strings via `DateTime<Utc>` / Rust ISO format.
 */
export function pickMostRecentMemory(
  entries: MemoryEntry[],
): MemoryEntry | null {
  let best: { entry: MemoryEntry; ts: number } | null = null;
  for (const entry of entries) {
    const ts = Date.parse(entry.timestamp);
    if (Number.isNaN(ts)) continue;
    if (!best || ts > best.ts) {
      best = { entry, ts };
    }
  }
  return best?.entry ?? null;
}
