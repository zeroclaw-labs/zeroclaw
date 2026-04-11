import test from "node:test";
import assert from "node:assert/strict";
import { buildRunDeliverableInput, extractApprovalHistory } from "./deliverables.js";

test("extractApprovalHistory captures approval-related events", () => {
  const history = extractApprovalHistory([
    { event_type: "tool_start", message: "Tool execution started", created_at: "2026-01-01T00:00:00Z" },
    {
      event_type: "approval_required",
      message: "Approval required before shell command",
      created_at: "2026-01-01T00:00:01Z",
      payload: { status: "pending" }
    },
    {
      event_type: "review_completed",
      message: "Operator approved execution",
      created_at: "2026-01-01T00:00:02Z"
    }
  ]);

  assert.equal(history.length, 2);
  assert.equal(history[0].status, "pending");
  assert.equal(history[1].status, "approved");
});

test("buildRunDeliverableInput binds workspace, run, artifacts, and next steps", () => {
  const payload = buildRunDeliverableInput({
    workspace: { _id: "workspace_1", rootPath: "/workspace/Clawpilot" },
    runId: "run_123",
    goalId: "goal_321",
    goal: "Produce release notes",
    runState: {
      status: "Completed",
      file_changes: ["mission-control/app/page.jsx"],
      artifacts: [{ path: "mission-control/out/report.md", artifact_type: "artifact", status: "created" }]
    },
    runResult: { summary: "Run completed", status: "ok" },
    runEvents: [{ event_type: "approval_required", message: "Approval required", created_at: "2026-01-01T00:00:01Z" }],
    outputLocation: {
      resultsRoot: "/tmp/results",
      resultFile: "run_123.json",
      statusFile: "run_123.status.json",
      eventsFile: "run_123.events.jsonl",
      artifactsDir: "/workspace/Clawpilot/out"
    }
  });

  assert.equal(payload.workspaceId, "workspace_1");
  assert.equal(payload.runId, "run_123");
  assert.equal(payload.goalId, "goal_321");
  assert.equal(payload.artifacts.length, 1);
  assert.equal(payload.changedFiles[0], "mission-control/app/page.jsx");
  assert.equal(payload.outputLocation.artifactsDir, "/workspace/Clawpilot/out");
  assert.equal(payload.approvalHistory.length, 1);
  assert.equal(payload.suggestedNextSteps.length > 0, true);
});
