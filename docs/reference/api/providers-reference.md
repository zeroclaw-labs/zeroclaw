# ZeroClaw Providers Reference

This document maps provider IDs, aliases, and credential environment variables.

Last verified: **April 7, 2026**.

## How to List Providers

```bash
zeroclaw providers
```

## Credential Resolution Order

Runtime resolution order is:

1. Explicit credential from config/CLI
2. Provider-specific env var(s)
3. Generic fallback env vars: `ZEROCLAW_API_KEY` then `API_KEY`

For resilient fallback chains (`reliability.fallback_providers`), each fallback
provider resolves credentials independently. The primary provider's explicit
credential is not reused for fallback providers.

## Provider Catalog

| Canonical ID | Aliases | Local | Provider-specific env var(s) |
|---|---|---:|---|
| `openrouter` | — | No | `OPENROUTER_API_KEY` |
| `anthropic` | — | No | `ANTHROPIC_OAUTH_TOKEN`, `ANTHROPIC_API_KEY` |
| `openai` | — | No | `OPENAI_API_KEY` |
| `gemini` | `google`, `google-gemini` | No | `GEMINI_API_KEY`, `GOOGLE_API_KEY` |

### Gemini Notes

- Provider ID: `gemini` (aliases: `google`, `google-gemini`)
- Auth can come from `GEMINI_API_KEY`, `GOOGLE_API_KEY`, or Gemini CLI OAuth cache (`~/.gemini/oauth_creds.json`)
- API key requests use `generativelanguage.googleapis.com/v1beta`
- Gemini CLI OAuth requests use `cloudcode-pa.googleapis.com/v1internal` with Code Assist request envelope semantics
- Thinking models (e.g. `gemini-3-pro-preview`) are supported — internal reasoning parts are automatically filtered from the response

## Custom Endpoints

- Anthropic-compatible endpoint:

```toml
default_provider = "anthropic-custom:https://your-api.example.com"
```

## Model Routing (`hint:<name>`)

You can route model calls by hint using `[[model_routes]]`:

```toml
[[model_routes]]
hint = "reasoning"
provider = "openrouter"
model = "anthropic/claude-opus-4-20250514"

[[model_routes]]
hint = "fast"
provider = "openai"
model = "gpt-4o-mini"
```

Then call with a hint model name (for example from tool or integration paths):

```text
hint:reasoning
```

## Embedding Routing (`hint:<name>`)

You can route embedding calls with the same hint pattern using `[[embedding_routes]]`.
Set `[memory].embedding_model` to a `hint:<name>` value to activate routing.

```toml
[memory]
embedding_model = "hint:semantic"

[[embedding_routes]]
hint = "semantic"
provider = "openai"
model = "text-embedding-3-small"
dimensions = 1536
```

Supported embedding providers:

- `none`
- `openai`
- `custom:<url>` (OpenAI-compatible embeddings endpoint)

Optional per-route key override:

```toml
[[embedding_routes]]
hint = "semantic"
provider = "openai"
model = "text-embedding-3-small"
api_key = "sk-route-specific"
```

## Upgrading Models Safely

Use stable hints and update only route targets when providers deprecate model IDs.

Recommended workflow:

1. Keep call sites stable (`hint:reasoning`, `hint:semantic`).
2. Change only the target model under `[[model_routes]]` or `[[embedding_routes]]`.
3. Run:
   - `zeroclaw doctor`
   - `zeroclaw status`
4. Smoke test one representative flow (chat + memory retrieval) before rollout.

This minimizes breakage because integrations and prompts do not need to change when model IDs are upgraded.
