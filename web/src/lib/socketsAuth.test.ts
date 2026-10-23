import assert from "node:assert/strict";
import test, { beforeEach } from "node:test";
import type { OidcSession } from "./auth.ts";

// Chat WebSocket and SSE stream credentials per auth mode, and how a signed-in
// session's close codes / refusals stop reconnection until re-auth.

class MemoryStorage implements Storage {
  private readonly values = new Map<string, string>();

  get length(): number { return this.values.size; }
  clear(): void { this.values.clear(); }
  getItem(key: string): string | null { return this.values.get(key) ?? null; }
  key(index: number): string | null { return [...this.values.keys()][index] ?? null; }
  removeItem(key: string): void { this.values.delete(key); }
  setItem(key: string, value: string): void { this.values.set(key, value); }
}

class FakeWebSocket {
  static readonly OPEN = 1;
  static instances: FakeWebSocket[] = [];
  readyState = 0;
  onopen: (() => void) | null = null;
  onclose: ((ev: { code: number }) => void) | null = null;
  onmessage: ((ev: { data: string }) => void) | null = null;
  onerror: ((ev: unknown) => void) | null = null;

  constructor(
    readonly url: string,
    readonly protocols: string[],
  ) {
    FakeWebSocket.instances.push(this);
  }

  send(): void {}

  close(): void {
    this.serverClose(1000);
  }

  serverClose(code: number): void {
    this.readyState = 3;
    this.onclose?.({ code });
  }
}

const storage = new MemoryStorage();
const fakeWindow = Object.assign(new EventTarget(), {
  location: { protocol: "https:", host: "zc.example", pathname: "/", search: "" },
});
Object.assign(globalThis, {
  window: fakeWindow,
  localStorage: storage,
  WebSocket: FakeWebSocket,
});

let sseResponder: () => Response = () => new Response("", { status: 503 });
const sseRequests: Headers[] = [];
globalThis.fetch = async (_input, init) => {
  sseRequests.push(new Headers(init?.headers));
  return sseResponder();
};

const auth = await import("./auth.ts");
const { WebSocketClient, WS_CLOSE_ACCESS_LOST, WS_CLOSE_SESSION_ENDED } = await import("./ws.ts");
const { SSEClient } = await import("./sse.ts");

const session: OidcSession = {
  kind: "oidc",
  provider: "oidc.corp",
  principal_id: "principal-a",
  display_name: "Ada",
  admin: true,
  expires_at: 2_000_000_000,
  csrf_token: "csrf-1",
  features: { chat: true, operator_pages: true },
};

const settle = () => new Promise((resolve) => setTimeout(resolve, 25));

function countEvents(): Record<"unauthorized" | "reauth" | "sessionEnded" | "accessLost", number> & {
  stop: () => void;
} {
  const counts = { unauthorized: 0, reauth: 0, sessionEnded: 0, accessLost: 0 };
  const offs = (Object.keys(counts) as (keyof typeof counts)[]).map((name) => {
    const handler = () => {
      counts[name] += 1;
    };
    fakeWindow.addEventListener(auth.AUTH_EVENTS[name], handler);
    return () => fakeWindow.removeEventListener(auth.AUTH_EVENTS[name], handler);
  });
  return Object.assign(counts, { stop: () => offs.forEach((off) => off()) });
}

const newClient = () =>
  new WebSocketClient({ agentAlias: "main", sessionId: "s1", reconnectDelay: 1 });
const lastSocket = () => {
  const socket = FakeWebSocket.instances[FakeWebSocket.instances.length - 1];
  assert.ok(socket, "expected a socket");
  return socket;
};

beforeEach(() => {
  FakeWebSocket.instances = [];
  sseRequests.length = 0;
  storage.clear();
  auth.setOidcSession(null);
});

// ---------------------------------------------------------------------------
// WebSocket
// ---------------------------------------------------------------------------

test("pairing mode puts the token on the chat socket, as before", () => {
  auth.setToken("tok");
  const client = newClient();
  client.connect();
  const socket = lastSocket();
  assert.match(socket.url, /[?&]token=tok(&|$)/);
  assert.deepEqual(socket.protocols, ["zeroclaw.v1", "bearer.tok"]);
  client.disconnect();
});

test("signed in: no token in the chat socket URL or subprotocols", () => {
  auth.setToken("stale");
  auth.setOidcSession(session);
  const client = newClient();
  client.connect();
  const socket = lastSocket();
  assert.doesNotMatch(socket.url, /token=/);
  assert.deepEqual(socket.protocols, ["zeroclaw.v1"]);
  client.disconnect();
});

test("an ordinary close still reconnects", async () => {
  const client = newClient();
  client.connect();
  lastSocket().serverClose(1006);
  await settle();
  assert.ok(FakeWebSocket.instances.length >= 2);
  client.disconnect();
});

test("close 4401 asks the auth layer to check the session and does not reconnect", async () => {
  auth.setOidcSession(session);
  const events = countEvents();
  const client = newClient();
  client.connect();
  lastSocket().serverClose(WS_CLOSE_SESSION_ENDED);
  await settle();
  events.stop();
  assert.equal(FakeWebSocket.instances.length, 1);
  // Not a prompt yet: an early re-auth may already hold a newer session.
  assert.equal(events.sessionEnded, 1);
  assert.equal(events.reauth, 0);
  assert.equal(events.accessLost, 0);
  client.disconnect();
});

test("close 4403 reports lost access and does not reconnect", async () => {
  auth.setOidcSession(session);
  const events = countEvents();
  const client = newClient();
  client.connect();
  lastSocket().serverClose(WS_CLOSE_ACCESS_LOST);
  await settle();
  events.stop();
  assert.equal(FakeWebSocket.instances.length, 1);
  assert.equal(events.accessLost, 1);
  assert.equal(events.reauth, 0);
  client.disconnect();
});

test("a stopped socket reconnects once the session is usable again", async () => {
  auth.setOidcSession(session);
  const client = newClient();
  client.connect();
  lastSocket().serverClose(WS_CLOSE_SESSION_ENDED);
  fakeWindow.dispatchEvent(new Event(auth.AUTH_EVENTS.resumed));
  assert.equal(FakeWebSocket.instances.length, 2);
  // A second resume while connected doesn't open another socket.
  fakeWindow.dispatchEvent(new Event(auth.AUTH_EVENTS.resumed));
  assert.equal(FakeWebSocket.instances.length, 2);
  client.disconnect();
});

test("while a re-auth is pending, a dropped socket waits instead of retrying", async () => {
  auth.setOidcSession(session);
  const client = newClient();
  client.connect();
  auth.markReauthPending(true);
  // A refused upgrade shows up as 1006.
  lastSocket().serverClose(1006);
  await settle();
  assert.equal(FakeWebSocket.instances.length, 1);
  fakeWindow.dispatchEvent(new Event(auth.AUTH_EVENTS.resumed));
  assert.equal(FakeWebSocket.instances.length, 2);
  client.disconnect();
});

test("a gateway reload (1001) reconnects", async () => {
  auth.setOidcSession(session);
  const events = countEvents();
  const client = newClient();
  client.connect();
  lastSocket().serverClose(1001);
  await settle();
  events.stop();
  assert.ok(FakeWebSocket.instances.length >= 2);
  assert.equal(events.sessionEnded + events.accessLost + events.reauth, 0);
  client.disconnect();
});

test("conversation limit (4409): no automatic reconnect; connect() tries again", async () => {
  auth.setOidcSession(session);
  const events = countEvents();
  const client = newClient();
  client.connect();
  lastSocket().serverClose(4409);
  await settle();
  events.stop();
  assert.equal(FakeWebSocket.instances.length, 1);
  assert.equal(events.sessionEnded + events.accessLost + events.reauth + events.unauthorized, 0);
  // A resume (or anything else) doesn't reopen it; only the owner does.
  fakeWindow.dispatchEvent(new Event(auth.AUTH_EVENTS.resumed));
  assert.equal(FakeWebSocket.instances.length, 1);
  client.connect();
  assert.equal(FakeWebSocket.instances.length, 2);
  client.disconnect();
});

test("conversation closed (4410): reconnects on the same id for a fresh conversation", async () => {
  auth.setOidcSession(session);
  const client = newClient();
  client.connect();
  lastSocket().serverClose(4410);
  await settle();
  assert.ok(FakeWebSocket.instances.length >= 2);
  assert.match(lastSocket().url, /[?&]session_id=s1(&|$)/);
  client.disconnect();
});

test("a disconnected socket ignores a resume", async () => {
  auth.setOidcSession(session);
  const client = newClient();
  client.connect();
  lastSocket().serverClose(WS_CLOSE_SESSION_ENDED);
  client.disconnect();
  fakeWindow.dispatchEvent(new Event(auth.AUTH_EVENTS.resumed));
  await settle();
  assert.equal(FakeWebSocket.instances.length, 1);
});

// ---------------------------------------------------------------------------
// SSE
// ---------------------------------------------------------------------------

test("signed in: the event stream sends no bearer", async () => {
  auth.setToken("stale");
  auth.setOidcSession(session);
  sseResponder = () => new Response("", { status: 403 });
  const client = new SSEClient({ reconnectDelay: 1 });
  client.connect();
  await settle();
  client.disconnect();
  assert.equal(sseRequests[0]?.has("Authorization"), false);
});

test("signed in: an expired session stops the stream and asks for re-auth", async () => {
  auth.setOidcSession(session);
  sseResponder = () =>
    new Response(JSON.stringify({ error: "expired", reason: "session_expired" }), {
      status: 401,
    });
  const events = countEvents();
  const client = new SSEClient({ reconnectDelay: 1 });
  client.connect();
  await settle();
  events.stop();
  assert.equal(sseRequests.length, 1);
  assert.equal(events.reauth, 1);
  // Usable again: the stream reconnects.
  sseResponder = () => new Response("", { status: 503 });
  fakeWindow.dispatchEvent(new Event(auth.AUTH_EVENTS.resumed));
  await settle();
  client.disconnect();
  assert.ok(sseRequests.length >= 2);
});

test("signed in: while a re-auth is pending, an ended stream waits for the resume", async () => {
  auth.setOidcSession(session);
  auth.markReauthPending(true);
  // The stream opens and ends at once (the gateway ended the session's stream).
  sseResponder = () => new Response("", { status: 200 });
  const client = new SSEClient({ reconnectDelay: 1 });
  client.connect();
  await settle();
  assert.equal(sseRequests.length, 1);
  auth.setOidcSession(session);
  fakeWindow.dispatchEvent(new Event(auth.AUTH_EVENTS.resumed));
  await settle();
  client.disconnect();
  assert.ok(sseRequests.length >= 2);
});

test("signed in: operator_required stops the stream without signing out", async () => {
  auth.setOidcSession({ ...session, admin: false });
  sseResponder = () =>
    new Response(JSON.stringify({ error: "no", reason: "operator_required" }), { status: 403 });
  const events = countEvents();
  const client = new SSEClient({ reconnectDelay: 1 });
  client.connect();
  await settle();
  client.disconnect();
  events.stop();
  assert.equal(sseRequests.length, 1);
  assert.equal(events.unauthorized + events.reauth, 0);
});

test("pairing mode keeps retrying a refused stream, as before", async () => {
  auth.setToken("tok");
  sseResponder = () => new Response("", { status: 401 });
  const client = new SSEClient({ reconnectDelay: 1 });
  client.connect();
  await settle();
  client.disconnect();
  assert.ok(sseRequests.length >= 2);
  assert.equal(sseRequests[0]?.get("Authorization"), "Bearer tok");
});

test("signed in: a reload's 503 gateway_restarting reconnects the stream, with no auth event", async () => {
  auth.setOidcSession(session);
  sseResponder = () =>
    new Response('{"error":"restarting","reason":"gateway_restarting"}', {
      status: 503,
      headers: { "Retry-After": "1" },
    });
  const events = countEvents();
  const client = new SSEClient({ reconnectDelay: 1 });
  client.connect();
  await settle();
  client.disconnect();
  events.stop();
  assert.ok(sseRequests.length >= 2);
  assert.equal(events.unauthorized + events.reauth + events.sessionEnded + events.accessLost, 0);
});

test("a socket without auto-reconnect still reports 4401 and 4403, but reconnects for nothing", async () => {
  // A chat socket that is one conversation: what ends it, ends it for good.
  for (const code of [WS_CLOSE_SESSION_ENDED, WS_CLOSE_ACCESS_LOST, 1001]) {
    FakeWebSocket.instances = [];
    auth.setOidcSession(session);
    const events = countEvents();
    const client = new WebSocketClient({
      agentAlias: "main",
      sessionId: "s1",
      reconnectDelay: 1,
      autoReconnect: false,
    });
    client.connect();
    lastSocket().serverClose(code);
    await settle();
    fakeWindow.dispatchEvent(new Event(auth.AUTH_EVENTS.resumed));
    await settle();
    events.stop();
    assert.equal(FakeWebSocket.instances.length, 1, String(code));
    assert.equal(events.sessionEnded, code === WS_CLOSE_SESSION_ENDED ? 1 : 0);
    assert.equal(events.accessLost, code === WS_CLOSE_ACCESS_LOST ? 1 : 0);
    client.disconnect();
  }
});
