# Provider Catalog

Every model-provider family ZeroClaw ships with. For each: config shape, notes on auth and endpoint behavior, and the slot key to use under `[providers.models.<type>.<alias>]`.

See [Configuration](./configuration.md) for universal fields (`api_key`, `uri`, `model`, ...) and resolution order.

> Examples below use `home` as the alias to underline that the alias half is operator-chosen — pick whatever name fits (`work`, `personal`, `cn`, `prod`, ...). Reference it from an agent via `model_provider = "<type>.<alias>"`.

---

## Native

### Anthropic — slot `anthropic`

```toml
[providers.models.anthropic.home]
model   = "claude-haiku-4-5-20251001"        # or claude-sonnet-4-6, claude-opus-4-7
api_key = "sk-ant-..."                       # or "sk-ant-oat-..." for OAuth
```

Supports OAuth tokens (`sk-ant-oat*`) from Claude Pro/Team subscriptions — no separate API billing. Streaming, tool calls, vision, and reasoning all supported. Custom endpoints (Anthropic-compatible proxies, e.g. Z.AI's Anthropic API) go on this slot too — set `uri` to override.

### OpenAI — slot `openai`

```toml
[providers.models.openai.home]
model   = "gpt-4o-mini"
api_key = "sk-..."
```

GPT-4o, GPT-5, o-series reasoning models. Reasoning tokens surfaced as `ReasoningDelta` events; see [Streaming](./streaming.md).

### OpenAI Codex — `openai` slot with `requires_openai_auth = true`

```toml
[providers.models.openai.coding]
model                  = "gpt-5-codex"
wire_api               = "responses"
requires_openai_auth   = true
```

OpenAI Codex subscription auth lives on the `openai` slot. Set `wire_api = "responses"` to route through `POST /v1/responses` and `requires_openai_auth = true` to pull credentials from the OAuth profile imported from `~/.codex/auth.json` instead of an `api_key` field on the entry. The subscription path does not read `OPENAI_API_KEY` — that variable applies only to the metered `openai` API-key mode.

### Ollama — slot `ollama`

```toml
[providers.models.ollama.local]
uri              = "http://localhost:11434"
model            = "qwen3.6:35b-a3b"
think            = false                     # disable chain-of-thought on reasoning models
reasoning_effort = "none"
```

Local inference via Ollama's native `/api/chat`. Schema-based structured output via `format`. No API key.

### Bedrock — slot `bedrock`

```toml
[providers.models.bedrock.home]
region = "us-east-1"                         # AWS region template variable
model  = "anthropic.claude-3-5-sonnet-20241022-v2:0"
# Auth via the standard AWS credentials chain (env, IAM role, ~/.aws/credentials).
```

### Gemini — slot `gemini`

```toml
[providers.models.gemini.home]
model   = "gemini-2.5-pro"
api_key = "..."
```

Google's Gemini API. Supports vision and pre-executed grounded search (see [Streaming](./streaming.md) for `PreExecutedToolCall` events).

### Gemini CLI — slot `gemini_cli`

```toml
[providers.models.gemini_cli.home]
model = "gemini-2.5-pro"
```

Shells out to the `gemini` CLI; uses the CLI's existing auth.

### Azure OpenAI — slot `azure`

```toml
[providers.models.azure.home]
resource    = "my-resource"                  # https://{resource}.openai.azure.com/...
deployment  = "gpt-4o"
api_version = "2024-10-01-preview"
api_key     = "..."
```

`resource`, `deployment`, and `api_version` live in this typed config — they are not read from environment variables.

### Copilot — slot `copilot`

```toml
[providers.models.copilot.home]
model = "gpt-4o"
```

Uses a GitHub Copilot subscription for agent inference. Authentication uses a Copilot OAuth token obtained from GitHub.

### Telnyx — slot `telnyx`

```toml
[providers.models.telnyx.home]
model   = "..."
api_key = "..."
```

Voice-oriented AI endpoint. Pair with the `clawdtalk` channel for real-time SIP calls.

### KiloCLI — slot `kilocli`

```toml
[providers.models.kilocli.local]
model = "..."
```

Local inference via KiloCLI.

---

## OpenAI-compatible families

Every OpenAI-compatible vendor has its own canonical slot. There is no generic `kind = "openai-compatible"` selector — pick the slot that matches your provider, or use `custom` for endpoints not listed here.

| Slot | Default endpoint | Notes |
|---|---|---|
| `groq` | `https://api.groq.com/openai` | Native tool streaming hints supported |
| `mistral` | `https://api.mistral.ai` | |
| `xai` | `https://api.x.ai` | |
| `deepseek` | `https://api.deepseek.com` | DeepSeek V3 / R1 |
| `cohere`, `perplexity`, `cerebras`, `sambanova`, `hyperbolic` | per vendor | Standard OpenAI shape |
| `deepinfra`, `huggingface`, `together`, `fireworks` | per vendor | |
| `ai21`, `reka`, `baseten`, `nscale`, `anyscale`, `nebius` | per vendor | |
| `friendli`, `stepfun`, `aihubmix`, `siliconflow` | per vendor | |
| `astrai`, `avian`, `deepmyst`, `venice`, `novita`, `nvidia` | per vendor | |
| `vercel`, `cloudflare`, `ovh` | per vendor gateway | |
| `lepton`, `synthetic`, `opencode` | per vendor | |
| `morph` | `https://api.morphllm.com/v1` | Fast apply-edits models |
| `github_models` | `https://models.github.ai/inference` | OpenAI/Meta/Microsoft models via a GitHub PAT (alias `github-models`) |
| `upstage` | `https://api.upstage.ai/v1` | Solar Pro / Solar Mini |
| `featherless` | `https://api.featherless.ai/v1` | Serverless open-weight models |
| `arcee` | `https://api.arcee.ai/api/v1` | Trinity / Conductor / Maestro — note the non-standard `/api/v1` path |
| `lambda_ai` | `https://api.lambda.ai/v1` | Lambda hosted inference (alias `lambda-ai`) |
| `inception` | `https://api.inceptionlabs.ai/v1` | Mercury diffusion LLM |
| `lmstudio`, `llamacpp`, `sglang`, `vllm`, `osaurus`, `litellm` | `http://localhost:<port>/v1` | Local-server slots with sensible defaults |

Worked example (Groq):

```toml
[providers.models.groq.fast]
model   = "llama-3.3-70b-versatile"
api_key = "gsk_..."
# `uri` is omitted — the family's typed endpoint enum supplies the URL.
```

If your vendor isn't listed, use `custom`:

```toml
[providers.models.custom.gateway]
uri     = "https://my-gateway.example.com/v1"
model   = "my-model-id"
api_key = "..."
```

### Worked examples: Morph, GitHub Models, Upstage, Featherless, Arcee, Lambda AI, Inception

Each of these is a standard OpenAI-compatible slot: set `model` and `api_key`, leave
`uri` off (the typed endpoint supplies it). None of them ship a public model index,
so the model picker stays empty until you paste a credential — once a key is set,
ZeroClaw lists models from the provider's live `/models` endpoint. The model IDs
below are illustrative; confirm the current catalog in the vendor dashboard.

**Morph** — slot `morph`. Fast apply-edits models (`morph-v3-large`, `morph-v3-fast`, or
`auto`). Key from the [Morph dashboard](https://morphllm.com).

```toml
[providers.models.morph.apply]
model   = "morph-v3-large"
api_key = "..."
```

**GitHub Models** — slot `github_models` (alias `github-models`). OpenAI / Meta /
Microsoft models behind a single GitHub Personal Access Token. Create a PAT with the
**`models`** permission (fine-grained) — a Copilot token is *not* the same credential.
Model IDs are publisher-prefixed.

```toml
[providers.models.github_models.home]
model   = "openai/gpt-4o"
api_key = "github_pat_..."
```

**Upstage** — slot `upstage`. Solar Pro / Solar Mini. Key from the
[Upstage console](https://console.upstage.ai/api-keys).

```toml
[providers.models.upstage.home]
model   = "solar-pro2"
api_key = "..."
```

**Featherless** — slot `featherless`. Serverless open-weight models, addressed by their
Hugging Face repo IDs. Key from [featherless.ai](https://featherless.ai).

```toml
[providers.models.featherless.home]
model   = "meta-llama/Meta-Llama-3.1-8B-Instruct"
api_key = "..."
```

**Arcee** — slot `arcee`. Native models include `conductor`, `maestro`,
`virtuoso-large`, `coder-large`, and `blitz`. Key from the
[Arcee platform](https://www.arcee.ai). Arcee's Platform API uses the non-standard
`/api/v1` base path; the typed endpoint already accounts for this, so still leave
`uri` off.

```toml
[providers.models.arcee.home]
model   = "conductor"
api_key = "..."
```

**Lambda AI** — slot `lambda_ai` (alias `lambda-ai`). Lambda's hosted inference. Key
from the [Lambda Cloud](https://cloud.lambda.ai) API-keys page.

```toml
[providers.models.lambda_ai.home]
model   = "hermes3-405b"
api_key = "..."
```

**Inception** — slot `inception`. The Mercury diffusion-LLM family (`mercury-coder` and
the newer `mercury-2`). Key from the
[Inception platform](https://platform.inceptionlabs.ai).

```toml
[providers.models.inception.home]
model   = "mercury-coder"
api_key = "..."
```

> Credentials come only from config (`api_key`) or the `--credential` override at run
> time — these slots do **not** read a per-provider `*_API_KEY` environment variable.

---

## Multi-region families

Several Chinese vendors expose distinct regional endpoints with different default models. Use one canonical slot and pick the region with the typed `endpoint` field on the alias entry.

### Moonshot — slot `moonshot`

```toml
[providers.models.moonshot.cn]
api_key  = "..."
endpoint = "cn"                              # https://api.moonshot.cn/v1

[providers.models.moonshot.intl]
api_key  = "..."
endpoint = "intl"                            # https://api.moonshot.ai/v1
```

Variants: `cn`, `intl`, `code`.

### Qwen / DashScope — slot `qwen`

```toml
[providers.models.qwen.intl]
api_key   = "..."
endpoint  = "intl"                           # variants: cn, intl
auth_mode = "oauth"                          # optional; for OAuth-backed Qwen accounts
```

OAuth-backed Qwen accounts use the same slot with `auth_mode = "oauth"`.

### GLM — slot `glm`

```toml
[providers.models.glm.home]
api_key  = "..."
endpoint = "default"
```

### MiniMax — slot `minimax`

```toml
[providers.models.minimax.intl]
api_key  = "..."
endpoint = "intl"                            # variants: cn, intl
```

### Z.AI — slot `zai`

```toml
[providers.models.zai.home]
api_key  = "..."
endpoint = "global"
```

For Z.AI's Anthropic-compatible API, use `[providers.models.anthropic.zai]` with `uri = "https://api.z.ai/api/anthropic"` instead.

### Doubao / Volcengine — slot `doubao`

```toml
[providers.models.doubao.home]
api_key  = "..."
endpoint = "default"
```

### Other Chinese-region slots

- `yi`
- `hunyuan`
- `qianfan`
- `baichuan`

---

## Routing layers

OpenRouter is treated as a single first-class provider, not a meta-router. The runtime sees one endpoint; OpenRouter handles vendor fan-out behind that endpoint.

```toml
[providers.models.openrouter.home]
model   = "anthropic/claude-sonnet-4-20250514"
api_key = "sk-or-..."
```

For per-task routing, run multiple agents and let channels pick which agent handles which traffic — see [Routing](./routing.md). For a narrower in-config hint mechanism, use `[[model_routes]]`.

---

## Something missing?

- If the endpoint is OpenAI-compatible, use the `custom` slot with `uri` set.
- If it has its own canonical slot above, use that — even if you only see one of its regions, the slot's `endpoint` enum covers the rest.
- If it speaks a non-OpenAI wire format and needs its own implementation, see [Custom providers](./custom.md).
