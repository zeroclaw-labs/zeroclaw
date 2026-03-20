import { Routes, Route, Navigate } from "react-router-dom";
import { useState, useEffect, createContext, useContext } from "react";
import Layout from "./components/layout/Layout";
import Dashboard from "./pages/Dashboard";
import AgentChat from "./pages/AgentChat";
import Tools from "./pages/Tools";
import Cron from "./pages/Cron";
import Integrations from "./pages/Integrations";
import Memory from "./pages/Memory";
import Config from "./pages/Config";
import Cost from "./pages/Cost";
import Logs from "./pages/Logs";
import Doctor from "./pages/Doctor";
import GraphTab from "./pages/GraphTab";
import { AuthProvider, useAuth } from "./hooks/useAuth";
import { DraftContext, useDraftStore } from "./hooks/useDraft";
import { ChatSocketProvider } from "./hooks/useChatSocket";
import { setLocale, type Locale } from "./lib/i18n";

// Locale context
interface LocaleContextType {
  locale: string;
  setAppLocale: (locale: string) => void;
}

export const LocaleContext = createContext<LocaleContextType>({
  locale: "tr",
  setAppLocale: () => {},
});

export const useLocaleContext = () => useContext(LocaleContext);

// Pairing dialog component
function PairingDialog({
  onPair,
}: {
  onPair: (code: string) => Promise<void>;
}) {
  const [code, setCode] = useState("");
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(false);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    setLoading(true);
    setError("");
    try {
      await onPair(code);
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : "Pairing failed");
    } finally {
      setLoading(false);
    }
  };

  return (
    <div
      className="min-h-screen flex items-center justify-center"
      style={{
        background:
          "radial-gradient(ellipse at center, #0a0a20 0%, #050510 70%)",
      }}
    >
      {/* Ambient glow */}
      <div
        className="fixed top-1/2 left-1/2 -translate-x-1/2 -translate-y-1/2 w-[500px] h-[500px] rounded-full opacity-20 pointer-events-none"
        style={{
          background: "radial-gradient(circle, #0080ff 0%, transparent 70%)",
        }}
      />

      <div className="relative glass-card p-8 w-full max-w-md animate-fade-in-scale">
        {/* Top glow accent */}
        <div
          className="absolute -top-px left-1/4 right-1/4 h-px"
          style={{
            background:
              "linear-gradient(90deg, transparent, #0080ff, transparent)",
          }}
        />

        <div className="text-center mb-8">
          <img
            src="/_app/logo.png"
            alt="JhedaiClaw"
            className="h-20 w-20 rounded-2xl object-cover mx-auto mb-4 animate-float"
            style={{ boxShadow: "0 0 30px rgba(0,128,255,0.3)" }}
          />
          <h1 className="text-2xl font-bold text-gradient-blue mb-2">
            JhedaiClaw
          </h1>
          <p className="text-[#556080] text-sm">
            Enter the pairing code from your terminal
          </p>
        </div>
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
          {error && (
            <p className="text-[#ff4466] text-sm mb-4 text-center animate-fade-in">
              {error}
            </p>
          )}
          <button
            type="submit"
            disabled={loading || code.length < 6}
            className="btn-electric w-full py-3.5 text-sm font-semibold tracking-wide"
          >
            {loading ? (
              <span className="flex items-center justify-center gap-2">
                <span className="h-4 w-4 border-2 border-white/30 border-t-white rounded-full animate-spin" />
                Pairing...
              </span>
            ) : (
              "Pair"
            )}
          </button>
        </form>
      </div>
    </div>
  );
}

function AppContent() {
  const { isAuthenticated, requiresPairing, loading, pair, logout } = useAuth();
  const [locale, setLocaleState] = useState("tr");
  const draftStore = useDraftStore();

  const setAppLocale = (newLocale: string) => {
    setLocaleState(newLocale);
    setLocale(newLocale as Locale);
  };

  // Listen for 401 events to force logout
  useEffect(() => {
    const handler = () => {
      logout();
    };
    window.addEventListener("jhedaiclaw-unauthorized", handler);
    return () => window.removeEventListener("jhedaiclaw-unauthorized", handler);
  }, [logout]);

  if (loading) {
    return (
      <div
        className="min-h-screen flex items-center justify-center"
        style={{
          background:
            "radial-gradient(ellipse at center, #0a0a20 0%, #050510 70%)",
        }}
      >
        <div className="flex flex-col items-center gap-4 animate-fade-in">
          <div className="h-10 w-10 border-2 border-[#0080ff30] border-t-[#0080ff] rounded-full animate-spin" />
          <p className="text-[#556080] text-sm">Connecting...</p>
        </div>
      </div>
    );
  }

  if (!isAuthenticated && requiresPairing) {
    return <PairingDialog onPair={pair} />;
  }

  return (
    <DraftContext.Provider value={draftStore}>
      <LocaleContext.Provider value={{ locale, setAppLocale }}>
        <ChatSocketProvider>
          <Routes>
            <Route element={<Layout />}>
              <Route path="/" element={<Dashboard />} />
              <Route path="/agent" element={<AgentChat />} />
              <Route path="/tools" element={<Tools />} />
              <Route path="/cron" element={<Cron />} />
              <Route path="/integrations" element={<Integrations />} />
              <Route path="/memory" element={<Memory />} />
              <Route path="/graph" element={<GraphTab active={true} />} />
              <Route path="/config" element={<Config />} />
              <Route path="/cost" element={<Cost />} />
              <Route path="/logs" element={<Logs />} />
              <Route path="/doctor" element={<Doctor />} />
              <Route path="*" element={<Navigate to="/" replace />} />
            </Route>
          </Routes>
        </ChatSocketProvider>
      </LocaleContext.Provider>
    </DraftContext.Provider>
  );
}

export default function App() {
  return (
    <AuthProvider>
      <AppContent />
    </AuthProvider>
  );
}
