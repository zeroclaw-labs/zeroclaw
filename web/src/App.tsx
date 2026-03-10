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
import { AuthProvider, useAuth } from './hooks/useAuth';
import {
  applyLocaleToDocument,
  coerceLocale,
  getLocaleDirection,
  setLocale,
  tLocale,
  type Locale,
} from './lib/i18n';
import LanguageSelector from './components/controls/LanguageSelector';

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
function PairingDialog({
  locale,
  setAppLocale,
  onPair,
}: {
  locale: Locale;
  setAppLocale: (locale: Locale) => void;
  onPair: (code: string) => Promise<void>;
}) {
  const [code, setCode] = useState('');
  const [error, setError] = useState('');
  const [loading, setLoading] = useState(false);

  const translate = (key: string) => tLocale(key, locale);
  const localeDirection = getLocaleDirection(locale);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    setLoading(true);
    setError('');
    try {
      await onPair(code);
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : translate('auth.pairing_failed'));
    } finally {
      setLoading(false);
    }
  };

  return (
    <div className="pairing-shell min-h-screen flex items-center justify-center px-4" dir={localeDirection}>
      <div className="pairing-card w-full max-w-md rounded-2xl p-8">
        <div className="mb-4 flex justify-end">
          <LanguageSelector
            locale={locale}
            onChange={setAppLocale}
            ariaLabel={translate('common.select_language')}
            title={translate('common.languages')}
            align="right"
            buttonClassName="inline-flex min-w-[12rem] items-center gap-2 rounded-xl border border-[#2b4f97] bg-[#091937]/75 px-3 py-2 text-sm text-[#c4d8ff] shadow-[0_0_0_1px_rgba(79,131,255,0.08)] transition hover:border-[#4f83ff] hover:text-white"
          />
        </div>
        <div className="text-center mb-6">
          <h1 className="mb-2 text-2xl font-semibold tracking-[0.16em] pairing-brand">ZEROCLAW</h1>
          <p className="text-sm text-[#9bb8e8]">{translate('auth.enter_code')}</p>
        </div>
        <form onSubmit={handleSubmit}>
          <input
            type="text"
            aria-label={translate('auth.enter_code')}
            aria-invalid={Boolean(error)}
            aria-describedby={error ? 'pairing-error' : undefined}
            value={code}
            onChange={(e) => setCode(e.target.value)}
            placeholder={translate('auth.code_placeholder')}
            className="w-full rounded-xl border border-[#29509c] bg-[#071228]/90 px-4 py-3 text-center text-2xl tracking-[0.35em] text-white focus:border-[#4f83ff] focus:outline-none mb-4"
            maxLength={6}
            autoFocus
          />
          {error && (
            <p id="pairing-error" role="alert" className="mb-4 text-center text-sm text-rose-300">{error}</p>
          )}
          <button
            type="submit"
            disabled={loading || code.length < 6}
            data-testid="pair-button"
            className="electric-button w-full rounded-xl py-3 font-medium text-white disabled:opacity-50"
          >
            {loading ? translate('auth.pairing_progress') : translate('auth.pair_button')}
          </button>
        </form>
      </div>
    </div>
  );
}

function AppContent() {
  const { isAuthenticated, loading, pair, logout } = useAuth();
  const [locale, setLocaleState] = useState<Locale>(() => {
    const initialLocale = (() => {
      if (typeof window === 'undefined') {
        return 'en';
      }

      const saved = window.localStorage.getItem(LOCALE_STORAGE_KEY);
      if (saved) {
        return coerceLocale(saved);
      }

      return coerceLocale(window.navigator.language);
    })();

    setLocale(initialLocale);
    if (typeof document !== 'undefined') {
      applyLocaleToDocument(initialLocale, document);
    }
    return initialLocale;
  });

  useEffect(() => {
    setLocale(locale);
    if (typeof window !== 'undefined') {
      window.localStorage.setItem(LOCALE_STORAGE_KEY, locale);
      applyLocaleToDocument(locale, document);
    }
  }, [locale]);

  const setAppLocale = (newLocale: Locale) => {
    setLocale(newLocale);
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
          <p className="text-[#a7c4f3]">{tLocale('common.connecting', locale)}</p>
        </div>
      </div>
    );
  }

  if (!isAuthenticated) {
    return <PairingDialog locale={locale} setAppLocale={setAppLocale} onPair={pair} />;
  }

  return (
    <LocaleContext.Provider value={{ locale, setAppLocale }}>
      <Routes key={locale}>
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
  );
}

export default function App() {
  return (
    <AuthProvider>
      <AppContent />
    </AuthProvider>
  );
}
