import { api } from './client';
export { formatBytes, timeAgo } from '../utils/format';

export interface ResourceSnapshot {
  cpu_pct: number;
  mem_bytes: number;
  mem_limit: number;
  disk_bytes: number;
  net_in_bytes: number;
  net_out_bytes: number;
  pids: number;
  ts: string;
}

export interface TenantResources {
  current: ResourceSnapshot | null;
  history: ResourceSnapshot[];
}

export interface AdminResourceEntry {
  tenant_id: string;
  slug: string;
  name: string;
  status: string;
  cpu_pct: number;
  mem_bytes: number;
  mem_limit: number;
  disk_bytes: number;
  pids: number;
  ts: string;
}

export function getTenantResources(id: string, range = '1h') {
  return api.get<TenantResources>(`/tenants/${id}/resources?range=${range}`);
}

export function getAdminResources() {
  return api.get<AdminResourceEntry[]>('/monitoring/resources');
}
