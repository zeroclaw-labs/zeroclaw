// Small frontend fallback for provider-family UX hints. The backend catalog is
// canonical; keep this list aligned with zeroclaw_providers::list_model_providers
// until provider capability metadata is available on every config field row.
const LOCAL_MODEL_PROVIDERS = new Set([
  'ollama',
  'gemini_cli',
  'kilocli',
  'lmstudio',
  'llamacpp',
  'sglang',
  'vllm',
  'osaurus',
  'atomic_chat',
]);

export function isLocalModelProviderName(provider: string): boolean {
  return LOCAL_MODEL_PROVIDERS.has(provider.replace(/-/g, '_'));
}
