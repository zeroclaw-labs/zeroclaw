import assert from "node:assert/strict";
import test from "node:test";

import {
  catalogCapabilities,
  catalogDescription,
  matchesCatalogFilter,
} from "./pluginCatalog.ts";

const packageWithBothSources = {
  name: "calendar",
  installed: {
    version: "0.1.0",
    description: "Installed calendar",
    capabilities: ["tool", "channel"],
    permissions: ["config_read"],
  },
  available: {
    version: "0.2.0",
    description: "Registry calendar",
    capabilities: ["channel", "skill"],
    install_source: "calendar@0.2.0",
  },
};

test("catalog capabilities are a sorted per-call union", () => {
  assert.deepEqual(catalogCapabilities(packageWithBothSources), [
    "channel",
    "skill",
    "tool",
  ]);
});

test("installed metadata takes display precedence without losing registry data", () => {
  assert.equal(
    catalogDescription(packageWithBothSources),
    "Installed calendar",
  );
  assert.equal(packageWithBothSources.available.version, "0.2.0");
});

test("source filters reflect the two canonical records", () => {
  assert.equal(matchesCatalogFilter(packageWithBothSources, "all"), true);
  assert.equal(matchesCatalogFilter(packageWithBothSources, "installed"), true);
  assert.equal(matchesCatalogFilter(packageWithBothSources, "available"), true);

  const registryOnly = { ...packageWithBothSources, installed: null };
  assert.equal(matchesCatalogFilter(registryOnly, "installed"), false);
  assert.equal(matchesCatalogFilter(registryOnly, "available"), true);
});
