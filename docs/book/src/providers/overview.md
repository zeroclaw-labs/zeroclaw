# Model Providers: Overview

Model providers are ZeroClaw's abstraction over any LLM endpoint the agent can call. Every chat-completion request goes through a `ModelProvider` trait implementation (`zeroclaw-api::ModelProvider`), whether the target is a remote API, a self-hosted inference server, or a local Ollama model.

An agent reaches a provider by referencing it; see [Agents](../agents/overview.md) for how that wiring fits together.

Why "model" provider? We use the phrase "model provider" consistently, there are also TTS providers and transcription providers, and keeping the qualifier specific avoids ambiguity.

## Configuration shape

Providers are reusable connections, addressed by an exact name under
`providers.connections`. Models are independent named entries under
`providers.models`; each model points at one connection and declares its
capability and model identifier. A model reference is an opaque key, so dots
inside a name are preserved and never interpreted as path separators. See the
[configuration guide](./configuration.md#canonical-connections-and-models) for
the complete shape.

Connections can be reused by any number of model entries, and model names are
opaque exact keys. Add and edit registry entries through the registry API or
the Web/Zerocode provider editor. The old family/profile tables are accepted
only while loading and migrating an older configuration file.

See [Configuration](./configuration.md) for the full schema and [Catalog](./catalog.md) for provider capabilities.

## Per-agent dispatch

A provider entry on its own does nothing. To use it, an agent references it by `model_provider` (along with a `risk_profile` and optional `runtime_profile`). `risk_profile` and `runtime_profile` reference independent alias maps, so their names need not match. `Config::validate()` fails loud at startup if any reference doesn't resolve. Every callsite picks a configured alias or opts out; there is no global "default provider" or "default model" knob.

For multi-agent deployments, give each agent its own `model_provider`. Channels that ingest messages bind to one agent at a time via the agent's `channels` list; see [Channels](../channels/overview.md) for the full picture.

## Speech models

Voice synthesis and speech-to-text use the same reusable connection/model
registry. Define a `tts` or `asr` model with an exact model reference and point
the agent or channel at that reference. There are no global TTS or
transcription selector fields; each consumer chooses its own capability model.

## Where to next

- [Configuration](./configuration.md): the full `[providers.*]` schema, Azure typed config, regional and OAuth variants
- [Streaming](./streaming.md): how tokens, tool calls, and reasoning deltas flow
- [Routing](./routing.md): multi-agent dispatch and OpenRouter as a routing layer
- [Provider catalog](./catalog.md): every supported family with a worked TOML example
- [Custom providers](./custom.md): pointing the `custom` slot at an OpenAI-compatible endpoint, or implementing the `ModelProvider` trait
