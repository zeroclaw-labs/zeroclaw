---
id: audit-skills-identity-3b63
stage: implement
deps: []
links: []
created: 2026-03-21T05:14:20Z
type: task
priority: 3
assignee: Dustin Reynolds
tags: [dx, skills]
version: 4
---
# audit skills for Identity/Orientation/Protocol/Quality Gates/Exit Protocol anatomy

Review all skills in .claude/skills/ against the five-section anatomy: Identity (what it does, when to invoke, what it does NOT do), Orientation (what to read first), Protocol (step-by-step procedure), Quality Gates (what must be true before done), Exit Protocol (what to write to disk before ending). Identity prevents misuse. Quality Gates define done. Exit Protocol ensures knowledge survives the session. Fix any skills missing these sections. The zeroclaw skill in particular needs Quality Gates and Exit Protocol.

## Notes

**2026-03-21T06:00:46Z**

Triage reviewed — well-described P3 task. Left in triage for now; lower priority than the P2 items. Ready to advance when bandwidth allows.

**2026-03-21T08:00:26Z**

Cron triage: well-scoped P3 task with clear acceptance criteria (five-section anatomy). Advanced to implement — no spec phase needed since the audit checklist is already defined in the description.
