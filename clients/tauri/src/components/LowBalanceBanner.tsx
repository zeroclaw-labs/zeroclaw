import { useEffect, useState } from "react";
import { t, type Locale } from "../lib/i18n";
import {
  fetchBalance,
  fetchBillingPreferences,
  type BillingPreferences,
} from "../lib/billing";

interface Props {
  locale: Locale;
  onOpenBilling: () => void;
}

const POLL_MS = 60_000; // 1-minute refresh is fine; the balance only
                        // moves at request completion time anyway.

/**
 * Thin banner rendered above the chat view whenever the user's credit
 * balance drops at or below their preferred low-balance threshold
 * (spec: 3,000 or 5,000). Clicking the banner opens the billing page;
 * dismissing hides it for the current session only so the nag re-
 * appears after each app launch as long as the condition persists.
 */
export function LowBalanceBanner({ locale, onOpenBilling }: Props) {
  const [balance, setBalance] = useState<number | null>(null);
  const [prefs, setPrefs] = useState<BillingPreferences | null>(null);
  const [dismissed, setDismissed] = useState(false);

  useEffect(() => {
    let cancelled = false;
    const tick = async () => {
      const [bal, pref] = await Promise.all([
        fetchBalance(),
        fetchBillingPreferences(),
      ]);
      if (cancelled) return;
      setBalance(bal?.balance ?? null);
      setPrefs(pref);
    };
    void tick();
    const timer = setInterval(tick, POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, []);

  if (dismissed || balance === null || prefs === null) return null;
  if (balance > prefs.low_balance_threshold) return null;

  return (
    <div className="low-balance-banner">
      <div className="low-balance-text">
        {t("low_balance_banner_text", locale)
          .replace("{balance}", balance.toLocaleString())
          .replace("{threshold}", prefs.low_balance_threshold.toLocaleString())}
      </div>
      <div className="low-balance-actions">
        <button className="low-balance-cta" onClick={onOpenBilling}>
          {t("low_balance_recharge", locale)}
        </button>
        <button className="low-balance-dismiss" onClick={() => setDismissed(true)}>
          ✕
        </button>
      </div>
    </div>
  );
}
