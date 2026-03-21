---
id: formalize-daemon-deploy-53cd
stage: implement
deps: []
links: []
created: 2026-03-21T05:14:08Z
type: task
priority: 2
assignee: Dustin Reynolds
tags: [ops, matrix]
version: 6
---
# formalize daemon deploy verification as required smoke test

After any daemon restart, require a structured smoke test before considering the deploy done: (1) confirm Matrix channel listening on expected N rooms in the log, (2) send 'idle' in zeroclaw room and verify all 5 tmux targets respond with correct state, (3) send 'cron' in one room and verify workspace and job listing are correct. Document this in docs/ops/ as the canonical post-deploy checklist. Exit code 0 (daemon starts) is not quality.

## Notes

**2026-03-21T06:00:45Z**

Over-advanced during triage — this is a docs task but still needs the checklist written. Actual status is implement-ready.
