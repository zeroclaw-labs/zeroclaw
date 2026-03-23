---
id: diagnose-restore-powers-0573
stage: done
deps: []
links: []
created: 2026-03-21T19:47:14Z
type: task
priority: 1
assignee: Dustin Reynolds
tags: [powers, learnings]
skipped: [implement, test, verify]
version: 3
---
# diagnose and restore Powers learning extraction pipeline

Session extraction stopped 2026-03-03 (18 days of lost sessions). Diagnosis: (1) verify PreCompact hook fires — check if agents are writing <!-- BEGIN_SESSION_SUMMARY --> sentinel markers, (2) check SessionEnd hook registration vs plugin system, (3) run catch-up script to recover missed sessions since 2026-03-03, (4) confirm learnings repo receives new commits after a test session. Fix whatever is broken so extraction runs automatically going forward.

## Notes

**2026-03-21T20:08:57Z**

DIAGNOSIS COMPLETE: Powers was never installed as a Claude Code plugin. Not in ~/.claude/plugins/installed_plugins.json. PreCompact hook never fired. Zero sessions with genuine session summaries in 1,671 transcripts. March 3rd learnings came from a one-time manual run. Historical data unrecoverable without expensive LLM pass. FIX: user must run /plugin install DustinReynoldsPE/powers in interactive Claude Code session. catchup.py and onboard-project.sh added to powers/scripts/. catchup.py ready to run after install to recover any future valid sessions.
