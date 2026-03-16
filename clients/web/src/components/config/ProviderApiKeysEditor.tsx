'use client';

import { useState } from 'react';
import { KeyRound, Plus, Check, AlertCircle, Trash2 } from 'lucide-react';
import { putProviderApiKey } from '@/lib/gateway-api';

/**
 * Supported LLM providers with their display names.
 * Sorted alphabetically for easy lookup in the dropdown.
 */
const PROVIDERS = [
  { value: 'anthropic', label: 'Anthropic (Claude)' },
  { value: 'astrai', label: 'AstrAI' },
  { value: 'cerebras', label: 'Cerebras' },
  { value: 'cohere', label: 'Cohere' },
  { value: 'deepseek', label: 'DeepSeek' },
  { value: 'fireworks', label: 'Fireworks AI' },
  { value: 'gemini', label: 'Google Gemini' },
  { value: 'groq', label: 'Groq' },
  { value: 'hunyuan', label: 'Hunyuan (Tencent)' },
  { value: 'hyperbolic', label: 'Hyperbolic' },
  { value: 'kluster', label: 'Kluster' },
  { value: 'lambdalabs', label: 'Lambda Labs' },
  { value: 'lepton', label: 'Lepton AI' },
  { value: 'mistral', label: 'Mistral' },
  { value: 'novita', label: 'Novita AI' },
  { value: 'ollama', label: 'Ollama (Local)' },
  { value: 'openai', label: 'OpenAI' },
  { value: 'openrouter', label: 'OpenRouter' },
  { value: 'osaurus', label: 'Osaurus' },
  { value: 'ovhcloud', label: 'OVHcloud' },
  { value: 'perplexity', label: 'Perplexity' },
  { value: 'sglang', label: 'SGLang' },
  { value: 'sambanova', label: 'SambaNova' },
  { value: 'together', label: 'Together AI' },
  { value: 'telnyx', label: 'Telnyx' },
  { value: 'venice', label: 'Venice' },
  { value: 'vllm', label: 'vLLM' },
  { value: 'xai', label: 'xAI (Grok)' },
] as const;

interface Props {
  /** Currently configured provider keys from config (may be masked). */
  configuredProviders: Record<string, string>;
  /** Called after a key is saved so parent can refresh config state. */
  onKeySaved?: (provider: string) => void;
  /** Called when a key is removed so parent can update config state. */
  onKeyRemoved?: (provider: string) => void;
}

export default function ProviderApiKeysEditor({
  configuredProviders,
  onKeySaved,
  onKeyRemoved,
}: Props) {
  const [selectedProvider, setSelectedProvider] = useState('');
  const [apiKey, setApiKey] = useState('');
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState<string | null>(null);

  const configuredList = Object.keys(configuredProviders).filter(
    (k) => configuredProviders[k] && configuredProviders[k] !== '',
  );

  const handleSave = async () => {
    if (!selectedProvider || !apiKey.trim()) {
      setError('Provider and API Key are required.');
      return;
    }

    setSaving(true);
    setError(null);
    setSuccess(null);

    try {
      await putProviderApiKey(selectedProvider, apiKey.trim());
      setSuccess(`${getProviderLabel(selectedProvider)} API key saved.`);
      setApiKey('');
      setSelectedProvider('');
      onKeySaved?.(selectedProvider);
      setTimeout(() => setSuccess(null), 3000);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to save API key.');
    } finally {
      setSaving(false);
    }
  };

  const handleRemove = async (provider: string) => {
    setSaving(true);
    setError(null);
    try {
      // Send empty key to clear the provider's API key
      await putProviderApiKey(provider, '');
      onKeyRemoved?.(provider);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to remove API key.');
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="bg-gray-900/50 border border-gray-800 rounded-xl overflow-hidden">
      {/* Header */}
      <div className="flex items-center gap-2 px-4 py-3 border-b border-gray-800">
        <KeyRound className="h-4 w-4 text-blue-400" />
        <h3 className="text-sm font-semibold text-white">Provider API Keys</h3>
        <span className="text-xs text-gray-500 ml-auto">
          {configuredList.length} configured
        </span>
      </div>

      <div className="p-4 space-y-4">
        {/* Description */}
        <p className="text-xs text-gray-400">
          Add API keys for different LLM providers. Each provider&apos;s key is stored separately
          and used automatically when you select that provider&apos;s model.
        </p>

        {/* Add new key form */}
        <div className="flex flex-col sm:flex-row gap-2">
          <select
            value={selectedProvider}
            onChange={(e) => { setSelectedProvider(e.target.value); setError(null); }}
            className="flex-shrink-0 sm:w-48 bg-gray-800 border border-gray-700 rounded-lg px-3 py-2 text-sm text-white focus:outline-none focus:ring-2 focus:ring-blue-500"
          >
            <option value="">Select provider...</option>
            {PROVIDERS.map((p) => (
              <option key={p.value} value={p.value}>
                {p.label}
              </option>
            ))}
          </select>

          <div className="flex-1 relative">
            <input
              type="password"
              value={apiKey}
              onChange={(e) => { setApiKey(e.target.value); setError(null); }}
              placeholder="Enter API key..."
              className="w-full bg-gray-800 border border-gray-700 rounded-lg px-3 py-2 text-sm text-white placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-blue-500"
            />
          </div>

          <button
            onClick={handleSave}
            disabled={saving || !selectedProvider || !apiKey.trim()}
            className="flex-shrink-0 flex items-center gap-1.5 bg-blue-600 hover:bg-blue-700 disabled:bg-gray-700 disabled:text-gray-500 text-white text-sm font-medium px-4 py-2 rounded-lg transition-colors"
          >
            <Plus className="h-3.5 w-3.5" />
            {saving ? 'Saving...' : 'Add'}
          </button>
        </div>

        {/* Status messages */}
        {success && (
          <div className="flex items-center gap-2 text-xs text-green-400">
            <Check className="h-3.5 w-3.5" />
            {success}
          </div>
        )}
        {error && (
          <div className="flex items-center gap-2 text-xs text-red-400">
            <AlertCircle className="h-3.5 w-3.5" />
            {error}
          </div>
        )}

        {/* Configured providers list */}
        {configuredList.length > 0 && (
          <div className="space-y-1.5">
            <p className="text-xs font-medium text-gray-500 uppercase tracking-wider">
              Configured Providers
            </p>
            <div className="grid gap-1.5">
              {configuredList.map((provider) => (
                <div
                  key={provider}
                  className="flex items-center justify-between bg-gray-800/50 border border-gray-700/50 rounded-lg px-3 py-2"
                >
                  <div className="flex items-center gap-2">
                    <div className="h-2 w-2 rounded-full bg-green-400" />
                    <span className="text-sm text-gray-200">
                      {getProviderLabel(provider)}
                    </span>
                    <span className="text-xs text-gray-600 font-mono">
                      {provider}
                    </span>
                  </div>
                  <div className="flex items-center gap-2">
                    <span className="text-xs text-gray-500 font-mono">
                      ••••••••
                    </span>
                    <button
                      onClick={() => handleRemove(provider)}
                      disabled={saving}
                      className="p-1 text-gray-500 hover:text-red-400 transition-colors disabled:opacity-50"
                      title={`Remove ${provider} API key`}
                    >
                      <Trash2 className="h-3.5 w-3.5" />
                    </button>
                  </div>
                </div>
              ))}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

function getProviderLabel(value: string): string {
  const found = PROVIDERS.find((p) => p.value === value);
  return found ? found.label : value;
}
