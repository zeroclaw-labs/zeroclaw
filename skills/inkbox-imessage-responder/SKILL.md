---
name: inkbox-imessage-responder
description: Handle the agent's Inkbox iMessage conversations — triage, read, and reply over the shared iMessage service.
version: 0.1.0
author: zeroclaw-labs
tags:
  - inkbox
  - imessage
---

# Inkbox iMessage responder

Use this skill for iMessage on the agent's Inkbox identity.

## Tools

- `inkbox_list_imessage_conversations` — thread summaries with `conversation_id`
  and `assignment_status`.
- `inkbox_get_imessage_conversation` — read one conversation by UUID.
- `inkbox_send_imessage` — send an iMessage.

## Workflow

1. `inkbox_list_imessage_conversations` to see active threads. Note
   `assignment_status`: `released` means that person disconnected and replies
   will fail until they reconnect.
2. `inkbox_get_imessage_conversation` with the `conversation_id` to read the
   thread (messages include any tapback reactions).
3. Reply with `inkbox_send_imessage`, passing `conversation_id`. Only use `to`
   (an E.164 number) when you know that person has already connected to this
   agent over iMessage — there is no cold outreach on iMessage.

Keep replies natural and conversational, matching the medium.
