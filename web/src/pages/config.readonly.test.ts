import assert from "node:assert/strict";
import test, { afterEach, beforeEach } from "node:test";
import React, { createElement } from "react";
import { act, create, type ReactTestInstance, type ReactTestRenderer } from "react-test-renderer";

// The config explorer for a signed-in account with only Config:Read. The
// gateway refuses every config POST for it, so opening an existing entry must
// not send one: browsing works in both alias tiers, and creating an entry
// still sends its write and shows the refusal.

const location = { protocol: "https:", host: "zc.example", hostname: "zc.example", port: "", pathname: "/config", search: "" };
const fakeWindow = Object.assign(new EventTarget(), {
  location,
  setTimeout: (fn: () => void, ms: number) => setTimeout(fn, ms),
  clearTimeout: (id: ReturnType<typeof setTimeout>) => clearTimeout(id),
});
// The page's editors pull in CodeMirror, which reads the root element's style
// when it is imported.
const fakeDocument = Object.assign(new EventTarget(), {
  hidden: false,
  activeElement: null,
  documentElement: { style: {} },
});
Object.assign(globalThis, { window: fakeWindow, document: fakeDocument, location, React });
(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

const REFUSAL = "Principal lacks the config grant required for this method";

function section(key: string, shape: "typed_family_map" | "one_tier_alias_map") {
  return {
    key,
    label: key,
    help: "",
    has_picker: true,
    completed: true,
    ready: true,
    group: "Foundation",
    is_quickstart: false,
    shape,
    cost_category: "",
  };
}

const MAP_KEYS: Record<string, string[]> = {
  channels: ["telegram"],
  "channels.telegram": ["main"],
  agents: ["default"],
};

interface Sent {
  method: string;
  path: string;
  body: string | null;
}
let sent: Sent[] = [];

function respond(path: string, method: string): Response {
  const json = (status: number, body: unknown) =>
    new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
  // The gateway's config route gate: a read-only session may not POST.
  if (method !== "GET") return json(403, { error: REFUSAL });
  const url = new URL(path, "https://zc.example");
  switch (url.pathname) {
    case "/api/config/sections":
      return json(200, {
        sections: [section("channels", "typed_family_map"), section("agents", "one_tier_alias_map")],
      });
    case "/api/config/sections/channels":
      return json(200, {
        section: "channels",
        help: "",
        items: [{ key: "telegram", label: "Telegram", badge: "configured" }],
      });
    case "/api/config/sections/agents":
      return json(200, {
        section: "agents",
        help: "",
        items: [{ key: "default", label: "default", badge: "configured" }],
      });
    case "/api/config/map-keys": {
      const mapPath = url.searchParams.get("path") ?? "";
      return json(200, { path: mapPath, keys: MAP_KEYS[mapPath] ?? [] });
    }
    case "/api/config/list":
      return json(200, { entries: [] });
    case "/api/config/drift":
      return json(200, { drifted: [] });
    default:
      return json(200, {});
  }
}

globalThis.fetch = async (input, init) => {
  const path = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
  const method = (init?.method ?? "GET").toUpperCase();
  sent.push({ method, path, body: typeof init?.body === "string" ? init.body : null });
  return respond(path, method);
};

const auth = await import("../lib/auth.ts");
const { default: Config } = await import("./Config.tsx");
const { MemoryRouter, Route, Routes, useLocation } = await import("react-router-dom");

let here = "";
function Where() {
  here = useLocation().pathname;
  return null;
}

let renderer: ReactTestRenderer | null = null;

async function settle(): Promise<void> {
  for (let i = 0; i < 10; i += 1) {
    await act(async () => {
      await new Promise<void>((resolve) => setTimeout(resolve, 0));
    });
  }
}

async function mountAt(path: string): Promise<ReactTestInstance> {
  await act(async () => {
    renderer = create(
      createElement(
        MemoryRouter,
        { initialEntries: [path] },
        createElement(Where),
        createElement(
          Routes,
          null,
          createElement(Route, { path: "/config/:section", element: createElement(Config) }),
          createElement(Route, { path: "/config/:section/:type", element: createElement(Config) }),
          createElement(Route, { path: "/config/:section/:type/:alias", element: createElement(Config) }),
        ),
      ),
    );
  });
  await settle();
  assert.ok(renderer);
  return renderer.root;
}

function textOf(node: ReactTestInstance | string): string {
  return typeof node === "string" ? node : node.children.map(textOf).join("");
}

function component(root: ReactTestInstance, name: string, props: Record<string, unknown> = {}): ReactTestInstance {
  const found = root.findAll(
    (n) =>
      typeof n.type === "function" &&
      n.type.name === name &&
      Object.entries(props).every(([key, value]) => n.props[key] === value),
  );
  assert.equal(found.length, 1, `exactly one ${name} ${JSON.stringify(props)}`);
  return found[0]!;
}

async function click(button: ReactTestInstance): Promise<void> {
  await act(async () => {
    button.props.onClick();
  });
  await settle();
}

async function openAliasRow(root: ReactTestInstance, alias: string): Promise<void> {
  await click(component(root, "AliasRow", { alias }).findAllByType("button")[0]!);
}

async function addAlias(root: ReactTestInstance, alias: string): Promise<void> {
  const input = component(root, "AliasListView").findByType("input");
  await act(async () => {
    input.props.onChange({ target: { value: alias } });
  });
  await act(async () => {
    component(root, "AliasListView").findByType("input").props.onKeyDown({ key: "Enter" });
  });
  await settle();
}

const writes = () => sent.filter((request) => request.method !== "GET");

beforeEach(() => {
  sent = [];
  here = "";
  auth.setOidcSession({
    kind: "oidc",
    provider: "oidc.corp",
    principal_id: "principal-reader",
    display_name: "Reader",
    admin: false,
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
  auth.setOidcSession(null);
});

test("a read-only account opens an existing channel from the overview without a write", async () => {
  const root = await mountAt("/config/channels");
  const telegram = component(root, "ConfiguredOnlyPicker")
    .findAllByType("button")
    .find((button) => textOf(button).includes("Telegram"));
  assert.ok(telegram, "the overview lists the configured channel type");
  await click(telegram);
  assert.equal(here, "/config/channels/telegram");

  await openAliasRow(component(renderer!.root, "AliasListView"), "main");
  assert.deepEqual(writes(), []);
  assert.equal(textOf(renderer!.root).includes(REFUSAL), false);
  assert.equal(here, "/config/channels/telegram/main");
});

test("a read-only account opens an existing agent (one-tier alias) without a write", async () => {
  const root = await mountAt("/config/agents");
  await openAliasRow(component(root, "AliasListView"), "default");
  assert.deepEqual(writes(), []);
  assert.equal(textOf(renderer!.root).includes(REFUSAL), false);
  assert.equal(here, "/config/agents/default");
});

test("creating a channel alias still sends its write, and the refusal is shown", async () => {
  const root = await mountAt("/config/channels/telegram");
  await addAlias(root, "backup");
  assert.deepEqual(writes().map(({ method, path, body }) => ({ method, path, body })), [
    {
      method: "POST",
      path: "/api/config/sections/channels/items/telegram",
      body: JSON.stringify({ alias: "backup" }),
    },
  ]);
  assert.equal(here, "/config/channels/telegram");
  assert.ok(textOf(renderer!.root).includes(REFUSAL), "the gateway's refusal is shown");
});

test("creating an agent still sends its write, and the refusal is shown", async () => {
  const root = await mountAt("/config/agents");
  await addAlias(root, "helper");
  assert.deepEqual(writes().map(({ method, path }) => ({ method, path })), [
    { method: "POST", path: "/api/config/sections/agents/items/helper" },
  ]);
  assert.equal(here, "/config/agents");
  assert.ok(textOf(renderer!.root).includes(REFUSAL), "the gateway's refusal is shown");
});
