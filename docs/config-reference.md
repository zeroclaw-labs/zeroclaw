# ZeroClaw Config Reference (Operator-Oriented)

This is a high-signal reference for common config sections and defaults.

Last verified: **February 21, 2026**.

Config path resolution at startup:

1. `ZEROCLAW_WORKSPACE` override (if set)
2. persisted `~/.zeroclaw/active_workspace.toml` marker (if present)
3. default `~/.zeroclaw/config.toml`

ZeroClaw logs the resolved config on startup at `INFO` level:

- `Config loaded` with fields: `path`, `workspace`, `source`, `initialized`

Schema export command:

- `zeroclaw config schema` (prints JSON Schema draft 2020-12 to stdout)

## Core Keys

| Key | Default | Notes |
|---|---|---|
| `default_provider` | `openrouter` | provider ID or alias |
| `provider_api` | unset | Optional API mode for `custom:<url>` providers: `openai-chat-completions` or `openai-responses` |
| `default_model` | `anthropic/claude-sonnet-4-6` | model routed through selected provider |
| `default_temperature` | `0.7` | model temperature |
| `model_support_vision` | unset (`None`) | Vision support override for active provider/model |

Notes:

- `model_support_vision = true` forces vision support on (e.g. Ollama running `llava`).
- `model_support_vision = false` forces vision support off.
- Unset keeps the provider's built-in default.
- Environment override: `ZEROCLAW_MODEL_SUPPORT_VISION` or `MODEL_SUPPORT_VISION` (values: `true`/`false`/`1`/`0`/`yes`/`no`/`on`/`off`).

## `[observability]`

| Key | Default | Purpose |
|---|---|---|
| `backend` | `none` | Observability backend: `none`, `noop`, `log`, `prometheus`, `otel`, `opentelemetry`, or `otlp` |
| `otel_endpoint` | `http://localhost:4318` | OTLP HTTP endpoint used when backend is `otel` |
| `otel_service_name` | `zeroclaw` | Service name emitted to OTLP collector |
| `runtime_trace_mode` | `none` | Runtime trace storage mode: `none`, `rolling`, or `full` |
| `runtime_trace_path` | `state/runtime-trace.jsonl` | Runtime trace JSONL path (relative to workspace unless absolute) |
| `runtime_trace_max_entries` | `200` | Maximum retained events when `runtime_trace_mode = "rolling"` |

Notes:

- `backend = "otel"` uses OTLP HTTP export with a blocking exporter client so spans and metrics can be emitted safely from non-Tokio contexts.
- Alias values `opentelemetry` and `otlp` map to the same OTel backend.
- Runtime traces are intended for debugging tool-call failures and malformed model tool payloads. They can contain model output text, so keep this disabled by default on shared hosts.
- Query runtime traces with:
  - `zeroclaw doctor traces --limit 20`
  - `zeroclaw doctor traces --event tool_call_result --contains \"error\"`
  - `zeroclaw doctor traces --id <trace-id>`

Example:

```toml
[observability]
backend = "otel"
otel_endpoint = "http://localhost:4318"
otel_service_name = "zeroclaw"
runtime_trace_mode = "rolling"
runtime_trace_path = "state/runtime-trace.jsonl"
runtime_trace_max_entries = 200
```

## Environment Provider Overrides

Provider selection can also be controlled by environment variables. Precedence is:

1. `ZEROCLAW_PROVIDER` (explicit override, always wins when non-empty)
2. `PROVIDER` (legacy fallback, only applied when config provider is unset or still `openrouter`)
3. `default_provider` in `config.toml`

Operational note for container users:

- If your `config.toml` sets an explicit custom provider like `custom:https://.../v1`, a default `PROVIDER=openrouter` from Docker/container env will no longer replace it.
- Use `ZEROCLAW_PROVIDER` when you intentionally want runtime env to override a non-default configured provider.
- For OpenAI-compatible Responses fallback transport:
  - `ZEROCLAW_RESPONSES_WEBSOCKET=1` forces websocket-first mode (`wss://.../responses`) for compatible providers.
  - `ZEROCLAW_RESPONSES_WEBSOCKET=0` forces HTTP-only mode.
  - Unset = auto (websocket-first only when endpoint host is `api.openai.com`, then HTTP fallback if websocket fails).

## `[agent]`

| Key | Default | Purpose |
|---|---|---|
| `compact_context` | `false` | When true: bootstrap_max_chars=6000, rag_chunk_limit=2. Use for 13B or smaller models |
| `max_tool_iterations` | `10` | Maximum tool-call loop turns per user message across CLI, gateway, and channels |
| `max_history_messages` | `50` | Maximum conversation history messages retained per session |
| `parallel_tools` | `false` | Enable parallel tool execution within a single iteration |
| `tool_dispatcher` | `auto` | Tool dispatch strategy |

Notes:

- Setting `max_tool_iterations = 0` falls back to safe default `10`.
- If a channel message exceeds this value, the runtime returns: `Agent exceeded maximum tool iterations (<value>)`.
- In CLI, gateway, and channel tool loops, multiple independent tool calls are executed concurrently by default when the pending calls do not require approval gating; result order remains stable.
- `parallel_tools` applies to the `Agent::turn()` API surface. It does not gate the runtime loop used by CLI, gateway, or channel handlers.

## `[security.otp]`

| Key | Default | Purpose |
|---|---|---|
| `enabled` | `false` | Enable OTP gating for sensitive actions/domains |
| `method` | `totp` | OTP method (`totp`, `pairing`, `cli-prompt`) |
| `token_ttl_secs` | `30` | TOTP time-step window in seconds |
| `cache_valid_secs` | `300` | Cache window for recently validated OTP codes |
| `gated_actions` | `["shell","file_write","browser_open","browser","memory_forget"]` | Tool actions protected by OTP |
| `gated_domains` | `[]` | Explicit domain patterns requiring OTP (`*.example.com`, `login.example.com`) |
| `gated_domain_categories` | `[]` | Domain preset categories (`banking`, `medical`, `government`, `identity_providers`) |

Notes:

- Domain patterns support wildcard `*`.
- Category presets expand to curated domain sets during validation.
- Invalid domain globs or unknown categories fail fast at startup.
- When `enabled = true` and no OTP secret exists, ZeroClaw generates one and prints an enrollment URI once.

Example:

```toml
[security.otp]
enabled = true
method = "totp"
token_ttl_secs = 30
cache_valid_secs = 300
gated_actions = ["shell", "browser_open"]
gated_domains = ["*.chase.com", "accounts.google.com"]
gated_domain_categories = ["banking"]
```

## `[security.estop]`

| Key | Default | Purpose |
|---|---|---|
| `enabled` | `false` | Enable emergency-stop state machine and CLI |
| `state_file` | `~/.zeroclaw/estop-state.json` | Persistent estop state path |
| `require_otp_to_resume` | `true` | Require OTP validation before resume operations |

Notes:

- Estop state is persisted atomically and reloaded on startup.
- Corrupted/unreadable estop state falls back to fail-closed `kill_all`.
- Use CLI command `zeroclaw estop` to engage and `zeroclaw estop resume` to clear levels.

## `[agents.<name>]`

Delegate sub-agent configurations. Each key under `[agents]` defines a named sub-agent that the primary agent can delegate to.

| Key | Default | Purpose |
|---|---|---|
| `provider` | _required_ | Provider name (e.g. `"ollama"`, `"openrouter"`, `"anthropic"`) |
| `model` | _required_ | Model name for the sub-agent |
| `system_prompt` | unset | Optional system prompt override for the sub-agent |
| `api_key` | unset | Optional API key override (stored encrypted when `secrets.encrypt = true`) |
| `temperature` | unset | Temperature override for the sub-agent |
| `max_depth` | `3` | Max recursion depth for nested delegation |
| `agentic` | `false` | Enable multi-turn tool-call loop mode for the sub-agent |
| `allowed_tools` | `[]` | Tool allowlist for agentic mode |
| `max_iterations` | `10` | Max tool-call iterations for agentic mode |

Notes:

- `agentic = false` preserves existing single prompt→response delegate behavior.
- `agentic = true` requires at least one matching entry in `allowed_tools`.
- The `delegate` tool is excluded from sub-agent allowlists to prevent re-entrant delegation loops.

```toml
[agents.researcher]
provider = "openrouter"
model = "anthropic/claude-sonnet-4-6"
system_prompt = "You are a research assistant."
max_depth = 2
agentic = true
allowed_tools = ["web_search", "http_request", "file_read"]
max_iterations = 8

[agents.coder]
provider = "ollama"
model = "qwen2.5-coder:32b"
temperature = 0.2
```

## `[runtime]`

| Key | Default | Purpose |
|---|---|---|
| `reasoning_enabled` | unset (`None`) | Global reasoning/thinking override for providers that support explicit controls |

Notes:

- `reasoning_enabled = false` explicitly disables provider-side reasoning for supported providers (currently `ollama`, via request field `think: false`).
- `reasoning_enabled = true` explicitly requests reasoning for supported providers (`think: true` on `ollama`).
- Unset keeps provider defaults.

## `[skills]`

| Key | Default | Purpose |
|---|---|---|
| `open_skills_enabled` | `false` | Opt-in loading/sync of community `open-skills` repository |
| `open_skills_dir` | unset | Optional local path for `open-skills` (defaults to `$HOME/open-skills` when enabled) |
| `prompt_injection_mode` | `full` | Skill prompt verbosity: `full` (inline instructions/tools) or `compact` (name/description/location only) |

Notes:

- Security-first default: ZeroClaw does **not** clone or sync `open-skills` unless `open_skills_enabled = true`.
- Environment overrides:
  - `ZEROCLAW_OPEN_SKILLS_ENABLED` accepts `1/0`, `true/false`, `yes/no`, `on/off`.
  - `ZEROCLAW_OPEN_SKILLS_DIR` overrides the repository path when non-empty.
  - `ZEROCLAW_SKILLS_PROMPT_MODE` accepts `full` or `compact`.
- Precedence for enable flag: `ZEROCLAW_OPEN_SKILLS_ENABLED` → `skills.open_skills_enabled` in `config.toml` → default `false`.
- `prompt_injection_mode = "compact"` is recommended on low-context local models to reduce startup prompt size while keeping skill files available on demand.
- Skill loading and `zeroclaw skills install` both apply a static security audit. Skills that contain symlinks, script-like files, high-risk shell payload snippets, or unsafe markdown link traversal are rejected.

## `[composio]`

| Key | Default | Purpose |
|---|---|---|
| `enabled` | `false` | Enable Composio managed OAuth tools |
| `api_key` | unset | Composio API key used by the `composio` tool |
| `entity_id` | `default` | Default `user_id` sent on connect/execute calls |

Notes:

- Backward compatibility: legacy `enable = true` is accepted as an alias for `enabled = true`.
- If `enabled = false` or `api_key` is missing, the `composio` tool is not registered.
- ZeroClaw requests Composio v3 tools with `toolkit_versions=latest` and executes tools with `version="latest"` to avoid stale default tool revisions.
- Typical flow: call `connect`, complete browser OAuth, then run `execute` for the desired tool action.
- If Composio returns a missing connected-account reference error, call `list_accounts` (optionally with `app`) and pass the returned `connected_account_id` to `execute`.

## `[cost]`

| Key | Default | Purpose |
|---|---|---|
| `enabled` | `false` | Enable cost tracking |
| `daily_limit_usd` | `10.00` | Daily spending limit in USD |
| `monthly_limit_usd` | `100.00` | Monthly spending limit in USD |
| `warn_at_percent` | `80` | Warn when spending reaches this percentage of limit |
| `allow_override` | `false` | Allow requests to exceed budget with `--override` flag |

Notes:

- When `enabled = true`, the runtime tracks per-request cost estimates and enforces daily/monthly limits.
- At `warn_at_percent` threshold, a warning is emitted but requests continue.
- When a limit is reached, requests are rejected unless `allow_override = true` and the `--override` flag is passed.

## `[identity]`

| Key | Default | Purpose |
|---|---|---|
| `format` | `openclaw` | Identity format: `"openclaw"` (default) or `"aieos"` |
| `aieos_path` | unset | Path to AIEOS JSON file (relative to workspace) |
| `aieos_inline` | unset | Inline AIEOS JSON (alternative to file path) |

Notes:

- Use `format = "aieos"` with either `aieos_path` or `aieos_inline` to load an AIEOS / OpenClaw identity document.
- Only one of `aieos_path` or `aieos_inline` should be set; `aieos_path` takes precedence.

## `[multimodal]`

| Key | Default | Purpose |
|---|---|---|
| `max_images` | `4` | Maximum image markers accepted per request |
| `max_image_size_mb` | `5` | Per-image size limit before base64 encoding |
| `allow_remote_fetch` | `false` | Allow fetching `http(s)` image URLs from markers |

Notes:

- Runtime accepts image markers in user messages with syntax: ``[IMAGE:<source>]``.
- Supported sources:
  - Local file path (for example ``[IMAGE:/tmp/screenshot.png]``)
- Data URI (for example ``[IMAGE:data:image/png;base64,...]``)
- Remote URL only when `allow_remote_fetch = true`
- Allowed MIME types: `image/png`, `image/jpeg`, `image/webp`, `image/gif`, `image/bmp`.
- When the active provider does not support vision, requests fail with a structured capability error (`capability=vision`) instead of silently dropping images.

## `[browser]`

| Key | Default | Purpose |
|---|---|---|
| `enabled` | `false` | Enable `browser_open` tool (opens URLs in the system browser without scraping) |
| `allowed_domains` | `[]` | Allowed domains for `browser_open` (exact/subdomain match, or `"*"` for all public domains) |
| `session_name` | unset | Browser session name (for agent-browser automation) |
| `backend` | `agent_browser` | Browser automation backend: `"agent_browser"`, `"rust_native"`, `"computer_use"`, or `"auto"` |
| `native_headless` | `true` | Headless mode for rust-native backend |
| `native_webdriver_url` | `http://127.0.0.1:9515` | WebDriver endpoint URL for rust-native backend |
| `native_chrome_path` | unset | Optional Chrome/Chromium executable path for rust-native backend |

### `[browser.computer_use]`

| Key | Default | Purpose |
|---|---|---|
| `endpoint` | `http://127.0.0.1:8787/v1/actions` | Sidecar endpoint for computer-use actions (OS-level mouse/keyboard/screenshot) |
| `api_key` | unset | Optional bearer token for computer-use sidecar (stored encrypted) |
| `timeout_ms` | `15000` | Per-action request timeout in milliseconds |
| `allow_remote_endpoint` | `false` | Allow remote/public endpoint for computer-use sidecar |
| `window_allowlist` | `[]` | Optional window title/process allowlist forwarded to sidecar policy |
| `max_coordinate_x` | unset | Optional X-axis boundary for coordinate-based actions |
| `max_coordinate_y` | unset | Optional Y-axis boundary for coordinate-based actions |

Notes:

- When `backend = "computer_use"`, the agent delegates browser actions to the sidecar at `computer_use.endpoint`.
- `allow_remote_endpoint = false` (default) rejects any non-loopback endpoint to prevent accidental public exposure.
- Use `window_allowlist` to restrict which OS windows the sidecar can interact with.

## `[http_request]`

| Key | Default | Purpose |
|---|---|---|
| `enabled` | `false` | Enable `http_request` tool for API interactions |
| `allowed_domains` | `[]` | Allowed domains for HTTP requests (exact/subdomain match, or `"*"` for all public domains) |
| `max_response_size` | `1000000` | Maximum response size in bytes (default: 1 MB) |
| `timeout_secs` | `30` | Request timeout in seconds |

Notes:

- Deny-by-default: if `allowed_domains` is empty, all HTTP requests are rejected.
- Use exact domain or subdomain matching (e.g. `"api.example.com"`, `"example.com"`), or `"*"` to allow any public domain.
- Local/private targets are still blocked even when `"*"` is configured.

## `[web_fetch]`

| Key | Default | Purpose |
|---|---|---|
| `enabled` | `false` | Enable `web_fetch` tool for page retrieval |
| `provider` | `fast_html2md` | Fetch provider: `fast_html2md`, `nanohtml2text`, or `firecrawl` |
| `api_key` | unset | Provider API key (required for `provider = "firecrawl"`) |
| `api_url` | unset | Optional provider API base URL override (self-hosted endpoints) |
| `allowed_domains` | `["*"]` | Allowed domains for web fetch (exact/subdomain match) |
| `blocked_domains` | `[]` | Blocklist that overrides allowlist |
| `max_response_size` | `500000` | Maximum output payload length in bytes |
| `timeout_secs` | `30` | Request timeout in seconds |

Notes:

- URL policy is deny-by-default when `allowed_domains` is empty.
- Local/private addresses are blocked even when allowlist contains `"*"`.
- Redirect responses return the validated redirect target URL string instead of destination page content.
- `fast_html2md` is the markdown-preserving default provider.
- `nanohtml2text` requires Cargo feature `web-fetch-plaintext`.
- `firecrawl` requires Cargo feature `firecrawl`.

## `[web_search]`

| Key | Default | Purpose |
|---|---|---|
| `enabled` | `false` | Enable `web_search_tool` |
| `provider` | `duckduckgo` | Search provider: `duckduckgo`, `brave`, or `firecrawl` |
| `api_key` | unset | Generic provider API key (`firecrawl`, optional fallback for `brave`) |
| `api_url` | unset | Optional provider API base URL override (self-hosted Firecrawl) |
| `brave_api_key` | unset | Brave Search API key (used when `provider = "brave"`) |
| `max_results` | `5` | Result count (clamped to 1..10) |
| `timeout_secs` | `15` | Request timeout in seconds |

Notes:

- `provider = "duckduckgo"` needs no API key.
- `provider = "brave"` uses `brave_api_key` first, then falls back to `api_key`.
- `provider = "firecrawl"` requires `api_key` and Cargo feature `firecrawl`.

## `[gateway]`

| Key | Default | Purpose |
|---|---|---|
| `host` | `127.0.0.1` | bind address |
| `port` | `42617` | gateway listen port |
| `require_pairing` | `true` | require pairing before bearer auth |
| `allow_public_bind` | `false` | block accidental public exposure |

## `[gateway.node_control]` (experimental)

| Key | Default | Purpose |
|---|---|---|
| `enabled` | `false` | enable node-control scaffold endpoint (`POST /api/node-control`) |
| `auth_token` | `null` | optional extra shared token checked via `X-Node-Control-Token` |
| `allowed_node_ids` | `[]` | allowlist for `node.describe`/`node.invoke` (`[]` accepts any) |

## `[autonomy]`

| Key | Default | Purpose |
|---|---|---|
| `level` | `supervised` | `read_only`, `supervised`, or `full` |
| `workspace_only` | `true` | reject absolute path inputs unless explicitly disabled |
| `allowed_commands` | _required for shell execution_ | allowlist of executable names, explicit executable paths, or `"*"` |
| `forbidden_paths` | built-in protected list | explicit path denylist (system paths + sensitive dotdirs by default) |
| `allowed_roots` | `[]` | additional roots allowed outside workspace after canonicalization |
| `max_actions_per_hour` | `20` | per-policy action budget |
| `max_cost_per_day_cents` | `500` | per-policy spend guardrail |
| `require_approval_for_medium_risk` | `true` | approval gate for medium-risk commands |
| `block_high_risk_commands` | `true` | hard block for high-risk commands |
| `auto_approve` | `[]` | tool operations always auto-approved |
| `always_ask` | `[]` | tool operations that always require approval |

Notes:

- `level = "full"` skips medium-risk approval gating for shell execution, while still enforcing configured guardrails.
- Access outside the workspace requires `allowed_roots`, even when `workspace_only = false`.
- `allowed_roots` supports absolute paths, `~/...`, and workspace-relative paths.
- `allowed_commands` entries can be command names (for example, `"git"`), explicit executable paths (for example, `"/usr/bin/antigravity"`), or `"*"` to allow any command name/path (risk gates still apply).
- Shell separator/operator parsing is quote-aware. Characters like `;` inside quoted arguments are treated as literals, not command separators.
- Unquoted shell chaining/operators are still enforced by policy checks (`;`, `|`, `&&`, `||`, background chaining, and redirects).

```toml
[autonomy]
workspace_only = false
forbidden_paths = ["/etc", "/root", "/proc", "/sys", "~/.ssh", "~/.gnupg", "~/.aws"]
allowed_roots = ["~/Desktop/projects", "/opt/shared-repo"]
```

## `[memory]`

| Key | Default | Purpose |
|---|---|---|
| `backend` | `sqlite` | `sqlite`, `lucid`, `markdown`, `none` |
| `auto_save` | `true` | persist user-stated inputs only (assistant outputs are excluded) |
| `embedding_provider` | `none` | `none`, `openai`, or custom endpoint |
| `embedding_model` | `text-embedding-3-small` | embedding model ID, or `hint:<name>` route |
| `embedding_dimensions` | `1536` | expected vector size for selected embedding model |
| `vector_weight` | `0.7` | hybrid ranking vector weight |
| `keyword_weight` | `0.3` | hybrid ranking keyword weight |

Notes:

- Memory context injection ignores legacy `assistant_resp*` auto-save keys to prevent old model-authored summaries from being treated as facts.

## `[[model_routes]]` and `[[embedding_routes]]`

Use route hints so integrations can keep stable names while model IDs evolve.

### `[[model_routes]]`

| Key | Default | Purpose |
|---|---|---|
| `hint` | _required_ | Task hint name (e.g. `"reasoning"`, `"fast"`, `"code"`, `"summarize"`) |
| `provider` | _required_ | Provider to route to (must match a known provider name) |
| `model` | _required_ | Model to use with that provider |
| `max_tokens` | unset | Optional per-route output token cap forwarded to provider APIs |
| `api_key` | unset | Optional API key override for this route's provider |

### `[[embedding_routes]]`

| Key | Default | Purpose |
|---|---|---|
| `hint` | _required_ | Route hint name (e.g. `"semantic"`, `"archive"`, `"faq"`) |
| `provider` | _required_ | Embedding provider (`"none"`, `"openai"`, or `"custom:<url>"`) |
| `model` | _required_ | Embedding model to use with that provider |
| `dimensions` | unset | Optional embedding dimension override for this route |
| `api_key` | unset | Optional API key override for this route's provider |

```toml
[memory]
embedding_model = "hint:semantic"

[[model_routes]]
hint = "reasoning"
provider = "openrouter"
model = "provider/model-id"
max_tokens = 8192

[[embedding_routes]]
hint = "semantic"
provider = "openai"
model = "text-embedding-3-small"
dimensions = 1536
```

Upgrade strategy:

1. Keep hints stable (`hint:reasoning`, `hint:semantic`).
2. Update only `model = "...new-version..."` in the route entries.
3. Validate with `zeroclaw doctor` before restart/rollout.

Natural-language config path:

- During normal agent chat, ask the assistant to rewire routes in plain language.
- The runtime can persist these updates via tool `model_routing_config` (defaults, scenarios, and delegate sub-agents) without manual TOML editing.

Example requests:

- `Set conversation to provider kimi, model moonshot-v1-8k.`
- `Set coding to provider openai, model gpt-5.3-codex, and auto-route when message contains code blocks.`
- `Create a coder sub-agent using openai/gpt-5.3-codex with tools file_read,file_write,shell.`

## `[query_classification]`

Automatic model hint routing — maps user messages to `[[model_routes]]` hints based on content patterns.

| Key | Default | Purpose |
|---|---|---|
| `enabled` | `false` | Enable automatic query classification |
| `rules` | `[]` | Classification rules (evaluated in priority order) |

Each rule in `rules`:

| Key | Default | Purpose |
|---|---|---|
| `hint` | _required_ | Must match a `[[model_routes]]` hint value |
| `keywords` | `[]` | Case-insensitive substring matches |
| `patterns` | `[]` | Case-sensitive literal matches (for code fences, keywords like `"fn "`) |
| `min_length` | unset | Only match if message length ≥ N chars |
| `max_length` | unset | Only match if message length ≤ N chars |
| `priority` | `0` | Higher priority rules are checked first |

```toml
[query_classification]
enabled = true

[[query_classification.rules]]
hint = "reasoning"
keywords = ["explain", "analyze", "why"]
min_length = 200
priority = 10

[[query_classification.rules]]
hint = "fast"
keywords = ["hi", "hello", "thanks"]
max_length = 50
priority = 5
```

## `[channels_config]`

Top-level channel options are configured under `channels_config`.

| Key | Default | Purpose |
|---|---|---|
| `message_timeout_secs` | `300` | Base timeout in seconds for channel message processing; runtime scales this with tool-loop depth (up to 4x) |

Examples:

- `[channels_config.telegram]`
- `[channels_config.discord]`
- `[channels_config.whatsapp]`
- `[channels_config.linq]`
- `[channels_config.nextcloud_talk]`
- `[channels_config.email]`
- `[channels_config.nostr]`

Notes:

- Default `300s` is optimized for on-device LLMs (Ollama) which are slower than cloud APIs.
- Runtime timeout budget is `message_timeout_secs * scale`, where `scale = min(max_tool_iterations, 4)` and a minimum of `1`.
- This scaling avoids false timeouts when the first LLM turn is slow/retried but later tool-loop turns still need to complete.
- If using cloud APIs (OpenAI, Anthropic, etc.), you can reduce this to `60` or lower.
- Values below `30` are clamped to `30` to avoid immediate timeout churn.
- When a timeout occurs, users receive: `⚠️ Request timed out while waiting for the model. Please try again.`
- Telegram-only interruption behavior is controlled with `channels_config.telegram.interrupt_on_new_message` (default `false`).
  When enabled, a newer message from the same sender in the same chat cancels the in-flight request and preserves interrupted user context.
- While `zeroclaw channel start` is running, updates to `default_provider`, `default_model`, `default_temperature`, `api_key`, `api_url`, and `reliability.*` are hot-applied from `config.toml` on the next inbound message.

### `[channels_config.nostr]`

| Key | Default | Purpose |
|---|---|---|
| `private_key` | _required_ | Nostr private key (hex or `nsec1…` bech32); encrypted at rest when `secrets.encrypt = true` |
| `relays` | see note | List of relay WebSocket URLs; defaults to `relay.damus.io`, `nos.lol`, `relay.primal.net`, `relay.snort.social` |
| `allowed_pubkeys` | `[]` (deny all) | Sender allowlist (hex or `npub1…`); use `"*"` to allow all senders |

Notes:

- Supports both NIP-04 (legacy encrypted DMs) and NIP-17 (gift-wrapped private messages). Replies mirror the sender's protocol automatically.
- The `private_key` is a high-value secret; keep `secrets.encrypt = true` (the default) in production.

See detailed channel matrix and allowlist behavior in [channels-reference.md](channels-reference.md).

### `[channels_config.whatsapp]`

WhatsApp supports two backends under one config table.

Cloud API mode (Meta webhook):

| Key | Required | Purpose |
|---|---|---|
| `access_token` | Yes | Meta Cloud API bearer token |
| `phone_number_id` | Yes | Meta phone number ID |
| `verify_token` | Yes | Webhook verification token |
| `app_secret` | Optional | Enables webhook signature verification (`X-Hub-Signature-256`) |
| `allowed_numbers` | Recommended | Allowed inbound numbers (`[]` = deny all, `"*"` = allow all) |

WhatsApp Web mode (native client):

| Key | Required | Purpose |
|---|---|---|
| `session_path` | Yes | Persistent SQLite session path |
| `pair_phone` | Optional | Pair-code flow phone number (digits only) |
| `pair_code` | Optional | Custom pair code (otherwise auto-generated) |
| `allowed_numbers` | Recommended | Allowed inbound numbers (`[]` = deny all, `"*"` = allow all) |

Notes:

- WhatsApp Web requires build flag `whatsapp-web`.
- If both Cloud and Web fields are present, Cloud mode wins for backward compatibility.

### `[channels_config.linq]`

Linq Partner V3 API integration for iMessage, RCS, and SMS.

| Key | Required | Purpose |
|---|---|---|
| `api_token` | Yes | Linq Partner API bearer token |
| `from_phone` | Yes | Phone number to send from (E.164 format) |
| `signing_secret` | Optional | Webhook signing secret for HMAC-SHA256 signature verification |
| `allowed_senders` | Recommended | Allowed inbound phone numbers (`[]` = deny all, `"*"` = allow all) |

Notes:

- Webhook endpoint is `POST /linq`.
- `ZEROCLAW_LINQ_SIGNING_SECRET` overrides `signing_secret` when set.
- Signatures use `X-Webhook-Signature` and `X-Webhook-Timestamp` headers; stale timestamps (>300s) are rejected.
- See [channels-reference.md](channels-reference.md) for full config examples.

### `[channels_config.nextcloud_talk]`

Native Nextcloud Talk bot integration (webhook receive + OCS send API).

| Key | Required | Purpose |
|---|---|---|
| `base_url` | Yes | Nextcloud base URL (e.g. `https://cloud.example.com`) |
| `app_token` | Yes | Bot app token used for OCS bearer auth |
| `webhook_secret` | Optional | Enables webhook signature verification |
| `allowed_users` | Recommended | Allowed Nextcloud actor IDs (`[]` = deny all, `"*"` = allow all) |

Notes:

- Webhook endpoint is `POST /nextcloud-talk`.
- `ZEROCLAW_NEXTCLOUD_TALK_WEBHOOK_SECRET` overrides `webhook_secret` when set.
- See [nextcloud-talk-setup.md](nextcloud-talk-setup.md) for setup and troubleshooting.

## `[hardware]`

Hardware wizard configuration for physical-world access (STM32, probe, serial).

| Key | Default | Purpose |
|---|---|---|
| `enabled` | `false` | Whether hardware access is enabled |
| `transport` | `none` | Transport mode: `"none"`, `"native"`, `"serial"`, or `"probe"` |
| `serial_port` | unset | Serial port path (e.g. `"/dev/ttyACM0"`) |
| `baud_rate` | `115200` | Serial baud rate |
| `probe_target` | unset | Probe target chip (e.g. `"STM32F401RE"`) |
| `workspace_datasheets` | `false` | Enable workspace datasheet RAG (index PDF schematics for AI pin lookups) |

Notes:

- Use `transport = "serial"` with `serial_port` for USB-serial connections.
- Use `transport = "probe"` with `probe_target` for debug-probe flashing (e.g. ST-Link).
- See [hardware-peripherals-design.md](hardware-peripherals-design.md) for protocol details.

## `[peripherals]`

Higher-level peripheral board configuration. Boards become agent tools when enabled.

| Key | Default | Purpose |
|---|---|---|
| `enabled` | `false` | Enable peripheral support (boards become agent tools) |
| `boards` | `[]` | Board configurations |
| `datasheet_dir` | unset | Path to datasheet docs (relative to workspace) for RAG retrieval |

Each entry in `boards`:

| Key | Default | Purpose |
|---|---|---|
| `board` | _required_ | Board type: `"nucleo-f401re"`, `"rpi-gpio"`, `"esp32"`, etc. |
| `transport` | `serial` | Transport: `"serial"`, `"native"`, `"websocket"` |
| `path` | unset | Path for serial: `"/dev/ttyACM0"`, `"/dev/ttyUSB0"` |
| `baud` | `115200` | Baud rate for serial |

```toml
[peripherals]
enabled = true
datasheet_dir = "docs/datasheets"

[[peripherals.boards]]
board = "nucleo-f401re"
transport = "serial"
path = "/dev/ttyACM0"
baud = 115200

[[peripherals.boards]]
board = "rpi-gpio"
transport = "native"
```

Notes:

- Place `.md`/`.txt` datasheet files named by board (e.g. `nucleo-f401re.md`, `rpi-gpio.md`) in `datasheet_dir` for RAG retrieval.
- See [hardware-peripherals-design.md](hardware-peripherals-design.md) for board protocol and firmware notes.

## Security-Relevant Defaults

- deny-by-default channel allowlists (`[]` means deny all)
- pairing required on gateway by default
- public bind disabled by default

## Validation Commands

After editing config:

```bash
zeroclaw status
zeroclaw doctor
zeroclaw channel doctor
zeroclaw service restart
```

## Related Docs

- [channels-reference.md](channels-reference.md)
- [providers-reference.md](providers-reference.md)
- [operations-runbook.md](operations-runbook.md)
- [troubleshooting.md](troubleshooting.md)
