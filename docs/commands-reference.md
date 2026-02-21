# ZeroClaw Commands Reference

This reference is derived from the current CLI surface (`zeroclaw --help`).

Last verified: **February 21, 2026**.

## Top-Level Commands

| Command | Purpose |
|---|---|
| `onboard` | Initialize workspace/config quickly or interactively |
| `agent` | Run interactive chat or single-message mode |
| `update` | Check/apply binary updates from GitHub Releases |
| `gateway` | Start webhook and WhatsApp HTTP gateway |
| `daemon` | Start supervised runtime (gateway + channels + optional heartbeat/scheduler) |
| `service` | Manage user-level OS service lifecycle |
| `doctor` | Run diagnostics and freshness checks |
| `status` | Print current configuration and system summary |
| `cron` | Manage scheduled tasks |
| `models` | Refresh provider model catalogs |
| `preset` | Manage preset composition/import/export/intent planning |
| `security` | Inspect and change security/autonomy profiles |
| `providers` | List provider IDs, aliases, and active provider |
| `channel` | Manage channels and channel health checks |
| `integrations` | Inspect integration details |
| `skills` | List/install/remove skills |
| `migrate` | Import from external runtimes (currently OpenClaw) |
| `hardware` | Discover and introspect USB hardware |
| `peripheral` | Configure and flash peripherals |

## Command Groups

### `onboard`

- `zeroclaw onboard`
- `zeroclaw onboard --interactive`
- `zeroclaw onboard --channels-only`
- `zeroclaw onboard --api-key <KEY> --provider <ID> --memory <sqlite|lucid|markdown|none>`
- `zeroclaw onboard --preset <ID> [--pack <PACK>]...`
- `zeroclaw onboard --security-profile <strict|balanced|flexible|full> [--yes-security-risk]`

Official preset IDs currently shipped:

- `minimal`
- `default`
- `automation`
- `hardware-lab`
- `hardened-linux`

### `agent`

- `zeroclaw agent`
- `zeroclaw agent -m "Hello"`
- `zeroclaw agent --provider <ID> --model <MODEL> --temperature <0.0-2.0>`
- `zeroclaw agent --peripheral <board:path>`

### `update`

- `zeroclaw update` (check latest release only)
- `zeroclaw update --version <VERSION>` (check a specific release tag)
- `zeroclaw update --apply --yes` (download/extract/install update)
- `zeroclaw update --apply --dry-run` (preview apply without file changes)
- `zeroclaw update --apply --version <VERSION> --yes` (apply specific version)
- `zeroclaw update --apply --install-path <PATH> --yes` (override install target)

### `gateway` / `daemon`

- `zeroclaw gateway [--host <HOST>] [--port <PORT>]`
- `zeroclaw daemon [--host <HOST>] [--port <PORT>]`

### `service`

- `zeroclaw service install`
- `zeroclaw service start`
- `zeroclaw service stop`
- `zeroclaw service status`
- `zeroclaw service uninstall`

### `cron`

- `zeroclaw cron list`
- `zeroclaw cron add <expr> [--tz <IANA_TZ>] <command>`
- `zeroclaw cron add-at <rfc3339_timestamp> <command>`
- `zeroclaw cron add-every <every_ms> <command>`
- `zeroclaw cron once <delay> <command>`
- `zeroclaw cron remove <id>`
- `zeroclaw cron pause <id>`
- `zeroclaw cron resume <id>`

### `models`

- `zeroclaw models refresh`
- `zeroclaw models refresh --provider <ID>`
- `zeroclaw models refresh --force`

`models refresh` currently supports live catalog refresh for provider IDs: `openrouter`, `openai`, `anthropic`, `groq`, `mistral`, `deepseek`, `xai`, `together-ai`, `gemini`, `ollama`, `astrai`, `venice`, `fireworks`, `cohere`, `moonshot`, `glm`, `zai`, `qwen`, and `nvidia`.

### `preset`

- `zeroclaw preset list`
- `zeroclaw preset show <ID>`
- `zeroclaw preset current`
- `zeroclaw preset apply [--preset <ID>] [--pack <PACK>]... [--remove-pack <PACK>]... [--dry-run] [--yes-risky] [--rebuild --yes-rebuild]`
- `zeroclaw preset intent "<text>" [--capabilities-file <path>]...` (plan only)
- `zeroclaw preset intent "<text>" --json [--capabilities-file <path>]...` (plan + security recommendation + generated next commands, no write)
- `zeroclaw preset intent "<text>" --emit-shell <path> [--capabilities-file <path>]...` (write orchestration script template, no execute)
- `zeroclaw preset intent "<text>" --apply [--capabilities-file <path>]... [--dry-run] [--yes-risky] [--rebuild --yes-rebuild]`
- `zeroclaw preset export <path> [--preset <ID>]`
- `zeroclaw preset import <path> [--mode overwrite|merge|fill] [--dry-run] [--yes-risky] [--rebuild --yes-rebuild]`
- `zeroclaw preset validate <path...> [--allow-unknown-packs] [--json]`
- `zeroclaw preset rebuild [--dry-run] [--yes]`

Safety notes:

- Risk-gated packs require explicit approval with `--yes-risky` when applying/importing/intent-applying.
- Rebuild execution requires explicit approval (`--yes-rebuild` for apply/import/intent and `--yes` for `preset rebuild`).
- `preset intent --json` is advisory/orchestration mode only and cannot be combined with `--apply`.
- `preset intent --emit-shell` is advisory/orchestration mode only and cannot be combined with `--apply`.
- `preset intent` in plan mode prints generated follow-up commands but does not execute them.
- `preset intent --json` includes `next_commands[].consent_reasons` for UI/agent confirmation flows (for example `risky_pack`, `rebuild`, `security_non_strict`).

### `security`

- `zeroclaw security show`
- `zeroclaw security profile set strict`
- `zeroclaw security profile set balanced --dry-run`
- `zeroclaw security profile set flexible --yes-risk`
- `zeroclaw security profile set full --yes-risk`
- `zeroclaw security profile set strict --non-cli-approval manual`
- `zeroclaw security profile set strict --non-cli-approval auto --yes-risk`
- `zeroclaw security profile recommend "need unattended browser automation"`
- `zeroclaw security profile recommend "need unattended browser automation" --from-preset automation --pack rag-pdf`
- `zeroclaw security profile recommend "hardened deployment" --from-preset hardened-linux --remove-pack tools-update`
- `zeroclaw security profile set full --dry-run --json`
- `zeroclaw security profile set balanced --dry-run --export-diff .zeroclaw-security-diff.json`

Safety notes:

- Setting non-strict profiles requires explicit consent (`--yes-risk`) unless using `--dry-run`.
- Enabling non-CLI auto-approval (`--non-cli-approval auto`) also requires explicit consent (`--yes-risk`) unless using `--dry-run`.
- `--non-cli-approval manual|auto` controls whether non-CLI channels can auto-approve approval-gated tool calls.
- `onboard --security-profile` in quick mode also requires `--yes-security-risk` for non-strict profiles.
- `security profile set` supports machine-readable reports via `--json` and file export via `--export-diff <PATH>`.
- `security profile recommend` is advisory-only (no write). Use it to turn intent text + preset plan into a guarded profile suggestion.
- `security profile recommend` supports preflight composition via `--from-preset`, `--pack`, and `--remove-pack` without mutating workspace state.
- If you need to immediately return to safe defaults, run `zeroclaw security profile set strict`.
- After onboarding, agent tool calls cannot silently bypass policy guards. If an operation is blocked by security policy, tool results include remediation guidance (`security show`, `security profile recommend`, and graded `security profile set ... --yes-risk` options) plus explicit risk warnings.

### `channel`

- `zeroclaw channel list`
- `zeroclaw channel start`
- `zeroclaw channel doctor`
- `zeroclaw channel bind-telegram <IDENTITY>`
- `zeroclaw channel add <type> <json>`
- `zeroclaw channel remove <name>`

Runtime in-chat commands (Telegram/Discord while channel server is running):

- `/models`
- `/models <provider>`
- `/model`
- `/model <model-id>`

`add/remove` currently route you back to managed setup/manual config paths (not full declarative mutators yet).

### `integrations`

- `zeroclaw integrations info <name>`

### `skills`

- `zeroclaw skills list`
- `zeroclaw skills install <source>`
- `zeroclaw skills remove <name>`

### `migrate`

- `zeroclaw migrate openclaw [--source <path>] [--dry-run]`

### `hardware`

- `zeroclaw hardware discover`
- `zeroclaw hardware introspect <path>`
- `zeroclaw hardware info [--chip <chip_name>]`

### `peripheral`

- `zeroclaw peripheral list`
- `zeroclaw peripheral add <board> <path>`
- `zeroclaw peripheral flash [--port <serial_port>]`
- `zeroclaw peripheral setup-uno-q [--host <ip_or_host>]`
- `zeroclaw peripheral flash-nucleo`

## Validation Tip

To verify docs against your current binary quickly:

```bash
zeroclaw --help
zeroclaw <command> --help
```
