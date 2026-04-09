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
| `default_model` | `anthropic/claude-sonnet-4-6` | model routed through selected provider |
| `default_temperature` | `0.7` | model temperature |

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

## `[agent]`

| Key | Default | Purpose |
|---|---|---|
| `compact_context` | `true` | When true: bootstrap_max_chars=6000, rag_chunk_limit=2. Use for 13B or smaller models |
| `max_tool_iterations` | `10` | Maximum tool-call loop turns per user message across CLI, gateway, and channels |
| `max_history_messages` | `50` | Maximum conversation history messages retained per session |
| `parallel_tools` | `false` | Enable parallel tool execution within a single iteration |
| `tool_dispatcher` | `auto` | Tool dispatch strategy |
| `tool_call_dedup_exempt` | `[]` | Tool names exempt from within-turn duplicate-call suppression |
| `tool_filter_groups` | `[]` | Per-turn MCP tool schema filter groups (see below) |

Notes:

- Setting `max_tool_iterations = 0` falls back to safe default `10`.
- If a channel message exceeds this value, the runtime returns: `Agent exceeded maximum tool iterations (<value>)`.
- In CLI, gateway, and channel tool loops, multiple independent tool calls are executed concurrently by default when the pending calls do not require approval gating; result order remains stable.
- `parallel_tools` applies to the `Agent::turn()` API surface. It does not gate the runtime loop used by CLI, gateway, or channel handlers.
- `tool_call_dedup_exempt` accepts an array of exact tool names. Tools listed here are allowed to be called multiple times with identical arguments in the same turn, bypassing the dedup check. Example: `tool_call_dedup_exempt = ["browser"]`.

### `tool_filter_groups`

Reduces per-turn token overhead by limiting which MCP tool schemas are sent to the LLM on each turn. Built-in (non-MCP) tools always pass through unchanged.

Each entry is a table with:

| Field | Type | Purpose |
|---|---|---|
| `mode` | `"always"` \| `"dynamic"` | `always`: tool is included unconditionally. `dynamic`: tool is included only when the user message contains a keyword. |
| `tools` | `[string]` | Tool name patterns. Single `*` wildcard supported (prefix/suffix/infix), e.g. `"mcp_vikunja_*"`. |
| `keywords` | `[string]` | (Dynamic only) Case-insensitive substrings matched against the last user message. |

When `tool_filter_groups` is empty the feature is inactive and all tools pass through (backward-compatible default).

Example:

```toml
[agent]
# Vikunja task-management MCP tools are always available.
[[agent.tool_filter_groups]]
mode = "always"
tools = ["mcp_vikunja_*"]

# Browser MCP tools are only included when the user message mentions browsing.
[[agent.tool_filter_groups]]
mode = "dynamic"
tools = ["mcp_browser_*"]
keywords = ["browse", "navigate", "open url", "screenshot"]
```

## `[pacing]`

Pacing controls for slow/local LLM workloads (Ollama, llama.cpp, vLLM). All keys are optional; when absent, existing behavior is preserved.

| Key | Default | Purpose |
|---|---|---|
| `step_timeout_secs` | _none_ | Per-step timeout: maximum seconds for a single LLM inference turn. Catches a truly hung model without terminating the overall task loop |
| `loop_detection_min_elapsed_secs` | _none_ | Minimum elapsed seconds before loop detection activates. Tasks completing under this threshold get aggressive loop protection; longer-running tasks receive a grace period |
| `loop_ignore_tools` | `[]` | Tool names excluded from identical-output loop detection. Useful for browser workflows where `browser_screenshot` structurally resembles a loop |
| `message_timeout_scale_max` | `4` | Override for the hardcoded timeout scaling cap. The channel message timeout budget is `message_timeout_secs * min(max_tool_iterations, message_timeout_scale_max)` |

Notes:

- These settings are intended for local/slow LLM deployments. Cloud-provider users typically do not need them.
- `step_timeout_secs` operates independently of the total channel message timeout budget. A step timeout abort does not consume the overall budget; the loop simply stops.
- `loop_detection_min_elapsed_secs` delays loop-detection counting, not the task itself. Loop protection remains fully active for short tasks (the default).
- `loop_ignore_tools` only suppresses tool-output-based loop detection for the listed tools. Other safety features (max iterations, overall timeout) remain active.
- `message_timeout_scale_max` must be >= 1. Setting it higher than `max_tool_iterations` has no additional effect (the formula uses `min()`).
- Example configuration for a slow local Ollama deployment:

```toml
[pacing]
step_timeout_secs = 120
loop_detection_min_elapsed_secs = 60
loop_ignore_tools = ["browser_screenshot", "browser_navigate"]
message_timeout_scale_max = 8
```

## `[reliability]`

Resilience configuration for multi-model fallback chains, API key rotation, and retry policies.

| Key | Type | Default | Purpose |
|---|---|---|---|
| `fallback_providers` | `[string]` | `[]` | Ordered list of fallback provider IDs when primary fails |
| `model_fallbacks` | `{string: [string]}` | `{}` | Per-model fallback chains (map of model → list of alternatives) |
| `api_keys` | `[string]` | `[]` | Additional API keys for rate-limit (429) rotation |
| `provider_retries` | `u32` | `2` | Retry attempts per provider before moving to next fallback |
| `provider_backoff_ms` | `u64` | `500` | Initial exponential backoff delay in milliseconds |
| `channel_initial_backoff_secs` | `u64` | `1` | Initial backoff for channel/daemon restart attempts |
| `channel_max_backoff_secs` | `u64` | `60` | Maximum backoff for channel/daemon restart attempts |
| `scheduler_poll_secs` | `u64` | `5` | Scheduler polling cadence in seconds |
| `scheduler_retries` | `u32` | `3` | Maximum retry attempts for cron job execution |

Notes:

- `fallback_providers` is a list of provider IDs to try in order when the primary provider fails (timeout, connection error, 503, rate limit after key rotation).
- Each fallback provider resolves credentials independently using the standard resolution order: explicit config → provider-specific env var → `ZEROCLAW_API_KEY` → `API_KEY`.
- `model_fallbacks` allows semantic fallbacks when a specific model is unavailable. Example: `{ "claude-opus-4-20250514" = ["claude-sonnet-4-20250514"] }`.
- `api_keys` supplies additional API keys that ZeroClaw rotates through on `429` (rate limit) responses. The primary `api_key` (set globally or per-channel) is tried first.
- `provider_retries` applies before each fallback attempt. With `provider_retries = 2` and `provider_backoff_ms = 500`, the runtime retries with delays of 500ms, then 1000ms.
- `channel_initial_backoff_secs` and `channel_max_backoff_secs` control exponential backoff for channel reconnection after transient failures.
- `scheduler_poll_secs` controls how often the built-in scheduler checks for cron-triggered tasks.
- `scheduler_retries` limits retry attempts for failed scheduled task executions.
- Hot-reload enabled: updates to this section take effect on the next channel message or provider request without restart.

Example:

```toml
[reliability]
fallback_providers = ["anthropic", "groq", "openrouter"]
api_keys = ["sk-backup-1", "sk-backup-2"]

[reliability.model_fallbacks]
"claude-opus-4-20250514" = ["claude-sonnet-4-20250514"]
"gpt-4o" = ["gpt-4-turbo", "gpt-3.5-turbo"]

provider_retries = 3
provider_backoff_ms = 1000
channel_initial_backoff_secs = 2
channel_max_backoff_secs = 120
scheduler_poll_secs = 10
scheduler_retries = 5
```

Fallback triggers:

- **Timeout**: No response within the provider timeout window.
- **Connection error**: Network/DNS failure.
- **Service unavailable (503)**: Provider temporary outage.
- **Rate limit (429)**: First, rotates through `api_keys` on the same provider/model; then falls back to next provider.
- **Model not found**: If `model_fallbacks` is configured for that model, tries alternatives in order.

Fallback does **not** trigger on:

- **Client error (400)**: Malformed request; retrying won't help.
- **Invalid credentials (401/403)**: Permanent auth failure.
- **Model output errors**: The provider responded but the model returned an error in its response.

For detailed configuration guidance, see [Multi-Model Setup and Fallback Chains](/docs/getting-started/multi-model-setup.md).

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
| `timeout_secs` | `120` | Timeout in seconds for non-agentic provider calls (1–3600) |
| `agentic_timeout_secs` | `300` | Timeout in seconds for agentic sub-agent loops (1–3600) |
| `skills_directory` | unset | Optional skills directory path (workspace-relative) for scoped skill loading |
| `max_context_tokens` | unset | Cap on the total tokens sent in the sub-agent context window; truncates oldest messages when exceeded |
| `max_tool_result_chars` | unset | Maximum characters retained from each tool result; longer outputs are truncated with a summary marker |

Notes:

- `agentic = false` preserves existing single prompt→response delegate behavior.
- `agentic = true` requires at least one matching entry in `allowed_tools`.
- The `delegate` tool is excluded from sub-agent allowlists to prevent re-entrant delegation loops.
- Sub-agents receive an enriched system prompt containing: tools section (allowed tools with parameters), skills section (from scoped or default directory), workspace path, current date/time, safety constraints, and shell policy when `shell` is in the effective tool list.
- When `skills_directory` is unset or empty, the sub-agent loads skills from the default workspace `skills/` directory. When set, skills are loaded exclusively from that directory (relative to workspace root), enabling per-agent scoped skill sets.

```toml
[agents.researcher]
provider = "openrouter"
model = "anthropic/claude-sonnet-4-6"
system_prompt = "You are a research assistant."
max_depth = 2
agentic = true
allowed_tools = ["web_search", "http_request", "file_read"]
max_iterations = 8
agentic_timeout_secs = 600

[agents.coder]
provider = "ollama"
model = "qwen2.5-coder:32b"
temperature = 0.2
timeout_secs = 60

[agents.code_reviewer]
provider = "anthropic"
model = "claude-opus-4-5"
system_prompt = "You are an expert code reviewer focused on security and performance."
agentic = true
allowed_tools = ["file_read", "shell"]
skills_directory = "skills/code-review"
```

## `[delegate]`

Concurrency controls for sub-agent execution.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `max_concurrent_subagents` | int | `0` | Maximum concurrent sub-agents. `0` = unlimited. Prevents LLM slot saturation on capacity-limited backends. |

Notes:

- When `max_concurrent_subagents > 0`, a shared semaphore limits the total number of simultaneously executing sub-agents across both `delegate` and `spawn_agent` invocations.
- Excess requests wait for a permit rather than failing.
- Recommended values: `1` for 1-2 LLM slots, `2` for 3-4 slots, `3-4` for 5+ slots, `0` (unlimited) for cloud APIs.

Example:

```toml
[delegate]
max_concurrent_subagents = 2
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

## `[google_workspace]`

| Key | Default | Purpose |
|---|---|---|
| `enabled` | `false` | Enable the `google_workspace` tool |
| `credentials_path` | unset | Path to Google service account or OAuth credentials JSON |
| `default_account` | unset | Default Google account passed as `--account` to `gws` |
| `allowed_services` | (built-in list) | Services the agent may access: `drive`, `gmail`, `calendar`, `sheets`, `docs`, `slides`, `tasks`, `people`, `chat`, `classroom`, `forms`, `keep`, `meet`, `events` |
| `rate_limit_per_minute` | `60` | Maximum `gws` calls per minute |
| `timeout_secs` | `30` | Per-call execution timeout before kill |
| `audit_log` | `false` | Emit an `INFO` log line for every `gws` call |

### `[[google_workspace.allowed_operations]]`

When this array is non-empty, only exact matches pass. An entry matches a call when
`service`, `resource`, `sub_resource`, and `method` all agree. When the array is
empty (the default), all combinations within `allowed_services` are available.

| Key | Required | Purpose |
|---|---|---|
| `service` | yes | Service identifier (must match an entry in `allowed_services`) |
| `resource` | yes | Top-level resource name (`users` for Gmail, `files` for Drive, `events` for Calendar) |
| `sub_resource` | no | Sub-resource for 4-segment gws commands. Gmail operations use `gws gmail users <sub_resource> <method>`, so Gmail entries need `sub_resource` to match at runtime. Drive, Calendar, and most other services use 3-segment commands and omit it. |
| `methods` | yes | One or more method names allowed on that resource/sub_resource |

Gmail uses `gws gmail users <sub_resource> <method>` for all operations. A Gmail
entry without `sub_resource` will never match at runtime. Drive and Calendar use
3-segment commands and omit `sub_resource`.

```toml
[google_workspace]
enabled = true
default_account = "owner@company.com"
allowed_services = ["gmail"]
audit_log = true

[[google_workspace.allowed_operations]]
service = "gmail"
resource = "users"
sub_resource = "messages"
methods = ["list", "get"]

[[google_workspace.allowed_operations]]
service = "gmail"
resource = "users"
sub_resource = "drafts"
methods = ["list", "get", "create", "update"]
```

Notes:

- Requires `gws` to be installed and authenticated (`gws auth login`). Install: `npm install -g @googleworkspace/cli`.
- `credentials_path` sets `GOOGLE_APPLICATION_CREDENTIALS` before each call.
- `allowed_services` defaults to the built-in list if omitted or empty.
- Validation rejects duplicate `(service, resource)` pairs and duplicate methods within a single entry.
- See `docs/superpowers/specs/2026-03-19-google-workspace-operation-allowlist.md` for the full policy model and verified workflow examples.

## `[gateway]`

| Key | Default | Purpose |
|---|---|---|
| `host` | `127.0.0.1` | bind address |
| `port` | `42617` | gateway listen port |
| `require_pairing` | `true` | require pairing before bearer auth |
| `allow_public_bind` | `false` | block accidental public exposure |
| `path_prefix` | _(none)_ | URL path prefix for reverse-proxy deployments (e.g. `"/zeroclaw"`) |

When deploying behind a reverse proxy that maps ZeroClaw to a sub-path,
set `path_prefix` to that sub-path (e.g. `"/zeroclaw"`). All gateway
routes will be served under this prefix. The value must start with `/`
and must not end with `/`.

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
| `message_timeout_secs` | `300` | Base timeout in seconds for channel message processing; runtime scales this with tool-loop depth (up to 4x, overridable via `[pacing].message_timeout_scale_max`) |

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
- Runtime timeout budget is `message_timeout_secs * scale`, where `scale = min(max_tool_iterations, cap)` and a minimum of `1`. The default cap is `4`; override with `[pacing].message_timeout_scale_max`.
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
| `mention_only` | Optional | When `true`, only respond to group messages that @-mention the bot (DMs always processed) |

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
| `bot_name` | Optional | Display name of the bot in Nextcloud Talk (e.g. `"zeroclaw"`). Used to filter out the bot's own messages and prevent feedback loops. |

Notes:

- Webhook endpoint is `POST /nextcloud-talk`.
- `ZEROCLAW_NEXTCLOUD_TALK_WEBHOOK_SECRET` overrides `webhook_secret` when set.
- See [nextcloud-talk-setup.md](../../setup-guides/nextcloud-talk-setup.md) for setup and troubleshooting.

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
- See [hardware-peripherals-design.md](../../hardware/hardware-peripherals-design.md) for protocol details.

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
- See [hardware-peripherals-design.md](../../hardware/hardware-peripherals-design.md) for board protocol and firmware notes.

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
- [operations-runbook.md](../../ops/operations-runbook.md)
- [troubleshooting.md](../../ops/troubleshooting.md)
