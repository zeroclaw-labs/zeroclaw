// Pure resolution logic for the agent model-provider picker, split out of
// `AgentContext` so the canonical resolver contract is unit-testable without
// a React render or a live backend.

/** Data sources the picker needs, injected so tests can stub them. */
export interface ModelProviderSources {
  /** `/api/config/resolve-alias-source?source=model_providers`. */
  resolveAliasSource: () => Promise<{ values: string[] }>;
}

/** Resolve configured provider refs from the canonical alias-source resolver. */
export async function resolveAvailableModels(
  sources: ModelProviderSources,
): Promise<string[]> {
  return (await sources.resolveAliasSource()).values;
}
