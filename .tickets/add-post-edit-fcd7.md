---
id: add-post-edit-fcd7
stage: done
deps: []
links: []
created: 2026-03-21T05:13:56Z
type: task
priority: 1
assignee: Dustin Reynolds
tags: [rust, hooks, dx]
skipped: [test, verify]
version: 3
---
# add post-edit cargo check hook for immediate Rust error feedback

Add a PostToolUse hook that runs cargo check scoped to the changed crate whenever a .rs file is written. Errors should surface on the edit that introduces them, not at build time. Hook must be wrapped in error handling so it never blocks the editing flow — degraded observability is acceptable, blocked work is not. Target: catch type/borrow errors within 2-5s of the edit that causes them.
