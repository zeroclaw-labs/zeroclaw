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

OpenAI Codex subscription auth lives on the `openai` slot. Set `wire_api = "responses"` to route through `POST /v1/responses` and `requires_openai_auth = true` to use the Codex subscription login (from the Codex CLI's own `~/.codex/auth.json`) instead of an `api_key` field on the entry. The subscription path does not read `OPENAI_API_KEY`; that variable applies only to the metered `openai` API-key mode. See [Provider Configuration → OAuth and subscription auth](./configuration.md#oauth-and-subscription-auth) for the credential model.

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

Every canonical slot, its default endpoint, whether it runs locally, and its
full config field set, generated from the provider registry and the config
schema. Click a slot to expand its fields; click a field to see how to set it.
Slots with no fixed default need `uri` set on the alias entry (Azure, `custom`,
multi-region families, CLI shims).

{{#model-provider-fields}}

For a worked example per family, see [Configuration](./configuration.md). If your vendor isn't listed, use the `custom` slot ([Custom providers](./custom.md)).

### Worked examples: Morph, GitHub Models, Upstage, Featherless, Arcee, Lambda AI, Inception

Each of these is a standard OpenAI-compatible slot: set `model` and `api_key`, leave
`uri` off (the typed endpoint supplies it). None of them ship a public model index,
so the model picker stays empty until you paste a credential. Once a key is set,
ZeroClaw lists models from the provider's live `/models` endpoint. The model IDs
below are illustrative; confirm the current catalog in the vendor dashboard.

**Morph**: slot `morph`. Fast apply-edits models (`morph-v3-large`, `morph-v3-fast`, or
`auto`). Key from the [Morph dashboard](https://morphllm.com).

**GitHub Models**: slot `github_models` (alias `github-models`). OpenAI / Meta /
Microsoft models behind a single GitHub Personal Access Token. Create a PAT with the
**`models`** permission (fine-grained); a Copilot token is *not* the same credential.
Model IDs are publisher-prefixed (e.g. `openai/gpt-4o`).

**Upstage**: slot `upstage`. Solar Pro / Solar Mini (e.g. `solar-pro2`). Key from the
[Upstage console](https://console.upstage.ai/api-keys).

**Featherless**: slot `featherless`. Serverless open-weight models, addressed by their
Hugging Face repo IDs (e.g. `meta-llama/Meta-Llama-3.1-8B-Instruct`). Key from
[featherless.ai](https://featherless.ai).

**Arcee**: slot `arcee`. Native models include `conductor`, `maestro`,
`virtuoso-large`, `coder-large`, and `blitz`. Key from the
[Arcee platform](https://www.arcee.ai). Arcee's Platform API uses the non-standard
`/api/v1` base path; the typed endpoint already accounts for this, so still leave
`uri` off.

**Lambda AI**: slot `lambda_ai` (alias `lambda-ai`). Lambda's hosted inference (e.g.
`hermes3-405b`). Key from the [Lambda Cloud](https://cloud.lambda.ai) API-keys page.

**Inception**: slot `inception`. The Mercury diffusion-LLM family (`mercury-coder` and
the newer `mercury-2`). Key from the
[Inception platform](https://platform.inceptionlabs.ai).

> Credentials come only from config (`api_key`) or the `--credential` override at run
> time, these slots do **not** read a per-provider `*_API_KEY` environment variable.

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
