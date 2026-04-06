import type {
  StatusResponse,
  ToolSpec,
  CronJob,
  CronRun,
  Integration,
  DiagResult,
  MemoryEntry,
  CostSummary,
  CliTool,
  HealthSnapshot,
  Session,
  ChannelDetail,
  SessionMessagesResponse,
} from '../types/api';
import { clearToken, getToken, setToken } from './auth';
import { apiOrigin, basePath } from './basePath';
import { isTauri } from './tauri';

// ---------------------------------------------------------------------------
// Base fetch wrapper
// ---------------------------------------------------------------------------

export class UnauthorizedError extends Error {
  constructor() {
    super('Unauthorized');
    this.name = 'UnauthorizedError';
  }
}

// Guard to prevent concurrent auto-repair attempts.
let autoRepairInFlight = false;

/**
 * Attempt to auto-re-pair with the gateway on localhost / in the desktop app.
 * Returns true if a fresh token was obtained and stored.
 */
async function tryAutoRepair(): Promise<boolean> {
  if (autoRepairInFlight) return false;
  autoRepairInFlight = true;
  try {
    // Desktop app: the Tauri health poller handles re-pairing and injects the
    // token into localStorage. Check if it already refreshed.
    if (isTauri()) {
      const existing = getToken();
      if (existing) return true;
      // Give the Tauri health poller a moment to inject a fresh token.
      await new Promise((r) => setTimeout(r, 2000));
      return getToken() !== null;
    }

    // Web: only works on localhost where the admin endpoint is reachable.
    const origin = apiOrigin || window.location.origin;
    const codeResp = await fetch(`${origin}/admin/paircode/new`, { method: 'POST' });
    if (!codeResp.ok) return false;
    const { pairing_code } = (await codeResp.json()) as { pairing_code?: string };
    if (!pairing_code) return false;

    const pairResp = await fetch(`${origin}${basePath}/pair`, {
      method: 'POST',
      headers: { 'X-Pairing-Code': pairing_code },
    });
    if (!pairResp.ok) return false;
    const { token: newToken } = (await pairResp.json()) as { token?: string };
    if (!newToken) return false;

    setToken(newToken);
    window.dispatchEvent(new Event('zeroclaw-token-refreshed'));
    return true;
  } catch {
    return false;
  } finally {
    autoRepairInFlight = false;
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

  // Prevent stale cached responses — always fetch fresh data from the server
  headers.set('Cache-Control', 'no-cache, no-store, must-revalidate');
  headers.set('Pragma', 'no-cache');

  if (
    options.body &&
    typeof options.body === 'string' &&
    !headers.has('Content-Type')
  ) {
    headers.set('Content-Type', 'application/json');
  }

  const response = await fetch(`${apiOrigin}${basePath}${path}`, { ...options, headers });

  if (response.status === 401) {
    // Attempt auto-re-pair on localhost / desktop before forcing logout.
    // The admin endpoint is only reachable from localhost, so this is safe.
    const repaired = await tryAutoRepair();
    if (repaired) {
      // Retry the original request with the fresh token.
      const retryHeaders = new Headers(options.headers);
      retryHeaders.set('Authorization', `Bearer ${getToken()}`);
      retryHeaders.set('Cache-Control', 'no-cache, no-store, must-revalidate');
      retryHeaders.set('Pragma', 'no-cache');
      if (options.body && typeof options.body === 'string' && !retryHeaders.has('Content-Type')) {
        retryHeaders.set('Content-Type', 'application/json');
      }
      const retry = await fetch(`${apiOrigin}${basePath}${path}`, { ...options, headers: retryHeaders });
      if (retry.ok) {
        if (retry.status === 204) return undefined as unknown as T;
        return retry.json() as Promise<T>;
      }
    }

    clearToken();
    window.dispatchEvent(new Event('zeroclaw-unauthorized'));
    throw new UnauthorizedError();
  }

  if (!response.ok) {
    const text = await response.text().catch(() => '');
    throw new Error(`API ${response.status}: ${text || response.statusText}`);
  }

  // Some endpoints may return 204 No Content
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

// ---------------------------------------------------------------------------
// Pairing
// ---------------------------------------------------------------------------

export async function pair(code: string): Promise<{ token: string }> {
  const response = await fetch(`${basePath}/pair`, {
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

export async function getAdminPairCode(): Promise<{ pairing_code: string | null; pairing_required: boolean }> {
  // Use the public /pair/code endpoint which works in Docker and remote environments
  // (no localhost restriction). Falls back to the admin endpoint for backward compat.
  const publicResp = await fetch(`${basePath}/pair/code`);
  if (publicResp.ok) {
    return publicResp.json() as Promise<{ pairing_code: string | null; pairing_required: boolean }>;
  }

  const response = await fetch('/admin/paircode');
  if (!response.ok) {
    throw new Error(`Failed to fetch pairing code (${response.status})`);
  }
  return response.json() as Promise<{ pairing_code: string | null; pairing_required: boolean }>;
}

// ---------------------------------------------------------------------------
// Public health (no auth required)
// ---------------------------------------------------------------------------

export async function getPublicHealth(): Promise<{ require_pairing: boolean; paired: boolean }> {
  const response = await fetch(`${basePath}/health`);
  if (!response.ok) {
    throw new Error(`Health check failed (${response.status})`);
  }
  return response.json() as Promise<{ require_pairing: boolean; paired: boolean }>;
}

// ---------------------------------------------------------------------------
// Status / Health
// ---------------------------------------------------------------------------

export function getStatus(): Promise<StatusResponse> {
  return apiFetch<StatusResponse>('/api/status');
}

export function getHealth(): Promise<HealthSnapshot> {
  return apiFetch<HealthSnapshot | { health: HealthSnapshot }>('/api/health').then((data) =>
    unwrapField(data, 'health'),
  );
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

export function getTools(): Promise<ToolSpec[]> {
  return apiFetch<ToolSpec[] | { tools: ToolSpec[] }>('/api/tools').then((data) =>
    unwrapField(data, 'tools'),
  );
}

// ---------------------------------------------------------------------------
// Cron
// ---------------------------------------------------------------------------

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
export function patchCronJob(
  id: string,
  patch: { name?: string; schedule?: string; command?: string },
): Promise<CronJob> {
  return apiFetch<CronJob | { status: string; job: CronJob }>(
    `/api/cron/${encodeURIComponent(id)}`,
    {
      method: 'PATCH',
      body: JSON.stringify(patch),
    },
  ).then((data) => (typeof (data as { job?: CronJob }).job === 'object' ? (data as { job: CronJob }).job : (data as CronJob)));
}


export function getCronRuns(
  jobId: string,
  limit: number = 20,
): Promise<CronRun[]> {
  const params = new URLSearchParams({ limit: String(limit) });
  return apiFetch<CronRun[] | { runs: CronRun[] }>(
    `/api/cron/${encodeURIComponent(jobId)}/runs?${params}`,
  ).then((data) => unwrapField(data, 'runs'));
}

export interface CronSettings {
  enabled: boolean;
  catch_up_on_startup: boolean;
  max_run_history: number;
}

export function getCronSettings(): Promise<CronSettings> {
  return apiFetch<CronSettings>('/api/cron/settings');
}

export function patchCronSettings(
  patch: Partial<CronSettings>,
): Promise<CronSettings> {
  return apiFetch<CronSettings & { status: string }>('/api/cron/settings', {
    method: 'PATCH',
    body: JSON.stringify(patch),
  });
}

// ---------------------------------------------------------------------------
// Integrations
// ---------------------------------------------------------------------------

export function getIntegrations(): Promise<Integration[]> {
  return apiFetch<Integration[] | { integrations: Integration[] }>('/api/integrations').then(
    (data) => unwrapField(data, 'integrations'),
  );
}

// ---------------------------------------------------------------------------
// Doctor / Diagnostics
// ---------------------------------------------------------------------------

export function runDoctor(): Promise<DiagResult[]> {
  return apiFetch<DiagResult[] | { results: DiagResult[]; summary?: unknown }>('/api/doctor', {
    method: 'POST',
    body: JSON.stringify({}),
  }).then((data) => (Array.isArray(data) ? data : data.results));
}

// ---------------------------------------------------------------------------
// Memory
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Cost
// ---------------------------------------------------------------------------

export function getCost(): Promise<CostSummary> {
  return apiFetch<CostSummary | { cost: CostSummary }>('/api/cost').then((data) =>
    unwrapField(data, 'cost'),
  );
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

export function getSessions(): Promise<Session[]> {
  return apiFetch<Session[] | { sessions: Session[] }>('/api/sessions').then((data) =>
    unwrapField(data, 'sessions'),
  );
}

export function getSession(id: string): Promise<Session> {
  return apiFetch<Session>(`/api/sessions/${encodeURIComponent(id)}`);
}

/** Load persisted gateway WebSocket chat transcript for the dashboard Agent Chat. */
export function getSessionMessages(id: string): Promise<SessionMessagesResponse> {
  return apiFetch<SessionMessagesResponse>(
    `/api/sessions/${encodeURIComponent(id)}/messages`,
  );
}

// ---------------------------------------------------------------------------
// Channels (detailed)
// ---------------------------------------------------------------------------

export function getChannels(): Promise<ChannelDetail[]> {
  return apiFetch<ChannelDetail[] | { channels: ChannelDetail[] }>('/api/channels').then((data) =>
    unwrapField(data, 'channels'),
  );
}

// ---------------------------------------------------------------------------
// CLI Tools
// ---------------------------------------------------------------------------

export function getCliTools(): Promise<CliTool[]> {
  return apiFetch<CliTool[] | { cli_tools: CliTool[] }>('/api/cli-tools').then((data) =>
    unwrapField(data, 'cli_tools'),
  );
}
