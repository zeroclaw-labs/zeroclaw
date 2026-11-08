import {
  Component,
  createContext,
  useContext,
  useEffect,
  useState,
  type ErrorInfo,
  type ReactNode,
} from "react";
import { useLocation, useNavigate } from "react-router-dom";
import { ThemeProvider } from "./contexts/ThemeContext";

import { loadLocale, saveLocale } from "./contexts/ThemeContext";
import { ReauthPrompt } from "./components/auth/ReauthPrompt";
import { SignInScreen } from "./components/auth/SignInScreen";
import { AuthProvider, useAuth } from "./hooks/useAuth";
import { DraftContext, useDraftStore } from "./hooks/useDraft";
import { getQuickstartState } from "./lib/api";
import { ConfigDraftProvider } from "./lib/draftStore";
import { setLocale, type Locale } from "./lib/i18n";
import { Router } from "./router/router";

// Locale context
interface LocaleContextType {
  locale: string;
  setAppLocale: (locale: string) => void;
}

export const LocaleContext = createContext<LocaleContextType>({
  locale: "en",
  setAppLocale: () => {},
});

export const useLocaleContext = () => useContext(LocaleContext);

// ---------------------------------------------------------------------------
// Error boundary — catches render crashes and shows a recoverable message
// instead of a black screen
// ---------------------------------------------------------------------------

interface ErrorBoundaryState {
  error: Error | null;
}

export class ErrorBoundary extends Component<
  { children: ReactNode },
  ErrorBoundaryState
> {
  constructor(props: { children: ReactNode }) {
    super(props);
    this.state = { error: null };
  }

  static getDerivedStateFromError(error: Error): ErrorBoundaryState {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error("[ZeroClaw] Render error:", error, info.componentStack);
    // Stale-chunk recovery: when Vite rebuilds, the loaded index.html
    // still references the previous chunk hashes. A dynamic import for
    // a lazy route then 404s with "error loading dynamically imported
    // module". Reload once so the user gets the new index.html and the
    // current chunk hashes; the sessionStorage marker prevents reload
    // loops if reload doesn't actually help.
    if (
      isChunkLoadError(error) &&
      !sessionStorage.getItem("zeroclaw-chunk-reloaded")
    ) {
      sessionStorage.setItem("zeroclaw-chunk-reloaded", "1");
      window.location.reload();
    }
  }

  render() {
    if (this.state.error) {
      return (
        <div className="p-6">
          <div
            className="card p-6 w-full max-w-lg"
            style={{ borderColor: "var(--color-status-error-alpha-30)" }}
          >
            <h2
              className="text-lg font-semibold mb-2"
              style={{ color: "var(--color-status-error)" }}
            >
              Something went wrong
            </h2>
            <p
              className="text-sm mb-4"
              style={{ color: "var(--pc-text-muted)" }}
            >
              A render error occurred. Check the browser console for details.
            </p>
            <pre
              className="text-xs rounded-lg p-3 overflow-x-auto whitespace-pre-wrap break-all font-mono"
              style={{
                background: "var(--pc-bg-base)",
                color: "var(--color-status-error)",
              }}
            >
              {this.state.error.message}
            </pre>
            <button
              onClick={() => {
                sessionStorage.removeItem("zeroclaw-chunk-reloaded");
                this.setState({ error: null });
              }}
              className="btn-electric mt-6 px-4 py-2 text-sm font-medium"
            >
              Try again
            </button>
          </div>
        </div>
      );
    }
    return this.props.children;
  }
}

function isChunkLoadError(error: Error): boolean {
  const m = error?.message ?? "";
  return (
    m.includes("dynamically imported module") ||
    m.includes("Failed to fetch dynamically") ||
    m.includes("Importing a module script failed") ||
    error?.name === "ChunkLoadError"
  );
}

function AppContent() {
  const {
    isAuthenticated,
    requiresPairing,
    loading,
    pair,
    providers,
    signIn,
    signOutNotice,
  } = useAuth();
  const [locale, setLocaleState] = useState(loadLocale());
  const draftStore = useDraftStore();
  setLocale(locale as Locale);

  const setAppLocale = (newLocale: string) => {
    setLocaleState(newLocale);
    setLocale(newLocale as Locale);
    saveLocale(newLocale);
  };

  if (loading) {
    return (
      <div
        className="min-h-screen flex items-center justify-center"
        style={{ background: "var(--pc-bg-base)" }}
      >
        <div className="flex flex-col items-center gap-4 animate-fade-in">
          <div
            className="h-10 w-10 border-2 rounded-full animate-spin"
            style={{
              borderColor: "var(--pc-border)",
              borderTopColor: "var(--pc-accent)",
            }}
          />
          <p className="text-sm" style={{ color: "var(--pc-text-muted)" }}>
            Connecting...
          </p>
        </div>
      </div>
    );
  }

  // 401s are routed by AuthProvider: pairing signs out, an expired sign-in
  // keeps the page under the re-auth prompt.
  if (!isAuthenticated && requiresPairing) {
    return (
      <SignInScreen
        onPair={pair}
        providers={providers}
        onSignIn={signIn}
        notice={signOutNotice}
      />
    );
  }

  return (
    <DraftContext.Provider value={draftStore}>
      <ConfigDraftProvider>
        <LocaleContext.Provider value={{ locale, setAppLocale }}>
          <FreshInstallRedirect />
          <ReauthPrompt />
          <Router />
        </LocaleContext.Provider>
      </ConfigDraftProvider>
    </DraftContext.Provider>
  );
}

// Redirects fresh installs (no agents yet, Quickstart never completed)
// from `/` to `/quickstart`. The daemon always writes a default
// config.toml on init, so file existence isn't the right signal —
// we ask the gateway via /api/quickstart/state which reports
// quickstart_completed plus the live agents list.
//
// Fires once per session. Only redirects when the user lands at `/` —
// manual navigation to other routes is left alone, so returning users
// who already have agents can always reach Quickstart from the nav.
function FreshInstallRedirect() {
  const navigate = useNavigate();
  const location = useLocation();
  const [checked, setChecked] = useState(false);

  useEffect(() => {
    if (checked) return;
    setChecked(true);
    if (location.pathname !== "/") return;
    void getQuickstartState()
      .then((state) => {
        if (!state.quickstart_completed && state.agents.length === 0) {
          navigate("/quickstart", { replace: true });
        }
      })
      .catch(() => {
        // Status check failed (network blip, gateway hiccup); the
        // dashboard renders normally as the safe default.
      });
  }, [checked, location.pathname, navigate]);

  return null;
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
