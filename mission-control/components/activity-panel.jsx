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
          const href = entityHrefMap[entry.entityType] || "/";
          return (
            <Link key={entry._id} href={href} className="activity-item">
              <div className="row">
                <small>{formatRelative(entry.createdAt)}</small>
                <small className="badge">{entry.entityType}</small>
              </div>
              <p>
                <b>{entry.actor}</b> · {entry.summary}
              </p>
              <small className="muted">{entry.action}</small>
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
