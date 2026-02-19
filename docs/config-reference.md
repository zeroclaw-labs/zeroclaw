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
| `max_tool_iterations` | `10` | Maximum tool-call loop turns per user message across CLI, gateway, and channels |

Notes:

- Setting `max_tool_iterations = 0` falls back to safe default `10`.
- If a channel message exceeds this value, the runtime returns: `Agent exceeded maximum tool iterations (<value>)`.

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
| `auto_save` | `true` | automatic persistence |
| `embedding_provider` | `none` | `none`, `openai`, or custom endpoint |
| `embedding_model` | `text-embedding-3-small` | embedding model ID, or `hint:<name>` route |
| `embedding_dimensions` | `1536` | expected vector size for selected embedding model |
| `vector_weight` | `0.7` | hybrid ranking vector weight |
| `keyword_weight` | `0.3` | hybrid ranking keyword weight |

## `[[model_routes]]` and `[[embedding_routes]]`

Use route hints so integrations can keep stable names while model IDs evolve.

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

See detailed channel matrix and allowlist behavior in [channels-reference.md](channels-reference.md).

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
