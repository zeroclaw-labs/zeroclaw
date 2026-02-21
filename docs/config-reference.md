# ZeroClaw Config Reference (Operator-Oriented)

This is a high-signal reference for common config sections and defaults.

Last verified: **February 21, 2026**.

Config file path:

- `~/.zeroclaw/config.toml`

## Core Keys

| Key | Default | Notes |
|---|---|---|
| `default_provider` | `openrouter` | provider ID or alias |
| `default_model` | `anthropic/claude-sonnet-4-6` | model routed through selected provider |
| `default_temperature` | `0.7` | model temperature |
| `api_url` | unset | provider base URL override (e.g. remote Ollama endpoint, or OpenAI Codex OAuth proxy base URL) |

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
| `level` | `supervised` | autonomy posture (`read_only`, `supervised`, `full`) |
| `workspace_only` | `true` | deny filesystem operations outside workspace |
| `allow_non_cli_auto_approval` | `false` | when `true`, non-CLI channels can auto-approve approval-gated tool calls |
| `require_approval_for_medium_risk` | `true` | force approval on medium-risk shell/tool actions |
| `block_high_risk_commands` | `true` | hard-block high-risk actions |
| `max_actions_per_hour` | `20` | budget guardrail |
| `max_cost_per_day_cents` | `500` | daily model/tool budget guardrail |

Notes:

- Keep `allow_non_cli_auto_approval = false` unless you intentionally accept reduced human-in-the-loop guarantees.
- Equivalent CLI controls:
  - `zeroclaw security profile set strict --non-cli-approval manual`
  - `zeroclaw security profile set strict --non-cli-approval auto --yes-risk`

## `[memory]`

| Key | Default | Purpose |
|---|---|---|
| `backend` | `sqlite` | `sqlite`, `lucid`, `markdown`, `none` |
| `auto_save` | `true` | automatic persistence |
| `embedding_provider` | `none` | `none`, `openai`, or custom endpoint |
| `vector_weight` | `0.7` | hybrid ranking vector weight |
| `keyword_weight` | `0.3` | hybrid ranking keyword weight |

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
