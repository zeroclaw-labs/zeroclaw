# Sam Screenshot Tool Image RCA

Date: 2026-04-10

## Summary

Sam's deployed screenshot flow failed on the `litellm-sam` endpoint with backend errors like `Invalid url value` even though the same endpoint could successfully answer direct vision prompts from the LiteLLM playground.

The root cause was request shape, not screenshot capture or image encoding.

## What Worked

The successful LiteLLM playground request used the documented multimodal chat shape:

- one `user` message
- `content` array containing:
  - a `text` part
  - an `image_url` part with a `data:image/...;base64,...` URL

This matched the LiteLLM vision docs and succeeded end-to-end on `litellm-sam`.

## What Failed

ZeroClaw's screenshot flow preserved the image bytes but attached them to a matched `tool` result message:

- `role: "tool"`
- `tool_call_id: ...`
- `content` array containing:
  - a `text` part
  - an `image_url` part with the screenshot data URL

That request shape produced `Invalid url value` errors from the backend.

## Root Cause

The OpenAI-compatible provider converted screenshot-bearing tool results into multimodal `tool` messages. The target LiteLLM/backend path accepts the documented multimodal `user` shape, but rejected the multimodal `tool` shape Sam emitted.

This was a protocol-compatibility issue in the provider serialization path.

## Fix

When ZeroClaw serializes a matched tool result:

- if the tool result is text-only, keep the normal `tool` message
- if the tool result contains image markers, rewrite it into a synthetic multimodal `user` message prefixed with `[Tool result]`

This preserves normal tool-call semantics for text-only tools while aligning screenshot requests with the proven working LiteLLM vision format.

## Files

- `src/providers/compatible.rs`
- `src/tools/browser.rs`
- `src/tools/screenshot.rs`

## Validation

Focused regression coverage was added for:

- image-bearing matched tool results serialize as multimodal `user` messages
- text-only matched tool results remain `tool` messages
