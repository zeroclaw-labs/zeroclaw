import { defineSchema, defineTable } from "convex/server";
import { v } from "convex/values";

const activityEntityType = v.union(
  v.literal("workspace"),
  v.literal("goal"),
  v.literal("progress"),
  v.literal("artifact"),
  v.literal("instruction")
);

export default defineSchema({
  projectWorkspaces: defineTable({
    name: v.string(),
    slug: v.string(),
    rootPath: v.string(),
    description: v.optional(v.string()),
    globalInstructions: v.optional(v.string()),
    active: v.boolean(),
    createdAt: v.number(),
    updatedAt: v.number()
  })
    .index("by_slug", ["slug"])
    .index("by_active", ["active"]),
  workspaceGoals: defineTable({
    workspaceId: v.id("projectWorkspaces"),
    goal: v.string(),
    status: v.union(v.literal("queued"), v.literal("in_progress"), v.literal("blocked"), v.literal("done")),
    createdAt: v.number(),
    updatedAt: v.number()
  }).index("by_workspace", ["workspaceId"]),
  workspaceProgress: defineTable({
    workspaceId: v.id("projectWorkspaces"),
    goalId: v.optional(v.id("workspaceGoals")),
    title: v.string(),
    detail: v.optional(v.string()),
    kind: v.union(v.literal("info"), v.literal("update"), v.literal("warning"), v.literal("complete")),
    createdAt: v.number()
  }).index("by_workspace", ["workspaceId"]),
  workspaceArtifacts: defineTable({
    workspaceId: v.id("projectWorkspaces"),
    path: v.string(),
    artifactType: v.union(v.literal("changed_file"), v.literal("artifact")),
    summary: v.optional(v.string()),
    status: v.union(v.literal("created"), v.literal("updated"), v.literal("deleted")),
    createdAt: v.number()
  }).index("by_workspace", ["workspaceId"]),
  folderInstructions: defineTable({
    workspaceId: v.id("projectWorkspaces"),
    folderPath: v.string(),
    instructions: v.string(),
    updatedAt: v.number()
  }).index("by_workspace", ["workspaceId"]),
  activity: defineTable({
    createdAt: v.number(),
    actor: v.union(v.literal("me"), v.literal("you")),
    entityType: activityEntityType,
    entityId: v.string(),
    action: v.string(),
    summary: v.string(),
    metadata: v.optional(v.any())
  })
    .index("by_createdAt", ["createdAt"])
    .index("by_entity", ["entityType", "entityId"])
});
