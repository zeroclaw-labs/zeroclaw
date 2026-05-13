# DaemonClaw CLI Reference

Complete command reference for the `daemonclaw` binary.

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
daemonclaw agent                                          # Interactive REPL
daemonclaw agent -m "Summarize today's logs"              # Single message
daemonclaw agent -p anthropic --model claude-sonnet-4-6   # Override provider/model
daemonclaw agent -t 0.3                                   # Set temperature
daemonclaw agent --peripheral nucleo-f401re:/dev/ttyACM0  # Attach hardware
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
daemonclaw onboard                                 # Quick mode (default: openrouter)
daemonclaw onboard --provider anthropic            # Quick mode with specific provider
daemonclaw onboard                                 # Guided wizard (default)
daemonclaw onboard --memory sqlite                 # Set memory backend
daemonclaw onboard --force                         # Overwrite existing config
daemonclaw onboard --channels-only                 # Repair channels only
```

**Key flags:**
- `--provider <name>` — openrouter (default), anthropic, openai, ollama
- `--model <model>` — default model
- `--memory <backend>` — sqlite, markdown, lucid, none
- `--force` — overwrite existing config.toml
- `--channels-only` — only repair channel configuration
- `--reinit` — start fresh (backs up existing config)

Creates `~/.daemonclaw/config.toml` with `0600` permissions.

---

## Status & Diagnostics

```bash
daemonclaw status                    # System overview
daemonclaw doctor                    # Run all diagnostic checks
daemonclaw doctor models             # Probe model connectivity
daemonclaw doctor traces             # Query execution traces
```

---

## Memory

```bash
daemonclaw memory list                              # List all entries
daemonclaw memory list --category core --limit 10   # Filtered list
daemonclaw memory get "some-key"                    # Get specific entry
daemonclaw memory stats                             # Usage statistics
daemonclaw memory clear --key "prefix" --yes        # Delete entries (requires --yes)
```

**Key flags:**
- `--category <name>` — filter by category (core, daily, conversation, custom)
- `--limit <n>` — limit results
- `--key <prefix>` — key prefix for clear operations
- `--yes` — skip confirmation (required for clear)

---

## Cron

```bash
daemonclaw cron list                                                      # List all jobs
daemonclaw cron add '0 9 * * 1-5' 'Good morning' --tz America/New_York   # Recurring (cron expr)
daemonclaw cron add-at '2026-03-11T10:00:00Z' 'Remind me about meeting'  # One-time at specific time
daemonclaw cron add-every 3600000 'Check server health'                   # Interval in milliseconds
daemonclaw cron once 30m 'Follow up on that task'                         # Delay from now
daemonclaw cron pause <id>                                                # Pause job
daemonclaw cron resume <id>                                               # Resume job
daemonclaw cron remove <id>                                               # Delete job
```

**Subcommands:**
- `add <cron-expr> <command>` — standard cron expression (5-field)
- `add-at <iso-datetime> <command>` — fire once at exact time
- `add-every <ms> <command>` — repeating interval
- `once <duration> <command>` — delay from now (e.g., `30m`, `2h`, `1d`)

---

## Providers & Models

```bash
daemonclaw providers                                # List all 40+ supported providers
daemonclaw models list                              # Show cached model catalog
daemonclaw models refresh --all                     # Refresh catalogs from all providers
daemonclaw models set anthropic/claude-sonnet-4-6   # Set default model
daemonclaw models status                            # Current model info
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
daemonclaw gateway                                 # Start HTTP gateway (foreground)
daemonclaw gateway -p 8080 --host 127.0.0.1        # Custom port/host

daemonclaw daemon                                  # Gateway + channels + scheduler + heartbeat
daemonclaw daemon -p 8080 --host 0.0.0.0           # Custom bind
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
daemonclaw service install     # Install as system service
daemonclaw service start       # Start the service
daemonclaw service status      # Check service status
daemonclaw service stop        # Stop the service
daemonclaw service restart     # Restart the service
daemonclaw service uninstall   # Remove the service
```

**Logs:**
- macOS: `~/.daemonclaw/logs/daemon.stdout.log`
- Linux: `journalctl -u daemonclaw`

---

## Channels

Channels are configured in `config.toml` under `[channels]` and `[channels_config.*]`.

```bash
daemonclaw channels list       # List configured channels
daemonclaw channels doctor     # Check channel health
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
daemonclaw estop --level kill-all                              # Stop everything
daemonclaw estop --level network-kill                          # Block all network access
daemonclaw estop --level domain-block --domain "*.example.com" # Block specific domains
daemonclaw estop --level tool-freeze --tool shell              # Freeze specific tool
daemonclaw estop status                                        # Check estop state
daemonclaw estop resume --network                              # Resume (may require OTP)
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
daemonclaw hardware discover                              # Find USB devices
daemonclaw hardware introspect /dev/ttyACM0               # Probe device capabilities
daemonclaw peripheral list                                # List configured peripherals
daemonclaw peripheral add nucleo-f401re /dev/ttyACM0      # Add peripheral
daemonclaw peripheral flash-nucleo                        # Flash STM32 firmware
daemonclaw peripheral flash --port /dev/cu.usbmodem101    # Flash Arduino firmware
```

**Supported boards:** STM32 Nucleo-F401RE, Arduino Uno R4, Raspberry Pi GPIO, ESP32.

Attach to agent session: `daemonclaw agent --peripheral nucleo-f401re:/dev/ttyACM0`

---

## Skills

```bash
daemonclaw skills list         # List installed skills
daemonclaw skills install <path-or-url>  # Install a skill
daemonclaw skills audit        # Audit installed skills
daemonclaw skills remove <name>  # Remove a skill
```

---

## Shell Completions

```bash
daemonclaw completions zsh     # Generate Zsh completions
daemonclaw completions bash    # Generate Bash completions
daemonclaw completions fish    # Generate Fish completions
```

---

## Config File

Default location: `~/.daemonclaw/config.toml`

Config resolution order (first match wins):
1. `DAEMONCLAW_CONFIG_DIR` environment variable
2. `DAEMONCLAW_WORKSPACE` environment variable
3. `~/.daemonclaw/active_workspace.toml` marker file
4. `~/.daemonclaw/config.toml` (default)
