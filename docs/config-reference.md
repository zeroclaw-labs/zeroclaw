# ZeroClaw Config Reference (Operator-Oriented)

This is a high-signal reference for common config sections and defaults.

Last verified: **February 19, 2026**.

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
| `compact_context` | `false` | When true: bootstrap_max_chars=6000, rag_chunk_limit=2. Use for 13B or smaller models |
| `max_tool_iterations` | `10` | Maximum tool-call loop turns per user message across CLI, gateway, and channels |
| `max_history_messages` | `50` | Maximum conversation history messages retained per session |
| `parallel_tools` | `false` | Enable parallel tool execution within a single iteration |
| `tool_dispatcher` | `auto` | Tool dispatch strategy |

Notes:

- Setting `max_tool_iterations = 0` falls back to safe default `10`.
- If a channel message exceeds this value, the runtime returns: `Agent exceeded maximum tool iterations (<value>)`.

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

```toml
[agents.researcher]
provider = "openrouter"
model = "anthropic/claude-sonnet-4-6"
system_prompt = "You are a research assistant."
max_depth = 2

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

## `[composio]`

| Key | Default | Purpose |
|---|---|---|
| `enabled` | `false` | Enable Composio managed OAuth tools |
| `api_key` | unset | Composio API key used by the `composio` tool |
| `entity_id` | `default` | Default `user_id` sent on connect/execute calls |

Notes:

- Backward compatibility: legacy `enable = true` is accepted as an alias for `enabled = true`.
- If `enabled = false` or `api_key` is missing, the `composio` tool is not registered.
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
| `enabled` | `false` | Enable `browser_open` tool (opens URLs without scraping) |
| `allowed_domains` | `[]` | Allowed domains for `browser_open` (exact or subdomain match) |
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
| `allowed_domains` | `[]` | Allowed domains for HTTP requests (exact or subdomain match) |
| `max_response_size` | `1000000` | Maximum response size in bytes (default: 1 MB) |
| `timeout_secs` | `30` | Request timeout in seconds |

Notes:

- Deny-by-default: if `allowed_domains` is empty, all HTTP requests are rejected.
- Use exact domain or subdomain matching (e.g. `"api.example.com"`, `"example.com"`).

## `[gateway]`

| Key | Default | Purpose |
|---|---|---|
| `host` | `127.0.0.1` | bind address |
| `port` | `3000` | gateway listen port |
| `require_pairing` | `true` | require pairing before bearer auth |
| `allow_public_bind` | `false` | block accidental public exposure |

## `[autonomy]`

| Key | Default | Purpose |
|---|---|---|
| `level` | `supervised` | `read_only`, `supervised`, or `full` |
| `workspace_only` | `true` | restrict writes/command paths to workspace scope |
| `allowed_commands` | _required for shell execution_ | allowlist of executable names |
| `forbidden_paths` | `[]` | explicit path denylist |
| `max_actions_per_hour` | `100` | per-policy action budget |
| `max_cost_per_day_cents` | `1000` | per-policy spend guardrail |
| `require_approval_for_medium_risk` | `true` | approval gate for medium-risk commands |
| `block_high_risk_commands` | `true` | hard block for high-risk commands |
| `auto_approve` | `[]` | tool operations always auto-approved |
| `always_ask` | `[]` | tool operations that always require approval |

Notes:

- `level = "full"` skips medium-risk approval gating for shell execution, while still enforcing configured guardrails.
- Shell separator/operator parsing is quote-aware. Characters like `;` inside quoted arguments are treated as literals, not command separators.
- Unquoted shell chaining/operators are still enforced by policy checks (`;`, `|`, `&&`, `||`, background chaining, and redirects).

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
| `message_timeout_secs` | `300` | Timeout in seconds for processing a single channel message (LLM + tools) |

Examples:

- `[channels_config.telegram]`
- `[channels_config.discord]`
- `[channels_config.whatsapp]`
- `[channels_config.email]`

Notes:

- Default `300s` is optimized for on-device LLMs (Ollama) which are slower than cloud APIs.
- If using cloud APIs (OpenAI, Anthropic, etc.), you can reduce this to `60` or lower.
- Values below `30` are clamped to `30` to avoid immediate timeout churn.
- When a timeout occurs, users receive: `⚠️ Request timed out while waiting for the model. Please try again.`
- Telegram-only interruption behavior is controlled with `channels_config.telegram.interrupt_on_new_message` (default `false`).
  When enabled, a newer message from the same sender in the same chat cancels the in-flight request and preserves interrupted user context.

See detailed channel matrix and allowlist behavior in [channels-reference.md](channels-reference.md).

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
```

## Related Docs

- [channels-reference.md](channels-reference.md)
- [providers-reference.md](providers-reference.md)
- [operations-runbook.md](operations-runbook.md)
- [troubleshooting.md](troubleshooting.md)
