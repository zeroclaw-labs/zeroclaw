import { Routes, Route, Navigate } from 'react-router-dom';
import { useState, useEffect, createContext, useContext } from 'react';
import Layout from './components/layout/Layout';
import Dashboard from './pages/Dashboard';
import AgentChat from './pages/AgentChat';
import Tools from './pages/Tools';
import Cron from './pages/Cron';
import Integrations from './pages/Integrations';
import Memory from './pages/Memory';
import Devices from './pages/Devices';
import Config from './pages/Config';
import Cost from './pages/Cost';
import Logs from './pages/Logs';
import Doctor from './pages/Doctor';
import { AuthProvider, useAuth } from './hooks/useAuth';
import { coerceLocale, setLocale, type Locale } from './lib/i18n';

const LOCALE_STORAGE_KEY = 'zeroclaw:locale';

// Locale context
interface LocaleContextType {
  locale: Locale;
  setAppLocale: (locale: Locale) => void;
}

export const LocaleContext = createContext<LocaleContextType>({
  locale: 'en',
  setAppLocale: (_locale: Locale) => {},
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
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Pairing failed');
    } finally {
      setLoading(false);
    }
  };

  return (
    <div className="pairing-shell min-h-screen flex items-center justify-center px-4">
      <div className="pairing-card w-full max-w-md rounded-2xl p-8">
        <div className="text-center mb-6">
          <h1 className="mb-2 text-2xl font-semibold tracking-[0.16em] pairing-brand">ZEROCLAW</h1>
          <p className="text-sm text-[#9bb8e8]">Enter the one-time pairing code from your terminal</p>
        </div>
        <form onSubmit={handleSubmit}>
          <input
            type="text"
            value={code}
            onChange={(e) => setCode(e.target.value)}
            placeholder="6-digit code"
            className="w-full rounded-xl border border-[#29509c] bg-[#071228]/90 px-4 py-3 text-center text-2xl tracking-[0.35em] text-white focus:border-[#4f83ff] focus:outline-none mb-4"
            maxLength={6}
            autoFocus
          />
          {error && (
            <p className="mb-4 text-center text-sm text-rose-300">{error}</p>
          )}
          <button
            type="submit"
            disabled={loading || code.length < 6}
            className="electric-button w-full rounded-xl py-3 font-medium text-white disabled:opacity-50"
          >
            {loading ? 'Pairing...' : 'Pair'}
          </button>
        </form>
      </div>
    </div>
  );
}

function AppContent() {
  const { isAuthenticated, loading, pair, logout } = useAuth();
  const [locale, setLocaleState] = useState<Locale>(() => {
    if (typeof window === 'undefined') {
      return 'en';
    }

    const saved = window.localStorage.getItem(LOCALE_STORAGE_KEY);
    if (saved) {
      return coerceLocale(saved);
    }

    return coerceLocale(window.navigator.language);
  });

  useEffect(() => {
    setLocale(locale);
    if (typeof window !== 'undefined') {
      window.localStorage.setItem(LOCALE_STORAGE_KEY, locale);
    }
  }, [locale]);

  const setAppLocale = (newLocale: Locale) => {
    setLocaleState(newLocale);
  };

  // Listen for 401 events to force logout
  useEffect(() => {
    const handler = () => {
      logout();
    };
    window.addEventListener('zeroclaw-unauthorized', handler);
    return () => window.removeEventListener('zeroclaw-unauthorized', handler);
  }, [logout]);

  if (loading) {
    return (
      <div className="pairing-shell min-h-screen flex items-center justify-center">
        <div className="flex flex-col items-center gap-3">
          <div className="electric-loader h-10 w-10 rounded-full" />
          <p className="text-[#a7c4f3]">Connecting...</p>
        </div>
      </div>
    );
  }

  if (!isAuthenticated) {
    return <PairingDialog onPair={pair} />;
  }

  return (
    <LocaleContext.Provider value={{ locale, setAppLocale }}>
      <Routes>
        <Route element={<Layout />}>
          <Route path="/" element={<Dashboard />} />
          <Route path="/agent" element={<AgentChat />} />
          <Route path="/tools" element={<Tools />} />
          <Route path="/cron" element={<Cron />} />
          <Route path="/integrations" element={<Integrations />} />
          <Route path="/memory" element={<Memory />} />
          <Route path="/devices" element={<Devices />} />
          <Route path="/config" element={<Config />} />
          <Route path="/cost" element={<Cost />} />
          <Route path="/logs" element={<Logs />} />
          <Route path="/doctor" element={<Doctor />} />
          <Route path="*" element={<Navigate to="/" replace />} />
        </Route>
      </Routes>
    </LocaleContext.Provider>
  );
}

export default function App() {
  return (
    <AuthProvider>
      <AppContent />
    </AuthProvider>
  );
}
