# Sam Screenshot Context Lifecycle RCA and Remediation

Date: 2026-04-11

## Summary

Sam's screenshot failures narrowed to a payload-lifecycle problem rather than screenshot capture itself. The browser tool successfully produced optimized image data, but the same raw screenshot payload could be replayed across provider retries after the first failed request. Gemma-style multimodal usage is a better fit when the image is treated as immediate-turn context, not durable raw history.

## Behavior Change

- The immediate post-screenshot request still carries the raw screenshot payload so the model can inspect pixels.
- Same-turn max-tokens continuations are allowed to retain that image payload.
- Provider-level retries downgrade image-bearing tool results to text-only summaries before resending the request.
- Durable tool history also downgrades screenshot-bearing entries after request assembly so later unrelated turns do not keep replaying raw `data:image/...` content.

## Why

- Transport retries were repeatedly resending the same raw screenshot payload after backend `Invalid url value` failures.
- Later turns do not need the original screenshot bytes unless the model is explicitly asked to inspect the image again.
- This matches the Gemma guidance better than keeping screenshots as permanent raw history artifacts.

## Rollback

Revert the screenshot history downgrade in:

- `src/agent/loop_.rs`
- `src/providers/reliable.rs`

If rollback is needed, keep the original multimodal request path intact and remove only the downgrade-on-retry / downgrade-in-history logic.
