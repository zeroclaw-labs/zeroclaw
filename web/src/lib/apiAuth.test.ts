import assert from "node:assert/strict";
import test, { beforeEach } from "node:test";
import type { OidcSession } from "./auth.ts";

// apiFetch in both auth modes: the pairing bearer (unchanged), and a signed-in
// session's CSRF header, 401 reason routing, operator_required and CSRF recovery.

class MemoryStorage implements Storage {
  private readonly values = new Map<string, string>();

  get length(): number { return this.values.size; }
  clear(): void { this.values.clear(); }
  getItem(key: string): string | null { return this.values.get(key) ?? null; }
  key(index: number): string | null { return [...this.values.keys()][index] ?? null; }
  removeItem(key: string): void { this.values.delete(key); }
  setItem(key: string, value: string): void { this.values.set(key, value); }
}

const storage = new MemoryStorage();
const fakeWindow = Object.assign(new EventTarget(), {
  location: { protocol: "https:", host: "zc.example", pathname: "/", search: "" },
});
Object.assign(globalThis, { window: fakeWindow, localStorage: storage });

interface Recorded {
  url: string;
  method: string;
  headers: Headers;
}
let requests: Recorded[] = [];
type Responder = (url: string, method: string) => Response | Promise<Response>;
let responder: Responder = () => new Response("{}");

globalThis.fetch = async (input, init) => {
  const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
  const method = (init?.method ?? "GET").toUpperCase();
  requests.push({ url, method, headers: new Headers(init?.headers) });
  return responder(url, method);
};

const auth = await import("./auth.ts");
const api = await import("./api.ts");

function session(overrides: Partial<OidcSession> = {}): OidcSession {
  return {
    kind: "oidc",
    provider: "oidc.corp",
    principal_id: "principal-a",
    display_name: "Ada",
    admin: true,
    expires_at: 2_000_000_000,
    csrf_token: "csrf-1",
    features: { chat: true, operator_pages: true },
    ...overrides,
  };
}

const json = (status: number, body: unknown) =>
  new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });

function countEvents(): Record<"unauthorized" | "reauth" | "resumed", number> & { stop: () => void } {
  const counts = { unauthorized: 0, reauth: 0, resumed: 0 };
  const handlers = (Object.keys(counts) as (keyof typeof counts)[]).map((name) => {
    const handler = () => {
      counts[name] += 1;
    };
    fakeWindow.addEventListener(auth.AUTH_EVENTS[name], handler);
    return () => fakeWindow.removeEventListener(auth.AUTH_EVENTS[name], handler);
  });
  return Object.assign(counts, { stop: () => handlers.forEach((off) => off()) });
}

beforeEach(() => {
  auth.setOidcSession(null);
  storage.clear();
  requests = [];
  responder = () => new Response("{}");
});

// ---------------------------------------------------------------------------
// Pairing mode: unchanged
// ---------------------------------------------------------------------------

test("pairing mode sends the bearer and no CSRF header", async () => {
  auth.setToken("tok");
  await api.apiFetch("/api/x", { method: "POST", body: "{}" });
  const [request] = requests;
  assert.equal(request?.headers.get("Authorization"), "Bearer tok");
  assert.equal(request?.headers.has(auth.CSRF_HEADER), false);
});

test("pairing mode 401 clears the token and signs out, as before", async () => {
  auth.setToken("tok");
  responder = () => new Response("Unauthorized", { status: 401 });
  const events = countEvents();
  await assert.rejects(api.apiFetch("/api/x"), (err: unknown) => {
    assert.ok(err instanceof api.UnauthorizedError);
    assert.ok(!(err instanceof api.SessionExpiredError));
    return true;
  });
  events.stop();
  assert.equal(storage.getItem("zeroclaw_token"), null);
  assert.equal(events.unauthorized, 1);
  assert.equal(events.reauth, 0);
});

// ---------------------------------------------------------------------------
// OIDC mode: headers
// ---------------------------------------------------------------------------

test("signed in: unsafe methods carry the CSRF header and never a bearer", async () => {
  // A token left over from earlier pairing must not be sent: the gateway would
  // prefer it over the session cookie.
  auth.setToken("stale");
  auth.setOidcSession(session());
  for (const method of ["POST", "PUT", "PATCH", "DELETE"]) {
    requests = [];
    await api.apiFetch("/api/x", { method, body: "{}" });
    const [request] = requests;
    assert.equal(request?.headers.get(auth.CSRF_HEADER), "csrf-1", method);
    assert.equal(request?.headers.has("Authorization"), false, method);
  }
});

test("signed in: safe methods carry neither CSRF nor a bearer", async () => {
  auth.setToken("stale");
  auth.setOidcSession(session());
  await api.apiFetch("/api/x");
  await api.apiFetch("/api/x", { method: "HEAD" });
  for (const request of requests) {
    assert.equal(request.headers.has(auth.CSRF_HEADER), false);
    assert.equal(request.headers.has("Authorization"), false);
  }
});

test("signed in: the personality PUT carries the CSRF header too", async () => {
  auth.setOidcSession(session());
  responder = () => json(200, { filename: "SOUL.md", mtime_ms: 1 });
  await api.putPersonalityFile("SOUL.md", "x", null);
  assert.equal(requests[0]?.headers.get(auth.CSRF_HEADER), "csrf-1");
  assert.equal(requests[0]?.headers.has("Authorization"), false);
});

test("signed in: concurrent GETs still share one request", async () => {
  auth.setOidcSession(session());
  let release: () => void = () => {};
  const gate = new Promise<void>((resolve) => {
    release = resolve;
  });
  responder = async () => {
    await gate;
    return json(200, { ok: true });
  };
  const both = Promise.all([api.apiFetch("/api/status"), api.apiFetch("/api/status")]);
  release();
  assert.deepEqual(await both, [{ ok: true }, { ok: true }]);
  assert.equal(requests.length, 1);
});

test("signed in: GETs made under different accounts never share one request", async () => {
  auth.setOidcSession(session());
  let release: () => void = () => {};
  const gate = new Promise<void>((resolve) => {
    release = resolve;
  });
  responder = async () => {
    await gate;
    return json(200, { ok: true });
  };
  const first = api.apiFetch("/api/status");
  auth.setOidcSession(session({ principal_id: "principal-b", csrf_token: "csrf-b" }));
  const second = api.apiFetch("/api/status");
  release();
  await Promise.all([first, second]);
  assert.equal(requests.length, 2);
});

// ---------------------------------------------------------------------------
// OIDC mode: 401 / 403 routing
// ---------------------------------------------------------------------------

test("session_expired asks for re-auth and keeps the session and the page", async () => {
  auth.setOidcSession(session());
  responder = () => json(401, { error: "expired", reason: "session_expired" });
  const events = countEvents();
  await assert.rejects(api.apiFetch("/api/x"), api.SessionExpiredError);
  events.stop();
  assert.equal(events.reauth, 1);
  assert.equal(events.unauthorized, 0);
  assert.equal(auth.getOidcSession()?.principal_id, "principal-a");
});

test("session_invalid, or a 401 without a reason, signs out", async () => {
  for (const body of [{ error: "gone", reason: "session_invalid" }, { error: "Unauthorized" }]) {
    auth.setOidcSession(session());
    responder = () => json(401, body);
    const events = countEvents();
    await assert.rejects(api.apiFetch("/api/x"), (err: unknown) => {
      assert.ok(err instanceof api.UnauthorizedError);
      assert.ok(!(err instanceof api.SessionExpiredError));
      return true;
    });
    events.stop();
    assert.equal(events.unauthorized, 1);
    assert.equal(events.reauth, 0);
  }
});

test("403 operator_required is a typed error the pages show as not available", async () => {
  auth.setOidcSession(session({ admin: false }));
  responder = () => json(403, { error: "no", reason: "operator_required" });
  await assert.rejects(api.apiFetch("/api/tools"), api.OperatorRequiredError);
});

test("403 csrf re-reads the session once and retries with its new token", async () => {
  auth.setOidcSession(session());
  let posts = 0;
  responder = (url, method) => {
    if (url.endsWith("/api/auth/session")) {
      return json(200, session({ csrf_token: "csrf-2" }));
    }
    if (method === "POST") {
      posts += 1;
      return posts === 1 ? json(403, { error: "csrf", reason: "csrf" }) : json(200, { saved: true });
    }
    return json(404, {});
  };
  const events = countEvents();
  assert.deepEqual(await api.apiFetch("/api/x", { method: "POST", body: "{}" }), { saved: true });
  events.stop();
  const postsSent = requests.filter((r) => r.method === "POST");
  assert.deepEqual(
    postsSent.map((r) => r.headers.get(auth.CSRF_HEADER)),
    ["csrf-1", "csrf-2"],
  );
  assert.equal(auth.getOidcSession()?.csrf_token, "csrf-2");
  assert.equal(events.resumed, 1);
});

test("a second CSRF refusal is not retried again", async () => {
  auth.setOidcSession(session());
  responder = (url) =>
    url.endsWith("/api/auth/session")
      ? json(200, session({ csrf_token: "csrf-2" }))
      : json(403, { error: "csrf", reason: "csrf" });
  await assert.rejects(api.apiFetch("/api/x", { method: "POST", body: "{}" }), api.HttpError);
  assert.equal(requests.filter((r) => r.method === "POST").length, 2);
});

test("a CSRF refusal after the session expired asks for re-auth instead of retrying", async () => {
  auth.setOidcSession(session());
  responder = (url) =>
    url.endsWith("/api/auth/session")
      ? json(401, { error: "expired", reason: "session_expired" })
      : json(403, { error: "csrf", reason: "csrf" });
  const events = countEvents();
  await assert.rejects(api.apiFetch("/api/x", { method: "POST", body: "{}" }), api.HttpError);
  events.stop();
  assert.equal(events.reauth, 1);
  assert.equal(requests.filter((r) => r.method === "POST").length, 1);
});

// ---------------------------------------------------------------------------
// Gateway reloads
// ---------------------------------------------------------------------------

const restarting = () =>
  new Response('{"error":"The gateway is restarting. Try again.","reason":"gateway_restarting"}', {
    status: 503,
    headers: { "Retry-After": "0" },
  });

test("signed in: a reload's 503 gateway_restarting is tried again, with no auth event", async () => {
  auth.setOidcSession(session());
  let refused = 0;
  responder = (url) => {
    if (url.endsWith("/api/status") && refused < 2) {
      refused += 1;
      return restarting();
    }
    return json(200, { ok: true });
  };
  const events = countEvents();
  assert.deepEqual(await api.apiFetch("/api/status"), { ok: true });
  events.stop();
  assert.equal(requests.filter((r) => r.url.endsWith("/api/status")).length, 3);
  assert.equal(events.unauthorized + events.reauth + events.resumed, 0);
});

test("signed in: an unsafe request refused by a reload is sent again, with its CSRF header", async () => {
  auth.setOidcSession(session());
  let refused = false;
  responder = (_url, method) => {
    if (method === "PUT" && !refused) {
      refused = true;
      return restarting();
    }
    return json(200, { ok: true });
  };
  await api.apiFetch("/api/config/prop", { method: "PUT", body: "{}" });
  const puts = requests.filter((r) => r.method === "PUT");
  assert.equal(puts.length, 2);
  assert.ok(puts.every((r) => r.headers.get(auth.CSRF_HEADER) === "csrf-1"));

  refused = false;
  requests = [];
  responder = (_url, method) => {
    if (method === "PUT" && !refused) {
      refused = true;
      return restarting();
    }
    return json(200, { mtime_ms: 1 });
  };
  await api.putPersonalityFile("SOUL.md", "hi", null);
  assert.equal(requests.filter((r) => r.method === "PUT").length, 2);
});

test("a 503 with another reason is not tried again", async () => {
  auth.setOidcSession(session());
  responder = () => json(503, { error: "down", reason: "core_unavailable" });
  await assert.rejects(api.apiFetch("/api/status"), (err: unknown) => {
    assert.ok(err instanceof api.HttpError);
    assert.equal(err.status, 503);
    return true;
  });
  assert.equal(requests.length, 1);
});

test("pairing: the pair, pair-code and new-code requests wait out a reload too", async () => {
  // A stopped gateway refuses every request, bearer or not, with the same 503.
  const refused = new Set<string>();
  responder = (url) => {
    const path = new URL(url, "https://zc.example").pathname;
    if (!refused.has(path)) {
      refused.add(path);
      return restarting();
    }
    if (path === "/api/pair") return json(200, { token: "tok-new" });
    return json(200, { pairing_code: "123456", pairing_required: true });
  };
  assert.deepEqual(await api.pair("123456"), { token: "tok-new" });
  assert.equal(auth.getToken(), "tok-new");
  assert.equal((await api.getAdminPairCode()).pairing_code, "123456");
  assert.equal((await api.generatePairCode()).pairing_code, "123456");
  const paths = requests.map((r) => new URL(r.url, "https://zc.example").pathname);
  assert.deepEqual(paths, [
    "/api/pair", "/api/pair",
    "/pair/code", "/pair/code",
    "/admin/paircode/new", "/admin/paircode/new",
  ]);
});
