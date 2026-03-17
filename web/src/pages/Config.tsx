import { useState, useEffect, useRef } from 'react';
import {
  Settings,
  Save,
  CheckCircle,
  AlertTriangle,
  ShieldAlert,
} from 'lucide-react';
import {
  getConfig,
  putConfig,
  getProviders,
  getProviderModels,
  getProviderModelConfig,
  putProviderModelConfig,
} from '@/lib/api';
import type {
  ProviderListItem,
  ProviderModelCatalog,
  ProviderModelOption,
} from '@/types/api';

export default function Config() {
  const [config, setConfig] = useState('');
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState<string | null>(null);
  const [providers, setProviders] = useState<ProviderListItem[]>([]);
  const [modelOptions, setModelOptions] = useState<ProviderModelOption[]>([]);
  const [modelCatalog, setModelCatalog] = useState<ProviderModelCatalog | null>(null);
  const [selectedProvider, setSelectedProvider] = useState('');
  const [selectedModel, setSelectedModel] = useState('');
  const [modelFilter, setModelFilter] = useState('');
  const [modelMenuOpen, setModelMenuOpen] = useState(false);
  const [providerSaving, setProviderSaving] = useState(false);
  const [providerError, setProviderError] = useState<string | null>(null);
  const [providerSuccess, setProviderSuccess] = useState<string | null>(null);
  const [initialProvider, setInitialProvider] = useState('');
  const [initialModel, setInitialModel] = useState('');
  const [loadingModels, setLoadingModels] = useState(false);
  const modelRequestRef = useRef(0);

  const formatAge = (ageSecs: number | null | undefined): string | null => {
    if (ageSecs === null || ageSecs === undefined) return null;
    if (ageSecs < 60) return `${ageSecs}s`;
    if (ageSecs < 60 * 60) return `${Math.round(ageSecs / 60)}m`;
    return `${Math.round(ageSecs / (60 * 60))}h`;
  };

  const ensureProviderOption = (
    items: ProviderListItem[],
    current: string,
  ): ProviderListItem[] => {
    if (!current) return items;
    if (items.some((item) => item.id === current)) return items;
    return [
      {
        id: current,
        label: `${current} (current)`,
        local: false,
        kind: 'custom',
      },
      ...items,
    ];
  };

  const ensureModelOption = (
    items: ProviderModelOption[],
    current: string,
  ): ProviderModelOption[] => {
    if (!current) return items;
    if (items.some((item) => item.id === current)) return items;
    return [
      {
        id: current,
        label: `${current} (current)`,
        source: 'current',
      },
      ...items,
    ];
  };

  const loadModels = async (
    providerId: string,
    preferredModel?: string | null,
    seedInitial = false,
  ) => {
    const requestId = modelRequestRef.current + 1;
    modelRequestRef.current = requestId;
    setLoadingModels(true);
    setProviderError(null);
    try {
      const catalog = await getProviderModels(providerId);
      if (modelRequestRef.current !== requestId) return;
      const candidate =
        preferredModel && preferredModel.trim().length > 0
          ? preferredModel.trim()
          : catalog.default_model;
      const nextModel = candidate.trim();
      const nextOptions = ensureModelOption(catalog.models, nextModel);

      setModelCatalog(catalog);
      setModelOptions(nextOptions);
      setSelectedModel(nextModel);
      setModelFilter('');

      if (seedInitial) {
        setInitialProvider(providerId);
        setInitialModel(nextModel);
      }
    } catch (err: unknown) {
      if (modelRequestRef.current !== requestId) return;
      setProviderError(err instanceof Error ? err.message : 'Failed to load models');
      setModelCatalog(null);
      setModelOptions([]);
    } finally {
      if (modelRequestRef.current === requestId) {
        setLoadingModels(false);
      }
    }
  };

  useEffect(() => {
    let cancelled = false;

    const load = async () => {
      setLoading(true);
      setError(null);
      try {
        const [configText, providerList, providerConfig] = await Promise.all([
          getConfig(),
          getProviders(),
          getProviderModelConfig(),
        ]);
        if (cancelled) return;
        setConfig(configText);

        const defaultProvider = providerConfig.default_provider || 'openrouter';
        const mergedProviders = ensureProviderOption(providerList, defaultProvider);

        setProviders(mergedProviders);
        setSelectedProvider(defaultProvider);
        await loadModels(defaultProvider, providerConfig.default_model, true);
      } catch (err: unknown) {
        if (cancelled) return;
        setError(err instanceof Error ? err.message : 'Failed to load configuration');
      } finally {
        if (!cancelled) {
          setLoading(false);
        }
      }
    };

    load();
    return () => {
      cancelled = true;
      modelRequestRef.current += 1;
    };
  }, []);

  const handleSave = async () => {
    setSaving(true);
    setError(null);
    setSuccess(null);
    try {
      await putConfig(config);
      setSuccess('Configuration saved successfully.');
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Failed to save configuration');
    } finally {
      setSaving(false);
    }
  };

  const handleProviderSave = async () => {
    if (!selectedProvider.trim() || !selectedModel.trim()) return;
    setProviderSaving(true);
    setProviderError(null);
    setProviderSuccess(null);
    try {
      await putProviderModelConfig({
        provider: selectedProvider.trim(),
        model: selectedModel.trim(),
      });
      setProviderSuccess('Provider and model saved. Restart the gateway to apply.');
      setInitialProvider(selectedProvider.trim());
      setInitialModel(selectedModel.trim());
      const updatedConfig = await getConfig();
      setConfig(updatedConfig);
    } catch (err: unknown) {
      setProviderError(err instanceof Error ? err.message : 'Failed to save provider/model');
    } finally {
      setProviderSaving(false);
    }
  };

  const handleProviderChange = async (nextProvider: string) => {
    setSelectedProvider(nextProvider);
    setSelectedModel('');
    setModelOptions([]);
    setModelCatalog(null);
    setProviderSuccess(null);
    setProviderError(null);
    setModelFilter('');
    await loadModels(nextProvider, null);
  };

  const handleModelSelect = (nextModel: string) => {
    setSelectedModel(nextModel);
    setModelFilter('');
    setProviderSuccess(null);
    setProviderError(null);
    setModelMenuOpen(false);
  };

  const handleModelInput = (nextModel: string) => {
    setSelectedModel(nextModel);
    setModelFilter(nextModel);
    setProviderSuccess(null);
    setProviderError(null);
    setModelMenuOpen(true);
  };

  // Auto-dismiss success after 4 seconds
  useEffect(() => {
    if (!success) return;
    const timer = setTimeout(() => setSuccess(null), 4000);
    return () => clearTimeout(timer);
  }, [success]);

  useEffect(() => {
    if (!providerSuccess) return;
    const timer = setTimeout(() => setProviderSuccess(null), 4000);
    return () => clearTimeout(timer);
  }, [providerSuccess]);

  const providerDirty =
    selectedProvider.trim().length > 0 &&
    selectedModel.trim().length > 0 &&
    (selectedProvider.trim() !== initialProvider ||
      selectedModel.trim() !== initialModel);
  const renderedModelOptions = ensureModelOption(
    modelOptions,
    selectedModel.trim(),
  );
  const normalizedModelSearch = modelFilter.trim().toLowerCase();
  const filteredModelOptions =
    normalizedModelSearch.length === 0
      ? renderedModelOptions
      : renderedModelOptions.filter((model) => {
          if (model.id === selectedModel.trim()) {
            return true;
          }
          const haystack = `${model.id} ${model.label}`.toLowerCase();
          return haystack.includes(normalizedModelSearch);
        });
  const modelAgeLabel = formatAge(modelCatalog?.source_age_secs);
  const modelSourceLabel = modelCatalog
    ? `${modelCatalog.source}${modelAgeLabel ? ` • ${modelAgeLabel} ago` : ''}`
    : null;
  const resolvedProviderLabel =
    modelCatalog &&
    modelCatalog.effective_provider &&
    modelCatalog.effective_provider !== modelCatalog.requested_provider
      ? modelCatalog.effective_provider
      : null;

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="animate-spin rounded-full h-8 w-8 border-2 border-blue-500 border-t-transparent" />
      </div>
    );
  }

  return (
    <div className="p-6 space-y-6">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Settings className="h-5 w-5 text-blue-400" />
          <h2 className="text-base font-semibold text-white">Configuration</h2>
        </div>
        <button
          onClick={handleSave}
          disabled={saving}
          className="flex items-center gap-2 bg-blue-600 hover:bg-blue-700 text-white text-sm font-medium px-4 py-2 rounded-lg transition-colors disabled:opacity-50"
        >
          <Save className="h-4 w-4" />
          {saving ? 'Saving...' : 'Save'}
        </button>
      </div>

      {/* Provider & Model */}
      <div className="bg-gray-900 rounded-xl border border-gray-800 p-4 space-y-4">
        <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
          <div>
            <h3 className="text-sm font-semibold text-white">Provider & Model</h3>
            <p className="text-xs text-gray-400 mt-1">
              Select the default provider and model. Changes update config only.
            </p>
          </div>
          <button
            onClick={handleProviderSave}
            disabled={!providerDirty || providerSaving}
            className="flex items-center gap-2 bg-blue-600 hover:bg-blue-700 text-white text-sm font-medium px-4 py-2 rounded-lg transition-colors disabled:opacity-50"
          >
            <Save className="h-4 w-4" />
            {providerSaving ? 'Saving...' : 'Save Selection'}
          </button>
        </div>

        {providerSuccess && (
          <div className="flex items-center gap-2 bg-green-900/30 border border-green-700 rounded-lg p-3">
            <CheckCircle className="h-4 w-4 text-green-400 flex-shrink-0" />
            <span className="text-sm text-green-300">{providerSuccess}</span>
          </div>
        )}

        {providerError && (
          <div className="flex items-center gap-2 bg-red-900/30 border border-red-700 rounded-lg p-3">
            <AlertTriangle className="h-4 w-4 text-red-400 flex-shrink-0" />
            <span className="text-sm text-red-300">{providerError}</span>
          </div>
        )}

        <div className="grid gap-4 md:grid-cols-2">
          <div>
            <label className="block text-sm font-medium text-gray-300 mb-1">
              Provider
            </label>
            <select
              value={selectedProvider}
              onChange={(e) => handleProviderChange(e.target.value)}
              className="w-full bg-gray-900 border border-gray-700 rounded-lg px-3 py-2.5 text-sm text-white appearance-none focus:outline-none focus:ring-2 focus:ring-blue-500"
            >
              {providers.length === 0 && (
                <option value="">No providers available</option>
              )}
              {providers.map((provider) => (
                <option key={provider.id} value={provider.id}>
                  {provider.label}
                </option>
              ))}
            </select>
            {resolvedProviderLabel && (
              <p className="text-xs text-gray-500 mt-2">
                Resolved provider: {resolvedProviderLabel}
              </p>
            )}
          </div>

          <div>
            <label className="block text-sm font-medium text-gray-300 mb-1">
              Model
            </label>
            <div className="relative">
              <input
                type="text"
                value={selectedModel}
                onChange={(e) => handleModelInput(e.target.value)}
                onFocus={() => {
                  setModelMenuOpen(true);
                  setModelFilter('');
                }}
                onBlur={() => {
                  window.setTimeout(() => setModelMenuOpen(false), 120);
                }}
                placeholder="Search or enter a model id"
                className="w-full bg-gray-900 border border-gray-700 rounded-lg px-3 py-2.5 text-sm text-white placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-blue-500"
              />
              {modelMenuOpen && (
                <div className="absolute z-20 mt-1 w-full max-h-64 overflow-y-auto rounded-lg border border-gray-700 bg-gray-900 shadow-lg">
                  {loadingModels && (
                    <div className="px-3 py-2 text-sm text-gray-400">
                      Loading models...
                    </div>
                  )}
                  {!loadingModels && filteredModelOptions.length === 0 && (
                    <div className="px-3 py-2 text-sm text-gray-400">
                      No models available
                    </div>
                  )}
                  {!loadingModels &&
                    filteredModelOptions.map((model) => (
                      <button
                        key={model.id}
                        type="button"
                        onMouseDown={(event) => {
                          event.preventDefault();
                          handleModelSelect(model.id);
                        }}
                        className="w-full text-left px-3 py-2 text-sm text-gray-200 hover:bg-gray-800"
                      >
                        {model.label}
                      </button>
                    ))}
                </div>
              )}
            </div>
            <p className="mt-2 text-[11px] text-gray-500">
              Type to search or enter a custom model id.
            </p>
            {modelSourceLabel && (
              <p className="text-xs text-gray-500 mt-2">
                Models source: {modelSourceLabel}
              </p>
            )}
          </div>
        </div>

        <p className="text-xs text-gray-500">
          Restart the gateway after saving to apply changes to the running
          runtime.
        </p>
      </div>

      {/* Sensitive fields note */}
      <div className="flex items-start gap-3 bg-yellow-900/20 border border-yellow-700/40 rounded-lg p-4">
        <ShieldAlert className="h-5 w-5 text-yellow-400 flex-shrink-0 mt-0.5" />
        <div>
          <p className="text-sm text-yellow-300 font-medium">
            Sensitive fields are masked
          </p>
          <p className="text-sm text-yellow-400/70 mt-0.5">
            API keys, tokens, and passwords are hidden for security. To update a
            masked field, replace the entire masked value with your new value.
          </p>
        </div>
      </div>

      {/* Success message */}
      {success && (
        <div className="flex items-center gap-2 bg-green-900/30 border border-green-700 rounded-lg p-3">
          <CheckCircle className="h-4 w-4 text-green-400 flex-shrink-0" />
          <span className="text-sm text-green-300">{success}</span>
        </div>
      )}

      {/* Error message */}
      {error && (
        <div className="flex items-center gap-2 bg-red-900/30 border border-red-700 rounded-lg p-3">
          <AlertTriangle className="h-4 w-4 text-red-400 flex-shrink-0" />
          <span className="text-sm text-red-300">{error}</span>
        </div>
      )}

      {/* Config Editor */}
      <div className="bg-gray-900 rounded-xl border border-gray-800 overflow-hidden">
        <div className="flex items-center justify-between px-4 py-2 border-b border-gray-800 bg-gray-800/50">
          <span className="text-xs text-gray-400 font-medium uppercase tracking-wider">
            TOML Configuration
          </span>
          <span className="text-xs text-gray-500">
            {config.split('\n').length} lines
          </span>
        </div>
        <textarea
          value={config}
          onChange={(e) => setConfig(e.target.value)}
          spellCheck={false}
          className="w-full min-h-[500px] bg-gray-950 text-gray-200 font-mono text-sm p-4 resize-y focus:outline-none focus:ring-2 focus:ring-blue-500 focus:ring-inset"
          style={{ tabSize: 4 }}
        />
      </div>
    </div>
  );
}
