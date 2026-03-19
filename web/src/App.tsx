import { Routes, Route, Navigate } from 'react-router-dom';
import { useState, useEffect, createContext, useContext, Component, type ReactNode, type ErrorInfo } from 'react';
import { ThemeProvider } from './contexts/ThemeContext';
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
import Pairing from './pages/Pairing';
import { AuthProvider, useAuth } from './hooks/useAuth';
import { DraftContext, useDraftStore } from './hooks/useDraft';
import { setLocale, type Locale } from './lib/i18n';
import { getAdminPairCode } from './lib/api';

interface LocaleContextType { locale: string; setAppLocale: (locale: string) => void; }
export const LocaleContext = createContext<LocaleContextType>({ locale: 'en', setAppLocale: () => {} });
export const useLocaleContext = () => useContext(LocaleContext);

interface ErrorBoundaryState { error: Error | null; }

export class ErrorBoundary extends Component<{ children: ReactNode }, ErrorBoundaryState> {
  constructor(props: { children: ReactNode }) {
    super(props);
    this.state = { error: null };
  }

  static getDerivedStateFromError(error: Error): ErrorBoundaryState {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error('[ZeroClaw] Render error:', error, info.componentStack);
  }

  render() {
    if (this.state.error) {
      return (
        <div className="p-6">
          <div className="card p-6 w-full max-w-lg" style={{ borderColor: 'rgba(239, 68, 68, 0.3)' }}>
            <h2 className="text-lg font-semibold mb-2" style={{ color: 'var(--color-status-error)' }}>Something went wrong</h2>
            <p className="text-sm mb-4" style={{ color: 'var(--pc-text-muted)' }}>A render error occurred. Check the browser console for details.</p>
            <pre className="text-xs rounded-lg p-3 overflow-x-auto whitespace-pre-wrap break-all font-mono" style={{ background: 'var(--pc-bg-base)', color: 'var(--color-status-error)' }}>
              {this.state.error.message}
            </pre>
            <button onClick={() => this.setState({ error: null })} className="btn-electric mt-6 px-4 py-2 text-sm font-medium">
              Try again
            </button>
          </div>
        </div>
      );
    }
    return this.props.children;
  }
}

function PairingDialog({ onPair }: { onPair: (code: string) => Promise<void> }) {
  const [code, setCode] = useState('');
  const [error, setError] = useState('');
  const [loading, setLoading] = useState(false);
  const [displayCode, setDisplayCode] = useState<string | null>(null);
  const [codeLoading, setCodeLoading] = useState(true);

  useEffect(() => {
    let cancelled = false;
    getAdminPairCode()
      .then((data) => { if (!cancelled && data.pairing_code) setDisplayCode(data.pairing_code); })
      .catch(() => {})
      .finally(() => { if (!cancelled) setCodeLoading(false); });
    return () => { cancelled = true; };
  }, []);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    setLoading(true);
    setError('');
    try { await onPair(code); } catch (err: unknown) { setError(err instanceof Error ? err.message : 'Pairing failed'); }
    finally { setLoading(false); }
  };

  return (
    <div className="min-h-screen flex items-center justify-center" style={{ background: 'var(--pc-bg-base)' }}>
      <div className="relative surface-panel p-8 w-full max-w-md animate-fade-in-scale">
        <div className="text-center mb-8">
          <img
            src="/_app/logo.png"
            alt="ZeroClaw"
            className="h-20 w-20 rounded-2xl object-cover mx-auto mb-4 animate-float"
            onError={(e) => { e.currentTarget.style.display = 'none'; }}
          />
          <h1 className="text-2xl font-bold mb-2 text-gradient-blue">ZeroClaw</h1>
          <p className="text-sm" style={{ color: 'var(--pc-text-muted)' }}>
            {displayCode ? 'Your pairing code' : 'Enter the pairing code from your terminal'}
          </p>
        </div>

        {!codeLoading && displayCode && (
          <div className="mb-6 p-4 rounded-2xl text-center border" style={{ background: 'var(--pc-accent-glow)', borderColor: 'var(--pc-accent-dim)' }}>
            <div className="text-4xl font-mono font-bold tracking-[0.4em] py-2" style={{ color: 'var(--pc-text-primary)' }}>
              {displayCode}
            </div>
            <p className="text-xs mt-2" style={{ color: 'var(--pc-text-muted)' }}>Enter this code below or on another device</p>
          </div>
        )}

        <form onSubmit={handleSubmit}>
          <input
            type="text"
            value={code}
            onChange={(e) => setCode(e.target.value)}
            placeholder="6-digit code"
            className="input-electric w-full px-4 py-4 text-center text-2xl tracking-[0.3em] font-medium mb-4"
            maxLength={6}
            autoFocus
          />
          {error && <p className="text-sm mb-4 text-center animate-fade-in" style={{ color: 'var(--color-status-error)' }}>{error}</p>}
          <button type="submit" disabled={loading || code.length < 6} className="btn-electric w-full py-3.5 text-sm font-semibold tracking-wide">
            {loading ? <span className="flex items-center justify-center gap-2"><span className="h-4 w-4 border-2 border-white/30 border-t-white rounded-full animate-spin" />Pairing...</span> : 'Pair'}
          </button>
        </form>
      </div>
    </div>
  );
}

function AppContent() {
  const { isAuthenticated, requiresPairing, loading, pair, logout } = useAuth();
  const [locale, setLocaleState] = useState('en');
  const draftStore = useDraftStore();
  const setAppLocale = (newLocale: string) => { setLocaleState(newLocale); setLocale(newLocale as Locale); };

  useEffect(() => {
    const handler = () => { logout(); };
    window.addEventListener('zeroclaw-unauthorized', handler);
    return () => window.removeEventListener('zeroclaw-unauthorized', handler);
  }, [logout]);

  if (loading) return (
    <div className="min-h-screen flex items-center justify-center" style={{ background: 'var(--pc-bg-base)' }}>
      <div className="flex flex-col items-center gap-4 animate-fade-in">
        <div className="h-10 w-10 border-2 rounded-full animate-spin" style={{ borderColor: 'var(--pc-border)', borderTopColor: 'var(--pc-accent)' }} />
        <p className="text-sm" style={{ color: 'var(--pc-text-muted)' }}>Connecting...</p>
      </div>
    </div>
  );

  if (!isAuthenticated && requiresPairing) return <PairingDialog onPair={pair} />;

  return (
    <DraftContext.Provider value={draftStore}>
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
            <Route path="/pairing" element={<Pairing />} />
            <Route path="*" element={<Navigate to="/" replace />} />
          </Route>
        </Routes>
      </LocaleContext.Provider>
    </DraftContext.Provider>
  );
}

export default function App() {
  return (
    <AuthProvider>
      <ThemeProvider>
        <AppContent />
      </ThemeProvider>
    </AuthProvider>
  );
}
