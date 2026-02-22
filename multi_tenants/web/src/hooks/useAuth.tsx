import { createContext, useContext, useState, useEffect, useCallback, type ReactNode } from 'react';
import { api } from '../api/client';
import { getMe } from '../api/auth';

interface AuthState {
  isLoggedIn: boolean;
  isSuperAdmin: boolean;
  email: string | null;
  userId: string | null;
  login: (token: string, email: string) => void;
  logout: () => void;
}

const AuthContext = createContext<AuthState | null>(null);

export function AuthProvider({ children }: { children: ReactNode }) {
  const [isLoggedIn, setIsLoggedIn] = useState(false);
  const [email, setEmail] = useState<string | null>(null);
  const [userId, setUserId] = useState<string | null>(null);
  const [isSuperAdmin, setIsSuperAdmin] = useState(false);

  useEffect(() => {
    const token = localStorage.getItem('jwt');
    if (!token) return;
    try {
      const payload = JSON.parse(atob(token.split('.')[1]));
      if (payload.exp * 1000 > Date.now()) {
        api.setToken(token);
        setIsLoggedIn(true);
        setEmail(payload.email);
        setUserId(payload.sub);
        getMe()
          .then(data => setIsSuperAdmin(data.is_super_admin))
          .catch(() => {});
      } else {
        localStorage.removeItem('jwt');
      }
    } catch {
      localStorage.removeItem('jwt');
    }
  }, []);

  const login = useCallback((token: string, userEmail: string) => {
    api.setToken(token);
    setIsLoggedIn(true);
    setEmail(userEmail);
    try {
      const payload = JSON.parse(atob(token.split('.')[1]));
      setUserId(payload.sub);
    } catch { /* ignore */ }
    getMe()
      .then(data => setIsSuperAdmin(data.is_super_admin))
      .catch(() => {});
  }, []);

  const logout = useCallback(() => {
    api.setToken(null);
    setIsLoggedIn(false);
    setEmail(null);
    setUserId(null);
    setIsSuperAdmin(false);
  }, []);

  return (
    <AuthContext value={{ isLoggedIn, isSuperAdmin, email, userId, login, logout }}>
      {children}
    </AuthContext>
  );
}

export function useAuth() {
  const ctx = useContext(AuthContext);
  if (!ctx) throw new Error('useAuth must be inside AuthProvider');
  return ctx;
}
