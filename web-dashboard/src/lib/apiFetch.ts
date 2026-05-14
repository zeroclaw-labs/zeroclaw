/**
 * Gateway API fetch wrapper.
 *
 * Centralises two concerns every `/api/*` call shares:
 *   1. Attaching the `Authorization: Bearer <token>` header when a
 *      token is stored. Without this, any handler gated by
 *      `require_auth` on the gateway returns 401 and the dashboard
 *      becomes non-functional on paired deployments.
 *   2. Surfacing 401s as a typed error that callers (or future
 *      interceptors) can react to — e.g. clearing the token and
 *      prompting re-pairing.
 *
 * Non-JSON responses flow through unchanged — callers that stream SSE
 * or read raw bytes can still use this helper for the auth header.
 */

import { clearToken, getToken } from "@/lib/auth";

export class UnauthorizedError extends Error {
  readonly status = 401;
  constructor(message = "Unauthorized") {
    super(message);
    this.name = "UnauthorizedError";
  }
}

export interface ApiFetchOptions extends RequestInit {
  /** Parse response body as JSON and return it typed. Default: true. */
  json?: boolean;
}

export async function apiFetch<T = unknown>(
  path: string,
  options: ApiFetchOptions = {},
): Promise<T> {
  const { json = true, headers: initHeaders, ...rest } = options;
  const headers = new Headers(initHeaders);
  if (!headers.has("Accept")) {
    headers.set("Accept", "application/json");
  }
  const token = getToken();
  if (token) {
    headers.set("Authorization", `Bearer ${token}`);
  }

  const res = await fetch(path, { ...rest, headers });

  if (res.status === 401) {
    // Token is no longer valid server-side — drop it so subsequent
    // navigation triggers the pairing flow cleanly. Callers catching
    // `UnauthorizedError` can decide whether to redirect.
    clearToken();
    throw new UnauthorizedError(`${path} → 401 Unauthorized`);
  }
  if (!res.ok) {
    throw new Error(`${path} → ${res.status} ${res.statusText}`);
  }
  if (!json) {
    return res as unknown as T;
  }
  return (await res.json()) as T;
}
