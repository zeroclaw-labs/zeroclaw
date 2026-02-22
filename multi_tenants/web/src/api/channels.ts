import { api } from './client';

export interface Channel {
  id: string;
  kind: string;
  enabled: boolean;
  created_at: string;
}

export interface ChannelDetail extends Channel {
  tenant_id: string;
  config: Record<string, unknown>;
}

export function listChannels(tenantId: string) {
  return api.get<Channel[]>(`/tenants/${tenantId}/channels`);
}
export function createChannel(tenantId: string, data: { kind: string; config: Record<string, unknown> }) {
  return api.post<Channel>(`/tenants/${tenantId}/channels`, data);
}
export function getChannel(tenantId: string, channelId: string) {
  return api.get<ChannelDetail>(`/tenants/${tenantId}/channels/${channelId}`);
}
export function updateChannel(tenantId: string, channelId: string, data: { config?: Record<string, unknown>; enabled?: boolean }) {
  return api.patch<{ updated: boolean }>(`/tenants/${tenantId}/channels/${channelId}`, data);
}
export function deleteChannel(tenantId: string, channelId: string) {
  return api.delete(`/tenants/${tenantId}/channels/${channelId}`);
}
