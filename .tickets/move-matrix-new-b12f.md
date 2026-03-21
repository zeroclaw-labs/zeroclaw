---
id: move-matrix-new-b12f
stage: test
deps: []
links: []
created: 2026-03-20T17:19:20Z
type: task
priority: 2
assignee: Dustin Reynolds
version: 7
---
# Move matrix to a new linux host, chat

dustin

## Notes

**2026-03-20T20:00:31Z**

[triage-bot] Title is clear but description needs detail: which host? which Matrix instance? acceptance criteria? Holding in triage until spec is fleshed out.

**2026-03-20T22:00:39Z**

[triage-bot] Still needs spec details: target host, migration steps, acceptance criteria. Holding in triage.

**2026-03-21T02:03:02Z**

[triage-bot] Third pass — still awaiting spec: target host, conduwuit vs synapse, chat history migration plan, and acceptance criteria. Keeping in triage.

**2026-03-21T04:29:17Z**

Architecture decision: hostname will be matrix.local (not 'chat'). Using .local for mDNS auto-resolution across LAN without /etc/hosts on each device. SSH key already enabled and synced with matrix.local host. Server will run continuwuity (already in use on dustinllm.local). Single bot architecture (@zeroclaw:matrix.local) retained — no per-project bots. cron-bot kept as separate identity for scheduled posts. Matrix Spaces to group project rooms. Stop hook on Claude Code for push-based idle notification (replaces tmux polling for 'went idle' signal). Migration doc at docs/ops/matrix-migration.md.
