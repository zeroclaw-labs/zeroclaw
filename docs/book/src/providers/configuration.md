# Provider Configuration

Every model provider lives at `[providers.models.<type>.<alias>]` in `~/.zeroclaw/config.toml`. `<type>` is the canonical family slot (`anthropic`, `openai`, `azure`, `gemini`, `groq`, `moonshot`, ...). `<alias>` is your operator-assigned instance name, pick any descriptive name (`home`, `work`, `cn`, `gpt5`, ...).

## Minimal working example

The smallest config that loads clean has four section headers: a provider entry, an agent that references it, and a risk profile the agent gates against:

```toml
{{#include ../_snippets/minimal-config.toml}}
```

The aliases (`home`, `assistant`) above are example names, substitute whatever suits your install.

## Field reference: provider entry

```toml
[providers.models.<type>.<alias>]
model = "<model-id>"        # passed to the provider as the model selector
```

Almost every family also takes:

```toml
api_key = "..."             # or use the secrets store, or a provider-specific env var
uri     = "https://..."     # optional operator override; otherwise the family's typed endpoint enum supplies the URL
```

## Field resolution order

For every family, the URL is resolved in this order:

1. **Operator override**: `uri` field on the alias entry, if set.
2. **Family endpoint**: the family's `*Endpoint` enum supplies the URL (e.g. `OpenAIEndpoint::Default` -> `https://api.openai.com/v1`). Multi-region families have an `endpoint` field on the alias entry that picks the variant (e.g. `endpoint = "cn"` for Moonshot).
3. **Templated families**: Azure and Bedrock take typed inputs (`resource`, `deployment`, `api_version` for Azure; `region` for Bedrock) and substitute them into the family's URI template. Missing fields fail loud at runtime.

## Family slots

Run `cargo doc --open -p zeroclaw-config` (or read [`crates/zeroclaw-config/src/providers.rs`](https://github.com/zeroclaw-labs/zeroclaw/blob/master/crates/zeroclaw-config/src/providers.rs)) for the complete list. Highlights:

| Slot | Notes |
|---|---|
| `anthropic` | API key or OAuth (`sk-ant-oat-*`) |
| `openai` | GPT, o-series; the OpenAI Codex subscription variant is `providers.models.openai.<alias>` with `wire_api = "responses"` and `requires_openai_auth = true` |
| `azure` | Typed: `resource`, `deployment`, `api_version`, all set on the alias entry |
| `gemini` | Google's API; `gemini_cli` is the CLI-shells-out variant |
| `bedrock` | AWS-credentials chain, region template |
| `ollama` | Local inference; `uri` defaults to `http://localhost:11434` |
| `openrouter` | Multi-vendor routing layer (treated as a single provider; see [Routing](./routing.md)) |
| `groq`, `mistral`, `xai`, `deepseek`, ... | OpenAI-compatible endpoints, each with its own canonical slot |
| `moonshot`, `qwen`, `glm`, `minimax`, `zai`, `doubao`, ... | Multi-region families; pick the region with `endpoint = "<variant>"` on the alias entry |
| `lmstudio`, `llamacpp`, `sglang`, `vllm`, `osaurus`, `litellm` | Local-server defaults (`http://localhost:<port>/v1`) |
| `custom` | Catch-all for OpenAI-compatible endpoints not covered above; `uri` is required |

There is one canonical key per vendor: no synonyms.

## Credentials

Three ways to supply credentials, in resolution order:

1. **Inline `api_key = "..."`** in the alias entry (fine for dev, risky for checked-in configs).
2. **Config-level secrets store**: encrypted at `~/.zeroclaw/secrets` via a local key file.
3. **Generic env override**: `ZEROCLAW_providers__models__<type>__<alias>__api_key=...` sets `providers.models.<type>.<alias>.api_key` at startup. See [Environment variables](../reference/env-vars.md) for the full grammar.

`zeroclaw quickstart` writes credentials to the secrets store by default. Configs you commit should not contain inline keys. For ecosystem-default names you already export in your shell (`$ANTHROPIC_API_KEY`, `$OPENROUTER_API_KEY`, …), the [env-vars reference](../reference/env-vars.md#bridging-ecosystem-default-env-vars) shows the one-line bash expansions that point a schema-mirror name at the existing value.

## OAuth and subscription auth

Several providers accept OAuth or subscription-style tokens instead of raw API keys. Get the token from the vendor's own dashboard or CLI flow, then drop it into the alias entry the same way you would an API key:

- **Anthropic**: `sk-ant-oat-*` OAuth tokens (from Claude Pro/Team) go in `api_key` on `[providers.models.anthropic.<alias>]`.
- **OpenAI Codex subscription**: set `requires_openai_auth = true` and leave `api_key` unset on `[providers.models.openai.<alias>]`; the runtime reads the stored Codex login.
- **Gemini CLI**: `[providers.models.gemini_cli.<alias>]` shells out to the `gemini` CLI; use the CLI's own auth flow.
- **Qwen / MiniMax**: set `auth_mode = "oauth"` on the alias entry plus the relevant `oauth_*` fields (see [env-vars → OAuth and CLI-path fields](../reference/env-vars.md#oauth-and-cli-path-fields)).

## Container-friendly overrides

When ZeroClaw runs inside a container and a provider is on the host (e.g. Ollama), set `uri` to a host-reachable address:

```toml
[providers.models.ollama.local]
uri   = "http://host.docker.internal:11434"
model = "qwen3.6:35b-a3b"
```

The generic env-override mechanism (`ZEROCLAW_<dotted_path_with_double_underscores>=<value>`) can set the same field at runtime without editing `config.toml`:

<div class="os-tabs-src">

#### sh

```sh
ZEROCLAW_providers__models__ollama__home__uri=http://ollama:11434 zeroclaw agent -a assistant
```

</div>

The `__` is the path separator; the example above sets `providers.models.ollama.home.uri`. See [Environment variables](../reference/env-vars.md) for the full grammar.

## Per-family knobs: worked examples

### Ollama

```toml
[providers.models.ollama.local]
uri              = "http://localhost:11434"
model            = "qwen3.6:35b-a3b"
think            = false                    # disable reasoning mode for faster output
reasoning_effort = "none"                   # same intent, passed as a top-level field
options          = { temperature = 0, num_ctx = 32768 }
```

### Azure OpenAI

```toml
[providers.models.azure.work]
resource    = "my-resource"                 # template var: https://{resource}.openai.azure.com/...
deployment  = "gpt-4o"
api_version = "2024-10-01-preview"
api_key     = "..."
```

The `resource`, `deployment`, and `api_version` values live in this typed config, they are not read from environment variables.

### Multi-region (Moonshot / Qwen / GLM / MiniMax / ...)

Pick the region with the typed `endpoint` field on the alias entry:

```toml
[providers.models.moonshot.cn]
api_key  = "..."
endpoint = "cn"                             # MoonshotEndpoint::Cn -> https://api.moonshot.cn/v1

[providers.models.moonshot.intl]
api_key  = "..."
endpoint = "intl"                           # MoonshotEndpoint::Intl -> https://api.moonshot.ai/v1
```

One type per family; region picks via the `endpoint` field on the alias entry.

### Custom OpenAI-compatible endpoint

```toml
[providers.models.custom.gateway]
uri     = "https://my-gateway.example.com/v1"
model   = "my-model-id"
api_key = "..."
```

The `custom` slot requires `uri`. See [Custom providers](./custom.md).

## Picking which provider an agent uses

Agents reference a provider by dotted alias. Provider entries on their own do nothing.

```toml
[agents.assistant]
model_provider  = "anthropic.home"   # `<type>.<alias>` into providers.models
risk_profile    = "hardened"         # alias into risk_profiles.<alias>
runtime_profile = "deep"             # alias into runtime_profiles.<alias>; independent of risk_profile
```

`risk_profile` and `runtime_profile` reference independent alias maps, so their names need not match (`runtime_profile` is also optional). `Config::validate()` fails loud at startup if `model_provider` doesn't resolve to a configured `[providers.models.<type>.<alias>]` entry, or if `risk_profile` doesn't resolve to a configured `[risk_profiles.<alias>]` entry.

For multiple agents pointing at different providers, see [Routing](./routing.md).

## Fallback on failure

When a request to a provider fails after exhausting its retries (provider down,
key rate-limited, model unavailable), the alias can fall over to alternatives
you declare on the alias entry. Two independent, ordered axes:

```toml
[providers.models.anthropic.prod]
model           = "claude-sonnet-4-5"
fallback_models = ["claude-haiku-4-5"]   # same provider, alternate models
fallback        = ["openai.backup"]      # other aliases, each with its own key/endpoint

[providers.models.openai.backup]
model = "gpt-4.1"
```

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
`fallback_models` and `fallback` recursively. For the example above:

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
