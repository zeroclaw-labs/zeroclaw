// Shared tool-catalog loader. `ToolPicker` and `ToolPermissionGrid` both need
// the same flattened, group-tagged list of agent + CLI tools; this is the one
// place that fetches and caches it so the two components (and anything else
// that needs the catalog) stay in sync instead of hitting the network twice.

import { getTools, getCliTools } from "@/lib/api";
import {
  settleToolCatalogResult,
  type CatalogEntry,
  type ToolCatalogLoadResult,
} from "./toolCatalog.logic";

export type { CatalogEntry, CatalogLoadWarning, ToolCatalogLoadResult } from "./toolCatalog.logic";

// Process-wide cache so re-mounting a consumer (e.g. reopening the Cron
// modal, or switching config sections) doesn't re-hit the network. Keyed by
// agent alias (`''` = the gateway's default-agent listing): the agent-tools
// half is `getTools(agent)`, so a catalog bound to a specific agent (e.g. a
// channel's owning agent) caches that agent's real scoped catalog separately
// from the default. Each per-agent catalog is effectively static for the
// daemon's lifetime.
const catalogCache = new Map<string, CatalogEntry[]>();
const catalogInflight = new Map<string, Promise<ToolCatalogLoadResult>>();

/** Synchronous cache peek — `null` when nothing has been fetched yet for
 *  this agent. Lets a consumer seed its initial state without waiting on
 *  the `loadToolCatalog` promise when a previous mount already warmed it. */
export function peekToolCatalog(agent?: string): CatalogEntry[] | null {
  return catalogCache.get(agent ?? "") ?? null;
}

export function loadToolCatalogResult(agent?: string): Promise<ToolCatalogLoadResult> {
  const key = agent ?? "";
  const cached = catalogCache.get(key);
  if (cached) return Promise.resolve({ entries: cached, warnings: [] });
  const inflight = catalogInflight.get(key);
  if (inflight) return inflight;
  const promise = Promise.allSettled([getTools(agent), getCliTools()])
    .then(([toolsResult, cliToolsResult]) => {
      const result = settleToolCatalogResult(toolsResult, cliToolsResult);
      if (result.warnings.length === 0) {
        catalogCache.set(key, result.entries);
      }
      return result;
    })
    .finally(() => {
      catalogInflight.delete(key);
    });
  catalogInflight.set(key, promise);
  return promise;
}

export function loadToolCatalog(agent?: string): Promise<CatalogEntry[]> {
  return loadToolCatalogResult(agent).then((result) => result.entries);
}
