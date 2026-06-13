# Provider Configuration

Every model provider lives at `[providers.models.<type>.<alias>]`. `<type>` is a canonical family slot (see the [Catalog](./catalog.md#all-slots) for every slot with its endpoint). `<alias>` is your operator-assigned instance name, pick any descriptive name (`home`, `work`, `cn`, `gpt5`, ...).

## Minimal working example

The smallest config that loads clean has four section headers: a provider entry, an agent that references it, and a risk profile the agent gates against. Configure them through the gateway, zerocode, or `zeroclaw config set`; the [config reference](../reference/config.md#providers) has the full field index.

## Field reference: provider entry

Almost every family also takes:

## Field resolution order

For every family, the URL is resolved in this order:

1. **Operator override**: `uri` field on the alias entry, if set.
2. **Family endpoint**: the family's `*Endpoint` enum supplies the URL (e.g. `OpenAIEndpoint::Default` -> `https://api.openai.com/v1`). Multi-region families have an `endpoint` field on the alias entry that picks the variant (e.g. `endpoint = "cn"` for Moonshot).
3. **Templated families**: Azure and Bedrock take typed inputs (`resource`, `deployment`, `api_version` for Azure; `region` for Bedrock) and substitute them into the family's URI template. Missing fields fail loud at runtime.

## Family slots

Every slot, its default endpoint, and whether it runs locally is in the [Catalog](./catalog.md#all-slots). There is one canonical key per vendor: no synonyms.

## Credentials

Supported credential input and storage forms:

1. **Inline `api_key = "..."`** in the alias entry (fine for dev, risky for checked-in configs).
2. **1Password references**: set a secret field to `op://vault/item/field`. ZeroClaw keeps the reference in config and resolves it at runtime with `op read`, so the 1Password CLI must be installed and signed in.
3. **Config-level secrets store**: encrypted at `~/.zeroclaw/secrets` via a local key file.
4. **Generic env override**: `ZEROCLAW_providers__models__<type>__<alias>__api_key=...` sets `providers.models.<type>.<alias>.api_key` at startup. See [Environment variables](../reference/env-vars.md) for the full grammar.

Schema-mirror env overrides win at startup. They replace the in-memory credential for that process without rewriting the stored inline, encrypted, or `op://` value on disk.

`zeroclaw quickstart` writes credentials to the secrets store by default. Configs you commit should not contain inline keys. For ecosystem-default names you already export in your shell (`$ANTHROPIC_API_KEY`, `$OPENROUTER_API_KEY`, …), the [env-vars reference](../reference/env-vars.md#bridging-ecosystem-default-env-vars) shows the one-line bash expansions that point a schema-mirror name at the existing value.

## OAuth and subscription auth

Several providers accept OAuth or subscription-style tokens instead of raw API keys. Get the token from the vendor's own dashboard or CLI flow, then drop it into the alias entry the same way you would an API key:

- **Anthropic**: `sk-ant-oat-*` OAuth tokens (from Claude Pro/Team) go in `api_key` on `[providers.models.anthropic.<alias>]`.
- **OpenAI Codex subscription**: set `requires_openai_auth = true` and leave `api_key` unset on `[providers.models.openai.<alias>]`; the runtime reads the stored Codex login.
- **Gemini CLI**: `[providers.models.gemini_cli.<alias>]` shells out to the `gemini` CLI; use the CLI's own auth flow.
- **Qwen / MiniMax**: set `auth_mode = "oauth"` on the alias entry plus the relevant `oauth_*` fields (see [env-vars → OAuth and CLI-path fields](../reference/env-vars.md#oauth-and-cli-path-fields)).

## Container-friendly overrides

When ZeroClaw runs inside a container and a provider is on the host (e.g. Ollama), set `uri` to a host-reachable address. The generic env-override mechanism (`ZEROCLAW_<dotted_path_with_double_underscores>=<value>`) can set the same field at runtime without editing config:

{{#env-var container}}

The `__` is the path separator; the example above sets `providers.models.ollama.home.uri`. See [Environment variables](../reference/env-vars.md) for the full grammar.

## Per-family knobs: worked examples

### Ollama

### Azure OpenAI

The `resource`, `deployment`, and `api_version` values live in this typed config, they are not read from environment variables.

### Multi-region (Moonshot / Qwen / GLM / MiniMax / ...)

One type per family; pick the region via the typed `endpoint` field on the alias entry.

### Custom OpenAI-compatible endpoint

The `custom` slot requires `uri`. See [Custom providers](./custom.md).

## Picking which provider an agent uses

Agents reference a provider by dotted alias. Provider entries on their own do nothing.

`risk_profile` and `runtime_profile` reference independent alias maps, so their names need not match (`runtime_profile` is also optional). `Config::validate()` fails loud at startup if `model_provider` doesn't resolve to a configured `[providers.models.<type>.<alias>]` entry, or if `risk_profile` doesn't resolve to a configured `[risk_profiles.<alias>]` entry.

For multiple agents pointing at different providers, see [Routing](./routing.md).

## Fallback on failure

When a request to a provider fails after exhausting its retries (provider down,
key rate-limited, model unavailable), the alias can fall over to alternatives
you declare on the alias entry. Two independent, ordered axes:

- **`fallback_models`**: alternate model IDs tried on *this* provider, using the
  same endpoint, key, and headers. Only the model identifier changes. Use it when
  a provider serves a backup model (a smaller or older variant) that should be
  tried before leaving the provider entirely.
- **`fallback`**: an ordered list of *other* provider aliases (dotted
  `<type>.<alias>` references into `[providers.models]`). Each fallback alias
  resolves with **its own** credentials, endpoint, and model, a fallback never
  inherits the failing alias's key.

### Order of attempts

The walk is depth-first: an alias's entire model list is exhausted before leaving
it, then each `fallback` alias is descended in turn, applying that alias's own
`fallback_models` and `fallback` recursively. Suppose `anthropic.prod` serves
`claude-sonnet-4-5`, lists `claude-haiku-4-5` in its `fallback_models`, and
names `openai.backup` (serving `gpt-4.1`) in its `fallback`. The attempt order
is then:

```
anthropic.prod/claude-sonnet-4-5
  -> anthropic.prod/claude-haiku-4-5
  -> openai.backup/gpt-4.1
  -> (request fails)
```

Fallback aliases can themselves declare `fallback`, so the chain is as long as
your config makes it, up to a maximum depth of **3 aliases**. A chain that loops
back on itself (`a` -> `b` -> `a`) is detected and the cycle edge is pruned, and
an acyclic chain deeper than the limit has its remaining links pruned; neither
ever loops, hangs, or overflows the stack.

### Misconfiguration

A `fallback` entry that names an alias which is not configured, one that closes a
cycle, or a chain that exceeds the maximum depth is **non-fatal**:
`Config::validate()` still succeeds, the offending edge is skipped at runtime, and
the issue is surfaced as a validation warning (`dangling_fallback_ref` /
`fallback_cycle` / `max_fallback_depth_exceeded`) on the CLI and in the dashboard.
A `fallback_models` entry that is blank or duplicates the alias's primary `model`
is likewise skipped at runtime and surfaced (`empty_fallback_model` /
`fallback_model_duplicates_primary`). A bad fallback link degrades gracefully, it
never prevents the agent from running.

## See also

- [Overview](./overview.md)
- [Provider catalog](./catalog.md): concrete config example for every family
- [Streaming](./streaming.md)
- [Routing](./routing.md)
- [Custom providers](./custom.md)
