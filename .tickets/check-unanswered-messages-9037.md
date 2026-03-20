---
id: check-unanswered-messages-9037
stage: done
deps: []
links: []
created: 2026-03-19T15:27:19Z
type: feature
priority: 2
assignee: Dustin Reynolds
tags: [channels, matrix, reliability]
skipped: [spec, design, implement, test, verify]
version: 2
---
# Check for unanswered messages on daemon startup

When the daemon starts or restarts, it should check each listened Matrix room for messages that were never answered during downtime. After initial sync completes, fetch recent messages per room via the Matrix messages API, compare against the last known bot response timestamp, and process any unanswered user messages. Only process the most recent unanswered message per room to avoid stale context. Rate-limit to prevent a flood of responses on startup.
