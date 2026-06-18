---
name: inkbox-troubleshooting
description: Diagnose the agent's Inkbox setup — identity, mailbox, phone number, iMessage status, and tunnel host.
version: 0.1.0
author: zeroclaw-labs
tags:
  - inkbox
  - diagnostics
---

# Inkbox troubleshooting

Use this skill when Inkbox messaging or calling isn't working, or when asked
what the agent's contact details are.

## Tool

- `inkbox_whoami` — returns the configured identity: `agent_handle`,
  `email_address`, `phone_number`, `imessage_enabled`, and
  `tunnel_public_host`.

## Workflow

1. Call `inkbox_whoami`.
2. Interpret the result:
   - **No `email_address`** → the mailbox isn't provisioned; email send/receive
     won't work.
   - **No `phone_number`** → no number assigned; SMS and calls are unavailable.
   - **`imessage_enabled: false`** → iMessage sends/receives are off for this
     identity.
   - **No `tunnel_public_host`** → the inbound tunnel isn't provisioned, so
     inbound email/SMS/iMessage/calls cannot be delivered even though outbound
     still works.
3. Report the relevant gap plainly and, when it's a config issue (missing key,
   wrong identity handle), say what needs to be set in
   `[channels.inkbox.<alias>]`.
