import assert from "node:assert/strict";
import test from "node:test";
import type { OidcSession } from "./auth.ts";
import { FULL_ACCESS, homePathFor, navAccessFor, pathAccess } from "./navAccess.ts";

function session(features: OidcSession["features"], admin = features.operator_pages): OidcSession {
  return {
    kind: "oidc",
    provider: "oidc.corp",
    principal_id: "p",
    display_name: "",
    admin,
    expires_at: 2_000_000_000,
    csrf_token: "t",
    features,
  };
}

const operatorPaths = [
  "/",
  "/agents",
  "/agent/main",
  "/agent/main/workspace",
  "/config",
  "/config/agents/main",
  "/tools",
  "/skills",
  "/sops/new",
  "/runs/a/1",
  "/integrations",
  "/plugins",
  "/cron",
  "/logs",
  "/doctor",
  "/pairing",
  "/canvas",
  "/quickstart",
];

test("a paired browser reaches every page, as before", () => {
  const access = navAccessFor(null);
  assert.deepEqual(access, FULL_ACCESS);
  for (const path of [...operatorPaths, "/acp-console"]) {
    assert.equal(pathAccess(path, access), "allowed", path);
  }
  assert.equal(homePathFor(access), "/");
});

test("an administrator reaches every page except the pairing-only ACP console", () => {
  const access = navAccessFor(session({ chat: true, operator_pages: true }));
  for (const path of operatorPaths) {
    assert.equal(pathAccess(path, access), "allowed", path);
  }
  assert.equal(pathAccess("/acp-console", access), "needs-pairing");
  assert.equal(homePathFor(access), "/");
});

test("a scoped account without chat: config only", () => {
  const access = navAccessFor(session({ chat: false, operator_pages: false }));
  for (const path of ["/config", "/config/providers", "/config/agents/main", "/setup/workspace"]) {
    assert.equal(pathAccess(path, access), "allowed", path);
  }
  for (const path of [
    "/",
    "/agents",
    "/agent/main",
    "/tools",
    "/logs",
    "/pairing",
    "/quickstart",
    "/configx",
  ]) {
    assert.equal(pathAccess(path, access), "not-available", path);
  }
  assert.equal(pathAccess("/acp-console", access), "needs-pairing");
  assert.equal(homePathFor(access), "/config");
});

test("a scoped account with chat: chat and config, not an agent's workspace", () => {
  const access = navAccessFor(session({ chat: true, operator_pages: false }));
  for (const path of ["/agents", "/agent", "/agent/main", "/AGENT/Main/", "/config"]) {
    assert.equal(pathAccess(path, access), "allowed", path);
  }
  for (const path of ["/agent/main/workspace", "/agents/x", "/", "/cron"]) {
    assert.equal(pathAccess(path, access), "not-available", path);
  }
  assert.equal(homePathFor(access), "/agents");
});

test("missing feature flags deny rather than allow", () => {
  // parseOidcSession maps absent flags to false; an all-false session sees config only.
  const access = navAccessFor(session({ chat: false, operator_pages: false }, true));
  assert.equal(pathAccess("/tools", access), "not-available");
});
