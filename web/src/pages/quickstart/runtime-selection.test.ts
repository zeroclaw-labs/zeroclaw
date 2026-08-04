import assert from "node:assert/strict";
import test from "node:test";

import {
  requiredQuickstartSelectionsComplete,
  runtimeAfterProviderChange,
  runtimeDefaultForProvider,
  runtimeValueForSubmit,
} from "./runtime-selection.ts";

const state = {
  default_runtime_profile: "balanced",
  model_provider_types: [
    { kind: "ollama", default_runtime_profile: "local_small" },
    { kind: "openai", default_runtime_profile: null },
  ],
};

test("runtimeDefaultForProvider prefers the provider recommendation", () => {
  assert.equal(runtimeDefaultForProvider(state, "ollama"), "local_small");
  assert.equal(runtimeDefaultForProvider(state, "openai"), "balanced");
  assert.equal(runtimeDefaultForProvider(state), "balanced");
});

test("runtimeAfterProviderChange updates only an auto-defaulted selection", () => {
  assert.deepEqual(
    runtimeAfterProviderChange(state, "ollama", { preset_name: "balanced" }, true),
    { preset_name: "local_small" },
  );
  assert.deepEqual(
    runtimeAfterProviderChange(state, "ollama", { preset_name: "unbounded" }, false),
    { preset_name: "unbounded" },
  );
});

test("runtimeValueForSubmit requires a runtime selection", () => {
  assert.equal(runtimeValueForSubmit(null), null);
  assert.equal(runtimeValueForSubmit({ preset_name: "local_small" }), "local_small");
});

test("required selections reject a missing runtime", () => {
  const complete = {
    provider: {},
    risk: { preset_name: "balanced" },
    runtime: { preset_name: "local_small" },
    memory: { preset_name: "sqlite" },
    agentName: "local-agent",
  };

  assert.equal(requiredQuickstartSelectionsComplete(complete), true);
  assert.equal(
    requiredQuickstartSelectionsComplete({ ...complete, runtime: null }),
    false,
  );
});
