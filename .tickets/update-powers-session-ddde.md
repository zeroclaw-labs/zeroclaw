---
id: update-powers-session-ddde
stage: done
deps: []
links: []
created: 2026-03-21T19:47:28Z
type: task
priority: 3
assignee: Dustin Reynolds
tags: [powers]
skipped: [implement, test, verify]
version: 3
---
# update Powers session-start context for zeroclaw workflow

Current SessionStart injection only mentions: 'powers:brainstorming, powers:create-tickets'. Missing: work-ticket (the most valuable skill), finishing-branch, subagent-execution. Update using-powers/SKILL.md and session-start.sh to surface the full relevant skill list for zeroclaw-context sessions. Also suppress css-architecture and tk-list/ready/ticket references — these add noise and don't apply to Rust/zeroclaw work.

## Notes

**2026-03-22T12:23:20Z**

Updated Powers 0.9.1→0.9.2. Changes: added /investigate and /brainstorming to using-powers SKILL.md, added /investigate to session-start.sh announcement, fixed dangling create-tickets reference→/create-feature, removed css-architecture refs from brainstorming and investigate skills, cleaned project structure docs. Committed, pushed, and updated local plugin cache.
