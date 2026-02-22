import { api } from './client';

export function requestOtp(email: string) {
  return api.post<{ message: string }>('/auth/otp/request', { email });
}

export function verifyOtp(email: string, code: string) {
  return api.post<{ token: string; expires_in: number }>('/auth/otp/verify', { email, code });
}

export function getMe() {
  return api.get<{
    id: string;
    email: string;
    is_super_admin: boolean;
    tenant_roles: Array<{ tenant_id: string; name: string; slug: string; role: string }>;
  }>('/auth/me');
}
