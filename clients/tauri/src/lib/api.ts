import {
  isTauri,
  getSyncStatus,
  triggerFullSync,
  getPlatformInfo,
  disconnectBackend,
  setServerUrl as setBackendServerUrl,
  type SyncStatus,
  type PlatformInfo,
} from "./tauri-bridge";

const STORAGE_KEY_TOKEN = "zeroclaw_token";
const STORAGE_KEY_SERVER = "zeroclaw_server_url";
const STORAGE_KEY_RELAY = "zeroclaw_relay_url";
const STORAGE_KEY_USER = "zeroclaw_user";
const STORAGE_KEY_DEVICE_ID = "zeroclaw_device_id";

// Local-first architecture:
// - LOCAL_GATEWAY_URL: MoA agent running on this device (chat, voice, tools)
// - RELAY_SERVER_URL: Railway relay for memory sync + operator key fallback only
const DEFAULT_LOCAL_GATEWAY_URL =
  import.meta.env.VITE_LOCAL_GATEWAY_URL || "http://127.0.0.1:3000";
const DEFAULT_RELAY_SERVER_URL =
  import.meta.env.VITE_RELAY_SERVER_URL || "https://moanew-production.up.railway.app";

// ── Types ────────────────────────────────────────────────────────

export interface ChatResponse {
  response: string;
  model: string;
}

export interface HealthResponse {
  status: string;
}

export interface DeviceInfo {
  device_id: string;
  device_name: string;
  platform: string | null;
  last_seen: number;
  is_online: boolean;
  has_pairing_code: boolean;
}

export interface LoginResponse {
  status: string;
  token: string;
  user_id: string;
  username: string;
  devices: DeviceInfo[];
}

export interface RegisterResponse {
  status: string;
  user_id: string;
}

export interface UserInfo {
  user_id: string;
  username: string;
}

export interface ToolInfo {
  name: string;
  description: string;
}

export interface AgentInfo {
  channels: string[];
  tools: ToolInfo[];
}

export type { SyncStatus, PlatformInfo };

// ── Client ──────────────────────────────────────────────────────

export class MoAClient {
  // Local MoA gateway URL (chat, voice, tools — runs on this device)
  private serverUrl: string;
  // Railway relay server URL (memory sync + operator API key fallback only)
  private relayUrl: string;
  private token: string | null;
  private user: UserInfo | null;
  private deviceId: string;
  private heartbeatInterval: ReturnType<typeof setInterval> | null = null;

  constructor() {
    this.serverUrl = localStorage.getItem(STORAGE_KEY_SERVER) || DEFAULT_LOCAL_GATEWAY_URL;
    this.relayUrl = localStorage.getItem(STORAGE_KEY_RELAY) || DEFAULT_RELAY_SERVER_URL;
    this.token = localStorage.getItem(STORAGE_KEY_TOKEN);
    const storedUser = localStorage.getItem(STORAGE_KEY_USER);
    this.user = storedUser ? JSON.parse(storedUser) : null;
    this.deviceId = this.getOrCreateDeviceId();
  }

  private getOrCreateDeviceId(): string {
    let id = localStorage.getItem(STORAGE_KEY_DEVICE_ID);
    if (!id) {
      id = crypto.randomUUID();
      localStorage.setItem(STORAGE_KEY_DEVICE_ID, id);
    }
    return id;
  }

  // ── Server URL ─────────────────────────────────────────────────
  // serverUrl = local MoA gateway (chat, voice, AI operations)
  // relayUrl  = Railway relay (memory sync + operator key fallback only)

  getServerUrl(): string {
    return this.serverUrl;
  }

  getRelayUrl(): string {
    return this.relayUrl;
  }

  setServerUrl(url: string): void {
    this.serverUrl = url.replace(/\/+$/, "");
    localStorage.setItem(STORAGE_KEY_SERVER, this.serverUrl);
    if (isTauri()) {
      setBackendServerUrl(this.serverUrl).catch(() => {});
    }
  }

  setRelayUrl(url: string): void {
    this.relayUrl = url.replace(/\/+$/, "");
    localStorage.setItem(STORAGE_KEY_RELAY, this.relayUrl);
  }

  // ── Auth State ─────────────────────────────────────────────────

  getToken(): string | null {
    return this.token;
  }

  getDeviceId(): string {
    return this.deviceId;
  }

  getUser(): UserInfo | null {
    return this.user;
  }

  isLoggedIn(): boolean {
    return this.token !== null && this.token.length > 0;
  }

  getMaskedToken(): string {
    if (!this.token) return "";
    if (this.token.length <= 8) return "****";
    return this.token.substring(0, 4) + "..." + this.token.substring(this.token.length - 4);
  }

  // ── Auth API ───────────────────────────────────────────────────

  async register(username: string, password: string): Promise<RegisterResponse> {
    const res = await fetch(`${this.relayUrl}/api/auth/register`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ username, password }),
    });

    if (!res.ok) {
      const data = await res.json().catch(() => ({ error: "Registration failed" }));
      throw new Error(data.error || `Registration failed (${res.status})`);
    }

    return await res.json();
  }

  async login(username: string, password: string): Promise<LoginResponse> {
    const deviceName = await this.getDeviceName();
    const res = await fetch(`${this.relayUrl}/api/auth/login`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        username,
        password,
        device_id: this.deviceId,
        device_name: deviceName,
      }),
    });

    if (!res.ok) {
      const data = await res.json().catch(() => ({ error: "Login failed" }));
      throw new Error(data.error || `Login failed (${res.status})`);
    }

    const data: LoginResponse = await res.json();

    // Save auth state
    this.token = data.token;
    this.user = { user_id: data.user_id, username: data.username };
    localStorage.setItem(STORAGE_KEY_TOKEN, data.token);
    localStorage.setItem(STORAGE_KEY_USER, JSON.stringify(this.user));

    return data;
  }

  async logout(): Promise<void> {
    if (this.token) {
      try {
        await fetch(`${this.relayUrl}/api/auth/logout`, {
          method: "POST",
          headers: { Authorization: `Bearer ${this.token}` },
        });
      } catch {
        // Ignore network errors during logout
      }
    }
    this.clearAuth();
  }

  private clearAuth(): void {
    this.token = null;
    this.user = null;
    localStorage.removeItem(STORAGE_KEY_TOKEN);
    localStorage.removeItem(STORAGE_KEY_USER);
    // Reset to local gateway, keep relay URL
    this.serverUrl = DEFAULT_LOCAL_GATEWAY_URL;
    localStorage.setItem(STORAGE_KEY_SERVER, this.serverUrl);
    this.stopHeartbeat();
    if (isTauri()) {
      disconnectBackend().catch(() => {});
    }
  }

  // ── Device API ─────────────────────────────────────────────────

  async getDevices(): Promise<DeviceInfo[]> {
    if (!this.token) return [];

    const res = await fetch(`${this.relayUrl}/api/auth/devices`, {
      headers: { Authorization: `Bearer ${this.token}` },
    });

    if (!res.ok) {
      if (res.status === 401) {
        this.clearAuth();
        throw new Error("Session expired");
      }
      return [];
    }

    const data = await res.json();
    return data.devices || [];
  }

  async registerDevice(deviceName: string, platform?: string): Promise<void> {
    if (!this.token) return;

    await fetch(`${this.relayUrl}/api/auth/devices`, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        Authorization: `Bearer ${this.token}`,
      },
      body: JSON.stringify({
        device_id: this.deviceId,
        device_name: deviceName,
        platform,
      }),
    });
  }

  async setDevicePairingCode(deviceId: string, code: string | null): Promise<void> {
    if (!this.token) throw new Error("Not authenticated");

    const res = await fetch(`${this.relayUrl}/api/auth/devices/${deviceId}/pairing-code`, {
      method: "PUT",
      headers: {
        "Content-Type": "application/json",
        Authorization: `Bearer ${this.token}`,
      },
      body: JSON.stringify({ pairing_code: code }),
    });

    if (!res.ok) {
      const data = await res.json().catch(() => ({ error: "Failed" }));
      throw new Error(data.error || "Failed to set pairing code");
    }
  }

  async verifyDevicePairing(deviceId: string, code: string): Promise<boolean> {
    if (!this.token) throw new Error("Not authenticated");

    const res = await fetch(`${this.relayUrl}/api/auth/devices/${deviceId}/verify-pairing`, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        Authorization: `Bearer ${this.token}`,
      },
      body: JSON.stringify({ pairing_code: code }),
    });

    if (!res.ok) return false;
    const data = await res.json();
    return data.verified === true;
  }

  // ── Agent Info ────────────────────────────────────────────────

  async getAgentInfo(): Promise<AgentInfo> {
    try {
      const res = await fetch(`${this.serverUrl}/api/agent/info`);
      if (!res.ok) return { channels: [], tools: [] };
      return await res.json();
    } catch {
      return { channels: [], tools: [] };
    }
  }

  // ── Heartbeat ──────────────────────────────────────────────────

  startHeartbeat(): void {
    if (this.heartbeatInterval) return;
    this.sendHeartbeat();
    this.heartbeatInterval = setInterval(() => this.sendHeartbeat(), 60_000);
  }

  stopHeartbeat(): void {
    if (this.heartbeatInterval) {
      clearInterval(this.heartbeatInterval);
      this.heartbeatInterval = null;
    }
  }

  private async sendHeartbeat(): Promise<void> {
    if (!this.token) return;
    try {
      await fetch(`${this.serverUrl}/api/auth/heartbeat`, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          Authorization: `Bearer ${this.token}`,
        },
        body: JSON.stringify({ device_id: this.deviceId }),
      });
    } catch {
      // Heartbeat failures are non-critical
    }
  }

  // ── Chat ───────────────────────────────────────────────────────
  // Routing logic (with automatic fallback):
  // 1. If user has a local API key → try local gateway first
  //    - If local gateway fails (network/connection) → fallback to relay
  // 2. If no local API key → try relay server (operator key, credits deducted)
  //    - If relay fails → try local gateway as last resort
  // 3. If API key is invalid (400 from local gateway) → fallback to relay

  private static readonly PROVIDER_KEY_MAP: Record<string, string> = {
    claude: "anthropic",
    openai: "openai",
    gemini: "gemini",
  };

  hasLocalApiKey(): boolean {
    // Check if the SELECTED provider has an API key configured.
    const provider = localStorage.getItem("zeroclaw_llm_provider") || "gemini";
    const keyStorageName = MoAClient.PROVIDER_KEY_MAP[provider] || provider;
    const key = localStorage.getItem(`zeroclaw_api_key_${keyStorageName}`);
    return !!key && key.trim().length > 0;
  }

  hasAnyLocalApiKey(): boolean {
    // Check if ANY provider has an API key configured (for Settings display).
    const keyNames = ["anthropic", "openai", "gemini"];
    return keyNames.some((p) => {
      const key = localStorage.getItem(`zeroclaw_api_key_${p}`);
      return key && key.trim().length > 0;
    });
  }

  /**
   * Try a single chat request against the given base URL.
   * Returns the Response on success, or null if a network-level failure occurred.
   * Throws on non-recoverable errors (auth expiry etc.).
   */
  private async tryChatRequest(
    baseUrl: string,
    body: Record<string, unknown>,
  ): Promise<Response | null> {
    try {
      return await fetch(`${baseUrl}/api/chat`, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          Authorization: `Bearer ${this.token}`,
        },
        body: JSON.stringify(body),
      });
    } catch (err) {
      if (err instanceof TypeError && err.message === "Failed to fetch") {
        // Network-level failure (connection refused, DNS, timeout, etc.)
        return null;
      }
      throw err;
    }
  }

  /**
   * Parse a successful chat response body into ChatResponse.
   */
  private async parseChatResponse(res: Response): Promise<ChatResponse> {
    if (!res.ok) {
      if (res.status === 401) {
        throw new Error("Chat authentication failed. Please check your connection settings.");
      }
      const text = await res.text().catch(() => "Unknown error");
      let errorMessage = text;
      try {
        const parsed = JSON.parse(text);
        if (parsed.error) {
          errorMessage = parsed.error;
        }
      } catch {
        // JSON parse failed, use raw text
      }
      throw new Error(errorMessage || `Chat request failed (${res.status})`);
    }

    const data = await res.json();
    return {
      response: data.response || data.reply || "",
      model: data.model || "",
    };
  }

  async chat(message: string, context: string[] = []): Promise<ChatResponse> {
    if (!this.token) {
      throw new Error("Not authenticated. Please login first.");
    }

    // Include user's selected provider/model preference
    const provider = localStorage.getItem("zeroclaw_llm_provider") || "gemini";
    const model = localStorage.getItem("zeroclaw_llm_model") || "gemini-2.5-flash";

    const keyStorageName = MoAClient.PROVIDER_KEY_MAP[provider] || provider;
    const apiKey = localStorage.getItem(`zeroclaw_api_key_${keyStorageName}`) || "";
    const hasSelectedProviderKey = !!apiKey && apiKey.trim().length > 0;

    // Build request body — include API key only when available
    const body: Record<string, unknown> = {
      message,
      context,
      provider,
      model,
      ...(hasSelectedProviderKey ? { api_key: apiKey } : {}),
    };

    // ── Determine routing order with fallback ──
    // Primary: local gateway if we have a key, relay otherwise
    // Fallback: the other server if primary fails
    const primaryUrl = hasSelectedProviderKey ? this.serverUrl : this.relayUrl;
    const fallbackUrl = hasSelectedProviderKey ? this.relayUrl : this.serverUrl;

    // ── Try primary ──
    let res = await this.tryChatRequest(primaryUrl, body);

    if (res !== null) {
      // Primary connected. Check if API key error from local gateway
      // → fallback to relay which can use operator keys.
      // Gateway returns 400 with { fallback_to_relay: true } for:
      // - missing API key (code: "missing_api_key")
      // - provider auth error / 401 (code: "provider_auth_error")
      if (primaryUrl === this.serverUrl && (res.status === 400 || res.status === 500)) {
        // Read the error body to check for fallback hint
        const errorText = await res.text().catch(() => "");
        let shouldFallback = false;
        let errorJson: Record<string, unknown> = {};
        try {
          errorJson = JSON.parse(errorText);
          shouldFallback = errorJson.fallback_to_relay === true
            || errorJson.code === "missing_api_key"
            || errorJson.code === "provider_auth_error";
        } catch {
          // Not JSON — might still be an API key issue, try fallback anyway
          shouldFallback = errorText.includes("API key")
            || errorText.includes("Unauthorized")
            || errorText.includes("authentication");
        }

        if (shouldFallback) {
          const relayBody = { ...body };
          delete relayBody.api_key; // Let relay use operator key
          const fallbackRes = await this.tryChatRequest(this.relayUrl, relayBody);
          if (fallbackRes !== null && fallbackRes.ok) {
            return this.parseChatResponse(fallbackRes);
          }
        }

        // Not a fallback-eligible error, or relay also failed — show error
        if (res.status === 400) {
          const errorMessage = (errorJson.error as string) || errorText || `Chat request failed (${res.status})`;
          throw new Error(errorMessage);
        }
        // For 500 errors without fallback, fall through to parseChatResponse
        // which will throw with the error detail from the response body
        if (errorText) {
          const errorMessage = (errorJson.error as string) || errorText || `Chat request failed (${res.status})`;
          throw new Error(errorMessage);
        }
      }

      return this.parseChatResponse(res);
    }

    // ── Primary failed (network error) — try fallback ──
    // When falling back to relay without a local key, omit api_key
    const fallbackBody = fallbackUrl === this.relayUrl
      ? { ...body, api_key: undefined }
      : body;

    res = await this.tryChatRequest(fallbackUrl, fallbackBody);

    if (res !== null) {
      return this.parseChatResponse(res);
    }

    // ── Both failed ──
    throw new Error(
      "Cannot connect to MoA server. Both local gateway and relay server are unreachable. " +
        "Please check that either the local server is running or you have network access.",
    );
  }

  // ── Health ─────────────────────────────────────────────────────

  async healthCheck(): Promise<HealthResponse> {
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), 5000);

    try {
      const res = await fetch(`${this.serverUrl}/health`, {
        method: "GET",
        signal: controller.signal,
      });

      if (!res.ok) {
        throw new Error(`Health check failed (${res.status})`);
      }

      return await res.json();
    } catch (err) {
      if (err instanceof DOMException && err.name === "AbortError") {
        throw new Error("Health check timed out");
      }
      throw err;
    } finally {
      clearTimeout(timeout);
    }
  }

  // ── Sync commands (Tauri backend only) ──────────────────────────

  async getSyncStatus(): Promise<SyncStatus | null> {
    return getSyncStatus();
  }

  async triggerFullSync(): Promise<string | null> {
    return triggerFullSync();
  }

  async getPlatformInfo(): Promise<PlatformInfo | null> {
    return getPlatformInfo();
  }

  // ── Credits (via relay server — billing is server-side) ──────

  async getCreditBalance(): Promise<number> {
    if (!this.token) return 0;

    try {
      const res = await fetch(`${this.relayUrl}/api/credits/balance`, {
        headers: { Authorization: `Bearer ${this.token}` },
      });

      if (!res.ok) return 0;
      const data = await res.json();
      return data.balance ?? 0;
    } catch {
      return 0;
    }
  }

  async purchaseCredits(packageId: string): Promise<{ payment_url?: string }> {
    if (!this.token) throw new Error("Not authenticated");

    const res = await fetch(`${this.relayUrl}/api/credits/purchase`, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        Authorization: `Bearer ${this.token}`,
      },
      body: JSON.stringify({ package_id: packageId }),
    });

    if (!res.ok) {
      const data = await res.json().catch(() => ({ error: "Payment failed" }));
      throw new Error(data.error || `Payment failed (${res.status})`);
    }

    return await res.json();
  }

  // ── API Key Management ──────────────────────────────────────
  // Save API keys to the local MoA agent config.
  // When user provides their own keys, MoA uses them directly.
  // When no key is set, MoA falls back to operator keys via relay.

  async saveApiKeyToAgent(provider: string, key: string): Promise<void> {
    try {
      await fetch(`${this.serverUrl}/api/config/api-key`, {
        method: "PUT",
        headers: {
          "Content-Type": "application/json",
          Authorization: `Bearer ${this.token}`,
        },
        body: JSON.stringify({ provider, api_key: key }),
      });
    } catch {
      // Local agent might not be running — key is still saved in localStorage
    }
  }

  // ── Operator Key Fallback (via relay server) ────────────────
  // When user has no API key, fetch operator's key from relay for use
  // with 2x credit deduction per API call

  async getOperatorKeyProxy(provider: string): Promise<string | null> {
    if (!this.token) return null;

    try {
      const res = await fetch(`${this.relayUrl}/api/operator/key-proxy`, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          Authorization: `Bearer ${this.token}`,
        },
        body: JSON.stringify({ provider, device_id: this.deviceId }),
      });

      if (!res.ok) return null;
      const data = await res.json();
      return data.proxied_key ?? null;
    } catch {
      return null;
    }
  }

  // ── Helpers ────────────────────────────────────────────────────

  private async getDeviceName(): Promise<string> {
    if (isTauri()) {
      const info = await getPlatformInfo();
      if (info) {
        return `MoA ${info.os} ${info.is_mobile ? "Mobile" : "Desktop"}`;
      }
    }
    return `MoA ${navigator.platform || "Web"}`;
  }
}

export const apiClient = new MoAClient();
