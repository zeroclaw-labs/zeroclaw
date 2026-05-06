cli-about = The fastest, smallest AI assistant.
cli-no-command-provided = No command provided.
cli-try-onboard = Try `zeroclaw onboard` to initialize your workspace.

cli-onboard-about = Initialize your workspace and configuration
cli-agent-about = Start the AI agent loop
cli-gateway-about = Manage the gateway server (webhooks, websockets)
cli-acp-about = Start the ACP server (JSON-RPC 2.0 over stdio)
cli-daemon-about = Start the long-running autonomous daemon
cli-service-about = Manage OS service lifecycle (launchd/systemd user service)
cli-doctor-about = Run diagnostics for daemon/scheduler/channel freshness
cli-status-about = Show system status (full details)
cli-estop-about = Engage, inspect, and resume emergency-stop states
cli-cron-about = Configure and manage scheduled tasks
cli-models-about = Manage provider model catalogs
cli-providers-about = List supported AI providers
cli-channel-about = Manage communication channels
cli-integrations-about = Browse 50+ integrations
cli-skills-about = Manage skills (user-defined capabilities)
cli-sop-about = Manage standard operating procedures (SOPs)
cli-migrate-about = Migrate data from other agent runtimes
cli-auth-about = Manage provider subscription authentication profiles
cli-hardware-about = Discover and introspect USB hardware
cli-peripheral-about = Manage hardware peripherals
cli-memory-about = Manage agent memory entries
cli-config-about = Manage ZeroClaw configuration
cli-update-about = Check for and apply ZeroClaw updates
cli-self-test-about = Run diagnostic self-tests
cli-completions-about = Generate shell completion scripts
cli-desktop-about = Launch the ZeroClaw companion desktop app

cli-config-schema-about = Dump the full configuration JSON Schema to stdout
cli-config-list-about = List all config properties with current values
cli-config-get-about = Get a config property value
cli-config-set-about = Set a config property (secret fields auto-prompt for masked input)
cli-config-init-about = Initialize unconfigured sections with defaults (enabled=false)
cli-config-migrate-about = Migrate config.toml to the current schema version on disk (preserves comments)

cli-service-install-about = Install daemon service unit for auto-start and restart
cli-service-start-about = Start daemon service
cli-service-stop-about = Stop daemon service
cli-service-restart-about = Restart daemon service to apply latest config
cli-service-status-about = Check daemon service status
cli-service-uninstall-about = Uninstall daemon service unit
cli-service-logs-about = Tail daemon service logs

cli-channel-list-about = List all configured channels
cli-channel-start-about = Start all configured channels
cli-channel-doctor-about = Run health checks for configured channels
cli-channel-add-about = Add a new channel configuration
cli-channel-remove-about = Remove a channel configuration
cli-channel-send-about = Send a one-off message to a configured channel
cli-wechat-pairing-required = 🔐 WeChat pairing required. One-time bind code: {$code}
cli-wechat-send-bind-command = Send `{$command} <code>` from your WeChat.
cli-wechat-qr-login = 📱 WeChat QR Login ({$attempt}/{$max})
cli-wechat-scan-to-connect = Scan with WeChat to connect.
cli-wechat-qr-url = QR URL: {$url}
cli-wechat-qr-expired-giving-up = WeChat QR code expired {$max} times, giving up.
cli-wechat-qr-fetch-failed = Failed to fetch WeChat QR code.
cli-wechat-qr-fetch-status-failed = WeChat QR code fetch failed ({$status}): {$body}
cli-wechat-missing-response-field = Missing {$field} in WeChat response.
cli-wechat-scanned-confirm = 👀 Scanned! Confirm on your phone...
cli-wechat-qr-expired-refreshing = ⏳ QR code expired, refreshing...
cli-wechat-login-confirmed-missing-field = Login confirmed but {$field} missing.
cli-wechat-connected = ✅ WeChat connected!
cli-wechat-bound-success = ✅ WeChat account bound successfully. You can talk to ZeroClaw now.
cli-wechat-invalid-bind-code = ❌ Invalid bind code. Please try again.

cli-skills-list-about = List all installed skills
cli-skills-audit-about = Audit a skill source directory or installed skill name
cli-skills-install-about = Install a new skill from a URL or local path
cli-skills-remove-about = Remove an installed skill
cli-skills-test-about = Run TEST.sh validation for a skill (or all skills)

cli-cron-list-about = List all scheduled tasks
cli-cron-add-about = Add a new recurring scheduled task
cli-cron-add-at-about = Add a one-shot task that fires at a specific UTC timestamp
cli-cron-add-every-about = Add a task that repeats at a fixed interval
cli-cron-once-about = Add a one-shot task that fires after a delay from now
cli-cron-remove-about = Remove a scheduled task
cli-cron-update-about = Update one or more fields of an existing scheduled task
cli-cron-pause-about = Pause a scheduled task
cli-cron-resume-about = Resume a paused task

cli-auth-login-about = Login with OAuth (OpenAI Codex or Gemini)
cli-auth-refresh-about = Refresh OpenAI Codex access token using refresh token
cli-auth-logout-about = Remove auth profile
cli-auth-use-about = Set active profile for a provider
cli-auth-list-about = List auth profiles
cli-auth-status-about = Show auth status with active profile and token expiry info

cli-memory-list-about = List memory entries with optional filters
cli-memory-get-about = Get a specific memory entry by key
cli-memory-stats-about = Show memory backend statistics and health
cli-memory-clear-about = Clear memories by category, by key, or clear all

cli-estop-status-about = Print current estop status
cli-estop-resume-about = Resume from an engaged estop level

cli-models-refresh-about = Refresh and cache provider models
cli-models-list-about = List cached models for a provider
cli-models-set-about = Set the default model in config
cli-models-status-about = Show current model configuration and cache status

cli-doctor-models-about = Probe model catalogs across providers and report availability
cli-doctor-traces-about = Query runtime trace events (tool diagnostics and model replies)

cli-hardware-discover-about = Enumerate USB devices and show known boards
cli-hardware-introspect-about = Introspect a device by its serial or device path
cli-hardware-info-about = Get chip info via USB using probe-rs over ST-Link

cli-peripheral-list-about = List configured peripherals
cli-peripheral-add-about = Add a peripheral by board type and transport path
cli-peripheral-flash-about = Flash ZeroClaw firmware to an Arduino board

cli-sop-list-about = List loaded SOPs
cli-sop-validate-about = Validate SOP definitions
cli-sop-show-about = Show details of an SOP

cli-migrate-openclaw-about = Import memory from an OpenClaw workspace into this ZeroClaw workspace

cli-agent-long-about =
    Start the AI agent loop.

    Launches an interactive chat session with the configured AI provider. Use --message for single-shot queries without entering interactive mode.

    Examples:
      zeroclaw agent                              # interactive session
      zeroclaw agent -m "Summarize today's logs"  # single message
      zeroclaw agent -p anthropic --model claude-sonnet-4-20250514
      zeroclaw agent --peripheral nucleo-f401re:/dev/ttyACM0

cli-gateway-long-about =
    Manage the gateway server (webhooks, websockets).

    Start, restart, or inspect the HTTP/WebSocket gateway that accepts incoming webhook events and WebSocket connections.

    Examples:
      zeroclaw gateway start              # start gateway
      zeroclaw gateway restart            # restart gateway
      zeroclaw gateway get-paircode       # show pairing code

cli-acp-long-about =
    Start the ACP server (JSON-RPC 2.0 over stdio).

    Launches a JSON-RPC 2.0 server on stdin/stdout for IDE and tool integration. Supports session management and streaming agent responses as notifications.

    Methods: initialize, session/new, session/prompt, session/stop.

    Examples:
      zeroclaw acp                        # start ACP server
      zeroclaw acp --max-sessions 5       # limit concurrent sessions

cli-daemon-long-about =
    Start the long-running autonomous daemon.

    Launches the full ZeroClaw runtime: gateway server, all configured channels (Telegram, Discord, Slack, etc.), heartbeat monitor, and the cron scheduler. This is the recommended way to run ZeroClaw in production or as an always-on assistant.

    Use 'zeroclaw service install' to register the daemon as an OS service (systemd/launchd) for auto-start on boot.

    Examples:
      zeroclaw daemon                   # use config defaults
      zeroclaw daemon -p 9090           # gateway on port 9090
      zeroclaw daemon --host 127.0.0.1  # localhost only

cli-cron-long-about =
    Configure and manage scheduled tasks.

    Schedule recurring, one-shot, or interval-based tasks using cron expressions, RFC 3339 timestamps, durations, or fixed intervals.

    Cron expressions use the standard 5-field format: 'min hour day month weekday'. Timezones default to UTC; override with --tz and an IANA timezone name.

    Examples:
      zeroclaw cron list
      zeroclaw cron add '0 9 * * 1-5' 'Good morning' --tz America/New_York --agent
      zeroclaw cron add '*/30 * * * *' 'Check system health' --agent
      zeroclaw cron add '*/5 * * * *' 'echo ok'
      zeroclaw cron add-at 2025-01-15T14:00:00Z 'Send reminder' --agent
      zeroclaw cron add-every 60000 'Ping heartbeat'
      zeroclaw cron once 30m 'Run backup in 30 minutes' --agent
      zeroclaw cron pause TASK_ID
      zeroclaw cron update TASK_ID --expression '0 8 * * *' --tz Europe/London

cli-channel-long-about =
    Manage communication channels.

    Add, remove, list, send, and health-check channels that connect ZeroClaw to messaging platforms. Supported channel types: telegram, discord, slack, whatsapp, matrix, imessage, email.

    Examples:
      zeroclaw channel list
      zeroclaw channel doctor
      zeroclaw channel add telegram '{ "{" }"bot_token":"...","name":"my-bot"{ "}" }'
      zeroclaw channel remove my-bot
      zeroclaw channel bind-telegram zeroclaw_user
      zeroclaw channel send 'Alert!' --channel-id telegram --recipient 123456789

cli-hardware-long-about =
    Discover and introspect USB hardware.

    Enumerate connected USB devices, identify known development boards (STM32 Nucleo, Arduino, ESP32), and retrieve chip information via probe-rs / ST-Link.

    Examples:
      zeroclaw hardware discover
      zeroclaw hardware introspect /dev/ttyACM0
      zeroclaw hardware info --chip STM32F401RETx

cli-peripheral-long-about =
    Manage hardware peripherals.

    Add, list, flash, and configure hardware boards that expose tools to the agent (GPIO, sensors, actuators). Supported boards: nucleo-f401re, rpi-gpio, esp32, arduino-uno.

    Examples:
      zeroclaw peripheral list
      zeroclaw peripheral add nucleo-f401re /dev/ttyACM0
      zeroclaw peripheral add rpi-gpio native
      zeroclaw peripheral flash --port /dev/cu.usbmodem12345
      zeroclaw peripheral flash-nucleo

cli-memory-long-about =
    Manage agent memory entries.

    List, inspect, and clear memory entries stored by the agent. Supports filtering by category and session, pagination, and batch clearing with confirmation.

    Examples:
      zeroclaw memory stats
      zeroclaw memory list
      zeroclaw memory list --category core --limit 10
      zeroclaw memory get KEY
      zeroclaw memory clear --category conversation --yes

cli-config-long-about =
    Manage ZeroClaw configuration.

    View, set, or initialize config properties by dotted path. Use 'schema' to dump the full JSON Schema for the config file.

    Properties are addressed by dotted path (e.g. channels.matrix.mention-only).
    Secret fields (API keys, tokens) automatically use masked input.
    Enum fields offer interactive selection when value is omitted.

    Examples:
      zeroclaw config list                                  # list all properties
      zeroclaw config list --secrets                        # list only secrets
      zeroclaw config list --filter channels.matrix         # filter by prefix
      zeroclaw config get channels.matrix.mention-only      # get a value
      zeroclaw config set channels.matrix.mention-only true # set a value
      zeroclaw config set channels.matrix.access-token      # secret: masked input
      zeroclaw config set channels.matrix.stream-mode       # enum: interactive select
      zeroclaw config init channels.matrix                  # init section with defaults
      zeroclaw config schema                                # print JSON Schema to stdout
      zeroclaw config schema > schema.json

    Property path tab completion is included automatically in `zeroclaw completions <shell>`.

cli-update-long-about =
    Check for and apply ZeroClaw updates.

    By default, downloads and installs the latest release with a 6-phase pipeline: preflight, download, backup, validate, swap, and smoke test. Automatic rollback on failure.

    Use --check to only check for updates without installing.
    Use --force to skip the confirmation prompt.
    Use --version to target a specific release instead of latest.

    Examples:
      zeroclaw update                      # download and install latest
      zeroclaw update --check              # check only, don't install
      zeroclaw update --force              # install without confirmation
      zeroclaw update --version 0.6.0      # install specific version

cli-self-test-long-about =
    Run diagnostic self-tests to verify the ZeroClaw installation.

    By default, runs the full test suite including network checks (gateway health, memory round-trip). Use --quick to skip network checks for faster offline validation.

    Examples:
      zeroclaw self-test             # full suite
      zeroclaw self-test --quick     # quick checks only (no network)

cli-completions-long-about =
    Generate shell completion scripts for `zeroclaw`.

    The script is printed to stdout so it can be sourced directly:

    Examples:
      source <(zeroclaw completions bash)
      zeroclaw completions zsh > ~/.zfunc/_zeroclaw
      zeroclaw completions fish > ~/.config/fish/completions/zeroclaw.fish

cli-desktop-long-about =
    Launch the ZeroClaw companion desktop app.

    The companion app is a lightweight menu bar / system tray application that connects to the same gateway as the CLI. It provides quick access to the dashboard, status monitoring, and device pairing.

    Use --install to download the pre-built companion app for your platform.

    Examples:
      zeroclaw desktop              # launch the companion app
      zeroclaw desktop --install    # download and install it
