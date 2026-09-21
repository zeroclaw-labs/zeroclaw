# Provider Configuration

Every model provider lives at `[providers.models.<type>.<alias>]`. `<type>` is a canonical family slot (see the [Catalog](./catalog.md#all-slots) for every slot with its endpoint). `<alias>` is your operator-assigned instance name, pick any descriptive name (`home`, `work`, `cn`, `gpt5`, ...).

## Minimal working example

The smallest config that loads clean has four section headers: a provider entry, an agent that references it, and a risk profile the agent gates against. Configure them through the gateway, zerocode, or `zeroclaw config set`; the [config reference](../reference/config.md#providers) has the full field index.

## Field reference: provider entry

Almost every family also takes the shared fields from `ModelProviderConfig`:

- `api_key`: credential for providers that use bearer or subscription-style API keys.
- `uri`: full endpoint override. Leave unset to use the family's endpoint resolver.
- `model`: model identifier sent to the provider.
- `temperature`: optional sampling temperature.
- `timeout_secs`: HTTP request timeout in seconds. Setting it above 300 also raises the provider's streaming idle bound (the default 300-second cap on the gap between stream reads) for OpenAI Responses and OpenAI-compatible providers.
- `max_tokens`: optional response length cap.
- `extra_headers`: extra HTTP headers for custom gateways or auth bridges.
- `fallback_models`: alternate model IDs on the same provider alias.
- `fallback`: ordered list of other dotted provider aliases to try after this alias fails.
- `wire_api`, `native_tools`, `provider_extra`, `think`, and `chat_template_kwargs`: advanced protocol and request-body overrides.
- `vision`: override the provider's image-input (vision) capability. Leave unset to use the family's built-in default. Set `false` for a text-only model served by a vision-capable family (for example, a text model behind llama.cpp) so image messages route to a configured `[multimodal] vision_model_provider` instead of erroring; set `true` to force it on.
- `tool_result_image_policy`: handling for image markers in native `role = "tool"` results sent to compatible chat-completions providers. Defaults to `"image_url"`; set to `"omit"` to remove image URI/base64 payloads and append a fixed notice. This does not change direct user images or OpenAI Responses providers.
- `cache_passthrough`: opt into Anthropic prompt caching on chat-completions gateways that translate to the Anthropic Messages API. Adds at most two `cache_control` breakpoints per request and surfaces gateway-reported cache reads in token usage. Default `false`, requests unchanged. Requires route qualification before production use; see [Prompt cache passthrough](#prompt-cache-passthrough-chat-completions-gateways).
- `tls_ca_cert_path`: absolute path to a PEM-encoded CA certificate for TLS connections to this provider (a per-provider trust override, distinct from the gateway TLS `ca_cert_path`). Shell expansion such as `~` is not performed; leave unset to use the system trust store.

Family-specific entries add their own typed fields on top of these shared fields.

## Field resolution order

For most families, the URL is resolved in this order:

1. **Operator override**: `uri` field on the alias entry, if set.
2. **Family endpoint**: the family's `*Endpoint` enum supplies the URL (e.g. `OpenAIEndpoint::Default` -> `https://api.openai.com/v1`). Multi-region families have an `endpoint` field on the alias entry that picks the variant (e.g. `endpoint = "cn"` for Moonshot).
3. **Templated families**: Azure takes typed inputs (`resource`, `deployment`, `api_version`) and substitutes them into the family's URI template. Missing fields fail loud at runtime.

Bedrock is an exception: its endpoint hostname is constructed at request time from the signing region resolved through the AWS credential chain (`AWS_REGION`, `AWS_DEFAULT_REGION`, or the `region` from the active `credential_process` or IMDS profile). The `uri` alias field and the schema-level `providers.models.bedrock.<alias>.region` field have no effect in the current implementation.

## Family slots

Every slot, its default endpoint, and whether it runs locally is in the [Catalog](./catalog.md#all-slots). There is one canonical key per vendor: no synonyms.

## Credentials

Supported credential input and storage forms:

1. **Inline `api_key = "..."`** in the alias entry (fine for dev, risky for checked-in configs).
2. **1Password references**: set a secret field to `op://vault/item/field`. ZeroClaw keeps the reference in config and resolves it at runtime with `op read`, so the 1Password CLI must be installed and signed in.
3. **Config-level secrets store**: encrypted at `~/.zeroclaw/secrets` via a local key file.
4. **Generic env override**: `ZEROCLAW_providers__models__<type>__<alias>__api_key=...` sets `providers.models.<type>.<alias>.api_key` at startup. See [Environment variables](../reference/env-vars.md) for the full grammar.

Schema-mirror env overrides win at startup. They replace the in-memory credential for that process without rewriting the stored inline, encrypted, or `op://` value on disk.

`zeroclaw quickstart` writes credentials to the secrets store by default. Configs you commit should not contain inline keys. For ecosystem-default names you already export in your shell (`$ANTHROPIC_API_KEY`, `$OPENROUTER_API_KEY`, …), the [env-vars reference](../reference/env-vars.md#bridging-ecosystem-default-env-vars) shows the one-line bash expansions that point a schema-mirror name at the existing value.

## OAuth and subscription auth

Several providers accept OAuth or subscription-style tokens instead of raw API keys. Get the token from the vendor's own dashboard or CLI flow, then drop it into the alias entry the same way you would an API key:

- **Anthropic / Claude**: Console API keys and tokens generated by `claude setup-token` for Claude Max go in `api_key` on `[providers.models.anthropic.<alias>]`. In Quickstart, pick `api_key` or `setup_token`; the saved provider entry is still the canonical `anthropic` slot.
- **OpenAI Codex subscription**: run `zeroclaw auth login --model-provider openai-codex` (or import an existing Codex CLI login with `--import ~/.codex/auth.json`), then set `requires_openai_auth = true` and leave `api_key` unset on `[providers.models.openai.<alias>]`; the runtime reads ZeroClaw's stored `openai-codex` auth profile.
- **Gemini CLI**: `[providers.models.gemini_cli.<alias>]` shells out to the `gemini` CLI; use the CLI's own auth flow.
- **Grok Build CLI**: `[providers.models.grok_cli.<alias>]` shells out through the documented `grok agent stdio` ACP surface. The assembled prompt is JSON-RPC on stdin, never argv or a prompt file. Auth uses the CLI login cache by default. For API-key auth, export `XAI_API_KEY` into the daemon environment and explicitly add `env_passthrough = ["XAI_API_KEY"]` to the alias; the typed alias `api_key` remains unsupported. An existing absolute `working_directory` is required and defines both the child cwd and ACP session boundary. The child environment is cleared before spawn, and `env_passthrough` defaults to empty. Other provider-owned `XAI_*` names and all `GROK_*` names are rejected. ZeroClaw defaults to `--sandbox strict`, `--permission-mode dontAsk`, an empty built-in tool set, and fail-closed ACP permission responses. `extra_args` is the explicit per-alias opt-in for relaxing those controls. The bypass flags `--always-approve`, `--dangerously-skip-permissions`, `--yolo`, and `--permission-mode=bypassPermissions` make the headless ACP client select `allow_once`; other permission modes continue to select `reject_once`. ACP transport/model/session/cwd flags plus positional and short arguments are reserved; unknown value-taking long options use `--flag=value`. Alias `vision = true` only opts ZeroClaw into sending ACP image blocks; Grok still advertises `promptCapabilities.image = false` through 0.2.118 and does not reliably use the image content - leave unset for production; see [ACP vision / image input](./catalog.md#acp-vision--image-input-current-grok-build-behavior).
- **Qwen / MiniMax**: set `auth_mode = "o_auth"` on the alias entry plus the relevant `oauth_*` fields (see [env-vars → OAuth and CLI-path fields](../reference/env-vars.md#oauth-and-cli-path-fields)).

## OpenAI Astra setup

OpenAI's public API model ID is `gpt-6-astra`. The
[model guide](https://developers.openai.com/api/docs/guides/latest-model#gpt-6-astra)
requires the Responses API for tool calling and lists `temperature` among the
unsupported parameters. Configure the public API route on an `openai` alias
with an API key and an explicit Responses wire:

```toml
[providers.models.openai.astra_api]
api_key        = "op://platform/openai/api-key"
model          = "gpt-6-astra"
wire_api       = "responses"
vision         = true
context_window = 1050000
max_tokens     = 32000

[runtime]
reasoning_effort = "high"

[runtime_profiles.astra]
agentic             = true
max_tool_iterations = 12
max_history_messages = 80
max_context_tokens  = 200000

[agents.astra]
model_provider  = "openai.astra_api"
runtime_profile = "astra"
risk_profile    = "supervised"
```

The numeric values above are an intentional local budget, not a recommendation
to fill the model's advertised window. The provider entry records the endpoint
capability and output cap, while the runtime profile keeps a smaller estimated
input budget and bounds the tool loop. Adjust them for the workload and leave
output headroom. The fields have separate owners:

- `providers.models.openai.astra_api.context_window` records the model input
  window used by provider-aware budgeting.
- `providers.models.openai.astra_api.max_tokens` caps generated output; it is
  not an input-history limit.
- `runtime_profiles.astra.max_context_tokens` is ZeroClaw's estimated local
  trimming threshold. It may be smaller than `context_window`.
- `runtime_profiles.astra.max_tool_iterations` limits the agentic tool loop,
  while `max_history_messages` separately bounds retained message count.
- `runtime.reasoning_effort` is the global provider-facing reasoning level.
  ZeroClaw currently accepts `minimal`, `low`, `medium`, `high`, and `xhigh`.
  Astra's public API accepts `low`, `medium`, `high`, `xhigh`, and `max`, so use
  a shared value until [max reasoning support](https://github.com/zeroclaw-labs/zeroclaw/issues/10705)
  lands.

Do not set provider `temperature` for Astra. The Responses adapter forwards a
configured value and does not repair unsupported sampling parameters. Keep API
validation errors distinct from account, usage-tier, region, or backend access
restrictions.

`vision = true` is the operator's assertion that this exact model and endpoint
accept image input. The public
[Astra model specification](https://developers.openai.com/api/docs/models/gpt-6-astra)
currently lists image input, but verify the configured account and endpoint
before enabling it. Tool-result image serialization is a separate adapter boundary tracked in
[#9599](https://github.com/zeroclaw-labs/zeroclaw/issues/9599).

The Codex subscription route uses `requires_openai_auth = true` instead of an
API key and must use an exact model ID served by that backend. Public API
availability does not prove Codex-backend availability; follow the
[Codex subscription guide](./openai-codex-subscription.md#astra-on-the-codex-subscription-backend)
and test the account before routing production traffic.

The public API may support capabilities that ZeroClaw does not yet expose for
this adapter. Track reasoning-state continuity in
[#10706](https://github.com/zeroclaw-labs/zeroclaw/issues/10706), async function
tools in [#10704](https://github.com/zeroclaw-labs/zeroclaw/issues/10704),
active-response steering in
[#10708](https://github.com/zeroclaw-labs/zeroclaw/issues/10708), and
programmatic tool calling in
[#10707](https://github.com/zeroclaw-labs/zeroclaw/issues/10707). These are
proposals until their ZeroClaw implementations and provider routes are verified.

## Container-friendly overrides

When ZeroClaw runs inside a container and a provider is on the host (e.g. Ollama), set `uri` to a host-reachable address. The generic env-override mechanism (`ZEROCLAW_<dotted_path_with_double_underscores>=<value>`) can set the same field at runtime without editing config:

{{#env-var container}}

The `__` is the path separator; the example above sets `providers.models.ollama.home.uri`. See [Environment variables](../reference/env-vars.md) for the full grammar.

## Per-model vision capability

Use `vision` when a provider family can serve both multimodal and text-only
models. The value belongs to the provider alias, so routing and fallback paths
resolve it together with that alias's endpoint, credentials, and model:

```toml
[providers.models.openai.vision]
model = "gpt-4o"
wire_api = "responses"
vision = true

[providers.models.llamacpp.text]
model = "qwen3-4b"
vision = false
```

Leaving `vision` unset preserves the provider family's built-in default. For
OpenAI Responses aliases, set `vision = true` for models that accept image
input; this opt-in keeps text-only Responses models from receiving image
payloads accidentally.

Setting `vision = true` is an explicit operator assertion that the selected
alias accepts image input. It changes image routing: ZeroClaw keeps image
attachments on that alias instead of treating it as text-only or routing them
to `multimodal.vision_model_provider`. Set it only for a tested provider and
model combination. For `grok_cli`, the same field only controls whether
ZeroClaw **sends** ACP image blocks; it does not rewrite Grok's
`promptCapabilities.image` advertise (still `false` through 0.2.118) and does
not make the CLI reliably describe the image. See
[ACP vision / image input](./catalog.md#acp-vision--image-input-current-grok-build-behavior).

When `[multimodal] vision_model_provider` names a dotted provider alias, its
`model` is used automatically. An explicit `[multimodal] vision_model` takes
precedence over the alias model; if neither is set, the primary turn model is
used for backward compatibility.

## Native thinking display (Anthropic)

`agent.thinking.display` controls how Anthropic extended thinking is
delivered when native thinking is enabled (`agent.thinking.native_thinking
= true`). Accepted values:

- `off` (default): no `display` field is sent; requests are byte-identical
  to earlier ZeroClaw versions and thinking requests use the non-streaming
  fallback.
- `omitted`: Anthropic omits thinking text from the response; blocks arrive
  signature-only (empty `thinking`, required signature), keeping replay
  intact while minimizing visible reasoning.
- `updates`: the request carries the
  `thinking-display-updates-2026-08-18` beta and uses the streaming
  response path. Readable thinking progress is surfaced live while the
  model works; the signed reasoning payload is retained separately for
  history replay and never shown.
- `summarized`: same streaming behavior, requesting summarized thinking.

```toml
[agent.thinking]
native_thinking = true
display = "updates"
```

The setting requires an Anthropic account enrolled in the
`thinking-display-updates` beta; without enrollment the API rejects the
request. Set `display = "off"` (or remove the field) to return to the
previous wire behavior.

## Prompt cache passthrough (chat-completions gateways)

`cache_passthrough = true` on a chat-completions provider alias opts its
requests into Anthropic prompt caching. Use it on gateways that translate
Chat Completions into the Anthropic Messages API (LiteLLM, TrueFoundry,
and similar); the native Anthropic family already caches by default and
ignores this field.

Before reaching for this flag, check whether the gateway also exposes an
Anthropic Messages endpoint. If it does, point a
`[providers.models.anthropic.<alias>]` entry at it with `uri` and skip the
passthrough entirely; the native provider places its own breakpoints.
`cache_passthrough` is for gateways that offer only the Chat Completions
surface.

With the flag on, requests gain at most two `cache_control` breakpoints:
one on the system prompt, and one rolling breakpoint on the last message
once the conversation has more than one non-system message, the same gate
the native Anthropic provider applies. On a message that ends with an
image, the rolling breakpoint sits on the message's last text block; the
image is covered by the following turn. With
`merge_system_into_user` the system role never reaches the wire, so the
merged first user message (or the synthetic user carrying the system text)
carries the system-equivalent breakpoint instead. Only breakpoint-carrying
messages change serialization.

The flag also scopes to the structured request paths: agent turns, tool
calls, and structured streaming. The text-only helpers (`chat_with_system`,
`chat_with_history`, the legacy chunk-stream APIs) deliberately emit no
breakpoints even with the flag on, because their responses drop token usage
entirely; a premium cache write they triggered could never show up in
accounting. On those helpers the flag is inert, which also means fallback
re-entries that route through them send unmarked requests. With the flag
off (the default), request bodies are byte-identical to previous versions.

```toml
[providers.models.custom.claude-via-gateway]
uri = "https://<gateway-host>/v1"
model = "<anthropic-routed model>"
api_key = "op://platform/gateway/api-key"
cache_passthrough = true
```

Requirements and caveats:

- **Gateway support is required.** The breakpoint reaches Anthropic only
  when the gateway forwards block-form content with `cache_control` into
  the native Messages API. Routes that proxy the OpenAI API proper ignore
  the field. A non-Anthropic-routed model behind the same gateway does not
  fail loudly: the field is accepted and dropped, and the gateway may still
  meter cache-write tokens on that route in its own usage accounting. Give
  Anthropic-routed models a dedicated alias instead of enabling the flag on
  a mixed entry.
- **Size and TTL.** Anthropic caches only prefixes of at least 1024 tokens
  (2048 on some smaller models), and entries expire after roughly five
  minutes, refreshed on each read. Short or infrequent conversations see
  no benefit.
- **Writes bill at a premium.** Tokens written to the cache are billed at
  a premium (1.25x on the observed route) and reads come back at a large
  discount. A route that writes the cache on every request without ever
  reading it costs more than no caching at all.
- **Qualify the exact route first.** A gateway exposes many model aliases
  to the same upstream account, and an alias that accepts and bills cache
  writes can still never serve cache reads. Before relying on the flag in
  production, send one flagged request and check the usage reports
  `cache_creation_input_tokens > 0`; then immediately repeat the
  byte-identical request and check for `cache_read_input_tokens > 0`.
  Cache creation alone is not evidence that caching works. Re-run the pair
  after any model-alias or gateway-route change.
- **Usage reporting.** When the gateway forwards the Anthropic-shaped
  usage counters, `cache_read_input_tokens` fills the cached-input figure
  in token usage and cost reporting, and `cache_creation_input_tokens` is
  written to the debug log with counts only. A response that reports zero
  cache reads keeps the cached figure at zero rather than substituting the
  OpenAI-shaped counter. This accounting covers the structured paths only,
  which is exactly why the helpers without usage capture stay inert above.
- **Tool definitions are not separately marked.** The native Anthropic
  provider additionally marks the last tool definition, which covers
  tool-schema tokens when no system prompt exists. This flag does not mark
  tool definitions; requests with tools but no system prompt cache only the
  rolling message breakpoint. The live gateway qualification showed that
  with a system prompt present, tool-schema tokens sit inside the cached
  prefix anyway.

## Image input limits

`[multimodal]` bounds every image that enters the request pipeline, whatever its
source: channel attachments, tool outputs that surface a local image path, and
the web dashboard upload.

```toml
[multimodal]
max_image_size_mb = 20   # per image, decoded bytes (default: 20, clamped to 1-20)
max_images        = 4    # images kept per request (default: 4, clamped to 1-16)
```

`max_image_size_mb` is measured before base64 encoding, so the encoded payload
reaching the provider is about a third larger. The 20 MiB ceiling is the lowest
common per-image or per-request bound across the supported vision APIs, and
images are buffered whole, so it also bounds gateway memory per upload. Lower
the value to cap per-turn upload cost.

Providers apply their own limits on top of this setting. Anthropic refuses a
single image over 10 MB base64-encoded, about 7.5 MiB decoded, and its client
enforces that regardless of what `max_image_size_mb` allows. Model context cost
does not track bytes: providers rasterize images and bill by pixel dimensions,
so a large file and a small one at the same resolution cost roughly the same.

## Per-family knobs: worked examples

### Ollama

Ollama defaults to the local endpoint, so a local alias only needs the model name:

```toml
[providers.models.ollama.local]
model = "llama3.1"
```

Set `uri` when ZeroClaw is not running on the same host as Ollama:

```toml
[providers.models.ollama.host]
model = "llama3.1"
uri = "http://host.docker.internal:11434"
```

Ollama-specific optional fields are `num_ctx`, `num_predict`, and `temperature_override`.

### Hailo-Ollama

Hailo-Ollama uses a separate `hailo_ollama` slot because its native API needs
stricter request shaping than ordinary Ollama. A local instance defaults to
`http://localhost:8000`; model discovery reads its live `/api/tags` response.

```toml
[providers.models.hailo_ollama.edge]
model = "qwen3:1.7b"
context_window = 2048
max_tokens = 256
timeout_secs = 90
queue_timeout_secs = 30
```

Set `uri` to the Hailo-Ollama base URL when it runs on another host. The URL
must use `http` or `https` and must not contain credentials, a query, or a
fragment. An alias `api_key` is sent as a Bearer `Authorization` header, and
`extra_headers` are forwarded to both native chat and catalog requests. Do not
configure both `api_key` and an explicit `Authorization` extra header: ZeroClaw
rejects that ambiguous credential combination. It serializes generation to one
active request per normalized endpoint, even when multiple aliases target that
endpoint, and rejects queued work after `queue_timeout_secs`.

For a non-loopback endpoint, plain `http` sends prompts and responses without
transport encryption or authentication; use `https` when the network is not a
trusted isolated link. The provider accepts plain HTTP for local and explicitly
trusted LAN deployments because Hailo-Ollama's native endpoint has no
authentication contract.

Hailo-Ollama rejects transport options it cannot honor rather than silently
ignoring them. In particular, `tls_ca_cert_path`, `think=true`, `vision=true`,
call-level native `thinking`, `provider_extra`, `api_path`, `wire_api`, and
`chat_template_kwargs` are not supported. Set `native_tools = false` (or leave
it unset).

If an accepted request reaches its HTTP timeout or ends with another post-connect
transport failure, ZeroClaw quarantines that endpoint for the rest of the process
because Hailo may still be generating after the client disconnects. Confirm the
backend is idle (restart it if necessary), then restart ZeroClaw to clear the
quarantine. A connection-establishment failure does not quarantine the endpoint.

For native-backend compatibility, ZeroClaw omits unsupported `think` and `num_ctx` wire fields rather than claiming to control them. The native service parses each decoded message as structured-prompt JSON a second time, so ZeroClaw escapes both literal backslashes and CR/LF/tab controls in the API value to preserve their original meaning through that parse. `context_window` controls ZeroClaw's best-effort local history budgeting. Omit it to avoid a provider-wide context assumption; configure a positive value to enable local budgeting. Explicit zero is rejected by the shared configuration doctor. It is deliberately not sent to Hailo-Ollama. Call-level native thinking requests are rejected before any backend request. Responses must be a completed non-streaming response (`done=true`), and an empty completed response is treated as an error. A low `context_window` drops complete older user-anchored turns using the configured best-effort aggregate budget. System instructions are folded into the first retained user message before that check. When a nonzero `context_window` is configured, if the newest complete turn alone then exceeds the configured budget, the request fails locally before transport instead of dropping that turn or substituting a synthetic prompt. When `context_window` is omitted, no provider-wide local budget is applied. There is no provider-wide character or message-count cap; the native HEF/runtime remains the authority for the actual supported context capacity.

If the native response has no visible content but does contain non-empty
internal reasoning, the provider uses that field as a last-resort ordinary-text
fallback for compatibility with models that put their only output there. It
does not expose a separate reasoning channel, and callers should not treat this
fallback as evidence that extended thinking is supported.

Native tool calling, streaming, and vision are not advertised. Because the
Hailo-Ollama 0.5.1 chat DTO has no image field, `vision=true` is rejected rather
than overriding that text-only capability; configure a separate vision-capable
provider instead.
Prompt-guided tool calls and results remain available as plain text history when
the complete injected tool protocol fits in the bounded first message; ZeroClaw
rejects an oversized protocol instead of sending truncated tool instructions.

### Azure OpenAI

Azure OpenAI computes its endpoint from the typed Azure fields:

```toml
[providers.models.azure.work]
api_key = "op://platform/azure-openai/api-key"
model = "gpt-4o"
resource = "example-resource"
deployment = "gpt-4o-prod"
api_version = "2024-10-21"
```

The `resource`, `deployment`, and `api_version` values live in this typed config, they are not read from Azure-specific environment variables. Use `uri` only when you need to override the computed endpoint completely.

### Amazon Bedrock

Bedrock needs an alias with a model; endpoint region currently comes from the Bedrock auth environment/profile path:

```toml
[providers.models.bedrock.work]
model = "anthropic.claude-sonnet-4-6"
```

The Bedrock provider uses the credential paths implemented in `crates/zeroclaw-providers/src/bedrock.rs`:

1. `api_key` on the Bedrock alias, or `BEDROCK_API_KEY`, uses Bedrock bearer-token auth and takes precedence over SigV4 credentials.
2. `AWS_ACCESS_KEY_ID` plus `AWS_SECRET_ACCESS_KEY` uses SigV4. `AWS_SESSION_TOKEN` is optional. `AWS_REGION` or `AWS_DEFAULT_REGION` selects the signing region and falls back to `us-east-1`.
3. `credential_process` in the active profile from `~/.aws/config`, or from `AWS_CONFIG_FILE`, uses SigV4. `AWS_PROFILE` selects the profile and defaults to `default`.
4. EC2 IMDSv2 instance credentials are the final SigV4 fallback.

The config schema additionally defines a `providers.models.bedrock.<alias>.region`
field, but the current implementation does not read it. The endpoint region is
always resolved from the AWS credential chain (environment variables,
`credential_process`, or IMDS) as described above.

A normal static profile in `~/.aws/credentials` is not read by the current Bedrock implementation. `~/.zeroclaw/secrets` only stores ZeroClaw config secrets such as an alias `api_key`; it does not export `AWS_*` variables for the provider.

To reuse an AWS CLI profile through the implemented profile path, put a `credential_process` in `~/.aws/config`:

```ini
[profile zeroclaw-bedrock]
credential_process = /usr/bin/aws configure export-credentials --profile my-existing-profile
region = us-east-1
```

`/usr/bin/aws` is the default path on Debian and Ubuntu. On other systems,
use the absolute path from `command -v aws`.

Then run ZeroClaw with `AWS_PROFILE=zeroclaw-bedrock`. For a systemd user service, see [Service management](../setup/service.md#environment-overrides-systemd).

### Multi-region (Moonshot / Qwen / GLM / MiniMax / ...)

One type per family; pick the region via the typed `endpoint` field on the alias entry.

### Custom OpenAI-compatible endpoint

The `custom` slot requires `uri`. See [Custom providers](./custom.md).

## Picking which provider an agent uses

Agents reference a provider by dotted alias. Provider entries on their own do nothing.

`risk_profile` and `runtime_profile` reference independent alias maps, so their names need not match (`runtime_profile` is also optional). `Config::validate()` fails loud at startup if `model_provider` doesn't resolve to a configured `[providers.models.<type>.<alias>]` entry, or if `risk_profile` doesn't resolve to a configured `[risk_profiles.<alias>]` entry.

For multiple agents pointing at different providers, see [Routing](./routing.md).

## Fallback on failure

When a request to a provider fails after exhausting its retries (provider down,
key rate-limited, model unavailable), the alias can fall over to alternatives
you declare on the alias entry. Two independent, ordered axes:

- **`fallback_models`**: alternate model IDs tried on *this* provider, using the
  same endpoint, key, and headers. Only the model identifier changes. Use it when
  a provider serves a backup model (a smaller or older variant) that should be
  tried before leaving the provider entirely.
- **`fallback`**: an ordered list of *other* provider aliases (dotted
  `<type>.<alias>` references into `[providers.models]`). Each fallback alias
  resolves with **its own** credentials, endpoint, and model, a fallback never
  inherits the failing alias's key.

### Order of attempts

The walk is depth-first: an alias's entire model list is exhausted before leaving
it, then each `fallback` alias is descended in turn, applying that alias's own
`fallback_models` and `fallback` recursively. Suppose `anthropic.prod` serves
`claude-sonnet-4-5`, lists `claude-haiku-4-5` in its `fallback_models`, and
names `openai.backup` (serving `gpt-4.1`) in its `fallback`. The attempt order
is then:

```
anthropic.prod/claude-sonnet-4-5
  -> anthropic.prod/claude-haiku-4-5
  -> openai.backup/gpt-4.1
  -> (request fails)
```

Fallback aliases can themselves declare `fallback`, so the chain is as long as
your config makes it, up to a maximum depth of **3 aliases**. A chain that loops
back on itself (`a` -> `b` -> `a`) is detected and the cycle edge is pruned, and
an acyclic chain deeper than the limit has its remaining links pruned; neither
ever loops, hangs, or overflows the stack.

### Misconfiguration

A `fallback` entry that names an alias which is not configured, one that closes a
cycle, or a chain that exceeds the maximum depth is **non-fatal**:
`Config::validate()` still succeeds, the offending edge is skipped at runtime, and
the issue is surfaced as a validation warning (`dangling_fallback_ref` /
`fallback_cycle` / `max_fallback_depth_exceeded`) on the CLI and in the dashboard.
A `fallback_models` entry that is blank or duplicates the alias's primary `model`
is likewise skipped at runtime and surfaced (`empty_fallback_model` /
`fallback_model_duplicates_primary`). A bad fallback link degrades gracefully, it
never prevents the agent from running.

### Anthropic refusals and fallback

Native Anthropic requests can come back as a *refusal*: an HTTP 200 whose
`stop_reason` is `"refusal"`, emitted by Fable's safety classifiers rather than
a normal completion. ZeroClaw treats a refusal as a typed error so fallback can
recover it two ways:

- **Client-side** (`fallback_models`, above): the refusal surfaces as an error,
  and ZeroClaw advances to the next model or alias just as it does for any other
  provider failure, so the reply comes from a different model.
- **Server-side** (`server_fallback_models` on
  `[providers.models.anthropic.<alias>]`): Anthropic retries the refused request
  against a listed model on its side, in a single non-streaming call. Entries
  must name permitted fallback targets, for example mapping `claude-fable-5` to
  `claude-opus-4-8`.

Either way the delivered reply carries a short footer naming the requested and
served models, so the switch is visible to the user. In the web dashboard chat
the same switch surfaces as an inline notice just before the answer, showing
only the requested and served model names. When an ordinary provider failure
already moved the request to a client-side fallback before Anthropic switched
models on its side, the reply keeps the ordinary fallback notice as well, so
the originally requested model stays visible and the two causes are not
conflated.

Streaming has a limit: a refusal that arrives after streamed output has begun
keeps the existing interrupted-reply behavior; fallback applies only to refusals
detected before output starts.

OpenAI-compatible and Responses wires cannot carry Anthropic's native
refusal/fallback metadata; route Anthropic models through
`[providers.models.anthropic.<alias>]` to get this behavior.

## See also

- [Overview](./overview.md)
- [Provider catalog](./catalog.md): concrete config example for every family
- [Streaming](./streaming.md)
- [Routing](./routing.md)
- [Custom providers](./custom.md)
