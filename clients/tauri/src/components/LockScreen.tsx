import { useState, useCallback, type KeyboardEvent } from "react";
import { t, type Locale } from "../lib/i18n";
import { apiClient } from "../lib/api";

interface LockScreenProps {
  locale: Locale;
  onUnlock: () => void;
  onLogout: () => void;
}

export function LockScreen({ locale, onUnlock, onLogout }: LockScreenProps) {
  const [password, setPassword] = useState("");
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const handleUnlock = useCallback(async () => {
    if (!password) return;

    setIsLoading(true);
    setError(null);

    try {
      await apiClient.verifyPasswordForUnlock(password);
      setPassword("");
      onUnlock();
    } catch (err) {
      if (err instanceof TypeError && err.message === "Failed to fetch") {
        setError(
          locale === "ko"
            ? "\uc11c\ubc84\uc5d0 \uc5f0\uacb0\ud560 \uc218 \uc5c6\uc2b5\ub2c8\ub2e4."
            : "Cannot connect to server.",
        );
      } else {
        setError(t("lock_failed", locale));
      }
      setPassword("");
    } finally {
      setIsLoading(false);
    }
  }, [password, locale, onUnlock]);

  const handleKeyDown = useCallback(
    (e: KeyboardEvent) => {
      if (e.key === "Enter") handleUnlock();
    },
    [handleUnlock],
  );

  const username = apiClient.getUser()?.username || "";

  return (
    <div className="auth-container">
      <div className="auth-card">
        <div className="auth-logo">
          <div className="auth-logo-icon" style={{ fontSize: "2.5rem" }}>
            &#x1f512;
          </div>
          <h1 className="auth-title">{t("lock_title", locale)}</h1>
          <p className="auth-subtitle">{t("lock_subtitle", locale)}</p>
          {username && (
            <p
              style={{
                fontSize: "0.85rem",
                color: "#94a3b8",
                marginTop: "0.5rem",
              }}
            >
              {username}
            </p>
          )}
        </div>

        {error && <div className="auth-error">{error}</div>}

        <div className="auth-field">
          <input
            className="auth-input"
            type="password"
            value={password}
            onChange={(e) => {
              setPassword(e.target.value);
              setError(null);
            }}
            onKeyDown={handleKeyDown}
            placeholder={t("lock_password_placeholder", locale)}
            autoComplete="current-password"
            autoFocus
            disabled={isLoading}
          />
        </div>

        <button
          className="auth-btn auth-btn-primary"
          onClick={handleUnlock}
          disabled={isLoading || !password}
        >
          {isLoading ? t("lock_unlocking", locale) : t("lock_unlock", locale)}
        </button>

        <div className="auth-link" style={{ marginTop: "1rem" }}>
          <button
            className="auth-link-btn"
            onClick={onLogout}
            disabled={isLoading}
          >
            {t("lock_logout", locale)}
          </button>
        </div>
      </div>
    </div>
  );
}
