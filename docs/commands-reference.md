# ZeroClaw Commands Reference

This reference is derived from the current CLI surface (`zeroclaw --help`).

Last verified: **February 28, 2026**.

## Top-Level Commands

| Command | Purpose |
|---|---|
| `onboard` | Initialize workspace/config quickly or interactively |
| `agent` | Run interactive chat or single-message mode |
| `gateway` | Start webhook and WhatsApp HTTP gateway |
| `daemon` | Start supervised runtime (gateway + channels + optional heartbeat/scheduler) |
| `service` | Manage user-level OS service lifecycle |
| `doctor` | Run diagnostics and freshness checks |
| `status` | Print current configuration and system summary |
| `update` | Check or install latest ZeroClaw release |
| `estop` | Engage/resume emergency stop levels and inspect estop state |
| `cron` | Manage scheduled tasks |
| `models` | Refresh provider model catalogs |
| `providers` | List provider IDs, aliases, and active provider |
| `providers-quota` | Check provider quota usage, rate limits, and health |
| `channel` | Manage channels and channel health checks |
| `integrations` | Inspect integration details |
| `skills` | List/install/remove skills |
| `migrate` | Import from external runtimes (currently OpenClaw) |
| `config` | Inspect, query, and modify runtime configuration |
| `completions` | Generate shell completion scripts to stdout |
| `hardware` | Discover and introspect USB hardware |
| `peripheral` | Configure and flash peripherals |

## Command Groups

### `onboard`

- `zeroclaw onboard`
- `zeroclaw onboard --interactive`
- `zeroclaw onboard --channels-only`
- `zeroclaw onboard --force`
- `zeroclaw onboard --api-key <KEY> --provider <ID> --memory <sqlite|lucid|markdown|none>`
- `zeroclaw onboard --api-key <KEY> --provider <ID> --model <MODEL_ID> --memory <sqlite|lucid|markdown|none>`
- `zeroclaw onboard --api-key <KEY> --provider <ID> --model <MODEL_ID> --memory <sqlite|lucid|markdown|none> --force`
- `zeroclaw onboard --migrate-openclaw`
- `zeroclaw onboard --migrate-openclaw --openclaw-source <PATH> --openclaw-config <PATH>`

`onboard` safety behavior:

- If `config.toml` already exists and you run `--interactive`, onboarding now offers two modes:
  - Full onboarding (overwrite `config.toml`)
  - Provider-only update (update provider/model/API key while preserving existing channels, tunnel, memory, hooks, and other settings)
- In non-interactive environments, existing `config.toml` causes a safe refusal unless `--force` is passed.
- Use `zeroclaw onboard --channels-only` when you only need to rotate channel tokens/allowlists.
- OpenClaw migration mode is merge-first by design: existing ZeroClaw data/config is preserved, missing fields are filled, and list-like values are union-merged with de-duplication.
- Interactive onboarding can auto-detect `~/.openclaw` and prompt for optional merge migration even without `--migrate-openclaw`.

### `agent`

- `zeroclaw agent`
- `zeroclaw agent -m "Hello"`
- `zeroclaw agent --provider <ID> --model <MODEL> --temperature <0.0-2.0>`
- `zeroclaw agent --peripheral <board:path>`

Tip:

- In interactive chat, you can ask for route changes in natural language (for example “conversation uses kimi, coding uses gpt-5.3-codex”); the assistant can persist this via tool `model_routing_config`.
- In interactive chat, you can also ask to:
  - switch web search provider/fallbacks (`web_search_config`)
  - inspect or update domain access policy (`web_access_config`)
  - preview/apply OpenClaw merge migration (`openclaw_migration`)

### `gateway` / `daemon`

- `zeroclaw gateway [--host <HOST>] [--port <PORT>] [--new-pairing]`
- `zeroclaw daemon [--host <HOST>] [--port <PORT>]`

`--new-pairing` clears all stored paired tokens and forces generation of a fresh pairing code on gateway startup.

### `estop`

- `zeroclaw estop` (engage `kill-all`)
- `zeroclaw estop --level network-kill`
- `zeroclaw estop --level domain-block --domain "*.chase.com" [--domain "*.paypal.com"]`
- `zeroclaw estop --level tool-freeze --tool shell [--tool browser]`
- `zeroclaw estop status`
- `zeroclaw estop resume`
- `zeroclaw estop resume --network`
- `zeroclaw estop resume --domain "*.chase.com"`
- `zeroclaw estop resume --tool shell`
- `zeroclaw estop resume --otp <123456>`

Notes:

- `estop` commands require `[security.estop].enabled = true`.
- When `[security.estop].require_otp_to_resume = true`, `resume` requires OTP validation.
- OTP prompt appears automatically if `--otp` is omitted.

### `service`

- `zeroclaw service install`
- `zeroclaw service start`
- `zeroclaw service stop`
- `zeroclaw service restart`
- `zeroclaw service status`
- `zeroclaw service uninstall`

### `update`

- `zeroclaw update --check` (check for new release, no install)
- `zeroclaw update` (install latest release binary for current platform)
- `zeroclaw update --force` (reinstall even if current version matches latest)
- `zeroclaw update --instructions` (print install-method-specific guidance)

Notes:

- If ZeroClaw is installed via Homebrew, prefer `brew upgrade zeroclaw`.
- `update --instructions` detects common install methods and prints the safest path.

### `cron`

- `zeroclaw cron list`
- `zeroclaw cron add <expr> [--tz <IANA_TZ>] <command>`
- `zeroclaw cron add-at <rfc3339_timestamp> <command>`
- `zeroclaw cron add-every <every_ms> <command>`
- `zeroclaw cron once <delay> <command>`
- `zeroclaw cron remove <id>`
- `zeroclaw cron pause <id>`
- `zeroclaw cron resume <id>`

Notes:

- Mutating schedule/cron actions require `cron.enabled = true`.
- Shell command payloads for schedule creation (`create` / `add` / `once`) are validated by security command policy before job persistence.

### `models`

- `zeroclaw models refresh`
- `zeroclaw models refresh --provider <ID>`
- `zeroclaw models refresh --force`

`models refresh` currently supports live catalog refresh for provider IDs: `openrouter`, `openai`, `anthropic`, `groq`, `mistral`, `deepseek`, `xai`, `together-ai`, `gemini`, `ollama`, `llamacpp`, `sglang`, `vllm`, `astrai`, `venice`, `fireworks`, `cohere`, `moonshot`, `stepfun`, `glm`, `zai`, `qwen`, `volcengine` (`doubao`/`ark` aliases), `siliconflow`, and `nvidia`.

#### Live model availability test

```bash
./dev/test_models.sh              # test all Gemini models + profile rotation
./dev/test_models.sh models       # test model availability only
./dev/test_models.sh profiles     # test profile rotation only
```

Runs a Rust integration test (`tests/gemini_model_availability.rs`) that verifies each model against the OAuth endpoint (cloudcode-pa). Requires valid Gemini OAuth credentials in `auth-profiles.json`.

### `providers-quota`

- `zeroclaw providers-quota` — show quota status for all configured providers
- `zeroclaw providers-quota --provider gemini` — show quota for a specific provider
- `zeroclaw providers-quota --format json` — JSON output for scripting

Displays provider quota usage, rate limits, circuit breaker state, and OAuth profile health.

### `doctor`

- `zeroclaw doctor`
- `zeroclaw doctor models [--provider <ID>] [--use-cache]`
- `zeroclaw doctor traces [--limit <N>] [--event <TYPE>] [--contains <TEXT>]`
- `zeroclaw doctor traces --id <TRACE_ID>`

Provider connectivity matrix CI/local helper:

- `python3 scripts/ci/provider_connectivity_matrix.py --binary target/release-fast/zeroclaw --contract .github/connectivity/probe-contract.json`

`doctor traces` reads runtime tool/model diagnostics from `observability.runtime_trace_path`.

### `channel`

- `zeroclaw channel list`
- `zeroclaw channel start`
- `zeroclaw channel doctor`
- `zeroclaw channel bind-telegram <IDENTITY>`
- `zeroclaw channel add <type> <json>`
- `zeroclaw channel remove <name>`

Runtime in-chat commands while channel server is running:

- Telegram/Discord sender-session routing:
  - `/models`
  - `/models <provider>`
  - `/model`
  - `/model <model-id>`
  - `/new`
- Supervised tool approvals (all non-CLI channels):
  - `/approve-request <tool-name>` (create pending approval request)
  - `/approve-confirm <request-id>` (confirm pending request; same sender + same chat/channel only)
  - `/approve-allow <request-id>` (approve current pending runtime execution request once; no policy persistence)
  - `/approve-deny <request-id>` (deny current pending runtime execution request)
  - `/approve-pending` (list pending requests in current sender+chat/channel scope)
  - `/approve <tool-name>` (direct one-step grant + persist to `autonomy.auto_approve`, compatibility path)
  - `/unapprove <tool-name>` (revoke + remove from `autonomy.auto_approve`)
  - `/approvals` (show runtime + persisted approval state)
  - Natural-language approval behavior is controlled by `[autonomy].non_cli_natural_language_approval_mode`:
    - `direct` (default): `授权工具 shell` / `approve tool shell` immediately grants
    - `request_confirm`: natural-language approval creates pending request, then confirm with request ID
    - `disabled`: natural-language approval commands are ignored (slash commands only)
  - Optional per-channel override: `[autonomy].non_cli_natural_language_approval_mode_by_channel`

Approval safety behavior:

- Runtime approval commands are parsed and executed **before** LLM inference in the channel loop.
- Pending requests are sender+chat/channel scoped and expire automatically.
- Confirmation requires the same sender in the same chat/channel that created the request.
- Once approved and persisted, the tool remains approved across restarts until revoked.
- Optional policy gate: `[autonomy].non_cli_approval_approvers` can restrict who may execute approval-management commands.

Startup behavior for multiple channels:
- `zeroclaw channel start` starts all configured channels in one process.
- If one channel fails initialization, other channels continue to start.
- If all configured channels fail initialization, startup exits with an error.

Channel runtime also watches `config.toml` and hot-applies updates to:
- `default_provider`
- `default_model`
- `default_temperature`
- `api_key` / `api_url` (for the default provider)
- `reliability.*` provider retry settings

`add/remove` currently route you back to managed setup/manual config paths (not full declarative mutators yet).

### `integrations`

- `zeroclaw integrations info <name>`

### `skills`

- `zeroclaw skills list`
- `zeroclaw skills audit <source_or_name>`
- `zeroclaw skills install <source>`
- `zeroclaw skills remove <name>`

`<source>` accepts:

| Format | Example | Notes |
|---|---|---|
| **ClawhHub profile URL** | `https://clawhub.ai/steipete/summarize` | Auto-detected by domain; downloads zip from ClawhHub API |
| **ClawhHub short prefix** | `clawhub:summarize` | Short form; slug is the skill name on ClawhHub |
| **Direct zip URL** | `zip:https://example.com/skill.zip` | Any HTTPS URL returning a zip archive |
| **Local zip file** | `/path/to/skill.zip` | Zip file already downloaded to local disk |
| **Registry packages** | `namespace/name` or `namespace/name@version` | Fetched from the configured registry (default: ZeroMarket) |
| **Git remotes** | `https://github.com/…`, `git@host:owner/repo.git` | Cloned with `git clone --depth 1` |
| **Local filesystem paths** | `./my-skill` or `/abs/path/skill` | Directory copied and audited |

**ClawhHub install examples:**

```bash
# Install by profile URL (slug extracted from last path segment)
zeroclaw skill install https://clawhub.ai/steipete/summarize

# Install using short prefix
zeroclaw skill install clawhub:summarize

# Install from a zip already downloaded locally
zeroclaw skill install ~/Downloads/summarize-1.0.0.zip
```

If the ClawhHub API returns 429 (rate limit) or requires authentication, set `clawhub_token` in `[skills]` config (see [config reference](config-reference.md#skills)).

**Zip-based install behavior:**
- If the zip contains `_meta.json` (OpenClaw convention), name/version/author are read from it.
- A minimal `SKILL.toml` is written automatically if neither `SKILL.toml` nor `SKILL.md` is present in the zip.

Registry packages are installed to `~/.zeroclaw/workspace/skills/<name>/`.

`skills install` always runs a built-in static security audit before the skill is accepted. The audit blocks:
- symlinks inside the skill package
- script-like files (`.sh`, `.bash`, `.zsh`, `.ps1`, `.bat`, `.cmd`)
- high-risk command snippets (for example pipe-to-shell payloads)
- markdown links that escape the skill root, point to remote markdown, or target script files

> **Note:** The security audit applies to directory-based installs (local paths, git remotes). Zip-based installs (ClawhHub, direct zip URLs, local zip files) perform path-traversal safety checks during extraction but do not run the full static audit — review zip contents manually for untrusted sources.

Use `skills audit` to manually validate a candidate skill directory (or an installed skill by name) before sharing it.

Workspace symlink policy:
- Symlinked entries under `~/.zeroclaw/workspace/skills/` are blocked by default.
- To allow shared local skill directories, set `[skills].trusted_skill_roots` in `config.toml`.
- A symlinked skill is accepted only when its resolved canonical target is inside one of the trusted roots.

Skill manifests (`SKILL.toml`) support `prompts` and `[[tools]]`; both are injected into the agent system prompt at runtime, so the model can follow skill instructions without manually reading skill files.

### `migrate`

- `zeroclaw migrate openclaw [--source <path>] [--source-config <path>] [--dry-run] [--no-memory] [--no-config]`

`migrate openclaw` behavior:

- Default mode migrates both memory and config/agents with merge-first semantics.
- Existing ZeroClaw values are preserved; migration does not overwrite existing user content.
- Memory migration de-duplicates repeated content during merge while keeping existing entries intact.
- `--dry-run` prints a migration report without writing data.
- `--no-memory` or `--no-config` scopes migration to selected modules.

### `config`

- `zeroclaw config show`
- `zeroclaw config get <key>`
- `zeroclaw config set <key> <value>`
- `zeroclaw config schema`

`config show` prints the full effective configuration as pretty JSON with secrets masked as `***REDACTED***`. Environment variable overrides are already applied.

`config get <key>` queries a single value by dot-separated path (e.g. `gateway.port`, `security.estop.enabled`). Scalars print raw values; objects and arrays print pretty JSON. Sensitive fields are masked.

`config set <key> <value>` updates a configuration value and persists it atomically to `config.toml`. Types are inferred automatically (`true`/`false` → bool, integers, floats, JSON syntax → object/array, otherwise string). Type mismatches are rejected before writing.

`config schema` prints a JSON Schema (draft 2020-12) for the full `config.toml` contract to stdout.

### `completions`

- `zeroclaw completions bash`
- `zeroclaw completions fish`
- `zeroclaw completions zsh`
- `zeroclaw completions powershell`
- `zeroclaw completions elvish`

`completions` is stdout-only by design so scripts can be sourced directly without log/warning contamination.

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
