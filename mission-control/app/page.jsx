"use client";

import { useEffect, useMemo, useState } from "react";
import { useMutation, useQuery } from "convex/react";
import { api } from "@/convex/_generated/api";
import { ActivityPanel } from "@/components/activity-panel";
import { buildRunDeliverableInput } from "@/lib/deliverables";

const POLL_MS = 2000;

export default function Page() {
  const seed = useMutation(api.mission.seed);
  const dashboard = useQuery(api.mission.dashboard, {}) || {
    workspaces: [],
    activeWorkspace: null,
    goals: [],
    progress: [],
    artifacts: [],
    folderInstructions: [],
    deliverables: []
  };

  const createWorkspace = useMutation(api.mission.createWorkspace);
  const setActiveWorkspace = useMutation(api.mission.setActiveWorkspace);
  const upsertGlobalInstructions = useMutation(api.mission.upsertGlobalInstructions);
  const upsertFolderInstruction = useMutation(api.mission.upsertFolderInstruction);
  const createGoal = useMutation(api.mission.createGoal);
  const upsertRunDeliverable = useMutation(api.mission.upsertRunDeliverable);

  const [activeRun, setActiveRun] = useState(null);
  const [runState, setRunState] = useState(null);
  const [runEvents, setRunEvents] = useState([]);
  const [runResult, setRunResult] = useState(null);
  const [fileDiffs, setFileDiffs] = useState({});

  useEffect(() => {
    seed();
  }, [seed]);

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
        <h2>1) Choose workspace</h2>
        <WorkspacePicker
          workspaces={dashboard.workspaces}
          activeWorkspace={dashboard.activeWorkspace}
          onCreate={createWorkspace}
          onSetActive={setActiveWorkspace}
        />
      </section>

      <section className="panel" id="goal">
        <h2>2) Enter goal and create real run</h2>
        {dashboard.activeWorkspace ? (
          <RunComposer
            workspace={dashboard.activeWorkspace}
            folderInstructions={dashboard.folderInstructions}
            onCreateGoal={createGoal}
            onRunCreated={setActiveRun}
            goals={sortedGoals}
            draft={composerDraft}
            onDraftApplied={() => setComposerDraft("")}
          />
        ) : (
          <p>Create a workspace first.</p>
        )}
      </section>

      <section className="panel" id="runtime">
        <h2>3) Live runtime status</h2>
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

      <ActivityPanel />
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

function RunComposer({ workspace, folderInstructions, onCreateGoal, onRunCreated, goals, draft, onDraftApplied }) {
  const [goal, setGoal] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

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
      <form className="composer" onSubmit={submit}>
        <textarea
          placeholder="Describe the real outcome you want in this workspace"
          value={goal}
          onChange={(e) => setGoal(e.target.value)}
        />
        <button type="submit" disabled={busy}>{busy ? "Creating run..." : "Create runtime run"}</button>
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
    return <p>No run selected yet. Create a run to stream runtime-backed status.</p>;
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
        <strong>Tool events</strong>
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

      <article className="card">
        <strong>Changed files / artifacts</strong>
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

      <article className="card" id="review-workbench">
        <strong>Review workbench</strong>
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
                      placeholder="Optional reviewer note (reason, requested revision, etc.)"
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

      {result && (
        <article className="card">
          <strong>Result</strong>
          <p>Status: {result.status}</p>
          <p>{result.summary}</p>
        </article>
      )}
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
      <h2>Global Instructions</h2>
      <form className="composer" onSubmit={submit}>
        <textarea value={instructions} onChange={(e) => setInstructions(e.target.value)} />
        <button type="submit">Save global instructions</button>
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
      <h2>Folder Instructions</h2>
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
