import { Routes, Route, Navigate } from 'react-router-dom';
import { useState, useEffect, createContext, useContext } from 'react';
import Layout from './components/layout/Layout';
import Dashboard from './pages/Dashboard';
import AgentChat from './pages/AgentChat';
import Tools from './pages/Tools';
import Cron from './pages/Cron';
import Integrations from './pages/Integrations';
import Memory from './pages/Memory';
import Config from './pages/Config';
import Cost from './pages/Cost';
import Logs from './pages/Logs';
import Doctor from './pages/Doctor';
import { getToken, clearToken } from './lib/auth';
import { pair } from './lib/api';
import { setLocale, type Locale } from './lib/i18n';

// Auth context
interface AuthContextType {
  token: string | null;
  isAuthenticated: boolean;
  login: (code: string) => Promise<void>;
  logout: () => void;
}

export const AuthContext = createContext<AuthContextType>({
  token: null,
  isAuthenticated: false,
  login: async () => {},
  logout: () => {},
});

export const useAuth = () => useContext(AuthContext);

// Locale context
interface LocaleContextType {
  locale: string;
  setAppLocale: (locale: string) => void;
}

export const LocaleContext = createContext<LocaleContextType>({
  locale: 'tr',
  setAppLocale: () => {},
});

export const useLocaleContext = () => useContext(LocaleContext);

// Pairing dialog component
function PairingDialog({ onPair }: { onPair: (code: string) => Promise<void> }) {
  const [code, setCode] = useState('');
  const [error, setError] = useState('');
  const [loading, setLoading] = useState(false);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    setLoading(true);
    setError('');
    try {
      await onPair(code);
    } catch (err: any) {
      setError(err.message || 'Pairing failed');
    } finally {
      setLoading(false);
    }
  };

  return (
    <div className="min-h-screen bg-gray-950 flex items-center justify-center">
      <div className="bg-gray-900 rounded-xl p-8 w-full max-w-md border border-gray-800">
        <div className="text-center mb-6">
          <h1 className="text-2xl font-bold text-white mb-2">ZeroClaw</h1>
          <p className="text-gray-400">Enter the pairing code from your terminal</p>
        </div>
        <form onSubmit={handleSubmit}>
          <input
            type="text"
            value={code}
            onChange={(e) => setCode(e.target.value)}
            placeholder="6-digit code"
            className="w-full px-4 py-3 bg-gray-800 border border-gray-700 rounded-lg text-white text-center text-2xl tracking-widest focus:outline-none focus:border-blue-500 mb-4"
            maxLength={6}
            autoFocus
          />
          {error && (
            <p className="text-red-400 text-sm mb-4 text-center">{error}</p>
          )}
          <button
            type="submit"
            disabled={loading || code.length < 6}
            className="w-full py-3 bg-blue-600 hover:bg-blue-700 disabled:bg-gray-700 disabled:text-gray-500 text-white rounded-lg font-medium transition-colors"
          >
            {loading ? 'Pairing...' : 'Pair'}
          </button>
        </form>
      </div>
    </div>
  );
}

export default function App() {
  const [token, setTokenState] = useState<string | null>(getToken());
  const [locale, setLocaleState] = useState('tr');

  const isAuthenticated = !!token;

  const login = async (code: string) => {
    const result = await pair(code);
    // pair() already stores the token in localStorage
    setTokenState(result.token);
  };

  const logout = () => {
    clearToken();
    setTokenState(null);
  };

  const setAppLocale = (newLocale: string) => {
    setLocaleState(newLocale);
    setLocale(newLocale as Locale);
  };

  // Listen for 401 events to force logout
  useEffect(() => {
    const handler = () => {
      clearToken();
      setTokenState(null);
    };
    window.addEventListener('zeroclaw-unauthorized', handler);
    return () => window.removeEventListener('zeroclaw-unauthorized', handler);
  }, []);

  if (!isAuthenticated) {
    return <PairingDialog onPair={login} />;
  }

  return (
    <AuthContext.Provider value={{ token, isAuthenticated, login, logout }}>
      <LocaleContext.Provider value={{ locale, setAppLocale }}>
        <Routes>
          <Route element={<Layout />}>
            <Route path="/" element={<Dashboard />} />
            <Route path="/agent" element={<AgentChat />} />
            <Route path="/tools" element={<Tools />} />
            <Route path="/cron" element={<Cron />} />
            <Route path="/integrations" element={<Integrations />} />
            <Route path="/memory" element={<Memory />} />
            <Route path="/config" element={<Config />} />
            <Route path="/cost" element={<Cost />} />
            <Route path="/logs" element={<Logs />} />
            <Route path="/doctor" element={<Doctor />} />
            <Route path="*" element={<Navigate to="/" replace />} />
          </Route>
        </Routes>
      </LocaleContext.Provider>
    </AuthContext.Provider>
  );
}
