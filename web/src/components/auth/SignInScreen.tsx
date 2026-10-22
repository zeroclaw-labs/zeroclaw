import { useEffect, useState, type ReactNode } from "react";
import { getAdminPairCode, generatePairCode, PairCodeForbiddenError } from "../../lib/api";
import type { OidcMethod } from "../../lib/authSession";
import { basePath } from "../../lib/basePath";
import { t } from "../../lib/i18n";
import type { SignOutNotice } from "../../hooks/useAuth";

// Pairing dialog: the sign-in screen when no sign-in providers are configured,
// unchanged from before dashboard sign-in existed.
export function PairingDialog({
  onPair,
  children,
}: {
  onPair: (code: string) => Promise<void>;
  /** Rendered between the header and the pairing controls. */
  children?: ReactNode;
}) {
  const [code, setCode] = useState("");
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(false);
  const [displayCode, setDisplayCode] = useState<string | null>(null);
  const [codeLoading, setCodeLoading] = useState(true);
  const [generating, setGenerating] = useState(false);
  // True once minting from the browser is impossible (remote/non-loopback origin,
  // a 403, or pairing disabled) — collapse to the copy-able CLI command instead.
  const [showCliFallback, setShowCliFallback] = useState(false);

  // The admin mint endpoint is localhost-only, so a self-serve "Generate" button
  // can only work when the dashboard itself is served from loopback. Remote /
  // Docker origins must use the CLI on the gateway host instead.
  const isLocalhost = ["localhost", "127.0.0.1", "::1", "[::1]"].includes(
    window.location.hostname,
  );
  // The browser knows the gateway's real host:port (it is talking to it), so it
  // can show the exact recovery command — including the alternate port that made
  // the config-default `get-paircode` miss the running instance (#5266).
  const gatewayPort = window.location.port;
  const cliRecoveryCommand = `zeroclaw gateway get-paircode --new${gatewayPort ? ` --port ${gatewayPort}` : ""}`;

  // Fetch the current pairing code (public endpoint works in Docker too)
  useEffect(() => {
    let cancelled = false;
    getAdminPairCode()
      .then((data) => {
        if (!cancelled && data.pairing_code) {
          setDisplayCode(data.pairing_code);
          setCode(data.pairing_code); // auto-fill so user just clicks "Pair"
        }
      })
      .catch(() => {
        // Endpoint not reachable — user must check terminal / docker logs
      })
      .finally(() => {
        if (!cancelled) setCodeLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, []);

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

  // Mint a fresh code on demand against the localhost-only admin endpoint. This
  // is the in-band escape from the #5266 dead end: an already-paired gateway
  // prints no code at startup, so without this the dashboard's 6-digit prompt
  // has no code to enter and no obvious way forward.
  const handleGenerate = async () => {
    setGenerating(true);
    setError("");
    try {
      const data = await generatePairCode();
      if (data.pairing_code) {
        setDisplayCode(data.pairing_code);
        setCode(data.pairing_code); // auto-fill so the user just clicks "Pair"
      } else {
        // Pairing required but no code minted (e.g. disabled) — point at the CLI.
        setShowCliFallback(true);
      }
    } catch (err: unknown) {
      if (err instanceof PairCodeForbiddenError) {
        // Non-loopback origin: the browser can't mint; show the CLI command.
        setShowCliFallback(true);
      } else {
        setError(
          err instanceof Error ? err.message : "Failed to generate pairing code",
        );
      }
    } finally {
      setGenerating(false);
    }
  };

  return (
    <div
      className="min-h-screen flex items-center justify-center"
      style={{ background: "var(--pc-bg-base)" }}
    >
      {/* Ambient glow */}
      <div className="relative surface-panel p-8 w-full max-w-md animate-fade-in-scale">
        <div className="text-center mb-8">
          <img
            src={`${basePath}/_app/logo.png`}
            alt="ZeroClaw"
            className="h-20 w-20 rounded-2xl object-cover mx-auto mb-4 animate-float"
            onError={(e) => {
              e.currentTarget.style.display = "none";
            }}
          />
          <h1 className="text-2xl font-bold mb-2 text-gradient-blue">
            ZeroClaw
          </h1>
          <p className="text-sm" style={{ color: "var(--pc-text-muted)" }}>
            {codeLoading
              ? "Checking pairing status…"
              : displayCode
                ? "Your pairing code — click Pair to connect"
                : "This gateway is already paired — generate a code to add this device"}
          </p>
        </div>

        {children}

        {/* Already paired, no code minted at startup (#5266): offer a self-serve
            mint on localhost, or the equivalent CLI command everywhere else. */}
        {!codeLoading && !displayCode && (
          <div
            className="mb-6 p-4 rounded-2xl border text-center text-sm"
            style={{
              background: "var(--pc-bg-elevated)",
              borderColor: "var(--pc-border)",
              color: "var(--pc-text-muted)",
            }}
          >
            {isLocalhost && !showCliFallback ? (
              <>
                <p className="mb-3">
                  No pairing code was generated because a device is already
                  paired.
                </p>
                <button
                  type="button"
                  onClick={handleGenerate}
                  disabled={generating}
                  className="btn-electric w-full py-3 text-sm font-semibold tracking-wide"
                >
                  {generating ? (
                    <span className="flex items-center justify-center gap-2">
                      <span className="h-4 w-4 border-2 border-white/30 border-t-white rounded-full animate-spin" />
                      Generating…
                    </span>
                  ) : (
                    "Generate pairing code"
                  )}
                </button>
              </>
            ) : (
              <>
                <p className="mb-2">
                  {isLocalhost
                    ? "Couldn't generate a code from the browser. On the machine running the gateway, run:"
                    : "Pairing codes can only be generated on the machine running the gateway. Run:"}
                </p>
                <code
                  className="block px-3 py-2 rounded-lg font-mono text-xs break-all select-all"
                  style={{
                    background: "var(--pc-bg-code)",
                    color: "var(--pc-text-primary)",
                  }}
                >
                  {cliRecoveryCommand}
                </code>
              </>
            )}
          </div>
        )}

        {/* Show the pairing code if available (localhost) */}
        {!codeLoading && displayCode && (
          <div
            className="mb-6 p-4 rounded-2xl text-center border"
            style={{
              background: "var(--pc-accent-glow)",
              borderColor: "var(--pc-accent-dim)",
            }}
          >
            <div
              className="text-2xl font-mono font-bold tracking-widest break-all py-2"
              style={{ color: "var(--pc-text-primary)" }}
            >
              {displayCode}
            </div>
            <p
              className="text-xs mt-2"
              style={{ color: "var(--pc-text-muted)" }}
            >
              Enter this code below or on another device
            </p>
          </div>
        )}

        <form onSubmit={handleSubmit}>
          {/* Pairing codes are case-sensitive alphanumeric by default
              (#6613). Mobile keyboards auto-capitalise and autocorrect free
              text, which silently corrupts a typed or pasted code, so both
              are turned off here. */}
          <input
            type="text"
            value={code}
            onChange={(e) => setCode(e.target.value)}
            placeholder="pairing code"
            className="input-electric w-full px-4 py-4 text-center text-xl tracking-widest font-medium mb-4"
            maxLength={128}
            autoCapitalize="none"
            autoCorrect="off"
            spellCheck={false}
            autoFocus
          />
          {error && (
            <p
              aria-live="polite"
              className="text-sm mb-4 text-center animate-fade-in"
              style={{ color: "var(--color-status-error)" }}
            >
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

export interface SignInScreenProps {
  onPair: (code: string) => Promise<void>;
  /** Sign-in providers to offer; empty when sign-in is off or in the desktop app. */
  providers: OidcMethod[];
  onSignIn: (alias: string) => void;
  /** Why a signed-in session ended, if one did. */
  notice: SignOutNotice | null;
}

/**
 * The sign-in screen: provider buttons when dashboard sign-in is configured,
 * then the pairing form. With no providers and no notice it is exactly the
 * pairing dialog, so installs without sign-in see no change.
 */
export function SignInScreen({ onPair, providers, onSignIn, notice }: SignInScreenProps) {
  if (providers.length === 0 && !notice) {
    return <PairingDialog onPair={onPair} />;
  }
  return (
    <PairingDialog onPair={onPair}>
      {notice && <SignOutNoticeMessage notice={notice} />}
      {providers.length > 0 && (
        <ProviderButtons providers={providers} onSignIn={onSignIn} />
      )}
    </PairingDialog>
  );
}

function SignOutNoticeMessage({ notice }: { notice: SignOutNotice }) {
  return (
    <p
      role="status"
      className="mb-6 p-3 rounded-2xl border text-sm text-center"
      style={{
        background: "var(--pc-bg-elevated)",
        borderColor: "var(--pc-border)",
        color: "var(--pc-text-muted)",
      }}
    >
      {notice.kind === "signed-out"
        ? t("auth.signin.signed_out_note").replace("{provider}", notice.provider)
        : t("auth.signin.session_ended")}
    </p>
  );
}

function ProviderButtons({
  providers,
  onSignIn,
}: {
  providers: OidcMethod[];
  onSignIn: (alias: string) => void;
}) {
  // Sign-in leaves the page; keep a second click from starting another flow.
  const [starting, setStarting] = useState<string | null>(null);
  return (
    <div className="mb-6" data-signin-providers="">
      <div className="space-y-2">
        {providers.map((provider) => (
          <button
            key={provider.alias}
            type="button"
            data-signin-provider={provider.alias}
            disabled={starting !== null}
            onClick={() => {
              setStarting(provider.alias);
              onSignIn(provider.alias);
            }}
            className="btn-electric w-full py-3 text-sm font-semibold tracking-wide"
          >
            {starting === provider.alias ? (
              <span className="flex items-center justify-center gap-2">
                <span className="h-4 w-4 border-2 border-white/30 border-t-white rounded-full animate-spin" />
                {t("auth.signin.redirecting")}
              </span>
            ) : (
              t("auth.signin.with").replace("{provider}", provider.alias)
            )}
          </button>
        ))}
      </div>
      <div
        className="flex items-center gap-3 mt-6 text-xs"
        style={{ color: "var(--pc-text-muted)" }}
      >
        <span className="h-px flex-1" style={{ background: "var(--pc-border)" }} />
        <span>{t("auth.signin.or_pair")}</span>
        <span className="h-px flex-1" style={{ background: "var(--pc-border)" }} />
      </div>
    </div>
  );
}
