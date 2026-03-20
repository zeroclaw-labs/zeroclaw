---
id: zero-token-ticket-a748
stage: done
deps: []
links: []
created: 2026-03-20T13:51:27Z
type: feature
priority: 1
assignee: Dustin Reynolds
tags: [channels, matrix, ux]
skipped: [verify]
version: 6
---
# Zero-token ticket filing via keyword prefix in channel handlers

Detect 'ticket' or 'tickets' as the first word of a message in channel handlers (Matrix, Telegram). Extract the remainder as ticket content, use the first sentence as the title and the rest as the description, then shell out to 'tk create' directly — no LLM involved, zero tokens burned. Implementation mirrors the existing is_usage_command pattern in matrix.rs. Respond with a confirmation message (ticket ID) back to the room.
