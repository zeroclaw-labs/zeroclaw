---
id: live-tmux-pane-9e08
stage: done
deps: []
links: []
created: 2026-03-23T04:37:09Z
type: feature
priority: 1
assignee: Dustin Reynolds
version: 7
---
# live tmux pane streaming to Matrix

Stream filtered tmux pane updates to Matrix as new messages during tmux-routed commands. Show only Claude's written response text (no tool calls, thinking, chrome). Each new text chunk arrives as a separate message. Handles timeout gracefully by showing partial progress.
