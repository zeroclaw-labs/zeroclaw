// Local-vs-remote provider classification, sourced from the backend catalog
// (`GET /api/config/catalog`, which serves `zeroclaw_providers::list_model_providers()`).
// The registry is canonical; this module caches the `local` flag per provider
// name so synchronous call sites (placeholder text, offline fallbacks) can ask
// without re-deriving the list. Until the cache is primed, classification
// defaults to remote (`false`) rather than guessing from a shadow list.

import { getCatalog } from "./api";

const localByName = new Map<string, boolean>();
const displayByName = new Map<string, string>();
let primePromise: Promise<void> | null = null;

function normalize(provider: string): string {
  return provider.replace(/-/g, "_");
}

// Prime the cache from the backend catalog. Idempotent and de-duplicated: N
// callers share one round-trip. Safe to call eagerly on app load.
export function primeModelProviderCatalog(): Promise<void> {
  if (primePromise) return primePromise;
  primePromise = getCatalog()
    .then((res) => {
      for (const p of res.providers) {
        const key = normalize(p.name);
        localByName.set(key, p.local);
        displayByName.set(key, p.display_name);
      }
    })
    .catch(() => {
      // Leave the cache empty; classification falls back to remote.
      primePromise = null;
    });
  return primePromise;
}

export function isLocalModelProviderName(provider: string): boolean {
  return localByName.get(normalize(provider)) ?? false;
}

// Display name for a provider from the backend catalog, falling back to the
// raw provider name when the cache is not yet primed or the name is unknown.
export function modelProviderDisplayName(provider: string): string {
  return displayByName.get(normalize(provider)) ?? provider;
}
