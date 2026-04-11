"use client";

import Link from "next/link";
import { useQuery } from "convex/react";
import { api } from "@/convex/_generated/api";

const entityHrefMap = {
  workspace: "/#workspace",
  goal: "/#goal",
  progress: "/#progress",
  artifact: "/#artifacts",
  instruction: "/#instructions",
  deliverable: "/#deliverables"
};

export function ActivityPanel() {
  const entries = useQuery(api.activity.listRecentActivity, { limit: 20 }) || [];

  return (
    <section className="panel" id="activity">
      <h2>Recent Activity</h2>
      {entries.length === 0 && <p>No activity yet.</p>}
      <div className="activity-list">
        {entries.map((entry) => {
          const entityType = entry.entityType || entry.entity_type || "activity";
          const href = entityHrefMap[entityType] || "/";
          const createdAt = entry.createdAt || entry.created_at || Date.now();
          const actor = entry.actor || "you";
          const summary = entry.summary || entry.message || "activity updated";
          const action = entry.action || entry.event_type || "event";
          return (
            <Link key={entry._id} href={href} className="activity-item">
              <div className="row">
                <small>{formatRelative(createdAt)}</small>
                <small className="badge">{entityType}</small>
              </div>
              <p>
                <b>{actor}</b> · {summary}
              </p>
              <small className="muted">{action}</small>
            </Link>
          );
        })}
      </div>
    </section>
  );
}

function formatRelative(timestamp) {
  const elapsedMs = Date.now() - timestamp;
  if (elapsedMs < 60_000) return "just now";
  if (elapsedMs < 3_600_000) return `${Math.floor(elapsedMs / 60_000)}m ago`;
  if (elapsedMs < 86_400_000) return `${Math.floor(elapsedMs / 3_600_000)}h ago`;
  return new Date(timestamp).toLocaleString();
}
