# Provider Catalog

Every model-provider family ZeroClaw ships with. For each: config shape, notes on auth and endpoint behavior, and the slot key to use under `[providers.models.<type>.<alias>]`.

See [Configuration](./configuration.md) for universal fields (`api_key`, `uri`, `model`, ...) and resolution order.

> Examples below use `home` as the alias to underline that the alias half is operator-chosen, pick whatever name fits (`work`, `personal`, `cn`, `prod`, ...). Reference it from an agent via `model_provider = "<type>.<alias>"`.

---

## Native

### Anthropic: slot `anthropic`

Supports OAuth tokens (`sk-ant-oat*`) from Claude Pro/Team subscriptions, no separate API billing. Streaming, tool calls, vision, and reasoning all supported. Custom endpoints (Anthropic-compatible proxies, e.g. Z.AI's Anthropic API) go on this slot too: set `uri` to override.

### OpenAI: slot `openai`

GPT-4o, GPT-5, o-series reasoning models. Reasoning tokens surfaced as `ReasoningDelta` events; see [Streaming](./streaming.md).

### OpenAI Codex: `openai` slot with `requires_openai_auth = true`

OpenAI Codex subscription auth lives on the `openai` slot. Set `wire_api = "responses"` to route through `POST /v1/responses` and `requires_openai_auth = true` to pull credentials from `OPENAI_API_KEY` / `~/.codex/auth.json` instead of an `api_key` field on the entry.

### Ollama: slot `ollama`

Local inference via Ollama's native `/api/chat`. Schema-based structured output via `format`. No API key.

### Bedrock: slot `bedrock`

### Gemini: slot `gemini`

Google's Gemini API. Supports vision and pre-executed grounded search (see [Streaming](./streaming.md) for `PreExecutedToolCall` events).

### Gemini CLI: slot `gemini_cli`

Shells out to the `gemini` CLI; uses the CLI's existing auth.

### Azure OpenAI: slot `azure`

`resource`, `deployment`, and `api_version` live in this typed config, they are not read from environment variables.

### Copilot: slot `copilot`

Uses a GitHub Copilot subscription for agent inference. Authentication uses a Copilot OAuth token obtained from GitHub.

### Telnyx: slot `telnyx`

Voice-oriented AI endpoint. Pair with the `clawdtalk` channel for real-time SIP calls.

### KiloCLI: slot `kilocli`

Local inference via KiloCLI.

---

## All slots

Every canonical slot, its default endpoint, and whether it runs locally, generated from the provider registry. Slots with no fixed default (`—`) need `uri` set on the alias entry (Azure, `custom`, multi-region families, CLI shims).

{{#model-provider-catalog-table}}

For a worked example per family, see [Configuration](./configuration.md). If your vendor isn't listed, use the `custom` slot ([Custom providers](./custom.md)).

---

## Multi-region families

Several Chinese vendors expose distinct regional endpoints with different default models. Use one canonical slot and pick the region with the typed `endpoint` field on the alias entry.

### Moonshot: slot `moonshot`

Variants: `cn`, `intl`, `code`.

### Qwen / DashScope: slot `qwen`

OAuth-backed Qwen accounts use the same slot with `auth_mode = "oauth"`.

### GLM: slot `glm`

### MiniMax: slot `minimax`

### Z.AI: slot `zai`

For Z.AI's Anthropic-compatible API, use `[providers.models.anthropic.zai]` with `uri = "https://api.z.ai/api/anthropic"` instead.

### Doubao / Volcengine: slot `doubao`

The remaining Chinese-region slots (`yi`, `hunyuan`, `qianfan`, `baichuan`) appear in the all-slots table above; select the region with the typed `endpoint` field on the alias entry.

---

## Routing layers

OpenRouter is treated as a single first-class provider, not a meta-router. The runtime sees one endpoint; OpenRouter handles vendor fan-out behind that endpoint.

For per-task routing, run multiple agents and let channels pick which agent handles which traffic, see [Routing](./routing.md). For a narrower in-config hint mechanism, use `[[model_routes]]`.

---

## Something missing?

- If the endpoint is OpenAI-compatible, use the `custom` slot with `uri` set.
- If it has its own canonical slot above, use that, even if you only see one of its regions, the slot's `endpoint` enum covers the rest.
- If it speaks a non-OpenAI wire format and needs its own implementation, see [Custom providers](./custom.md).
