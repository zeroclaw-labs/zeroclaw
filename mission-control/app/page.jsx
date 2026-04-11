"use client";

import { useEffect, useMemo, useState } from "react";
import { useMutation, useQuery } from "convex/react";
import { api } from "@/convex/_generated/api";
import { ActivityPanel } from "@/components/activity-panel";

const progressKinds = ["info", "update", "warning", "complete"];
const artifactTypes = ["changed_file", "artifact"];
const artifactStatuses = ["created", "updated", "deleted"];

export default function Page() {
  const seed = useMutation(api.mission.seed);
  const dashboard = useQuery(api.mission.dashboard, {}) || {
    workspaces: [],
    activeWorkspace: null,
    goals: [],
    progress: [],
    artifacts: [],
    folderInstructions: []
  };

  const createWorkspace = useMutation(api.mission.createWorkspace);
  const setActiveWorkspace = useMutation(api.mission.setActiveWorkspace);
  const upsertGlobalInstructions = useMutation(api.mission.upsertGlobalInstructions);
  const upsertFolderInstruction = useMutation(api.mission.upsertFolderInstruction);
  const createGoal = useMutation(api.mission.createGoal);
  const createProgressEntry = useMutation(api.mission.createProgressEntry);
  const createArtifact = useMutation(api.mission.createArtifact);

  useEffect(() => {
    seed();
  }, [seed]);

  const sortedProgress = useMemo(
    () => [...dashboard.progress].sort((a, b) => b.createdAt - a.createdAt),
    [dashboard.progress]
  );

  const sortedArtifacts = useMemo(
    () => [...dashboard.artifacts].sort((a, b) => b.createdAt - a.createdAt),
    [dashboard.artifacts]
  );

  return (
    <main className="page">
      <header className="hero">
        <h1>Mission Control</h1>
        <p>Project Workspace-centered planning (Phase 1).</p>
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
        <h2>2) Enter goal</h2>
        {dashboard.activeWorkspace ? (
          <GoalComposer workspaceId={dashboard.activeWorkspace._id} onCreate={createGoal} goals={dashboard.goals} />
        ) : (
          <p>Create a workspace first.</p>
        )}
      </section>

      <section className="panel" id="progress">
        <h2>3) Progress area</h2>
        {dashboard.activeWorkspace ? (
          <ProgressPanel
            workspaceId={dashboard.activeWorkspace._id}
            goals={dashboard.goals}
            progress={sortedProgress}
            onCreate={createProgressEntry}
          />
        ) : (
          <p>Select a workspace to track progress.</p>
        )}
      </section>

      <section className="panel" id="artifacts">
        <h2>4) Changed files / artifacts area</h2>
        {dashboard.activeWorkspace ? (
          <ArtifactsPanel
            workspaceId={dashboard.activeWorkspace._id}
            artifacts={sortedArtifacts}
            onCreate={createArtifact}
          />
        ) : (
          <p>Select a workspace to track changed files and artifacts.</p>
        )}
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

function GoalComposer({ workspaceId, onCreate, goals }) {
  const [goal, setGoal] = useState("");

  const submit = async (event) => {
    event.preventDefault();
    if (!goal.trim()) return;
    await onCreate({ workspaceId, goal: goal.trim() });
    setGoal("");
  };

  return (
    <div className="stack">
      <form className="composer" onSubmit={submit}>
        <textarea
          placeholder="Describe the outcome you want in this workspace"
          value={goal}
          onChange={(e) => setGoal(e.target.value)}
        />
        <button type="submit">Capture goal</button>
      </form>
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

function ProgressPanel({ workspaceId, goals, progress, onCreate }) {
  const [form, setForm] = useState({ goalId: "", title: "", detail: "", kind: "update" });

  const submit = async (event) => {
    event.preventDefault();
    if (!form.title.trim()) return;
    await onCreate({
      workspaceId,
      goalId: form.goalId || undefined,
      title: form.title.trim(),
      detail: form.detail.trim() || undefined,
      kind: form.kind
    });
    setForm({ goalId: "", title: "", detail: "", kind: "update" });
  };

  return (
    <div className="stack">
      <form className="composer" onSubmit={submit}>
        <select value={form.goalId} onChange={(e) => setForm({ ...form, goalId: e.target.value })}>
          <option value="">No goal link</option>
          {goals.map((goal) => (
            <option key={goal._id} value={goal._id}>
              {goal.goal}
            </option>
          ))}
        </select>
        <input placeholder="Progress title" value={form.title} onChange={(e) => setForm({ ...form, title: e.target.value })} />
        <textarea placeholder="Progress detail" value={form.detail} onChange={(e) => setForm({ ...form, detail: e.target.value })} />
        <select value={form.kind} onChange={(e) => setForm({ ...form, kind: e.target.value })}>
          {progressKinds.map((kind) => (
            <option key={kind} value={kind}>
              {kind}
            </option>
          ))}
        </select>
        <button type="submit">Log progress</button>
      </form>

      <div className="list">
        {progress.length === 0 && <p>No progress yet.</p>}
        {progress.map((entry) => (
          <article key={entry._id} className="card">
            <div className="row">
              <strong>{entry.title}</strong>
              <small className="badge">{entry.kind}</small>
            </div>
            {entry.detail && <p>{entry.detail}</p>}
            <small className="muted">{new Date(entry.createdAt).toLocaleString()}</small>
          </article>
        ))}
      </div>
    </div>
  );
}

function ArtifactsPanel({ workspaceId, artifacts, onCreate }) {
  const [form, setForm] = useState({ path: "", artifactType: "changed_file", summary: "", status: "updated" });

  const submit = async (event) => {
    event.preventDefault();
    if (!form.path.trim()) return;
    await onCreate({
      workspaceId,
      path: form.path.trim(),
      artifactType: form.artifactType,
      summary: form.summary.trim() || undefined,
      status: form.status
    });
    setForm({ path: "", artifactType: "changed_file", summary: "", status: "updated" });
  };

  return (
    <div className="stack">
      <form className="composer" onSubmit={submit}>
        <input placeholder="File path or artifact path" value={form.path} onChange={(e) => setForm({ ...form, path: e.target.value })} />
        <select value={form.artifactType} onChange={(e) => setForm({ ...form, artifactType: e.target.value })}>
          {artifactTypes.map((value) => (
            <option key={value} value={value}>
              {value}
            </option>
          ))}
        </select>
        <select value={form.status} onChange={(e) => setForm({ ...form, status: e.target.value })}>
          {artifactStatuses.map((value) => (
            <option key={value} value={value}>
              {value}
            </option>
          ))}
        </select>
        <input placeholder="Summary (optional)" value={form.summary} onChange={(e) => setForm({ ...form, summary: e.target.value })} />
        <button type="submit">Log changed file/artifact</button>
      </form>

      <div className="list">
        {artifacts.length === 0 && <p>No changed files/artifacts yet.</p>}
        {artifacts.map((entry) => (
          <article key={entry._id} className="card">
            <div className="row">
              <strong>{entry.path}</strong>
              <small className="badge">{entry.status}</small>
            </div>
            <p className="muted">{entry.artifactType}</p>
            {entry.summary && <p>{entry.summary}</p>}
            <small className="muted">{new Date(entry.createdAt).toLocaleString()}</small>
          </article>
        ))}
      </div>
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
