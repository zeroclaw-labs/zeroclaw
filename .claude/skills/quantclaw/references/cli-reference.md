# QuantClaw CLI Reference

Complete command reference for the `quantclaw` binary.

## Table of Contents

1. [Agent](#agent)
2. [Onboarding](#onboarding)
3. [Status & Diagnostics](#status--diagnostics)
4. [Memory](#memory)
5. [Cron](#cron)
6. [Providers & Models](#providers--models)
7. [Gateway & Daemon](#gateway--daemon)
8. [Service Management](#service-management)
9. [Channels](#channels)
10. [Security & Emergency Stop](#security--emergency-stop)
11. [Hardware Peripherals](#hardware-peripherals)
12. [Skills](#skills)
13. [Shell Completions](#shell-completions)

---

## Agent

Interactive chat or single-message mode.

```bash
quantclaw agent                                          # Interactive REPL
quantclaw agent -m "Summarize today's logs"              # Single message
quantclaw agent -p anthropic --model claude-sonnet-4-6   # Override provider/model
quantclaw agent -t 0.3                                   # Set temperature
quantclaw agent --peripheral nucleo-f401re:/dev/ttyACM0  # Attach hardware
```

**Key flags:**
- `-m <message>` — single message mode (no REPL)
- `-p <provider>` — override provider (openrouter, anthropic, openai, ollama)
- `--model <model>` — override model
- `-t <float>` — temperature (0.0–2.0)
- `--peripheral <name>:<port>` — attach hardware peripheral

The agent has access to 30+ tools gated by security policy: shell, file_read, file_write, file_edit, glob_search, content_search, memory_store, memory_recall, memory_forget, browser, http_request, web_fetch, web_search, cron, delegate, git, and more. Max tool iterations defaults to 10.

---

## Onboarding

First-time setup or reconfiguration.

```bash
quantclaw onboard                                 # Quick mode (default: openrouter)
quantclaw onboard --provider anthropic            # Quick mode with specific provider
quantclaw onboard                                 # Guided wizard (default)
quantclaw onboard --memory sqlite                 # Set memory backend
quantclaw onboard --force                         # Overwrite existing config
quantclaw onboard --channels-only                 # Repair channels only
```

**Key flags:**
- `--provider <name>` — openrouter (default), anthropic, openai, ollama
- `--model <model>` — default model
- `--memory <backend>` — sqlite, markdown, lucid, none
- `--force` — overwrite existing config.toml
- `--channels-only` — only repair channel configuration
- `--reinit` — start fresh (backs up existing config)

Creates `~/.quantclaw/config.toml` with `0600` permissions.

---

## Status & Diagnostics

```bash
quantclaw status                    # System overview
quantclaw doctor                    # Run all diagnostic checks
quantclaw doctor models             # Probe model connectivity
quantclaw doctor traces             # Query execution traces
```

---

## Memory

```bash
quantclaw memory list                              # List all entries
quantclaw memory list --category core --limit 10   # Filtered list
quantclaw memory get "some-key"                    # Get specific entry
quantclaw memory stats                             # Usage statistics
quantclaw memory clear --key "prefix" --yes        # Delete entries (requires --yes)
```

**Key flags:**
- `--category <name>` — filter by category (core, daily, conversation, custom)
- `--limit <n>` — limit results
- `--key <prefix>` — key prefix for clear operations
- `--yes` — skip confirmation (required for clear)

---

## Cron

```bash
quantclaw cron list                                                      # List all jobs
quantclaw cron add '0 9 * * 1-5' 'Good morning' --tz America/New_York   # Recurring (cron expr)
quantclaw cron add-at '2026-03-11T10:00:00Z' 'Remind me about meeting'  # One-time at specific time
quantclaw cron add-every 3600000 'Check server health'                   # Interval in milliseconds
quantclaw cron once 30m 'Follow up on that task'                         # Delay from now
quantclaw cron pause <id>                                                # Pause job
quantclaw cron resume <id>                                               # Resume job
quantclaw cron remove <id>                                               # Delete job
```

**Subcommands:**
- `add <cron-expr> <command>` — standard cron expression (5-field)
- `add-at <iso-datetime> <command>` — fire once at exact time
- `add-every <ms> <command>` — repeating interval
- `once <duration> <command>` — delay from now (e.g., `30m`, `2h`, `1d`)

---

## Providers & Models

```bash
quantclaw providers                                # List all 40+ supported providers
quantclaw models list                              # Show cached model catalog
quantclaw models refresh --all                     # Refresh catalogs from all providers
quantclaw models set anthropic/claude-sonnet-4-6   # Set default model
quantclaw models status                            # Current model info
```

Model routing in config.toml:
```toml
[[model_routes]]
hint = "reasoning"
provider = "openrouter"
model = "anthropic/claude-sonnet-4-6"
```

---

## Gateway & Daemon

```bash
quantclaw gateway                                 # Start HTTP gateway (foreground)
quantclaw gateway -p 8080 --host 127.0.0.1        # Custom port/host

quantclaw daemon                                  # Gateway + channels + scheduler + heartbeat
quantclaw daemon -p 8080 --host 0.0.0.0           # Custom bind
```

**Gateway defaults:**
- Port: 42617
- Host: 127.0.0.1
- Pairing required: true
- Public bind allowed: false

---

## Service Management

OS service lifecycle (systemd on Linux, launchd on macOS).

```bash
quantclaw service install     # Install as system service
quantclaw service start       # Start the service
quantclaw service status      # Check service status
quantclaw service stop        # Stop the service
quantclaw service restart     # Restart the service
quantclaw service uninstall   # Remove the service
```

**Logs:**
- macOS: `~/.quantclaw/logs/daemon.stdout.log`
- Linux: `journalctl -u quantclaw`

---

## Channels

Channels are configured in `config.toml` under `[channels]` and `[channels_config.*]`.

```bash
quantclaw channels list       # List configured channels
quantclaw channels doctor     # Check channel health
```

Supported channels (21 total): Telegram, Discord, Slack, WhatsApp (Meta), WATI, Linq (iMessage/RCS/SMS), Email (IMAP/SMTP), IRC, Matrix, Nostr, Signal, Nextcloud Talk, and more.

Channel config example (Telegram):
```toml
[channels]
telegram = true

[channels_config.telegram]
bot_token = "..."
allowed_users = [123456789]
```

---

## Security & Emergency Stop

```bash
quantclaw estop --level kill-all                              # Stop everything
quantclaw estop --level network-kill                          # Block all network access
quantclaw estop --level domain-block --domain "*.example.com" # Block specific domains
quantclaw estop --level tool-freeze --tool shell              # Freeze specific tool
quantclaw estop status                                        # Check estop state
quantclaw estop resume --network                              # Resume (may require OTP)
```

**Estop levels:**
- `kill-all` — nuclear option, stops all agent activity
- `network-kill` — blocks all outbound network
- `domain-block` — blocks specific domain patterns
- `tool-freeze` — freezes individual tools

Autonomy config in config.toml:
```toml
[autonomy]
level = "supervised"                           # read_only | supervised | full
workspace_only = true
allowed_commands = ["git", "cargo", "python"]
forbidden_paths = ["/etc", "/root", "~/.ssh"]
max_actions_per_hour = 20
max_cost_per_day_cents = 500
```

---

## Hardware Peripherals

```bash
quantclaw hardware discover                              # Find USB devices
quantclaw hardware introspect /dev/ttyACM0               # Probe device capabilities
quantclaw peripheral list                                # List configured peripherals
quantclaw peripheral add nucleo-f401re /dev/ttyACM0      # Add peripheral
quantclaw peripheral flash-nucleo                        # Flash STM32 firmware
quantclaw peripheral flash --port /dev/cu.usbmodem101    # Flash Arduino firmware
```

**Supported boards:** STM32 Nucleo-F401RE, Arduino Uno R4, Raspberry Pi GPIO, ESP32.

Attach to agent session: `quantclaw agent --peripheral nucleo-f401re:/dev/ttyACM0`

---

## Skills

```bash
quantclaw skills list         # List installed skills
quantclaw skills install <path-or-url>  # Install a skill
quantclaw skills audit        # Audit installed skills
quantclaw skills remove <name>  # Remove a skill
```

---

## Shell Completions

```bash
quantclaw completions zsh     # Generate Zsh completions
quantclaw completions bash    # Generate Bash completions
quantclaw completions fish    # Generate Fish completions
```

---

## Config File

Default location: `~/.quantclaw/config.toml`

Config resolution order (first match wins):
1. `QUANTCLAW_CONFIG_DIR` environment variable
2. `QUANTCLAW_WORKSPACE` environment variable
3. `~/.quantclaw/active_workspace.toml` marker file
4. `~/.quantclaw/config.toml` (default)
