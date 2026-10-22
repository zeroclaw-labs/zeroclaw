import assert from "node:assert/strict";
import test, { afterEach, beforeEach } from "node:test";
import React, { createElement } from "react";
import { act, create, type ReactTestInstance, type ReactTestRenderer } from "react-test-renderer";

// The Canvas page's own socket under dashboard sign-in: it stops on the
// session close codes and while a re-auth is pending, reconnects once when the
// session is usable again, reconnects after a gateway reload, retries a socket
// that never opened once, and leaves nothing running after a switch or unmount.

class FakeWebSocket {
  static readonly OPEN = 1;
  static instances: FakeWebSocket[] = [];
  readyState = 0;
  onopen: (() => void) | null = null;
  onclose: ((ev: { code: number }) => void) | null = null;
  onmessage: ((ev: { data: string }) => void) | null = null;
  onerror: (() => void) | null = null;

  constructor(readonly url: string) {
    FakeWebSocket.instances.push(this);
  }

  send(): void {}

  close(): void {
    this.serverClose(1000);
  }

  open(): void {
    this.readyState = 1;
    this.onopen?.();
  }

  serverClose(code: number): void {
    if (this.readyState === 3) return;
    this.readyState = 3;
    this.onclose?.({ code });
  }
}

const location = { protocol: "https:", host: "zc.example", hostname: "zc.example", port: "", pathname: "/canvas", search: "" };
const fakeWindow = Object.assign(new EventTarget(), {
  location,
  setTimeout: (fn: () => void, ms: number) => setTimeout(fn, ms),
  clearTimeout: (id: ReturnType<typeof setTimeout>) => clearTimeout(id),
  setInterval: (fn: () => void, ms: number) => setInterval(fn, ms),
  clearInterval: (id: ReturnType<typeof setInterval>) => clearInterval(id),
});
const fakeDocument = Object.assign(new EventTarget(), { hidden: false });
Object.assign(globalThis, {
  window: fakeWindow,
  document: fakeDocument,
  location,
  WebSocket: FakeWebSocket,
  React,
});
(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;
globalThis.fetch = async () =>
  new Response(JSON.stringify({ canvases: [] }), {
    status: 200,
    headers: { "Content-Type": "application/json" },
  });

const auth = await import("../lib/auth.ts");
const { default: Canvas } = await import("./Canvas.tsx");

// Canvas reconnects after 1 s; wait past that to prove a retry, or its absence.
const pastRetry = () => new Promise<void>((resolve) => setTimeout(resolve, 1300));
const sockets = () => FakeWebSocket.instances.length;
const last = () => {
  const socket = FakeWebSocket.instances[FakeWebSocket.instances.length - 1];
  assert.ok(socket);
  return socket;
};

function countEvents() {
  const counts = { sessionEnded: 0, accessLost: 0, reauth: 0 };
  const offs = (Object.keys(counts) as (keyof typeof counts)[]).map((name) => {
    const handler = () => {
      counts[name] += 1;
    };
    fakeWindow.addEventListener(auth.AUTH_EVENTS[name], handler);
    return () => fakeWindow.removeEventListener(auth.AUTH_EVENTS[name], handler);
  });
  return Object.assign(counts, { stop: () => offs.forEach((off) => off()) });
}

let renderer: ReactTestRenderer | null = null;
async function mountCanvas(): Promise<void> {
  await act(async () => {
    renderer = create(createElement(Canvas));
  });
  assert.equal(sockets(), 1);
  assert.match(last().url, /\/ws\/canvas\/default$/);
}

async function openLast(): Promise<void> {
  await act(async () => last().open());
}

async function resume(): Promise<void> {
  await act(async () => {
    fakeWindow.dispatchEvent(new Event(auth.AUTH_EVENTS.resumed));
  });
}

function canvasInput(): ReactTestInstance {
  assert.ok(renderer);
  return renderer.root.find((n) => n.type === "input" && n.props.type === "text");
}

async function switchCanvas(id: string): Promise<void> {
  await act(async () => canvasInput().props.onChange({ target: { value: id } }));
  await act(async () => canvasInput().props.onKeyDown({ key: "Enter" }));
}

beforeEach(() => {
  FakeWebSocket.instances = [];
  auth.setOidcSession({
    kind: "oidc",
    provider: "oidc.corp",
    principal_id: "principal-a",
    display_name: "Ada",
    admin: true,
    expires_at: 4_102_444_800,
    csrf_token: "csrf-1",
    features: { chat: true, operator_pages: true },
  });
});

afterEach(async () => {
  await act(async () => {
    renderer?.unmount();
  });
  renderer = null;
  auth.markReauthPending(false);
  auth.setOidcSession(null);
});

test("session expiry (4401): no timed retry, one reconnect after re-auth", async () => {
  await mountCanvas();
  await openLast();
  const events = countEvents();
  await act(async () => last().serverClose(4401));
  await pastRetry();
  assert.equal(sockets(), 1);
  assert.equal(events.sessionEnded, 1);
  await resume();
  assert.equal(sockets(), 2);
  assert.match(last().url, /\/ws\/canvas\/default$/);
  await resume();
  await pastRetry();
  assert.equal(sockets(), 2);
  events.stop();
});

test("admin removed (4403): no timed retry, one reconnect once access is back", async () => {
  await mountCanvas();
  await openLast();
  const events = countEvents();
  await act(async () => last().serverClose(4403));
  await pastRetry();
  assert.equal(sockets(), 1);
  assert.equal(events.accessLost, 1);
  await resume();
  assert.equal(sockets(), 2);
  events.stop();
});

test("while a re-auth is pending, a refused upgrade is not retried until the resume", async () => {
  await mountCanvas();
  auth.markReauthPending(true);
  await act(async () => last().serverClose(1006));
  await pastRetry();
  assert.equal(sockets(), 1);
  await resume();
  assert.equal(sockets(), 2);
});

test("a gateway reload (1001) reconnects after a second, with no auth event", async () => {
  await mountCanvas();
  await openLast();
  const events = countEvents();
  await act(async () => last().serverClose(1001));
  assert.equal(sockets(), 1);
  await pastRetry();
  assert.equal(sockets(), 2);
  assert.equal(events.sessionEnded + events.accessLost + events.reauth, 0);
  events.stop();
});

test("a socket that never opened (a refused upgrade) is retried once", async () => {
  await mountCanvas();
  await act(async () => last().serverClose(1006));
  await pastRetry();
  assert.equal(sockets(), 2);
  // The retry is refused too: no third try on a timer.
  await act(async () => last().serverClose(1006));
  await pastRetry();
  assert.equal(sockets(), 2);
});

test("an open socket that drops is left for the Reconnect button, as before", async () => {
  await mountCanvas();
  await openLast();
  await act(async () => last().serverClose(1006));
  await pastRetry();
  assert.equal(sockets(), 1);
});

test("a resume while connected opens no second socket", async () => {
  await mountCanvas();
  await openLast();
  await resume();
  assert.equal(sockets(), 1);
});

test("switching canvas replaces the socket, and the old one's close does nothing", async () => {
  await mountCanvas();
  const first = last();
  await act(async () => first.open());
  const events = countEvents();
  await switchCanvas("other");
  assert.equal(sockets(), 2);
  assert.match(last().url, /\/ws\/canvas\/other$/);
  assert.equal(first.readyState, 3);
  // A late close of the replaced socket starts nothing and tells nobody.
  first.readyState = 1;
  await act(async () => first.serverClose(4401));
  await act(async () => {
    first.readyState = 1;
    first.serverClose(1001);
  });
  await pastRetry();
  assert.equal(sockets(), 2);
  assert.equal(events.sessionEnded, 0);
  events.stop();
});

test("unmount closes the socket and cancels a pending reconnect", async () => {
  await mountCanvas();
  await openLast();
  await act(async () => last().serverClose(1001));
  await act(async () => {
    renderer?.unmount();
  });
  renderer = null;
  await pastRetry();
  assert.equal(sockets(), 1);

  // A socket open at unmount is closed, and a later resume reopens nothing.
  FakeWebSocket.instances = [];
  await mountCanvas();
  const open = last();
  await act(async () => open.open());
  await act(async () => {
    renderer?.unmount();
  });
  renderer = null;
  assert.equal(open.readyState, 3);
  await resume();
  await pastRetry();
  assert.equal(sockets(), 1);
});
