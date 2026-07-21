export interface RuntimeSelection {
  preset_name: string;
}

interface RuntimeRecommendationState {
  default_runtime_profile?: string | null;
  model_provider_types: ReadonlyArray<{
    kind: string;
    default_runtime_profile?: string | null;
  }>;
}

export function runtimeDefaultForProvider(
  state: RuntimeRecommendationState | null,
  providerType?: string,
): string | null {
  const advertised = providerType
    ? state?.model_provider_types.find((provider) => provider.kind === providerType)
        ?.default_runtime_profile
    : null;
  return advertised ?? state?.default_runtime_profile ?? null;
}

export function runtimeAfterProviderChange(
  state: RuntimeRecommendationState | null,
  providerType: string,
  currentRuntime: RuntimeSelection | null,
  autoDefaulted: boolean,
): RuntimeSelection | null {
  if (!autoDefaulted) return currentRuntime;
  const recommended = runtimeDefaultForProvider(state, providerType);
  return recommended ? { preset_name: recommended } : currentRuntime;
}

export function runtimeValueForSubmit(
  runtime: RuntimeSelection | null,
): string | null {
  return runtime?.preset_name ?? null;
}

export function requiredQuickstartSelectionsComplete(input: {
  provider: unknown | null;
  risk: unknown | null;
  runtime: RuntimeSelection | null;
  memory: unknown | null;
  agentName: string;
}): boolean {
  return (
    input.provider !== null &&
    input.risk !== null &&
    input.runtime !== null &&
    input.memory !== null &&
    input.agentName.trim() !== ""
  );
}
