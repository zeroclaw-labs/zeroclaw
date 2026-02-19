import type {
  StatusResponse,
  ToolSpec,
  CronJob,
  Integration,
  DiagResult,
  MemoryEntry,
  CostSummary,
  CliTool,
  HealthSnapshot,
} from '../types/api';

const TOKEN_KEY = 'zeroclaw_token';

// ---------------------------------------------------------------------------
// Token helpers
// ---------------------------------------------------------------------------

export function getToken(): string | null {
  return localStorage.getItem(TOKEN_KEY);
}

export function setToken(token: string): void {
  localStorage.setItem(TOKEN_KEY, token);
}

export function clearToken(): void {
  localStorage.removeItem(TOKEN_KEY);
}

// ---------------------------------------------------------------------------
// Base fetch wrapper
// ---------------------------------------------------------------------------

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

  const response = await fetch(path, { ...options, headers });

  if (response.status === 401) {
    clearToken();
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

// ---------------------------------------------------------------------------
// Pairing
// ---------------------------------------------------------------------------

export async function pair(code: string): Promise<{ token: string }> {
  const response = await fetch('/pair', {
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

// ---------------------------------------------------------------------------
// Status / Health
// ---------------------------------------------------------------------------

export function getStatus(): Promise<StatusResponse> {
  return apiFetch<StatusResponse>('/api/status');
}

export function getHealth(): Promise<HealthSnapshot> {
  return apiFetch<HealthSnapshot>('/api/health');
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

export function getConfig(): Promise<string> {
  return apiFetch<string>('/api/config');
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
  return apiFetch<ToolSpec[]>('/api/tools');
}

// ---------------------------------------------------------------------------
// Cron
// ---------------------------------------------------------------------------

export function getCronJobs(): Promise<CronJob[]> {
  return apiFetch<CronJob[]>('/api/cron');
}

export function addCronJob(body: {
  name?: string;
  command: string;
  schedule: string;
  enabled?: boolean;
}): Promise<CronJob> {
  return apiFetch<CronJob>('/api/cron', {
    method: 'POST',
    body: JSON.stringify(body),
  });
}

export function deleteCronJob(id: string): Promise<void> {
  return apiFetch<void>(`/api/cron/${encodeURIComponent(id)}`, {
    method: 'DELETE',
  });
}

// ---------------------------------------------------------------------------
// Integrations
// ---------------------------------------------------------------------------

export function getIntegrations(): Promise<Integration[]> {
  return apiFetch<Integration[]>('/api/integrations');
}

// ---------------------------------------------------------------------------
// Doctor / Diagnostics
// ---------------------------------------------------------------------------

export function runDoctor(): Promise<DiagResult[]> {
  return apiFetch<DiagResult[]>('/api/doctor');
}

// ---------------------------------------------------------------------------
// Memory
// ---------------------------------------------------------------------------

export function getMemory(
  query?: string,
  category?: string,
): Promise<MemoryEntry[]> {
  const params = new URLSearchParams();
  if (query) params.set('q', query);
  if (category) params.set('category', category);
  const qs = params.toString();
  return apiFetch<MemoryEntry[]>(`/api/memory${qs ? `?${qs}` : ''}`);
}

export function storeMemory(
  key: string,
  content: string,
  category?: string,
): Promise<MemoryEntry> {
  return apiFetch<MemoryEntry>('/api/memory', {
    method: 'POST',
    body: JSON.stringify({ key, content, category }),
  });
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
  return apiFetch<CostSummary>('/api/cost');
}

// ---------------------------------------------------------------------------
// CLI Tools
// ---------------------------------------------------------------------------

export function getCliTools(): Promise<CliTool[]> {
  return apiFetch<CliTool[]>('/api/cli-tools');
}
