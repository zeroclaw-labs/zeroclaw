# Model tiers and live catalog

Zeroclaw's `model_switch` tool resolves semantic tiers (`chat`, `thinking`, `fast`) to concrete model IDs via a local YAML file and validates raw model IDs against the provider's live `/v1/models` catalog.

## Agent-facing actions

- `list_tiers` — returns the configured tiers with their resolved model IDs and descriptions.
- `set_tier { tier: "chat" | "thinking" | "fast" }` — stages a tier switch applied on the next agent turn.
- `list_models` — returns the live catalog from the provider's `/v1/models` (cached 60s per process).
- `set { provider, model }` — validates the model against the live catalog before staging. Unknown names are rejected with the available list.

## Configuration

Tier mapping lives in `/zeroclaw-data/.zeroclaw/tiers.yaml` (seeded by the Docker image on first boot). Example:

```yaml
tiers:
  - name: chat
    model: claude-sonnet-4-6
    description: Default.
  - name: thinking
    model: claude-opus-4-7
    description: Deep reasoning.
  - name: fast
    model: claude-haiku-4-5-20251001
    description: Classification / triage.
```

After editing, `flyctl machine restart <id> -a <app>` to reload. The file is re-read on each `list_tiers` call, so restart is only needed for effect on an in-flight conversation.

## Failure modes

- Catalog unreachable → `list_models` and raw `set` return a structured error mentioning HTTP status + body.
- Unknown tier → `set_tier` returns "unknown tier 'X'. Available tiers: chat, thinking, fast".
- Unknown model ID → `set` returns "Unknown model 'X'. See available_models."
- Stale `tiers.yaml` (model name no longer served by upstream) → `set_tier` validates the resolved model against the live `/v1/models` and rejects with "not in the live catalog" before staging.

## Why tiers

- The agent thinks in capability terms (`thinking` vs `chat`) rather than memorizing dated model IDs that change as the provider ships new models.
- Operators promote a new model (e.g. `claude-opus-4-8`) by editing one line in `tiers.yaml` and restarting — no zeroclaw rebuild.
