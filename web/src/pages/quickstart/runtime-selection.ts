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

/**
 * Resolve the user-facing presentation of a provider credential field.
 *
 * Anthropic setup-token onboarding deliberately keeps the transport field key
 * as `api_key`: the runtime consumes that submitted value to stage a stored
 * auth profile and never writes it to provider config. The UI must therefore
 * change the label and guidance without changing the submitted field key.
 */
export function quickstartCredentialPresentation(input: {
  providerType: string;
  authMode: string | undefined;
  fieldKey: string;
  label: string;
  help: string | undefined;
  setupTokenLabel: string;
  setupTokenHelp: string;
}): Pick<typeof input, "label" | "help"> {
  if (
    input.providerType === "anthropic" &&
    input.authMode === "setup_token" &&
    input.fieldKey === "api_key"
  ) {
    return {
      label: input.setupTokenLabel,
      help: input.setupTokenHelp,
    };
  }

  return {
    label: input.label,
    help: input.help,
  };
}
