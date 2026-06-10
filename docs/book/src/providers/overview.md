# Model Providers: Overview

Model providers are ZeroClaw's abstraction over any LLM endpoint the agent can call. Every chat-completion request goes through a `ModelProvider` trait implementation (`zeroclaw-api::ModelProvider`), whether the target is a remote API, a self-hosted inference server, or a local Ollama model.

An agent reaches a provider by referencing it; see [Agents](../agents/overview.md) for how that wiring fits together.

Why "model" provider? We use the phrase "model provider" consistently, there are also TTS providers and transcription providers, and keeping the qualifier specific avoids ambiguity.

## Configuration shape

Providers are typed by family, addressed as `providers.models.<type>.<alias>`. `<type>` is a canonical family slot (see the [Catalog](./catalog.md#all-slots) for every slot). There is one slot per vendor, with no synonyms: `azure_openai`, `azure-openai`, and `claude` (for Anthropic) are not accepted.

`<alias>` is your operator-assigned instance name, and you can define **as many aliases per type as you want**. Run several profiles of the same vendor family side by side: same `type`, different aliases, each with its own key, model, and settings. For example, two Anthropic accounts as `anthropic.personal` and `anthropic.work` (each with its own `api_key` and `model`), where an agent picks one with `model_provider = "anthropic.personal"` (or `"anthropic.work"`). Add and edit these through the surfaces below, not by hand:

{{#config-where providers.models}}

See [Configuration](./configuration.md) for the full schema and [Catalog](./catalog.md) for a worked example per family.

## Per-agent dispatch: there are no global defaults

A provider entry on its own does nothing. To use it, an agent references it by `model_provider` (along with a `risk_profile` and optional `runtime_profile`). `risk_profile` and `runtime_profile` reference independent alias maps, so their names need not match. `Config::validate()` fails loud at startup if any reference doesn't resolve. Every callsite picks a configured alias or opts out; there is no global "default provider" or "default model" knob.

For multi-agent deployments, give each agent its own `model_provider`. Channels that ingest messages bind to one agent at a time via the agent's `channels` list; see [Channels](../channels/overview.md) for the full picture.

## Per-agent voice (TTS) and transcription

Voice synthesis and speech-to-text follow the same pattern: a typed-family provider entry, then a per-agent reference. There are no global TTS or transcription selector fields. Each agent that wants voice sets its own routing.

## Where to next

- [Configuration](./configuration.md): the full `[providers.*]` schema, Azure typed config, regional and OAuth variants
- [Streaming](./streaming.md): how tokens, tool calls, and reasoning deltas flow
- [Routing](./routing.md): multi-agent dispatch and OpenRouter as a routing layer
- [Provider catalog](./catalog.md): every supported family with a worked TOML example
- [Custom providers](./custom.md): pointing the `custom` slot at an OpenAI-compatible endpoint, or implementing the `ModelProvider` trait
