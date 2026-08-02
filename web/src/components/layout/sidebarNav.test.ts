import assert from "node:assert/strict";
import test from "node:test";

import { findActiveNavPath } from "./sidebarNav.ts";

const navPaths = [
  "/",
  "/agents",
  "/config",
  "/config/agents",
  "/tools",
  "/skills",
  "/sops",
  "/runs",
  "/integrations",
  "/cron",
  "/logs",
  "/pairing",
  "/doctor",
  "/canvas",
  "/acp-console",
];

test("specific config destinations win over their parent", () => {
  assert.equal(findActiveNavPath("/config", navPaths), "/config");
  assert.equal(findActiveNavPath("/config/providers", navPaths), "/config");
  assert.equal(findActiveNavPath("/config/agents", navPaths), "/config/agents");
  assert.equal(
    findActiveNavPath("/config/agents/zeroclaw_agent", navPaths),
    "/config/agents",
  );
});

test("every top-level sidebar destination selects itself", () => {
  for (const navPath of navPaths) {
    assert.equal(findActiveNavPath(navPath, navPaths), navPath);
  }
});

test("path matching respects segment boundaries", () => {
  assert.equal(findActiveNavPath("/config/agents-old", navPaths), "/config");
  assert.equal(findActiveNavPath("/agents-old", navPaths), null);
  assert.equal(findActiveNavPath("/unknown", navPaths), null);
});

test("nested route families keep their owning sidebar destination active", () => {
  assert.equal(findActiveNavPath("/", navPaths), "/");
  assert.equal(findActiveNavPath("/agents", navPaths), "/agents");
  assert.equal(findActiveNavPath("/sops/new", navPaths), "/sops");
  assert.equal(findActiveNavPath("/sops/example/edit", navPaths), "/sops");
  assert.equal(findActiveNavPath("/runs/example/123", navPaths), "/runs");
  assert.equal(findActiveNavPath("/ACp-Console", navPaths), "/acp-console");
});
