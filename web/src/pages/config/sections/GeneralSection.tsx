import { Zap } from 'lucide-react';
import SectionCard from '../controls/SectionCard';
import FieldRow from '../controls/FieldRow';
import NumberInput from '../controls/NumberInput';
import Slider from '../controls/Slider';
import Select from '../controls/Select';
import { t } from '@/lib/i18n';

interface Props {
  config: Record<string, unknown>;
  onUpdate: (field: string, value: unknown) => void;
}

const LOCALE_OPTIONS = [
  { value: '', label: 'Auto-detect' },
  { value: 'en', label: 'English' },
  { value: 'zh', label: '中文' },
  { value: 'tr', label: 'Türkçe' },
];

const PROVIDER_OPTIONS = [
  { value: 'openrouter', label: 'OpenRouter' },
  { value: 'anthropic', label: 'Anthropic' },
  { value: 'openai', label: 'OpenAI' },
  { value: 'copilot', label: 'GitHub Copilot' },
  { value: 'ollama', label: 'Ollama' },
  { value: 'gemini', label: 'Google Gemini' },
  { value: 'azure-openai', label: 'Azure OpenAI' },
  { value: 'bedrock', label: 'AWS Bedrock' },
  { value: 'groq', label: 'Groq' },
  { value: 'mistral', label: 'Mistral' },
  { value: 'deepseek', label: 'DeepSeek' },
  { value: 'xai', label: 'xAI (Grok)' },
  { value: 'together', label: 'Together AI' },
  { value: 'fireworks', label: 'Fireworks AI' },
  { value: 'perplexity', label: 'Perplexity' },
  { value: 'cohere', label: 'Cohere' },
  { value: 'cerebras', label: 'Cerebras' },
  { value: 'sambanova', label: 'SambaNova' },
  { value: 'lmstudio', label: 'LM Studio' },
  { value: 'llamacpp', label: 'llama.cpp' },
  { value: 'vllm', label: 'vLLM' },
  { value: 'qwen', label: 'Qwen' },
  { value: 'deepinfra', label: 'DeepInfra' },
  { value: 'huggingface', label: 'Hugging Face' },
  { value: 'nvidia', label: 'NVIDIA NIM' },
  { value: 'cloudflare', label: 'Cloudflare AI' },
  { value: 'litellm', label: 'LiteLLM' },
];

// Models grouped by provider. Newest models listed first.
const MODELS_BY_PROVIDER: Record<string, { value: string; label: string }[]> = {
  openrouter: [
    { value: 'anthropic/claude-sonnet-4-6', label: 'Claude Sonnet 4.6' },
    { value: 'anthropic/claude-opus-4-6', label: 'Claude Opus 4.6' },
    { value: 'anthropic/claude-4.5-sonnet', label: 'Claude 4.5 Sonnet' },
    { value: 'anthropic/claude-opus-4-20250514', label: 'Claude Opus 4' },
    { value: 'openai/gpt-5.4', label: 'GPT-5.4' },
    { value: 'openai/gpt-5.4-pro', label: 'GPT-5.4 Pro' },
    { value: 'openai/gpt-4o', label: 'GPT-4o' },
    { value: 'google/gemini-3.1-pro', label: 'Gemini 3.1 Pro' },
    { value: 'google/gemini-3.1-flash-lite', label: 'Gemini 3.1 Flash Lite' },
    { value: 'google/gemini-2.5-pro', label: 'Gemini 2.5 Pro' },
    { value: 'deepseek/deepseek-v3.2', label: 'DeepSeek V3.2' },
    { value: 'deepseek/deepseek-r1-0528', label: 'DeepSeek R1' },
    { value: 'x-ai/grok-4.1-fast', label: 'Grok 4.1 Fast' },
    { value: 'meta-llama/llama-4-maverick', label: 'Llama 4 Maverick 400B' },
    { value: 'meta-llama/llama-4-70b', label: 'Llama 4 70B' },
    { value: 'mistralai/devstral-2', label: 'Devstral 2' },
    { value: 'qwen/qwen-3.6-plus-preview', label: 'Qwen 3.6 Plus Preview' },
  ],
  anthropic: [
    { value: 'claude-sonnet-4-6', label: 'Claude Sonnet 4.6' },
    { value: 'claude-opus-4-6', label: 'Claude Opus 4.6' },
    { value: 'claude-4.5-sonnet', label: 'Claude 4.5 Sonnet' },
    { value: 'claude-opus-4-20250514', label: 'Claude Opus 4' },
    { value: 'claude-haiku-4-5-20251001', label: 'Claude Haiku 4.5' },
  ],
  openai: [
    { value: 'gpt-5.4', label: 'GPT-5.4' },
    { value: 'gpt-5.4-pro', label: 'GPT-5.4 Pro' },
    { value: 'gpt-4o', label: 'GPT-4o' },
    { value: 'gpt-4o-mini', label: 'GPT-4o Mini' },
    { value: 'o1-preview', label: 'o1 Preview' },
  ],
  copilot: [
    { value: 'gpt-5.4', label: 'GPT-5.4' },
    { value: 'gpt-5.4-mini', label: 'GPT-5.4 Mini (recommended)' },
    { value: 'gpt-5.3', label: 'GPT-5.3' },
    { value: 'gpt-5.3-codex', label: 'GPT-5.3 Codex' },
    { value: 'gpt-5.2', label: 'GPT-5.2' },
    { value: 'gpt-5.2-codex', label: 'GPT-5.2 Codex' },
    { value: 'gpt-5.1', label: 'GPT-5.1' },
    { value: 'gpt-5.1-codex', label: 'GPT-5.1 Codex' },
    { value: 'gpt-5.1-codex-max', label: 'GPT-5.1 Codex Max' },
    { value: 'gpt-5-mini', label: 'GPT-5 Mini' },
    { value: 'gpt-4.1', label: 'GPT-4.1' },
    { value: 'gpt-4o', label: 'GPT-4o' },
    { value: 'claude-opus-4.6', label: 'Claude Opus 4.6' },
    { value: 'claude-opus-4.5', label: 'Claude Opus 4.5' },
    { value: 'claude-sonnet-4.5', label: 'Claude Sonnet 4.5' },
    { value: 'claude-haiku-4.5', label: 'Claude Haiku 4.5' },
    { value: 'gemini-3.1-pro', label: 'Gemini 3.1 Pro' },
    { value: 'gemini-3-pro', label: 'Gemini 3 Pro' },
    { value: 'gemini-3-flash', label: 'Gemini 3 Flash' },
    { value: 'gemini-2.5-pro', label: 'Gemini 2.5 Pro' },
    { value: 'grok-code-fast-1', label: 'Grok Code Fast 1' },
  ],
  gemini: [
    { value: 'gemini-3.1-pro', label: 'Gemini 3.1 Pro' },
    { value: 'gemini-3.1-flash-lite', label: 'Gemini 3.1 Flash Lite' },
    { value: 'gemini-3-pro', label: 'Gemini 3 Pro' },
    { value: 'gemini-2.5-pro', label: 'Gemini 2.5 Pro' },
    { value: 'gemini-2.5-flash', label: 'Gemini 2.5 Flash' },
  ],
  groq: [
    { value: 'llama-4-70b', label: 'Llama 4 70B' },
    { value: 'gpt-oss-120b', label: 'GPT-OSS 120B' },
    { value: 'llama-3.3-70b-versatile', label: 'Llama 3.3 70B' },
  ],
  mistral: [
    { value: 'mistral-large-latest', label: 'Mistral Large' },
    { value: 'devstral-2', label: 'Devstral 2' },
    { value: 'mistral-small-latest', label: 'Mistral Small' },
    { value: 'codestral-latest', label: 'Codestral' },
  ],
  deepseek: [
    { value: 'deepseek-chat', label: 'DeepSeek V3.2 Chat' },
    { value: 'deepseek-reasoner', label: 'DeepSeek R1 Reasoner' },
  ],
  xai: [
    { value: 'grok-4.1-fast', label: 'Grok 4.1 Fast' },
    { value: 'grok-3', label: 'Grok 3' },
    { value: 'grok-3-mini', label: 'Grok 3 Mini' },
  ],
  together: [
    { value: 'meta-llama/Llama-4-Maverick-400B', label: 'Llama 4 Maverick 400B' },
    { value: 'meta-llama/Llama-4-70B', label: 'Llama 4 70B' },
    { value: 'meta-llama/Llama-3.3-70B-Instruct-Turbo', label: 'Llama 3.3 70B Turbo' },
  ],
  fireworks: [
    { value: 'accounts/fireworks/models/llama-4-maverick-400b', label: 'Llama 4 Maverick 400B' },
    { value: 'accounts/fireworks/models/llama-v3p3-70b-instruct', label: 'Llama 3.3 70B' },
  ],
  cerebras: [
    { value: 'llama-4-70b', label: 'Llama 4 70B' },
    { value: 'llama-3.3-70b', label: 'Llama 3.3 70B' },
  ],
  bedrock: [
    { value: 'anthropic.claude-sonnet-4-6', label: 'Claude Sonnet 4.6' },
    { value: 'anthropic.claude-opus-4-6', label: 'Claude Opus 4.6' },
    { value: 'anthropic.claude-haiku-4-5', label: 'Claude Haiku 4.5' },
  ],
  'azure-openai': [
    { value: 'gpt-5.4', label: 'GPT-5.4' },
    { value: 'gpt-4o', label: 'GPT-4o' },
    { value: 'gpt-4o-mini', label: 'GPT-4o Mini' },
  ],
  qwen: [
    { value: 'qwen-3.6-plus-preview', label: 'Qwen 3.6 Plus Preview' },
    { value: 'qwen-max', label: 'Qwen Max' },
    { value: 'qwen-plus', label: 'Qwen Plus' },
    { value: 'qwen-turbo', label: 'Qwen Turbo' },
  ],
  perplexity: [
    { value: 'sonar-pro', label: 'Sonar Pro' },
    { value: 'sonar', label: 'Sonar' },
  ],
  sambanova: [
    { value: 'llama-4-maverick-400b', label: 'Llama 4 Maverick 400B' },
    { value: 'llama-3.3-70b', label: 'Llama 3.3 70B' },
  ],
};

export default function GeneralSection({ config, onUpdate }: Props) {
  const rawProvider = (config.default_provider as string) ?? 'openrouter';

  function normalizeProvider(p: string) {
    if (!p) return p;
    const lower = p.toLowerCase();
    if (lower === 'github-copilot') return 'copilot';
    return lower;
  }

  const provider = normalizeProvider(rawProvider);
  const modelOptions = MODELS_BY_PROVIDER[provider];
  const currentModel = (config.default_model as string) ?? '';

  // When provider changes, auto-select the first model for that provider
  const handleProviderChange = (v: string) => {
    const canonical = normalizeProvider(v);
    onUpdate('default_provider', canonical);
    const models = MODELS_BY_PROVIDER[canonical];
    if (models && models.length > 0) {
      // If switching to Copilot, prefer the recommended mini model as default
      if (canonical === 'copilot') {
        onUpdate('default_model', 'gpt-5.4-mini');
      } else {
        onUpdate('default_model', models[0]!.value);
      }
    }
  };

  return (
    <SectionCard
      icon={<Zap className='h-5 w-5' />}
      title={t('config.section.general')}
      defaultOpen
    >
      <FieldRow label={t('config.field.default_provider')} description={t('config.field.default_provider.desc')}>
        <Select
          value={provider}
          onChange={handleProviderChange}
          options={PROVIDER_OPTIONS}
        />
      </FieldRow>
      <FieldRow label={t('config.field.default_model')} description={t('config.field.default_model.desc')}>
        {modelOptions ? (
          <Select
            value={modelOptions.some((o) => o.value === currentModel) ? currentModel : ''}
            onChange={(v) => onUpdate('default_model', v)}
            options={[
              ...(currentModel && !modelOptions.some((o) => o.value === currentModel)
                ? [{ value: currentModel, label: currentModel }]
                : []),
              ...modelOptions,
            ]}
          />
          ) : (
            <input
              type={'text'}
              value={currentModel}
              onChange={(e) => onUpdate('default_model', e.target.value)}
              placeholder={'model name'}
              className={'input-electric text-sm px-3 py-1.5 w-52 font-mono'}
            />
          )}
      </FieldRow>
      <FieldRow label={t('config.field.default_temperature')} description={t('config.field.default_temperature.desc')}>
        <Slider
          value={(config.default_temperature as number) ?? 0.7}
          onChange={(v) => onUpdate('default_temperature', v)}
          min={0}
          max={2}
          step={0.1}
        />
      </FieldRow>
      <FieldRow label={t('config.field.provider_timeout_secs')} description={t('config.field.provider_timeout_secs.desc')}>
        <NumberInput
          value={(config.provider_timeout_secs as number) ?? 120}
          onChange={(v) => onUpdate('provider_timeout_secs', v)}
          min={1}
        />
      </FieldRow>
      <FieldRow label={t('config.field.locale')} description={t('config.field.locale.desc')}>
        <Select
          value={(config.locale as string) ?? ''}
          onChange={(v) => onUpdate('locale', v || undefined)}
          options={LOCALE_OPTIONS}
        />
      </FieldRow>
    </SectionCard>
  );
}