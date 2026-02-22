import { api } from './client';

export interface User {
  id: string;
  email: string;
  name: string | null;
  is_super_admin: boolean;
  created_at: string;
}

export function listUsers() { return api.get<User[]>('/users'); }
export function createUser(data: { email: string; name?: string }) {
  return api.post<User>('/users', data);
}
export function deleteUser(id: string) { return api.delete(`/users/${id}`); }
export function updateUser(id: string, data: { name?: string; is_super_admin?: boolean }) {
  return api.patch<{ updated: boolean }>(`/users/${id}`, data);
}
