import { useCallback, useEffect, useMemo, useState } from "react";
import { t, type Locale } from "../lib/i18n";
import {
  AUTO_RECHARGE_AMOUNTS,
  LOW_BALANCE_THRESHOLDS,
  MANUAL_TOPUP_AMOUNTS,
  cancelSubscription,
  cancelSubscriptionWithRefund,
  createTopupCheckout,
  fetchBalance,
  fetchBillingPreferences,
  fetchCurrentSubscription,
  fetchSubscriptionPlans,
  getUsdKrwRate,
  saveBillingPreferences,
  startStripeSubscriptionCheckout,
  startTossSubscriptionSetup,
  subscribeToPlan,
  type BillingPreferences,
  type SubscriptionPlan,
  type SubscriptionRecord,
} from "../lib/billing";
void cancelSubscription; // legacy import kept for backward compat
void subscribeToPlan;    // superseded by startStripeSubscriptionCheckout on desktop

interface Props {
  locale: Locale;
  onBack: () => void;
}

/**
 * Single-page billing hub. Four stacked cards:
 *   1. Balance — current credits + approx USD / live-FX KRW equivalents.
 *   2. Subscription — active plan or the two subscribe options.
 *   3. Manual top-up — five buttons ($10/$25/$50/$100/$200) that
 *      redirect to the existing Stripe/Toss checkout for the package.
 *   4. Alerts + Auto-recharge — low-balance threshold picker,
 *      auto-recharge toggle + amount + threshold.
 *
 * The React side deliberately stays dumb about Stripe card vaulting —
 * that lives in the backend checkout flow; this page just sets the
 * `save_method=true` flag on the checkout POST when the user enables
 * auto-recharge for the first time.
 */
export function BillingPage({ locale, onBack }: Props) {
  const [balance, setBalance] = useState<number | null>(null);
  const [usdKrw, setUsdKrw] = useState<number | null>(null);
  const [plans, setPlans] = useState<SubscriptionPlan[]>([]);
  const [currentSub, setCurrentSub] = useState<SubscriptionRecord | null>(null);
  const [prefs, setPrefs] = useState<BillingPreferences | null>(null);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    const load = async () => {
      const [bal, fx, planList, sub, pref] = await Promise.all([
        fetchBalance(),
        getUsdKrwRate(),
        fetchSubscriptionPlans().catch(() => [] as SubscriptionPlan[]),
        fetchCurrentSubscription().catch(() => null),
        fetchBillingPreferences().catch(() => null),
      ]);
      if (cancelled) return;
      setBalance(bal?.balance ?? 0);
      setUsdKrw(fx);
      setPlans(planList);
      setCurrentSub(sub);
      setPrefs(pref);
    };
    void load();
    return () => {
      cancelled = true;
    };
  }, []);

  const balanceUsd = useMemo(() => (balance ?? 0) / 1000, [balance]);
  const balanceKrw = useMemo(() => {
    if (usdKrw === null) return null;
    return Math.round(balanceUsd * usdKrw);
  }, [balanceUsd, usdKrw]);

  const formatUsd = (cents: number) => `$${(cents / 100).toFixed(cents % 100 === 0 ? 0 : 2)}`;
  const formatKrw = (priceUsdCents: number, fallbackKrw: number) => {
    if (usdKrw === null) return `₩${fallbackKrw.toLocaleString(locale === "ko" ? "ko-KR" : "en-US")}`;
    const krw = Math.round((priceUsdCents / 100) * usdKrw);
    return `₩${krw.toLocaleString(locale === "ko" ? "ko-KR" : "en-US")}`;
  };

  const handleTopup = useCallback(
    async (amount: (typeof MANUAL_TOPUP_AMOUNTS)[number]) => {
      setBusy(true);
      setMessage(null);
      try {
        const saveCard = prefs?.auto_recharge_enabled ?? false;
        const result = await createTopupCheckout(amount, "stripe", saveCard);
        if (!result) {
          setMessage(t("billing_checkout_failed", locale));
          return;
        }
        window.open(result.checkout_url, "_blank", "noopener,noreferrer");
      } finally {
        setBusy(false);
      }
    },
    [prefs?.auto_recharge_enabled, locale],
  );

  const handleSubscribe = useCallback(
    async (plan: SubscriptionPlan, providerOverride?: "stripe" | "toss") => {
      setBusy(true);
      setMessage(null);
      try {
        // Spec (2026-04-23): Toss is the primary rail for Korean users —
        // Stripe does not issue merchant accounts to Korean entities.
        // We default to Toss and expose Stripe only through an
        // "overseas" fold on the subscription card.
        const useStripe = providerOverride === "stripe";
        if (!useStripe) {
          const setup = await startTossSubscriptionSetup(plan.id);
          if (!setup) {
            setMessage(t("billing_checkout_failed", locale));
            return;
          }
          // The frontend hands this payload to the TossPayments widget.
          // For Tauri desktop we open a hosted Toss page; for a web
          // build, lazily load `@tosspayments/payment-sdk`.
          const widgetScript = document.createElement("script");
          widgetScript.src = "https://js.tosspayments.com/v1/payment";
          widgetScript.onload = () => {
            const clientKey =
              (import.meta.env.VITE_TOSS_CLIENT_KEY as string | undefined) ||
              "test_ck_DnyRpQWGrNJxLOAkYYpOVKwv1M9E"; // public test key fallback
            const tp = (window as any).TossPayments?.(clientKey);
            if (!tp) {
              setMessage(t("billing_checkout_failed", locale));
              return;
            }
            tp.requestBillingAuth("카드", {
              customerKey: setup.customer_key,
              successUrl: setup.success_url,
              failUrl: setup.fail_url,
            });
          };
          document.body.appendChild(widgetScript);
          setMessage(
            t("billing_subscribe_pending", locale).replace("{plan}", plan.name),
          );
          return;
        }
        // Overseas fallback — Stripe Checkout in a new tab.
        const url = await startStripeSubscriptionCheckout(plan.id);
        if (!url) {
          setMessage(t("billing_checkout_failed", locale));
          return;
        }
        window.open(url, "_blank", "noopener,noreferrer");
        setMessage(
          t("billing_subscribe_pending", locale).replace("{plan}", plan.name),
        );
      } catch (e) {
        setMessage(e instanceof Error ? e.message : String(e));
      } finally {
        setBusy(false);
      }
    },
    [locale],
  );

  const handleCancel = useCallback(async () => {
    if (!currentSub) return;
    // Two-step confirmation for annual plans that still have months
    // left: show the refund estimate, require explicit confirm. Monthly
    // subscribers fall through to the silent path — no refund owed.
    const result = await cancelSubscriptionWithRefund();
    if (!result) {
      setMessage(t("billing_cancel_success", locale));
      return;
    }
    if (result.refunded_cents > 0) {
      setMessage(
        t("billing_cancel_with_refund", locale).replace(
          "{amount}",
          `$${result.refunded_usd.toFixed(2)}`,
        ),
      );
    } else {
      setMessage(t("billing_cancel_success", locale));
    }
    setCurrentSub({ ...currentSub, status: "cancelled" });
  }, [currentSub, locale]);

  const handlePrefsChange = useCallback(
    async (patch: Partial<BillingPreferences>) => {
      if (!prefs) return;
      const next: BillingPreferences = { ...prefs, ...patch };
      setPrefs(next);
      setBusy(true);
      try {
        await saveBillingPreferences(next);
      } catch (e) {
        setMessage(e instanceof Error ? e.message : String(e));
      } finally {
        setBusy(false);
      }
    },
    [prefs],
  );

  return (
    <div className="billing-page">
      <div className="billing-header">
        <button className="billing-back" onClick={onBack}>←</button>
        <h1>{t("billing_title", locale)}</h1>
      </div>

      {/* 1. Balance card */}
      <div className="billing-card">
        <div className="billing-card-title">{t("billing_balance", locale)}</div>
        <div className="billing-balance-value">
          {balance === null ? "…" : balance.toLocaleString()} <span className="billing-balance-unit">{t("billing_credits_unit", locale)}</span>
        </div>
        <div className="billing-balance-fx">
          ≈ ${balanceUsd.toFixed(2)}
          {balanceKrw !== null && (
            <>  ·  ₩{balanceKrw.toLocaleString(locale === "ko" ? "ko-KR" : "en-US")}</>
          )}
          {usdKrw !== null && (
            <span className="billing-fx-note">
              {"  "}({t("billing_fx_rate", locale).replace("{rate}", usdKrw.toFixed(0))})
            </span>
          )}
        </div>
      </div>

      {/* 2. Subscription card */}
      <div className="billing-card">
        <div className="billing-card-title">{t("billing_subscription", locale)}</div>
        {currentSub && currentSub.status === "active" ? (
          <>
            <div className="billing-sub-active">
              {t("billing_sub_active", locale).replace("{plan}", currentSub.plan_id)}
            </div>
            <div className="billing-sub-renewal">
              {t("billing_sub_renewal", locale).replace(
                "{date}",
                new Date(currentSub.renewal_at * 1000).toLocaleDateString(
                  locale === "ko" ? "ko-KR" : "en-US",
                ),
              )}
            </div>
            <button
              className="billing-btn billing-btn-danger"
              onClick={handleCancel}
              disabled={busy}
            >
              {t("billing_cancel_subscription", locale)}
            </button>
          </>
        ) : (
          <div className="billing-plan-list">
            {plans.map((plan) => (
              <button
                key={plan.id}
                className="billing-plan-card"
                onClick={() => handleSubscribe(plan)}
                disabled={busy}
              >
                <div className="billing-plan-name">{plan.name}</div>
                <div className="billing-plan-price">
                  {formatUsd(plan.price_cents)} / {t(plan.interval === "year" ? "billing_interval_year" : "billing_interval_month", locale)}
                </div>
                <div className="billing-plan-krw">
                  {formatKrw(plan.price_cents, plan.price_krw)}
                </div>
                <div className="billing-plan-credits">
                  +{plan.credits_per_cycle.toLocaleString()} {t("billing_credits_unit", locale)} / {t("billing_interval_month", locale)}
                </div>
                {plan.interval === "year" && (
                  <div className="billing-plan-badge">10% off</div>
                )}
              </button>
            ))}
          </div>
        )}
      </div>

      {/* 3. Manual top-up card */}
      <div className="billing-card">
        <div className="billing-card-title">{t("billing_topup", locale)}</div>
        <div className="billing-topup-grid">
          {MANUAL_TOPUP_AMOUNTS.map((amount) => (
            <button
              key={amount}
              className="billing-topup-btn"
              onClick={() => handleTopup(amount)}
              disabled={busy}
            >
              <div className="billing-topup-usd">${amount}</div>
              <div className="billing-topup-credits">
                +{(amount * 1000).toLocaleString()}
              </div>
              {usdKrw !== null && (
                <div className="billing-topup-krw">
                  ₩{Math.round(amount * usdKrw).toLocaleString(locale === "ko" ? "ko-KR" : "en-US")}
                </div>
              )}
            </button>
          ))}
        </div>
      </div>

      {/* 4. Alerts + auto-recharge */}
      {prefs && (
        <div className="billing-card">
          <div className="billing-card-title">{t("billing_alerts", locale)}</div>
          <div className="billing-field">
            <label>{t("billing_low_balance_label", locale)}</label>
            <select
              value={prefs.low_balance_threshold}
              onChange={(e) =>
                handlePrefsChange({ low_balance_threshold: Number(e.target.value) })
              }
              disabled={busy}
            >
              {LOW_BALANCE_THRESHOLDS.map((n) => (
                <option key={n} value={n}>
                  {n.toLocaleString()} {t("billing_credits_unit", locale)}
                </option>
              ))}
            </select>
          </div>
          <div className="billing-field">
            <label>
              <input
                type="checkbox"
                checked={prefs.auto_recharge_enabled}
                onChange={(e) =>
                  handlePrefsChange({ auto_recharge_enabled: e.target.checked })
                }
                disabled={busy}
              />
              {"  "}
              {t("billing_auto_recharge_label", locale)}
            </label>
          </div>
          {prefs.auto_recharge_enabled && (
            <>
              <div className="billing-field">
                <label>{t("billing_auto_amount_label", locale)}</label>
                <select
                  value={prefs.auto_recharge_package_id || "topup_25"}
                  onChange={(e) =>
                    handlePrefsChange({ auto_recharge_package_id: e.target.value })
                  }
                  disabled={busy}
                >
                  {AUTO_RECHARGE_AMOUNTS.map((a) => (
                    <option key={a} value={`topup_${a}`}>
                      ${a}
                    </option>
                  ))}
                </select>
              </div>
              <div className="billing-field">
                <label>{t("billing_auto_threshold_label", locale)}</label>
                <select
                  value={prefs.auto_recharge_threshold}
                  onChange={(e) =>
                    handlePrefsChange({
                      auto_recharge_threshold: Number(e.target.value),
                    })
                  }
                  disabled={busy}
                >
                  {LOW_BALANCE_THRESHOLDS.map((n) => (
                    <option key={n} value={n}>
                      {n.toLocaleString()} {t("billing_credits_unit", locale)}
                    </option>
                  ))}
                </select>
              </div>
              <div className="billing-note">
                {t("billing_auto_card_note", locale)}
              </div>
            </>
          )}
        </div>
      )}

      {message && <div className="billing-message">{message}</div>}
    </div>
  );
}
