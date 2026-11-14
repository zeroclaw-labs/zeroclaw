import assert from "node:assert/strict";
import test, { afterEach } from "node:test";
import React, { createElement } from "react";
import { act, create, type ReactTestInstance, type ReactTestRenderer } from "react-test-renderer";
import type { OidcSession } from "../lib/auth.ts";

// The agent pages for a signed-in account without the operator dashboard: the
// agents list comes from /api/agents/available, and the chat route refuses an
// agent the account may not use before any socket opens.

// The chat a successful retry opens connects through this; no real socket.
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

  sent: string[] = [];
  send(data: string): void { this.sent.push(data); }

  close(): void {
    if (this.readyState === 3) return;
    this.readyState = 3;
    this.onclose?.({ code: 1000 });
  }
}

const fakeWindow = Object.assign(new EventTarget(), {
  location: { protocol: "https:", host: "zc.example", hostname: "zc.example", port: "", pathname: "/", search: "" },
  setTimeout: (fn: () => void, ms: number) => setTimeout(fn, ms),
  clearTimeout: (id: ReturnType<typeof setTimeout>) => clearTimeout(id),
});
// The chat's outside-click and Escape handlers listen on the document.
Object.assign(globalThis, { window: fakeWindow, document: new EventTarget(), WebSocket: FakeWebSocket, React });
(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

const requested: string[] = [];
let agentsBody: unknown = { agents: [] };
let agentsStatus = 200;
globalThis.fetch = async (input) => {
  const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
  requested.push(url);
  if (url.endsWith("/api/agents/available")) {
    return new Response(JSON.stringify(agentsBody), {
      status: agentsStatus,
      headers: { "Content-Type": "application/json" },
    });
  }
  return new Response(JSON.stringify({ error: "no", reason: "operator_required" }), { status: 403 });
};

const { setOidcSession } = await import("../lib/auth.ts");
const { default: AgentsList } = await import("./AgentsList.tsx");
const { default: AgentChat } = await import("./AgentChat.tsx");
const { default: ChatWorkspace } = await import("./ChatWorkspace.tsx");
const { MemoryRouter, Route, Routes } = await import("react-router-dom");

const scoped: OidcSession = {
  kind: "oidc",
  provider: "oidc.corp",
  principal_id: "principal-a",
  display_name: "Ada",
  admin: false,
  expires_at: 4_102_444_800,
  csrf_token: "csrf-1",
  features: { chat: true, operator_pages: false },
};

const settle = () =>
  act(async () => {
    await new Promise<void>((resolve) => setTimeout(resolve, 10));
  });

function text(node: ReactTestInstance): string {
  return node.children.map((c) => (typeof c === "string" ? c : text(c))).join("");
}

const marked = (r: ReactTestRenderer, attr: string) =>
  r.root.findAll((n) => typeof n.type === "string" && n.props[attr] !== undefined);
const availableRequests = () => requested.filter((u) => u.endsWith("/api/agents/available"));

let renderer: ReactTestRenderer | null = null;
async function render(path: string, element: React.ReactElement, routePath: string) {
  await act(async () => {
    renderer = create(
      createElement(
        MemoryRouter,
        { initialEntries: [path] },
        createElement(Routes, null, createElement(Route, { path: routePath, element })),
      ),
    );
  });
  await settle();
  assert.ok(renderer);
  return renderer;
}

afterEach(async () => {
  await act(async () => {
    renderer?.unmount();
  });
  renderer = null;
  setOidcSession(null);
  requested.length = 0;
  agentsStatus = 200;
  FakeWebSocket.instances.length = 0;
});

test("the agents page lists only the agents this account may use", async () => {
  setOidcSession(scoped);
  agentsBody = {
    agents: [
      { alias: "support", enabled: true },
      { alias: "triage", enabled: false },
    ],
  };
  const r = await render("/agents", createElement(AgentsList), "/agents");
  const rows = r.root.findAll(
    (n) => typeof n.type === "string" && n.props["data-available-agent"] !== undefined,
  );
  assert.deepEqual(
    rows.map((n) => n.props["data-available-agent"]),
    ["support", "triage"],
  );
  // Enabled agents open the chat; a disabled one is listed without a link.
  const links = r.root.findAll((n) => typeof n.props.to === "string");
  assert.deepEqual(
    [...new Set(links.map((n) => n.props.to))],
    ["/agent/support"],
  );
  const page = text(r.root);
  assert.equal(page.includes("New agent"), false);
  // No config, cost or memory reads.
  assert.deepEqual(requested.map((u) => u.replace(/^.*\/api\//, "/api/")), [
    "/api/agents/available",
  ]);
});

test("an account with no agents is told so", async () => {
  setOidcSession(scoped);
  agentsBody = { agents: [] };
  const r = await render("/agents", createElement(AgentsList), "/agents");
  assert.ok(text(r.root).includes("No agents are available to your account."));
});

test("the chat route refuses an agent the account may not use, without opening a socket", async () => {
  setOidcSession(scoped);
  agentsBody = { agents: [{ alias: "support", enabled: true }] };
  const r = await render("/agent/secret", createElement(AgentChat), "/agent/:alias");
  const notice = r.root.findAll(
    (n) => typeof n.type === "string" && n.props["data-not-available"] === "not-available",
  );
  assert.equal(notice.length, 1);
  // A list that loaded is an answer: no load error or Retry.
  assert.equal(marked(r, "data-agents-load-error").length, 0);
  assert.deepEqual(requested.map((u) => u.replace(/^.*\/api\//, "/api/")), [
    "/api/agents/available",
  ]);
});

test("a disabled agent is refused the same way", async () => {
  setOidcSession(scoped);
  agentsBody = { agents: [{ alias: "triage", enabled: false }] };
  const r = await render("/agent/triage", createElement(AgentChat), "/agent/:alias");
  assert.equal(
    r.root.findAll((n) => typeof n.type === "string" && n.props["data-not-available"] !== undefined)
      .length,
    1,
  );
});

test("a failed agent list shows a load error with Retry, not the refusal", async () => {
  setOidcSession(scoped);
  agentsStatus = 503;
  agentsBody = { error: "temporarily unavailable" };
  const r = await render("/agent/support", createElement(AgentChat), "/agent/:alias");
  assert.equal(marked(r, "data-not-available").length, 0);
  const notices = marked(r, "data-agents-load-error");
  assert.equal(notices.length, 1);
  const [notice] = notices;
  assert.ok(notice);
  assert.equal(marked(r, "data-agents-retry").length, 1);
  assert.ok(text(notice).includes("Retry"));
});

test("Retry asks again and opens the chat once the list loads", async () => {
  setOidcSession(scoped);
  agentsStatus = 503;
  agentsBody = { error: "temporarily unavailable" };
  const r = await render("/agent/support", createElement(AgentChat), "/agent/:alias");
  assert.equal(marked(r, "data-agents-load-error").length, 1);
  assert.equal(r.root.findAllByType(ChatWorkspace).length, 0);

  agentsStatus = 200;
  agentsBody = { agents: [{ alias: "support", enabled: true }] };
  const [retry] = marked(r, "data-agents-retry");
  assert.ok(retry);
  await act(async () => {
    retry.props.onClick();
  });
  await settle();

  assert.equal(availableRequests().length, 2);
  assert.equal(marked(r, "data-agents-load-error").length, 0);
  assert.equal(marked(r, "data-not-available").length, 0);
  assert.equal(r.root.findAllByType(ChatWorkspace).length, 1);
});

test("a failed agents page load shows the error with Retry, not an empty list", async () => {
  setOidcSession(scoped);
  agentsStatus = 503;
  agentsBody = { error: "temporarily unavailable" };
  const r = await render("/agents", createElement(AgentsList), "/agents");
  assert.equal(text(r.root).includes("No agents are available to your account."), false);
  assert.equal(marked(r, "data-agents-load-error").length, 1);

  agentsStatus = 200;
  agentsBody = { agents: [{ alias: "support", enabled: true }] };
  const [retry] = marked(r, "data-agents-retry");
  assert.ok(retry);
  await act(async () => {
    retry.props.onClick();
  });
  await settle();

  assert.equal(availableRequests().length, 2);
  assert.equal(marked(r, "data-agents-load-error").length, 0);
  assert.deepEqual(
    marked(r, "data-available-agent").map((n) => n.props["data-available-agent"]),
    ["support"],
  );
});


test("the first scoped voice submission starts a conversation and preserves operator-only uploads", async () => {
  setOidcSession(scoped);
  agentsBody = { agents: [{ alias: "support", enabled: true }] };
  const { default: Voice } = await import("../components/BrowserVoiceControls.tsx");
  const r = await render("/agent/support", createElement(AgentChat), "/agent/:alias");
  assert.equal(FakeWebSocket.instances.length, 0);
  assert.equal(r.root.findAll((n) => n.type === "input" && n.props.type === "file").length, 0);
  await act(async () => { r.root.findByType(Voice).props.onTranscript("Synthetic voice request"); });
  await act(async () => { r.root.findByType(Voice).props.onSilence(); });
  await settle();
  assert.equal(FakeWebSocket.instances.length, 1);
  const socket = FakeWebSocket.instances[0]!;
  await act(async () => {
    socket.readyState = FakeWebSocket.OPEN;
    socket.onopen?.();
    socket.onmessage?.({ data: JSON.stringify({ type: "session_start", session_id: "synthetic-session" }) });
  });
  assert.deepEqual(socket.sent.map((data) => JSON.parse(data)).filter((frame) => frame.type === "message"), [
    { type: "message", content: "Synthetic voice request" },
  ]);
});
