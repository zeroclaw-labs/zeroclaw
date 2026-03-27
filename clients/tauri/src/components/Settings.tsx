import { useState, useCallback, useEffect } from "react";
import { t, type Locale } from "../lib/i18n";
import { apiClient, type SyncStatus, type PlatformInfo, type DeviceInfo } from "../lib/api";
import { isTauri } from "../lib/tauri-bridge";

interface SettingsProps {
  locale: Locale;
  isConnected: boolean;
  onLocaleChange: (locale: Locale) => void;
  onBack: () => void;
  onLogout: () => void;
}

const API_KEY_STORAGE_PREFIX = "zeroclaw_api_key_";
const STORAGE_KEY_LLM_PROVIDER = "zeroclaw_llm_provider";
const STORAGE_KEY_LLM_MODEL = "zeroclaw_llm_model";

interface ModelOption {
  id: string;
  label: string;
  tier: string;
}

const MODEL_OPTIONS: Record<string, ModelOption[]> = {
  claude: [
    { id: "claude-opus-4-6", label: "Claude Opus 4.6", tier: "Premium" },
    { id: "claude-sonnet-4-6", label: "Claude Sonnet 4.6", tier: "Standard" },
    { id: "claude-haiku-4-5-20251001", label: "Claude Haiku 4.5", tier: "Fast" },
  ],
  openai: [
    { id: "gpt-5.4", label: "GPT-5.4", tier: "Premium" },
    { id: "gpt-5-mini", label: "GPT-5 Mini", tier: "Standard" },
    { id: "gpt-4.1", label: "GPT-4.1", tier: "Standard" },
    { id: "gpt-4.1-mini", label: "GPT-4.1 Mini", tier: "Fast" },
  ],
  gemini: [
    { id: "gemini-3.1-pro-preview", label: "Gemini 3.1 Pro Preview", tier: "Premium" },
    { id: "gemini-3.1-flash-lite-preview", label: "Gemini 3.1 Flash-Lite", tier: "Standard" },
    { id: "gemini-3.1-flash-image-preview", label: "Gemini 3.1 Flash Image Preview", tier: "Standard" },
    { id: "gemini-3-flash-preview", label: "Gemini 3 Flash Preview", tier: "Standard" },
    { id: "gemini-3-pro-image-preview", label: "Gemini 3 Pro Image Preview", tier: "Standard" },
    { id: "gemini-2.5-pro", label: "Gemini 2.5 Pro", tier: "Standard" },
    { id: "gemini-2.5-flash", label: "Gemini 2.5 Flash", tier: "Fast" },
    { id: "gemini-2.5-flash-lite", label: "Gemini 2.5 Flash-Lite", tier: "Fast" },
  ],
};

const PROVIDER_LIST = [
  { id: "claude", labelKey: "llm_provider_claude" as const },
  { id: "openai", labelKey: "llm_provider_openai" as const },
  { id: "gemini", labelKey: "llm_provider_gemini" as const },
];

function getStoredApiKey(provider: string): string {
  return localStorage.getItem(`${API_KEY_STORAGE_PREFIX}${provider}`) || "";
}

function setStoredApiKey(provider: string, key: string): void {
  if (key) {
    localStorage.setItem(`${API_KEY_STORAGE_PREFIX}${provider}`, key);
  } else {
    localStorage.removeItem(`${API_KEY_STORAGE_PREFIX}${provider}`);
  }
}

function getStoredProvider(): string {
  const stored = localStorage.getItem(STORAGE_KEY_LLM_PROVIDER);
  if (stored) return stored;
  // Auto-select provider priority: Anthropic > OpenAI > Gemini (free fallback)
  if (localStorage.getItem(`${API_KEY_STORAGE_PREFIX}anthropic`)) return "claude";
  if (localStorage.getItem(`${API_KEY_STORAGE_PREFIX}openai`)) return "openai";
  return "gemini";
}

function getStoredModel(): string {
  return localStorage.getItem(STORAGE_KEY_LLM_MODEL) || "gemini-3.1-pro-preview";
}

export function Settings({ locale, isConnected, onLocaleChange, onBack, onLogout }: SettingsProps) {
  const [serverUrl, setServerUrl] = useState(apiClient.getServerUrl());
  const [isHealthChecking, setIsHealthChecking] = useState(false);
  const [message, setMessage] = useState<{ type: "success" | "error"; text: string } | null>(null);
  const [syncStatus, setSyncStatus] = useState<SyncStatus | null>(null);
  const [platformInfo, setPlatformInfo] = useState<PlatformInfo | null>(null);
  const [isSyncing, setIsSyncing] = useState(false);
  const [devices, setDevices] = useState<DeviceInfo[]>([]);
  const [editingPairingDevice, setEditingPairingDevice] = useState<string | null>(null);
  const [newPairingCode, setNewPairingCode] = useState("");
  const [claudeKey, setClaudeKey] = useState(() => getStoredApiKey("anthropic"));
  const [openaiKey, setOpenaiKey] = useState(() => getStoredApiKey("openai"));
  const [geminiKey, setGeminiKey] = useState(() => getStoredApiKey("gemini"));
  const [creditBalance, setCreditBalance] = useState<number | null>(null);
  const [selectedProvider, setSelectedProvider] = useState(getStoredProvider);
  const [selectedModel, setSelectedModel] = useState(getStoredModel);
  const [showAdvanced, setShowAdvanced] = useState(false);
  const inTauri = isTauri();
  const user = apiClient.getUser();
  const isLoggedIn = apiClient.isLoggedIn();
  const currentDeviceId = apiClient.getDeviceId();

  useEffect(() => {
    if (inTauri) {
      apiClient.getSyncStatus().then(setSyncStatus).catch(() => {});
      apiClient.getPlatformInfo().then(setPlatformInfo).catch(() => {});
    }
    if (isLoggedIn) {
      apiClient.getDevices().then(setDevices).catch(() => {});
      // Load credit balance from relay server (billing is server-side)
      apiClient.getCreditBalance()
        .then((b: number) => setCreditBalance(b || null))
        .catch(() => setCreditBalance(null));
    }
  }, [inTauri, isLoggedIn]);

  const clearMessage = useCallback(() => {
    setTimeout(() => setMessage(null), 5000);
  }, []);

  const handleSaveApiKey = useCallback(async (provider: string, key: string) => {
    // Save to localStorage (for UI state persistence)
    setStoredApiKey(provider, key);

    // Also sync the API key to the local MoA agent config.
    // This tells the local agent to use this key for LLM calls.
    try {
      await apiClient.saveApiKeyToAgent(provider, key);
      setMessage({
        type: "success",
        text: key ? t("api_key_saved", locale) : t("api_key_cleared", locale),
      });
    } catch {
      // Local agent might not be running — saved to localStorage only
      setMessage({
        type: "error",
        text: locale === "ko"
          ? "로컬 에이전트에 저장 실패 — 로컬 저장소에만 저장됨"
          : "Failed to save to local agent — saved to local storage only",
      });
    }
    clearMessage();
  }, [locale, clearMessage]);

  const handleServerUrlChange = useCallback(
    (url: string) => {
      setServerUrl(url);
      apiClient.setServerUrl(url);
    },
    [],
  );

  const handleProviderChange = useCallback((provider: string) => {
    setSelectedProvider(provider);
    localStorage.setItem(STORAGE_KEY_LLM_PROVIDER, provider);
    // Set default model for this provider (first = highest tier)
    const defaultModel = MODEL_OPTIONS[provider]?.[0]?.id || "";
    setSelectedModel(defaultModel);
    localStorage.setItem(STORAGE_KEY_LLM_MODEL, defaultModel);
    // Sync provider/model selection to local agent so server-side config stays current
    apiClient.saveProviderModelToAgent(provider, defaultModel).catch(() => {});
  }, []);

  const handleModelChange = useCallback((model: string) => {
    setSelectedModel(model);
    localStorage.setItem(STORAGE_KEY_LLM_MODEL, model);
    // Sync model selection to local agent
    apiClient.saveProviderModelToAgent(selectedProvider, model).catch(() => {});
  }, [selectedProvider]);

  const handleTriggerSync = useCallback(async () => {
    setIsSyncing(true);
    setMessage(null);
    try {
      const result = await apiClient.triggerFullSync();
      if (result) {
        setMessage({ type: "success", text: t("sync_triggered", locale) });
      }
      const status = await apiClient.getSyncStatus();
      setSyncStatus(status);
    } catch (err) {
      setMessage({
        type: "error",
        text: err instanceof Error ? err.message : t("sync_failed", locale),
      });
    } finally {
      setIsSyncing(false);
      clearMessage();
    }
  }, [locale, clearMessage]);

  const handleHealthCheck = useCallback(async () => {
    setIsHealthChecking(true);
    setMessage(null);

    try {
      // When user has no API key, check relay server; otherwise check local gateway
      if (apiClient.hasAnyLocalApiKey()) {
        const result = await apiClient.healthCheck();
        if (result.status === "ok") {
          setMessage({ type: "success", text: t("server_healthy", locale) });
        } else {
          setMessage({ type: "error", text: t("server_unreachable", locale) });
        }
      } else {
        // Check relay server health
        const controller = new AbortController();
        const timeout = setTimeout(() => controller.abort(), 5000);
        try {
          const res = await fetch(`${apiClient.getRelayUrl()}/health`, {
            signal: controller.signal,
          });
          if (res.ok) {
            setMessage({ type: "success", text: t("server_healthy", locale) });
          } else {
            setMessage({ type: "error", text: t("server_unreachable", locale) });
          }
        } finally {
          clearTimeout(timeout);
        }
      }
    } catch (err) {
      setMessage({
        type: "error",
        text: err instanceof Error ? err.message : t("server_unreachable", locale),
      });
    } finally {
      setIsHealthChecking(false);
      clearMessage();
    }
  }, [locale, clearMessage]);

  const handleLogout = useCallback(async () => {
    await onLogout();
  }, [onLogout]);

  const handleSetPairingCode = useCallback(async (deviceId: string) => {
    if (!newPairingCode.trim()) return;
    try {
      await apiClient.setDevicePairingCode(deviceId, newPairingCode.trim());
      setMessage({ type: "success", text: t("pairing_code_set", locale) });
      setEditingPairingDevice(null);
      setNewPairingCode("");
      // Refresh devices
      const updated = await apiClient.getDevices();
      setDevices(updated);
    } catch (err) {
      setMessage({
        type: "error",
        text: err instanceof Error ? err.message : "Failed",
      });
    }
    clearMessage();
  }, [newPairingCode, locale, clearMessage]);

  const handleRemovePairingCode = useCallback(async (deviceId: string) => {
    try {
      await apiClient.setDevicePairingCode(deviceId, null);
      setMessage({ type: "success", text: t("pairing_code_removed", locale) });
      const updated = await apiClient.getDevices();
      setDevices(updated);
    } catch (err) {
      setMessage({
        type: "error",
        text: err instanceof Error ? err.message : "Failed",
      });
    }
    clearMessage();
  }, [locale, clearMessage]);

  const handleRemoveDevice = useCallback(async (deviceId: string) => {
    try {
      await apiClient.removeDevice(deviceId);
      setMessage({ type: "success", text: t("device_removed", locale) });
      const updated = await apiClient.getDevices();
      setDevices(updated);
    } catch (err) {
      setMessage({
        type: "error",
        text: err instanceof Error ? err.message : "Failed",
      });
    }
    clearMessage();
  }, [locale, clearMessage]);

  const hasApiKey = apiClient.hasAnyLocalApiKey();
  const models = MODEL_OPTIONS[selectedProvider] || [];

  const formatLastSeen = (timestamp: number) => {
    const now = Date.now() / 1000;
    const diff = now - timestamp;
    if (diff < 120) return locale === "ko" ? "\uBC29\uAE08 \uC804" : "Just now";
    if (diff < 3600) return `${Math.floor(diff / 60)}${locale === "ko" ? "\uBD84 \uC804" : "m ago"}`;
    if (diff < 86400) return `${Math.floor(diff / 3600)}${locale === "ko" ? "\uC2DC\uAC04 \uC804" : "h ago"}`;
    return `${Math.floor(diff / 86400)}${locale === "ko" ? "\uC77C \uC804" : "d ago"}`;
  };

  return (
    <div className="settings-container">
      {/* Header */}
      <div className="settings-header">
        <button className="settings-back-btn" onClick={onBack} aria-label={t("back_to_chat", locale)}>
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <polyline points="15 18 9 12 15 6" />
          </svg>
        </button>
        <span className="settings-header-title">{t("settings", locale)}</span>
      </div>

      {/* Body */}
      <div className="settings-body">
        <div className="settings-inner">

          {/* Account section */}
          {isLoggedIn && user && (
            <div className="settings-section">
              <div className="settings-section-title">{t("account_info", locale)}</div>
              <div className="settings-card">
                <div className="settings-field">
                  <label className="settings-label">{t("username", locale)}</label>
                  <div className="settings-token-display">{user.username}</div>
                </div>
                <div className="settings-actions" style={{ marginTop: 12 }}>
                  <button className="settings-btn settings-btn-danger" onClick={handleLogout}>
                    {t("logout", locale)}
                  </button>
                </div>
              </div>
            </div>
          )}

          {/* Devices section */}
          {isLoggedIn && devices.length > 0 && (
            <div className="settings-section">
              <div className="settings-section-title">{t("my_devices", locale)}</div>
              <div className="settings-card">
                {devices.map((device) => {
                  const isLocal = device.device_id === currentDeviceId;
                  return (
                    <div key={device.device_id} className="settings-device-item">
                      <div className="settings-device-header">
                        <div className="settings-device-name">
                          {device.device_name}
                          {isLocal && (
                            <span className="device-badge device-badge-local">
                              {t("device_this", locale)}
                            </span>
                          )}
                        </div>
                        <div className={`device-status-mini ${device.is_online ? "online" : "offline"}`}>
                          <div className={`status-dot ${device.is_online ? "connected" : ""}`} />
                          <span>{device.is_online ? t("device_online", locale) : t("device_offline", locale)}</span>
                        </div>
                      </div>
                      <div className="settings-device-meta">
                        {device.platform && <span>{device.platform}</span>}
                        <span>{formatLastSeen(device.last_seen)}</span>
                      </div>

                      {/* Pairing code management */}
                      <div className="settings-device-pairing">
                        {editingPairingDevice === device.device_id ? (
                          <div className="settings-device-pairing-edit">
                            <input
                              className="settings-input"
                              type="password"
                              value={newPairingCode}
                              onChange={(e) => setNewPairingCode(e.target.value)}
                              onKeyDown={(e) => { if (e.key === "Enter") handleSetPairingCode(device.device_id); }}
                              placeholder={t("new_pairing_code", locale)}
                              autoFocus
                            />
                            <div className="settings-device-pairing-btns">
                              <button
                                className="settings-btn settings-btn-primary settings-btn-sm"
                                onClick={() => handleSetPairingCode(device.device_id)}
                                disabled={!newPairingCode.trim()}
                              >
                                {t("save_pairing_code", locale)}
                              </button>
                              <button
                                className="settings-btn settings-btn-secondary settings-btn-sm"
                                onClick={() => { setEditingPairingDevice(null); setNewPairingCode(""); }}
                              >
                                {locale === "ko" ? "\uCDE8\uC18C" : "Cancel"}
                              </button>
                            </div>
                          </div>
                        ) : (
                          <div className="settings-device-pairing-btns">
                            <button
                              className="settings-btn settings-btn-secondary settings-btn-sm"
                              onClick={() => { setEditingPairingDevice(device.device_id); setNewPairingCode(""); }}
                            >
                              {device.has_pairing_code ? t("change_pairing_code", locale) : t("set_pairing_code", locale)}
                            </button>
                            {device.has_pairing_code && (
                              <button
                                className="settings-btn settings-btn-danger settings-btn-sm"
                                onClick={() => handleRemovePairingCode(device.device_id)}
                              >
                                {t("remove_pairing_code", locale)}
                              </button>
                            )}
                          </div>
                        )}
                      </div>

                      {/* Remove device button — only for non-local offline devices */}
                      {!isLocal && !device.is_online && (
                        <div className="settings-device-remove" style={{ marginTop: 8 }}>
                          <button
                            className="settings-btn settings-btn-danger settings-btn-sm"
                            onClick={() => {
                              if (window.confirm(t("confirm_remove_device", locale))) {
                                handleRemoveDevice(device.device_id);
                              }
                            }}
                          >
                            {t("remove_device", locale)}
                          </button>
                        </div>
                      )}
                    </div>
                  );
                })}
              </div>
            </div>
          )}

          {/* Connection Mode indicator */}
          {isLoggedIn && (
            <div className="settings-section">
              <div className="settings-section-title">{t("connection_mode", locale)}</div>
              <div className="settings-card">
                <div className="settings-connection-mode">
                  <div className={`settings-connection-badge ${hasApiKey ? "local" : "relay"}`}>
                    <div className={`status-dot ${isConnected ? "connected" : ""}`} />
                    <span>
                      {hasApiKey
                        ? t("connection_mode_local", locale)
                        : t("connection_mode_relay", locale)}
                    </span>
                  </div>
                  <p style={{ fontSize: 12, color: "var(--color-text-muted)", marginTop: 8 }}>
                    {hasApiKey
                      ? t("connection_mode_local_hint", locale)
                      : t("connection_mode_relay_hint", locale)}
                  </p>
                  <div className="settings-device-pairing-btns" style={{ marginTop: 8, gap: 8, display: "flex", flexWrap: "wrap" }}>
                    <button
                      className="settings-btn settings-btn-secondary settings-btn-sm"
                      onClick={handleHealthCheck}
                      disabled={isHealthChecking}
                    >
                      {isHealthChecking ? t("checking", locale) : t("health_check", locale)}
                    </button>
                    {hasApiKey && (
                      <button
                        className="settings-btn settings-btn-danger settings-btn-sm"
                        onClick={() => {
                          if (window.confirm(t("confirm_switch_to_relay", locale))) {
                            // Remove all LLM API keys to switch to relay mode
                            localStorage.removeItem("zeroclaw_api_key_anthropic");
                            localStorage.removeItem("zeroclaw_api_key_openai");
                            localStorage.removeItem("zeroclaw_api_key_gemini");
                            setMessage({ type: "success", text: t("switched_to_relay", locale) });
                            clearMessage();
                            // Force re-render by reloading the page
                            window.location.reload();
                          }
                        }}
                      >
                        {t("switch_to_relay", locale)}
                      </button>
                    )}
                  </div>
                </div>
              </div>
            </div>
          )}

          {/* LLM Provider & Model selection */}
          {isLoggedIn && (
            <div className="settings-section">
              <div className="settings-section-title">{t("llm_settings", locale)}</div>
              <div className="settings-card">
                <div className="settings-field">
                  <label className="settings-label">{t("llm_provider", locale)}</label>
                  <div className="settings-provider-selector">
                    {PROVIDER_LIST.map((p) => (
                      <button
                        key={p.id}
                        className={`settings-provider-btn ${selectedProvider === p.id ? "active" : ""}`}
                        onClick={() => handleProviderChange(p.id)}
                      >
                        {t(p.labelKey, locale)}
                      </button>
                    ))}
                  </div>
                </div>
                <div className="settings-field" style={{ marginTop: 12 }}>
                  <label className="settings-label">{t("llm_model", locale)}</label>
                  <select
                    className="settings-select"
                    value={selectedModel}
                    onChange={(e) => handleModelChange(e.target.value)}
                  >
                    {models.map((m) => (
                      <option key={m.id} value={m.id}>
                        {m.label} ({m.tier})
                      </option>
                    ))}
                  </select>
                </div>
              </div>
            </div>
          )}

          {/* API Keys section */}
          {isLoggedIn && (
            <div className="settings-section">
              <div className="settings-section-title">{t("api_keys", locale)}</div>
              <div className="settings-card">
                <p style={{ fontSize: 12, color: "var(--color-text-muted)", marginBottom: 12 }}>
                  {t("api_key_hint", locale)}
                </p>
                <div className="settings-field">
                  <label className="settings-label">{t("api_key_claude", locale)}</label>
                  <div className="settings-input-row">
                    <input
                      className="settings-input"
                      type="password"
                      value={claudeKey}
                      onChange={(e) => setClaudeKey(e.target.value)}
                      placeholder={t("api_key_placeholder", locale)}
                    />
                    <button
                      className="settings-btn settings-btn-secondary settings-btn-sm"
                      onClick={() => handleSaveApiKey("anthropic", claudeKey)}
                    >
                      {locale === "ko" ? "\uC800\uC7A5" : "Save"}
                    </button>
                  </div>
                </div>
                <div className="settings-field" style={{ marginTop: 8 }}>
                  <label className="settings-label">{t("api_key_openai", locale)}</label>
                  <div className="settings-input-row">
                    <input
                      className="settings-input"
                      type="password"
                      value={openaiKey}
                      onChange={(e) => setOpenaiKey(e.target.value)}
                      placeholder={t("api_key_placeholder", locale)}
                    />
                    <button
                      className="settings-btn settings-btn-secondary settings-btn-sm"
                      onClick={() => handleSaveApiKey("openai", openaiKey)}
                    >
                      {locale === "ko" ? "\uC800\uC7A5" : "Save"}
                    </button>
                  </div>
                </div>
                <div className="settings-field" style={{ marginTop: 8 }}>
                  <label className="settings-label">{t("api_key_gemini", locale)}</label>
                  <div className="settings-input-row">
                    <input
                      className="settings-input"
                      type="password"
                      value={geminiKey}
                      onChange={(e) => setGeminiKey(e.target.value)}
                      placeholder={t("api_key_placeholder", locale)}
                    />
                    <button
                      className="settings-btn settings-btn-secondary settings-btn-sm"
                      onClick={() => handleSaveApiKey("gemini", geminiKey)}
                    >
                      {locale === "ko" ? "\uC800\uC7A5" : "Save"}
                    </button>
                  </div>
                </div>
              </div>
            </div>
          )}

          {/* Credits section */}
          {isLoggedIn && (
            <div className="settings-section">
              <div className="settings-section-title">{t("credits", locale)}</div>
              <div className="settings-card">
                <p style={{ fontSize: 12, color: "var(--color-text-muted)", marginBottom: 12 }}>
                  {t("credit_operator_hint", locale)}
                </p>
                <div className="settings-field">
                  <label className="settings-label">{t("credit_balance", locale)}</label>
                  <div className="settings-token-display" style={{ fontSize: 18, fontWeight: 600 }}>
                    {creditBalance !== null ? `${creditBalance.toLocaleString()} C` : "---"}
                  </div>
                </div>
                <div className="settings-credit-packages" style={{ marginTop: 12, display: "grid", gridTemplateColumns: "1fr 1fr 1fr", gap: 8 }}>
                  {[
                    { id: "starter_10", name: "Starter", price: "$10", priceKrw: "\u20A914,000", credits: "1,500C" },
                    { id: "standard_20", name: "Standard", price: "$20", priceKrw: "\u20A928,000", credits: "3,200C" },
                    { id: "power_50", name: "Power", price: "$50", priceKrw: "\u20A969,000", credits: "8,500C" },
                  ].map((pkg) => (
                    <button
                      key={pkg.id}
                      className="settings-btn settings-btn-secondary"
                      style={{ display: "flex", flexDirection: "column", alignItems: "center", padding: "8px 4px", fontSize: 12 }}
                      onClick={async () => {
                        try {
                          const data = await apiClient.purchaseCredits(pkg.id);
                          if (data.payment_url) {
                            window.open(data.payment_url, "_blank");
                          }
                          setMessage({ type: "success", text: locale === "ko" ? "\uACB0\uC81C \uC694\uCCAD \uC644\uB8CC" : "Payment initiated" });
                        } catch (err) {
                          setMessage({ type: "error", text: err instanceof Error ? err.message : (locale === "ko" ? "\uACB0\uC81C \uC2E4\uD328" : "Payment failed") });
                        }
                        clearMessage();
                      }}
                    >
                      <span style={{ fontWeight: 600 }}>{pkg.name}</span>
                      <span style={{ color: "var(--color-text-muted)" }}>{pkg.price} ({pkg.priceKrw})</span>
                      <span style={{ color: "var(--color-accent)", fontSize: 11 }}>{pkg.credits}</span>
                    </button>
                  ))}
                </div>
              </div>
            </div>
          )}

          {message && (
            <div className={`settings-message ${message.type}`}>{message.text}</div>
          )}

          {/* Advanced Settings (collapsible) */}
          <div className="settings-section">
            <button
              className="settings-advanced-toggle"
              onClick={() => setShowAdvanced(!showAdvanced)}
            >
              <span>{t("advanced_settings_toggle", locale)}</span>
              <svg
                width="14"
                height="14"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="2"
                strokeLinecap="round"
                strokeLinejoin="round"
                style={{ transform: showAdvanced ? "rotate(180deg)" : "rotate(0deg)", transition: "transform 0.2s" }}
              >
                <polyline points="6 9 12 15 18 9" />
              </svg>
            </button>
            {showAdvanced && (
              <div className="settings-card" style={{ marginTop: 8 }}>
                <div className="settings-field">
                  <label className="settings-label">{t("local_gateway_url", locale)}</label>
                  <div className="settings-input-row">
                    <input
                      className="settings-input"
                      type="url"
                      value={serverUrl}
                      onChange={(e) => handleServerUrlChange(e.target.value)}
                      placeholder="http://127.0.0.1:3000"
                    />
                  </div>
                </div>
              </div>
            )}
          </div>

          {/* Language section */}
          <div className="settings-section">
            <div className="settings-section-title">{t("language", locale)}</div>
            <div className="settings-card">
              <div className="settings-lang-selector">
                <button
                  className={`settings-lang-btn ${locale === "en" ? "active" : ""}`}
                  onClick={() => onLocaleChange("en")}
                >
                  English
                </button>
                <button
                  className={`settings-lang-btn ${locale === "ko" ? "active" : ""}`}
                  onClick={() => onLocaleChange("ko")}
                >
                  {"\uD55C\uAD6D\uC5B4"}
                </button>
              </div>
            </div>
          </div>

          {/* Sync section (Tauri only) */}
          {inTauri && (
            <div className="settings-section">
              <div className="settings-section-title">{t("sync_status", locale)}</div>
              <div className="settings-card">
                {syncStatus ? (
                  <>
                    <div className={`settings-status ${syncStatus.connected ? "connected" : "disconnected"}`}>
                      <div className={`status-dot ${syncStatus.connected ? "connected" : ""}`} />
                      {syncStatus.connected ? t("sync_connected", locale) : t("sync_disconnected", locale)}
                    </div>
                    <div className="settings-field" style={{ marginTop: 12 }}>
                      <label className="settings-label">{t("sync_device_id", locale)}</label>
                      <div className="settings-token-display" style={{ fontSize: 11 }}>
                        {syncStatus.device_id}
                      </div>
                    </div>
                    {isLoggedIn && (
                      <div className="settings-actions" style={{ marginTop: 12 }}>
                        <button
                          className="settings-btn settings-btn-secondary"
                          onClick={handleTriggerSync}
                          disabled={isSyncing}
                        >
                          {isSyncing ? t("sync_triggering", locale) : t("sync_trigger", locale)}
                        </button>
                      </div>
                    )}
                  </>
                ) : (
                  <div className="settings-status disconnected">
                    <div className="status-dot" />
                    {t("sync_disconnected", locale)}
                  </div>
                )}
              </div>
            </div>
          )}

          {/* Platform info (Tauri only) */}
          {inTauri && platformInfo && (
            <div className="settings-section">
              <div className="settings-section-title">{t("platform", locale)}</div>
              <div className="settings-card">
                <p style={{ fontSize: 13, color: "var(--color-text-secondary)" }}>
                  {platformInfo.os} / {platformInfo.arch}
                  {platformInfo.is_mobile ? " (Mobile)" : " (Desktop)"}
                </p>
              </div>
            </div>
          )}

          {/* About */}
          <div className="settings-section">
            <div className="settings-section-title">About</div>
            <div className="settings-card">
              <p style={{ fontSize: 13, color: "var(--color-text-secondary)", marginBottom: 4 }}>
                <strong>MoA</strong>
              </p>
              <p style={{ fontSize: 12, color: "var(--color-text-muted)" }}>
                Powered by MoA Agent Runtime
              </p>
              <p style={{ fontSize: 12, color: "var(--color-text-muted)", marginTop: 4 }}>
                Version 0.1.0
              </p>
            </div>
          </div>

        </div>
      </div>
    </div>
  );
}
