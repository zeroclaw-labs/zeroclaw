---
id: install-powers-claude-2c7b
stage: done
deps: []
links: []
created: 2026-03-21T20:09:04Z
type: chore
priority: 1
assignee: Dustin Reynolds
tags: [powers]
skipped: [implement]
version: 2
---
# install Powers as Claude Code plugin

Run in an interactive Claude Code session: /plugin install DustinReynoldsPE/powers — This registers SessionStart (context injection), PreCompact (summary generation prompt), and SessionEnd (extract-session-summary.sh) hooks via the plugin system. Verify: after install, start a new session and check that ~/.claude/plugins/installed_plugins.json includes powers@DustinReynoldsPE/powers. After first session ends, check ~/code/learnings/sessions/ for a new file. This is the single blocking item for the entire learning extraction flywheel.
