---
id: add-campaign-file-6def
stage: implement
deps: []
links: []
created: 2026-03-21T05:14:14Z
type: task
priority: 3
assignee: Dustin Reynolds
tags: [dx, docs]
version: 2
---
# add campaign file convention for multi-session development work

Establish a .campaigns/ directory convention for work that spans multiple conversations. A campaign file tracks: current objective, decisions made and why, what's been completed, what's still open, discovered constraints. This lets future sessions resume without re-discovering context. Define a minimal schema (YAML frontmatter + markdown body), create a template, and document when to create one vs just using a ticket. Tickets track what; campaigns track the thinking.
