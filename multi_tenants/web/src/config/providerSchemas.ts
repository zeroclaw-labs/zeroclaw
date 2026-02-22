export interface ModelDef {
  id: string;
  label: string;
  context?: string;
}

export interface ProviderDef {
  id: string;
  label: string;
  models: ModelDef[];
  keyPlaceholder?: string;
  keyHelp?: string;
}

export const PROVIDERS: ProviderDef[] = [
  {
    id: 'openai',
    label: 'OpenAI',
    keyPlaceholder: 'sk-...',
    keyHelp: 'https://platform.openai.com/api-keys',
    models: [
      { id: 'gpt-4.1', label: 'GPT-4.1', context: '1M' },
      { id: 'gpt-4.1-mini', label: 'GPT-4.1 Mini', context: '1M' },
      { id: 'gpt-4.1-nano', label: 'GPT-4.1 Nano', context: '1M' },
      { id: 'o3', label: 'o3', context: '200K' },
      { id: 'o4-mini', label: 'o4 Mini', context: '200K' },
      { id: 'o3-mini', label: 'o3 Mini', context: '200K' },
      { id: 'gpt-4o', label: 'GPT-4o', context: '128K' },
      { id: 'gpt-4o-mini', label: 'GPT-4o Mini', context: '128K' },
      { id: 'o1', label: 'o1', context: '200K' },
      { id: 'o1-mini', label: 'o1 Mini', context: '128K' },
    ],
  },
  {
    id: 'anthropic',
    label: 'Anthropic',
    keyPlaceholder: 'sk-ant-...',
    keyHelp: 'https://console.anthropic.com/settings/keys',
    models: [
      { id: 'claude-opus-4-6', label: 'Claude Opus 4.6', context: '200K' },
      { id: 'claude-sonnet-4-6', label: 'Claude Sonnet 4.6', context: '200K' },
      { id: 'claude-sonnet-4-5-20250514', label: 'Claude Sonnet 4.5', context: '200K' },
      { id: 'claude-haiku-4-5-20251001', label: 'Claude Haiku 4.5', context: '200K' },
      { id: 'claude-3-5-sonnet-20241022', label: 'Claude 3.5 Sonnet', context: '200K' },
      { id: 'claude-3-5-haiku-20241022', label: 'Claude 3.5 Haiku', context: '200K' },
    ],
  },
  {
    id: 'gemini',
    label: 'Google Gemini',
    keyPlaceholder: 'AIza...',
    keyHelp: 'https://aistudio.google.com/apikey',
    models: [
      { id: 'gemini-2.5-pro', label: 'Gemini 2.5 Pro', context: '1M' },
      { id: 'gemini-2.5-flash', label: 'Gemini 2.5 Flash', context: '1M' },
      { id: 'gemini-2.0-flash', label: 'Gemini 2.0 Flash', context: '1M' },
      { id: 'gemini-2.0-flash-lite', label: 'Gemini 2.0 Flash Lite', context: '1M' },
      { id: 'gemini-1.5-pro', label: 'Gemini 1.5 Pro', context: '2M' },
      { id: 'gemini-1.5-flash', label: 'Gemini 1.5 Flash', context: '1M' },
    ],
  },
  {
    id: 'deepseek',
    label: 'DeepSeek',
    keyPlaceholder: 'sk-...',
    keyHelp: 'https://platform.deepseek.com/api_keys',
    models: [
      { id: 'deepseek-chat', label: 'DeepSeek Chat (V3)', context: '64K' },
      { id: 'deepseek-reasoner', label: 'DeepSeek Reasoner (R1)', context: '64K' },
      { id: 'deepseek-coder', label: 'DeepSeek Coder', context: '128K' },
    ],
  },
  {
    id: 'groq',
    label: 'Groq',
    keyPlaceholder: 'gsk_...',
    keyHelp: 'https://console.groq.com/keys',
    models: [
      { id: 'llama-4-maverick-17b-128e-instruct', label: 'Llama 4 Maverick 17B', context: '128K' },
      { id: 'llama-4-scout-17b-16e-instruct', label: 'Llama 4 Scout 17B', context: '128K' },
      { id: 'llama-3.3-70b-versatile', label: 'Llama 3.3 70B', context: '128K' },
      { id: 'llama-3.1-8b-instant', label: 'Llama 3.1 8B Instant', context: '128K' },
      { id: 'deepseek-r1-distill-llama-70b', label: 'DeepSeek R1 Distill 70B', context: '128K' },
      { id: 'mixtral-8x7b-32768', label: 'Mixtral 8x7B', context: '32K' },
      { id: 'gemma2-9b-it', label: 'Gemma 2 9B', context: '8K' },
    ],
  },
  {
    id: 'mistral',
    label: 'Mistral AI',
    keyPlaceholder: 'sk-...',
    keyHelp: 'https://console.mistral.ai/api-keys',
    models: [
      { id: 'mistral-large-latest', label: 'Mistral Large', context: '128K' },
      { id: 'mistral-medium-latest', label: 'Mistral Medium', context: '32K' },
      { id: 'mistral-small-latest', label: 'Mistral Small', context: '32K' },
      { id: 'codestral-latest', label: 'Codestral', context: '32K' },
    ],
  },
  {
    id: 'together',
    label: 'Together AI',
    keyPlaceholder: 'sk-...',
    models: [
      { id: 'meta-llama/Meta-Llama-3.1-70B-Instruct-Turbo', label: 'Llama 3.1 70B Turbo', context: '128K' },
      { id: 'meta-llama/Meta-Llama-3.1-8B-Instruct-Turbo', label: 'Llama 3.1 8B Turbo', context: '128K' },
      { id: 'Qwen/Qwen2.5-72B-Instruct-Turbo', label: 'Qwen 2.5 72B Turbo', context: '128K' },
      { id: 'deepseek-ai/DeepSeek-R1', label: 'DeepSeek R1', context: '64K' },
    ],
  },
  {
    id: 'cohere',
    label: 'Cohere',
    keyPlaceholder: 'sk-...',
    models: [
      { id: 'command-r-plus', label: 'Command R+', context: '128K' },
      { id: 'command-r', label: 'Command R', context: '128K' },
      { id: 'command', label: 'Command', context: '4K' },
    ],
  },
  {
    id: 'ollama',
    label: 'Ollama (Self-hosted)',
    keyPlaceholder: '(optional)',
    keyHelp: 'No key required for local Ollama',
    models: [
      { id: 'llama3.1', label: 'Llama 3.1' },
      { id: 'llama3.1:70b', label: 'Llama 3.1 70B' },
      { id: 'mistral', label: 'Mistral 7B' },
      { id: 'codellama', label: 'Code Llama' },
      { id: 'deepseek-r1', label: 'DeepSeek R1' },
      { id: 'qwen2.5', label: 'Qwen 2.5' },
      { id: 'gemma2', label: 'Gemma 2' },
      { id: 'phi3', label: 'Phi-3' },
    ],
  },
  {
    id: 'qwen',
    label: 'Qwen (Alibaba)',
    keyPlaceholder: 'sk-...',
    models: [
      { id: 'qwen-max', label: 'Qwen Max', context: '32K' },
      { id: 'qwen-plus', label: 'Qwen Plus', context: '128K' },
      { id: 'qwen-turbo', label: 'Qwen Turbo', context: '128K' },
      { id: 'qwen-long', label: 'Qwen Long', context: '1M' },
    ],
  },
  {
    id: 'moonshot',
    label: 'Moonshot (Kimi)',
    keyPlaceholder: 'sk-...',
    models: [
      { id: 'moonshot-v1-128k', label: 'Moonshot V1 128K', context: '128K' },
      { id: 'moonshot-v1-32k', label: 'Moonshot V1 32K', context: '32K' },
      { id: 'moonshot-v1-8k', label: 'Moonshot V1 8K', context: '8K' },
    ],
  },
  {
    id: 'glm',
    label: 'GLM (Zhipu)',
    keyPlaceholder: 'sk-...',
    models: [
      { id: 'glm-4-plus', label: 'GLM-4 Plus', context: '128K' },
      { id: 'glm-4', label: 'GLM-4', context: '128K' },
      { id: 'glm-4-flash', label: 'GLM-4 Flash', context: '128K' },
    ],
  },
  {
    id: 'minimax',
    label: 'MiniMax',
    keyPlaceholder: 'sk-...',
    models: [
      { id: 'abab6.5s-chat', label: 'ABAB 6.5s Chat' },
      { id: 'abab6.5-chat', label: 'ABAB 6.5 Chat' },
    ],
  },
  {
    id: 'qianfan',
    label: 'Qianfan (Baidu)',
    keyPlaceholder: 'sk-...',
    models: [
      { id: 'ernie-4.0-turbo', label: 'ERNIE 4.0 Turbo' },
      { id: 'ernie-4.0', label: 'ERNIE 4.0' },
      { id: 'ernie-3.5', label: 'ERNIE 3.5' },
    ],
  },
];

export const PROVIDER_MAP = Object.fromEntries(PROVIDERS.map(p => [p.id, p]));

/** Get models for a given provider, returns empty array if provider not found. */
export function getModels(providerId: string): ModelDef[] {
  return PROVIDER_MAP[providerId]?.models ?? [];
}
