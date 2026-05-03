# Fallback & Routing

Two meta-providers let you stack concrete providers into reliability pipelines and task-aware routing.

## Fallback chains — `reliable`

A `reliable` provider wraps a list of other providers. On timeout, network error, or authentication failure on the first, it transparently falls through to the next.

```toml
[providers.models.primary]
kind = "reliable"
fallback_providers = ["claude", "haiku", "local"]
# fallback_providers references other [providers.models.*] by name

[providers.models.claude]
kind = "anthropic"
model = "claude-sonnet-4-6"
api_key = "sk-ant-..."

[providers.models.haiku]
kind = "anthropic"
model = "claude-haiku-4-5-20251001"
api_key = "sk-ant-..."

[providers.models.local]
kind = "ollama"
base_url = "http://localhost:11434"
model = "qwen3.6:35b-a3b"
```

Then:

```toml
default_model = "primary"
```

Behaviour:

- Tries `claude` first; on transient error retries twice with exponential backoff (2 s, 4 s).
- After two retries exhausted, falls through to `haiku` and restarts the retry cycle.
- If `haiku` also exhausts, falls through to `local`.
- Only returns an error once every provider in the chain has failed.

**Best practice — order by reliability:** put the most reliable provider first, not the best. A Claude fallback behind a flaky local Ollama won't save you during an API outage. `[fastest/best_quality, fallback_quality, always_on_local]` is a common pattern.

## Routing — `router`

A `router` provider picks a backend per request based on hints supplied by the caller.

```toml
[providers.models.brain]
kind = "router"
default = "haiku"                       # used if no hint matches
routes = [
    { hint = "reasoning", provider = "deepseek-r1" },
    { hint = "cheap",     provider = "haiku" },
    { hint = "vision",    provider = "gemini" },
]
```

Channels, tools, and SOPs can emit hints via request metadata. For example, an SOP step might request `hint:reasoning` for a planning phase and `hint:cheap` for a summarisation phase. Everything else goes to `default`.

## Combining them

`reliable` and `router` compose. Routes can point at `reliable` providers, and `reliable` can wrap a `router`.

```toml
[providers.models.production]
kind = "reliable"
fallback_providers = ["brain", "local"]  # if routing fails, fall to local Ollama

[providers.models.brain]
kind = "router"
default = "haiku"
routes = [
    { hint = "reasoning", provider = "sonnet" },
    { hint = "cheap",     provider = "haiku" },
]
```

## Cost, performance, and reliability

Three common patterns users pick:

### Cost-optimised
```toml
fallback_providers = ["haiku", "gpt-4o-mini", "local"]
```
Cheapest hosted first, local as the final safety net.

### Quality-optimised
```toml
fallback_providers = ["opus", "sonnet", "haiku"]
```
Best model first; fall to cheaper Claude models on failure.

### Hybrid routing
```toml
routes = [
    { hint = "code",       provider = "claude-code" },
    { hint = "reasoning",  provider = "deepseek-r1" },
    { hint = "multimodal", provider = "gemini" },
]
default = "haiku"
```
Match the tool to the job.

## Observability

Fallback events and routing decisions are logged via the infra crate:

```
INFO provider=claude attempt=1 → timeout
INFO provider=claude attempt=2 → timeout
WARN provider=claude exhausted, falling back → provider=haiku
INFO router hint=reasoning → provider=deepseek-r1
```

For production deployments, wire the log output to Loki / Grafana. See [Operations → Logs & observability](../ops/observability.md).

## See also

- [Overview](./overview.md)
- [Configuration](./configuration.md)
- [Provider catalog](./catalog.md)
