# JhedaiClaw Commands Reference

This reference is derived from the current CLI surface (`jhedaiclaw --help`).

Last verified: **February 21, 2026**.

## Top-Level Commands

| Command        | Purpose                                                                      |
| -------------- | ---------------------------------------------------------------------------- |
| `onboard`      | Initialize workspace/config quickly or interactively                         |
| `agent`        | Run interactive chat or single-message mode                                  |
| `gateway`      | Start webhook and WhatsApp HTTP gateway                                      |
| `daemon`       | Start supervised runtime (gateway + channels + optional heartbeat/scheduler) |
| `service`      | Manage user-level OS service lifecycle                                       |
| `doctor`       | Run diagnostics and freshness checks                                         |
| `status`       | Print current configuration and system summary                               |
| `estop`        | Engage/resume emergency stop levels and inspect estop state                  |
| `cron`         | Manage scheduled tasks                                                       |
| `models`       | Refresh provider model catalogs                                              |
| `providers`    | List provider IDs, aliases, and active provider                              |
| `channel`      | Manage channels and channel health checks                                    |
| `integrations` | Inspect integration details                                                  |
| `skills`       | List/install/remove skills                                                   |
| `migrate`      | Import from external runtimes (currently OpenClaw)                           |
| `config`       | Export machine-readable config schema                                        |
| `completions`  | Generate shell completion scripts to stdout                                  |
| `hardware`     | Discover and introspect USB hardware                                         |
| `peripheral`   | Configure and flash peripherals                                              |

## Command Groups

### `onboard`

- `jhedaiclaw onboard`
- `jhedaiclaw onboard --channels-only`
- `jhedaiclaw onboard --force`
- `jhedaiclaw onboard --reinit`
- `jhedaiclaw onboard --api-key <KEY> --provider <ID> --memory <sqlite|lucid|markdown|none>`
- `jhedaiclaw onboard --api-key <KEY> --provider <ID> --model <MODEL_ID> --memory <sqlite|lucid|markdown|none>`
- `jhedaiclaw onboard --api-key <KEY> --provider <ID> --model <MODEL_ID> --memory <sqlite|lucid|markdown|none> --force`

`onboard` safety behavior:

- If `config.toml` already exists, onboarding offers two modes:
  - Full onboarding (overwrite `config.toml`)
  - Provider-only update (update provider/model/API key while preserving existing channels, tunnel, memory, hooks, and other settings)
- In non-interactive environments, existing `config.toml` causes a safe refusal unless `--force` is passed.
- Use `jhedaiclaw onboard --channels-only` when you only need to rotate channel tokens/allowlists.
- Use `jhedaiclaw onboard --reinit` to start fresh. This backs up your existing config directory with a timestamp suffix and creates a new configuration from scratch.

### `agent`

- `jhedaiclaw agent`
- `jhedaiclaw agent -m "Hello"`
- `jhedaiclaw agent --provider <ID> --model <MODEL> --temperature <0.0-2.0>`
- `jhedaiclaw agent --peripheral <board:path>`

Tip:

- In interactive chat, you can ask for route changes in natural language (for example “conversation uses kimi, coding uses gpt-5.3-codex”); the assistant can persist this via tool `model_routing_config`.

### `gateway` / `daemon`

- `jhedaiclaw gateway [--host <HOST>] [--port <PORT>]`
- `jhedaiclaw daemon [--host <HOST>] [--port <PORT>]`

### `estop`

- `jhedaiclaw estop` (engage `kill-all`)
- `jhedaiclaw estop --level network-kill`
- `jhedaiclaw estop --level domain-block --domain "*.chase.com" [--domain "*.paypal.com"]`
- `jhedaiclaw estop --level tool-freeze --tool shell [--tool browser]`
- `jhedaiclaw estop status`
- `jhedaiclaw estop resume`
- `jhedaiclaw estop resume --network`
- `jhedaiclaw estop resume --domain "*.chase.com"`
- `jhedaiclaw estop resume --tool shell`
- `jhedaiclaw estop resume --otp <123456>`

Notes:

- `estop` commands require `[security.estop].enabled = true`.
- When `[security.estop].require_otp_to_resume = true`, `resume` requires OTP validation.
- OTP prompt appears automatically if `--otp` is omitted.

### `service`

- `jhedaiclaw service install`
- `jhedaiclaw service start`
- `jhedaiclaw service stop`
- `jhedaiclaw service restart`
- `jhedaiclaw service status`
- `jhedaiclaw service uninstall`

### `cron`

- `jhedaiclaw cron list`
- `jhedaiclaw cron add <expr> [--tz <IANA_TZ>] <command>`
- `jhedaiclaw cron add-at <rfc3339_timestamp> <command>`
- `jhedaiclaw cron add-every <every_ms> <command>`
- `jhedaiclaw cron once <delay> <command>`
- `jhedaiclaw cron remove <id>`
- `jhedaiclaw cron pause <id>`
- `jhedaiclaw cron resume <id>`

Notes:

- Mutating schedule/cron actions require `cron.enabled = true`.
- Shell command payloads for schedule creation (`create` / `add` / `once`) are validated by security command policy before job persistence.

### `models`

- `jhedaiclaw models refresh`
- `jhedaiclaw models refresh --provider <ID>`
- `jhedaiclaw models refresh --force`

`models refresh` currently supports live catalog refresh for provider IDs: `openrouter`, `openai`, `anthropic`, `groq`, `mistral`, `deepseek`, `xai`, `together-ai`, `gemini`, `ollama`, `llamacpp`, `sglang`, `vllm`, `astrai`, `venice`, `fireworks`, `cohere`, `moonshot`, `glm`, `zai`, `qwen`, and `nvidia`.

### `doctor`

- `jhedaiclaw doctor`
- `jhedaiclaw doctor models [--provider <ID>] [--use-cache]`
- `jhedaiclaw doctor traces [--limit <N>] [--event <TYPE>] [--contains <TEXT>]`
- `jhedaiclaw doctor traces --id <TRACE_ID>`

`doctor traces` reads runtime tool/model diagnostics from `observability.runtime_trace_path`.

### `channel`

- `jhedaiclaw channel list`
- `jhedaiclaw channel start`
- `jhedaiclaw channel doctor`
- `jhedaiclaw channel bind-telegram <IDENTITY>`
- `jhedaiclaw channel add <type> <json>`
- `jhedaiclaw channel remove <name>`

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

- `jhedaiclaw integrations info <name>`

### `skills`

- `jhedaiclaw skills list`
- `jhedaiclaw skills audit <source_or_name>`
- `jhedaiclaw skills install <source>`
- `jhedaiclaw skills remove <name>`

`<source>` accepts git remotes (`https://...`, `http://...`, `ssh://...`, and `git@host:owner/repo.git`) or a local filesystem path.

`skills install` always runs a built-in static security audit before the skill is accepted. The audit blocks:

- symlinks inside the skill package
- script-like files (`.sh`, `.bash`, `.zsh`, `.ps1`, `.bat`, `.cmd`)
- high-risk command snippets (for example pipe-to-shell payloads)
- markdown links that escape the skill root, point to remote markdown, or target script files

Use `skills audit` to manually validate a candidate skill directory (or an installed skill by name) before sharing it.

Skill manifests (`SKILL.toml`) support `prompts` and `[[tools]]`; both are injected into the agent system prompt at runtime, so the model can follow skill instructions without manually reading skill files.

### `migrate`

- `jhedaiclaw migrate openclaw [--source <path>] [--dry-run]`

### `config`

- `jhedaiclaw config schema`

`config schema` prints a JSON Schema (draft 2020-12) for the full `config.toml` contract to stdout.

### `completions`

- `jhedaiclaw completions bash`
- `jhedaiclaw completions fish`
- `jhedaiclaw completions zsh`
- `jhedaiclaw completions powershell`
- `jhedaiclaw completions elvish`

`completions` is stdout-only by design so scripts can be sourced directly without log/warning contamination.

### `hardware`

- `jhedaiclaw hardware discover`
- `jhedaiclaw hardware introspect <path>`
- `jhedaiclaw hardware info [--chip <chip_name>]`

### `peripheral`

- `jhedaiclaw peripheral list`
- `jhedaiclaw peripheral add <board> <path>`
- `jhedaiclaw peripheral flash [--port <serial_port>]`
- `jhedaiclaw peripheral setup-uno-q [--host <ip_or_host>]`
- `jhedaiclaw peripheral flash-nucleo`

## Validation Tip

To verify docs against your current binary quickly:

```bash
jhedaiclaw --help
jhedaiclaw <command> --help
```
