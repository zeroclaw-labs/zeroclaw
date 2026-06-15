cli-about = The fastest, smallest AI assistant.
cli-no-command-provided = No command provided.
cli-try-quickstart = Try `zeroclaw quickstart` to create your first agent.

cli-quickstart-about = Create your first agent end-to-end
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
cli-skills-review-summary = { "  " }💾 Skill review: {$summary}
cli-skills-install-start = Installing skill from: {$source}
cli-skills-install-resolving-registry = { "  " }Resolving '{$source}' from skills registry...
cli-skills-install-installed-audited = { "  " }{$status} Skill installed and audited: {$path} ({$files} files scanned)
cli-skills-install-security-audit-completed = { "  " }Security audit completed successfully.
cli-skills-install-tier-official = Installing {$name} v{$version} — Official (zeroclaw-labs maintained)
cli-skills-install-tier-community =
    Installing {$name} v{$version} — Community submission
    This skill is not audited by ZeroClaw. Review the skill content
    and run `zeroclaw skills audit {$name}` before granting any
    permissions or running it in production.

cli-skills-add-scaffolded = Scaffolded skill {$target} at {$dir}

cli-skills-bundle-add-prompt =
    To create skill-bundle '{$alias}' with directory '{$dir}', run:
      zeroclaw config map-key skill-bundles {$alias}
      zeroclaw config set skill-bundles.{$alias}.directory {$dir}

    (Direct bundle creation through `zeroclaw skills bundle add` would duplicate the config mutation surface.)

cli-skills-bundle-remove-prompt =
    To remove skill-bundle '{$alias}', run:
      zeroclaw config map-key-delete skill-bundles {$alias}

    (Removes the config entry; the bundle's directory on disk is left in place.)

cli-skills-bundle-list-empty =
    No skill bundles configured.
      Create one: zeroclaw config set skill-bundles.default.directory shared/skills/default
cli-skills-bundle-list-header = Skill bundles ({$count}):
cli-skills-bundle-entry = {$alias} -> {$dir}
cli-skills-bundle-include = include: {$values}
cli-skills-bundle-exclude = exclude: {$values}
cli-skills-bundle-show-no-skills = (no skills installed)
cli-skills-bundle-show-skills-header = skills ({$count}):
cli-skills-bundle-show-skill = {$name}: {$description}

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
cli-memory-clear-unsupported-backend = memory clear is unsupported for append-only backend '{$backend}'; switch to a deletable backend (sqlite, lucid, or postgres)

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

cli-skills-install-suggestion =
    It looks like this request needs the `{$name}` skill, but it is not installed.

    Matched capability: {$matched}
    Next: Run `{$install_command}` to install it.

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

# Channel-side reply emitted when chat dispatch refuses because the
# gateway has no model configured. Used by the gateway crate channel
# webhook handlers (WhatsApp, Linq, WATI, Nextcloud Talk).
channel-needs-quickstart-reply = This agent isn't fully set up yet. The operator needs to run Quickstart before I can reply.

channel-whatsapp-web-feature-missing-warning =   ⚠ WhatsApp Web is configured but the 'whatsapp-web' feature is not compiled in.
channel-whatsapp-web-feature-missing-build =     Build/run with: cargo build --features whatsapp-web
channel-whatsapp-web-feature-missing-install =     If installed to PATH, reinstall with: cargo install --path . --force --locked --features whatsapp-web
channel-whatsapp-web-feature-missing-error = WhatsApp Web channel requires the 'whatsapp-web' feature. Enable with: cargo build --features whatsapp-web (or, if installed to PATH: cargo install --path . --force --locked --features whatsapp-web)

channel-wecom-ws-stream-bootstrap = Working on it, please wait.
channel-wecom-ws-stop-ack = Stopped the current message.
channel-wecom-ws-voice-unavailable = I can't process voice messages right now {$emoji}
channel-wecom-ws-unsupported-message = This message type is not supported yet.
channel-wecom-ws-welcome = Hi, welcome to chat with me {$emoji}
channel-wecom-ws-supplemental-message =
    {"["}Supplemental message]
    {$extra}
channel-wecom-ws-group-allowlist-missing =
    The WeCom allowlist is not configured, so this bot is not accepting group messages.

    Group chatid: {$chatid}
    Sender userid: {$userid}

    Add an allowed entry to {$allowed_groups_path} or {$allowed_users_path}. You can also temporarily set it to ["*"] for testing.
channel-wecom-ws-group-access-denied =
    This group is not allowed to use this bot.

    Group chatid: {$chatid}
    Sender userid: {$userid}

    Ask an administrator to add this group to {$allowed_groups_path}, or add your userid to {$allowed_users_path}.
channel-wecom-ws-dm-allowlist-missing =
    The WeCom allowlist is not configured, so this bot is not accepting messages.

    Your userid: {$userid}

    Add an allowed entry to {$allowed_users_path}. You can also temporarily set it to ["*"] for testing.
channel-wecom-ws-dm-access-denied =
    You do not have permission to use this bot.

    Your userid: {$userid}

    Ask an administrator to add your userid to {$allowed_users_path}.
channel-discord-interaction-unauthorized = You're not authorized to use this command here.
channel-discord-interaction-malformed = Unknown or malformed command.
channel-discord-delivery-failure-note-one = (note: I couldn't deliver {$count} file.)
channel-discord-delivery-failure-note-many = (note: I couldn't deliver {$count} files.)

# Onboarding — OpenAI auth picker
onboard-openai-auth-note =
    OpenAI authentication:
    • API key — standard API access via platform.openai.com (sk-...)
    • Codex subscription — uses your ChatGPT Plus/Pro account (no API key needed)
onboard-openai-auth-prompt = Authentication
onboard-openai-auth-api-key = API key
onboard-openai-auth-codex = Codex subscription
onboard-openai-codex-followup =
    Codex subscription auth uses your ChatGPT account.
    Run `zeroclaw auth login --model-provider openai-codex` to authenticate before starting your agent.

# Diagnostics emitted by `zeroclaw doctor` and `zeroclaw self-test` for
# `gateway.web_dist_dir` values that rely on shell-style expansion the
# gateway never performs (a leading `~` or any `$VAR` / `${VAR}`).
# Issue #6079; companion runtime check in
# `crates/zeroclaw-runtime/src/doctor/mod.rs` and `src/commands/self_test.rs`.
cli-web-dist-dir-reason-tilde = starts with `~` which is not expanded
cli-web-dist-dir-reason-dollar = contains `$` which is not expanded
cli-doctor-web-dist-dir-expansion-warning = gateway.web_dist_dir = "{$path}" — {$reason}; gateway.web_dist_dir is read verbatim, so expand the value yourself (e.g. an absolute path)
cli-self-test-web-dist-dir-name = web_dist_dir
cli-self-test-web-dist-dir-pass-unset = not set (using auto-detect)
cli-self-test-web-dist-dir-pass-literal = {$path} (literal path)
cli-self-test-web-dist-dir-fail-expansion = WARNING: {$path} — {$reason}; gateway.web_dist_dir is read verbatim, so expand the value yourself (e.g. an absolute path)

# ── peripherals (zeroclaw peripheral) ──
cli-peripherals-none = No peripherals configured.
cli-peripherals-add-hint = Add one with: zeroclaw peripheral add <board> <path>
cli-peripherals-add-example = {"  "}Example: zeroclaw peripheral add nucleo-f401re <serial-path>
cli-peripherals-config-hint = Or add to config.toml:
cli-peripherals-configured = Configured peripherals:
cli-peripherals-already-configured = Board {$board} at {$path} already configured.
cli-peripherals-added = Added {$board} at {$path}. Restart daemon to apply.
cli-peripherals-flash-needs-hardware = Arduino flash requires the 'hardware' feature.
cli-peripherals-unoq-needs-hardware = Uno Q setup requires the 'hardware' feature.
cli-peripherals-nucleo-needs-hardware = Nucleo flash requires the 'hardware' feature.

# ── skills (zeroclaw skills list) ──
cli-skills-none-installed = No skills installed.
cli-skills-create-hint = {"  "}Create one: mkdir -p ~/.zeroclaw/workspace/skills/my-skill
cli-skills-install-hint = {"  "}Or install: zeroclaw skills install <source>
cli-skills-installed-header = Installed skills ({$count}):
cli-skills-tags = Tags:  {$tags}

# ── sop (zeroclaw sop) ──
cli-sop-none = No SOPs found.
cli-sop-create-hint = {"  "}Create one: mkdir -p <workspace>/sops/my-sop
cli-sop-create-hint-2 = {"              "}then add SOP.toml and SOP.md
cli-sop-loaded-header = Loaded SOPs ({$count}):
cli-sop-none-to-validate = No SOPs found to validate.
cli-sop-valid = ✅ {$name} — valid
cli-sop-warnings = ⚠️  {$name} — {$count} warning(s):
cli-sop-all-passed = All SOPs passed validation.
cli-sop-priority = {"  "}Priority:       {$value}
cli-sop-execution-mode = {"  "}Execution mode: {$value}
cli-sop-deterministic = {"  "}Deterministic:  {$value}
cli-sop-cooldown = {"  "}Cooldown:       {$value}s
cli-sop-max-concurrent = {"  "}Max concurrent: {$value}
cli-sop-location = {"  "}Location:       {$value}
cli-sop-triggers = {"  "}Triggers:
cli-sop-steps = {"  "}Steps:
cli-sop-step-tools = Tools: {$tools}

# ── memory (zeroclaw memory) ──
cli-memory-reindexing = Reindexing memory backend...
cli-memory-none = No memory entries found.
cli-memory-none-at-offset = No entries at offset {$offset} (total: {$total}).
cli-memory-next-page = Use --offset {$offset} to see the next page.
cli-memory-key-not-found = No memory entry found for key: {$key}
cli-memory-prefix-matched = Prefix '{$key}' matched {$n} entries:
cli-memory-narrow-prefix = Specify a longer prefix to narrow the match.
cli-memory-key = Key:       {$value}
cli-memory-category = Category:  {$value}
cli-memory-timestamp = Timestamp: {$value}
cli-memory-session = Session:   {$value}
cli-memory-stats-header = Memory Statistics:
cli-memory-backend = {"  "}Backend:  {$value}
cli-memory-total = {"  "}Total:    {$value}
cli-memory-by-category = {"  "}By category:
cli-memory-none-to-clear = No entries to clear.
cli-memory-found-in-scope = Found {$count} entries in '{$scope}'.
cli-memory-aborted = Aborted.
cli-memory-deleted-key = Deleted key: {$key}

# ── cron (zeroclaw cron) ──
cli-cron-none = No scheduled tasks yet.
cli-cron-usage = Usage:
cli-cron-jobs-header = 🕒 Scheduled jobs ({$count}):
cli-cron-list-cmd = {"    "}cmd: {$cmd}
cli-cron-list-prompt = {"    "}prompt: {$prompt}
cli-cron-added-agent = ✅ Added agent cron job {$id}
cli-cron-added = ✅ Added cron job {$id}
cli-cron-added-oneshot-agent = ✅ Added one-shot agent cron job {$id}
cli-cron-added-oneshot = ✅ Added one-shot cron job {$id}
cli-cron-added-interval-agent = ✅ Added interval agent cron job {$id}
cli-cron-added-interval = ✅ Added interval cron job {$id}
cli-cron-updated = ✅ Updated cron job {$id}
cli-cron-paused = ⏸️  Paused cron job {$id}
cli-cron-resumed = ▶️  Resumed cron job {$id}
cli-cron-expr = {"  "}Expr  : {$v}
cli-cron-expr2 = {"  "}Expr: {$v}
cli-cron-next = {"  "}Next  : {$v}
cli-cron-next2 = {"  "}Next: {$v}
cli-cron-next3 = {"  "}Next     : {$v}
cli-cron-prompt = {"  "}Prompt: {$v}
cli-cron-prompt3 = {"  "}Prompt   : {$v}
cli-cron-cmd = {"  "}Cmd : {$v}
cli-cron-cmd3 = {"  "}Cmd      : {$v}
cli-cron-at = {"  "}At    : {$v}
cli-cron-at2 = {"  "}At  : {$v}
cli-cron-every = {"  "}Every(ms): {$v}

# ── main / status / quickstart / pairing / desktop ──
cli-no-command = No command provided.
cli-press-enter = Press Enter to exit...
cli-quickstart-title = Quickstart — create one working agent end-to-end.
cli-quickstart-needs-tty = Quickstart is interactive and needs a terminal on stdin and stderr. Run it from an interactive shell, or use `zeroclaw config set <path> <value>` for headless configuration.
cli-quickstart-cancelled = Quickstart cancelled. No config written.
cli-quickstart-incomplete = {"  "}Not all selectors are filled yet.
cli-quickstart-create-agent = ── Create agent
cli-quickstart-create-agent-locked = ── Create agent (locked — fill every selector first)
cli-quickstart-open-selector-prompt = Open a selector (Enter), or pick Create. Esc to quit.
cli-quickstart-use-existing = Use existing
cli-quickstart-create-new = Create new
cli-quickstart-model-provider-prompt = Model provider
cli-quickstart-pick-configured-provider = Pick a configured provider
cli-no-channels-compiled = {"  "}No channel types are compiled into this binary.
cli-quickstart-complete = Quickstart complete. Created agent `{$alias}`.
cli-next-steps = Next steps:
cli-agent-not-created = Your agent was not created — and nothing on disk was changed.
cli-onboard-deprecated = `zeroclaw onboard` is deprecated — use `zeroclaw quickstart`.
cli-otp-initialized = Initialized OTP secret for ZeroClaw.
cli-otp-enrollment-uri = Enrollment URI: {$uri}
cli-pairing-enabled = 🔐 Gateway pairing is enabled.
cli-pairing-use-code = {"  "}Use this one-time code to pair a new device:
cli-pairing-post = {"    "}POST /pair with header X-Pairing-Code: {$code}
cli-pairing-restart = {"   "}Restart the gateway to generate a new pairing code.
cli-pairing-disabled = ⚠️  Gateway pairing is disabled in config.
cli-gateway-running-q = {"   "}Is the gateway running? Start it with:
cli-status-title = 🦀 ZeroClaw Status
cli-status-provider-none = 🤖 ModelProvider:      (none configured)
cli-status-agents-none = 🛡️  Agents:        (none configured)
cli-status-service-running = 🟢 Service:       running
cli-status-service-stopped = 🔴 Service:       stopped
cli-status-channels = Channels:
cli-status-cli-always = {"  "}CLI:      ✅ always
cli-status-peripherals = Peripherals:
cli-desktop-download = Download the ZeroClaw companion app:
cli-desktop-homebrew = Or install via Homebrew (coming soon):
cli-desktop-linux-pkg = {"  "}Download the .deb or .AppImage for your architecture.
cli-desktop-launching = Launching ZeroClaw companion app...

# ── status fields ──
cli-status-version = Version:     {$v}
cli-status-workspace = Workspace:   {$v}
cli-status-config = Config:      {$v}
cli-status-provider-indent = {"   "}ModelProvider:      {$family}.{$alias}
cli-status-provider = 🤖 ModelProvider:      {$family}.{$alias}
cli-status-model = {"   "}Model:         {$model}
cli-status-observability = 📊 Observability:  {$v}
cli-status-agents = 🛡️  Agents:        {$v}
cli-status-runtime = ⚙️  Runtime:       {$v}
cli-status-security-noprofile = Security ({$alias}): <no risk_profile>
cli-status-security = Security ({$alias}):
cli-status-workspace-only = {"  "}Workspace only:    {$v}
cli-status-max-actions = {"  "}Max actions/hour:  {$v}
cli-status-max-cost-day = {"  "}Max cost/day:      ${$v}
cli-status-max-cost-month = {"  "}Max cost/month:    ${$v}
cli-status-otp = {"  "}OTP enabled:       {$v}
cli-status-estop = {"  "}E-stop enabled:    {$v}
cli-status-boards = {"  "}Boards:    {$v}

# ── desktop / config / plugins / estop / auth ──
cli-desktop-not-installed = ZeroClaw companion app is not installed.
cli-desktop-blurb1 = The companion app is a lightweight menu bar app that
cli-desktop-blurb2 = connects to the same gateway as the CLI.
cli-config-all-configured = All sections already configured.
cli-config-schema-current = Config already at current schema version.
cli-config-applied-ops = Applied {$count} operation(s):
cli-plugins-none = No plugins installed.
cli-plugins-installed = Installed plugins:
cli-plugin-installed-from = Plugin installed from {$source}
cli-plugin-removed = Plugin '{$name}' removed.
cli-plugin-not-found = Plugin '{$name}' not found.
cli-plugin-legacy-detected = Note: plugins in a legacy location ({$path}) are not loaded by the agent — run `zeroclaw plugin migrate` to move them into {$target}.
cli-plugin-migrated = Moved {$count} plugin(s) from {$path} to {$target}.
cli-plugin-migrate-none = Nothing to migrate.
cli-estop-resume-done = Estop resume completed.
cli-estop-engaged = Estop engaged.
cli-estop-status = Estop status:
cli-auth-none = No auth profiles configured.
cli-auth-active = Active profiles:

# ── misc main (errors, config, plugin info, estop fields, auth) ──
cli-warn-crypto-provider = Warning: Failed to install default crypto provider: {$err}
cli-error-label = {"   "}Error: {$err}
cli-warn-cost-usage = {"  "}⚠ Could not load cost usage: {$err}
cli-warn-cost-tracker = {"  "}⚠ Could not init cost tracker: {$err}
cli-desktop-download-at = {"  "}Download it at: {$url}
cli-config-legend = Legend: 💉 env-overridden  🔒 secret
cli-config-secret-set = {$path} is set (encrypted secret — value not displayed)
cli-config-secret-unset = {$path} is not set (encrypted secret)
cli-config-updated = {$path} updated.
cli-config-review-hint = Run `zeroclaw config list` to review, then set required fields.
cli-config-backed-up = Backed up to {$path}
cli-plugin-name-version = Plugin: {$name} v{$version}
cli-plugin-description = Description: {$desc}
cli-plugin-capabilities = Capabilities: {$v}
cli-plugin-permissions = Permissions: {$v}
cli-plugin-wasm = WASM: {$path}
cli-plugin-wasm-none = WASM: (skill-only plugin)
cli-estop-domains-none = {"  "}domain_blocks:  (none)
cli-estop-domains = {"  "}domain_blocks:  {$v}
cli-estop-tools-none = {"  "}tool_freeze:    (none)
cli-estop-tools = {"  "}tool_freeze:    {$v}
cli-estop-updated-at = {"  "}updated_at:     {$v}
cli-auth-saved = Saved profile {$profile}
cli-auth-active-for = Active profile for {$provider}: {$profile}
cli-auth-refresh-ok = ✓ Token refresh OK (profile {$profile})
cli-auth-removed = Removed auth profile {$provider}:{$profile}
cli-auth-not-found = Auth profile not found: {$provider}:{$profile}

# ── locales fetch ──
cli-locales-fetched = {"  "}fetched {$name} -> {$path}
cli-locales-skipped = {"  "}skipped {$name}: not on upstream ({$path}; tried {$refs})
cli-locales-installed = Installed {$count} catalogue(s) for '{$locale}' under {$dir}

# ── browse (zeroclaw browse) ──
cli-browse-header = {$path} ({$count} entries)
cli-browse-empty = (empty)
cli-browse-file-bytes = {$name} ({$bytes} bytes)

# ── hardware (zeroclaw hardware) ──
cli-hardware-feature-required = Hardware discovery requires the 'hardware' feature.
cli-hardware-feature-build = Build with: cargo build --features hardware
cli-hardware-unsupported-platform = Hardware USB discovery is not supported on this platform.
cli-hardware-supported-platforms = Supported platforms: Linux, macOS, Windows.

# ── update (zeroclaw update) ──
cli-update-already-current = Already up to date (v{$version}).
cli-update-success = Successfully updated to v{$version}!

# ── self-test (zeroclaw self-test) ──
cli-selftest-all-passed = All {$total} checks passed.
cli-selftest-some-failed = {$failed}/{$total} checks failed.

# ── channels (zeroclaw channel list) ──
cli-channels-header = Channels:
cli-channels-cli-always = {"  "}✅ CLI (always available)
cli-channels-notion = {"  "}{$status} Notion
cli-channels-start-hint = To start channels: zeroclaw channel start
cli-channels-doctor-hint = To check health:    zeroclaw channel doctor
cli-channels-configure-hint = To configure:      zeroclaw config set channels.<name>.<field>=<value>

# ── Agent turn-engine user-visible markers (#7415) ────────────────────
# Appended to (or persisted as) assistant output when a turn is cut short;
# shown to end users across every transport (channels, WS, RPC, ACP, CLI).
turn-interrupted-by-user = [interrupted by user]
turn-stream-interrupted = [stream interrupted]
turn-tool-interrupted-before-result = [interrupted by user before this tool produced a result]
# Safe reply delivered when the model repeatedly emits malformed internal
# tool-call protocol and the turn gives up retrying.
channel-runtime-malformed-tool-output = I generated an internal tool-call format error and could not complete this request. Please try again.
