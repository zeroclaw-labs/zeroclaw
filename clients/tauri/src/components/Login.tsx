import { useState, useCallback, type KeyboardEvent } from "react";
import { t, type Locale } from "../lib/i18n";
import { apiClient } from "../lib/api";

interface LoginProps {
  locale: Locale;
  onLoginSuccess: (devices: Array<{ device_id: string; device_name: string; platform: string | null; last_seen: number; is_online: boolean; has_pairing_code: boolean }>) => void;
  onGoToSignUp: () => void;
  onGoToSettings: () => void;
}

export function Login({ locale, onLoginSuccess, onGoToSignUp, onGoToSettings }: LoginProps) {
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const handleLogin = useCallback(async () => {
    if (!username.trim() || !password) return;

    setIsLoading(true);
    setError(null);

    try {
      const result = await apiClient.login(username.trim(), password);
      onLoginSuccess(result.devices || []);
    } catch (err) {
      if (err instanceof TypeError && err.message === "Failed to fetch") {
        setError(locale === "ko" ? "\uC11C\uBC84\uC5D0 \uC5F0\uACB0\uD560 \uC218 \uC5C6\uC2B5\uB2C8\uB2E4. \uC11C\uBC84 \uC8FC\uC18C\uB97C \uD655\uC778\uD574\uC8FC\uC138\uC694." : "Cannot connect to server. Please check the server URL.");
      } else if (err instanceof Error && (err.message.includes("401") || err.message.toLowerCase().includes("invalid") || err.message.toLowerCase().includes("unauthorized"))) {
        setError(locale === "ko" ? "\uD68C\uC6D0\uC815\uBCF4\uB97C \uCC3E\uC744 \uC218 \uC5C6\uC2B5\uB2C8\uB2E4." : "Account not found. Please check your credentials.");
      } else {
        setError(err instanceof Error ? err.message : t("login_failed", locale));
      }
    } finally {
      setIsLoading(false);
    }
  }, [username, password, locale, onLoginSuccess]);

  const handleKeyDown = useCallback(
    (e: KeyboardEvent) => {
      if (e.key === "Enter") handleLogin();
    },
    [handleLogin],
  );

  return (
    <div className="auth-container">
      <div className="auth-card">
        <div className="auth-logo">
          <div className="auth-logo-icon">M</div>
          <h1 className="auth-title">{t("login_title", locale)}</h1>
          <p className="auth-subtitle">{t("login_subtitle", locale)}</p>
        </div>

        {error && <div className="auth-error">{error}</div>}

        <div className="auth-field">
          <label className="auth-label">{t("username", locale)}</label>
          <input
            className="auth-input"
            type="email"
            value={username}
            onChange={(e) => setUsername(e.target.value)}
            onKeyDown={handleKeyDown}
            placeholder={locale === "ko" ? "example@email.com" : "example@email.com"}
            autoComplete="email"
            autoFocus
            disabled={isLoading}
          />
        </div>

        <div className="auth-field">
          <label className="auth-label">{t("password", locale)}</label>
          <input
            className="auth-input"
            type="password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            onKeyDown={handleKeyDown}
            placeholder={t("password", locale)}
            autoComplete="current-password"
            disabled={isLoading}
          />
        </div>

        <button
          className="auth-btn auth-btn-primary"
          onClick={handleLogin}
          disabled={isLoading || !username.trim() || !password}
        >
          {isLoading ? t("logging_in", locale) : t("login_button", locale)}
        </button>

        <div className="auth-link">
          {t("no_account", locale)}{" "}
          <button className="auth-link-btn" onClick={onGoToSignUp} disabled={isLoading}>
            {t("signup", locale)}
          </button>
        </div>

        <div className="auth-settings-link">
          <button className="auth-link-btn" onClick={onGoToSettings} disabled={isLoading}>
            {t("advanced_settings", locale)}
          </button>
        </div>
      </div>
    </div>
  );
}
