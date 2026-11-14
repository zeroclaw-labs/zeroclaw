import { t } from "../../lib/i18n";

/**
 * Says that this dashboard is reached through a relay that ends the
 * browser's TLS, so the relay's operator can read and change the session.
 * Shown on the sign-in screen and above the dashboard while that is so.
 */
export function RelayNotice({ bar = false }: { bar?: boolean }) {
  return (
    <p
      role="note"
      data-relay-notice=""
      className={bar ? "px-4 py-2 text-xs text-center border-b" : "mb-6 p-3 rounded-2xl border text-sm text-center"}
      style={{
        background: "var(--pc-bg-elevated)",
        borderColor: "var(--pc-border)",
        color: "var(--pc-text-muted)",
      }}
    >
      {t("auth.relay.notice")}
    </p>
  );
}
