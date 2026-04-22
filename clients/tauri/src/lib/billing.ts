import { apiClient } from "./api";

/**
 * Billing helper module — keeps the credit ratio, plan catalog, and API
 * wrapper calls in one place so React components don't each have to
 * recreate the fetch plumbing.
 *
 * The 1:1000 USD-to-credit ratio here MUST match the Rust-side constant
 * `billing::CREDITS_PER_USD`. Any change has to land in both files.
 */
export const CREDITS_PER_USD = 1000;

export interface SubscriptionPlan {
  id: string;
  name: string;
  price_cents: number;
  price_usd: number;
  price_krw: number;
  credits_per_cycle: number;
  cycles: number;
  interval: "month" | "year";
}

export interface SubscriptionRecord {
  user_id: string;
  plan_id: string;
  provider: string | null;
  provider_sub_id: string | null;
  status: "active" | "cancelled" | "past_due";
  started_at: number;
  renewal_at: number;
  expires_at: number;
}

export interface BillingPreferences {
  user_id: string;
  low_balance_threshold: number;
  auto_recharge_enabled: boolean;
  auto_recharge_package_id: string | null;
  auto_recharge_threshold: number;
  saved_method_id: string | null;
  saved_method_provider: string | null;
}

/**
 * Live USD→KRW conversion via open.er-api.com (no key required).
 * Cached for 30 minutes to avoid hammering the public endpoint on
 * every re-render. Returns `null` on any failure so the UI can fall
 * back to the hardcoded KRW baseline shipped in the plan catalog.
 */
let cachedRate: { usdKrw: number; fetchedAt: number } | null = null;
const FX_CACHE_MS = 30 * 60 * 1000;

export async function getUsdKrwRate(): Promise<number | null> {
  const now = Date.now();
  if (cachedRate && now - cachedRate.fetchedAt < FX_CACHE_MS) {
    return cachedRate.usdKrw;
  }
  try {
    const res = await fetch("https://open.er-api.com/v6/latest/USD");
    if (!res.ok) return null;
    const data = await res.json();
    const rate = data?.rates?.KRW;
    if (typeof rate !== "number" || !Number.isFinite(rate)) return null;
    cachedRate = { usdKrw: rate, fetchedAt: now };
    return rate;
  } catch {
    return null;
  }
}

function authHeaders(): Record<string, string> {
  const token = apiClient.getToken();
  const headers: Record<string, string> = { "Content-Type": "application/json" };
  if (token) headers["Authorization"] = `Bearer ${token}`;
  return headers;
}

export async function fetchSubscriptionPlans(): Promise<SubscriptionPlan[]> {
  const res = await fetch(`${apiClient.getServerUrl()}/api/subscriptions/plans`, {
    headers: authHeaders(),
  });
  if (!res.ok) throw new Error(`plans: ${res.status}`);
  const data = await res.json();
  return data.plans ?? [];
}

export async function fetchCurrentSubscription(): Promise<SubscriptionRecord | null> {
  const res = await fetch(`${apiClient.getServerUrl()}/api/subscriptions/current`, {
    headers: authHeaders(),
  });
  if (!res.ok) return null;
  const data = await res.json();
  return data.subscription ?? null;
}

export async function subscribeToPlan(planId: string, provider = "stripe"): Promise<void> {
  const res = await fetch(`${apiClient.getServerUrl()}/api/subscriptions/subscribe`, {
    method: "POST",
    headers: authHeaders(),
    body: JSON.stringify({ plan_id: planId, provider }),
  });
  if (!res.ok) {
    const body = await res.json().catch(() => ({}));
    throw new Error(body?.error || `subscribe failed: ${res.status}`);
  }
}

/**
 * Kick off a real Stripe recurring-billing Checkout for this plan.
 * Returns the Stripe-hosted checkout URL — caller is expected to open
 * it in a new tab. On success Stripe invokes our webhook and we record
 * the subscription locally; the UI then refreshes `fetchCurrentSubscription`.
 */
export async function startStripeSubscriptionCheckout(planId: string): Promise<string | null> {
  const res = await fetch(`${apiClient.getServerUrl()}/api/subscriptions/stripe-checkout`, {
    method: "POST",
    headers: authHeaders(),
    body: JSON.stringify({ plan_id: planId }),
  });
  if (!res.ok) return null;
  const data = await res.json();
  return data?.checkout_url ?? null;
}

export interface CancelResult {
  status: string;
  refunded_usd: number;
  refunded_cents: number;
}

/** Cancel the active subscription. Returns the refund breakdown so the
 *  UI can surface "Refunded $27 for 1 unused month" style messaging. */
export async function cancelSubscriptionWithRefund(): Promise<CancelResult | null> {
  const res = await fetch(`${apiClient.getServerUrl()}/api/subscriptions/current`, {
    method: "DELETE",
    headers: authHeaders(),
  });
  if (!res.ok) return null;
  return res.json();
}

export async function cancelSubscription(): Promise<void> {
  const res = await fetch(`${apiClient.getServerUrl()}/api/subscriptions/current`, {
    method: "DELETE",
    headers: authHeaders(),
  });
  if (!res.ok) throw new Error(`cancel failed: ${res.status}`);
}

export async function fetchBillingPreferences(): Promise<BillingPreferences | null> {
  const res = await fetch(`${apiClient.getServerUrl()}/api/billing/preferences`, {
    headers: authHeaders(),
  });
  if (!res.ok) return null;
  return res.json();
}

export async function saveBillingPreferences(prefs: BillingPreferences): Promise<void> {
  const res = await fetch(`${apiClient.getServerUrl()}/api/billing/preferences`, {
    method: "PUT",
    headers: authHeaders(),
    body: JSON.stringify(prefs),
  });
  if (!res.ok) {
    const body = await res.json().catch(() => ({}));
    throw new Error(body?.error || `save failed: ${res.status}`);
  }
}

export async function fetchBalance(): Promise<{ balance: number; total_spent?: number } | null> {
  const res = await fetch(`${apiClient.getServerUrl()}/api/credits/balance`, {
    headers: authHeaders(),
  });
  if (!res.ok) return null;
  return res.json();
}

/**
 * Initiate a one-off top-up checkout session. `amountUsd` must be one
 * of 10 / 25 / 50 / 100 / 200 (the five spec tiers). Returns the
 * provider checkout URL for the caller to redirect to.
 */
export async function createTopupCheckout(
  amountUsd: 10 | 25 | 50 | 100 | 200,
  provider: "stripe" | "toss" = "stripe",
  saveMethod = false,
): Promise<{ checkout_url: string; transaction_id: string } | null> {
  const packageId = `topup_${amountUsd}`;
  const res = await fetch(`${apiClient.getServerUrl()}/api/checkout/create`, {
    method: "POST",
    headers: authHeaders(),
    body: JSON.stringify({
      package_id: packageId,
      provider,
      save_method: saveMethod,
    }),
  });
  if (!res.ok) return null;
  return res.json();
}

export const MANUAL_TOPUP_AMOUNTS = [10, 25, 50, 100, 200] as const;
export const AUTO_RECHARGE_AMOUNTS = [10, 25, 50] as const;
export const LOW_BALANCE_THRESHOLDS = [3000, 5000] as const;

export interface TossBillingSetup {
  customer_key: string;
  transaction_id: string;
  success_url: string;
  fail_url: string;
  price_krw: number;
  plan_name: string;
}

/**
 * Kick off the Toss 빌링키 flow for a recurring subscription. Returns
 * the payload the frontend hands to the TossPayments JS widget:
 *
 *   const tossPayments = TossPayments(clientKey);
 *   tossPayments.requestBillingAuth({
 *     customerKey: setup.customer_key,
 *     successUrl: setup.success_url,
 *     failUrl: setup.fail_url,
 *   });
 *
 * For a browser-based build we load `@tosspayments/payment-sdk` lazily;
 * for the Tauri desktop build we open `success_url` in a system browser
 * after redirecting the user to a hosted Toss widget page.
 */
export async function startTossSubscriptionSetup(
  planId: string,
): Promise<TossBillingSetup | null> {
  const res = await fetch(`${apiClient.getServerUrl()}/api/subscriptions/toss-setup`, {
    method: "POST",
    headers: authHeaders(),
    body: JSON.stringify({ plan_id: planId }),
  });
  if (!res.ok) return null;
  return res.json();
}
