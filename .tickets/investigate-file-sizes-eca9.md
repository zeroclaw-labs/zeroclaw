---
id: investigate-file-sizes-eca9
stage: implement
deps: []
links: []
created: 2026-03-22T03:47:05Z
type: task
priority: 2
assignee: Dustin Reynolds
version: 3
---
# Investigate file sizes in project, keep files less than 500 lines if possible



## Notes

**2026-03-22T12:11:10Z**

Audit found 9 files over 3000 lines. Top: schema.rs (10994), channels/mod.rs (9988), onboard/wizard.rs (7285), agent/loop_.rs (6818). Priority splits: (1) extract tool execution from loop_.rs to tool_executor.rs, (2) split wizard.rs by stage. Schema.rs is intentionally large (tight config coupling). 26 files in 1000-1500 range, mostly reasonable single-responsibility modules.
