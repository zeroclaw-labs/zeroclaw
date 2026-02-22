import { api } from './client';

export interface DashboardData {
  total_tenants: number;
  running_tenants: number;
  stopped_tenants: number;
  error_tenants: number;
  total_users: number;
  total_channels: number;
}

export interface HealthEntry {
  tenant_id: string;
  slug: string;
  status: string;
  last_check: string | null;
}

export interface UsageEntry {
  tenant_id: string;
  period: string;
  messages: number;
  tokens_in: number;
  tokens_out: number;
}

export interface AuditEntry {
  id: string;
  action: string;
  resource: string;
  resource_id: string | null;
  actor_id: string | null;
  details: string | null;
  created_at: string;
}

export interface AuditPage {
  entries: AuditEntry[];
  total: number;
  page: number;
  per_page: number;
}

export function getDashboard() { return api.get<DashboardData>('/monitoring/dashboard'); }
export function getHealth() { return api.get<HealthEntry[]>('/monitoring/health'); }
export function getUsage(tenantId?: string, days = 30) {
  const params = new URLSearchParams({ days: String(days) });
  if (tenantId) params.set('tenant_id', tenantId);
  return api.get<UsageEntry[]>(`/monitoring/usage?${params}`);
}
export function getAudit(page = 1, perPage = 50, filters?: { action?: string; actor_id?: string; since?: string }) {
  const params = new URLSearchParams({ page: String(page), per_page: String(perPage) });
  if (filters?.action) params.set('action', filters.action);
  if (filters?.actor_id) params.set('actor_id', filters.actor_id);
  if (filters?.since) params.set('since', filters.since);
  return api.get<AuditPage>(`/monitoring/audit?${params}`);
}
