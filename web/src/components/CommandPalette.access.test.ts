import assert from "node:assert/strict";
import test from "node:test";
import type { OidcSession } from "../lib/auth.ts";
import type { ConfigSearchItem } from "../lib/configSearch.ts";

// The palette's imports read `window` and `document` when they load, so the
// globals go in first and the modules load afterwards.
const location = {
  protocol: "https:",
  host: "zc.example",
  hostname: "zc.example",
  port: "",
  pathname: "/",
  search: "",
};
Object.assign(globalThis, {
  window: Object.assign(new EventTarget(), { location }),
  document: Object.assign(new EventTarget(), {
    hidden: false,
    activeElement: null,
    documentElement: { style: {} },
  }),
  location,
});

const { paletteItemsFor } = await import("./CommandPalette.tsx");
const { navAccessFor } = await import("../lib/navAccess.ts");

function session(features: OidcSession["features"]): OidcSession {
  return {
    kind: "oidc",
    provider: "oidc.corp",
    principal_id: "p",
    display_name: "",
    admin: false,
    expires_at: 2_000_000_000,
    csrf_token: "t",
    features,
  };
}

// What the config search loaded while the account could still read config.
// The loader caches it for the page's lifetime, so it outlives a grant change.
const loaded: ConfigSearchItem[] = [
  { label: "Providers", sublabel: "Foundation", url: "/config/providers", group: "Config section" },
  { label: "main", sublabel: "Agents", url: "/config/agents/main", group: "Config entry" },
];

const targets = (items: { to: string }[]) => items.map((item) => item.to);

test("config results loaded for a config reader drop out once the session loses config", () => {
  const reader = navAccessFor(session({ chat: true, operator_pages: false, config: true }));
  assert.deepEqual(targets(paletteItemsFor(reader, loaded)), [
    "/agents",
    "/config",
    "/config/agents",
    "/config/providers",
    "/config/agents/main",
  ]);

  // The grant is removed and the session re-read; the cached results remain.
  const revoked = navAccessFor(session({ chat: true, operator_pages: false, config: false }));
  assert.deepEqual(targets(paletteItemsFor(revoked, loaded)), ["/agents"]);
});

test("a paired browser keeps every page and every config result", () => {
  const items = targets(paletteItemsFor(navAccessFor(null), loaded));
  for (const to of ["/", "/config", "/acp-console", "/config/providers", "/config/agents/main"]) {
    assert.ok(items.includes(to), to);
  }
});
