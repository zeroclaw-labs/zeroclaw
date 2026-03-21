---
id: proactive-automated-ticket-2343
stage: design
deps: []
links: []
created: 2026-03-20T13:53:29Z
type: task
priority: 2
assignee: Dustin Reynolds
tags: [scheduler, tickets, automation, matrix, cron]
version: 3
---
# Proactive automated ticket triage and progression — per-room Matrix cron

## Overview

A setup script creates a per-room cron job that periodically checks for tickets needing attention and posts findings to the respective Matrix room. This replaces the previous global-cron approach with room-scoped automation.

## Architecture

### Setup Script

A script (e.g. `dev/setup-room-cron.sh` or integrated into `zeroclaw` CLI) that:
1. Takes a Matrix room ID as input
2. Creates a cron job scoped to that room with delivery configured to announce to that room
3. Each room gets its own independent cron job instance

### Idle Detection — tmux session gating

Before injecting a prompt, the cron job MUST check whether the `tmux main:room` session is idle:

- **Idle** = no active Claude Code process running, not waiting for user feedback/approval
- **Not idle / waiting for feedback** = a question or approval prompt is pending in the tmux pane

Behavior based on session state:

| Session State | Action |
|---|---|
| Idle (no pending work) | Inject the triage prompt into `tmux main:room`, post results to the Matrix room |
| Waiting for feedback/question | Extract the pending question and post it to the Matrix room instead (surfaces blocked work) |
| Usage exhausted | Do nothing — no post, no injection |

### Per-room cron job

Each room's cron job:
- Runs on a configurable interval (default: every 2 hours)
- Uses `job_type: agent` with `session_target: isolated`
- Has `delivery: {"mode": "announce", "channel": "matrix", "to": "<room_id>"}`
- The agent prompt instructs it to:
  1. Check tmux session state first (idle vs waiting vs exhausted)
  2. If idle: run `tk list`, find tickets in triage, advance up to 3, post summary
  3. If waiting: extract the pending question, post it to the room
  4. If usage exhausted: exit silently

### Detecting session state

The script should check the tmux pane content for indicators:
- **Idle**: prompt is visible, no spinner, no "waiting for" text
- **Waiting for feedback**: look for patterns like `? `, `[y/N]`, `Allow`, `approve`, or Claude Code's permission prompts
- **Usage exhausted**: look for rate limit / quota messages in pane output

Use `tmux capture-pane -p -t main:room` to read pane contents.

## Ticket triage behavior (when idle)

Same as before:
1. Triage sweep: detect tickets stuck in 'triage', classify and advance
2. Ready ticket progression: advance low-risk unblocked tickets (docs, chore, tests)
3. Rate limit: max 3 tickets per cycle
4. Higher-risk tickets get a comment suggesting next action, wait for human approval
5. Log each action to audit trail

## Deliverables

1. Setup script that creates per-room cron jobs
2. Idle/feedback/exhaustion detection logic (tmux pane inspection)
3. Cron job prompt that handles all three states
4. Documentation in `docs/ops/` for configuring per-room automation
