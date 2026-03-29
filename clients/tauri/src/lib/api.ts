import {
  isTauri,
  gatewayFetch,
  getSyncStatus,
  triggerFullSync,
  getPlatformInfo,
  getDeviceFingerprint,
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
  import.meta.env.VITE_RELAY_SERVER_URL || "https://api.mymoa.app";

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

export interface ChannelInfo {
  name: string;
  enabled: boolean;
}

export interface AgentInfo {
  channels: string[];
  channels_detail: ChannelInfo[];
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

  // Available tools fetched from gateway on login/connect
  private availableTools: ToolInfo[] = [];

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
    const fingerprint = await getDeviceFingerprint();
    const res = await fetch(`${this.relayUrl}/api/auth/login`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        username,
        password,
        device_id: this.deviceId,
        device_name: deviceName,
        fingerprint,
      }),
    });

    if (!res.ok) {
      const data = await res.json().catch(() => ({ error: "Login failed" }));
      throw new Error(data.error || `Login failed (${res.status})`);
    }

    const data: LoginResponse & { resolved_device_id?: string } = await res.json();

    // If server matched an existing device by fingerprint, adopt that device_id
    if (data.resolved_device_id) {
      this.deviceId = data.resolved_device_id;
      localStorage.setItem(STORAGE_KEY_DEVICE_ID, data.resolved_device_id);
    }

    // Save auth state
    this.token = data.token;
    this.user = { user_id: data.user_id, username: data.username };
    localStorage.setItem(STORAGE_KEY_TOKEN, data.token);
    localStorage.setItem(STORAGE_KEY_USER, JSON.stringify(this.user));

    return data;
  }

  /**
   * Verify password for lock screen unlock.
   * Re-authenticates using the stored username and provided password.
   * On success, refreshes the session token (server may issue a new one).
   * Also checks gateway health and updates liveness state.
   */
  async verifyPasswordForUnlock(password: string): Promise<void> {
    const username = this.user?.username;
    if (!username) {
      throw new Error("No stored user session");
    }

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
      const data = await res.json().catch(() => ({ error: "Verification failed" }));
      throw new Error(data.error || `Verification failed (${res.status})`);
    }

    const data: LoginResponse = await res.json();

    // Refresh token with the new one from server
    this.token = data.token;
    localStorage.setItem(STORAGE_KEY_TOKEN, data.token);

    // Check gateway health after unlock — triggers watchdog awareness
    await this.checkGatewayHealth();
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

  /** Register this device with its real platform name (e.g. "MoA windows Desktop"). */
  async registerCurrentDevice(): Promise<void> {
    const name = await this.getDeviceName();
    const info = await getPlatformInfo();
    return this.registerDevice(name, info?.os);
  }

  async registerDevice(deviceName: string, platform?: string): Promise<void> {
    if (!this.token) return;

    const fingerprint = await getDeviceFingerprint();
    const res = await fetch(`${this.relayUrl}/api/auth/devices`, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        Authorization: `Bearer ${this.token}`,
      },
      body: JSON.stringify({
        device_id: this.deviceId,
        device_name: deviceName,
        platform,
        fingerprint,
      }),
    });

    if (res.ok) {
      const data = await res.json().catch(() => ({}));
      // If server matched an existing device by fingerprint, adopt that device_id
      if (data.resolved_device_id) {
        this.deviceId = data.resolved_device_id;
        localStorage.setItem(STORAGE_KEY_DEVICE_ID, data.resolved_device_id);
      }
    }
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

  async removeDevice(deviceId: string): Promise<boolean> {
    if (!this.token) throw new Error("Not authenticated");

    const res = await fetch(`${this.relayUrl}/api/auth/devices/${deviceId}`, {
      method: "DELETE",
      headers: {
        Authorization: `Bearer ${this.token}`,
      },
    });

    if (!res.ok) {
      const data = await res.json().catch(() => ({ error: "Failed" }));
      throw new Error(data.error || "Failed to remove device");
    }
    const data = await res.json();
    return data.deleted === true;
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
      const res = await this.fetchWithFallback("/api/agent/info", {
        method: "GET",
      });
      if (!res.ok) return this.fallbackAgentInfo();
      const data = await res.json();
      const channels_detail = data.channels_detail ?? [];
      // If backend doesn't return channels_detail yet, build from channels array
      const effectiveDetail = channels_detail.length > 0
        ? channels_detail
        : (data.channels ?? []).map((name: string) => ({ name, enabled: true }));
      const tools = data.tools ?? [];
      // If backend returns empty tools, use fallback list
      const fallback = this.fallbackAgentInfo();
      return {
        channels: data.channels ?? [],
        channels_detail: effectiveDetail.length > 0 ? effectiveDetail : fallback.channels_detail,
        tools: tools.length > 0 ? tools : fallback.tools,
      };
    } catch {
      return this.fallbackAgentInfo();
    }
  }

  /** Fallback agent info with all known ZeroClaw channels and tools */
  private fallbackAgentInfo(): AgentInfo {
    const ALL_CHANNELS = [
      "telegram", "discord", "slack", "mattermost", "whatsapp", "line",
      "kakao", "qq", "lark", "feishu", "dingtalk", "matrix", "signal",
      "irc", "email", "github", "nostr", "imessage", "bluebubbles",
      "linq", "wati", "nextcloud_talk", "napcat", "acp", "clawdtalk", "webhook",
    ];
    const ALL_TOOLS: ToolInfo[] = [
      { name: "shell", description: "Execute a shell command in the workspace directory" },
      { name: "process", description: "Manage background processes: spawn, check output, terminate" },
      { name: "git_operations", description: "Git repository operations (status, diff, commit, branch, etc.)" },
      { name: "file_read", description: "Read a file from the workspace" },
      { name: "file_write", description: "Write contents to a file in the workspace" },
      { name: "file_edit", description: "Edit a file by replacing text content" },
      { name: "apply_patch", description: "Apply a unified diff patch to git repository" },
      { name: "glob_search", description: "Search for files by glob pattern in the workspace" },
      { name: "content_search", description: "Search file contents by regex pattern in the workspace" },
      { name: "browser", description: "Web/browser automation with pluggable backends" },
      { name: "browser_open", description: "Open an approved HTTPS URL in a browser" },
      { name: "http_request", description: "Make HTTP requests (GET, POST, PUT, DELETE)" },
      { name: "web_fetch", description: "Fetch content from a URL" },
      { name: "web_search", description: "Search the web for information" },
      { name: "memory_store", description: "Store a fact or note in long-term memory" },
      { name: "memory_recall", description: "Search long-term memory for relevant facts" },
      { name: "memory_observe", description: "Observe and record context for memory" },
      { name: "memory_forget", description: "Remove a memory by key" },
      { name: "pdf_read", description: "Extract text from PDF files" },
      { name: "docx_read", description: "Extract text from DOCX (Word) files" },
      { name: "document_process", description: "Hancom/HWP document viewer and converter" },
      { name: "pptx_read", description: "Extract text from PPTX (PowerPoint) files" },
      { name: "xlsx_read", description: "Extract data from XLSX (Excel) files" },
      { name: "screenshot", description: "Capture a screenshot of the current screen" },
      { name: "image_info", description: "Read image file metadata and base64 data" },
      { name: "task_plan", description: "Manage task checklists for the current session" },
      { name: "cron_list", description: "List scheduled cron jobs" },
      { name: "cron_add", description: "Add a new cron job" },
      { name: "cron_remove", description: "Remove a cron job" },
      { name: "cron_run", description: "Run a cron job immediately" },
      { name: "cron_runs", description: "List recent cron job run history" },
      { name: "cron_update", description: "Update an existing cron job" },
      { name: "bg_run", description: "Execute a tool in the background" },
      { name: "bg_status", description: "Query background job status" },
      { name: "subagent_spawn", description: "Spawn a background sub-agent" },
      { name: "subagent_list", description: "List active sub-agents" },
      { name: "subagent_manage", description: "Manage sub-agent sessions" },
      { name: "delegate", description: "Delegate tasks to specialized agents" },
      { name: "delegate_coordination_status", description: "Inspect delegate coordination state" },
      { name: "wasm_module", description: "Run WebAssembly modules" },
      { name: "composio", description: "Execute actions on 1000+ apps via Composio" },
      { name: "openclaw_migration", description: "OpenClaw migration tool" },
      { name: "manage_auth_profile", description: "Manage auth profiles and tokens" },
      { name: "proxy_config", description: "Manage proxy settings" },
      { name: "web_access_config", description: "Manage network URL access policy" },
      { name: "web_search_config", description: "Configure web search settings" },
      { name: "check_provider_quota", description: "Check AI provider rate limits" },
      { name: "switch_provider", description: "Switch to a different AI provider" },
      { name: "estimate_quota_cost", description: "Estimate quota cost for operations" },
      { name: "hardware_board_info", description: "Get connected hardware board info" },
      { name: "hardware_memory_map", description: "Get hardware memory map" },
      { name: "hardware_memory_read", description: "Read memory/registers from hardware" },
      { name: "sop_list", description: "List available SOPs" },
      { name: "sop_execute", description: "Execute a standard operating procedure" },
      { name: "sop_status", description: "Query SOP execution status" },
      { name: "sop_advance", description: "Advance SOP execution to next step" },
      { name: "sop_approve", description: "Approve a pending SOP step" },
      { name: "state_get", description: "Get agent runtime state" },
      { name: "state_set", description: "Set agent runtime state" },
      { name: "model_routing_config", description: "Configure model routing" },
      { name: "channel_ack_config", description: "Configure channel acknowledgment" },
      { name: "schedule", description: "Schedule tasks for future execution" },
    ];
    return {
      channels: [],
      channels_detail: ALL_CHANNELS.map((name) => ({ name, enabled: false })),
      tools: ALL_TOOLS,
    };
  }

  // ── Gateway Liveness ─────────────────────────────────────────────

  /** Whether the local gateway was reachable on the last heartbeat check. */
  private gatewayAlive = true;

  /** Count consecutive heartbeat failures before marking gateway as down. */
  private heartbeatFailCount = 0;

  /** Check if the local gateway is currently alive. */
  isGatewayAlive(): boolean {
    return this.gatewayAlive;
  }

  /**
   * Try fetching from local gateway first; if it fails, try relay.
   * Returns the Response from whichever succeeded.
   */
  private async fetchWithFallback(
    path: string,
    init: RequestInit,
  ): Promise<Response> {
    // If local gateway is known to be down, go straight to relay
    if (!this.gatewayAlive) {
      return fetch(`${this.relayUrl}${path}`, init);
    }

    try {
      // In Tauri mode, route local gateway requests through Rust backend
      if (isTauri()) {
        const headers: Record<string, string> = {};
        if (init.headers) {
          const h = init.headers as Record<string, string>;
          for (const [k, v] of Object.entries(h)) {
            headers[k] = v;
          }
        }
        const result = await gatewayFetch(
          `${this.serverUrl}${path}`,
          init.method || "GET",
          headers,
          typeof init.body === "string" ? init.body : undefined,
        );
        if (result) {
          return new Response(result.body, {
            status: result.status,
            headers: { "Content-Type": "application/json" },
          });
        }
      }
      const res = await fetch(`${this.serverUrl}${path}`, init);
      return res;
    } catch {
      // Local gateway unreachable — try relay
      this.gatewayAlive = false;
      return fetch(`${this.relayUrl}${path}`, init);
    }
  }

  /** Quick health probe against the local gateway (5s timeout).
   *  If local is down, also checks relay so gateway operations can continue. */
  async checkGatewayHealth(): Promise<boolean> {
    // Try local first — use Tauri IPC proxy when in desktop mode
    try {
      let ok = false;
      if (isTauri()) {
        const result = await gatewayFetch(
          `${this.serverUrl}/health`,
          "GET",
          {},
        );
        ok = result !== null && result.status >= 200 && result.status < 300;
      } else {
        const controller = new AbortController();
        const timeout = setTimeout(() => controller.abort(), 3000);
        const res = await fetch(`${this.serverUrl}/health`, {
          method: "GET",
          signal: controller.signal,
        });
        clearTimeout(timeout);
        ok = res.ok;
      }
      if (ok) {
        this.gatewayAlive = true;
        return true;
      }
    } catch {
      // Local unreachable — continue to relay check
    }

    // Local is down — check if relay is reachable
    try {
      const controller = new AbortController();
      const timeout = setTimeout(() => controller.abort(), 5000);
      const res = await fetch(`${this.relayUrl}/health`, {
        method: "GET",
        signal: controller.signal,
      });
      clearTimeout(timeout);
      // Relay is reachable but local is not — mark gateway alive=false
      // so fetchWithFallback uses relay
      this.gatewayAlive = false;
      return res.ok;
    } catch {
      this.gatewayAlive = false;
      return false;
    }
  }

  /**
   * Assert that the local gateway is reachable.
   * Retries once after a short delay to handle transient failures
   * (e.g. gateway still starting up, brief network hiccup).
   * Does NOT throw — chat() will fall back to relay if gateway is down.
   */
  private async requireGateway(): Promise<void> {
    if (this.gatewayAlive) return; // fast path — last check was ok
    const alive = await this.checkGatewayHealth();
    if (alive) return;
    // Retry once after 1s — handles gateway startup race
    await new Promise((r) => setTimeout(r, 1000));
    await this.checkGatewayHealth();
    // No throw — chat() handles relay fallback
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
    const fingerprint = await getDeviceFingerprint();
    const heartbeatBody = JSON.stringify({
      device_id: this.deviceId,
      fingerprint: fingerprint || undefined,
    });
    const headers = {
      "Content-Type": "application/json",
      Authorization: `Bearer ${this.token}`,
    };

    // Try local gateway first
    try {
      const res = await fetch(`${this.serverUrl}/api/auth/heartbeat`, {
        method: "POST",
        headers,
        body: heartbeatBody,
      });
      this.gatewayAlive = true;
      this.heartbeatFailCount = 0;
      await this.handleHeartbeatDeviceResolution(res);
      return;
    } catch {
      // Local failed — try relay
    }

    // Local unreachable — try relay heartbeat
    try {
      const res = await fetch(`${this.relayUrl}/api/auth/heartbeat`, {
        method: "POST",
        headers,
        body: heartbeatBody,
      });
      await this.handleHeartbeatDeviceResolution(res);
      // Relay works but local is down
      this.heartbeatFailCount += 1;
      if (this.heartbeatFailCount >= 2) {
        this.gatewayAlive = false;
      }
    } catch {
      this.heartbeatFailCount += 1;
      if (this.heartbeatFailCount >= 2) {
        this.gatewayAlive = false;
      }
    }
  }

  /** If heartbeat response resolves a different device_id via fingerprint, adopt it. */
  private async handleHeartbeatDeviceResolution(res: Response): Promise<void> {
    try {
      const data = await res.json();
      if (data.resolved_device_id && data.resolved_device_id !== this.deviceId) {
        this.deviceId = data.resolved_device_id;
        localStorage.setItem(STORAGE_KEY_DEVICE_ID, data.resolved_device_id);
      }
    } catch {
      // Non-JSON or parse error — ignore
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
    // In Tauri mode, route through Rust backend to bypass webview
    // security restrictions (WebView2 blocks http://127.0.0.1 fetch
    // from https://tauri.localhost origin in production builds).
    if (isTauri()) {
      return this.tryChatRequestViaTauri(baseUrl, body);
    }

    // 5-minute timeout for chat requests — the agent loop may run tools
    // (web search, browser, shell, etc.) which take significant time.
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), 5 * 60 * 1000);
    try {
      const res = await fetch(`${baseUrl}/api/chat`, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          Authorization: `Bearer ${this.token}`,
        },
        body: JSON.stringify(body),
        signal: controller.signal,
      });
      clearTimeout(timeout);
      return res;
    } catch (err) {
      clearTimeout(timeout);
      if (err instanceof DOMException && err.name === "AbortError") {
        // Timed out after 5 minutes — treat as network failure
        return null;
      }
      if (err instanceof TypeError && err.message === "Failed to fetch") {
        // Network-level failure (connection refused, DNS, timeout, etc.)
        return null;
      }
      throw err;
    }
  }

  /**
   * Tauri-mode chat request: routes through Rust backend's reqwest client.
   * Returns a synthetic Response object for compatibility with parseChatResponse.
   */
  private async tryChatRequestViaTauri(
    baseUrl: string,
    body: Record<string, unknown>,
  ): Promise<Response | null> {
    try {
      const result = await gatewayFetch(
        `${baseUrl}/api/chat`,
        "POST",
        {
          "Content-Type": "application/json",
          Authorization: `Bearer ${this.token}`,
        },
        JSON.stringify(body),
      );
      if (!result) {
        // gatewayFetch returned null (not in Tauri mode) — shouldn't happen
        return null;
      }
      // Build a synthetic Response for compatibility with existing parsing
      return new Response(result.body, {
        status: result.status,
        headers: { "Content-Type": "application/json" },
      });
    } catch (err) {
      // Check for network error marker from Rust backend
      if (err instanceof Error && err.message.includes("__NETWORK_ERROR__")) {
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
        // Use the user-friendly 'error' field from the server response
        if (parsed.error) {
          errorMessage = parsed.error;
        }
      } catch {
        // JSON parse failed — sanitize raw text for display
        errorMessage = this.sanitizeErrorForDisplay(text);
      }
      throw new Error(errorMessage || `Chat request failed (${res.status})`);
    }

    const data = await res.json();
    return {
      response: data.response || data.reply || "",
      model: data.model || "",
    };
  }

  /**
   * Sanitize raw error text into a user-friendly message.
   */
  private sanitizeErrorForDisplay(raw: string): string {
    if (raw.includes("401") || raw.includes("Unauthorized") || raw.includes("authentication")) {
      return "API key is invalid or expired. Please update your API key in Settings.";
    }
    if (raw.includes("429") || raw.includes("rate limit") || raw.includes("Rate limit")) {
      return "Too many requests. Please wait a moment and try again.";
    }
    if (raw.includes("context") || raw.includes("token limit") || raw.includes("too long")) {
      return "Message too long for the selected model. Try a shorter message.";
    }
    // Truncate overly long raw errors
    if (raw.length > 200) {
      return raw.substring(0, 200) + "...";
    }
    return raw;
  }

  async chat(message: string, context: string[] = []): Promise<ChatResponse> {
    if (!this.token) {
      throw new Error("Not authenticated. Please login first.");
    }

    // Include user's selected provider/model preference
    // Priority: user's explicit choice > Anthropic (if key exists) > OpenAI > Gemini
    let provider = localStorage.getItem("zeroclaw_llm_provider") || "";
    if (!provider) {
      if (localStorage.getItem("zeroclaw_api_key_anthropic")) provider = "claude";
      else if (localStorage.getItem("zeroclaw_api_key_openai")) provider = "openai";
      else provider = "gemini";
    }
    const providerDefaults: Record<string, string> = {
      claude: "claude-opus-4-6",
      openai: "gpt-5.4",
      gemini: "gemini-3.1-pro-preview",
    };
    const model = localStorage.getItem("zeroclaw_llm_model") || providerDefaults[provider] || "gemini-3.1-pro-preview";

    const keyStorageName = MoAClient.PROVIDER_KEY_MAP[provider] || provider;
    const apiKey = localStorage.getItem(`zeroclaw_api_key_${keyStorageName}`) || "";
    const hasSelectedProviderKey = !!apiKey && apiKey.trim().length > 0;

    // Build request body — include API key only when available
    // ★ Hybrid relay: when no local LLM key, send proxy_url + proxy_token
    // to the LOCAL gateway so it uses ProxyProvider for LLM calls while
    // keeping all local tool API keys and settings. This ensures the
    // operator's API key never leaves the Railway server.
    const body: Record<string, unknown> = {
      message,
      context,
      provider,
      model,
      ...(hasSelectedProviderKey ? { api_key: apiKey } : {}),
      ...(this.workspaceConnected ? { workspace_connected: true } : {}),
      ...(!hasSelectedProviderKey && this.token
        ? {
            proxy_url: `${this.relayUrl}/api/llm/proxy`,
            proxy_token: this.token,
          }
        : {}),
    };

    // ── Determine routing ──
    // ★ Two distinct modes based on whether user has a local API key:
    //
    // MODE 1 — LOCAL (user has own LLM API key):
    //   ALL conversations happen locally. Server is NEVER involved.
    //   Local MoA gateway calls LLM directly with user's API key.
    //   If local gateway is unreachable → error (do NOT fall back to relay).
    //
    // MODE 2 — RELAY (no local LLM API key):
    //   Conversations go through Railway server using operator's API key.
    //   Credits are deducted at 2.2× the base LLM cost.
    //   Local gateway is tried first (with proxy_url pointing to relay),
    //   then relay directly if local gateway is down.

    if (hasSelectedProviderKey) {
      // ════════════════════════════════════════════════════════════
      // MODE 1 — LOCAL: User has their own API key → local only
      // API key NEVER leaves the device. No relay fallback.
      // ════════════════════════════════════════════════════════════
      const res = await this.tryChatRequest(this.serverUrl, body);

      if (res !== null) {
        return this.parseChatResponse(res);
      }

      // Local gateway unreachable — do NOT send API key to relay
      this.gatewayAlive = false;
      throw new Error(
        "로컬 게이트웨이에 연결할 수 없습니다.\n" +
          "MoA 앱을 재시작하거나, 설정에서 로컬 게이트웨이 상태를 확인해 주세요.",
      );
    }

    // ════════════════════════════════════════════════════════════
    // MODE 2 — RELAY: No local API key → use server (credits 2.2×)
    // ════════════════════════════════════════════════════════════
    // Try local gateway first (it will use proxy_url to relay LLM calls
    // through Railway while keeping local tool keys and settings).
    const localAlive = this.gatewayAlive;

    if (localAlive) {
      const res = await this.tryChatRequest(this.serverUrl, body);

      if (res !== null) {
        // Local gateway connected — check for relay-fallback errors
        if (!res.ok && (res.status === 400 || res.status === 500)) {
          const errorText = await res.text().catch(() => "");
          let shouldFallback = false;
          try {
            const errorJson = JSON.parse(errorText);
            shouldFallback = errorJson.fallback_to_relay === true
              || errorJson.code === "missing_api_key"
              || errorJson.code === "provider_auth_error";
          } catch {
            shouldFallback = errorText.includes("API key")
              || errorText.includes("Unauthorized");
          }

          if (shouldFallback) {
            const fallbackBody = { ...body, api_key: undefined };
            const fallbackRes = await this.tryChatRequest(this.relayUrl, fallbackBody);
            if (fallbackRes !== null && fallbackRes.ok) {
              return this.parseChatResponse(fallbackRes);
            }
          }

          const errorMessage = this.sanitizeErrorForDisplay(errorText)
            || `Chat request failed (${res.status})`;
          throw new Error(errorMessage);
        }

        return this.parseChatResponse(res);
      }

      // Local gateway unreachable — mark as down
      this.gatewayAlive = false;
    }

    // Local gateway is down — try relay server directly (credits 2.2×)
    const relayBody = { ...body, api_key: undefined };
    const res = await this.tryChatRequest(this.relayUrl, relayBody);

    if (res !== null) {
      return this.parseChatResponse(res);
    }

    // Both failed
    throw new Error(
      "MoA 서버에 연결할 수 없습니다. 네트워크 연결을 확인해 주세요.\n" +
        "Cannot connect to MoA server. Please check your network connection.",
    );
  }

  // ── Health ─────────────────────────────────────────────────────

  async healthCheck(): Promise<HealthResponse> {
    // Try local gateway first, then relay
    const urls = this.gatewayAlive
      ? [this.serverUrl, this.relayUrl]
      : [this.relayUrl, this.serverUrl];

    for (const url of urls) {
      const controller = new AbortController();
      const timeout = setTimeout(() => controller.abort(), 5000);
      try {
        const res = await fetch(`${url}/health`, {
          method: "GET",
          signal: controller.signal,
        });
        clearTimeout(timeout);
        if (res.ok) {
          // Update gateway liveness based on which URL succeeded
          this.gatewayAlive = (url === this.serverUrl);
          return await res.json();
        }
      } catch {
        clearTimeout(timeout);
        // Continue to next URL
      }
    }
    throw new Error("Health check failed: both local and relay unreachable");
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
  // ★ SECURITY PRINCIPLE: User's API keys NEVER leave the local device.
  //   - Stored in localStorage (user's browser/Tauri app)
  //   - Stored in local config.toml via Tauri bridge (user's device)
  //   - Sent per-request in chat body for one-time use (like a password)
  //   - NEVER saved to relay/Railway server (operator's infrastructure)

  async saveApiKeyToAgent(provider: string, key: string): Promise<void> {
    // Save API key to LOCAL gateway only. Never send to relay/Railway.
    // If no local gateway is running, the key stays in localStorage
    // and is sent per-request in the chat body.
    if (!this.gatewayAlive) return;

    const headers: Record<string, string> = {
      "Content-Type": "application/json",
      ...(this.token ? { Authorization: `Bearer ${this.token}` } : {}),
    };
    const body = JSON.stringify({ provider, api_key: key });

    try {
      if (isTauri()) {
        await gatewayFetch(
          `${this.serverUrl}/api/config/api-key`, "PUT", headers, body,
        );
      } else {
        await fetch(`${this.serverUrl}/api/config/api-key`, {
          method: "PUT", headers, body,
        });
      }
    } catch {
      // Local gateway unreachable — key is safe in localStorage
    }
  }

  /** Save an API key for a specific tool. LOCAL gateway only — never relay. */
  async saveToolApiKey(tool: string, apiKey: string): Promise<void> {
    if (this.gatewayAlive) {
      try {
        const res = await fetch(`${this.serverUrl}/api/config/api-key`, {
          method: "PUT",
          headers: {
            "Content-Type": "application/json",
            ...(this.token ? { Authorization: `Bearer ${this.token}` } : {}),
          },
          body: JSON.stringify({ provider: `tool:${tool}`, api_key: apiKey }),
        });
        if (!res.ok) {
          // Non-critical
        }
      } catch {
        // Local gateway unreachable
      }
    }

    // Store locally for UI state
    if (apiKey) {
      localStorage.setItem(`zeroclaw_tool_api_key_${tool}`, "configured");
    } else {
      localStorage.removeItem(`zeroclaw_tool_api_key_${tool}`);
    }
  }

  /** Check if a tool has an API key configured (local cache check) */
  hasToolApiKey(tool: string): boolean {
    return localStorage.getItem(`zeroclaw_tool_api_key_${tool}`) === "configured";
  }

  // ── Tool Discovery ──────────────────────────────────────────────
  // Fetch available tools from the gateway so LLM knows what it can use.
  // Called after login and when gateway connection is (re)established.

  async fetchAvailableTools(): Promise<ToolInfo[]> {
    // Try local gateway first, then relay
    const urls = this.gatewayAlive
      ? [this.serverUrl, this.relayUrl]
      : [this.relayUrl, this.serverUrl];

    for (const baseUrl of urls) {
      try {
        const controller = new AbortController();
        const timeout = setTimeout(() => controller.abort(), 10000);
        const res = await fetch(`${baseUrl}/api/tools`, {
          headers: {
            ...(this.token ? { Authorization: `Bearer ${this.token}` } : {}),
          },
          signal: controller.signal,
        });
        clearTimeout(timeout);
        if (res.ok) {
          const data = await res.json();
          const tools: ToolInfo[] = (data.tools || []).map(
            (t: { name: string; description: string }) => ({
              name: t.name,
              description: t.description,
            }),
          );
          this.availableTools = tools;
          return tools;
        }
      } catch {
        // Try next URL
      }
    }
    return this.availableTools; // return cached if both fail
  }

  getAvailableTools(): ToolInfo[] {
    return this.availableTools;
  }

  /**
   * Sync provider and model selection to the local MoA agent config.
   * This ensures the server uses the correct provider/model for chat requests
   * that don't include explicit overrides (e.g. WebSocket chat).
   */
  async saveProviderModelToAgent(provider: string, model?: string): Promise<void> {
    try {
      const body: Record<string, string> = { provider };
      if (model) body.model = model;
      await fetch(`${this.serverUrl}/api/config/api-key`, {
        method: "PUT",
        headers: {
          "Content-Type": "application/json",
          Authorization: `Bearer ${this.token}`,
        },
        body: JSON.stringify(body),
      });
    } catch {
      // Local agent might not be running — preference is still saved in localStorage
    }
  }

  // ── Workspace Management ──────────────────────────────────────

  /** Whether a workspace (folder or git repo) has been explicitly connected. */
  private workspaceConnected = false;

  /** The connected workspace directory path (for UI display). */
  private workspacePath: string | null = null;

  /** Check if a workspace is currently connected. */
  isWorkspaceConnected(): boolean {
    return this.workspaceConnected;
  }

  /** Get the connected workspace path (or null). */
  getWorkspacePath(): string | null {
    return this.workspacePath;
  }

  /** Set the workspace directory on the local gateway.
   *  Also grants folder access (SecurityPolicy allowed_roots) automatically,
   *  so all file tools (file_read, file_write, file_edit, etc.) work immediately. */
  async setWorkspaceDir(dirPath: string): Promise<string> {
    await this.requireGateway();
    const headers: Record<string, string> = {
      "Content-Type": "application/json",
      ...(this.token ? { Authorization: `Bearer ${this.token}` } : {}),
    };

    // 1. Set workspace_dir in config (use gateway proxy in Tauri mode)
    let data: Record<string, string>;
    if (isTauri()) {
      const result = await gatewayFetch(
        `${this.serverUrl}/api/workspace`,
        "PUT",
        headers,
        JSON.stringify({ path: dirPath }),
      );
      if (!result || result.status >= 400) {
        const errBody = result ? JSON.parse(result.body) : { error: "Gateway unreachable" };
        throw new Error(errBody.error || `Failed to set workspace (${result?.status})`);
      }
      data = JSON.parse(result.body);
    } else {
      const res = await fetch(`${this.serverUrl}/api/workspace`, {
        method: "PUT",
        headers,
        body: JSON.stringify({ path: dirPath }),
      });
      if (!res.ok) {
        const errData = await res.json().catch(() => ({ error: "Failed to set workspace" }));
        throw new Error(errData.error || `Failed to set workspace (${res.status})`);
      }
      data = await res.json();
    }
    this.workspaceConnected = true;
    this.workspacePath = data.workspace_dir ?? dirPath;

    // 2. Also grant folder access (SecurityPolicy allowed_roots)
    //    This enables file_read/file_write/file_edit/glob_search etc.
    //    The user clicking "폴더 연결" is implicit consent for folder access.
    try {
      if (isTauri()) {
        await gatewayFetch(
          `${this.serverUrl}/api/workspace/folder`,
          "PUT",
          headers,
          JSON.stringify({ path: this.workspacePath }),
        );
      } else {
        await fetch(`${this.serverUrl}/api/workspace/folder`, {
          method: "PUT",
          headers,
          body: JSON.stringify({ path: this.workspacePath }),
        });
      }
    } catch {
      // Non-critical — workspace is set, tools may still work within workspace_dir
    }

    return this.workspacePath!;
  }

  /** Clone a GitHub repo and set it as workspace.
   *  Also grants folder access automatically after clone. */
  async connectGitHubRepo(repoUrl: string): Promise<string> {
    await this.requireGateway();
    const headers: Record<string, string> = {
      "Content-Type": "application/json",
      ...(this.token ? { Authorization: `Bearer ${this.token}` } : {}),
    };

    let data: Record<string, string>;
    if (isTauri()) {
      const result = await gatewayFetch(
        `${this.serverUrl}/api/workspace`,
        "PUT",
        headers,
        JSON.stringify({ git_url: repoUrl }),
      );
      if (!result || result.status >= 400) {
        const errBody = result ? JSON.parse(result.body) : { error: "Gateway unreachable" };
        throw new Error(errBody.error || `Failed to connect repo (${result?.status})`);
      }
      data = JSON.parse(result.body);
    } else {
      const res = await fetch(`${this.serverUrl}/api/workspace`, {
        method: "PUT",
        headers,
        body: JSON.stringify({ git_url: repoUrl }),
      });
      if (!res.ok) {
        const errData = await res.json().catch(() => ({ error: "Failed to connect repo" }));
        throw new Error(errData.error || `Failed to connect repo (${res.status})`);
      }
      data = await res.json();
    }
    this.workspaceConnected = true;
    this.workspacePath = data.workspace_dir ?? repoUrl;

    // Grant folder access for cloned repo
    try {
      if (isTauri()) {
        await gatewayFetch(
          `${this.serverUrl}/api/workspace/folder`,
          "PUT",
          headers,
          JSON.stringify({ path: this.workspacePath }),
        );
      } else {
        await fetch(`${this.serverUrl}/api/workspace/folder`, {
          method: "PUT",
          headers,
          body: JSON.stringify({ path: this.workspacePath }),
        });
      }
    } catch {
      // Non-critical
    }

    return this.workspacePath!;
  }

  /** Disconnect the workspace (reset to default). */
  disconnectWorkspace(): void {
    this.workspaceConnected = false;
    this.workspacePath = null;
  }

  // ── LLM Proxy (via Railway relay server) ──────────────────────
  // When user has no LLM API key, use Railway's /api/llm/proxy endpoint.
  // ★ SECURITY: Operator API key NEVER leaves the server.
  // The session token is used to authenticate proxy requests.
  // Credits are deducted at 2.2× per LLM call, server-side.

  /**
   * Get the LLM proxy URL for operator-key-backed LLM calls.
   * Returns the proxy endpoint URL if available, null if not authenticated.
   */
  getLlmProxyUrl(): string | null {
    if (!this.token) return null;
    return `${this.relayUrl}/api/llm/proxy`;
  }

  /**
   * Get the proxy authorization token (same as session token).
   * The token is validated server-side and has a limited TTL.
   */
  getLlmProxyToken(): string | null {
    return this.token;
  }

  /**
   * Make an LLM call through Railway's proxy endpoint.
   * The operator's API key is injected server-side — never exposed to client.
   *
   * @param provider - LLM provider name (e.g., "anthropic", "gemini")
   * @param model - Model identifier (e.g., "claude-sonnet-4")
   * @param messages - Chat messages in {role, content} format
   * @param options - Optional: temperature, max_tokens, system_prompt, tools
   * @returns LLM response with content, tool_calls, and usage info
   */
  async llmProxyChat(
    provider: string,
    model: string,
    messages: Array<{ role: string; content: string }>,
    options?: {
      temperature?: number;
      max_tokens?: number;
      system_prompt?: string;
      tools?: unknown[];
    }
  ): Promise<{
    content: string;
    tool_calls: Array<{ id: string; name: string; arguments: string }>;
    usage: { input_tokens: number; output_tokens: number; credits_deducted: number };
  }> {
    if (!this.token) {
      throw new Error("Not authenticated. Please login first.");
    }

    const res = await fetch(`${this.relayUrl}/api/llm/proxy`, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        Authorization: `Bearer ${this.token}`,
      },
      body: JSON.stringify({
        provider,
        model,
        messages,
        ...(options?.temperature !== undefined ? { temperature: options.temperature } : {}),
        ...(options?.max_tokens !== undefined ? { max_tokens: options.max_tokens } : {}),
        ...(options?.system_prompt ? { system_prompt: options.system_prompt } : {}),
        ...(options?.tools ? { tools: options.tools } : {}),
      }),
    });

    if (!res.ok) {
      const errorData = await res.json().catch(() => ({ error: `HTTP ${res.status}` }));
      if (res.status === 401) {
        throw new Error("Session expired. Please login again.");
      }
      if (res.status === 402) {
        throw new Error("Insufficient credits. Please add credits to continue.");
      }
      throw new Error(errorData.error || `Proxy request failed (${res.status})`);
    }

    return res.json();
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
