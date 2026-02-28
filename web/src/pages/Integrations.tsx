import { useState, useEffect } from 'react';
import { Link } from 'react-router-dom';
import { Puzzle, Check, Zap, Clock, KeyRound, X } from 'lucide-react';
import type {
  Integration,
  IntegrationCredentialsField,
  IntegrationSettingsEntry,
  StatusResponse,
} from '@/types/api';
import {
  getIntegrations,
  getIntegrationSettings,
  getStatus,
  putIntegrationCredentials,
} from '@/lib/api';

function statusBadge(status: Integration['status']) {
  switch (status) {
    case 'Active':
      return {
        icon: Check,
        label: 'Active',
        classes: 'bg-green-900/40 text-green-400 border-green-700/50',
      };
    case 'Available':
      return {
        icon: Zap,
        label: 'Available',
        classes: 'bg-blue-900/40 text-blue-400 border-blue-700/50',
      };
    case 'ComingSoon':
      return {
        icon: Clock,
        label: 'Coming Soon',
        classes: 'bg-gray-800 text-gray-400 border-gray-700',
      };
  }
}

function formatCategory(category: string): string {
  if (!category) return category;
  return category
    .replace(/([a-z])([A-Z])/g, '$1 $2')
    .replace(/Ai/g, 'AI');
}

const SELECT_KEEP = '__keep__';
const SELECT_CUSTOM = '__custom__';
const SELECT_CLEAR = '__clear__';

const FALLBACK_MODEL_OPTIONS: Record<string, string[]> = {
  openrouter: ['anthropic/claude-sonnet-4-6', 'openai/gpt-5.2', 'google/gemini-3.1-pro'],
  anthropic: ['claude-sonnet-4-6', 'claude-opus-4-6'],
  openai: ['gpt-5.2', 'gpt-5.2-codex', 'gpt-4o'],
  google: ['google/gemini-3.1-pro', 'google/gemini-3-flash', 'google/gemini-2.5-pro'],
  deepseek: ['deepseek/deepseek-reasoner', 'deepseek/deepseek-chat'],
  xai: ['x-ai/grok-4', 'x-ai/grok-3'],
  mistral: ['mistral-large-latest', 'codestral-latest', 'mistral-small-latest'],
  perplexity: ['sonar-pro', 'sonar-reasoning-pro', 'sonar'],
  vercel: ['openai/gpt-5.2', 'anthropic/claude-sonnet-4-6', 'google/gemini-3.1-pro'],
  bedrock: ['anthropic.claude-sonnet-4-5-20250929-v1:0', 'anthropic.claude-opus-4-6-v1:0'],
  groq: ['llama-3.3-70b-versatile', 'mixtral-8x7b-32768'],
  together: [
    'meta-llama/Llama-3.3-70B-Instruct-Turbo',
    'Qwen/Qwen2.5-72B-Instruct-Turbo',
    'deepseek-ai/DeepSeek-R1-Distill-Llama-70B',
  ],
  cohere: ['command-r-plus-08-2024', 'command-r-08-2024'],
};

function customModelFormatHint(integrationId: string): string {
  if (integrationId === 'openrouter' || integrationId === 'vercel') {
    return 'Format: anthropic/claude-sonnet-4-6';
  }
  return 'Format: claude-sonnet-4-6 (or provider/model when required)';
}

function modelOptionsForField(
  integrationId: string,
  field: IntegrationCredentialsField,
): string[] {
  if (field.key !== 'default_model') return field.options ?? [];
  if (field.options?.length) return field.options;
  return FALLBACK_MODEL_OPTIONS[integrationId] ?? [];
}

export default function Integrations() {
  const [integrations, setIntegrations] = useState<Integration[]>([]);
  const [settingsByName, setSettingsByName] = useState<
    Record<string, IntegrationSettingsEntry>
  >({});
  const [settingsRevision, setSettingsRevision] = useState('');
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [activeCategory, setActiveCategory] = useState<string>('all');
  const [activeEditor, setActiveEditor] = useState<IntegrationSettingsEntry | null>(
    null,
  );
  const [fieldValues, setFieldValues] = useState<Record<string, string>>({});
  const [customFieldValues, setCustomFieldValues] = useState<Record<string, string>>({});
  const [dirtyFields, setDirtyFields] = useState<Record<string, boolean>>({});
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [saveSuccess, setSaveSuccess] = useState<string | null>(null);
  const [runtimeStatus, setRuntimeStatus] = useState<Pick<StatusResponse, 'model'> | null>(null);
  const [activeAiIntegrationId, setActiveAiIntegrationId] = useState<string | null>(null);
  const [quickModelDrafts, setQuickModelDrafts] = useState<Record<string, string>>({});
  const [quickModelSavingId, setQuickModelSavingId] = useState<string | null>(null);
  const [quickModelError, setQuickModelError] = useState<string | null>(null);

  const buildInitialFieldValues = (integration: IntegrationSettingsEntry) =>
    integration.fields.reduce<Record<string, string>>((acc, field) => {
      if (modelOptionsForField(integration.id, field).length > 0) {
        acc[field.key] = field.has_value ? SELECT_KEEP : '';
      } else {
        acc[field.key] = '';
      }
      return acc;
    }, {});

  const modelFieldFor = (integration: IntegrationSettingsEntry) =>
    integration.fields.find((field) => field.key === 'default_model');

  const fallbackModelFor = (integration: IntegrationSettingsEntry): string | null => {
    const modelField = modelFieldFor(integration);
    if (!modelField) return null;
    return modelOptionsForField(integration.id, modelField)[0] ?? null;
  };

  const modelValueFor = (
    integration: IntegrationSettingsEntry,
    isActiveDefaultProvider: boolean,
  ): string | null => {
    if (isActiveDefaultProvider && runtimeStatus?.model?.trim()) {
      return runtimeStatus.model.trim();
    }

    const fieldModel = modelFieldFor(integration)?.current_value?.trim();
    if (fieldModel) {
      return fieldModel;
    }

    return null;
  };

  const activeAiIntegration = Object.values(settingsByName).find(
    (integration) => integration.id === activeAiIntegrationId,
  );

  const loadData = async (
    showLoadingState = true,
  ): Promise<Record<string, IntegrationSettingsEntry> | null> => {
    if (showLoadingState) {
      setLoading(true);
    }
    setError(null);
    try {
      const [integrationList, settings, status] = await Promise.all([
        getIntegrations(),
        getIntegrationSettings(),
        getStatus().catch(() => null),
      ]);

      const nextSettingsByName = settings.integrations.reduce<
        Record<string, IntegrationSettingsEntry>
      >((acc, item) => {
        acc[item.name] = item;
        return acc;
      }, {});

      setIntegrations(integrationList);
      setSettingsRevision(settings.revision);
      setSettingsByName(nextSettingsByName);
      setActiveAiIntegrationId(settings.active_default_provider_integration_id ?? null);
      setRuntimeStatus(status ? { model: status.model } : null);
      return nextSettingsByName;
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Failed to load integrations');
      setActiveAiIntegrationId(null);
      setRuntimeStatus(null);
      return null;
    } finally {
      if (showLoadingState) {
        setLoading(false);
      }
    }
  };

  useEffect(() => {
    void loadData();
  }, []);

  useEffect(() => {
    if (!saveSuccess) return;
    const timer = setTimeout(() => setSaveSuccess(null), 4000);
    return () => clearTimeout(timer);
  }, [saveSuccess]);

  useEffect(() => {
    if (!activeEditor) return;

    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        closeEditor();
      }
    };

    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [activeEditor, saving]);

  const openEditor = (integration: IntegrationSettingsEntry) => {
    setActiveEditor(integration);
    setFieldValues(buildInitialFieldValues(integration));
    setCustomFieldValues({});
    setDirtyFields({});
    setSaveError(null);
  };

  const closeEditor = () => {
    if (saving) return;
    setActiveEditor(null);
    setFieldValues({});
    setCustomFieldValues({});
    setDirtyFields({});
    setSaveError(null);
  };

  const updateField = (key: string, value: string) => {
    setFieldValues((prev) => ({ ...prev, [key]: value }));
    setDirtyFields((prev) => ({ ...prev, [key]: true }));
  };

  const updateCustomField = (key: string, value: string) => {
    setCustomFieldValues((prev) => ({ ...prev, [key]: value }));
    setDirtyFields((prev) => ({ ...prev, [key]: true }));
  };

  const saveCredentials = async () => {
    if (!activeEditor) return;

    setSaveError(null);
    setQuickModelError(null);

    const payload: Record<string, string> = {};
    for (const field of activeEditor.fields) {
      const value = fieldValues[field.key] ?? '';
      const isDirty = !!dirtyFields[field.key];
      const isSelectField = modelOptionsForField(activeEditor.id, field).length > 0;

      let resolvedValue = value;
      if (isSelectField) {
        if (value === SELECT_KEEP) {
          if (field.required && !field.has_value) {
            setSaveError(`${field.label} is required.`);
            return;
          }
          if (isDirty) {
            continue;
          }
        } else if (value === SELECT_CUSTOM) {
          resolvedValue = customFieldValues[field.key] ?? '';
        } else if (value === SELECT_CLEAR) {
          resolvedValue = '';
        }
      }

      const trimmed = resolvedValue.trim();

      if (isSelectField && value === SELECT_CUSTOM && !trimmed) {
        setSaveError(`Enter a custom value for ${field.label} or choose a recommended model.`);
        return;
      }

      if (field.required && !trimmed && !field.has_value) {
        setSaveError(`${field.label} is required.`);
        return;
      }

      if (isDirty) {
        if (isSelectField && value === SELECT_KEEP) {
          continue;
        }
        payload[field.key] = resolvedValue;
      }
    }

    if (
      Object.keys(payload).length === 0 &&
      !activeEditor.activates_default_provider
    ) {
      setSaveError('No changes to save.');
      return;
    }

    if (
      activeEditor.activates_default_provider &&
      activeAiIntegrationId &&
      activeEditor.id !== activeAiIntegrationId
    ) {
      const currentProvider = activeAiIntegration?.name ?? 'current provider';
      const confirmed = window.confirm(
        `Switch default AI provider from ${currentProvider} to ${activeEditor.name}?`,
      );
      if (!confirmed) {
        return;
      }
    }

    setSaving(true);
    try {
      await putIntegrationCredentials(activeEditor.id, {
        revision: settingsRevision,
        fields: payload,
      });

      await loadData(false);
      setSaveSuccess(`${activeEditor.name} credentials saved.`);
      closeEditor();
    } catch (err: unknown) {
      const message = err instanceof Error ? err.message : 'Failed to save credentials';
      if (message.includes('API 409')) {
        const refreshed = await loadData(false);
        if (refreshed) {
          const latestEditor = refreshed[activeEditor.name];
          if (latestEditor) {
            setActiveEditor(latestEditor);
            setFieldValues(buildInitialFieldValues(latestEditor));
            setCustomFieldValues({});
            setDirtyFields({});
          }
        }
        setSaveError(
          'Configuration changed elsewhere. Refreshed latest settings; re-enter values and save again.',
        );
      } else {
        setSaveError(message);
      }
    } finally {
      setSaving(false);
    }
  };

  const saveQuickModel = async (
    integration: IntegrationSettingsEntry,
    targetModel: string,
    currentModel: string,
    isActiveDefaultProvider: boolean,
  ) => {
    const trimmedTarget = targetModel.trim();
    if (!trimmedTarget || trimmedTarget === currentModel) {
      return;
    }

    if (
      activeAiIntegrationId &&
      !isActiveDefaultProvider &&
      integration.id !== activeAiIntegrationId
    ) {
      const currentProvider = activeAiIntegration?.name ?? 'current provider';
      const confirmed = window.confirm(
        `Switch default AI provider from ${currentProvider} to ${integration.name} and set model to ${trimmedTarget}?`,
      );
      if (!confirmed) {
        return;
      }
    }

    setQuickModelSavingId(integration.id);
    setQuickModelError(null);
    setSaveError(null);
    try {
      await putIntegrationCredentials(integration.id, {
        revision: settingsRevision,
        fields: {
          default_model: trimmedTarget,
        },
      });

      await loadData(false);
      setSaveSuccess(`Model updated to ${trimmedTarget} for ${integration.name}.`);
      setQuickModelDrafts((prev) => {
        const next = { ...prev };
        delete next[integration.id];
        return next;
      });
    } catch (err: unknown) {
      const message = err instanceof Error ? err.message : 'Failed to update model';
      if (message.includes('API 409')) {
        await loadData(false);
        setQuickModelError(
          'Configuration changed elsewhere. Refreshed latest settings; choose the model again.',
        );
      } else {
        setQuickModelError(message);
      }
    } finally {
      setQuickModelSavingId(null);
    }
  };

  const categories = [
    'all',
    ...Array.from(new Set(integrations.map((i) => i.category))).sort(),
  ];

  const filtered =
    activeCategory === 'all'
      ? integrations
      : integrations.filter((i) => i.category === activeCategory);

  // Group by category for display
  const grouped = filtered.reduce<Record<string, Integration[]>>((acc, item) => {
    const key = item.category;
    if (!acc[key]) acc[key] = [];
    acc[key].push(item);
    return acc;
  }, {});

  if (error) {
    return (
      <div className="p-6">
        <div className="rounded-lg bg-red-900/30 border border-red-700 p-4 text-red-300">
          Failed to load integrations: {error}
        </div>
      </div>
    );
  }

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
      <div className="flex items-center gap-2">
        <Puzzle className="h-5 w-5 text-blue-400" />
        <h2 className="text-base font-semibold text-white">
          Integrations ({integrations.length})
        </h2>
      </div>

      {saveSuccess && (
        <div className="rounded-lg bg-green-900/30 border border-green-700 p-3 text-sm text-green-300">
          {saveSuccess}
        </div>
      )}

      {quickModelError && (
        <div className="rounded-lg bg-red-900/30 border border-red-700 p-3 text-sm text-red-300">
          {quickModelError}
        </div>
      )}

      {/* Category Filter Tabs */}
      <div className="flex flex-wrap gap-2">
        {categories.map((cat) => (
          <button
            key={cat}
            onClick={() => setActiveCategory(cat)}
            className={`px-3 py-1.5 rounded-lg text-sm font-medium transition-colors capitalize ${
              activeCategory === cat
                ? 'bg-blue-600 text-white'
                : 'bg-gray-900 text-gray-400 border border-gray-700 hover:bg-gray-800 hover:text-white'
            }`}
          >
            {cat === 'all' ? 'All' : formatCategory(cat)}
          </button>
        ))}
      </div>

      {/* Grouped Integration Cards */}
      {Object.keys(grouped).length === 0 ? (
        <div className="bg-gray-900 rounded-xl border border-gray-800 p-8 text-center">
          <Puzzle className="h-10 w-10 text-gray-600 mx-auto mb-3" />
          <p className="text-gray-400">No integrations found.</p>
        </div>
      ) : (
        Object.entries(grouped)
          .sort(([a], [b]) => a.localeCompare(b))
          .map(([category, items]) => (
            <div key={category}>
              <h3 className="text-sm font-semibold text-gray-400 uppercase tracking-wider mb-3 capitalize">
                {formatCategory(category)}
              </h3>
              <div className="grid grid-cols-1 md:grid-cols-2 xl:grid-cols-3 gap-4">
                {items.map((integration) => {
                  const badge = statusBadge(integration.status);
                  const BadgeIcon = badge.icon;
                  const editable = settingsByName[integration.name];
                  const isAiIntegration = !!editable?.activates_default_provider;
                  const isActiveDefaultProvider =
                    !!editable &&
                    isAiIntegration &&
                    editable.id === activeAiIntegrationId;
                  const modelField = editable ? modelFieldFor(editable) : undefined;
                  const modelOptions =
                    editable && modelField ? modelOptionsForField(editable.id, modelField) : [];
                  const currentModel =
                    editable && isAiIntegration
                      ? modelValueFor(editable, isActiveDefaultProvider)
                      : null;
                  const fallbackModel =
                    editable && isAiIntegration ? fallbackModelFor(editable) : null;
                  const modelSummary = currentModel
                    ? currentModel
                    : fallbackModel
                      ? `default: ${fallbackModel}`
                      : 'default';
                  const modelBaseline = currentModel ?? fallbackModel ?? '';
                  const quickDraft = editable
                    ? quickModelDrafts[editable.id] ?? modelBaseline
                    : '';
                  const quickOptions = [
                    ...(currentModel && !modelOptions.includes(currentModel)
                      ? [currentModel]
                      : []),
                    ...modelOptions,
                  ];
                  const showQuickModelControls =
                    !!editable &&
                    editable.configured &&
                    isAiIntegration &&
                    quickOptions.length > 0;

                  return (
                    <div
                      key={integration.name}
                      className={`bg-gray-900 rounded-xl border p-5 transition-colors ${
                        isActiveDefaultProvider
                          ? 'border-green-700/70 bg-gradient-to-b from-green-950/20 to-gray-900'
                          : 'border-gray-800 hover:border-gray-700'
                      }`}
                    >
                      <div className="flex items-start justify-between gap-3">
                        <div className="min-w-0">
                          <h4 className="text-sm font-semibold text-white truncate">
                            {integration.name}
                          </h4>
                          <p className="text-sm text-gray-400 mt-1 line-clamp-2">
                            {integration.description}
                          </p>
                        </div>
                        <div className="flex items-center gap-1.5 flex-wrap justify-end">
                          {isAiIntegration && editable?.configured && (
                            <span
                              className={`flex-shrink-0 inline-flex items-center gap-1 px-2 py-1 rounded-full text-xs font-medium border ${
                                isActiveDefaultProvider
                                  ? 'bg-emerald-900/40 text-emerald-300 border-emerald-700/60'
                                  : 'bg-gray-800 text-gray-300 border-gray-700'
                              }`}
                            >
                              {isActiveDefaultProvider ? 'Default' : 'Configured'}
                            </span>
                          )}
                          <span
                            className={`flex-shrink-0 inline-flex items-center gap-1 px-2 py-1 rounded-full text-xs font-medium border ${badge.classes}`}
                          >
                            <BadgeIcon className="h-3 w-3" />
                            {badge.label}
                          </span>
                        </div>
                      </div>

                      {editable && isAiIntegration && editable.configured && (
                        <div className="mt-3 rounded-lg border border-gray-800 bg-gray-950/50 p-3 space-y-2">
                          <div className="flex items-center justify-between gap-2">
                            <span className="text-[11px] uppercase tracking-wider text-gray-500">
                              Current model
                            </span>
                            <span className="text-xs text-gray-200 truncate" title={modelSummary}>
                              {modelSummary}
                            </span>
                          </div>

                          {showQuickModelControls && editable && (
                            <div className="space-y-1">
                              <div className="flex items-center gap-2">
                                <select
                                  value={quickDraft}
                                  onChange={(e) =>
                                    setQuickModelDrafts((prev) => ({
                                      ...prev,
                                      [editable.id]: e.target.value,
                                    }))
                                  }
                                  disabled={quickModelSavingId === editable.id}
                                  className="min-w-0 flex-1 px-2.5 py-1.5 rounded-lg bg-gray-950 border border-gray-700 text-xs text-gray-200 focus:outline-none focus:ring-2 focus:ring-blue-500 focus:border-transparent disabled:opacity-50"
                                >
                                  {quickOptions.map((option) => (
                                    <option key={option} value={option}>
                                      {option}
                                    </option>
                                  ))}
                                </select>
                                <button
                                  onClick={() =>
                                    editable &&
                                    void saveQuickModel(
                                      editable,
                                      quickDraft,
                                      modelBaseline,
                                      isActiveDefaultProvider,
                                    )
                                  }
                                  disabled={
                                    quickModelSavingId === editable.id ||
                                    !quickDraft ||
                                    quickDraft === modelBaseline
                                  }
                                  className="px-2.5 py-1.5 rounded-lg text-xs font-medium bg-blue-600 hover:bg-blue-700 text-white transition-colors disabled:opacity-50"
                                >
                                  {quickModelSavingId === editable.id ? 'Saving...' : 'Apply'}
                                </button>
                              </div>
                              <p className="text-[11px] text-gray-500">
                                For custom model IDs, use Edit Keys.
                              </p>
                            </div>
                          )}
                        </div>
                      )}

                      {editable && (
                        <div className="mt-4 pt-4 border-t border-gray-800 flex items-center justify-between gap-3">
                          <div className="text-xs text-gray-400">
                            {editable.configured
                              ? editable.activates_default_provider
                                ? isActiveDefaultProvider
                                  ? 'Default provider configured'
                                  : 'Provider configured'
                                : 'Credentials configured'
                              : 'Credentials not configured'}
                          </div>
                          <button
                            onClick={() => openEditor(editable)}
                            className="inline-flex items-center gap-1.5 px-3 py-1.5 rounded-lg border border-blue-700/70 bg-blue-900/30 hover:bg-blue-900/50 text-blue-300 text-xs font-medium transition-colors"
                          >
                            <KeyRound className="h-3.5 w-3.5" />
                            {editable.configured ? 'Edit Keys' : 'Configure'}
                          </button>
                        </div>
                      )}
                    </div>
                  );
                })}
              </div>
            </div>
          ))
      )}

      {activeEditor && (
        <div
          className="fixed inset-0 z-50 bg-black/70 flex items-center justify-center p-4"
          onMouseDown={(event) => {
            if (event.target === event.currentTarget) {
              closeEditor();
            }
          }}
        >
          <div className="w-full max-w-lg bg-gray-900 border border-gray-800 rounded-xl shadow-xl">
            <div className="px-5 py-4 border-b border-gray-800 flex items-center justify-between gap-3">
              <div>
                <h3 className="text-sm font-semibold text-white">
                  Configure {activeEditor.name}
                </h3>
                <p className="text-xs text-gray-400 mt-0.5">
                  {activeEditor.configured
                    ? 'Enter only fields you want to update.'
                    : 'Enter required fields to configure this integration.'}
                </p>
              </div>
              <button
                onClick={closeEditor}
                disabled={saving}
                className="text-gray-400 hover:text-white transition-colors disabled:opacity-50"
                aria-label="Close"
              >
                <X className="h-4 w-4" />
              </button>
            </div>

            <div className="p-5 space-y-4">
              {activeEditor.activates_default_provider && (
                <div className="rounded-lg border border-blue-800 bg-blue-950/30 p-3 text-xs text-blue-200">
                  Saving here updates credentials and switches your default AI provider to{' '}
                  <strong>{activeEditor.name}</strong>. For advanced provider settings, use{' '}
                  <Link to="/config" className="underline underline-offset-2 hover:text-blue-100">
                    Configuration
                  </Link>
                  .
                </div>
              )}

              {activeEditor.fields.map((field) => (
                (() => {
                  const selectOptions = modelOptionsForField(activeEditor.id, field);
                  const isSelectField = selectOptions.length > 0;
                  const isSecretField = field.input_type === 'secret';
                  const maskedSecretValue = isSecretField
                    ? (field.masked_value || (field.has_value ? '••••••••' : undefined))
                    : undefined;
                  const activeEditorIsDefaultProvider =
                    activeEditor.activates_default_provider &&
                    activeEditor.id === activeAiIntegrationId;
                  const currentModelValue =
                    field.current_value?.trim() ||
                    (activeEditorIsDefaultProvider ? runtimeStatus?.model?.trim() || '' : '');
                  const keepCurrentLabel = currentModelValue
                    ? `Keep current model (${currentModelValue})`
                    : 'Keep current model';

                  return (
                    <div key={field.key}>
                      <label className="flex items-center gap-2 text-sm font-medium text-gray-300 mb-1.5">
                        <span>{field.label}</span>
                        {field.required && <span className="text-red-400">*</span>}
                        {field.has_value && (
                          <span className="text-[11px] text-green-400 bg-green-900/30 border border-green-800 px-1.5 py-0.5 rounded">
                            Configured
                          </span>
                        )}
                      </label>
                      {isSelectField ? (
                        <div className="space-y-2">
                          <select
                            value={fieldValues[field.key] ?? (field.has_value ? SELECT_KEEP : '')}
                            onChange={(e) => updateField(field.key, e.target.value)}
                            className="w-full px-3 py-2 rounded-lg bg-gray-950 border border-gray-700 text-sm text-gray-200 focus:outline-none focus:ring-2 focus:ring-blue-500 focus:border-transparent"
                          >
                            {field.has_value ? (
                              <option value={SELECT_KEEP}>{keepCurrentLabel}</option>
                            ) : (
                              <option value="" disabled>
                                Select a recommended model
                              </option>
                            )}
                            {selectOptions.map((option) => (
                              <option key={option} value={option}>
                                {option}
                              </option>
                            ))}
                            <option value={SELECT_CUSTOM}>Custom model...</option>
                            {field.has_value && <option value={SELECT_CLEAR}>Clear current model</option>}
                          </select>

                          {fieldValues[field.key] === SELECT_CUSTOM && (
                            <input
                              type="text"
                              value={customFieldValues[field.key] ?? ''}
                              onChange={(e) => updateCustomField(field.key, e.target.value)}
                              placeholder={customModelFormatHint(activeEditor.id)}
                              className="w-full px-3 py-2 rounded-lg bg-gray-950 border border-gray-700 text-sm text-gray-200 focus:outline-none focus:ring-2 focus:ring-blue-500 focus:border-transparent"
                            />
                          )}

                          <p className="text-[11px] text-gray-500">
                            Pick a recommended model or choose Custom model. {customModelFormatHint(activeEditor.id)}.
                          </p>
                        </div>
                      ) : (
                        <div className="space-y-2">
                          {maskedSecretValue && (
                            <p className="text-[11px] text-gray-500">
                              Current value: <span className="font-mono text-gray-300">{maskedSecretValue}</span>
                            </p>
                          )}
                          <input
                            type={isSecretField ? 'password' : 'text'}
                            value={fieldValues[field.key] ?? ''}
                            onChange={(e) => updateField(field.key, e.target.value)}
                            placeholder={
                              field.required
                                ? field.has_value
                                  ? 'Enter a new value to replace current'
                                  : 'Enter value'
                                : field.has_value
                                  ? 'Type new value, or leave empty to keep current'
                                  : 'Optional'
                            }
                            className="w-full px-3 py-2 rounded-lg bg-gray-950 border border-gray-700 text-sm text-gray-200 focus:outline-none focus:ring-2 focus:ring-blue-500 focus:border-transparent"
                          />
                        </div>
                      )}
                    </div>
                  );
                })()
              ))}

              {saveError && (
                <div className="rounded-lg bg-red-900/30 border border-red-700 p-3 text-sm text-red-300">
                  {saveError}
                </div>
              )}
            </div>

            <div className="px-5 py-4 border-t border-gray-800 flex items-center justify-end gap-2">
              <button
                onClick={closeEditor}
                disabled={saving}
                className="px-4 py-2 rounded-lg text-sm border border-gray-700 text-gray-300 hover:bg-gray-800 transition-colors disabled:opacity-50"
              >
                Cancel
              </button>
              <button
                onClick={saveCredentials}
                disabled={saving}
                className="px-4 py-2 rounded-lg text-sm font-medium bg-blue-600 hover:bg-blue-700 text-white transition-colors disabled:opacity-50"
              >
                {saving
                  ? 'Saving...'
                  : activeEditor.activates_default_provider
                    ? 'Save & Activate'
                    : 'Save Keys'}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
