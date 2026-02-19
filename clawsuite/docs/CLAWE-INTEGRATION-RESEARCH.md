# Clawe Integration Research

> **Date:** 2026-02-17  
> **Source:** https://github.com/getclawe/clawe  
> **Verdict:** ⚡ HIGH VALUE — Not a direct integration, but a blueprint for ClawSuite's Agent Hub multi-agent coordination features.

---

## What Is Clawe?

Clawe is a **multi-agent coordination system built on top of OpenClaw**. It's literally what we're building — a team of AI agents (Squad Lead, Content Editor, Designer, SEO) that work together on tasks, coordinate through a shared backend, and are monitored via a web dashboard.

It's made by `@getclawe` — almost certainly the same team or community around OpenClaw.

---

## Tech Stack

- **Agent runtime:** OpenClaw (squadhub gateway, one per agent)
- **Backend:** Convex (real-time database — tasks, notifications, activities, agent state)
- **Dashboard:** Next.js app (`apps/web/`)
- **Watcher service:** Node.js — registers agents, sets up crons, delivers notifications
- **CLI:** `clawe` CLI — agents call this to check tasks, update status, send notifications
- **Infrastructure:** Docker Compose (3 containers: squadhub, watcher, clawe web)

---

## How Agents Coordinate

1. **Shared files** — each agent has `/data/workspace-{agent}/shared/` symlinked to shared state (`WORKING.md`, `WORKFLOW.md`)
2. **Convex backend** — real-time DB stores tasks, subtasks, deliverables, notifications, activity feed
3. **CLI** — agents call `clawe check`, `clawe tasks`, `clawe notify <session>` etc. during heartbeats
4. **@mentions** — agents can notify other agents via session key
5. **Heartbeats** — every 15 min, staggered to avoid rate limits, cron-managed by watcher service

### Agent Workspace Structure (identical to ours!)
```
/data/workspace-{agent}/
├── AGENTS.md      # Instructions
├── SOUL.md        # Identity/personality  
├── USER.md        # Human context
├── HEARTBEAT.md   # Wake instructions
├── MEMORY.md      # Long-term memory
├── TOOLS.md       # Tool notes
└── shared/        # Symlink → shared state
```

This is **exactly our workspace structure**. They're running the same playbook.

---

## Compatibility With OpenClaw/ClawSuite

**Very high.** Clawe IS OpenClaw — it uses OpenClaw as the agent runtime. The coordination layer on top (Convex + CLI + watcher) is what's new.

Key differences:
- They use **Convex** for shared state; we'd use our existing gateway + DB
- Their dashboard is a separate Next.js app; ours is ClawSuite itself
- They have a dedicated `watcher` service for notifications; we have crons + Telegram

---

## What To Steal For ClawSuite Agent Hub

### 1. **Task Board with Agent Assignment**
Clawe's kanban board lets you assign tasks to specific agents, track subtasks, register deliverables. This is exactly what Agent Hub is missing — right now it just shows running agents, not what they're working on.

### 2. **Agent-to-Agent Notifications**
`clawe notify <session-key> "Need your review"` — agents can ping each other. We have `sessions_send()` but no UI surface for it in Agent Hub.

### 3. **Squad Status View**
`clawe squad` — one-screen view of all agents: who's active, what they're doing, last heartbeat. Build this into Agent Hub dashboard widget.

### 4. **Activity Feed**
`clawe feed` — chronological feed of all agent actions. We have activity log but not multi-agent scoped.

### 5. **Shared WORKING.md Pattern**
Simple shared file that all agents read/write to show current team status. Low-tech but effective for coordination without a full database.

### 6. **Staggered Heartbeats**
They explicitly stagger agent heartbeats to avoid API rate limits. We should do this too if we ever run multiple agents simultaneously.

---

## What NOT To Copy

- **Convex dependency** — adds complexity, external service, cost. Our gateway already handles real-time state.
- **Separate watcher service** — we already have cron jobs for this.
- **Docker Compose multi-container setup** — overkill for our single-machine setup.
- **4 pre-configured agents** (Clawe, Inky, Pixel, Scout) — too opinionated, we want user-configurable agents.

---

## Security

- Agents share files via symlinks — low risk (local only)
- `SQUADHUB_TOKEN` for gateway auth — same pattern as our gateway token
- Convex is a third-party service — means task data leaves the machine. Skip for us.
- No obvious injection vectors in the CLI design

---

## Stability

Looks **production-ready** for its scope. Clean README, Docker setup, proper env validation. It's a demo/template system more than a framework — meant to be cloned and customized.

---

## Recommendation

**Don't integrate Clawe directly. Build ClawSuite's Agent Hub with Clawe as the reference design.**

Specifically implement these features in Agent Hub:

| Feature | Priority | Notes |
|---------|----------|-------|
| Squad status view (who's running, last heartbeat, current task) | 🔴 High | Core Agent Hub widget |
| Task board with agent assignment | 🔴 High | Kanban in Agent Hub |
| Agent-to-agent notifications via UI | 🟡 Medium | sessions_send() already exists |
| Activity feed scoped to agents | 🟡 Medium | We have activity log, just needs agent filter |
| Shared WORKING.md file for team state | 🟢 Low | Simple, add to workspace conventions |
| Staggered heartbeat scheduling | 🟢 Low | Add when running 3+ simultaneous agents |

The architecture doc at `docs/AGENT-HUB-STREAMING.md` already covers warden controls and streaming. Add the above as the coordination layer on top.
