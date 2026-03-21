/**
 * Tauri Bridge — TypeScript interface for Tauri backend commands.
 *
 * Detects whether the app is running inside Tauri (desktop/mobile) and
 * provides typed wrappers around `invoke()` calls.  When running in a
 * plain browser, every call returns `null` so callers can fall back to
 * direct HTTP fetch.
 */

// ── Types ────────────────────────────────────────────────────────

export interface SyncStatus {
  connected: boolean;
  device_id: string;
  last_sync: number | null;
}

export interface PlatformInfo {
  os: string;
  arch: string;
  is_mobile: boolean;
}

export interface ResumeResult {
  restored_token: boolean;
  restored_url: boolean;
  is_online: boolean;
  has_token: boolean;
}

// ── Runtime detection ────────────────────────────────────────────

/** True when the app is running inside Tauri (desktop or mobile). */
export function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

// ── Lazy import of Tauri API ─────────────────────────────────────

let invokeImpl: ((cmd: string, args?: Record<string, unknown>) => Promise<unknown>) | null = null;
let listenImpl: ((event: string, handler: (event: { payload: unknown }) => void) => Promise<() => void>) | null = null;

async function getInvoke() {
  if (invokeImpl) return invokeImpl;
  if (!isTauri()) return null;
  try {
    const mod = await import("@tauri-apps/api/core");
    invokeImpl = mod.invoke;
    return invokeImpl;
  } catch {
    return null;
  }
}

async function getListen() {
  if (listenImpl) return listenImpl;
  if (!isTauri()) return null;
  try {
    const mod = await import("@tauri-apps/api/event");
    listenImpl = mod.listen;
    return listenImpl;
  } catch {
    return null;
  }
}

// ── Typed invoke wrappers ────────────────────────────────────────

/** Get sync connection status + device ID. */
export async function getSyncStatus(): Promise<SyncStatus | null> {
  const invoke = await getInvoke();
  if (!invoke) return null;
  return invoke("get_sync_status") as Promise<SyncStatus>;
}

/** Trigger a full sync (Layer 3) with the server. */
export async function triggerFullSync(): Promise<string | null> {
  const invoke = await getInvoke();
  if (!invoke) return null;
  return invoke("trigger_full_sync") as Promise<string>;
}

/** Get platform info (os, arch, is_mobile). */
export async function getPlatformInfo(): Promise<PlatformInfo | null> {
  const invoke = await getInvoke();
  if (!invoke) return null;
  return invoke("get_platform_info") as Promise<PlatformInfo>;
}

/** Get a deterministic device fingerprint based on machine hardware info.
 *  This survives app reinstalls and is used for device deduplication. */
export async function getDeviceFingerprint(): Promise<string | null> {
  const invoke = await getInvoke();
  if (!invoke) return null;
  return invoke("get_device_fingerprint") as Promise<string>;
}

/** Notify backend that the app is going to background. */
export async function onAppPause(): Promise<void> {
  const invoke = await getInvoke();
  if (!invoke) return;
  await invoke("on_app_pause");
}

/** Notify backend that the app returned to foreground. */
export async function onAppResume(): Promise<ResumeResult | null> {
  const invoke = await getInvoke();
  if (!invoke) return null;
  return invoke("on_app_resume") as Promise<ResumeResult>;
}

/** Check if the backend has an active auth token. */
export async function isAuthenticated(): Promise<boolean | null> {
  const invoke = await getInvoke();
  if (!invoke) return null;
  return invoke("is_authenticated") as Promise<boolean>;
}

/** Clear backend auth token and stop sync. */
export async function disconnectBackend(): Promise<void> {
  const invoke = await getInvoke();
  if (!invoke) return;
  await invoke("disconnect");
}

/** Check if the local MoA gateway is running. */
export async function isGatewayRunning(): Promise<boolean | null> {
  const invoke = await getInvoke();
  if (!invoke) return null;
  return invoke("is_gateway_running") as Promise<boolean>;
}

/** Get current server URL from backend. */
export async function getServerUrl(): Promise<string | null> {
  const invoke = await getInvoke();
  if (!invoke) return null;
  return invoke("get_server_url") as Promise<string>;
}

/** Set server URL on the backend. */
export async function setServerUrl(url: string): Promise<void> {
  const invoke = await getInvoke();
  if (!invoke) return;
  await invoke("set_server_url", { url });
}

// ── Auth commands (new multi-user flow) ──────────────────────────

/** Login via Tauri backend (proxies to /api/auth/login). */
export async function authLogin(
  username: string,
  password: string,
  deviceId?: string,
  deviceName?: string,
): Promise<unknown | null> {
  const invoke = await getInvoke();
  if (!invoke) return null;
  return invoke("auth_login", {
    username,
    password,
    device_id: deviceId,
    device_name: deviceName,
  });
}

/** Register via Tauri backend (proxies to /api/auth/register). */
export async function authRegister(
  username: string,
  password: string,
): Promise<unknown | null> {
  const invoke = await getInvoke();
  if (!invoke) return null;
  return invoke("auth_register", { username, password });
}

// ── MoA Config Commands ─────────────────────────────────────

/** Write provider/API key to MoA's ~/.zeroclaw/config.toml. */
export async function writeMoAConfig(
  provider: string,
  apiKey?: string,
  model?: string,
): Promise<string | null> {
  const invoke = await getInvoke();
  if (!invoke) return null;
  return invoke("write_zeroclaw_config", {
    provider,
    api_key: apiKey ?? null,
    model: model ?? null,
  }) as Promise<string>;
}

/** Check if MoA config.toml already exists. */
export async function isMoAConfigured(): Promise<boolean | null> {
  const invoke = await getInvoke();
  if (!invoke) return null;
  return invoke("is_zeroclaw_configured") as Promise<boolean>;
}

// ── Gateway status event listener ────────────────────────────────

export interface GatewayStatusEvent {
  status: "starting" | "ready" | "failed";
  message: string;
}

/**
 * Listen for gateway-status events emitted by the Rust backend.
 * Returns an unlisten function.
 */
export async function onGatewayStatus(
  handler: (event: GatewayStatusEvent) => void,
): Promise<() => void> {
  const listen = await getListen();
  if (!listen) return () => {};
  return listen("gateway-status", (e) => {
    handler(e.payload as GatewayStatusEvent);
  });
}

// ── Python environment status event listener ────────────────────

export interface PythonEnvStatus {
  stage: "creating_venv" | "venv_created" | "installing_packages" | "packages_installed" | "ready" | "error";
  detail: string;
}

/**
 * Listen for python-env-status events emitted by the Rust backend.
 * These fire during first-launch setup when the app auto-installs
 * pymupdf4llm into the embedded Python venv.
 * Returns an unlisten function.
 */
export async function onPythonEnvStatus(
  handler: (event: PythonEnvStatus) => void,
): Promise<() => void> {
  const listen = await getListen();
  if (!listen) return () => {};
  return listen("python-env-status", (e) => {
    handler(e.payload as PythonEnvStatus);
  });
}

/** Check the current Python environment status. */
export async function checkPythonEnv(): Promise<{
  venv_exists: boolean;
  packages_installed: boolean;
  python_path: string | null;
} | null> {
  const invoke = await getInvoke();
  if (!invoke) return null;
  return invoke("check_python_env") as Promise<{
    venv_exists: boolean;
    packages_installed: boolean;
    python_path: string | null;
  }>;
}

// ── Mobile lifecycle event listeners ─────────────────────────────

/** Register a handler for Tauri lifecycle events. Returns an unlisten fn. */
export async function onLifecycleEvent(
  handler: (event: "pause" | "resume", data?: ResumeResult) => void,
): Promise<() => void> {
  const listen = await getListen();
  if (!listen) return () => {};

  const unlisteners: (() => void)[] = [];

  // Tauri 2 emits "tauri://focus" / "tauri://blur" on mobile
  const unFocus = await listen("tauri://focus", async () => {
    const result = await onAppResume();
    handler("resume", result ?? undefined);
  });
  unlisteners.push(unFocus);

  const unBlur = await listen("tauri://blur", async () => {
    await onAppPause();
    handler("pause");
  });
  unlisteners.push(unBlur);

  return () => unlisteners.forEach((fn) => fn());
}
