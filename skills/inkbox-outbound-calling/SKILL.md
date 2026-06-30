---
name: inkbox-outbound-calling
description: Place a live outbound phone call from the agent's Inkbox number and hold the conversation over the call-media bridge.
version: 0.1.0
author: zeroclaw-labs
tags:
  - inkbox
  - voice
---

# Inkbox outbound calling

Use this skill when asked to call someone by phone.

## Tool

- `inkbox_place_call` — dials a number and bridges the call's audio to the agent
  over the tunnel's call-media WebSocket. You speak the conversation live;
  Inkbox handles speech-to-text and text-to-speech.

## Workflow

1. Confirm you have the recipient's E.164 number and a clear purpose for the
   call.
2. Call `inkbox_place_call` with `to_number`. Leave `client_websocket_url`
   unset — it defaults to this agent's tunnel so the call routes back to you.
3. When the callee answers, the call connects to the live voice bridge: you
   receive each caller utterance as a turn and your replies are spoken back.
   Open with who you are and why you're calling, then keep turns short and
   conversational — this is spoken audio, not text.
4. After hanging up, summarize the outcome and take any follow-up actions (send
   a recap text or email) the conversation called for.

Only place calls the user actually asked for, to the number they specified.
