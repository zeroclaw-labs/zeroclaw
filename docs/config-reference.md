# ZeroClaw Config Reference (Operator-Oriented)

This is a high-signal reference for common config sections and defaults.

Last verified: **February 28, 2026**.

Config path resolution at startup:

1. `ZEROCLAW_WORKSPACE` override (if set)
2. persisted `~/.zeroclaw/active_workspace.toml` marker (if present)
3. default `~/.zeroclaw/config.toml`

ZeroClaw logs the resolved config on startup at `INFO` level:

- `Config loaded` with fields: `path`, `workspace`, `source`, `initialized`

CLI commands for config inspection and modification:

- `zeroclaw config show` — print effective config as JSON (secrets masked)
- `zeroclaw config get <key>` — query a value by dot-path (e.g. `zeroclaw config get gateway.port`)
- `zeroclaw config set <key> <value>` — update a value and save to `config.toml`
- `zeroclaw config schema` — print JSON Schema (draft 2020-12) to stdout

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

## `[model_providers.<profile>]`

Use named profiles to map a logical provider id to a provider name/base URL and optional profile-scoped credentials.

| Key | Default | Notes |
|---|---|---|
| `name` | unset | Optional provider id override (for example `openai`, `openai-codex`) |
| `base_url` | unset | Optional OpenAI-compatible endpoint URL |
| `wire_api` | unset | Optional protocol mode: `responses` or `chat_completions` |
| `model` | unset | Optional profile-scoped default model |
| `api_key` | unset | Optional profile-scoped API key (used when top-level `api_key` is empty) |
| `requires_openai_auth` | `false` | Load OpenAI auth material (`OPENAI_API_KEY` / Codex auth file) |

Notes:

- If both top-level `api_key` and profile `api_key` are present, top-level `api_key` wins.
- If top-level `default_model` is still the global OpenRouter default, profile `model` is used as an automatic compatibility override.
- Secrets encryption applies to profile API keys when `secrets.encrypt = true`.

Example:

```toml
default_provider = "sub2api"

[model_providers.sub2api]
name = "sub2api"
base_url = "https://api.example.com/v1"
wire_api = "chat_completions"
model = "qwen-max"
api_key = "sk-profile-key"
```

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
| `compact_context` | `true` | When true: bootstrap_max_chars=6000, rag_chunk_limit=2. Use for 13B or smaller models |
| `max_tool_iterations` | `20` | Maximum tool-call loop turns per user message across CLI, gateway, and channels |
| `max_history_messages` | `50` | Maximum conversation history messages retained per session |
| `parallel_tools` | `false` | Enable parallel tool execution within a single iteration |
| `tool_dispatcher` | `auto` | Tool dispatch strategy |
| `loop_detection_no_progress_threshold` | `3` | Same tool+args producing identical output this many times triggers loop detection. `0` disables |
| `loop_detection_ping_pong_cycles` | `2` | A→B→A→B alternating pattern cycle count threshold. `0` disables |
| `loop_detection_failure_streak` | `3` | Same tool consecutive failure count threshold. `0` disables |

Notes:

- Setting `max_tool_iterations = 0` falls back to safe default `20`.
- If a channel message exceeds this value, the runtime returns: `Agent exceeded maximum tool iterations (<value>)`.
- In CLI, gateway, and channel tool loops, multiple independent tool calls are executed concurrently by default when the pending calls do not require approval gating; result order remains stable.
- `parallel_tools` applies to the `Agent::turn()` API surface. It does not gate the runtime loop used by CLI, gateway, or channel handlers.
- **Loop detection** intervenes before `max_tool_iterations` is exhausted. On first detection the agent receives a self-correction prompt; if the loop persists the agent is stopped early. Detection is result-aware: repeated calls with *different* outputs (genuine progress) do not trigger. Set any threshold to `0` to disable that detector.

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

## `[security.url_access]`

| Key | Default | Purpose |
|---|---|---|
| `block_private_ip` | `true` | Block local/private/link-local/multicast addresses by default |
| `allow_cidrs` | `[]` | CIDR ranges allowed to bypass private-IP blocking (`100.64.0.0/10`, `198.18.0.0/15`) |
| `allow_domains` | `[]` | Domain patterns that bypass private-IP blocking before DNS checks (`internal.example`, `*.svc.local`) |
| `allow_loopback` | `false` | Permit loopback targets (`localhost`, `127.0.0.1`, `::1`) |
| `require_first_visit_approval` | `false` | Require explicit human confirmation before first-time access to unseen domains |
| `enforce_domain_allowlist` | `false` | Require all URL targets to match `domain_allowlist` (in addition to tool-level allowlists) |
| `domain_allowlist` | `[]` | Global trusted domain allowlist shared across URL tools |
| `domain_blocklist` | `[]` | Global domain denylist shared across URL tools (highest priority) |
| `approved_domains` | `[]` | Persisted first-visit approvals granted by a human operator |

Notes:

- This policy is shared by `browser_open`, `http_request`, and `web_fetch`.
- `browser` automation (`action = "open"`) also follows this policy.
- Tool-level allowlists still apply. `allow_domains` / `allow_cidrs` only override private/local blocking.
- `domain_blocklist` is evaluated before allowlists; blocked hosts are always denied.
- With `require_first_visit_approval = true`, unseen domains are denied until added to `approved_domains` (or matched by `domain_allowlist`).
- DNS rebinding protection remains enabled: resolved local/private IPs are denied unless explicitly allowlisted.
- Agents can inspect/update these settings at runtime via `web_access_config` (`action=get|set|check_url`).
- In supervised mode, `web_access_config` mutations still require normal tool approval unless explicitly auto-approved.

Example:

```toml
[security.url_access]
block_private_ip = true
allow_cidrs = ["100.64.0.0/10", "198.18.0.0/15"]
allow_domains = ["internal.example", "*.svc.local"]
allow_loopback = false
require_first_visit_approval = true
enforce_domain_allowlist = false
domain_allowlist = ["docs.rs", "github.com", "*.rust-lang.org"]
domain_blocklist = ["*.malware.test"]
approved_domains = ["example.com"]
```

Runtime workflow (`web_access_config`):

1. Start strict-first mode (deny unknown domains until reviewed):

```json
{"action":"set","require_first_visit_approval":true,"enforce_domain_allowlist":false}
```

2. Dry-run a target URL before access:

```json
{"action":"check_url","url":"https://docs.rs"}
```

3. After human confirmation, persist approval for future runs:

```json
{"action":"set","add_approved_domains":["docs.rs"]}
```

4. Escalate to strict allowlist-only mode (recommended for production agents):

```json
{"action":"set","enforce_domain_allowlist":true,"domain_allowlist":["docs.rs","github.com","*.rust-lang.org"]}
```

5. Emergency deny of a domain across all URL tools:

```json
{"action":"set","add_domain_blocklist":["*.malware.test"]}
```

Operational guidance:

- Use `approved_domains` for iterative onboarding and temporary approvals.
- Use `domain_allowlist` for stable long-term trusted domains.
- Use `domain_blocklist` for immediate global deny; it always overrides allow rules.
- Keep `allow_domains` focused on private-network bypass cases only (`internal.example`, `*.svc.local`).

Environment overrides:

- `ZEROCLAW_URL_ACCESS_BLOCK_PRIVATE_IP` / `URL_ACCESS_BLOCK_PRIVATE_IP`
- `ZEROCLAW_URL_ACCESS_ALLOW_LOOPBACK` / `URL_ACCESS_ALLOW_LOOPBACK`
- `ZEROCLAW_URL_ACCESS_REQUIRE_FIRST_VISIT_APPROVAL` / `URL_ACCESS_REQUIRE_FIRST_VISIT_APPROVAL`
- `ZEROCLAW_URL_ACCESS_ENFORCE_DOMAIN_ALLOWLIST` / `URL_ACCESS_ENFORCE_DOMAIN_ALLOWLIST`
- `ZEROCLAW_URL_ACCESS_ALLOW_CIDRS` / `URL_ACCESS_ALLOW_CIDRS` (comma-separated)
- `ZEROCLAW_URL_ACCESS_ALLOW_DOMAINS` / `URL_ACCESS_ALLOW_DOMAINS` (comma-separated)
- `ZEROCLAW_URL_ACCESS_DOMAIN_ALLOWLIST` / `URL_ACCESS_DOMAIN_ALLOWLIST` (comma-separated)
- `ZEROCLAW_URL_ACCESS_DOMAIN_BLOCKLIST` / `URL_ACCESS_DOMAIN_BLOCKLIST` (comma-separated)
- `ZEROCLAW_URL_ACCESS_APPROVED_DOMAINS` / `URL_ACCESS_APPROVED_DOMAINS` (comma-separated)

## `[security.syscall_anomaly]`

| Key | Default | Purpose |
|---|---|---|
| `enabled` | `true` | Enable syscall anomaly detection over command output telemetry |
| `strict_mode` | `false` | Emit anomaly when denied syscalls are observed even if in baseline |
| `alert_on_unknown_syscall` | `true` | Alert on syscall names not present in baseline |
| `max_denied_events_per_minute` | `5` | Threshold for denied-syscall spike alerts |
| `max_total_events_per_minute` | `120` | Threshold for total syscall-event spike alerts |
| `max_alerts_per_minute` | `30` | Global alert budget guardrail per rolling minute |
| `alert_cooldown_secs` | `20` | Cooldown between identical anomaly alerts |
| `log_path` | `syscall-anomalies.log` | JSONL anomaly log path |
| `baseline_syscalls` | built-in allowlist | Expected syscall profile; unknown entries trigger alerts |

Notes:

- Detection consumes seccomp/audit hints from command `stdout`/`stderr`.
- Numeric syscall IDs in Linux audit lines are mapped to common x86_64 names when available.
- Alert budget and cooldown reduce duplicate/noisy events during repeated retries.
- `max_denied_events_per_minute` must be less than or equal to `max_total_events_per_minute`.

Example:

```toml
[security.syscall_anomaly]
enabled = true
strict_mode = false
alert_on_unknown_syscall = true
max_denied_events_per_minute = 5
max_total_events_per_minute = 120
max_alerts_per_minute = 30
alert_cooldown_secs = 20
log_path = "syscall-anomalies.log"
baseline_syscalls = ["read", "write", "openat", "close", "execve", "futex"]
```

## `[security.perplexity_filter]`

Lightweight, opt-in adversarial suffix filter that runs before provider calls in channel and gateway message pipelines.

| Key | Default | Purpose |
|---|---|---|
| `enable_perplexity_filter` | `false` | Enable pre-LLM statistical suffix anomaly blocking |
| `perplexity_threshold` | `18.0` | Character-class bigram perplexity threshold |
| `suffix_window_chars` | `64` | Trailing character window used for anomaly scoring |
| `min_prompt_chars` | `32` | Minimum prompt length before filter is evaluated |
| `symbol_ratio_threshold` | `0.20` | Minimum punctuation ratio in suffix window for blocking |

Notes:

- This filter is disabled by default to preserve baseline latency/behavior.
- The detector combines character-class perplexity with GCG-like token heuristics.
- Inputs are blocked only when anomaly conditions are met; normal natural-language prompts pass.
- Typical per-message overhead is designed to stay under `50ms` in debug-safe local tests and substantially lower in release builds.

Example:

```toml
[security.perplexity_filter]
enable_perplexity_filter = true
perplexity_threshold = 16.5
suffix_window_chars = 72
min_prompt_chars = 40
symbol_ratio_threshold = 0.25
```

## `[security.outbound_leak_guard]`

Controls outbound credential leak handling for channel replies after tool-output sanitization.

| Key | Default | Purpose |
|---|---|---|
| `enabled` | `true` | Enable outbound credential leak scanning on channel responses |
| `action` | `redact` | Leak handling mode: `redact` (mask and deliver) or `block` (do not deliver original content) |
| `sensitivity` | `0.7` | Leak detector sensitivity (`0.0` to `1.0`, higher is more aggressive) |

Notes:

- Detection uses the same leak detector used by existing redaction guardrails (API keys, JWTs, private keys, high-entropy tokens, etc.).
- `action = "redact"` preserves current behavior (safe-by-default compatibility).
- `action = "block"` is stricter and returns a safe fallback message instead of potentially sensitive content.
- When this guard is enabled, `/v1/chat/completions` streaming responses are safety-buffered and emitted after sanitization to avoid leaking raw token deltas before final scan.

Example:

```toml
[security.outbound_leak_guard]
enabled = true
action = "block"
sensitivity = 0.9
```

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

## `[research]`

Research phase allows the agent to gather information through tools before generating the main response.

| Key | Default | Purpose |
|---|---|---|
| `enabled` | `false` | Enable research phase |
| `trigger` | `never` | Research trigger strategy: `never`, `always`, `keywords`, `length`, `question` |
| `keywords` | `["find", "search", "check", "investigate"]` | Keywords that trigger research (when trigger = `keywords`) |
| `min_message_length` | `50` | Minimum message length to trigger research (when trigger = `length`) |
| `max_iterations` | `5` | Maximum tool calls during research phase |
| `show_progress` | `true` | Show research progress to user |

Notes:

- Research phase is **disabled by default** (`trigger = never`).
- When enabled, the agent first gathers facts through tools (grep, file_read, shell, memory search), then responds using the collected context.
- Research runs before the main agent turn and does not count toward `agent.max_tool_iterations`.
- Trigger strategies:
  - `never` — research disabled (default)
  - `always` — research on every user message
  - `keywords` — research when message contains any keyword from the list
  - `length` — research when message length exceeds `min_message_length`
  - `question` — research when message contains '?'

Example:

```toml
[research]
enabled = true
trigger = "keywords"
keywords = ["find", "show", "check", "how many"]
max_iterations = 3
show_progress = true
```

The agent will research the codebase before responding to queries like:
- "Find all TODO in src/"
- "Show contents of main.rs"
- "How many files in the project?"

## `[runtime]`

| Key | Default | Purpose |
|---|---|---|
| `kind` | `native` | Runtime backend: `native`, `docker`, or `wasm` |
| `reasoning_enabled` | unset (`None`) | Global reasoning/thinking override for providers that support explicit controls |

Notes:

- `reasoning_enabled = false` explicitly disables provider-side reasoning for supported providers (currently `ollama`, via request field `think: false`).
- `reasoning_enabled = true` explicitly requests reasoning for supported providers (`think: true` on `ollama`).
- Unset keeps provider defaults.
- Deprecated compatibility alias: `runtime.reasoning_level` is still accepted but should be migrated to `provider.reasoning_level`.
- `runtime.kind = "wasm"` enables capability-bounded module execution and disables shell/process style execution.

### `[runtime.wasm]`

| Key | Default | Purpose |
|---|---|---|
| `tools_dir` | `"tools/wasm"` | Workspace-relative directory containing `.wasm` modules |
| `fuel_limit` | `1000000` | Instruction budget per module invocation |
| `memory_limit_mb` | `64` | Per-module memory cap (MB) |
| `max_module_size_mb` | `50` | Maximum allowed `.wasm` file size (MB) |
| `allow_workspace_read` | `false` | Allow WASM host calls to read workspace files (future-facing) |
| `allow_workspace_write` | `false` | Allow WASM host calls to write workspace files (future-facing) |
| `allowed_hosts` | `[]` | Explicit network host allowlist for WASM host calls (future-facing) |

Notes:

- `allowed_hosts` entries must be normalized `host` or `host:port` strings; wildcards, schemes, and paths are rejected when `runtime.wasm.security.strict_host_validation = true`.
- Invocation-time capability overrides are controlled by `runtime.wasm.security.capability_escalation_mode`:
  - `deny` (default): reject escalation above runtime baseline.
  - `clamp`: reduce requested capabilities to baseline.

### `[runtime.wasm.security]`

| Key | Default | Purpose |
|---|---|---|
| `require_workspace_relative_tools_dir` | `true` | Require `runtime.wasm.tools_dir` to be workspace-relative and reject `..` traversal |
| `reject_symlink_modules` | `true` | Block symlinked `.wasm` module files during execution |
| `reject_symlink_tools_dir` | `true` | Block execution when `runtime.wasm.tools_dir` is itself a symlink |
| `strict_host_validation` | `true` | Fail config/invocation on invalid host entries instead of dropping them |
| `capability_escalation_mode` | `"deny"` | Escalation policy: `deny` or `clamp` |
| `module_hash_policy` | `"warn"` | Module integrity policy: `disabled`, `warn`, or `enforce` |
| `module_sha256` | `{}` | Optional map of module names to pinned SHA-256 digests |

Notes:

- `module_sha256` keys must match module names (without `.wasm`) and use `[A-Za-z0-9_-]` only.
- `module_sha256` values must be 64-character hexadecimal SHA-256 strings.
- `module_hash_policy = "warn"` allows execution but logs missing/mismatched digests.
- `module_hash_policy = "enforce"` blocks execution on missing/mismatched digests and requires at least one pin.

WASM profile templates:

- `dev/config.wasm.dev.toml`
- `dev/config.wasm.staging.toml`
- `dev/config.wasm.prod.toml`

## `[provider]`

| Key | Default | Purpose |
|---|---|---|
| `reasoning_level` | unset (`None`) | Reasoning effort/level override for providers that support explicit levels (currently OpenAI Codex `/responses`) |
| `transport` | unset (`None`) | Provider transport override (`auto`, `websocket`, `sse`) |

Notes:

- Supported values: `minimal`, `low`, `medium`, `high`, `xhigh` (case-insensitive).
- When set, overrides `ZEROCLAW_CODEX_REASONING_EFFORT` for OpenAI Codex requests.
- Unset falls back to `ZEROCLAW_CODEX_REASONING_EFFORT` if present, otherwise defaults to `xhigh`.
- If both `provider.reasoning_level` and deprecated `runtime.reasoning_level` are set, provider-level value wins.
- `provider.transport` is normalized case-insensitively (`ws` aliases to `websocket`; `http` aliases to `sse`).
- For OpenAI Codex, default transport mode is `auto` (WebSocket-first with SSE fallback).
- Transport override precedence for OpenAI Codex:
  1. `[[model_routes]].transport` (route-specific)
  2. `PROVIDER_TRANSPORT` / `ZEROCLAW_PROVIDER_TRANSPORT` / `ZEROCLAW_CODEX_TRANSPORT`
  3. `provider.transport`
  4. legacy `ZEROCLAW_RESPONSES_WEBSOCKET` (boolean)
- Environment overrides replace configured `provider.transport` when set.

## `[skills]`

| Key | Default | Purpose |
|---|---|---|
| `open_skills_enabled` | `false` | Opt-in loading/sync of community `open-skills` repository |
| `open_skills_dir` | unset | Optional local path for `open-skills` (defaults to `$HOME/open-skills` when enabled) |
| `trusted_skill_roots` | `[]` | Allowlist of directory roots for symlink targets in `workspace/skills/*` |
| `prompt_injection_mode` | `full` | Skill prompt verbosity: `full` (inline instructions/tools) or `compact` (name/description/location only) |
| `clawhub_token` | unset | Optional Bearer token for authenticated ClawhHub skill downloads |

Notes:

- Security-first default: ZeroClaw does **not** clone or sync `open-skills` unless `open_skills_enabled = true`.
- Environment overrides:
  - `ZEROCLAW_OPEN_SKILLS_ENABLED` accepts `1/0`, `true/false`, `yes/no`, `on/off`.
  - `ZEROCLAW_OPEN_SKILLS_DIR` overrides the repository path when non-empty.
  - `ZEROCLAW_SKILLS_PROMPT_MODE` accepts `full` or `compact`.
- Precedence for enable flag: `ZEROCLAW_OPEN_SKILLS_ENABLED` → `skills.open_skills_enabled` in `config.toml` → default `false`.
- `prompt_injection_mode = "compact"` is recommended on low-context local models to reduce startup prompt size while keeping skill files available on demand.
- Symlinked workspace skills are blocked by default. Set `trusted_skill_roots` to allow local shared-skill directories after explicit trust review.
- `zeroclaw skills install` and `zeroclaw skills audit` apply a static security audit. Skills that contain script-like files, high-risk shell payload snippets, or unsafe markdown link traversal are rejected.
- `clawhub_token` is sent as `Authorization: Bearer <token>` when downloading from ClawhHub. Obtain a token from [https://clawhub.ai](https://clawhub.ai) after signing in. Required if the API returns 429 (rate-limited) or 401 (unauthorized) for anonymous requests.

**ClawhHub token example:**

```toml
[skills]
clawhub_token = "your-token-here"
```

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
- In `proxy.scope = "services"` mode, remote image fetch uses service-key routing. For best compatibility include relevant selectors/keys such as:
  - `channel.qq` (QQ media hosts like `multimedia.nt.qq.com.cn`)
  - `tool.multimodal` (dedicated multimodal fetch path)
  - `tool.http_request` (compatibility fallback path)
  - `provider.*` or the active provider key (for example `provider.openai`)

## `[browser]`

| Key | Default | Purpose |
|---|---|---|
| `enabled` | `false` | Enable browser tools (`browser_open` and `browser`) |
| `allowed_domains` | `[]` | Allowed domains for `browser_open` and `browser` (exact/subdomain match, or `"*"` for all public domains) |
| `browser_open` | `default` | Browser used by `browser_open`: `disable`, `brave`, `chrome`, `firefox`, `edge` (`msedge` alias), `default` |
| `session_name` | unset | Browser session name (for agent-browser automation) |
| `backend` | `agent_browser` | Browser automation backend: `"agent_browser"`, `"rust_native"`, `"computer_use"`, or `"auto"` |
| `auto_backend_priority` | `[]` | Priority order for `backend = "auto"` (for example `["agent_browser","rust_native","computer_use"]`) |
| `agent_browser_command` | `agent-browser` | Executable/path for agent-browser CLI |
| `agent_browser_extra_args` | `[]` | Extra args prepended to each agent-browser command |
| `agent_browser_timeout_ms` | `30000` | Timeout per agent-browser action command |
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

- `browser_open` is a simple URL opener; `browser` is full browser automation (open/click/type/scroll/screenshot).
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
| `user_agent` | `ZeroClaw/1.0` | User-Agent header for outbound HTTP requests |
| `credential_profiles` | `{}` | Optional named env-backed auth profiles used by tool arg `credential_profile` |

Notes:

- Deny-by-default: if `allowed_domains` is empty, all HTTP requests are rejected.
- Use exact domain or subdomain matching (e.g. `"api.example.com"`, `"example.com"`), or `"*"` to allow any public domain.
- Local/private targets are still blocked even when `"*"` is configured.
- Shell `curl`/`wget` are classified as high-risk and may be blocked by autonomy policy. Prefer `http_request` for direct HTTP calls.
- `credential_profiles` lets the harness inject auth headers from environment variables, so agents can call authenticated APIs without raw tokens in tool arguments.

Example:

```toml
[http_request]
enabled = true
allowed_domains = ["api.github.com", "api.linear.app"]

[http_request.credential_profiles.github]
header_name = "Authorization"
env_var = "GITHUB_TOKEN"
value_prefix = "Bearer "

[http_request.credential_profiles.linear]
header_name = "Authorization"
env_var = "LINEAR_API_KEY"
value_prefix = ""
```

Then call `http_request` with:

```json
{
  "url": "https://api.github.com/user",
  "credential_profile": "github"
}
```

When using `credential_profile`, do not also set the same header key in `args.headers` (case-insensitive), or the request will be rejected as a header conflict.

## `[web_fetch]`

| Key | Default | Purpose |
|---|---|---|
| `enabled` | `false` | Enable `web_fetch` for page-to-text extraction |
| `provider` | `fast_html2md` | Fetch/render backend: `fast_html2md`, `nanohtml2text`, `firecrawl`, `tavily` |
| `api_key` | unset | API key for provider backends that require it (e.g. `firecrawl`, `tavily`) |
| `api_url` | unset | Optional API URL override (self-hosted/alternate endpoint) |
| `allowed_domains` | `["*"]` | Domain allowlist (`"*"` allows all public domains) |
| `blocked_domains` | `[]` | Denylist applied before allowlist |
| `max_response_size` | `500000` | Maximum returned payload size in bytes |
| `timeout_secs` | `30` | Request timeout in seconds |
| `user_agent` | `ZeroClaw/1.0` | User-Agent header for fetch requests |

Notes:

- `web_fetch` is optimized for summarization/data extraction from web pages.
- Redirect targets are revalidated against allow/deny domain policy.
- Local/private network targets remain blocked even when `allowed_domains = ["*"]`.

## `[web_search]`

| Key | Default | Purpose |
|---|---|---|
| `enabled` | `false` | Enable `web_search_tool` |
| `provider` | `duckduckgo` | Search backend: `duckduckgo` (`ddg` alias), `brave`, `firecrawl`, `tavily`, `perplexity`, `exa`, `jina` |
| `fallback_providers` | `[]` | Fallback providers tried in order after primary failure |
| `retries_per_provider` | `0` | Retry count before switching to next provider |
| `retry_backoff_ms` | `250` | Delay between retry attempts (milliseconds) |
| `api_key` | unset | Generic provider key (used by `firecrawl`/`tavily`, fallback for dedicated provider keys) |
| `api_url` | unset | Optional API URL override |
| `brave_api_key` | unset | Dedicated Brave key (required for `provider = "brave"` unless `api_key` is set) |
| `perplexity_api_key` | unset | Dedicated Perplexity key |
| `exa_api_key` | unset | Dedicated Exa key |
| `jina_api_key` | unset | Optional Jina key |
| `domain_filter` | `[]` | Optional domain filter forwarded to supported providers |
| `language_filter` | `[]` | Optional language filter forwarded to supported providers |
| `country` | unset | Optional country hint for supported providers |
| `recency_filter` | unset | Optional recency filter for supported providers |
| `max_tokens` | unset | Optional token budget for providers that support it (for example Perplexity) |
| `max_tokens_per_page` | unset | Optional per-page token budget for supported providers |
| `exa_search_type` | `auto` | Exa search mode: `auto`, `keyword`, `neural` |
| `exa_include_text` | `false` | Include text payloads in Exa responses |
| `jina_site_filters` | `[]` | Optional site filters for Jina search |
| `max_results` | `5` | Maximum search results returned (must be 1-10) |
| `timeout_secs` | `15` | Request timeout in seconds |
| `user_agent` | `ZeroClaw/1.0` | User-Agent header for search requests |

Notes:

- If DuckDuckGo returns `403`/`429` in your network, switch provider to `brave`, `perplexity`, `exa`, or configure `fallback_providers`.
- `web_search` finds candidate URLs; pair it with `web_fetch` for page content extraction.
- Agents can modify these settings at runtime via the `web_search_config` tool (`action=get|set|list_providers`).
- In supervised mode, `web_search_config` mutations still require normal tool approval unless explicitly auto-approved.
- Invalid provider names, `exa_search_type`, and out-of-range retry/result/timeout values are rejected during config validation.

Recommended resilient profile:

```toml
[web_search]
enabled = true
provider = "perplexity"
fallback_providers = ["exa", "jina", "duckduckgo"]
retries_per_provider = 1
retry_backoff_ms = 300
max_results = 5
timeout_secs = 20
```

Runtime workflow (`web_search_config`):

1. Inspect available providers and current config snapshot:

```json
{"action":"list_providers"}
```

```json
{"action":"get"}
```

2. Set a primary provider with fallback chain:

```json
{"action":"set","provider":"perplexity","fallback_providers":["exa","jina","duckduckgo"]}
```

3. Tune provider-specific options:

```json
{"action":"set","exa_search_type":"neural","exa_include_text":true}
```

```json
{"action":"set","jina_site_filters":["docs.rs","github.com"]}
```

4. Add geo/language/recency filters for region-aware queries:

```json
{"action":"set","country":"US","language_filter":["en"],"recency_filter":"week"}
```

Environment overrides:

- `ZEROCLAW_WEB_SEARCH_ENABLED` / `WEB_SEARCH_ENABLED`
- `ZEROCLAW_WEB_SEARCH_PROVIDER` / `WEB_SEARCH_PROVIDER`
- `ZEROCLAW_WEB_SEARCH_MAX_RESULTS` / `WEB_SEARCH_MAX_RESULTS`
- `ZEROCLAW_WEB_SEARCH_TIMEOUT_SECS` / `WEB_SEARCH_TIMEOUT_SECS`
- `ZEROCLAW_WEB_SEARCH_FALLBACK_PROVIDERS` / `WEB_SEARCH_FALLBACK_PROVIDERS` (comma-separated)
- `ZEROCLAW_WEB_SEARCH_RETRIES_PER_PROVIDER` / `WEB_SEARCH_RETRIES_PER_PROVIDER`
- `ZEROCLAW_WEB_SEARCH_RETRY_BACKOFF_MS` / `WEB_SEARCH_RETRY_BACKOFF_MS`
- `ZEROCLAW_WEB_SEARCH_DOMAIN_FILTER` / `WEB_SEARCH_DOMAIN_FILTER` (comma-separated)
- `ZEROCLAW_WEB_SEARCH_LANGUAGE_FILTER` / `WEB_SEARCH_LANGUAGE_FILTER` (comma-separated)
- `ZEROCLAW_WEB_SEARCH_COUNTRY` / `WEB_SEARCH_COUNTRY`
- `ZEROCLAW_WEB_SEARCH_RECENCY_FILTER` / `WEB_SEARCH_RECENCY_FILTER`
- `ZEROCLAW_WEB_SEARCH_MAX_TOKENS` / `WEB_SEARCH_MAX_TOKENS`
- `ZEROCLAW_WEB_SEARCH_MAX_TOKENS_PER_PAGE` / `WEB_SEARCH_MAX_TOKENS_PER_PAGE`
- `ZEROCLAW_WEB_SEARCH_EXA_SEARCH_TYPE` / `WEB_SEARCH_EXA_SEARCH_TYPE`
- `ZEROCLAW_WEB_SEARCH_EXA_INCLUDE_TEXT` / `WEB_SEARCH_EXA_INCLUDE_TEXT`
- `ZEROCLAW_WEB_SEARCH_JINA_SITE_FILTERS` / `WEB_SEARCH_JINA_SITE_FILTERS` (comma-separated)
- `ZEROCLAW_BRAVE_API_KEY` / `BRAVE_API_KEY`
- `ZEROCLAW_PERPLEXITY_API_KEY` / `PERPLEXITY_API_KEY`
- `ZEROCLAW_EXA_API_KEY` / `EXA_API_KEY`
- `ZEROCLAW_JINA_API_KEY` / `JINA_API_KEY`

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
| `command_context_rules` | `[]` | per-command context-aware allow/deny rules (domain/path constraints, optional high-risk override) |
| `forbidden_paths` | built-in protected list | explicit path denylist (system paths + sensitive dotdirs by default) |
| `allowed_roots` | `[]` | additional roots allowed outside workspace after canonicalization |
| `max_actions_per_hour` | `20` | per-policy action budget |
| `max_cost_per_day_cents` | `500` | per-policy spend guardrail |
| `require_approval_for_medium_risk` | `true` | approval gate for medium-risk commands |
| `block_high_risk_commands` | `true` | hard block for high-risk commands |
| `allow_sensitive_file_reads` | `false` | allow `file_read` on sensitive files/dirs (for example `.env`, `.aws/credentials`, private keys) |
| `allow_sensitive_file_writes` | `false` | allow `file_write`/`file_edit` on sensitive files/dirs (for example `.env`, `.aws/credentials`, private keys) |
| `auto_approve` | `[]` | tool operations always auto-approved |
| `always_ask` | `[]` | tool operations that always require approval |
| `non_cli_excluded_tools` | built-in denylist (includes `shell`, `process`, `file_write`, ...) | tools hidden from non-CLI channel tool specs |
| `non_cli_approval_approvers` | `[]` | optional allowlist for who can run non-CLI approval-management commands |
| `non_cli_natural_language_approval_mode` | `direct` | natural-language behavior for approval-management commands (`direct`, `request_confirm`, `disabled`) |
| `non_cli_natural_language_approval_mode_by_channel` | `{}` | per-channel override map for natural-language approval mode |

Notes:

- `level = "full"` skips medium-risk approval gating for shell execution, while still enforcing configured guardrails.
- Access outside the workspace requires `allowed_roots`, even when `workspace_only = false`.
- `allowed_roots` supports absolute paths, `~/...`, and workspace-relative paths.
- `allowed_commands` entries can be command names (for example, `"git"`), explicit executable paths (for example, `"/usr/bin/antigravity"`), or `"*"` to allow any command name/path (risk gates still apply).
- `command_context_rules` can narrow or override `allowed_commands` for matching commands:
  - `action = "allow"` rules are restrictive when present for a command: at least one allow rule must match.
  - `action = "deny"` rules explicitly block matching contexts.
  - `allow_high_risk = true` allows a matching high-risk command to pass the hard block, but supervised mode still requires `approved=true`.
- `file_read` blocks sensitive secret-bearing files/directories by default. Set `allow_sensitive_file_reads = true` only for controlled debugging sessions.
- `file_write` and `file_edit` block sensitive secret-bearing files/directories by default. Set `allow_sensitive_file_writes = true` only for controlled break-glass sessions.
- `file_read`, `file_write`, and `file_edit` refuse multiply-linked files (hard-link guard) to reduce workspace path bypass risk via hard-link escapes.
- Shell separator/operator parsing is quote-aware. Characters like `;` inside quoted arguments are treated as literals, not command separators.
- Unquoted shell chaining/operators are still enforced by policy checks (`;`, `|`, `&&`, `||`, background chaining, and redirects).
- In supervised mode on non-CLI channels, operators can persist human-approved tools with:
  - One-step flow: `/approve <tool>`.
  - Two-step flow: `/approve-request <tool>` then `/approve-confirm <request-id>` (same sender + same chat/channel).
  Both paths write to `autonomy.auto_approve` and remove the tool from `autonomy.always_ask`.
- For pending runtime execution prompts (including Telegram inline approval buttons), use:
  - `/approve-allow <request-id>` to approve only the current pending request.
  - `/approve-deny <request-id>` to reject the current pending request.
  This path does not modify `autonomy.auto_approve` or `autonomy.always_ask`.
- `non_cli_natural_language_approval_mode` controls how strict natural-language approval intents are:
  - `direct` (default): natural-language approval grants immediately (private-chat friendly).
  - `request_confirm`: natural-language approval creates a pending request that needs explicit confirm.
  - `disabled`: natural-language approval commands are rejected; use slash commands only.
- `non_cli_natural_language_approval_mode_by_channel` can override that mode for specific channels (keys are channel names like `telegram`, `discord`, `slack`).
  - Example: keep global `direct`, but force `discord = "request_confirm"` for team chats.
- `non_cli_approval_approvers` can restrict who is allowed to run approval commands (`/approve*`, `/unapprove`, `/approvals`):
  - `*` allows all channel-admitted senders.
  - `alice` allows sender `alice` on any channel.
  - `telegram:alice` allows only that channel+sender pair.
  - `telegram:*` allows any sender on Telegram.
  - `*:alice` allows `alice` on any channel.
- By default, `process` is excluded on non-CLI channels alongside `shell`. To opt in intentionally, remove `"process"` from `[autonomy].non_cli_excluded_tools` in `config.toml`.
- Use `/unapprove <tool>` to remove persisted approval from `autonomy.auto_approve`.
- `/approve-pending` lists pending requests for the current sender+chat/channel scope.
- If a tool remains unavailable after approval, check `autonomy.non_cli_excluded_tools` (runtime `/approvals` shows this list). Channel runtime reloads this list from `config.toml` automatically.

```toml
[autonomy]
workspace_only = false
forbidden_paths = ["/etc", "/root", "/proc", "/sys", "~/.ssh", "~/.gnupg", "~/.aws"]
allowed_roots = ["~/Desktop/projects", "/opt/shared-repo"]

[[autonomy.command_context_rules]]
command = "curl"
action = "allow"
allowed_domains = ["api.github.com", "*.example.internal"]
allow_high_risk = true

[[autonomy.command_context_rules]]
command = "rm"
action = "allow"
allowed_path_prefixes = ["/tmp"]
allow_high_risk = true
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
- Observation memory is available via tool `memory_observe`, which stores entries under category `observation` by default (override with `category` when needed).

Example (tool-call payload):

```json
{
  "observation": "User asks for brief release notes when CI is green.",
  "source": "chat",
  "confidence": 0.9
}
```

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
| `transport` | unset | Optional per-route transport override (`auto`, `websocket`, `sse`) |

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
- Telegram/Discord/Slack/Mattermost/Lark/Feishu support `[channels_config.<channel>.group_reply]`:
  - `mode = "all_messages"` or `mode = "mention_only"`
  - `allowed_sender_ids = ["..."]` to bypass mention gating in groups
  - `allowed_users` allowlist checks still run first
- Telegram/Discord/Lark/Feishu ACK emoji reactions are configurable under
  `[channels_config.ack_reaction.<channel>]` with switchable enable state,
  custom emoji pools, and conditional rules.
- Legacy `mention_only` flags (Telegram/Discord/Mattermost/Lark) remain supported as fallback only.
  If `group_reply.mode` is set, it takes precedence over legacy `mention_only`.
- While `zeroclaw channel start` is running, updates to `default_provider`, `default_model`, `default_temperature`, `api_key`, `api_url`, and `reliability.*` are hot-applied from `config.toml` on the next inbound message.

### `[channels_config.ack_reaction.<channel>]`

Per-channel ACK reaction policy (`<channel>`: `telegram`, `discord`, `lark`, `feishu`).

| Key | Default | Purpose |
|---|---|---|
| `enabled` | `true` | Master switch for ACK reactions on this channel |
| `strategy` | `random` | Pool selection strategy: `random` or `first` |
| `sample_rate` | `1.0` | Probabilistic gate in `[0.0, 1.0]` for channel fallback ACKs |
| `emojis` | `[]` | Channel-level custom fallback pool (uses built-in pool when empty) |
| `rules` | `[]` | Ordered conditional rules; first matching rule can react or suppress |

Rule object fields (`[[channels_config.ack_reaction.<channel>.rules]]`):

| Key | Default | Purpose |
|---|---|---|
| `enabled` | `true` | Enable/disable this single rule |
| `contains_any` | `[]` | Match when message contains any keyword (case-insensitive) |
| `contains_all` | `[]` | Match when message contains all keywords (case-insensitive) |
| `contains_none` | `[]` | Match only when message contains none of these keywords |
| `regex_any` | `[]` | Match when any regex pattern matches |
| `regex_all` | `[]` | Match only when all regex patterns match |
| `regex_none` | `[]` | Match only when none of these regex patterns match |
| `sender_ids` | `[]` | Match only these sender IDs (`"*"` matches all) |
| `chat_ids` | `[]` | Match only these chat/channel IDs (`"*"` matches all) |
| `chat_types` | `[]` | Restrict to `group` and/or `direct` |
| `locale_any` | `[]` | Restrict by locale tag (prefix supported, e.g. `zh`) |
| `action` | `react` | `react` to emit ACK, `suppress` to force no ACK when matched |
| `sample_rate` | unset | Optional rule-level gate in `[0.0, 1.0]` (overrides channel `sample_rate`) |
| `strategy` | unset | Optional per-rule strategy override |
| `emojis` | `[]` | Emoji pool used when this rule matches |

Example:

```toml
[channels_config.ack_reaction.telegram]
enabled = true
strategy = "random"
sample_rate = 1.0
emojis = ["✅", "👌", "🔥"]

[[channels_config.ack_reaction.telegram.rules]]
contains_any = ["deploy", "release"]
contains_none = ["dry-run"]
regex_none = ["panic|fatal"]
chat_ids = ["-100200300"]
chat_types = ["group"]
strategy = "first"
sample_rate = 0.9
emojis = ["🚀"]

[[channels_config.ack_reaction.telegram.rules]]
contains_any = ["error", "failed"]
action = "suppress"
sample_rate = 1.0
```

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

## `[agents_ipc]`

Inter-process communication for independent ZeroClaw agents on the same host.

| Key | Default | Purpose |
|---|---|---|
| `enabled` | `false` | Enable IPC tools (`agents_list`, `agents_send`, `agents_inbox`, `state_get`, `state_set`) |
| `db_path` | `~/.zeroclaw/agents.db` | Shared SQLite database path (all agents on this host share one file) |
| `staleness_secs` | `300` | Agents not seen within this window are considered offline (seconds) |

Notes:

- When `enabled = false` (default), no IPC tools are registered and no database is created.
- All agents that share a `db_path` can discover each other and exchange messages.
- Agent identity is derived from `workspace_dir` (SHA-256 hash), not user-supplied.

Example:

```toml
[agents_ipc]
enabled = true
db_path = "~/.zeroclaw/agents.db"
staleness_secs = 300
```

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
