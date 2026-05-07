# Provider Configuration

Every model provider lives at `[providers.models.<type>.<alias>]` in `~/.zeroclaw/config.toml`. `<type>` is the canonical family slot (`anthropic`, `openai`, `azure`, `gemini`, `groq`, `moonshot`, ...). `<alias>` is your operator-assigned instance name (`default`, `work`, `cn`, ...).

## Minimum shape

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

1. **Operator override** — `uri` field on the alias entry, if set.
2. **Family endpoint** — the family's `*Endpoint` enum supplies the URL (e.g. `OpenAIEndpoint::Default` -> `https://api.openai.com/v1`). Multi-region families have an `endpoint` field on the alias entry that picks the variant (e.g. `endpoint = "cn"` for Moonshot).
3. **Templated families** — Azure and Bedrock take typed inputs (`resource`, `deployment`, `api_version` for Azure; `region` for Bedrock) and substitute them into the family's URI template. Missing fields fail loud at runtime.

## Family slots

Run `cargo doc --open -p zeroclaw-config` (or read [`crates/zeroclaw-config/src/providers.rs`](https://github.com/zeroclaw-labs/zeroclaw/blob/master/crates/zeroclaw-config/src/providers.rs)) for the complete list. Highlights:

| Slot | Notes |
|---|---|
| `anthropic` | API key or OAuth (`sk-ant-oat-*`); supports Claude Code via the `claude_code` slot |
| `openai` | GPT, o-series; OpenAI Codex variants live on `openai_codex` |
| `azure` | Typed: `resource`, `deployment`, `api_version`. Env-var read path is gone — values must live in config |
| `gemini` | Google's API; `gemini_cli` is the CLI-shells-out variant |
| `bedrock` | AWS-credentials chain, region template |
| `ollama` | Local inference; `uri` defaults to `http://localhost:11434` |
| `openrouter` | Multi-vendor routing layer (treated as a single provider; see [Fallback & routing](./fallback-and-routing.md)) |
| `groq`, `mistral`, `xai`, `deepseek`, ... | OpenAI-compatible endpoints, each with its own canonical slot |
| `moonshot`, `qwen`, `glm`, `minimax`, `zai`, `doubao`, ... | Multi-region families; pick the region with `endpoint = "<variant>"` on the alias entry |
| `lmstudio`, `llamacpp`, `sglang`, `vllm`, `osaurus`, `litellm` | Local-server defaults (`http://localhost:<port>/v1`) |
| `custom` | Catch-all for OpenAI-compatible endpoints not covered above; `uri` is required |

Synonyms are gone — there is one canonical key per vendor. The migration auto-renames `azure_openai`/`azure-openai` -> `azure`, `claude` -> `anthropic`, `google`/`google-gemini` -> `gemini`, etc.

## Credentials

Four ways to supply credentials, in resolution order:

1. **Inline `api_key = "..."`** in the alias entry (fine for dev, risky for checked-in configs).
2. **Config-level secrets store** — encrypted at `~/.zeroclaw/secrets` via a local key file.
3. **Provider-specific env var** — `ANTHROPIC_API_KEY`, `ANTHROPIC_OAUTH_TOKEN`, `OPENAI_API_KEY`, `OPENROUTER_API_KEY`, `GROQ_API_KEY`, etc.
4. **Generic fallback** — `ZEROCLAW_API_KEY`, `API_KEY`.

The onboarding wizard writes credentials to the secrets store by default. Configs you commit should use neither inline keys nor `env_passthrough` entries that leak user keys.

## OAuth and subscription auth

Several providers support OAuth or subscription-style tokens instead of raw API keys:

- **Anthropic** — `sk-ant-oat-*` OAuth tokens work anywhere an API key does. No cost if you're on a Pro/Team plan.
- **GitHub Copilot** — authenticate via the onboarding wizard (`zeroclaw onboard`) which handles the OAuth flow. The token is stored in the secrets backend.
- **Gemini CLI** — uses the `gemini` CLI's existing auth.
- **Claude Code** — uses your Claude Code login.
- **Qwen / MiniMax / Kimi-Code** — set `auth_mode = "oauth"` on the alias entry to switch from API key to OAuth.

## Container-friendly overrides

The onboarding wizard detects Docker / Podman / Kubernetes and rewrites `localhost` to container-appropriate hostnames:

```toml
[providers.models.ollama.default]
uri   = "http://host.docker.internal:11434"   # was "http://localhost:11434" on host
model = "qwen3.6:35b-a3b"
```

You can also force this manually at runtime:

```bash
ZEROCLAW_OLLAMA_URI=http://ollama:11434 zeroclaw agent --agent default
```

## Per-family knobs — worked examples

### Ollama

```toml
[providers.models.ollama.default]
uri              = "http://localhost:11434"
model            = "qwen3.6:35b-a3b"
think            = false                    # disable reasoning mode for faster output
reasoning_effort = "none"                   # same intent, passed as a top-level field
options          = { temperature = 0, num_ctx = 32768 }
```

### Azure OpenAI

```toml
[providers.models.azure.default]
resource    = "my-resource"                 # template var: https://{resource}.openai.azure.com/...
deployment  = "gpt-4o"
api_version = "2024-10-01-preview"
api_key     = "..."
```

The `AZURE_OPENAI_RESOURCE` / `_DEPLOYMENT` / `_API_VERSION` environment variables are no longer read at runtime — values must live in this typed config. Migration auto-renames the legacy `azure_openai_*` field names to the unprefixed forms.

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

The previous `moonshot-cn` / `moonshot-intl` outer keys are gone — one type per family, region in the alias entry.

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
[agents.default]
enabled        = true
model_provider = "anthropic.default"
risk_profile   = "default"
runtime_profile = "default"
```

`Config::validate()` fails loud at startup if `model_provider` doesn't resolve to a configured `[providers.models.<type>.<alias>]` entry. There is no `default_provider` / `default_model` / `fallback_providers` concept.

For multiple agents pointing at different providers, see [Fallback & routing](./fallback-and-routing.md).

## See also

- [Overview](./overview.md)
- [Provider catalog](./catalog.md) — concrete config example for every family
- [Streaming](./streaming.md)
- [Fallback & routing](./fallback-and-routing.md)
- [Custom providers](./custom.md)
