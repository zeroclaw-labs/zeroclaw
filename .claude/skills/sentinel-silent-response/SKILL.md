---
name: sentinel-silent-response
description: "Silent response sentinel for the ZeroClaw channel orchestrator. Trigger when working on conditional task silence, `__ZEROCLAW_NO_REPLY__`, `SILENT_RESPONSE_SENTINEL`, or no-reply behavior in the orchestrator pipeline."
---

# Sentinel Silent Response

The channel orchestrator supports a **silent response sentinel** that lets the LLM signal intentional silence without sending a message.

## How It Works

1. The LLM outputs `__ZEROCLAW_NO_REPLY__` as its entire response
2. The orchestrator intercepts this **before sanitization** in `process_channel_message_body()`
3. The draft is cancelled, a log event is emitted, and the function returns early
4. No message is sent, no history is appended, no memory is consolidated

## Relationship With Existing NoReply Classifier

The sentinel is **complementary** to the existing classifier pre-check:
- **Classifier**: runs *before* the main LLM, uses a cheaper model to decide if a reply is needed at all. Returns `NO_REPLY[INFO]`, `NO_REPLY[REFUSE]`, `NO_REPLY[FAIL]` with emoji reactions.
- **Sentinel**: runs *after* the main LLM, for cases where the classifier said REPLY but the LLM determines during execution that no response is needed.

## Design Constraints

- The sentinel check must be placed **after hooks** (so hooks can Cancel) but **before sanitization** (so the sentinel isn't mangled by `sanitize_channel_response()` or replaced by `ensure_nonempty_channel_reply()`)
- No new runtime state fields — follows the Single Source of Truth rule
- The sentinel is a `const &str`, not a runtime struct field
