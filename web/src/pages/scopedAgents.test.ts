import assert from "node:assert/strict";
import test, { afterEach } from "node:test";
import React, { createElement } from "react";
import { act, create, type ReactTestInstance, type ReactTestRenderer } from "react-test-renderer";
import type { OidcSession } from "../lib/auth.ts";

// The agent pages for a signed-in account without the operator dashboard: the
// agents list comes from /api/agents/available, and the chat route refuses an
// agent the account may not use before any socket opens.

const fakeWindow = Object.assign(new EventTarget(), {
  location: { protocol: "https:", host: "zc.example", hostname: "zc.example", port: "", pathname: "/", search: "" },
});
Object.assign(globalThis, { window: fakeWindow, React });
(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

const requested: string[] = [];
let agentsBody: unknown = { agents: [] };
globalThis.fetch = async (input) => {
  const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
  requested.push(url);
  if (url.endsWith("/api/agents/available")) {
    return new Response(JSON.stringify(agentsBody), {
      status: 200,
      headers: { "Content-Type": "application/json" },
    });
  }
  return new Response(JSON.stringify({ error: "no", reason: "operator_required" }), { status: 403 });
};

const { setOidcSession } = await import("../lib/auth.ts");
const { default: AgentsList } = await import("./AgentsList.tsx");
const { default: AgentChat } = await import("./AgentChat.tsx");
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
