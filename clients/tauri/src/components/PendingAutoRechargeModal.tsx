import { useCallback, useEffect, useState } from "react";
import { t, type Locale } from "../lib/i18n";
import { apiClient } from "../lib/api";

/**
 * Full-screen modal shown when the gateway has queued a pending
 * auto-recharge that needs the user's explicit approval.
 *
 * Behaviour:
 *   - Polls `/api/billing/pending-auto-recharge` every 30s in the
 *     background (cheap query, indexed by `user_id + unresolved`).
 *   - Renders the modal only when the endpoint returns a row.
 *   - Three buttons: Approve / Defer / Cancel. Each POSTs back to
 *     `/resolve`. On Approve, the backend runs the charge via the
 *     stored Toss billing key or Stripe saved-method.
 *   - 10-minute on-screen countdown mirroring
 *     `PENDING_AUTO_RECHARGE_TIMEOUT_SECS`. If the timer expires the
 *     backend will auto-timeout the row on its next sweep; we close
 *     the modal preemptively so the user isn't blocked.
 */
interface PendingRow {
  pending_id: string;
  user_id: string;
  package_id: string;
  balance_at: number;
  threshold_at: number;
  created_at: number;
}

interface Props {
  locale: Locale;
}

const POLL_MS = 30_000;
const TIMEOUT_SECS = 10 * 60;

export function PendingAutoRechargeModal({ locale }: Props) {
  const [pending, setPending] = useState<PendingRow | null>(null);
  const [secondsLeft, setSecondsLeft] = useState(TIMEOUT_SECS);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);

  const authHeaders = useCallback((): Record<string, string> => {
    const token = apiClient.getToken();
    const h: Record<string, string> = { "Content-Type": "application/json" };
    if (token) h["Authorization"] = `Bearer ${token}`;
    return h;
  }, []);

  // Poll loop.
  useEffect(() => {
    let cancelled = false;
    const poll = async () => {
      try {
        const res = await fetch(
          `${apiClient.getServerUrl()}/api/billing/pending-auto-recharge`,
          { headers: authHeaders() },
        );
        if (!res.ok) return;
        const data = await res.json();
        if (cancelled) return;
        const row = (data?.pending ?? null) as PendingRow | null;
        if (row) {
          const elapsed = Math.floor(Date.now() / 1000) - row.created_at;
          setSecondsLeft(Math.max(0, TIMEOUT_SECS - elapsed));
        }
        setPending(row);
      } catch {
        /* keep old state; retry on next tick */
      }
    };
    void poll();
    const timer = setInterval(poll, POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [authHeaders]);

  // Countdown (runs only while a pending row is visible).
  useEffect(() => {
    if (!pending) return;
    const tick = setInterval(() => {
      setSecondsLeft((s) => (s <= 1 ? 0 : s - 1));
    }, 1000);
    return () => clearInterval(tick);
  }, [pending]);

  // Close the modal as soon as the countdown hits zero — the backend
  // will flip the row to `timeout` on its next sweep, but the user
  // shouldn't keep staring at a frozen modal in the meantime.
  useEffect(() => {
    if (pending && secondsLeft === 0) setPending(null);
  }, [pending, secondsLeft]);

  const resolve = useCallback(
    async (resolution: "approve" | "defer" | "cancel") => {
      if (!pending) return;
      setBusy(true);
      setMessage(null);
      try {
        const res = await fetch(
          `${apiClient.getServerUrl()}/api/billing/pending-auto-recharge/${pending.pending_id}/resolve`,
          {
            method: "POST",
            headers: authHeaders(),
            body: JSON.stringify({ resolution }),
          },
        );
        const data = await res.json().catch(() => ({}));
        if (!res.ok) {
          setMessage(data?.error || `${res.status}`);
          return;
        }
        if (resolution === "approve" && data?.charge === "ok") {
          setMessage(t("pending_ar_charge_ok", locale));
        } else if (resolution === "approve") {
          setMessage(
            t("pending_ar_charge_skipped", locale).replace(
              "{reason}",
              String(data?.charge ?? "unknown"),
            ),
          );
        }
        setPending(null);
      } finally {
        setBusy(false);
      }
    },
    [pending, authHeaders, locale],
  );

  if (!pending) return null;

  const minutes = Math.floor(secondsLeft / 60);
  const seconds = secondsLeft % 60;
  const amountUsd =
    pending.package_id.startsWith("topup_")
      ? Number(pending.package_id.slice("topup_".length))
      : null;

  return (
    <div className="pending-ar-backdrop">
      <div className="pending-ar-card">
        <div className="pending-ar-title">{t("pending_ar_title", locale)}</div>
        <div className="pending-ar-body">
          {t("pending_ar_body", locale)
            .replace("{balance}", pending.balance_at.toLocaleString())
            .replace("{threshold}", pending.threshold_at.toLocaleString())
            .replace("{amount}", amountUsd !== null ? `$${amountUsd}` : pending.package_id)}
        </div>
        <div className="pending-ar-countdown">
          {t("pending_ar_timer", locale)
            .replace("{m}", String(minutes).padStart(2, "0"))
            .replace("{s}", String(seconds).padStart(2, "0"))}
        </div>
        <div className="pending-ar-actions">
          <button
            className="pending-ar-btn approve"
            onClick={() => resolve("approve")}
            disabled={busy}
          >
            {t("pending_ar_approve", locale)}
          </button>
          <button
            className="pending-ar-btn defer"
            onClick={() => resolve("defer")}
            disabled={busy}
          >
            {t("pending_ar_defer", locale)}
          </button>
          <button
            className="pending-ar-btn cancel"
            onClick={() => resolve("cancel")}
            disabled={busy}
          >
            {t("pending_ar_cancel", locale)}
          </button>
        </div>
        {message && <div className="pending-ar-message">{message}</div>}
      </div>
    </div>
  );
}
