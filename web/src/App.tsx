import { Routes, Route, Navigate, useSearchParams } from 'react-router-dom';
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
import Credits from './pages/Credits';
import AuthPage from './pages/AuthPage';
import { AuthProvider, useAuth } from './hooks/useAuth';
import { setLocale, type Locale } from './lib/i18n';
import { authKakaoCallback } from './lib/api';
import { setToken } from './lib/auth';

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

// Kakao OAuth callback handler
function KakaoCallback() {
  const [searchParams] = useSearchParams();
  const [error, setError] = useState<string | null>(null);
  const { refreshAuth } = useAuth();

  useEffect(() => {
    const code = searchParams.get('code');
    if (!code) {
      setError('No authorization code received from Kakao');
      return;
    }

    authKakaoCallback(code)
      .then(() => {
        refreshAuth();
        window.location.href = '/';
      })
      .catch((err) => {
        setError(err instanceof Error ? err.message : 'Kakao login failed');
      });
  }, [searchParams, refreshAuth]);

  if (error) {
    return (
      <div className="min-h-screen bg-gray-950 flex items-center justify-center">
        <div className="bg-gray-900 rounded-xl p-8 max-w-md border border-gray-800 text-center">
          <p className="text-red-400 mb-4">{error}</p>
          <a href="/" className="text-blue-400 hover:text-blue-300">Back to login</a>
        </div>
      </div>
    );
  }

  return (
    <div className="min-h-screen bg-gray-950 flex items-center justify-center">
      <p className="text-gray-400">Processing Kakao login...</p>
    </div>
  );
}

function AppContent() {
  const { isAuthenticated, loading, pair, logout, refreshAuth } = useAuth();
  const [locale, setLocaleState] = useState<Locale>('en');
  const [requiresPairing, setRequiresPairing] = useState(true);

  const setAppLocale = (newLocale: Locale) => {
    setLocaleState(newLocale);
    setLocale(newLocale);
  };

  // Check health to determine if pairing is required
  useEffect(() => {
    fetch('/health')
      .then((r) => r.json())
      .then((data) => {
        if (data && typeof data.require_pairing === 'boolean') {
          setRequiresPairing(data.require_pairing);
        }
      })
      .catch(() => {});
  }, []);

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
      <div className="min-h-screen bg-gray-950 flex items-center justify-center">
        <div className="text-center">
          <img
            src={`${import.meta.env.BASE_URL}MoA_icon_128px.png`}
            alt="MoA"
            className="h-16 w-16 rounded-2xl object-cover mx-auto mb-4 animate-pulse"
          />
          <p className="text-gray-400">Connecting...</p>
        </div>
      </div>
    );
  }

  if (!isAuthenticated) {
    return (
      <Routes>
        <Route
          path="/auth/kakao/callback"
          element={<KakaoCallback />}
        />
        <Route
          path="*"
          element={
            <AuthPage
              onAuthSuccess={(token) => {
                setToken(token);
                refreshAuth();
              }}
              onPair={pair}
              showPairing={requiresPairing}
            />
          }
        />
      </Routes>
    );
  }

  return (
    <LocaleContext.Provider value={{ locale, setAppLocale }}>
      <Routes>
        <Route path="/auth/kakao/callback" element={<KakaoCallback />} />
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
          <Route path="/credits" element={<Credits />} />
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
