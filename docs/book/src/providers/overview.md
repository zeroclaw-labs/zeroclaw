# Model Providers — Overview

Model providers are ZeroClaw's abstraction over any LLM endpoint the agent can call. Every chat-completion request goes through a `ModelProvider` trait implementation (`zeroclaw-api::ModelProvider`), whether the target is a remote API, a self-hosted inference server, or a local Ollama model.

Why "model" provider? We use the phrase "model provider" consistently — there are also TTS providers and transcription providers, and keeping the qualifier specific avoids ambiguity.

## Configuration shape

Providers are typed by family. Every entry lives at:

```toml
[providers.models.<type>.<alias>]
```

`<type>` is the canonical family slot (`anthropic`, `openai`, `azure`, `gemini`, `ollama`, `openrouter`, `groq`, `moonshot`, ...). There is one slot per vendor, with no synonyms — `azure_openai`, `azure-openai`, and `claude` (for Anthropic) are not accepted.

`<alias>` is your operator-assigned instance name. Use it to distinguish multiple instances of the same provider — for example, `[providers.models.openai.work]` and `[providers.models.openai.personal]` use different keys against the same vendor.

```toml
[providers.models.anthropic.home]
model = "claude-haiku-4-5-20251001"
api_key = "sk-ant-..."

[providers.models.ollama.local]
uri = "http://localhost:11434"
model = "qwen3.6:35b-a3b"

[providers.models.groq.fast]
model = "llama-3.3-70b-versatile"
api_key = "gsk_..."
```

See [Configuration](./configuration.md) for the full schema and [Catalog](./catalog.md) for a worked example per family.

## Per-agent dispatch — there are no global defaults

A provider entry on its own does nothing. To use it, name it from an agent:

```toml
[agents.assistant]
model_provider  = "anthropic.home"   # references [providers.models.anthropic.home]
risk_profile    = "hardened"         # references [risk_profiles.hardened]
runtime_profile = "deep"             # references [runtime_profiles.deep]; independent of risk_profile
```

`risk_profile` and `runtime_profile` reference independent alias maps, so their names need not match (`runtime_profile` is also optional). `Config::validate()` fails loud at startup if any reference doesn't resolve. Every callsite picks a configured alias or opts out — there is no global "default provider" or "default model" knob.

For multi-agent deployments, give each agent its own `model_provider`:

```toml
[agents.researcher]
model_provider = "anthropic.home"

[agents.summariser]
model_provider = "groq.fast"
```

Channels that ingest messages bind to one agent at a time via the agent's `channels = [...]` list — see [Channels](../channels/) for the full picture.

## Per-agent voice (TTS) and transcription

Voice synthesis and speech-to-text follow the same pattern: typed-family entry, then a per-agent reference.

```toml
[providers.tts.openai.alloy]
api_key = "sk-..."
voice   = "alloy"

[providers.transcription.groq.fast]
api_key = "gsk_..."

[agents.assistant]
model_provider         = "anthropic.home"
tts_provider           = "openai.alloy"        # empty string = no TTS for this agent
transcription_provider = "groq.fast"           # empty string = agent has no STT preference
```

There are no global TTS or transcription selector fields. Each agent that wants voice sets its own routing.

## Where to next

- [Configuration](./configuration.md) — the full `[providers.*]` schema, Azure typed config, regional and OAuth variants
- [Streaming](./streaming.md) — how tokens, tool calls, and reasoning deltas flow
- [Routing](./routing.md) — multi-agent dispatch and OpenRouter as a routing layer
- [Provider catalog](./catalog.md) — every supported family with a worked TOML example
- [Custom providers](./custom.md) — pointing the `custom` slot at an OpenAI-compatible endpoint, or implementing the `ModelProvider` trait
