import type {
  StatusResponse,
  ToolSpec,
  CronJob,
  Integration,
  IntegrationSettingsPayload,
  DiagResult,
  MemoryEntry,
  PairedDevice,
  CostSummary,
  CliTool,
  HealthSnapshot,
} from '../types/api';
import { clearToken, getToken, setToken } from './auth';

const API_BASE = process.env.NEXT_PUBLIC_DEFAULT_SERVER_URL || '';

function resolveUrl(path: string): string {
  if (API_BASE) return `${API_BASE}${path}`;
  return path;
}

export class UnauthorizedError extends Error {
  constructor() {
    super('Unauthorized');
    this.name = 'UnauthorizedError';
  }
}

export async function apiFetch<T = unknown>(
  path: string,
  options: RequestInit = {},
): Promise<T> {
  const token = getToken();
  const headers = new Headers(options.headers);

  if (token) {
    headers.set('Authorization', `Bearer ${token}`);
  }

  if (
    options.body &&
    typeof options.body === 'string' &&
    !headers.has('Content-Type')
  ) {
    headers.set('Content-Type', 'application/json');
  }

  const response = await fetch(resolveUrl(path), { ...options, headers });

  if (response.status === 401) {
    clearToken();
    window.dispatchEvent(new Event('zeroclaw-unauthorized'));
    throw new UnauthorizedError();
  }

  if (!response.ok) {
    const text = await response.text().catch(() => '');
    throw new Error(`API ${response.status}: ${text || response.statusText}`);
  }

  if (response.status === 204) {
    return undefined as unknown as T;
  }

  return response.json() as Promise<T>;
}

function unwrapField<T>(value: T | Record<string, T>, key: string): T {
  if (value !== null && typeof value === 'object' && !Array.isArray(value) && key in value) {
    const unwrapped = (value as Record<string, T | undefined>)[key];
    if (unwrapped !== undefined) {
      return unwrapped;
    }
  }
  return value as T;
}

// Pairing
export async function pair(code: string): Promise<{ token: string }> {
  const response = await fetch(resolveUrl('/pair'), {
    method: 'POST',
    headers: { 'X-Pairing-Code': code },
  });

  if (!response.ok) {
    const text = await response.text().catch(() => '');
    throw new Error(`Pairing failed (${response.status}): ${text || response.statusText}`);
  }

  const data = (await response.json()) as { token: string };
  setToken(data.token);
  return data;
}

// Public health (no auth required)
export async function getPublicHealth(): Promise<{ require_pairing: boolean; paired: boolean }> {
  const response = await fetch(resolveUrl('/health'));
  if (!response.ok) {
    throw new Error(`Health check failed (${response.status})`);
  }
  return response.json() as Promise<{ require_pairing: boolean; paired: boolean }>;
}

// Status / Health
export function getStatus(): Promise<StatusResponse> {
  return apiFetch<StatusResponse>('/api/status');
}

export function getHealth(): Promise<HealthSnapshot> {
  return apiFetch<HealthSnapshot | { health: HealthSnapshot }>('/api/health').then((data) =>
    unwrapField(data, 'health'),
  );
}

// Config
export function getConfig(): Promise<string> {
  return apiFetch<string | { format?: string; content: string }>('/api/config').then((data) =>
    typeof data === 'string' ? data : data.content,
  );
}

export function putConfig(toml: string): Promise<void> {
  return apiFetch<void>('/api/config', {
    method: 'PUT',
    headers: { 'Content-Type': 'application/toml' },
    body: toml,
  });
}

// Tools
export function getTools(): Promise<ToolSpec[]> {
  return apiFetch<ToolSpec[] | { tools: ToolSpec[] }>('/api/tools').then((data) =>
    unwrapField(data, 'tools'),
  );
}

// Cron
export function getCronJobs(): Promise<CronJob[]> {
  return apiFetch<CronJob[] | { jobs: CronJob[] }>('/api/cron').then((data) =>
    unwrapField(data, 'jobs'),
  );
}

export function addCronJob(body: {
  name?: string;
  command: string;
  schedule: string;
  enabled?: boolean;
}): Promise<CronJob> {
  return apiFetch<CronJob | { status: string; job: CronJob }>('/api/cron', {
    method: 'POST',
    body: JSON.stringify(body),
  }).then((data) => (typeof (data as { job?: CronJob }).job === 'object' ? (data as { job: CronJob }).job : (data as CronJob)));
}

export function deleteCronJob(id: string): Promise<void> {
  return apiFetch<void>(`/api/cron/${encodeURIComponent(id)}`, {
    method: 'DELETE',
  });
}

// Integrations
export function getIntegrations(): Promise<Integration[]> {
  return apiFetch<Integration[] | { integrations: Integration[] }>('/api/integrations').then(
    (data) => unwrapField(data, 'integrations'),
  );
}

export function getIntegrationSettings(): Promise<IntegrationSettingsPayload> {
  return apiFetch<IntegrationSettingsPayload>('/api/integrations/settings');
}

export function putIntegrationCredentials(
  integrationId: string,
  body: { revision?: string; fields: Record<string, string> },
): Promise<{ status: string; revision: string; unchanged?: boolean }> {
  return apiFetch<{ status: string; revision: string; unchanged?: boolean }>(
    `/api/integrations/${encodeURIComponent(integrationId)}/credentials`,
    {
      method: 'PUT',
      body: JSON.stringify(body),
    },
  );
}

// Doctor / Diagnostics
export function runDoctor(): Promise<DiagResult[]> {
  return apiFetch<DiagResult[] | { results: DiagResult[]; summary?: unknown }>('/api/doctor', {
    method: 'POST',
    body: JSON.stringify({}),
  }).then((data) => (Array.isArray(data) ? data : data.results));
}

// Memory
export function getMemory(
  query?: string,
  category?: string,
): Promise<MemoryEntry[]> {
  const params = new URLSearchParams();
  if (query) params.set('query', query);
  if (category) params.set('category', category);
  const qs = params.toString();
  return apiFetch<MemoryEntry[] | { entries: MemoryEntry[] }>(`/api/memory${qs ? `?${qs}` : ''}`).then(
    (data) => unwrapField(data, 'entries'),
  );
}

export function storeMemory(
  key: string,
  content: string,
  category?: string,
): Promise<void> {
  return apiFetch<unknown>('/api/memory', {
    method: 'POST',
    body: JSON.stringify({ key, content, category }),
  }).then(() => undefined);
}

export function deleteMemory(key: string): Promise<void> {
  return apiFetch<void>(`/api/memory/${encodeURIComponent(key)}`, {
    method: 'DELETE',
  });
}

// Paired Devices
export function getPairedDevices(): Promise<PairedDevice[]> {
  return apiFetch<PairedDevice[] | { devices: PairedDevice[] }>('/api/pairing/devices').then(
    (data) => unwrapField(data, 'devices'),
  );
}

export function revokePairedDevice(id: string): Promise<void> {
  return apiFetch<void>(`/api/pairing/devices/${encodeURIComponent(id)}`, {
    method: 'DELETE',
  });
}

// Cost
export function getCost(): Promise<CostSummary> {
  return apiFetch<CostSummary | { cost: CostSummary }>('/api/cost').then((data) =>
    unwrapField(data, 'cost'),
  );
}

// CLI Tools
export function getCliTools(): Promise<CliTool[]> {
  return apiFetch<CliTool[] | { cli_tools: CliTool[] }>('/api/cli-tools').then((data) =>
    unwrapField(data, 'cli_tools'),
  );
}

// User Auth
export interface AuthRegisterResponse {
  status: string;
  user_id: string;
}

export interface UserDevice {
  device_id: string;
  device_name: string;
  platform: string;
  last_seen_at: string | null;
  is_online: boolean;
}

export interface AuthLoginResponse {
  status: string;
  token: string;
  user_id: string;
  username: string;
  email: string;
  devices: UserDevice[];
}

export interface EmailVerificationResponse {
  status: string;
  expires_in_seconds: number;
}

export interface EmailVerifyCodeResponse {
  status: string;
  token: string;
}

export interface DevicePairResponse {
  status: string;
  token: string;
  device_id: string;
  device_name: string;
}

export async function authRegister(
  username: string,
  password: string,
  email?: string,
): Promise<AuthRegisterResponse> {
  const response = await fetch(resolveUrl('/api/auth/register'), {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ username, password, email }),
  });

  if (!response.ok) {
    const data = await response.json().catch(() => ({}));
    throw new Error(data.error || `Registration failed (${response.status})`);
  }

  return response.json() as Promise<AuthRegisterResponse>;
}

export async function authLogin(
  username: string,
  password: string,
  deviceId?: string,
  deviceName?: string,
): Promise<AuthLoginResponse> {
  const response = await fetch(resolveUrl('/api/auth/login'), {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      username,
      password,
      device_id: deviceId,
      device_name: deviceName,
    }),
  });

  if (!response.ok) {
    const data = await response.json().catch(() => ({}));
    throw new Error(data.error || `Login failed (${response.status})`);
  }

  const data = (await response.json()) as AuthLoginResponse;
  // Don't set token yet - wait for device selection + pairing + email verification
  return data;
}

export async function authLogout(): Promise<void> {
  const token = getToken();
  if (token) {
    await fetch(resolveUrl('/api/auth/logout'), {
      method: 'POST',
      headers: { Authorization: `Bearer ${token}` },
    }).catch(() => {});
  }
  clearToken();
}

// Remote device login (pairing code + credentials -> email verification)
// Uses POST /api/remote/login which validates user+password+device+pairing_code
// and triggers email verification if configured.
export interface RemoteLoginResponse {
  status: string;
  user_id: string;
  device_id: string;
  device_name: string;
  requires_email_verification: boolean;
  email_hint?: string;
  token?: string;
}

export async function remoteLogin(
  username: string,
  password: string,
  deviceId: string,
  pairingCode: string,
): Promise<RemoteLoginResponse> {
  const response = await fetch(resolveUrl('/api/remote/login'), {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      username,
      password,
      device_id: deviceId,
      pairing_code: pairingCode,
    }),
  });

  if (!response.ok) {
    const data = await response.json().catch(() => ({}));
    throw new Error(data.error || `Remote login failed (${response.status})`);
  }

  const data = (await response.json()) as RemoteLoginResponse;
  // If email verification is NOT required, token is returned directly
  if (data.token && !data.requires_email_verification) {
    setToken(data.token);
  }
  return data;
}

// Verify email code for remote device access
// Uses POST /api/remote/verify-email
export async function verifyRemoteEmail(
  userId: string,
  code: string,
): Promise<EmailVerifyCodeResponse> {
  const response = await fetch(resolveUrl('/api/remote/verify-email'), {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ user_id: userId, code }),
  });

  if (!response.ok) {
    const data = await response.json().catch(() => ({}));
    throw new Error(data.error || `Email verification failed (${response.status})`);
  }

  const data = (await response.json()) as EmailVerifyCodeResponse;
  setToken(data.token);
  return data;
}

// Get user devices list
// Uses GET /api/auth/devices (requires auth token from initial login)
export async function getUserDevices(loginToken: string): Promise<UserDevice[]> {
  const response = await fetch(resolveUrl('/api/auth/devices'), {
    headers: { Authorization: `Bearer ${loginToken}` },
  });

  if (!response.ok) {
    const data = await response.json().catch(() => ({}));
    throw new Error(data.error || `Failed to get devices (${response.status})`);
  }

  const data = await response.json();
  return (data.devices || data || []) as UserDevice[];
}

// Get remote devices list (alternative endpoint for web flow)
export async function getRemoteDevices(loginToken: string): Promise<UserDevice[]> {
  const response = await fetch(resolveUrl('/api/remote/devices'), {
    headers: { Authorization: `Bearer ${loginToken}` },
  });

  if (!response.ok) {
    // Fallback to /api/auth/devices if /api/remote/devices fails
    return getUserDevices(loginToken);
  }

  const data = await response.json();
  return (data.devices || data || []) as UserDevice[];
}

// Kakao OAuth
export interface KakaoAuthResponse {
  status: string;
  token: string;
  user_id: string;
  username: string;
  kakao_id: string;
  email: string;
  devices: UserDevice[];
}

export async function authKakaoCallback(code: string): Promise<KakaoAuthResponse> {
  const response = await fetch(resolveUrl('/api/auth/kakao/callback'), {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ code }),
  });

  if (!response.ok) {
    const data = await response.json().catch(() => ({}));
    throw new Error(data.error || `Kakao login failed (${response.status})`);
  }

  return response.json() as Promise<KakaoAuthResponse>;
}
