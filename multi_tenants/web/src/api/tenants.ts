import { api } from './client';

export interface Tenant {
  id: string;
  name: string;
  slug: string;
  status: string;
  plan: string;
  port: number | null;
  created_at: string;
}

export interface TenantConfig {
  provider: string;
  model: string;
  temperature: number;
  autonomy_level: string;
  system_prompt: string | null;
  api_key_masked: string;
  tool_settings?: Record<string, Record<string, unknown>>;
}

export function listTenants() { return api.get<Tenant[]>('/tenants'); }

export function createTenant(data: {
  name: string; plan: string;
  api_key?: string; provider?: string; model?: string;
  custom_slug?: string;
}) { return api.post<{ id: string; slug: string; subdomain: string; status: string }>('/tenants', data); }

export function deleteTenant(id: string) { return api.delete(`/tenants/${id}`); }
export function restartTenant(id: string) { return api.post(`/tenants/${id}/restart`); }
export function stopTenant(id: string) { return api.post(`/tenants/${id}/stop`); }
export function getTenantLogs(id: string, tail = 100) {
  return api.get<{ logs: string }>(`/tenants/${id}/logs?tail=${tail}`);
}

export function getTenantConfig(id: string) {
  return api.get<TenantConfig>(`/tenants/${id}/config`);
}

export function updateTenantConfig(id: string, data: Partial<{
  provider: string; model: string; temperature: number;
  autonomy_level: string; system_prompt: string; api_key: string;
  tool_settings: Record<string, Record<string, unknown>>;
}>) {
  return api.patch<{ updated: boolean }>(`/tenants/${id}/config`, data);
}

export function deployTenant(id: string) {
  return api.post<{ deployed: boolean; status: string }>(`/tenants/${id}/deploy`);
}

export function getTenantStatus(id: string) {
  return api.get<{ status: string; container_running: boolean }>(`/tenants/${id}/status`);
}

export function testProvider(id: string, data: { provider: string; api_key: string; model?: string }) {
  return api.post<{ success: boolean; message: string }>(`/tenants/${id}/provider/test`, data);
}

export function execInTenant(id: string, command: string) {
  return api.post<{ success: boolean; output: string }>(`/tenants/${id}/exec`, { command });
}

export function getPairingCode(id: string) {
  return api.get<{ pairing_code: string | null; status: string }>(`/tenants/${id}/pairing-code`);
}

export function resetPairing(id: string) {
  return api.post<{ reset: boolean; pairing_code: string | null }>(`/tenants/${id}/reset-pairing`);
}
