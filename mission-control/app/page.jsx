"use client";

import { useEffect, useMemo, useState } from "react";
import { ActivityPanel } from "@/components/activity-panel";
import { buildRunDeliverableInput } from "@/lib/deliverables";
import { WORKFLOW_PRESETS, buildGoalFromPreset, getWorkflowPreset } from "@/lib/workflow-presets";

const POLL_MS = 2000;


const EMPTY_DASHBOARD = {
  workspaces: [],
  activeWorkspace: null,
  goals: [],
  progress: [],
  artifacts: [],
  folderInstructions: [],
  deliverables: []
};

function useMissionControlData() {
  const [dashboard, setDashboard] = useState(EMPTY_DASHBOARD);

  const refresh = async () => {
    const response = await fetch("/api/mission/dashboard", { cache: "no-store" });
    if (!response.ok) return;
    const payload = await response.json();
    setDashboard(payload || EMPTY_DASHBOARD);
  };

  useEffect(() => {
    refresh();
    const timer = setInterval(refresh, POLL_MS);
    return () => clearInterval(timer);
  }, []);

  const seed = async () => {
    await fetch("/api/mission/seed", { method: "POST" });
    await refresh();
  };

  const command = async (action, args) => {
    const response = await fetch("/api/mission/command", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ action, args })
    });
    const payload = await response.json();
    if (!response.ok) {
      throw new Error(payload.error || "Mission Control command failed");
    }
    await refresh();
    return payload.result;
  };

  return {
    dashboard,
    seed,
    createWorkspace: (args) => command("createWorkspace", args),
    setActiveWorkspace: (args) => command("setActiveWorkspace", args),
    upsertGlobalInstructions: (args) => command("upsertGlobalInstructions", args),
    upsertFolderInstruction: (args) => command("upsertFolderInstruction", args),
    createGoal: (args) => command("createGoal", args),
    upsertRunDeliverable: (args) => command("upsertRunDeliverable", args)
  };
}

export default function Page() {
  const {
    dashboard,
    createWorkspace,
    setActiveWorkspace,
    upsertGlobalInstructions,
    upsertFolderInstruction,
    createGoal,
    upsertRunDeliverable,
    seed
  } = useMissionControlData();

  const [activeRun, setActiveRun] = useState(null);
  const [runState, setRunState] = useState(null);
  const [runEvents, setRunEvents] = useState([]);
  const [runResult, setRunResult] = useState(null);
  const [fileDiffs, setFileDiffs] = useState({});
  const [persistedRunIds, setPersistedRunIds] = useState([]);
  const [composerDraft, setComposerDraft] = useState("");

  const activeRunId = activeRun?.runId || "";
  const outputLocation = useMemo(
    () => ({
      resultsRoot: "/var/lib/clawpilot/results",
      resultFile: activeRunId ? `${activeRunId}.json` : "",
      statusFile: activeRunId ? `${activeRunId}.status.json` : "",
      eventsFile: activeRunId ? `${activeRunId}.events.jsonl` : "",
      artifactsDir: dashboard.activeWorkspace?.rootPath || ""
    }),
    [activeRunId, dashboard.activeWorkspace?.rootPath]
  );

  useEffect(() => {
    seed();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    if (!activeRun?.runId) return;

    let cancelled = false;
    const load = async () => {
      const response = await fetch(`/api/runtime/runs/${activeRun.runId}`, { cache: "no-store" });
      if (!response.ok) return;
      const payload = await response.json();
      if (cancelled) return;
      setRunState(payload.state || null);
      setRunEvents(payload.events || []);
      setRunResult(payload.result || null);
      setFileDiffs(payload.fileDiffs || {});
    };

    load();
    const timer = setInterval(load, POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [activeRun?.runId]);

  useEffect(() => {
    const runId = activeRun?.runId;
    if (!runId || !dashboard.activeWorkspace) return;
    const finalStatus = String(runState?.status || "").toLowerCase();
    const terminal = finalStatus === "completed" || finalStatus === "failed" || Boolean(runResult);
    if (!terminal || persistedRunIds.includes(runId)) return;

    const persist = async () => {
      const deliverable = buildRunDeliverableInput({
        workspace: dashboard.activeWorkspace,
        runId,
        runState,
        runResult,
        runEvents,
        goalId: activeRun.goalId,
        goal: activeRun.goal,
        outputLocation
      });

      await upsertRunDeliverable(deliverable);
      setPersistedRunIds((current) => [...current, runId]);
    };

    persist();
  }, [
    activeRun,
    dashboard.activeWorkspace,
    outputLocation,
    persistedRunIds,
    runEvents,
    runResult,
    runState,
    upsertRunDeliverable
  ]);

  const sortedGoals = useMemo(
    () => [...dashboard.goals].sort((a, b) => b.createdAt - a.createdAt),
    [dashboard.goals]
  );

  const sortedDeliverables = useMemo(
    () => [...dashboard.deliverables].sort((a, b) => b.updatedAt - a.updatedAt),
    [dashboard.deliverables]
  );

  return (
    <main className="page">
      <header className="hero">
        <h1>Mission Control</h1>
        <p>Phase 4 deliverable-first runtime bridge.</p>
      </header>

      <section className="panel" id="workspace">
        <h2>1) Select workspace</h2>
        <WorkspacePicker
          workspaces={dashboard.workspaces}
          activeWorkspace={dashboard.activeWorkspace}
          onCreate={createWorkspace}
          onSetActive={setActiveWorkspace}
        />
      </section>

      <section className="panel" id="goal">
        <h2>2) Pick or type a goal</h2>
        {dashboard.activeWorkspace ? (
          <RunComposer
            workspace={dashboard.activeWorkspace}
            folderInstructions={dashboard.folderInstructions}
            onCreateGoal={createGoal}
            onRunCreated={setActiveRun}
            goals={sortedGoals}
            deliverables={sortedDeliverables}
            draft={composerDraft}
            onDraftApplied={() => setComposerDraft("")}
          />
        ) : (
          <p>Create a workspace first.</p>
        )}
      </section>

      <section className="panel" id="progress">
        <h2>3) Monitor progress</h2>
        <RunStatusPanel
          runId={activeRunId}
          state={runState}
          result={runResult}
          events={runEvents}
          fileDiffs={fileDiffs}
          onRefresh={async () => {
            const response = await fetch(`/api/runtime/runs/${activeRunId}`, { cache: "no-store" });
            if (!response.ok) return;
            const payload = await response.json();
            setRunState(payload.state || null);
            setRunEvents(payload.events || []);
            setRunResult(payload.result || null);
            setFileDiffs(payload.fileDiffs || {});
          }}
        />
      </section>

      <section className="panel" id="deliverables">
        <h2>5) Deliverables</h2>
        <DeliverablesPanel
          workspace={dashboard.activeWorkspace}
          deliverables={sortedDeliverables}
          onInspectRun={(runId) => setActiveRun((current) => ({ ...(current || {}), runId }))}
          onRefine={(draft) => setComposerDraft(draft)}
        />
      </section>

      <ActivityPanel />

      {dashboard.activeWorkspace && (
        <section className="panel two-col" id="instructions">
          <GlobalInstructionsEditor
            workspace={dashboard.activeWorkspace}
            onSave={upsertGlobalInstructions}
          />
          <FolderInstructionsEditor
            workspaceId={dashboard.activeWorkspace._id}
            items={dashboard.folderInstructions}
            onSave={upsertFolderInstruction}
          />
        </section>
      )}
    </main>
  );
}

function WorkspacePicker({ workspaces, activeWorkspace, onCreate, onSetActive }) {
  const [form, setForm] = useState({ name: "", slug: "", rootPath: "", description: "" });

  const submit = async (event) => {
    event.preventDefault();
    if (!form.name.trim() || !form.slug.trim() || !form.rootPath.trim()) return;
    await onCreate({
      name: form.name.trim(),
      slug: form.slug.trim(),
      rootPath: form.rootPath.trim(),
      description: form.description.trim() || undefined
    });
    setForm({ name: "", slug: "", rootPath: "", description: "" });
  };

  return (
    <div className="stack">
      <div className="workspace-list">
        {workspaces.map((workspace) => (
          <button
            key={workspace._id}
            className={`workspace-item ${activeWorkspace?._id === workspace._id ? "active" : ""}`}
            onClick={() => onSetActive({ workspaceId: workspace._id })}
            type="button"
          >
            <strong>{workspace.name}</strong>
            <small>{workspace.rootPath}</small>
          </button>
        ))}
      </div>

      <form className="composer" onSubmit={submit}>
        <input placeholder="Workspace name" value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} />
        <input placeholder="Workspace slug" value={form.slug} onChange={(e) => setForm({ ...form, slug: e.target.value })} />
        <input placeholder="Workspace path" value={form.rootPath} onChange={(e) => setForm({ ...form, rootPath: e.target.value })} />
        <input
          placeholder="Description (optional)"
          value={form.description}
          onChange={(e) => setForm({ ...form, description: e.target.value })}
        />
        <button type="submit">Add workspace</button>
      </form>
    </div>
  );
}

function RunComposer({ workspace, folderInstructions, onCreateGoal, onRunCreated, goals, deliverables, draft, onDraftApplied }) {
  const [goal, setGoal] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [presetId, setPresetId] = useState(WORKFLOW_PRESETS[0]?.id || "");
  const [targetPath, setTargetPath] = useState("");
  const [priorRunId, setPriorRunId] = useState("");

  const preset = getWorkflowPreset(presetId) || WORKFLOW_PRESETS[0];
  const priorRuns = useMemo(
    () => deliverables.map((item) => ({ id: item.runId, status: item.status })),
    [deliverables]
  );

  const applyPreset = () => {
    const drafted = buildGoalFromPreset({
      presetId,
      targetPath: targetPath.trim() || workspace.rootPath,
      runId: priorRunId
    });
    if (!drafted) return;
    setGoal(drafted);
  };

  useEffect(() => {
    if (!draft) return;
    setGoal(draft);
    onDraftApplied();
  }, [draft, onDraftApplied]);

  const submit = async (event) => {
    event.preventDefault();
    if (!goal.trim() || busy) return;

    setBusy(true);
    setError("");
    try {
      const goalText = goal.trim();
      const goalId = await onCreateGoal({ workspaceId: workspace._id, goal: goalText });
      const response = await fetch("/api/runtime/runs", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          goal: goalText,
          workspacePath: workspace.rootPath,
          globalInstructions: workspace.globalInstructions || "",
          folderInstructions
        })
      });

      if (!response.ok) {
        const payload = await response.json();
        throw new Error(payload.error || "Failed to create runtime run");
      }

      const payload = await response.json();
      onRunCreated({ runId: payload.runId, goalId, goal: goalText });
      setGoal("");
    } catch (runError) {
      setError(runError.message);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="stack">
      <article className="card">
        <strong>Workflow presets</strong>
        <p className="muted">Focused Phase 5 knowledge-work flows that run on today&apos;s runtime/tools.</p>
        <div className="stack">
          <select value={presetId} onChange={(event) => setPresetId(event.target.value)}>
            {WORKFLOW_PRESETS.map((item) => (
              <option key={item.id} value={item.id}>{item.value}</option>
            ))}
          </select>
          <p className="muted">{preset.description}</p>
          {preset.requiresTargetPath && (
            <input
              placeholder="Target folder or file path (optional, defaults to workspace root)"
              value={targetPath}
              onChange={(event) => setTargetPath(event.target.value)}
            />
          )}
          {preset.supportsPriorDeliverable && (
            <select value={priorRunId} onChange={(event) => setPriorRunId(event.target.value)}>
              <option value="">Select prior run (optional)</option>
              {priorRuns.map((run) => (
                <option key={run.id} value={run.id}>
                  {run.id} · {run.status || "unknown"}
                </option>
              ))}
            </select>
          )}
          <button type="button" onClick={applyPreset}>Draft goal from preset</button>
        </div>
      </article>

      <form className="composer" onSubmit={submit}>
        <textarea
          placeholder="Type your workspace goal or start from a workflow preset"
          value={goal}
          onChange={(e) => setGoal(e.target.value)}
        />
        <button type="submit" disabled={busy}>{busy ? "Creating run..." : "Start workspace run"}</button>
      </form>
      {error && <p className="muted">Error: {error}</p>}
      <div className="list">
        {goals.length === 0 && <p>No goals yet.</p>}
        {goals.map((item) => (
          <article key={item._id} className="card">
            <strong>{item.goal}</strong>
            <p className="muted">Status: {item.status}</p>
          </article>
        ))}
      </div>
    </div>
  );
}

function RunStatusPanel({ runId, state, events, result, fileDiffs, onRefresh }) {
  const [reviewNote, setReviewNote] = useState("");
  const [reviewBusy, setReviewBusy] = useState("");

  const pendingApprovals = (state?.approvals || []).filter((item) => item.state === "pending_approval");

  const updateApproval = async (approvalId, nextState) => {
    setReviewBusy(`${approvalId}:${nextState}`);
    try {
      await fetch(`/api/runtime/runs/${runId}`, {
        method: "PATCH",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          approvalId,
          state: nextState,
          reviewerNote: reviewNote
        })
      });
      setReviewNote("");
      await onRefresh();
    } finally {
      setReviewBusy("");
    }
  };

  if (!runId) {
    return <p>No run selected yet. Start a workspace run to monitor progress and approvals.</p>;
  }

  return (
    <div className="stack">
      <article className="card">
        <strong>Run ID: {runId}</strong>
        <p className="muted">Status: {state?.status || "pending"}</p>
        <p className="muted">Current step: {state?.current_step || "queued"}</p>
        <p className="muted">Plan state: {state?.plan_state || "queued"}</p>
      </article>

      <article className="card">
        <strong>Live tool activity</strong>
        {state?.tool_events?.length ? (
          <ul>
            {state.tool_events.slice(-10).map((event, index) => (
              <li key={`${event.tool}-${event.created_at}-${index}`}>
                {event.tool} · {event.success === null || event.success === undefined ? "started" : event.success ? "ok" : "error"}
              </li>
            ))}
          </ul>
        ) : (
          <p className="muted">No tool events yet.</p>
        )}
      </article>

      <article className="card" id="approvals">
        <strong>4) Review approvals</strong>
        <p className="muted">
          Pending approvals: {pendingApprovals.length} / {(state?.approvals || []).length}
        </p>
        {(state?.approvals || []).length ? (
          <div className="stack">
            {(state?.approvals || []).map((approval) => (
              <article className="card approval-item" key={approval.id}>
                <div className="row">
                  <strong>{approval.title}</strong>
                  <span className="badge">{approval.state}</span>
                </div>
                <p>{approval.summary}</p>
                {approval.target_type === "file_edit" && approval.metadata?.path && (
                  <details>
                    <summary>File diff preview: {approval.metadata.path}</summary>
                    <pre>{fileDiffs[approval.metadata.path] || "Loading diff..."}</pre>
                  </details>
                )}
                {(approval.target_type === "shell_command" || approval.target_type === "browser_action") && (
                  <details>
                    <summary>Command / action preview</summary>
                    <pre>{JSON.stringify(approval.metadata || {}, null, 2)}</pre>
                  </details>
                )}
                {approval.state === "pending_approval" && (
                  <div className="stack">
                    <textarea
                      placeholder="Optional reviewer note"
                      value={reviewNote}
                      onChange={(e) => setReviewNote(e.target.value)}
                    />
                    <div className="row">
                      <button
                        type="button"
                        disabled={Boolean(reviewBusy)}
                        onClick={() => updateApproval(approval.id, "approved")}
                      >
                        {reviewBusy === `${approval.id}:approved` ? "Approving..." : "Approve"}
                      </button>
                      <button
                        type="button"
                        disabled={Boolean(reviewBusy)}
                        onClick={() => updateApproval(approval.id, "rejected")}
                      >
                        {reviewBusy === `${approval.id}:rejected` ? "Rejecting..." : "Reject"}
                      </button>
                      <button
                        type="button"
                        disabled={Boolean(reviewBusy)}
                        onClick={() => updateApproval(approval.id, "needs_input")}
                      >
                        {reviewBusy === `${approval.id}:needs_input` ? "Saving..." : "Request revision"}
                      </button>
                    </div>
                  </div>
                )}
              </article>
            ))}
          </div>
        ) : (
          <p className="muted">No approvals tracked for this run yet.</p>
        )}
      </article>

      <article className="card" id="deliverable">
        <strong>5) Receive deliverable</strong>
        {result ? (
          <div className="stack">
            <p>Status: {result.status}</p>
            <p>{result.summary}</p>
          </div>
        ) : (
          <p className="muted">Deliverable not available yet.</p>
        )}
      </article>

      <article className="card">
        <strong>Changed files</strong>
        {state?.file_changes?.length ? (
          <ul>
            {state.file_changes.map((filePath) => (
              <li key={filePath}>{filePath}</li>
            ))}
          </ul>
        ) : (
          <p className="muted">No changed files detected yet.</p>
        )}
      </article>

      <article className="card">
        <strong>Runtime events</strong>
        {events.length ? (
          <ul>
            {events.slice(-20).map((event, index) => (
              <li key={`${event.created_at}-${index}`}>
                {event.created_at}: {event.event_type}
              </li>
            ))}
          </ul>
        ) : (
          <p className="muted">No runtime events yet.</p>
        )}
      </article>
    </div>
  );
}

function DeliverablesPanel({ workspace, deliverables, onInspectRun, onRefine }) {
  if (!workspace) {
    return <p>Create and select a workspace to view deliverables.</p>;
  }

  return (
    <div className="list">
      {deliverables.length === 0 && <p>No finalized deliverables yet. Complete a run to generate one.</p>}
      {deliverables.map((deliverable) => (
        <article className="card" key={deliverable._id}>
          <div className="row">
            <strong>{deliverable.goal || "Run deliverable"}</strong>
            <small className="badge">{deliverable.status}</small>
          </div>
          <p>{deliverable.summary}</p>
          <p className="muted">Run: {deliverable.runId}</p>

          <details>
            <summary>Generated files ({deliverable.changedFiles.length})</summary>
            <ul>
              {deliverable.changedFiles.map((filePath) => (
                <li key={filePath}>
                  {filePath}
                  <a className="inline-link" href={`file://${workspace.rootPath}/${filePath}`}>
                    Open file
                  </a>
                </li>
              ))}
            </ul>
          </details>

          <details>
            <summary>Artifacts and output location</summary>
            <p className="muted">Results root: {deliverable.outputLocation.resultsRoot}</p>
            <p className="muted">Result file: {deliverable.outputLocation.resultFile}</p>
            <p className="muted">Status file: {deliverable.outputLocation.statusFile}</p>
            <p className="muted">Events file: {deliverable.outputLocation.eventsFile}</p>
            <p className="muted">Artifacts dir: {deliverable.outputLocation.artifactsDir || workspace.rootPath}</p>
            {deliverable.artifacts.length > 0 && (
              <ul>
                {deliverable.artifacts.map((artifact) => (
                  <li key={`${artifact.path}-${artifact.status}`}>
                    {artifact.path} · {artifact.artifactType} · {artifact.status}
                  </li>
                ))}
              </ul>
            )}
          </details>

          <details>
            <summary>Approvals and review history ({deliverable.approvalHistory.length})</summary>
            {deliverable.approvalHistory.length ? (
              <ul>
                {deliverable.approvalHistory.map((entry, index) => (
                  <li key={`${entry.createdAt}-${index}`}>
                    {entry.createdAt}: {entry.eventType} · {entry.summary}
                    {entry.status ? ` (${entry.status})` : ""}
                  </li>
                ))}
              </ul>
            ) : (
              <p className="muted">No approval events captured for this run.</p>
            )}
          </details>

          <details>
            <summary>Suggested next steps</summary>
            <ul>
              {deliverable.suggestedNextSteps.map((step) => (
                <li key={step}>{step}</li>
              ))}
            </ul>
          </details>

          <div className="row">
            <button type="button" onClick={() => onInspectRun(deliverable.runId)}>Inspect run</button>
            <button type="button" onClick={() => onRefine(`Refine previous run outcome: ${deliverable.goal}. Context: ${deliverable.summary}`)}>
              Rerun / refine
            </button>
          </div>
        </article>
      ))}
    </div>
  );
}

function GlobalInstructionsEditor({ workspace, onSave }) {
  const [instructions, setInstructions] = useState(workspace.globalInstructions || "");

  useEffect(() => {
    setInstructions(workspace.globalInstructions || "");
  }, [workspace._id, workspace.globalInstructions]);

  const submit = async (event) => {
    event.preventDefault();
    await onSave({ workspaceId: workspace._id, instructions });
  };

  return (
    <div>
      <h2>Workspace instructions</h2>
      <form className="composer" onSubmit={submit}>
        <textarea value={instructions} onChange={(e) => setInstructions(e.target.value)} />
        <button type="submit">Save workspace instructions</button>
      </form>
    </div>
  );
}

function FolderInstructionsEditor({ workspaceId, items, onSave }) {
  const [folderPath, setFolderPath] = useState("");
  const [instructions, setInstructions] = useState("");

  const submit = async (event) => {
    event.preventDefault();
    if (!folderPath.trim() || !instructions.trim()) return;
    await onSave({
      workspaceId,
      folderPath: folderPath.trim(),
      instructions: instructions.trim()
    });
    setFolderPath("");
    setInstructions("");
  };

  return (
    <div>
      <h2>Folder instructions</h2>
      <form className="composer" onSubmit={submit}>
        <input placeholder="Folder path (e.g. src/runtime)" value={folderPath} onChange={(e) => setFolderPath(e.target.value)} />
        <textarea placeholder="Instructions for this folder" value={instructions} onChange={(e) => setInstructions(e.target.value)} />
        <button type="submit">Add folder instruction</button>
      </form>
      <div className="list">
        {items.length === 0 && <p>No folder instructions yet.</p>}
        {items.map((item) => (
          <article className="card" key={item._id}>
            <strong>{item.folderPath}</strong>
            <p>{item.instructions}</p>
          </article>
        ))}
      </div>
    </div>
  );
}
