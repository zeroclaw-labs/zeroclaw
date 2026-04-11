import { mutation, query } from "./_generated/server";
import { v } from "convex/values";

async function writeActivity(ctx, entry) {
  const doc = { createdAt: Date.now(), ...entry };
  if (doc.metadata === undefined) {
    delete doc.metadata;
  }
  await ctx.db.insert("activity", doc);
}

async function setWorkspaceActive(ctx, workspaceId) {
  const all = await ctx.db.query("projectWorkspaces").collect();
  await Promise.all(
    all.map((workspace) =>
      ctx.db.patch(workspace._id, {
        active: workspace._id === workspaceId,
        updatedAt: Date.now()
      })
    )
  );
}

export const seed = mutation({
  args: {},
  handler: async (ctx) => {
    const existing = await ctx.db.query("projectWorkspaces").first();
    if (existing) {
      return;
    }

    const now = Date.now();
    const workspaceId = await ctx.db.insert("projectWorkspaces", {
      name: "ClawPilot Workspace",
      slug: "clawpilot-workspace",
      rootPath: "/workspace/Clawpilot",
      description: "Primary Project Workspace for Mission Control.",
      globalInstructions:
        "Follow repository AGENTS.md instructions. Keep changes incremental and compilable.",
      active: true,
      createdAt: now,
      updatedAt: now
    });

    await ctx.db.insert("workspaceProgress", {
      workspaceId,
      title: "Workspace initialized",
      detail: "Mission Control is now workspace-first in Phase 1.",
      kind: "info",
      createdAt: now
    });

    await writeActivity(ctx, {
      actor: "you",
      entityType: "workspace",
      entityId: String(workspaceId),
      action: "workspace.seeded",
      summary: "you initialized the default Project Workspace"
    });
  }
});

export const dashboard = query({
  args: { workspaceId: v.optional(v.id("projectWorkspaces")) },
  handler: async (ctx, args) => {
    const workspaces = await ctx.db.query("projectWorkspaces").collect();
    const activeWorkspace =
      (args.workspaceId && (await ctx.db.get(args.workspaceId))) ||
      workspaces.find((workspace) => workspace.active) ||
      workspaces[0] ||
      null;

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

    const [goals, progress, artifacts, folderInstructions, deliverables] = await Promise.all([
      ctx.db
        .query("workspaceGoals")
        .withIndex("by_workspace", (q) => q.eq("workspaceId", activeWorkspace._id))
        .collect(),
      ctx.db
        .query("workspaceProgress")
        .withIndex("by_workspace", (q) => q.eq("workspaceId", activeWorkspace._id))
        .collect(),
      ctx.db
        .query("workspaceArtifacts")
        .withIndex("by_workspace", (q) => q.eq("workspaceId", activeWorkspace._id))
        .collect(),
      ctx.db
        .query("folderInstructions")
        .withIndex("by_workspace", (q) => q.eq("workspaceId", activeWorkspace._id))
        .collect(),
      ctx.db
        .query("workspaceDeliverables")
        .withIndex("by_workspace", (q) => q.eq("workspaceId", activeWorkspace._id))
        .collect()
    ]);

    return {
      workspaces,
      activeWorkspace,
      goals,
      progress,
      artifacts,
      folderInstructions,
      deliverables
    };
  }
});

export const createWorkspace = mutation({
  args: {
    name: v.string(),
    slug: v.string(),
    rootPath: v.string(),
    description: v.optional(v.string())
  },
  handler: async (ctx, args) => {
    const now = Date.now();
    const id = await ctx.db.insert("projectWorkspaces", {
      ...args,
      globalInstructions: "",
      active: false,
      createdAt: now,
      updatedAt: now
    });

    await writeActivity(ctx, {
      actor: "you",
      entityType: "workspace",
      entityId: String(id),
      action: "workspace.created",
      summary: `you created workspace "${args.name}"`
    });

    return id;
  }
});

export const setActiveWorkspace = mutation({
  args: { workspaceId: v.id("projectWorkspaces") },
  handler: async (ctx, args) => {
    const workspace = await ctx.db.get(args.workspaceId);
    if (!workspace) {
      throw new Error("Workspace not found");
    }

    await setWorkspaceActive(ctx, args.workspaceId);

    await writeActivity(ctx, {
      actor: "you",
      entityType: "workspace",
      entityId: String(args.workspaceId),
      action: "workspace.activated",
      summary: `you switched to workspace "${workspace.name}"`
    });
  }
});

export const upsertGlobalInstructions = mutation({
  args: {
    workspaceId: v.id("projectWorkspaces"),
    instructions: v.string()
  },
  handler: async (ctx, args) => {
    const workspace = await ctx.db.get(args.workspaceId);
    if (!workspace) {
      throw new Error("Workspace not found");
    }

    await ctx.db.patch(args.workspaceId, {
      globalInstructions: args.instructions,
      updatedAt: Date.now()
    });

    await writeActivity(ctx, {
      actor: "you",
      entityType: "instruction",
      entityId: String(args.workspaceId),
      action: "workspace.instructions.updated",
      summary: `you updated global instructions for "${workspace.name}"`
    });
  }
});

export const upsertFolderInstruction = mutation({
  args: {
    id: v.optional(v.id("folderInstructions")),
    workspaceId: v.id("projectWorkspaces"),
    folderPath: v.string(),
    instructions: v.string()
  },
  handler: async (ctx, args) => {
    const workspace = await ctx.db.get(args.workspaceId);
    if (!workspace) {
      throw new Error("Workspace not found");
    }

    const now = Date.now();
    const payload = {
      workspaceId: args.workspaceId,
      folderPath: args.folderPath,
      instructions: args.instructions,
      updatedAt: now
    };

    if (args.id) {
      const existing = await ctx.db.get(args.id);
      if (!existing) {
        throw new Error("Folder instruction not found");
      }
      await ctx.db.patch(args.id, payload);
      await writeActivity(ctx, {
        actor: "you",
        entityType: "instruction",
        entityId: String(args.id),
        action: "folder_instruction.updated",
        summary: `you updated folder instructions for ${args.folderPath}`
      });
      return args.id;
    }

    const id = await ctx.db.insert("folderInstructions", payload);
    await writeActivity(ctx, {
      actor: "you",
      entityType: "instruction",
      entityId: String(id),
      action: "folder_instruction.created",
      summary: `you added folder instructions in "${workspace.name}" for ${args.folderPath}`
    });
    return id;
  }
});

export const createGoal = mutation({
  args: {
    workspaceId: v.id("projectWorkspaces"),
    goal: v.string()
  },
  handler: async (ctx, args) => {
    const workspace = await ctx.db.get(args.workspaceId);
    if (!workspace) {
      throw new Error("Workspace not found");
    }

    const now = Date.now();
    const goalId = await ctx.db.insert("workspaceGoals", {
      workspaceId: args.workspaceId,
      goal: args.goal,
      status: "queued",
      createdAt: now,
      updatedAt: now
    });

    await ctx.db.insert("workspaceProgress", {
      workspaceId: args.workspaceId,
      goalId,
      title: "Goal captured",
      detail: args.goal,
      kind: "update",
      createdAt: now
    });

    await writeActivity(ctx, {
      actor: "you",
      entityType: "goal",
      entityId: String(goalId),
      action: "goal.created",
      summary: `you added a goal in "${workspace.name}"`
    });

    return goalId;
  }
});

export const createProgressEntry = mutation({
  args: {
    workspaceId: v.id("projectWorkspaces"),
    goalId: v.optional(v.id("workspaceGoals")),
    title: v.string(),
    detail: v.optional(v.string()),
    kind: v.union(v.literal("info"), v.literal("update"), v.literal("warning"), v.literal("complete"))
  },
  handler: async (ctx, args) => {
    const id = await ctx.db.insert("workspaceProgress", {
      workspaceId: args.workspaceId,
      goalId: args.goalId,
      title: args.title,
      detail: args.detail,
      kind: args.kind,
      createdAt: Date.now()
    });

    await writeActivity(ctx, {
      actor: "you",
      entityType: "progress",
      entityId: String(id),
      action: "progress.logged",
      summary: `you logged progress: ${args.title}`
    });

    return id;
  }
});

export const createArtifact = mutation({
  args: {
    workspaceId: v.id("projectWorkspaces"),
    path: v.string(),
    artifactType: v.union(v.literal("changed_file"), v.literal("artifact")),
    summary: v.optional(v.string()),
    status: v.union(v.literal("created"), v.literal("updated"), v.literal("deleted"))
  },
  handler: async (ctx, args) => {
    const id = await ctx.db.insert("workspaceArtifacts", {
      ...args,
      createdAt: Date.now()
    });

    await writeActivity(ctx, {
      actor: "you",
      entityType: "artifact",
      entityId: String(id),
      action: "artifact.logged",
      summary: `you logged ${args.artifactType}: ${args.path}`
    });

    return id;
  }
});

export const upsertRunDeliverable = mutation({
  args: {
    workspaceId: v.id("projectWorkspaces"),
    runId: v.string(),
    goalId: v.optional(v.id("workspaceGoals")),
    goal: v.string(),
    status: v.union(v.literal("completed"), v.literal("failed"), v.literal("running")),
    summary: v.string(),
    changedFiles: v.array(v.string()),
    artifacts: v.array(
      v.object({
        path: v.string(),
        artifactType: v.string(),
        status: v.string()
      })
    ),
    outputLocation: v.object({
      resultsRoot: v.string(),
      resultFile: v.string(),
      statusFile: v.string(),
      eventsFile: v.string(),
      artifactsDir: v.optional(v.string())
    }),
    suggestedNextSteps: v.array(v.string()),
    approvalHistory: v.array(
      v.object({
        createdAt: v.string(),
        eventType: v.string(),
        summary: v.string(),
        status: v.optional(v.string())
      })
    )
  },
  handler: async (ctx, args) => {
    const workspace = await ctx.db.get(args.workspaceId);
    if (!workspace) {
      throw new Error("Workspace not found");
    }

    if (args.goalId) {
      const goal = await ctx.db.get(args.goalId);
      if (!goal || goal.workspaceId !== args.workspaceId) {
        throw new Error("Goal not found in workspace");
      }
      await ctx.db.patch(args.goalId, {
        status: args.status === "failed" ? "blocked" : args.status === "completed" ? "done" : "in_progress",
        updatedAt: Date.now()
      });
    }

    const now = Date.now();
    const existing = await ctx.db
      .query("workspaceDeliverables")
      .withIndex("by_run", (q) => q.eq("runId", args.runId))
      .first();

    const payload = {
      workspaceId: args.workspaceId,
      runId: args.runId,
      goalId: args.goalId,
      goal: args.goal,
      status: args.status,
      summary: args.summary,
      changedFiles: args.changedFiles,
      artifacts: args.artifacts,
      outputLocation: args.outputLocation,
      suggestedNextSteps: args.suggestedNextSteps,
      approvalHistory: args.approvalHistory,
      updatedAt: now
    };

    if (existing) {
      await ctx.db.patch(existing._id, payload);
      await writeActivity(ctx, {
        actor: "you",
        entityType: "deliverable",
        entityId: String(existing._id),
        action: "deliverable.updated",
        summary: `you updated run deliverable for ${args.runId}`
      });
      return existing._id;
    }

    const id = await ctx.db.insert("workspaceDeliverables", {
      ...payload,
      createdAt: now
    });

    await writeActivity(ctx, {
      actor: "you",
      entityType: "deliverable",
      entityId: String(id),
      action: "deliverable.created",
      summary: `you finalized deliverable for run ${args.runId}`
    });

    return id;
  }
});
