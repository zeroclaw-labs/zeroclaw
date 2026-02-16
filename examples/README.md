# Zeroclaw Examples

These examples are intentionally practical and map to real runtime behavior in this repo.

- `examples/tools/`:
  Registry tool handler patterns (validation, timeout-safe fetch, strict output).
- `examples/agents/`:
  Agent orchestration patterns using tools + memory.
- `examples/feed/`:
  Per-card-type feed examples (`examples/feed/<canonical-type>/...`) with strict metadata guidance.
- `examples/cron/`:
  Scheduler jobs that execute feeds/tools on intervals.

## Learning Notes

- Keep handler output strict. Do not emit fallback card types.
- Prefer resilient public APIs and defensive parsing.
- Keep payloads bounded. Very large outputs can exceed container exec output limits.
- Always include required metadata fields for each card type.
- For live feeds, degrade source-by-source, not type-by-type.
