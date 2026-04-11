import { appendFile, readFile, writeFile } from "node:fs/promises";
import { execFile } from "node:child_process";
import path from "node:path";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);

function resultsRoot() {
  return process.env.RUNTIME_RESULTS_ROOT || "/var/lib/clawpilot/results";
}

async function maybeReadJson(filePath) {
  try {
    const body = await readFile(filePath, "utf8");
    return JSON.parse(body);
  } catch (_error) {
    return null;
  }
}

async function readEvents(filePath) {
  try {
    const body = await readFile(filePath, "utf8");
    return body
      .split("\n")
      .map((line) => line.trim())
      .filter(Boolean)
      .map((line) => JSON.parse(line));
  } catch (_error) {
    return [];
  }
}

async function loadFileDiffs(state) {
  if (!state?.workspace_path || !Array.isArray(state?.file_changes) || state.file_changes.length === 0) {
    return {};
  }

  const diffs = {};
  await Promise.all(
    state.file_changes.slice(0, 25).map(async (filePath) => {
      try {
        const { stdout } = await execFileAsync(
          "git",
          ["-C", state.workspace_path, "diff", "--", filePath],
          { maxBuffer: 1024 * 1024 }
        );
        diffs[filePath] = stdout || "No diff output available.";
      } catch (_error) {
        diffs[filePath] = "Diff unavailable for this file.";
      }
    })
  );
  return diffs;
}

export async function GET(_request, { params }) {
  const { runId } = params;
  const root = resultsRoot();

  const resultFile = `${runId}.json`;
  const statusFile = `${runId}.status.json`;
  const eventsFile = `${runId}.events.jsonl`;

  const [state, result, events] = await Promise.all([
    maybeReadJson(path.join(root, statusFile)),
    maybeReadJson(path.join(root, resultFile)),
    readEvents(path.join(root, eventsFile))
  ]);

  if (!state && !result) {
    return Response.json({ error: "run not found" }, { status: 404 });
  }

  const fileDiffs = await loadFileDiffs(state);
  return Response.json({ state, result, events, fileDiffs });
}

export async function PATCH(request, { params }) {
  const { runId } = params;
  const root = resultsRoot();
  const statusPath = path.join(root, `${runId}.status.json`);
  const eventsPath = path.join(root, `${runId}.events.jsonl`);
  const state = await maybeReadJson(statusPath);
  if (!state) {
    return Response.json({ error: "run not found" }, { status: 404 });
  }

  const body = await request.json();
  const approvalId = String(body.approvalId || "").trim();
  const nextState = String(body.state || "").trim();
  const reviewerNote = String(body.reviewerNote || "").trim();

  if (!approvalId || !["approved", "rejected", "needs_input"].includes(nextState)) {
    return Response.json(
      { error: "approvalId and valid state (approved|rejected|needs_input) are required" },
      { status: 400 }
    );
  }

  const idx = Array.isArray(state.approvals)
    ? state.approvals.findIndex((item) => item.id === approvalId)
    : -1;
  if (idx < 0) {
    return Response.json({ error: "approval not found" }, { status: 404 });
  }

  const now = new Date().toISOString();
  state.approvals[idx] = {
    ...state.approvals[idx],
    state: nextState,
    reviewer_note: reviewerNote || null,
    updated_at: now,
    resolved_at: now
  };

  const hasPendingBlocking = state.approvals.some(
    (item) => item.requires_blocking && item.state === "pending_approval"
  );
  if (hasPendingBlocking) {
    state.status = "pending_approval";
    state.plan_state = "awaiting_approval";
    state.current_step = "review_required";
  } else if (state.approvals.some((item) => item.state === "rejected")) {
    state.status = "rejected";
    state.plan_state = "rejected";
  } else if (state.approvals.some((item) => item.state === "needs_input")) {
    state.status = "needs_input";
    state.plan_state = "needs_input";
  } else if (state.approvals.length > 0 && state.approvals.every((item) => item.state === "approved")) {
    state.status = "approved";
    state.plan_state = "approved";
    state.current_step = "review_complete";
  }

  await writeFile(statusPath, JSON.stringify(state, null, 2));

  const approval = state.approvals[idx] || {};
  const approvalEvent = {
    created_at: now,
    event_type: "approval_reviewed",
    message: `Approval ${approvalId} marked as ${nextState}`,
    payload: {
      approval_id: approvalId,
      status: nextState,
      reviewer_note: reviewerNote || null,
      title: approval.title || null,
      target_type: approval.target_type || null
    }
  };
  await appendFile(eventsPath, `${JSON.stringify(approvalEvent)}\n`);

  return Response.json({ ok: true, state });
}
