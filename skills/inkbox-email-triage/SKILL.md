---
name: inkbox-email-triage
description: Review and reply to the agent's Inkbox email — list the inbox, read full messages, and send threaded replies.
version: 0.1.0
author: zeroclaw-labs
tags:
  - inkbox
  - email
---

# Inkbox email triage

Use this skill when asked to check, review, or reply to email on the agent's
Inkbox mailbox.

## Tools

- `inkbox_list_emails` — newest-first inbox summaries (each has an `id`).
- `inkbox_get_email` — full body for one message `id`.
- `inkbox_send_email` — send or reply (set `in_reply_to_message_id` to thread).

## Workflow

1. Call `inkbox_list_emails` to see what's waiting. Summaries carry `id`,
   `from_address`, `subject`, and a snippet.
2. For anything you intend to act on, call `inkbox_get_email` with its `id` to
   read the full body before replying — never reply off the snippet alone.
3. Reply with `inkbox_send_email`. To keep the thread intact, pass the original
   message's RFC `message_id` as `in_reply_to_message_id`, reuse the subject
   (prefixed `Re:`), and set `to` to the original sender.
4. Keep replies concise and in the agent's voice. Confirm what you sent.

Inbound email also arrives automatically as a chat message (the channel pushes
it), so you can reply in-conversation without polling — use this skill when the
user asks you to go look proactively.
