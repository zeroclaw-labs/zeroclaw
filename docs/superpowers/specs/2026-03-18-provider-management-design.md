# Provider Management — Full Bot Capabilities

**Date:** 2026-03-18
**Status:** Draft
**Scope:** Telegram bot provider management via natural language

## Problem

The ZeroClaw Telegram bot has provider-manager tools (`provider_status`, `provider_find`, `provider_apply`, `provider_health`) but lacks:
1. **Model testing** — no way to test a specific model or validate a specific key through the bot
2. **Model listing** — no way to ask "what models does provider X support?"
3. **Fallback removal** — can add fallbacks but not remove them
4. **TOOLS.md gaps** — bootstrap instructions don't cover all workflows, no multi-turn examples
5. **No E2E test coverage** — zero bot-level tests for provider management

## Design

### New Tools

#### `provider_test(provider, model, key?, prompt?)`

Tests a model by sending a **chat completion request** (not a quota/models endpoint check). This is fundamentally different from `quota.check_one()` which only pings the provider's health endpoint.

- **provider** (required): provider name (e.g. "deepseek", "gemini", "openai")
- **model** (required): model ID (e.g. "deepseek-chat", "gemini-2.5-flash")
- **key** (optional): specific API key to validate. If omitted, uses configured key from config.toml.
- **prompt** (optional): custom test prompt. Default: "Say hello in one word"

**API call construction per provider type:**

| Provider Type | Endpoint | Body Format |
|--------------|----------|-------------|
| OpenAI-compatible (openai, deepseek, moonshot, minimax, groq, together, mistral, fireworks, openrouter, perplexity) | `{base_url}/v1/chat/completions` | `{"model": M, "messages": [{"role":"user","content":P}], "max_tokens": 50}` |
| Google/Gemini | `generativelanguage.googleapis.com/v1beta/models/{model}:generateContent?key=K` | `{"contents":[{"parts":[{"text":P}]}]}` |
| Anthropic | `api.anthropic.com/v1/messages` | `{"model": M, "max_tokens": 50, "messages": [{"role":"user","content":P}]}` |

New dict `CHAT_ENDPOINTS` added to `providers.py` with `{base_url, auth_header, body_template}` per provider — similar to existing `quota_*` fields but for chat completion.

**Timeout:** 20 seconds (longer than quota check's 15s, since chat completion is slower).

Returns JSON:
```json
{
  "ok": true,
  "action": "provider_test",
  "data": {
    "provider": "deepseek",
    "model": "deepseek-chat",
    "key_masked": "sk-***abc",
    "response": "Hello!",
    "latency_ms": 342,
    "valid": true
  }
}
```

On failure (timeout, auth error, model not found):
```json
{
  "ok": false,
  "error": "401 Unauthorized — key is invalid or expired",
  "data": { "provider": "deepseek", "model": "deepseek-chat", "valid": false, "latency_ms": 150 }
}
```

On timeout:
```json
{
  "ok": false,
  "error": "Request timed out after 20s",
  "data": { "provider": "deepseek", "model": "deepseek-chat", "valid": false, "latency_ms": 20000 }
}
```

**No key format validation** — the script sends the key as-is and reports the API response. Format validation would reject keys from providers with non-standard patterns.

**Implementation:** New script `scripts/provider_test.py`.

#### `provider_models(provider, key?)`

Lists available models for a provider.

- **provider** (required): provider name
- **key** (optional): specific key to use for the API call. If omitted, uses configured key from config.toml.

**Model discovery strategy:**

| Source | Providers | Method |
|--------|-----------|--------|
| Live API | openai, groq, deepseek, moonshot, minimax, together, mistral, fireworks, openrouter | GET `{base_url}/v1/models` (already used in `quota_url`) |
| Live API (custom) | google/gemini | GET `generativelanguage.googleapis.com/v1beta/models?key=K` |
| Static list | anthropic, perplexity | Return curated list from new `KNOWN_MODELS` dict in `providers.py` |

For live API responses: filter to chat/completion models only (exclude embedding, whisper, tts models). Cap output at 50 models to stay within `max_result_chars`.

For providers not in `providers.py` (zhipu, alibaba, sambanova, cohere): return `{"ok": false, "error": "Provider not supported yet"}`. These will NOT be added to `providers.py` in this PR — they need full quota/chat endpoint configs first. TOOLS.md provider table will note them as "planned".

Returns JSON:
```json
{
  "ok": true,
  "action": "provider_models",
  "data": {
    "provider": "openai",
    "models": ["gpt-4o", "gpt-4o-mini", "gpt-5.1", "o4-mini"],
    "count": 4,
    "source": "api"
  }
}
```

**Implementation:** New script `scripts/provider_models.py`.

**SKILL.toml settings for both new tools:**
- `max_result_chars = 4000` (same as existing tools)
- `max_calls_per_turn = 2` for provider_test, `1` for provider_models

### New Action: `remove_fallback`

Added to existing `provider_apply` tool.

```
provider_apply(action="remove_fallback", profile="groq:groq-1")
```

Removes the specified profile from `fallback_providers` list in config.toml and pushes via gateway API.

**Implementation changes in `scripts/provider_apply.py`:**
1. Add `"remove_fallback"` to argparse `choices` list (line 118)
2. Add `remove_fallback` handler function
3. Handler reads config, filters out matching profile(s) from `fallback_providers`, writes config, pushes via gateway

**SKILL.toml `provider_apply` updates:**
1. Tool description: add `remove_fallback` to actions list and examples
2. `action` arg description: change from `'replace_keys', 'set_default', or 'add_fallback'` to `'replace_keys', 'set_default', 'add_fallback', or 'remove_fallback'`

### providers.py Updates

Add two new dicts to `providers.py`:

1. **`CHAT_ENDPOINTS`** — per-provider chat completion config (base_url, auth style, body template). Used by `provider_test.py`.
2. **`KNOWN_MODELS`** — static model lists for providers without a model-list API (anthropic, perplexity). Used by `provider_models.py` as fallback.

### TOOLS.md Updates

Full rewrite of TOOLS.md with:

1. **Complete tool reference** — all 6 tools with full parameter signatures, types, and descriptions
2. **Provider ↔ Model reference table** — exact model IDs, case-sensitivity notes, test prompt hints per provider (e.g. "deepseek-reasoner is slow, use short prompt", "MiniMax IDs are case-sensitive")
3. **Per-provider testing guide** — which model to use for quick checks, which to avoid, key format notes
4. **All workflows** with concrete parameter examples:
   - "добавь провайдера X" (key_store → provider_apply → provider_test)
   - "переключи на модель Y" (provider_apply set_default)
   - "удали X из фоллбэков" (provider_apply remove_fallback)
   - "протестируй модель X" (provider_test)
   - "валидируй ключ" (provider_test with key param)
   - "какие модели у X?" (provider_models)
   - "проверь/почини провайдеры" (provider_health)
   - "статус провайдеров" (provider_status)
5. **Multi-turn examples** showing contextual chains with actual tool calls
6. **Critical rules** — string types, case-sensitivity, key sourcing, cheapest model preference

### Multi-turn Context

ZeroClaw already maintains conversation history per channel. The bot sees prior messages and tool results. No runtime changes needed — the LLM resolves anaphora ("there", "that one", "first key") from conversation context.

TOOLS.md multi-turn examples teach the model the expected tool-call chains.

## E2E Test Plan

All tests use WebSocket connection to ZeroClaw gateway, sending natural language messages and asserting on tool calls and response content.

### Test Infrastructure

- Connect via WebSocket to `ws://localhost:{gateway_port}/ws`
- Send message, wait for response (with tool call metadata)
- Assert: correct tool was called, parameters match, response contains expected content
- Multi-turn: send sequence of messages on same WS connection (same connection = same conversation context), assert each step
- Gateway WS protocol uses the same connection for session continuity — no explicit session ID needed

### Scenarios

| # | Type | Input (ru/en) | Expected Tool Call | Assert |
|---|------|---------------|-------------------|--------|
| 1 | single | "покажи статус провайдеров" | `provider_status()` | response contains "default" and provider names |
| 2 | single | "сколько у нас ключей deepseek?" | `key_store(action="list", provider="deepseek")` | response contains a number |
| 3 | single | "добавь moonshot в фоллбэк" | `key_store(...)` → `provider_apply(action="add_fallback", ...)` | step 1: key_store returns ≥1 key; step 2: provider_apply confirms addition; response says "добавлен/added" |
| 4 | single | "удали groq из фоллбэков" | `provider_apply(action="remove_fallback", profile="groq:...")` | response confirms removal |
| 5 | single | "проверь модель gemini-2.5-flash" | `provider_test(provider="gemini", model="gemini-2.5-flash")` | response contains latency_ms or error description |
| 6 | single | "какие модели есть у openai?" | `provider_models(provider="openai")` | response lists model names |
| 7 | single | "переключи на deepseek-chat" | `provider_apply(action="set_default", provider="deepseek", model="deepseek-chat")` | response confirms switch |
| 8 | single | "проверь здоровье провайдеров" | `provider_health()` | response contains checked/replaced counts |
| 9 | single | "валидируй ключ deepseek sk-a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4" | `provider_test(provider="deepseek", model="deepseek-chat", key="sk-...")` | response says valid or invalid with reason |
| 10 | multi | "сколько ключей deepseek?" → "какие модели там?" → "установи deepseek-reasoner основной" | `key_store(provider="deepseek")` → `provider_models(provider="deepseek")` → `provider_apply(set_default, deepseek, deepseek-reasoner)` | each step resolves "там" to deepseek from context |
| 11 | multi | "добавь minimax" → "протестируй MiniMax-M1" → "сделай основным" | `provider_apply(add_fallback)` → `provider_test(minimax, MiniMax-M1)` → `provider_apply(set_default)` | chain completes with correct provider/model |
| 12 | multi+val | "найди ключи groq" → "проверь первый" → "добавь его" | `provider_find(groq)` → `provider_test(groq, ..., key=K)` → `provider_apply(add_fallback, key=K)` | **Precondition:** ≥1 groq key exists in key store. Key from step 1 reused in steps 2-3. |
| 13 | single | "find me some groq keys" | `provider_find(provider="groq")` | response lists keys with masked format (English input works) |
| 14 | single | "что сейчас основной провайдер?" | `provider_status()` | response highlights default provider and model |

### Test Preconditions

- **All tests:** ZeroClaw daemon running with gateway, Telegram channel active
- **Test #3:** moonshot key must exist in key store (or provider_find must succeed)
- **Test #4:** groq must be in current fallback chain
- **Test #9:** uses realistic deepseek key format (`sk-[a-f0-9]{32}`)
- **Test #12:** ≥1 groq key in key store — if zero keys, test is skipped (not failed)

### Test File Location

`tests/provider_management_e2e.rs` — Rust integration tests using `tokio-tungstenite` for WebSocket.

### Test Setup/Teardown

- Tests require running ZeroClaw daemon with gateway enabled
- Before each config-mutating test: snapshot `config.toml` to temp file
- After each config-mutating test: restore from snapshot + push via gateway
- Tests marked `#[ignore]` — run explicitly with `--ignored`
- Sequential execution: `--test-threads=1` (provider state is shared)
- 60s pause between tests that make real provider API calls (rate limit protection)

## File Changes Summary

| File | Change |
|------|--------|
| `~/.zeroclaw/workspace/skills/provider-manager/SKILL.toml` | Add `provider_test` and `provider_models` tool definitions; update `provider_apply` description and `action` arg to include `remove_fallback` |
| `~/.zeroclaw/workspace/skills/provider-manager/scripts/provider_test.py` | New script — chat completion per provider type, 20s timeout |
| `~/.zeroclaw/workspace/skills/provider-manager/scripts/provider_models.py` | New script — live API or static list |
| `~/.zeroclaw/workspace/skills/provider-manager/scripts/provider_apply.py` | Add `remove_fallback` to argparse choices (line 118) and handler function |
| `~/.zeroclaw/workspace/skills/provider-manager/scripts/providers.py` | Add `CHAT_ENDPOINTS` dict and `KNOWN_MODELS` dict |
| `~/.zeroclaw/workspace/TOOLS.md` | Expand with new workflows, multi-turn examples, updated model table (only supported providers) |
| `tests/provider_management_e2e.rs` | New E2E test file with 14 scenarios |

## Risks & Mitigations

- **Rate limits during E2E tests**: Use Gemini free tier or Groq (generous limits). 60s pauses between real API calls.
- **Config mutation in tests**: Snapshot/restore per test prevents pollution.
- **Key exposure in test logs**: All keys masked in provider_test output. Tests use provider_find keys (from public repos).
- **Chat completion vs quota check confusion**: `provider_test` uses chat completions (new `CHAT_ENDPOINTS`), `quota.check_one` uses health endpoints (existing `quota_*` fields). Both coexist, different purposes.
- **Empty key store for test #12**: Skip test gracefully if no groq keys found, rather than fail nondeterministically.
