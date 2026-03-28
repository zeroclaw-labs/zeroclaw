# AGENTS.md — providers/

> LLM model provider integrations and resilience layer.

## Overview

This subsystem wraps every supported LLM API behind a unified `Provider` trait.
Providers range from first-party implementations (Anthropic, OpenAI, Gemini, Ollama)
to the generic `OpenAiCompatibleProvider` (compatible.rs) which covers 20+ backends
sharing the `/v1/chat/completions` shape. On top sit two composition layers:
`ReliableProvider` (retry/fallback/key-rotation) and `RouterProvider` (task-hint routing).

## Key Files

| File | Purpose |
|------|---------|
| `traits.rs` | `Provider` trait, `ChatResponse`, `TokenUsage`, `ProviderCapabilities`, `ToolsPayload` enum, streaming types |
| `mod.rs` | Factory functions (`create_provider*`), alias resolution (`is_*_alias`), credential resolution, `ProviderRuntimeOptions` |
| `compatible.rs` | `OpenAiCompatibleProvider` — generic OpenAI-shaped backend with `AuthStyle`, vision, tool-schema fallback |
| `reliable.rs` | `ReliableProvider` — retry, exponential backoff, API key rotation, model fallback chains, context-window truncation |
| `router.rs` | `RouterProvider` — hint-based multi-model routing across providers |
| `anthropic.rs` | Anthropic Messages API with native tool calling, prompt caching (`cache_control`), vision |
| `openai.rs` | OpenAI Chat Completions with native tools, `reasoning_content` pass-through |
| `gemini.rs` | Google Gemini with multi-auth (API key, Gemini CLI OAuth, ADC, managed profiles) |
| `ollama.rs` | Ollama local models with `think` parameter, vision via multimodal, native tool calling |

## Trait Contract

`Provider` (traits.rs) requires only `chat_with_system`. Everything else has defaults:

- `capabilities()` → `ProviderCapabilities::default()` (no native tools, no vision, no caching)
- `chat()` → structured agent-loop entry; auto-injects tools into system prompt if `supports_native_tools()` is false
- `chat_with_tools()` → falls back to `chat_with_history` returning empty `tool_calls`
- `convert_tools()` → returns `ToolsPayload::PromptGuided` (XML `<tool_call>` protocol)
- `warmup()` → no-op; override to pre-warm HTTP/2 connection pools
- Streaming methods default to unsupported

Override `capabilities()` to declare `native_tool_calling`, `vision`, `prompt_caching`.

## Extension Playbook

1. Create `src/providers/<name>.rs` with a struct holding `credential` + `base_url` (or reuse `OpenAiCompatibleProvider`)
2. Implement `Provider` — at minimum `chat_with_system`; override `capabilities()`, `chat()`, `chat_with_tools()`, `convert_tools()` for native tool support
3. Add `pub mod <name>;` to `mod.rs` module declarations (alphabetical order)
4. Register in `create_provider_with_url_and_options()` match arm with all aliases
5. If the API is OpenAI-shaped: prefer `OpenAiCompatibleProvider::new_with_options()` — set `AuthStyle`, vision, `merge_system_into_user`, `native_tool_calling`
6. Add alias functions `is_<name>_alias()` if the provider has regional variants or multiple brand names
7. Add env var resolution in `resolve_provider_credential()` if a dedicated env var is expected
8. Add integration test gated behind `#[cfg(test)]` with mock HTTP or skip-in-CI guard

## Factory Registration

`create_provider_with_url_and_options()` is the central match. Pattern:
- First-class providers (anthropic, openai, gemini, ollama) have dedicated modules
- Compatible providers are constructed via the `compat()` closure which applies `ProviderRuntimeOptions` (timeout, reasoning effort, extra headers, api_path)
- Alias functions (`is_minimax_alias`, `is_qwen_alias`, etc.) group regional/brand variants
- Credential resolution uses `resolve_provider_credential()` which checks config key → env var → None, with provider-specific env var names

## Token Tracking & Caching

- `TokenUsage` has `input_tokens`, `output_tokens`, `cached_input_tokens` (all `Option<u64>`)
- Anthropic maps `cache_read_input_tokens`; OpenAI maps `prompt_tokens_details.cached_tokens`
- `ProviderCapabilities::prompt_caching` declares support; only Anthropic sets it `true` currently
- Anthropic uses `cache_control` blocks in system prompts; OpenAI caching is automatic/transparent
- Ollama reports tokens via `prompt_eval_count` / `eval_count`

## Thinking Model Integration

Several providers return `reasoning_content` (DeepSeek-R1, Kimi, GLM-4.7, QwQ):
- `ChatResponse.reasoning_content` stores raw thinking; `ConversationMessage::AssistantToolCalls` preserves it for round-trip fidelity
- OpenAI-compatible: `ResponseMessage.reasoning_content` field, with `effective_content()` falling back to it when `content` is empty
- Ollama: mapped from `thinking` field in response
- **Round-trip requirement**: some providers reject tool-call history that omits `reasoning_content` — always include it in subsequent requests
- The agent loop must avoid leaking thinking-level prefixes across conversation turns (see commit `ffb8b81f`)

## Resilience Wrapper

`ReliableProvider` implements three-level failover:
1. **Model chain** — original model first, then `model_fallbacks` alternatives
2. **Provider chain** — iterate registered providers in priority order
3. **Retry loop** — exponential backoff per (provider, model) pair with API key rotation

Error classification drives behavior:
- `is_non_retryable()` → abort immediately (auth failure, model not found, 4xx except 429/408)
- `is_rate_limited()` → rotate API key, respect `Retry-After` header, continue retrying
- `is_non_retryable_rate_limit()` → quota/plan errors disguised as 429 → skip to next provider
- `is_context_window_exceeded()` → truncate oldest non-system messages by half, retry same provider
- `is_tool_schema_error()` → NOT non-retryable; `compatible.rs` can recover via prompt-guided fallback

`compatible.rs` has its own recovery: `is_native_tool_schema_unsupported()` detects 400/422 errors about unknown tool parameters and falls back to `PromptGuided` tool instructions.

## Testing Patterns

- Every provider file has `#[cfg(test)] mod tests` with unit tests
- Trait tests in `traits.rs` use `MockProvider` / `EchoSystemProvider` / `CapabilityMockProvider`
- `reliable.rs` tests error classification functions (`is_non_retryable`, `is_rate_limited`, etc.)
- `compatible.rs` tests tool-schema-unsupported detection
- `mod.rs` tests alias resolution and credential routing
- For new providers: test `capabilities()`, `convert_tools()`, `chat_with_tools()` response parsing, error mapping

## Common Gotchas

- **ToolsPayload mismatch**: if `supports_native_tools()` returns false but `convert_tools()` returns a non-`PromptGuided` variant, `chat()` will bail with an error
- **Missing reasoning_content round-trip**: dropping it from `AssistantToolCalls` breaks some providers' tool-call history validation
- **MAX_API_ERROR_CHARS**: `mod.rs` truncates error bodies to 200 chars via `sanitize_api_error` — errors beyond this are lost in diagnostics
- **merge_system_into_user**: some providers (MiniMax) reject `role: system` — the compatible provider has a flag for this
- **Auth pre-flight**: `check_api_key_prefix()` catches obvious key/provider mismatches early (e.g., `sk-ant-*` key used with OpenAI)
- **Alias sprawl**: adding a provider without covering all regional aliases causes silent fallthrough to the wrong backend

## Cross-Subsystem Coupling

- **agent/** — consumes `Provider` via `ChatRequest`/`ChatResponse`; manages `ConversationMessage` history including `reasoning_content` preservation
- **config/** — provides `provider`, `model`, `api_key`, `api_url`, `provider_timeout`, `reasoning_effort`, `extra_headers`, `model_fallbacks`
- **tools/** — `ToolSpec` is the canonical tool definition; providers convert it via `convert_tools()` → `ToolsPayload`
- **auth/** — `AuthService` handles OAuth token refresh for Gemini managed profiles and Qwen OAuth
- **observability/** — token usage flows through `ChatResponse.usage` to observers for cost tracking
