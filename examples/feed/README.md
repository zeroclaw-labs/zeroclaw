# Feed Examples (Strict)

Canonical card types accepted by runtime validation:

`stock, crypto, prediction, game, news, social, poll, chart, logs, table, kv, metric, code, integration, weather, calendar, flight, ci, github, image, video, audio, webview, file`

## Learnings

- No fallback card types. Emit only canonical types.
- Every card type has required metadata fields. Keep metadata exact.
- Keep each execution payload bounded; very large item arrays can fail at exec-output boundaries.
- Prefer robust public APIs; if one source is unstable, switch source while keeping card type stable.

For production seeding logic used in this workspace:
- `openclaw-afw/sdk/scripts/seed-24-feeds.mjs` (full live source implementation)
- `examples/feed/_shared/live_24_types_sdk.mjs` (minimal SDK upload structure)
