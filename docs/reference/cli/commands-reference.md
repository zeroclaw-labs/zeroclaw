# QuantClaw Commands Reference

This reference is derived from the current CLI surface (`quantclaw --help`).

Last verified: **March 26, 2026**.

## Top-Level Commands

| Command | Purpose |
|---|---|
| `onboard` | Initialize workspace/config quickly or interactively |
| `agent` | Run interactive chat or single-message mode |
| `gateway` | Start webhook and WhatsApp HTTP gateway |
| `acp` | Start ACP (Agent Control Protocol) server over stdio |
| `daemon` | Start supervised runtime (gateway + channels + optional heartbeat/scheduler) |
| `service` | Manage user-level OS service lifecycle |
| `doctor` | Run diagnostics and freshness checks |
| `status` | Print current configuration and system summary |
| `estop` | Engage/resume emergency stop levels and inspect estop state |
| `cron` | Manage scheduled tasks |
| `models` | Refresh provider model catalogs |
| `providers` | List provider IDs, aliases, and active provider |
| `channel` | Manage channels and channel health checks |
| `integrations` | Inspect integration details |
| `skills` | List/install/remove skills |
| `migrate` | Import from external runtimes (currently OpenClaw) |
| `config` | Manage configuration (view/set properties, export schema) |
| `completions` | Generate shell completion scripts to stdout |
| `hardware` | Discover and introspect USB hardware |
| `peripheral` | Configure and flash peripherals |

## Command Groups

### `onboard`

- `quantclaw onboard`
- `quantclaw onboard --channels-only`
- `quantclaw onboard --force`
- `quantclaw onboard --reinit`
- `quantclaw onboard --api-key <KEY> --provider <ID> --memory <sqlite|lucid|markdown|none>`
- `quantclaw onboard --api-key <KEY> --provider <ID> --model <MODEL_ID> --memory <sqlite|lucid|markdown|none>`
- `quantclaw onboard --api-key <KEY> --provider <ID> --model <MODEL_ID> --memory <sqlite|lucid|markdown|none> --force`

`onboard` safety behavior:

- If `config.toml` already exists, onboarding offers two modes:
  - Full onboarding (overwrite `config.toml`)
  - Provider-only update (update provider/model/API key while preserving existing channels, tunnel, memory, hooks, and other settings)
- In non-interactive environments, existing `config.toml` causes a safe refusal unless `--force` is passed.
- Use `quantclaw onboard --channels-only` when you only need to rotate channel tokens/allowlists.
- Use `quantclaw onboard --reinit` to start fresh. This backs up your existing config directory with a timestamp suffix and creates a new configuration from scratch.

### `agent`

- `quantclaw agent`
- `quantclaw agent -m "Hello"`
- `quantclaw agent --provider <ID> --model <MODEL> --temperature <0.0-2.0>`
- `quantclaw agent --peripheral <board:path>`

Tip:

- In interactive chat, you can ask for route changes in natural language (for example “conversation uses kimi, coding uses gpt-5.3-codex”); the assistant can persist this via tool `model_routing_config`.

### `acp`

- `quantclaw acp`
- `quantclaw acp --max-sessions <N>`
- `quantclaw acp --session-timeout <SECONDS>`

Start the ACP (Agent Control Protocol) server for IDE and tool integration.

- Uses JSON-RPC 2.0 over stdin/stdout
- Supports methods: `initialize`, `session/new`, `session/prompt`, `session/stop`
- Streams agent reasoning, tool calls, and content in real-time as notifications
- Default max sessions: 10
- Default session timeout: 3600 seconds (1 hour)

### `gateway` / `daemon`

- `quantclaw gateway [--host <HOST>] [--port <PORT>]`
- `quantclaw daemon [--host <HOST>] [--port <PORT>]`

### `estop`

- `quantclaw estop` (engage `kill-all`)
- `quantclaw estop --level network-kill`
- `quantclaw estop --level domain-block --domain "*.chase.com" [--domain "*.paypal.com"]`
- `quantclaw estop --level tool-freeze --tool shell [--tool browser]`
- `quantclaw estop status`
- `quantclaw estop resume`
- `quantclaw estop resume --network`
- `quantclaw estop resume --domain "*.chase.com"`
- `quantclaw estop resume --tool shell`
- `quantclaw estop resume --otp <123456>`

Notes:

- `estop` commands require `[security.estop].enabled = true`.
- When `[security.estop].require_otp_to_resume = true`, `resume` requires OTP validation.
- OTP prompt appears automatically if `--otp` is omitted.

### `service`

- `quantclaw service install`
- `quantclaw service start`
- `quantclaw service stop`
- `quantclaw service restart`
- `quantclaw service status`
- `quantclaw service uninstall`

### `cron`

- `quantclaw cron list`
- `quantclaw cron add <expr> [--tz <IANA_TZ>] <command>`
- `quantclaw cron add-at <rfc3339_timestamp> <command>`
- `quantclaw cron add-every <every_ms> <command>`
- `quantclaw cron once <delay> <command>`
- `quantclaw cron remove <id>`
- `quantclaw cron pause <id>`
- `quantclaw cron resume <id>`

Notes:

- Mutating schedule/cron actions require `cron.enabled = true`.
- Shell command payloads for schedule creation (`create` / `add` / `once`) are validated by security command policy before job persistence.

### `models`

- `quantclaw models refresh`
- `quantclaw models refresh --provider <ID>`
- `quantclaw models refresh --force`

`models refresh` currently supports live catalog refresh for provider IDs: `openrouter`, `openai`, `anthropic`, `groq`, `mistral`, `deepseek`, `xai`, `together-ai`, `gemini`, `ollama`, `llamacpp`, `sglang`, `vllm`, `astrai`, `venice`, `fireworks`, `cohere`, `moonshot`, `glm`, `zai`, `qwen`, and `nvidia`.

### `doctor`

- `quantclaw doctor`
- `quantclaw doctor models [--provider <ID>] [--use-cache]`
- `quantclaw doctor traces [--limit <N>] [--event <TYPE>] [--contains <TEXT>]`
- `quantclaw doctor traces --id <TRACE_ID>`

`doctor traces` reads runtime tool/model diagnostics from `observability.runtime_trace_path`.

### `channel`

- `quantclaw channel list`
- `quantclaw channel start`
- `quantclaw channel doctor`
- `quantclaw channel bind-telegram <IDENTITY>`
- `quantclaw channel add <type> <json>`
- `quantclaw channel remove <name>`

Runtime in-chat commands (Telegram/Discord while channel server is running):

- `/models`
- `/models <provider>`
- `/model`
- `/model <model-id>`
- `/new`

Channel runtime also watches `config.toml` and hot-applies updates to:
- `default_provider`
- `default_model`
- `default_temperature`
- `api_key` / `api_url` (for the default provider)
- `reliability.*` provider retry settings

`add/remove` currently route you back to managed setup/manual config paths (not full declarative mutators yet).

### `integrations`

- `quantclaw integrations info <name>`

### `skills`

- `quantclaw skills list`
- `quantclaw skills audit <source_or_name>`
- `quantclaw skills install <source>`
- `quantclaw skills remove <name>`

`<source>` accepts git remotes (`https://...`, `http://...`, `ssh://...`, and `git@host:owner/repo.git`) or a local filesystem path.

`skills install` always runs a built-in static security audit before the skill is accepted. The audit blocks:
- symlinks inside the skill package
- script-like files (`.sh`, `.bash`, `.zsh`, `.ps1`, `.bat`, `.cmd`)
- high-risk command snippets (for example pipe-to-shell payloads)
- markdown links that escape the skill root, point to remote markdown, or target script files

Use `skills audit` to manually validate a candidate skill directory (or an installed skill by name) before sharing it.

Skill manifests (`SKILL.toml`) support `prompts` and `[[tools]]`; both are injected into the agent system prompt at runtime, so the model can follow skill instructions without manually reading skill files.

### `migrate`

- `quantclaw migrate openclaw [--source <path>] [--dry-run]`

### `config`

- `quantclaw config list` — list all properties with current values
- `quantclaw config list --secrets` — list only secret (encrypted) fields
- `quantclaw config list --filter channels.matrix` — filter by path prefix
- `quantclaw config get <path>` — get a single property value (secrets show set/unset status)
- `quantclaw config set <path> <value>` — set a property value
- `quantclaw config set <path>` — secret fields prompt for masked input; enum fields offer interactive selection
- `quantclaw config set --no-interactive <path> <value>` — scripted mode, no prompts
- `quantclaw config init <section>` — create an unconfigured section with defaults (`enabled=false`)
- `quantclaw config init` — initialize all unconfigured sections
- `quantclaw config schema` — print JSON Schema (draft 2020-12) to stdout

Secret fields (API keys, tokens, passwords) are automatically detected via `#[secret]`
annotations. When setting a secret, input is masked regardless of whether a value is
provided on the command line.

Enum fields (e.g. `stream-mode`, `search-mode`) offer interactive selection via arrow
keys when the value is omitted. Provide the value directly to skip the prompt.

Shell tab-completion for property paths is included in `quantclaw completions <shell>`.

### `completions`

- `quantclaw completions bash`
- `quantclaw completions fish`
- `quantclaw completions zsh`
- `quantclaw completions powershell`
- `quantclaw completions elvish`

`completions` is stdout-only by design so scripts can be sourced directly without log/warning contamination.

### `hardware`

- `quantclaw hardware discover`
- `quantclaw hardware introspect <path>`
- `quantclaw hardware info [--chip <chip_name>]`

### `peripheral`

- `quantclaw peripheral list`
- `quantclaw peripheral add <board> <path>`
- `quantclaw peripheral flash [--port <serial_port>]`
- `quantclaw peripheral setup-uno-q [--host <ip_or_host>]`
- `quantclaw peripheral flash-nucleo`

### `props` (deprecated)

`quantclaw props` has been renamed to `quantclaw config`. Replace `props` with `config` in your commands.

#### Adding new config fields

Config structs derive `Configurable` with `#[prefix]` and `#[nested]` attributes.
Adding a new field to an existing struct makes it immediately available via `config`.
New enum types require a one-line `HasPropKind` impl. See `CONTRIBUTING.md` for details.

## Validation Tip

To verify docs against your current binary quickly:

```bash
quantclaw --help
quantclaw <command> --help
```
