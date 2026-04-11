import { mutation, query } from "./_generated/server";
import { v } from "convex/values";

const entityType = v.union(
  v.literal("workspace"),
  v.literal("goal"),
  v.literal("progress"),
  v.literal("artifact"),
  v.literal("instruction"),
  v.literal("deliverable")
);

const activityArgs = {
  actor: v.union(v.literal("me"), v.literal("you")),
  entityType,
  entityId: v.string(),
  action: v.string(),
  summary: v.string(),
  metadata: v.optional(v.any())
};

export const logActivity = mutation({
  args: activityArgs,
  handler: async (ctx, args) => {
    return await ctx.db.insert("activity", {
      ...args,
      createdAt: Date.now()
    });
  }
});

export const listRecentActivity = query({
  args: { limit: v.optional(v.number()) },
  handler: async (ctx, args) => {
    const limit = Math.min(Math.max(args.limit ?? 30, 1), 100);
    return await ctx.db.query("activity").order("desc").take(limit);
  }
});

export const listActivityByEntity = query({
  args: {
    entityType,
    entityId: v.string(),
    limit: v.optional(v.number())
  },
  handler: async (ctx, args) => {
    const limit = Math.min(Math.max(args.limit ?? 50, 1), 200);
    return await ctx.db
      .query("activity")
      .withIndex("by_entity", (q) => q.eq("entityType", args.entityType).eq("entityId", args.entityId))
      .order("desc")
      .take(limit);
  }
});
