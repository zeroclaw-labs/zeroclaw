# Agent Hub v2 — Warden + Multi-Agent Coordination

> **Warden** = real-time oversight and control of running agents  
> **Coordination** = task assignment, agent-to-agent comms, squad status (inspired by Clawe)

---

## Current State (v1)

Agent Hub right now:
- Shows list of running agents/sessions
- Basic session history
- Can view agent activity in terminal
- No controls, no tasks, no coordination

---

## v2 Architecture

### 1. Warden Controls (Real-Time Oversight)

Live controls for each running agent:

| Control | What It Does |
|---------|--------------|
| **Steer** | Inject a message into agent's context ("focus on X", "stop doing Y") |
| **Pause** | Pause heartbeats/crons for this agent |
| **Resume** | Resume paused agent |
| **Kill** | Terminate agent session immediately |
| **Guardrails** | Set constraints: max tokens, allowed tools, auto-stop triggers |

**UI:** Each agent card gets a "⋮" menu with these actions. Steer opens a text input modal.

**Backend:** Already have `subagents steer/kill`, just need UI surface. Pause/resume = cron toggle via `cron update`.

### 2. Live Agent Streaming

Stream agent activity in real-time:
- Current message being typed (delta streaming)
- Tool calls as they happen
- Thinking/reasoning indicators
- Token usage ticking up

**UI:** Click agent card → expands to show live output stream (like terminal but formatted). Use existing SSE from gateway.

### 3. Squad Status Panel

Inspired by Clawe's `clawe squad`:

```
┌─────────────────────────────────────────────────────┐
│ SQUAD STATUS                                        │
├─────────────────────────────────────────────────────┤
│ 🟢 Aurora (main)     Active    "Reviewing PR #28"  │
│ 🟡 Codex Worker #1   Idle      Last: 5 min ago     │
│ 🔴 Research Agent    Paused    Paused by user      │
│ 🟢 Dashboard Sonnet  Running   "Migrating widgets" │
└─────────────────────────────────────────────────────┘
```

**Data:** Poll `sessions_list` + `subagents list`, merge with cron status for heartbeat info.

### 4. Task Board Integration

Mini kanban inside Agent Hub:

```
┌─────────────┬─────────────┬─────────────┐
│  BACKLOG    │ IN PROGRESS │    DONE     │
├─────────────┼─────────────┼─────────────┤
│ Dashboard   │ Slash cmds  │ Model fix   │
│ redesign    │ @Codex      │ @Aurora     │
│ @unassigned │             │             │
├─────────────┼─────────────┼─────────────┤
│ SSE filter  │             │ Security    │
│ @queued     │             │ audit       │
└─────────────┴─────────────┴─────────────┘
```

**Storage:** `data/tasks.json` (already exists) or new `data/agent-tasks.json` with proper schema.

**Assignment:** Drag task to agent, or right-click → Assign. Creates a cron/spawn if agent isn't running.

### 5. Agent-to-Agent Notifications

When Agent A needs Agent B:
1. A calls `sessions_send(target, "Need review on X")`
2. Notification appears in Agent Hub
3. If B is idle, optionally auto-wake via cron trigger

**UI:** Notification bell in Agent Hub header. Click → see pending messages between agents.

---

## Implementation Order

### Phase 1: Warden Controls (High Impact, Low Effort)
- [x] Kill button → `subagents kill`
- [ ] Steer modal → `subagents steer` with text input
- [ ] Pause/Resume → cron toggle
- [ ] Guardrails modal (token limit, tool allowlist)

### Phase 2: Live Streaming
- [ ] Expand agent card to show live output
- [ ] Delta streaming from gateway SSE
- [ ] Tool call indicators
- [ ] Token counter

### Phase 3: Squad Status
- [ ] Consolidated status widget
- [ ] Last activity timestamp
- [ ] Current task/message preview
- [ ] Heartbeat schedule display

### Phase 4: Task Board
- [ ] Schema for agent tasks
- [ ] Kanban UI component
- [ ] Drag-to-assign
- [ ] Auto-spawn on assign

### Phase 5: Agent Comms
- [ ] Notification inbox in Agent Hub
- [ ] `sessions_send` UI surface
- [ ] Auto-wake on notification

---

## File Changes Required

| File | Change |
|------|--------|
| `src/components/agent-swarm/agent-card.tsx` | Add warden controls menu |
| `src/components/agent-swarm/steer-modal.tsx` | New: text input for steering |
| `src/components/agent-swarm/agent-stream.tsx` | New: live output viewer |
| `src/components/agent-swarm/squad-status.tsx` | New: consolidated status panel |
| `src/components/agent-swarm/task-board.tsx` | New: mini kanban |
| `src/routes/api/agent-steer.ts` | New: POST endpoint for steer |
| `src/routes/api/agent-pause.ts` | New: POST endpoint for pause/resume |
| `data/agent-tasks.json` | New: task storage schema |

---

## Open Questions

1. **Task persistence** — JSON file vs SQLite vs Convex-style real-time DB?
   - Recommendation: JSON file for now, migrate later if needed

2. **Multi-user** — what if two people assign tasks?
   - Recommendation: Single-user for now (Eric only)

3. **Guardrails enforcement** — where does it happen?
   - Gateway side (ideal) or ClawSuite proxy (hack)
   - Need to check if gateway supports token limits per session

4. **Agent identity** — how do we know which agent is which?
   - Session labels (`subagent:codex-1`), agent IDs, or custom metadata?
   - Clawe uses explicit agent configs with names/emojis

---

## Relation to Dashboard Redesign

Agent Hub v2 becomes a **dashboard widget** (medium or large size):
- Squad Status = small widget
- Task Board = medium widget
- Full Agent Hub = large widget or separate screen

Use `WidgetShell` wrapper from dashboard redesign architecture.
