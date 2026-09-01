// Pure resolution logic for the agent model-provider picker, split out of
// `AgentContext` so the resolver-success and older-daemon fallback paths are
// unit-testable without a React render or a live backend.

/** Minimal shape of a `listProps` entry the scan reads. */
export interface PropEntry {
  path: string;
}

/** Data sources the picker needs, injected so tests can stub them. */
export interface ModelProviderSources {
  /** `/api/config/resolve-alias-source?source=model_providers`. Rejects on
   *  daemons that predate the `PropKind::AliasRef` endpoint. */
  resolveAliasSource: () => Promise<{ values: string[] }>;
  /** Pre-resolver `config/list` scan, available on every daemon. */
  listProps: () => Promise<{ entries?: PropEntry[] }>;
}

/** Extract sorted `family.alias` refs from a raw `providers.models` scan.
 *  Mirrors the canonical resolver's ordering key so both paths agree. */
export function scanConfiguredRefs(entries: PropEntry[] | undefined): string[] {
  const scanned = (entries ?? [])
    .map((e) => e.path)
    .filter((p) => /^providers\.models\.[^.]+\.[^.]+\.model$/.test(p))
    .map((p) => p.replace(/^providers\.models\./, '').replace(/\.model$/, ''));
  return Array.from(new Set(scanned)).sort();
}

/** Resolve the configured provider refs for the picker.
 *
 *  Prefers the canonical alias-source resolver. When that endpoint is
 *  unavailable — a newer `web_dist_dir` bundle served by an older daemon —
 *  falls back to the `listProps` scan sorted through the same ordering key,
 *  so the picker never silently collapses to a single ref. Throws only when
 *  both paths fail, letting the caller degrade to `activeRef`. */
export async function resolveAvailableModels(
  sources: ModelProviderSources,
): Promise<string[]> {
  try {
    return (await sources.resolveAliasSource()).values;
  } catch {
    return scanConfiguredRefs((await sources.listProps()).entries);
  }
}
