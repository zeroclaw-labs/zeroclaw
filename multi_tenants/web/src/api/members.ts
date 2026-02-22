import { api } from './client';

export interface Member {
  id: string;
  user_id: string;
  email: string;
  role: string;
  joined_at: string;
}

export function listMembers(tenantId: string) {
  return api.get<Member[]>(`/tenants/${tenantId}/members`);
}
export function addMember(tenantId: string, data: { email: string; role: string }) {
  return api.post<Member>(`/tenants/${tenantId}/members`, data);
}
export function updateMemberRole(tenantId: string, memberId: string, role: string) {
  return api.patch(`/tenants/${tenantId}/members/${memberId}`, { role });
}
export function removeMember(tenantId: string, memberId: string) {
  return api.delete(`/tenants/${tenantId}/members/${memberId}`);
}
