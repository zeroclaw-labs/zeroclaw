---
name: inkbox-sms-responder
description: Handle the agent's Inkbox SMS/MMS — triage conversations, read threads, and reply (including group texts).
version: 0.1.0
author: zeroclaw-labs
tags:
  - inkbox
  - sms
---

# Inkbox SMS responder

Use this skill for texting on the agent's Inkbox phone number.

## Tools

- `inkbox_list_text_conversations` — newest-first thread summaries with
  `conversation_id` and unread counts (groups included by default).
- `inkbox_get_text_conversation` — read one thread by `conversation_id` (or
  remote E.164 number).
- `inkbox_send_sms` — send a text.

## Workflow

1. `inkbox_list_text_conversations` to find threads needing a reply (watch
   `unread_count`).
2. `inkbox_get_text_conversation` with the `conversation_id` to read context
   before replying.
3. Reply with `inkbox_send_sms`:
   - To reply into an existing thread, pass `conversation_id` (preferred — it
     handles 1:1 and group threads correctly).
   - To start a new 1:1, pass `to` with a single E.164 number.
   - For a group MMS, pass `to` as a list of numbers.
4. Keep texts short. SMS bodies over ~1600 characters are rejected — split long
   replies.

## Group texts

In a group thread you see every participant's messages. Reply by
`conversation_id` so the message fans out to the whole group; address people by
name when it isn't obvious who you're answering.
