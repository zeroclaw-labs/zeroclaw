const DEFAULT_RESULTS_ROOT = "/var/lib/clawpilot/results";

export function buildRunDeliverableInput({
  workspace,
  runId,
  runState,
  runResult,
  runEvents,
  goalId,
  goal,
  outputLocation
}) {
  const changedFiles = normalizeFileList(runState?.file_changes || runState?.fileChanges || []);
  const artifacts = normalizeArtifacts(runState?.artifacts || []);
  const approvals = extractApprovalHistory(runEvents || []);
  const status = normalizeStatus(runState?.status, runResult?.status);
  const summary =
    String(runResult?.summary || "").trim() ||
    buildFallbackSummary({ status, changedFiles, artifactsCount: artifacts.length });

  return {
    workspaceId: workspace._id,
    runId,
    goalId,
    goal: String(goal || runState?.goal || "").trim(),
    status,
    summary,
    changedFiles,
    artifacts,
    outputLocation: {
      resultsRoot: outputLocation?.resultsRoot || DEFAULT_RESULTS_ROOT,
      resultFile: outputLocation?.resultFile || `${runId}.json`,
      statusFile: outputLocation?.statusFile || `${runId}.status.json`,
      eventsFile: outputLocation?.eventsFile || `${runId}.events.jsonl`,
      artifactsDir: outputLocation?.artifactsDir || workspace.rootPath
    },
    suggestedNextSteps: normalizeNextSteps(runResult, status),
    approvalHistory: approvals
  };
}

export function extractApprovalHistory(events) {
  return events
    .filter((event) => {
      const eventType = String(event?.event_type || "").toLowerCase();
      const message = String(event?.message || "").toLowerCase();
      return eventType.includes("approval") || eventType.includes("review") || message.includes("approval") || message.includes("review");
    })
    .map((event) => ({
      createdAt: String(event.created_at || ""),
      eventType: String(event.event_type || "unknown"),
      summary: String(event.message || "approval-related event"),
      status: normalizeApprovalStatus(event)
    }));
}

function normalizeFileList(value) {
  return Array.isArray(value)
    ? value.map((item) => String(item || "").trim()).filter(Boolean)
    : [];
}

function normalizeArtifacts(value) {
  if (!Array.isArray(value)) return [];
  return value
    .map((artifact) => ({
      path: String(artifact?.path || "").trim(),
      artifactType: String(artifact?.artifact_type || artifact?.artifactType || "artifact").trim(),
      status: String(artifact?.status || "created").trim()
    }))
    .filter((artifact) => artifact.path);
}

function normalizeStatus(stateStatus, resultStatus) {
  const combined = String(stateStatus || resultStatus || "").toLowerCase();
  if (combined.includes("fail") || combined === "error") return "failed";
  if (combined.includes("complete") || combined === "ok" || combined === "done") return "completed";
  return "running";
}

function buildFallbackSummary({ status, changedFiles, artifactsCount }) {
  if (status === "failed") {
    return "Run ended with errors. Inspect events and artifacts before retrying.";
  }
  if (changedFiles.length || artifactsCount) {
    return `Run completed with ${changedFiles.length} changed files and ${artifactsCount} artifacts.`;
  }
  return "Run completed without recorded file changes.";
}

function normalizeNextSteps(runResult, status) {
  const explicit = runResult?.suggested_next_steps || runResult?.suggestedNextSteps;
  if (Array.isArray(explicit) && explicit.length) {
    return explicit.map((step) => String(step)).filter(Boolean);
  }

  if (status === "failed") {
    return ["Review approval and runtime events", "Refine the task prompt", "Rerun in the same workspace"];
  }

  return ["Inspect generated files", "Review artifact output path", "Refine and rerun if additional changes are needed"];
}

function normalizeApprovalStatus(event) {
  const payloadStatus = event?.payload?.status || event?.payload?.approval_status;
  if (payloadStatus) return String(payloadStatus);

  const message = String(event?.message || "").toLowerCase();
  if (message.includes("approved")) return "approved";
  if (message.includes("rejected") || message.includes("denied")) return "rejected";
  if (message.includes("required") || message.includes("pending")) return "pending";
  return undefined;
}
