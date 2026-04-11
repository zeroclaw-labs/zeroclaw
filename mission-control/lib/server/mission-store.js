import { mkdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { randomUUID } from "node:crypto";

const DEFAULT_ROOT = "/var/lib/clawpilot/mission-control";
const STATE_FILE = "state.json";

function dataRoot() {
  return process.env.MISSION_CONTROL_DATA_ROOT || DEFAULT_ROOT;
}

function statePath() {
  return path.join(dataRoot(), STATE_FILE);
}

const EMPTY_STATE = {
  projectWorkspaces: [],
  workspaceGoals: [],
  workspaceProgress: [],
  workspaceArtifacts: [],
  workspaceDeliverables: [],
  folderInstructions: [],
  activity: []
};

async function readState() {
  const root = dataRoot();
  await mkdir(root, { recursive: true });
  try {
    const body = await readFile(statePath(), "utf8");
    return { ...EMPTY_STATE, ...JSON.parse(body) };
  } catch (_error) {
    return { ...EMPTY_STATE };
  }
}

async function writeState(state) {
  await writeFile(statePath(), JSON.stringify(state, null, 2));
}

function now() {
  return Date.now();
}

function addActivity(state, entry) {
  state.activity.push({ _id: `a_${randomUUID()}`, createdAt: now(), ...entry });
}

function pickActiveWorkspace(state, workspaceId) {
  const explicit = workspaceId ? state.projectWorkspaces.find((item) => item._id === workspaceId) : null;
  return explicit || state.projectWorkspaces.find((item) => item.active) || state.projectWorkspaces[0] || null;
}

function byWorkspace(items, workspaceId) {
  return items.filter((item) => item.workspaceId === workspaceId);
}

function setWorkspaceActive(state, workspaceId) {
  const ts = now();
  state.projectWorkspaces = state.projectWorkspaces.map((workspace) => ({
    ...workspace,
    active: workspace._id === workspaceId,
    updatedAt: ts
  }));
}

export async function seedMissionData() {
  const state = await readState();
  if (state.projectWorkspaces.length > 0) {
    return { ok: true, seeded: false };
  }

  const ts = now();
  const workspaceId = `w_${randomUUID()}`;
  state.projectWorkspaces.push({
    _id: workspaceId,
    name: "ClawPilot Workspace",
    slug: "clawpilot-workspace",
    rootPath: process.env.CLAWPILOT_DEFAULT_WORKSPACE || "/workspace/Clawpilot",
    description: "Primary Project Workspace for Mission Control.",
    globalInstructions: "Follow repository AGENTS.md instructions. Keep changes incremental and compilable.",
    active: true,
    createdAt: ts,
    updatedAt: ts
  });

  state.workspaceProgress.push({
    _id: `p_${randomUUID()}`,
    workspaceId,
    title: "Workspace initialized",
    detail: "Mission Control is now workspace-first in production mode.",
    kind: "info",
    createdAt: ts
  });

  addActivity(state, {
    actor: "you",
    entityType: "workspace",
    entityId: workspaceId,
    action: "workspace.seeded",
    summary: "you initialized the default Project Workspace"
  });

  await writeState(state);
  return { ok: true, seeded: true };
}

export async function dashboardData(workspaceId) {
  const state = await readState();
  const activeWorkspace = pickActiveWorkspace(state, workspaceId);
  if (!activeWorkspace) {
    return {
      workspaces: [],
      activeWorkspace: null,
      goals: [],
      progress: [],
      artifacts: [],
      folderInstructions: [],
      deliverables: []
    };
  }

  return {
    workspaces: state.projectWorkspaces,
    activeWorkspace,
    goals: byWorkspace(state.workspaceGoals, activeWorkspace._id),
    progress: byWorkspace(state.workspaceProgress, activeWorkspace._id),
    artifacts: byWorkspace(state.workspaceArtifacts, activeWorkspace._id),
    folderInstructions: byWorkspace(state.folderInstructions, activeWorkspace._id),
    deliverables: byWorkspace(state.workspaceDeliverables, activeWorkspace._id)
  };
}

export async function recentActivity(limit = 20) {
  const state = await readState();
  return [...state.activity].sort((a, b) => b.createdAt - a.createdAt).slice(0, Math.max(1, Math.min(limit, 100)));
}

export async function runMissionCommand(action, args) {
  const state = await readState();
  const ts = now();

  if (action === "createWorkspace") {
    const id = `w_${randomUUID()}`;
    state.projectWorkspaces.push({
      _id: id,
      name: String(args.name || "").trim(),
      slug: String(args.slug || "").trim(),
      rootPath: String(args.rootPath || "").trim(),
      description: args.description ? String(args.description) : undefined,
      globalInstructions: "",
      active: false,
      createdAt: ts,
      updatedAt: ts
    });
    addActivity(state, {
      actor: "you",
      entityType: "workspace",
      entityId: id,
      action: "workspace.created",
      summary: `you created workspace "${String(args.name || "workspace")}"`
    });
    await writeState(state);
    return id;
  }

  if (action === "setActiveWorkspace") {
    const workspaceId = String(args.workspaceId || "");
    const workspace = state.projectWorkspaces.find((item) => item._id === workspaceId);
    if (!workspace) {
      throw new Error("Workspace not found");
    }
    setWorkspaceActive(state, workspaceId);
    addActivity(state, {
      actor: "you",
      entityType: "workspace",
      entityId: workspaceId,
      action: "workspace.activated",
      summary: `you switched to workspace "${workspace.name}"`
    });
    await writeState(state);
    return { ok: true };
  }

  if (action === "upsertGlobalInstructions") {
    const workspaceId = String(args.workspaceId || "");
    const idx = state.projectWorkspaces.findIndex((item) => item._id === workspaceId);
    if (idx < 0) {
      throw new Error("Workspace not found");
    }
    state.projectWorkspaces[idx] = {
      ...state.projectWorkspaces[idx],
      globalInstructions: String(args.instructions || ""),
      updatedAt: ts
    };
    addActivity(state, {
      actor: "you",
      entityType: "instruction",
      entityId: workspaceId,
      action: "workspace.instructions.updated",
      summary: `you updated global instructions for "${state.projectWorkspaces[idx].name}"`
    });
    await writeState(state);
    return { ok: true };
  }

  if (action === "upsertFolderInstruction") {
    const workspaceId = String(args.workspaceId || "");
    const folderPath = String(args.folderPath || "").trim();
    const instructions = String(args.instructions || "").trim();
    const id = args.id ? String(args.id) : `fi_${randomUUID()}`;
    const existingIdx = state.folderInstructions.findIndex((item) => item._id === id);
    if (existingIdx >= 0) {
      state.folderInstructions[existingIdx] = {
        ...state.folderInstructions[existingIdx],
        folderPath,
        instructions,
        updatedAt: ts
      };
    } else {
      state.folderInstructions.push({ _id: id, workspaceId, folderPath, instructions, updatedAt: ts });
    }
    addActivity(state, {
      actor: "you",
      entityType: "instruction",
      entityId: id,
      action: existingIdx >= 0 ? "folder_instruction.updated" : "folder_instruction.created",
      summary: `you updated folder instructions for ${folderPath}`
    });
    await writeState(state);
    return id;
  }

  if (action === "createGoal") {
    const id = `g_${randomUUID()}`;
    const workspaceId = String(args.workspaceId || "");
    const goal = String(args.goal || "").trim();
    state.workspaceGoals.push({
      _id: id,
      workspaceId,
      goal,
      status: "queued",
      createdAt: ts,
      updatedAt: ts
    });
    state.workspaceProgress.push({
      _id: `p_${randomUUID()}`,
      workspaceId,
      goalId: id,
      title: "Goal captured",
      detail: goal,
      kind: "update",
      createdAt: ts
    });
    addActivity(state, {
      actor: "you",
      entityType: "goal",
      entityId: id,
      action: "goal.created",
      summary: "you added a goal"
    });
    await writeState(state);
    return id;
  }

  if (action === "upsertRunDeliverable") {
    const runId = String(args.runId || "");
    const existingIdx = state.workspaceDeliverables.findIndex((item) => item.runId === runId);
    const payload = {
      _id: existingIdx >= 0 ? state.workspaceDeliverables[existingIdx]._id : `d_${randomUUID()}`,
      workspaceId: String(args.workspaceId || ""),
      runId,
      goalId: args.goalId ? String(args.goalId) : undefined,
      goal: String(args.goal || ""),
      status: args.status || "running",
      summary: String(args.summary || ""),
      changedFiles: Array.isArray(args.changedFiles) ? args.changedFiles : [],
      artifacts: Array.isArray(args.artifacts) ? args.artifacts : [],
      outputLocation: args.outputLocation || {
        resultsRoot: "",
        resultFile: "",
        statusFile: "",
        eventsFile: ""
      },
      suggestedNextSteps: Array.isArray(args.suggestedNextSteps) ? args.suggestedNextSteps : [],
      approvalHistory: Array.isArray(args.approvalHistory) ? args.approvalHistory : [],
      createdAt: existingIdx >= 0 ? state.workspaceDeliverables[existingIdx].createdAt : ts,
      updatedAt: ts
    };

    if (existingIdx >= 0) {
      state.workspaceDeliverables[existingIdx] = payload;
    } else {
      state.workspaceDeliverables.push(payload);
    }

    const goalIdx = state.workspaceGoals.findIndex((item) => item._id === payload.goalId);
    if (goalIdx >= 0) {
      state.workspaceGoals[goalIdx].status = payload.status === "failed" ? "blocked" : payload.status === "completed" ? "done" : "in_progress";
      state.workspaceGoals[goalIdx].updatedAt = ts;
    }

    addActivity(state, {
      actor: "you",
      entityType: "deliverable",
      entityId: payload._id,
      action: existingIdx >= 0 ? "deliverable.updated" : "deliverable.created",
      summary: `you finalized deliverable for run ${runId}`
    });
    await writeState(state);
    return payload._id;
  }

  throw new Error(`Unsupported action: ${action}`);
}
