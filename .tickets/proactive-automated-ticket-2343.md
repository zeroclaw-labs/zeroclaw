---
id: proactive-automated-ticket-2343
stage: triage
deps: []
links: []
created: 2026-03-20T13:53:29Z
type: task
priority: 2
assignee: Dustin Reynolds
tags: [scheduler, tickets, automation]
version: 1
---
# Proactive automated ticket triage and progression via scheduler


The system should periodically check for tickets that need attention without waiting for user prompts.

Two main behaviors:
1. Triage sweep: detect tickets stuck in 'triage' stage, run LLM classification to assign spec/priority/tags and advance them.
2. Ready ticket progression: find unblocked 'ready' tickets and attempt to advance low-risk ones (e.g. docs, chore, test) autonomously.

Implementation approach:
- Add a cron job (or heartbeat hook) that runs every N minutes when the system is otherwise idle.
- Idle detection: skip cycle if a message was processed in the last M minutes.
- Rate limit: max K tickets advanced per cycle to prevent runaway.
- Only auto-advance tickets where risk tier is 'low' (docs/chore/tests); higher-risk tickets get a comment added suggesting next action but wait for human approval.
- Use 'tk list --stage triage' and 'tk list --stage spec' to find candidates.
- Log each action to audit trail.
- Config: scheduler interval, idle threshold, max tickets per cycle, allowed stages for auto-advance.
