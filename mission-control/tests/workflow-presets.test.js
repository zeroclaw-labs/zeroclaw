import test from "node:test";
import assert from "node:assert/strict";
import { WORKFLOW_PRESETS, buildGoalFromPreset, getWorkflowPreset } from "../lib/workflow-presets.js";

test("workflow presets include phase 5 priority workflows", () => {
  const ids = WORKFLOW_PRESETS.map((preset) => preset.id);
  assert.deepEqual(ids, [
    "folder-summarization",
    "file-organization",
    "document-synthesis",
    "data-extraction",
    "rerun-refine"
  ]);
});

test("buildGoalFromPreset injects target path", () => {
  const goal = buildGoalFromPreset({
    presetId: "folder-summarization",
    targetPath: "src/runtime"
  });
  assert.match(goal, /src\/runtime/);
});

test("buildGoalFromPreset injects run id for rerun workflow", () => {
  const goal = buildGoalFromPreset({
    presetId: "rerun-refine",
    runId: "abc-123"
  });
  assert.match(goal, /abc-123/);
});

test("unknown preset returns null", () => {
  assert.equal(getWorkflowPreset("unknown"), null);
  assert.equal(buildGoalFromPreset({ presetId: "unknown" }), null);
});
