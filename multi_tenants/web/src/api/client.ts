const API_BASE = '/api';

class ApiClient {
  private token: string | null = null;

  setToken(token: string | null) {
    this.token = token;
    if (token) localStorage.setItem('jwt', token);
    else localStorage.removeItem('jwt');
  }

  getToken(): string | null {
    if (!this.token) this.token = localStorage.getItem('jwt');
    return this.token;
  }

  async request<T>(path: string, options: RequestInit = {}): Promise<T> {
    const headers: Record<string, string> = {
      'Content-Type': 'application/json',
      ...(options.headers as Record<string, string>),
    };
    const token = this.getToken();
    if (token) headers['Authorization'] = `Bearer ${token}`;

    const resp = await fetch(`${API_BASE}${path}`, { ...options, headers });

    if (resp.status === 401) {
      this.setToken(null);
      window.location.href = '/login';
      throw new Error('unauthorized');
    }
    if (!resp.ok) {
      const body = await resp.json().catch(() => ({ error: resp.statusText }));
      throw new Error(body.error || resp.statusText);
    }
    if (resp.status === 204) return undefined as T;
    return resp.json();
  }

  get<T>(path: string) { return this.request<T>(path); }
  post<T>(path: string, body?: unknown) {
    return this.request<T>(path, { method: 'POST', body: body ? JSON.stringify(body) : undefined });
  }
  patch<T>(path: string, body: unknown) {
    return this.request<T>(path, { method: 'PATCH', body: JSON.stringify(body) });
  }
  delete<T>(path: string) { return this.request<T>(path, { method: 'DELETE' }); }
}

export const api = new ApiClient();
