---
id: audit-trim-claude-5627
stage: implement
deps: []
links: []
created: 2026-03-21T05:14:02Z
type: task
priority: 2
assignee: Dustin Reynolds
tags: [dx, docs]
version: 2
---
# audit and trim CLAUDE.md to stay under compliance cliff

Count lines in CLAUDE.md (global and project). Compliance degrades visibly past ~100 lines. Audit for: redundancy with hooks/rules, procedures that belong in skills, verbose sections reducible to one-liners, anything that has stabilized into a rule that should be in a rules file. Target: both files under 120 lines with zero redundancy. Establish a discipline for promoting content out rather than accreting.
