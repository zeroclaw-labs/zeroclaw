import assert from "node:assert/strict";
import test from "node:test";
import { createJiti } from "jiti";
import type { OidcSession } from "./auth.ts";
import type { AuthMethods, InitialAuthDeps, SessionProbe } from "./authSession.ts";

// lib/basePath reads `window` at import, so the globals go in first and the
// modules load afterwards.
const fakeWindow = Object.assign(new EventTarget(), {
  location: { protocol: "https:", host: "zc.example", pathname: "/", search: "" },
});
Object.assign(globalThis, { window: fakeWindow });

const auth = await import("./auth.ts");
const authSession = await import("./authSession.ts");

function session(overrides: Partial<OidcSession> = {}): OidcSession {
  return {
    kind: "oidc",
    provider: "oidc.corp",
    principal_id: "principal-a",
    display_name: "Ada",
    admin: false,
    expires_at: 2_000_000_000,
    csrf_token: "csrf-1",
    features: { chat: true, operator_pages: false, core_sessions: false },
    ...overrides,
  };
}

const methods = (aliases: string[]): AuthMethods => ({
  pairing_required: true,
  oidc: aliases.map((alias) => ({ alias, provider: `oidc.${alias}` })),
});

function deps(overrides: Partial<InitialAuthDeps>): InitialAuthDeps & { calls: string[] } {
  const calls: string[] = [];
  return {
    calls,
    hasToken: false,
    desktop: false,
    getHealth: async () => {
      calls.push("health");
      return { require_pairing: true };
    },
    getMethods: async () => {
      calls.push("methods");
      return methods([]);
    },
    getSession: async (): Promise<SessionProbe> => {
      calls.push("session");
      return { kind: "none", reason: null };
    },
    ...overrides,
  };
}

// ---------------------------------------------------------------------------
// Startup state machine
// ---------------------------------------------------------------------------

test("a stored pairing token starts paired without any request", async () => {
  const d = deps({
    hasToken: true,
    getHealth: () => assert.fail("health must not be requested"),
    getMethods: () => assert.fail("methods must not be requested"),
    getSession: () => assert.fail("session must not be requested"),
  });
  assert.deepEqual(await authSession.resolveInitialAuth(d), { kind: "paired" });
});

test("a gateway without pairing is open, as before", async () => {
  const d = deps({ getHealth: async () => ({ require_pairing: false }) });
  assert.deepEqual(await authSession.resolveInitialAuth(d), { kind: "open" });
});

test("sign-in off: today's pairing screen, and the session cookie is never consulted", async () => {
  const d = deps({});
  assert.deepEqual(await authSession.resolveInitialAuth(d), {
    kind: "signed-out",
    providers: [],
  });
  assert.deepEqual(d.calls.sort(), ["health", "methods"]);
});

test("a gateway that doesn't answer /api/auth/methods is treated as sign-in off", async () => {
  for (const getMethods of [async () => null, async () => Promise.reject(new Error("404"))]) {
    const d = deps({ getMethods });
    assert.deepEqual(await authSession.resolveInitialAuth(d), {
      kind: "signed-out",
      providers: [],
    });
    assert.ok(!d.calls.includes("session"));
  }
});

test("an unreachable gateway falls back to the pairing screen", async () => {
  const d = deps({
    getHealth: async () => Promise.reject(new Error("offline")),
    getMethods: async () => Promise.reject(new Error("offline")),
  });
  assert.deepEqual(await authSession.resolveInitialAuth(d), {
    kind: "signed-out",
    providers: [],
  });
});

test("the desktop app never asks for sign-in providers", async () => {
  const d = deps({
    desktop: true,
    getMethods: () => assert.fail("methods must not be requested in the desktop app"),
  });
  assert.deepEqual(await authSession.resolveInitialAuth(d), {
    kind: "signed-out",
    providers: [],
  });
});

test("providers configured and a live session cookie: signed in", async () => {
  const s = session();
  const d = deps({
    getMethods: async () => methods(["corp"]),
    getSession: async () => ({ kind: "oidc", session: s, clockOffsetMs: 1500 }),
  });
  assert.deepEqual(await authSession.resolveInitialAuth(d), {
    kind: "signed-in",
    session: s,
    clockOffsetMs: 1500,
    providers: [{ alias: "corp", provider: "oidc.corp" }],
  });
});

test("providers configured and no session: the sign-in screen offers them", async () => {
  const d = deps({
    getMethods: async () => methods(["corp", "okta"]),
    getSession: async () => ({ kind: "none", reason: "session_invalid" }),
  });
  assert.deepEqual(await authSession.resolveInitialAuth(d), {
    kind: "signed-out",
    providers: [
      { alias: "corp", provider: "oidc.corp" },
      { alias: "okta", provider: "oidc.okta" },
    ],
  });
});

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

test("failureReason reads the contract's reasons and nothing else", () => {
  assert.equal(
    authSession.failureReason('{"error":"x","reason":"session_expired"}'),
    "session_expired",
  );
  assert.equal(authSession.failureReason('{"reason":"operator_required"}'), "operator_required");
  assert.equal(authSession.failureReason('{"reason":"something_else"}'), null);
  assert.equal(authSession.failureReason("Unauthorized"), null);
  assert.equal(authSession.failureReason(""), null);
});

test("parseAuthMethods drops malformed providers", () => {
  assert.deepEqual(
    authSession.parseAuthMethods({
      pairing_required: true,
      oidc: [{ alias: "corp", provider: "oidc.corp" }, { alias: "" }, null, { provider: "x" }],
    }),
    methods(["corp"]),
  );
  assert.deepEqual(authSession.parseAuthMethods({ oidc: "nope" }), methods([]));
  assert.equal(authSession.parseAuthMethods(null), null);
});

test("parseOidcSession requires the credential fields and denies missing features", () => {
  const raw = {
    kind: "oidc",
    provider: "oidc.corp",
    principal_id: "p",
    display_name: "Ada",
    admin: true,
    expires_at: 123,
    csrf_token: "t",
  };
  assert.deepEqual(authSession.parseOidcSession(raw), {
    ...raw,
    features: { chat: false, operator_pages: false, core_sessions: false },
  });
  assert.deepEqual(
    authSession.parseOidcSession({
      ...raw,
      features: { chat: true, operator_pages: true, core_sessions: true },
    })?.features,
    { chat: true, operator_pages: true, core_sessions: true },
  );
  assert.equal(authSession.parseOidcSession({ ...raw, csrf_token: "" }), null);
  assert.equal(authSession.parseOidcSession({ ...raw, principal_id: undefined }), null);
  assert.equal(authSession.parseOidcSession({ ...raw, kind: "pairing" }), null);
  assert.equal(authSession.parseOidcSession({ ...raw, expires_at: "soon" }), null);
});

test("fetchAuthSession reads a cookie session, a 401 reason and a pairing answer", async () => {
  const s = session();
  const at = Date.parse("Mon, 19 Oct 2026 12:00:00 GMT");
  const asFetch = (response: Response) => async () => response;

  const signedIn = await authSession.fetchAuthSession(
    asFetch(
      new Response(JSON.stringify(s), {
        status: 200,
        headers: { Date: "Mon, 19 Oct 2026 12:00:30 GMT" },
      }),
    ),
    () => at,
  );
  assert.deepEqual(signedIn, { kind: "oidc", session: s, clockOffsetMs: 30_000 });

  assert.deepEqual(
    await authSession.fetchAuthSession(
      asFetch(new Response('{"error":"x","reason":"session_expired"}', { status: 401 })),
    ),
    { kind: "none", reason: "session_expired" },
  );
  assert.deepEqual(
    await authSession.fetchAuthSession(asFetch(new Response('{"kind":"pairing"}'))),
    { kind: "pairing" },
  );
  assert.deepEqual(
    await authSession.fetchAuthSession(asFetch(new Response("", { status: 503 }))),
    { kind: "error" },
  );
});

test("fetchAuthSession never sends a bearer", async () => {
  let init: RequestInit | undefined;
  await authSession.fetchAuthSession(async (_url, i) => {
    init = i;
    return new Response("", { status: 401 });
  });
  assert.equal(new Headers(init?.headers).has("Authorization"), false);
});

// ---------------------------------------------------------------------------
// Expiry and re-authentication
// ---------------------------------------------------------------------------

test("expiryPhase: banner in the last two minutes, expired at the deadline", () => {
  const expiresAt = 1_000_000;
  const ms = expiresAt * 1000;
  assert.equal(authSession.expiryPhase(expiresAt, ms - 121_000, 0), "active");
  assert.equal(authSession.expiryPhase(expiresAt, ms - 120_000, 0), "warning");
  assert.equal(authSession.expiryPhase(expiresAt, ms - 1, 0), "warning");
  assert.equal(authSession.expiryPhase(expiresAt, ms, 0), "expired");
});

test("expiryPhase uses the server clock, so a fast browser clock doesn't expire a fresh session", () => {
  const expiresAt = 1_000_000;
  const serverNow = expiresAt * 1000 - 3_600_000; // an hour left
  const browserNow = serverNow + 7_200_000; // browser clock two hours fast
  assert.equal(authSession.expiryPhase(expiresAt, browserNow, 0), "expired");
  assert.equal(authSession.expiryPhase(expiresAt, browserNow, serverNow - browserNow), "active");
});

test("decideSignedIn: adopt, resume for the same account, reload for another", () => {
  assert.equal(authSession.decideSignedIn(null, session()), "adopt");
  assert.equal(
    authSession.decideSignedIn(session(), session({ csrf_token: "csrf-2" })),
    "resume",
  );
  assert.equal(
    authSession.decideSignedIn(session(), session({ principal_id: "principal-b" })),
    "reload",
  );
});

test("re-auth as the same account resumes in place with the new session", async () => {
  auth.setOidcSession(session());
  let resumed = 0;
  const onResumed = () => {
    resumed += 1;
  };
  window.addEventListener(auth.AUTH_EVENTS.resumed, onResumed);
  const next = session({ csrf_token: "csrf-2", expires_at: 2_000_000_600 });
  const outcome = await authSession.refreshOidcSession({
    probe: async () => ({ kind: "oidc", session: next, clockOffsetMs: 0 }),
    reload: () => assert.fail("same account must not reload"),
  });
  window.removeEventListener(auth.AUTH_EVENTS.resumed, onResumed);
  assert.equal(outcome, "resumed");
  assert.deepEqual(auth.getOidcSession(), next);
  assert.equal(resumed, 1);
  auth.setOidcSession(null);
});

test("re-auth as a different account reloads and keeps nothing of the old one", async () => {
  const before = session();
  auth.setOidcSession(before);
  let reloads = 0;
  const outcome = await authSession.refreshOidcSession({
    probe: async () => ({
      kind: "oidc",
      session: session({ principal_id: "principal-b", csrf_token: "csrf-b" }),
      clockOffsetMs: 0,
    }),
    reload: () => {
      reloads += 1;
    },
  });
  assert.equal(outcome, "reloading");
  assert.equal(reloads, 1);
  // The page is about to reload; it must not start using the other account.
  assert.deepEqual(auth.getOidcSession(), before);
  auth.setOidcSession(null);
});

test("refreshOidcSession reports a missing session without acting on it", async () => {
  auth.setOidcSession(session());
  const run = (probe: SessionProbe) =>
    authSession.refreshOidcSession({
      probe: async () => probe,
      reload: () => assert.fail("must not reload"),
    });
  assert.equal(await run({ kind: "none", reason: "session_expired" }), "expired");
  assert.equal(await run({ kind: "none", reason: "session_invalid" }), "signed-out");
  assert.equal(await run({ kind: "none", reason: null }), "signed-out");
  assert.equal(await run({ kind: "error" }), "error");
  assert.equal(auth.getOidcSession()?.principal_id, "principal-a");
  auth.setOidcSession(null);
});

// ---------------------------------------------------------------------------
// URLs and the channel
// ---------------------------------------------------------------------------

test("sign-in URLs: a full-page start returns to the current path, the popup doesn't", () => {
  assert.equal(
    authSession.signInUrl("corp", { returnPath: "/agent/x?y=1" }),
    "/auth/sign-in/corp?return=%2Fagent%2Fx%3Fy%3D1",
  );
  assert.equal(
    authSession.signInUrl("corp", { popup: true, returnPath: "/ignored" }),
    "/auth/sign-in/corp?popup=1",
  );
  assert.equal(authSession.signInUrl("a/b c"), "/auth/sign-in/a%2Fb%20c");
});

test("sign-in URLs and return paths honour a gateway path prefix", async () => {
  Object.assign(fakeWindow, { __ZEROCLAW_BASE__: "/zc/" });
  try {
    // A fresh module graph, so lib/basePath reads the prefix.
    const fresh = createJiti(import.meta.url, { moduleCache: false, fsCache: false });
    const m = await fresh.import<typeof import("./authSession.ts")>("./authSession.ts");
    assert.equal(
      m.currentReturnPath({ pathname: "/zc/agent/x", search: "?y=1" }),
      "/agent/x?y=1",
    );
    assert.equal(m.currentReturnPath({ pathname: "/zc", search: "" }), "/");
    assert.equal(
      m.signInUrl("corp", { returnPath: "/agent/x" }),
      "/zc/auth/sign-in/corp?return=%2Fagent%2Fx",
    );
  } finally {
    delete (fakeWindow as { __ZEROCLAW_BASE__?: string }).__ZEROCLAW_BASE__;
  }
});

test("the auth channel accepts only the two message types", () => {
  assert.deepEqual(authSession.parseChannelMessage({ type: "signed-in", principal: "ab12" }), {
    type: "signed-in",
  });
  assert.deepEqual(authSession.parseChannelMessage({ type: "signed-out" }), {
    type: "signed-out",
  });
  assert.equal(authSession.parseChannelMessage({ type: "token", token: "x" }), null);
  assert.equal(authSession.parseChannelMessage("signed-in"), null);
});

// ---------------------------------------------------------------------------
// Gateway reloads
// ---------------------------------------------------------------------------

const restarting = () =>
  new Response('{"error":"The gateway is restarting. Try again.","reason":"gateway_restarting"}', {
    status: 503,
    headers: { "Retry-After": "0" },
  });

function answers(...responses: (() => Response)[]) {
  const calls: string[] = [];
  const fetchImpl = async (url: string) => {
    calls.push(url);
    const next = responses.shift();
    assert.ok(next, `unexpected request ${url}`);
    return next();
  };
  return { calls, fetchImpl };
}

test("a reload's 503 gateway_restarting is waited out by the methods, session and sign-out requests", async () => {
  const m = answers(restarting, () => new Response(JSON.stringify(methods(["corp"]))));
  assert.deepEqual(await authSession.fetchAuthMethods(m.fetchImpl), methods(["corp"]));
  assert.equal(m.calls.length, 2);

  const s = session();
  const p = answers(restarting, restarting, () => new Response(JSON.stringify(s)));
  const probe = await authSession.fetchAuthSession(p.fetchImpl);
  assert.equal(probe.kind, "oidc");
  assert.equal(p.calls.length, 3);

  const o = answers(restarting, () => new Response(null, { status: 204 }));
  await authSession.postSignOut("csrf-1", o.fetchImpl);
  assert.equal(o.calls.length, 2);
});

test("only gateway_restarting is tried again, and only a few times", async () => {
  const other = answers(
    () => new Response('{"error":"x","reason":"core_unavailable"}', { status: 503 }),
  );
  assert.deepEqual(await authSession.fetchAuthSession(other.fetchImpl), { kind: "error" });
  assert.equal(other.calls.length, 1);

  const endless = answers(...Array.from({ length: 10 }, () => restarting));
  assert.deepEqual(await authSession.fetchAuthSession(endless.fetchImpl), { kind: "error" });
  assert.equal(endless.calls.length, authSession.RESTART_ATTEMPTS);
});

test("Retry-After sets the wait before trying again, within bounds", () => {
  const wait = (value?: string) =>
    authSession.restartRetryDelayMs(
      new Response(null, { status: 503, headers: value === undefined ? {} : { "Retry-After": value } }),
    );
  assert.equal(wait("1"), 1000);
  assert.equal(wait("0"), 0);
  assert.equal(wait(), 1000);
  assert.equal(wait("soon"), 1000);
  assert.equal(wait("120"), 5000);
});
