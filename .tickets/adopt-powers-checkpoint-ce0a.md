---
id: adopt-powers-checkpoint-ce0a
stage: implement
deps: []
links: []
created: 2026-03-21T19:47:20Z
type: task
priority: 2
assignee: Dustin Reynolds
tags: [powers, workflow]
version: 3
---
# adopt Powers checkpoint convention in zeroclaw ticket workflow

Replace add-campaign-file-6def with Powers' existing checkpoint pattern. Convention: embed <\!-- checkpoint: <phase> --> comments in ticket markdown as work progresses (brainstorm/planning/executing/testing/finalized). Add <\!-- exit-state: --> block (immediate next action) and <\!-- key-files: --> block (load-bearing paths) to extend the pattern. Update zeroclaw skills to write these blocks on session end. This co-locates state with the ticket rather than a separate .campaign/ directory.

## Notes

**2026-03-21T21:32:35Z**

Scope clarified by third-party review: skip bulk extraction entirely. Focus is a targeted exit hook — when a session ends or compacts, emit a structured block (exit-state + key-files + open-questions) directly into the active ticket markdown. 20% of the effort, 80% of the value, verifiable via 'tk show'. This replaces both add-campaign-file-6def and the learning extraction tickets.
