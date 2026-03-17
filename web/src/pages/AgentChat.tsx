import { useState, useEffect, useRef, useCallback } from 'react';
import { Send, Bot, User, AlertCircle, Copy, Check } from 'lucide-react';
import type { WsMessage } from '@/types/api';
import { WebSocketClient } from '@/lib/ws';
import { generateUUID } from '@/lib/uuid';
import {
  getProviders,
  getProviderModels,
  getProviderModelConfig,
  putProviderModelConfig,
  adminShutdown,
} from '@/lib/api';
import type {
  ProviderListItem,
  ProviderModelCatalog,
  ProviderModelOption,
} from '@/types/api';

interface ChatMessage {
  id: string;
  role: 'user' | 'agent';
  content: string;
  timestamp: Date;
}

export default function AgentChat() {
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [input, setInput] = useState('');
  const [typing, setTyping] = useState(false);
  const [connected, setConnected] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [providers, setProviders] = useState<ProviderListItem[]>([]);
  const [modelOptions, setModelOptions] = useState<ProviderModelOption[]>([]);
  const [modelCatalog, setModelCatalog] = useState<ProviderModelCatalog | null>(null);
  const [selectedProvider, setSelectedProvider] = useState('');
  const [selectedModel, setSelectedModel] = useState('');
  const [modelFilter, setModelFilter] = useState('');
  const [modelMenuOpen, setModelMenuOpen] = useState(false);
  const [modelSaving, setModelSaving] = useState(false);
  const [modelError, setModelError] = useState<string | null>(null);
  const [modelSaved, setModelSaved] = useState<string | null>(null);
  const [initialProvider, setInitialProvider] = useState('');
  const [initialModel, setInitialModel] = useState('');
  const [loadingModels, setLoadingModels] = useState(false);
  const [restarting, setRestarting] = useState(false);
  const [restartError, setRestartError] = useState<string | null>(null);
  const [restartMessage, setRestartMessage] = useState<string | null>(null);

  const wsRef = useRef<WebSocketClient | null>(null);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const [copiedId, setCopiedId] = useState<string | null>(null);
  const pendingContentRef = useRef('');
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
    setModelError(null);
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
      setModelError(err instanceof Error ? err.message : 'Failed to load models');
      setModelCatalog(null);
      setModelOptions([]);
    } finally {
      if (modelRequestRef.current === requestId) {
        setLoadingModels(false);
      }
    }
  };

  useEffect(() => {
    const ws = new WebSocketClient();

    ws.onOpen = () => {
      setConnected(true);
      setError(null);
    };

    ws.onClose = () => {
      setConnected(false);
    };

    ws.onError = () => {
      setError('Connection error. Attempting to reconnect...');
    };

    ws.onMessage = (msg: WsMessage) => {
      switch (msg.type) {
        case 'chunk':
          setTyping(true);
          pendingContentRef.current += msg.content ?? '';
          break;

        case 'message':
        case 'done': {
          const content = msg.full_response ?? msg.content ?? pendingContentRef.current;
          if (content) {
            setMessages((prev) => [
              ...prev,
              {
                id: generateUUID(),
                role: 'agent',
                content,
                timestamp: new Date(),
              },
            ]);
          }
          pendingContentRef.current = '';
          setTyping(false);
          break;
        }

        case 'tool_call':
          setMessages((prev) => [
            ...prev,
            {
              id: generateUUID(),
              role: 'agent',
              content: `[Tool Call] ${msg.name ?? 'unknown'}(${JSON.stringify(msg.args ?? {})})`,
              timestamp: new Date(),
            },
          ]);
          break;

        case 'tool_result':
          setMessages((prev) => [
            ...prev,
            {
              id: generateUUID(),
              role: 'agent',
              content: `[Tool Result] ${msg.output ?? ''}`,
              timestamp: new Date(),
            },
          ]);
          break;

        case 'error':
          setMessages((prev) => [
            ...prev,
            {
              id: generateUUID(),
              role: 'agent',
              content: `[Error] ${msg.message ?? 'Unknown error'}`,
              timestamp: new Date(),
            },
          ]);
          setTyping(false);
          pendingContentRef.current = '';
          break;
      }
    };

    ws.connect();
    wsRef.current = ws;

    return () => {
      ws.disconnect();
    };
  }, []);

  useEffect(() => {
    let cancelled = false;

    const load = async () => {
      try {
        const [providerList, providerConfig] = await Promise.all([
          getProviders(),
          getProviderModelConfig(),
        ]);
        if (cancelled) return;
        const defaultProvider = providerConfig.default_provider || 'openrouter';
        setProviders(ensureProviderOption(providerList, defaultProvider));
        setSelectedProvider(defaultProvider);
        await loadModels(defaultProvider, providerConfig.default_model, true);
      } catch (err: unknown) {
        if (cancelled) return;
        setModelError(err instanceof Error ? err.message : 'Failed to load provider settings');
      }
    };

    load();
    return () => {
      cancelled = true;
      modelRequestRef.current += 1;
    };
  }, []);

  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [messages, typing]);

  const handleSend = () => {
    const trimmed = input.trim();
    if (!trimmed || !wsRef.current?.connected) return;

    setMessages((prev) => [
      ...prev,
      {
        id: generateUUID(),
        role: 'user',
        content: trimmed,
        timestamp: new Date(),
      },
    ]);

    try {
      wsRef.current.sendMessage(trimmed);
      setTyping(true);
      pendingContentRef.current = '';
    } catch {
      setError('Failed to send message. Please try again.');
    }

    setInput('');
    if (inputRef.current) {
      inputRef.current.style.height = 'auto';
      inputRef.current.focus();
    }
  };

  const handleProviderChange = async (nextProvider: string) => {
    setSelectedProvider(nextProvider);
    setSelectedModel('');
    setModelOptions([]);
    setModelCatalog(null);
    setModelSaved(null);
    setModelError(null);
    setModelFilter('');
    await loadModels(nextProvider, null);
  };

  const handleModelSelect = (nextModel: string) => {
    setSelectedModel(nextModel);
    setModelFilter('');
    setModelSaved(null);
    setModelError(null);
    setModelMenuOpen(false);
  };

  const handleModelInput = (nextModel: string) => {
    setSelectedModel(nextModel);
    setModelFilter(nextModel);
    setModelSaved(null);
    setModelError(null);
    setModelMenuOpen(true);
  };

  const handleModelSave = async () => {
    const provider = selectedProvider.trim();
    const model = selectedModel.trim();
    if (!provider || !model) return;
    setModelSaving(true);
    setModelError(null);
    setModelSaved(null);
    try {
      await putProviderModelConfig({ provider, model });
      setModelSaved('Saved. Restart the gateway to apply.');
      setInitialProvider(provider);
      setInitialModel(model);
    } catch (err: unknown) {
      setModelError(err instanceof Error ? err.message : 'Failed to save selection');
    } finally {
      setModelSaving(false);
    }
  };

  const handleRestart = async () => {
    const confirmed = window.confirm(
      'This will stop the gateway. If you are not running under a supervisor, you will need to start it again manually. Continue?',
    );
    if (!confirmed) return;
    setRestarting(true);
    setRestartError(null);
    setRestartMessage(null);
    try {
      const result = await adminShutdown();
      setRestartMessage(
        result.message ||
          'Gateway shutdown initiated. If it does not restart automatically, run `zeroclaw gateway`.',
      );
    } catch (err: unknown) {
      setRestartError(err instanceof Error ? err.message : 'Failed to restart gateway');
    } finally {
      setRestarting(false);
    }
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  };

  const handleTextareaChange = (e: React.ChangeEvent<HTMLTextAreaElement>) => {
    setInput(e.target.value);
    e.target.style.height = 'auto';
    e.target.style.height = `${Math.min(e.target.scrollHeight, 200)}px`;
  };

  const handleCopy = useCallback((msgId: string, content: string) => {
    navigator.clipboard.writeText(content).then(() => {
      setCopiedId(msgId);
      setTimeout(() => setCopiedId((prev) => (prev === msgId ? null : prev)), 2000);
    });
  }, []);

  useEffect(() => {
    if (!modelSaved) return;
    const timer = setTimeout(() => setModelSaved(null), 4000);
    return () => clearTimeout(timer);
  }, [modelSaved]);

  useEffect(() => {
    if (!restartMessage) return;
    const timer = setTimeout(() => setRestartMessage(null), 4000);
    return () => clearTimeout(timer);
  }, [restartMessage]);

  const modelDirty =
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

  return (
    <div className="flex flex-col h-[calc(100vh-3.5rem)]">
      {/* Connection status bar */}
      {error && (
        <div className="px-4 py-2 bg-red-900/30 border-b border-red-700 flex items-center gap-2 text-sm text-red-300">
          <AlertCircle className="h-4 w-4 flex-shrink-0" />
          {error}
        </div>
      )}

      {/* Messages area */}
      <div className="flex-1 overflow-y-auto p-4 space-y-4">
        {messages.length === 0 && (
          <div className="flex flex-col items-center justify-center h-full text-gray-500">
            <Bot className="h-12 w-12 mb-3 text-gray-600" />
            <p className="text-lg font-medium">ZeroClaw Agent</p>
            <p className="text-sm mt-1">Send a message to start the conversation</p>
          </div>
        )}

        {messages.map((msg) => (
          <div
            key={msg.id}
            className={`group flex items-start gap-3 ${
              msg.role === 'user' ? 'flex-row-reverse' : ''
            }`}
          >
            <div
              className={`flex-shrink-0 w-8 h-8 rounded-full flex items-center justify-center ${
                msg.role === 'user'
                  ? 'bg-blue-600'
                  : 'bg-gray-700'
              }`}
            >
              {msg.role === 'user' ? (
                <User className="h-4 w-4 text-white" />
              ) : (
                <Bot className="h-4 w-4 text-white" />
              )}
            </div>
            <div className="relative max-w-[75%]">
              <div
                className={`rounded-xl px-4 py-3 ${
                  msg.role === 'user'
                    ? 'bg-blue-600 text-white'
                    : 'bg-gray-800 text-gray-100 border border-gray-700'
                }`}
              >
                <p className="text-sm whitespace-pre-wrap break-words">{msg.content}</p>
                <p
                  className={`text-xs mt-1 ${
                    msg.role === 'user' ? 'text-blue-200' : 'text-gray-500'
                  }`}
                >
                  {msg.timestamp.toLocaleTimeString()}
                </p>
              </div>
              <button
                onClick={() => handleCopy(msg.id, msg.content)}
                aria-label="Copy message"
                className="absolute top-1 right-1 opacity-0 group-hover:opacity-100 transition-opacity p-1 rounded bg-gray-700 hover:bg-gray-600 text-gray-400 hover:text-white"
              >
                {copiedId === msg.id ? (
                  <Check className="h-3.5 w-3.5 text-green-400" />
                ) : (
                  <Copy className="h-3.5 w-3.5" />
                )}
              </button>
            </div>
          </div>
        ))}

        {typing && (
          <div className="flex items-start gap-3">
            <div className="flex-shrink-0 w-8 h-8 rounded-full bg-gray-700 flex items-center justify-center">
              <Bot className="h-4 w-4 text-white" />
            </div>
            <div className="bg-gray-800 border border-gray-700 rounded-xl px-4 py-3">
              <div className="flex items-center gap-1">
                <span className="w-2 h-2 bg-gray-400 rounded-full animate-bounce" style={{ animationDelay: '0ms' }} />
                <span className="w-2 h-2 bg-gray-400 rounded-full animate-bounce" style={{ animationDelay: '150ms' }} />
                <span className="w-2 h-2 bg-gray-400 rounded-full animate-bounce" style={{ animationDelay: '300ms' }} />
              </div>
              <p className="text-xs text-gray-500 mt-1">Typing...</p>
            </div>
          </div>
        )}

        <div ref={messagesEndRef} />
      </div>

      {/* Model selector */}
      <div className="border-t border-gray-800 bg-gray-900/70 px-4 py-3">
        <div className="flex flex-col gap-3 max-w-4xl mx-auto">
          <div className="flex flex-col gap-3 sm:flex-row sm:items-end sm:gap-4">
            <div className="flex-1 grid gap-3 sm:grid-cols-2">
              <div>
                <label className="block text-xs font-medium text-gray-400 mb-1">
                  Provider
                </label>
                <select
                  value={selectedProvider}
                  onChange={(e) => handleProviderChange(e.target.value)}
                  className="w-full bg-gray-800 border border-gray-700 rounded-lg px-3 py-2 text-sm text-white appearance-none focus:outline-none focus:ring-2 focus:ring-blue-500"
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
                  <p className="text-xs text-gray-500 mt-1">
                    Resolved provider: {resolvedProviderLabel}
                  </p>
                )}
              </div>

              <div>
                <label className="block text-xs font-medium text-gray-400 mb-1">
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
                    className="w-full bg-gray-800 border border-gray-700 rounded-lg px-3 py-2 text-sm text-white placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-blue-500"
                  />
                  {modelMenuOpen && (
                    <div className="absolute z-20 mt-1 w-full max-h-64 overflow-y-auto rounded-lg border border-gray-700 bg-gray-800 shadow-lg">
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
                            className="w-full text-left px-3 py-2 text-sm text-gray-200 hover:bg-gray-700"
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
                  <p className="text-xs text-gray-500 mt-1">
                    Models source: {modelSourceLabel}
                  </p>
                )}
              </div>
            </div>

            <div className="flex flex-col gap-2 sm:min-w-[180px]">
              <button
                onClick={handleModelSave}
                disabled={!modelDirty || modelSaving}
                className="flex items-center justify-center gap-2 bg-blue-600 hover:bg-blue-700 text-white text-sm font-medium px-4 py-2 rounded-lg transition-colors disabled:opacity-50"
              >
                {modelSaving ? 'Saving...' : 'Save selection'}
              </button>
              <button
                onClick={handleRestart}
                disabled={restarting}
                className="flex items-center justify-center gap-2 border border-gray-700 bg-gray-900 text-gray-200 text-sm font-medium px-4 py-2 rounded-lg transition-colors hover:border-gray-600 hover:bg-gray-800 disabled:opacity-50"
              >
                {restarting ? 'Restarting...' : 'Restart gateway (requires supervisor)'}
              </button>
            </div>
          </div>

          {modelSaved && (
            <div className="rounded-lg bg-green-900/30 border border-green-700 p-2 text-xs text-green-300">
              {modelSaved}
            </div>
          )}
          {modelError && (
            <div className="rounded-lg bg-red-900/30 border border-red-700 p-2 text-xs text-red-300">
              {modelError}
            </div>
          )}
          {restartMessage && (
            <div className="rounded-lg bg-blue-900/30 border border-blue-700 p-2 text-xs text-blue-300">
              {restartMessage}
            </div>
          )}
          {restartError && (
            <div className="rounded-lg bg-red-900/30 border border-red-700 p-2 text-xs text-red-300">
              {restartError}
            </div>
          )}
          <p className="text-[11px] text-gray-500">
            Changes update config only. Restart the gateway to apply in the running runtime.
          </p>
        </div>
      </div>

      {/* Input area */}
      <div className="border-t border-gray-800 bg-gray-900 p-4">
        <div className="flex items-end gap-3 max-w-4xl mx-auto">
          <div className="flex-1 relative">
            <textarea
              ref={inputRef}
              rows={1}
              value={input}
              onChange={handleTextareaChange}
              onKeyDown={handleKeyDown}
              placeholder={connected ? 'Type a message...' : 'Connecting...'}
              disabled={!connected}
              className="w-full bg-gray-800 border border-gray-700 rounded-xl px-4 py-3 text-sm text-white placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-blue-500 focus:border-transparent disabled:opacity-50 resize-none overflow-y-auto"
              style={{ minHeight: '44px', maxHeight: '200px' }}
            />
          </div>
          <button
            onClick={handleSend}
            disabled={!connected || !input.trim()}
            className="flex-shrink-0 bg-blue-600 hover:bg-blue-700 disabled:bg-gray-700 disabled:text-gray-500 text-white rounded-xl p-3 transition-colors"
          >
            <Send className="h-5 w-5" />
          </button>
        </div>
        <div className="flex items-center justify-center mt-2 gap-2">
          <span
            className={`inline-block h-2 w-2 rounded-full ${
              connected ? 'bg-green-500' : 'bg-red-500'
            }`}
          />
          <span className="text-xs text-gray-500">
            {connected ? 'Connected' : 'Disconnected'}
          </span>
        </div>
      </div>
    </div>
  );
}
