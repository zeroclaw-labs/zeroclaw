cli-about = The fastest, smallest AI assistant.
cli-no-command-provided = No command provided.
cli-try-quickstart = Try `zeroclaw quickstart` to create your first agent.

cli-quickstart-about = Create your first agent end-to-end
cli-agent-about = Start the AI agent loop
cli-chat-about = Chat with an agent through the gateway
cli-gateway-about = Manage the gateway server (webhooks, websockets)
cli-acp-about = Start the ACP server (JSON-RPC 2.0 over stdio)
cli-daemon-about = Start the long-running autonomous daemon
cli-service-about = Manage OS service lifecycle (launchd/systemd user service)
cli-doctor-about = Run diagnostics for daemon/scheduler/channel freshness
cli-status-about = Show system status (full details)
cli-status-long-about =
    Show system status (full details).

    When companion memory is enabled and companion-memory.db already exists,
    this command opens that file with deny-migration and never creates a
    missing store. Opening is not a strictly read-only inspect: SQLite takes
    a WAL lock, and the probe still applies owner-only permissions (0600)
    to the database file and any existing -wal/-shm sidecars.

    --format exit-code exits 0 if the gateway /health probe succeeds, 1 otherwise
    (for Docker HEALTHCHECK).
cli-estop-about = Engage, inspect, and resume emergency-stop states
cli-cron-about = Configure and manage scheduled tasks
cli-models-about = Manage provider model catalogs
cli-providers-about = List supported AI providers
cli-channel-about = Manage communication channels
cli-integrations-about = Browse 50+ integrations
cli-skills-about = Manage skills (user-defined capabilities)
cli-migrate-about = Migrate data from other agent runtimes
cli-auth-about = Manage provider subscription authentication profiles
cli-memory-about = Manage agent memory entries
cli-config-about = Manage ZeroClaw configuration
cli-update-about = Update this fork from source
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

cli-skills-list-about = List all installed skills
cli-skills-audit-about = Audit a skill source directory or installed skill name
cli-skills-audit-failed = Skill audit failed.
cli-skills-install-about = Install a new skill from a URL or local path
cli-skills-remove-about = Remove an installed skill
cli-skills-test-about = Run TEST.sh validation for a skill (or all skills)
cli-skills-review-summary = { "  " }💾 Skill review: {$summary}
cli-skills-install-start = Installing skill from: {$source}
cli-skills-install-resolving-registry = { "  " }Resolving '{$source}' from skills registry...
cli-skills-install-resolving-extra-registry = { "  " }Resolving '{$source}' from registry '{$registry}'...
cli-skills-install-skill-requires-git = --skill <name> requires a git repository URL as the source (got '{$source}')
cli-skills-install-catalog-failed = failed to install skill '{$skill}' from catalog {$source}
cli-skills-install-invalid-skill-name = invalid --skill name '{$skill}': use a bare skill name (letters, digits, '-', '_')
cli-skills-install-catalog-clone-failed = failed to clone skill catalog {$url}
cli-skills-install-skill-not-in-catalog-empty = skill '{$skill}' not found in {$url}: no skills/ directory, or it is empty
cli-skills-install-skill-not-in-catalog =
    skill '{$skill}' not found in {$url}.
    Available skills: {$available}
cli-skills-install-catalog-root-symlink = skill catalog {$url} has a symlinked skills/ directory; refusing to inspect it
cli-skills-install-catalog-root-escapes = skill catalog {$url} has a skills/ directory that resolves outside the cloned catalog; refusing to inspect it
cli-skills-install-catalog-skill-symlink = skill '{$skill}' in {$url} is a symlink; catalog skills must be real directories inside the repository
cli-skills-install-catalog-skill-escapes = skill '{$skill}' in {$url} resolves outside the cloned catalog; refusing to install
cli-skills-install-git-failed = failed to install git skill source: {$source}
cli-skills-install-registry-failed = failed to install skill from registry: {$source}
cli-skills-install-extra-registry-failed = failed to install skill from extra registry: {$source}
cli-skills-install-local-failed = failed to install local skill source: {$source}
cli-skills-install-installed-audited = { "  " }{$status} Skill installed and audited: {$path} ({$files} files scanned)
cli-skills-install-security-audit-completed = { "  " }Security audit completed successfully.
cli-skills-install-into-bundle = { "  " }Installed into bundle '{$alias}'. Agents that list this bundle in skill_bundles will load it.
cli-skills-install-global-note = { "  " }Note: installed into the global skills dir, which no agent loads automatically. Re-run with --bundle <alias>, or assign a bundle to an agent, to make it loadable.
cli-skills-removed-archived = { "  " }{$status} Skill '{$name}' removed from bundle '{$bundle}' (archived under shared/skills/_deleted/).
cli-skills-removed-global = { "  " }{$status} Skill '{$name}' removed from the global skills dir.
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

cli-auth-login-about = Login with OAuth (OpenAI Codex, Gemini, or xAI)
cli-auth-refresh-about = Refresh OAuth access token using refresh token
cli-auth-logout-about = Remove auth profile
cli-auth-use-about = Set active profile for a provider
cli-auth-list-about = List auth profiles
cli-auth-status-about = Show auth status with active profile and token expiry info

cli-memory-list-about = List memory entries with optional filters
cli-memory-migrate-kernel-about = Copy a recognized legacy kernel database to a new, verified database
cli-memory-kernel-feature-required = Kernel migration requires a build with the memory-tachi feature.
cli-memory-kernel-approval-required = Provide a non-empty operator identity with --approved-by.
cli-memory-kernel-platform-required = Offline kernel migration requires Unix file identity and owner-only permissions. Run it on a Unix system.
cli-memory-kernel-migration-complete = Verified migration complete. Review the receipt before switching database paths.
cli-memory-migrate-native-about = Convert a quiescent native brain.db database into a new, verified Tachi database
cli-memory-native-feature-required = Native brain migration requires a build with the memory-tachi feature.
cli-memory-native-approval-required = Provide a non-empty operator identity with --approved-by.
cli-memory-native-platform-required = Offline native brain migration requires Unix file identity and owner-only permissions. Run it on a Unix system.
cli-memory-native-migration-complete = Verified native brain migration complete. Review the receipt before switching database paths.
cli-memory-get-about = Get a specific memory entry by key
cli-memory-stats-about = Show memory backend statistics and health
cli-memory-clear-about = Clear memories by category, by key, or clear all
cli-memory-clear-unsupported-backend = memory clear is unsupported for append-only backend '{$backend}'; switch to a deletable backend (sqlite or lucid)

cli-estop-status-about = Print current estop status
cli-estop-resume-about = Resume from an engaged estop level

cli-models-refresh-about = Refresh and cache provider models
cli-models-list-about = List cached models for a provider
cli-models-set-about = Set the default model in config
cli-models-status-about = Show current model configuration and cache status

cli-doctor-models-about = Probe model catalogs across providers and report availability
cli-doctor-traces-about = Query runtime trace events (tool diagnostics and model replies)
cli-doctor-update-context-windows-about = Update context_window in config.toml from provider /models endpoints

cli-migrate-openclaw-about = Import memory from an OpenClaw workspace into this ZeroClaw workspace

cli-agent-long-about =
    Start the AI agent loop.

    With --message, runs one turn locally and prints the reply. Without it, opens `zeroclaw chat` for this agent on this machine's gateway, so the conversation is shared with every other client.

    Examples:
      zeroclaw agent -a assistant                              # chat through the gateway
      zeroclaw agent -a assistant -m "Summarize today's logs"  # single message
      zeroclaw agent -a assistant -m "Hi" -p anthropic --model claude-sonnet-4-20250514

    --model-provider, --model, and --temperature apply only with --message; the interactive chat uses the agent's configured model.

cli-agent-interactive-flags-need-message =
    { $flags } only apply with -m/--message. Interactive `zeroclaw agent` opens `zeroclaw chat`, which uses the model configured for agent "{ $agent }". Add -m "<message>" for a one-off turn with these overrides, or change the agent's configuration.

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

    Launches the full ZeroClaw runtime: gateway server, all configured channel surfaces (Telegram bridge, webhook ingress, Email/Discord tools), heartbeat monitor, and the cron scheduler. This is the recommended way to run ZeroClaw in production or as an always-on assistant.

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

    Add, remove, list, send, and health-check channels that connect ZeroClaw to messaging platforms. Live channel types: telegram, discord, email, webhook (other legacy sections are retired).

    Examples:
      zeroclaw channel list
      zeroclaw channel doctor
      zeroclaw channel add telegram '{ "{" }"bot_token":"...","name":"my-bot"{ "}" }'
      zeroclaw channel remove my-bot
      zeroclaw channel bind-telegram zeroclaw_user
      zeroclaw channel send 'Alert!' --channel-id telegram --recipient 123456789

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
    This fork has no verified binary update channel. Update a checkout of https://github.com/kckylechen1/zeroclaw and run ./install.sh --source with your existing preset/features. Upstream releases cannot update this fork.

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
# webhook handlers.
channel-needs-quickstart-reply = This agent isn't fully set up yet. The operator needs to run Quickstart before I can reply.


channel-discord-interaction-unauthorized = You're not authorized to use this command here.
channel-discord-interaction-malformed = Unknown or malformed command.
channel-discord-interaction-unavailable = That command is no longer available, or its input was empty.
channel-discord-component-expired = This button or menu has expired or was already used.
channel-discord-approval-recorded = Your decision has been recorded.
channel-discord-delivery-failure-note-one = (note: I couldn't deliver {$count} file.)
channel-discord-delivery-failure-note-many = (note: I couldn't deliver {$count} files.)
channel-line-bind-success = ✅ Paired! You can now chat.
channel-line-bind-invalid-code = ❌ Invalid code. Please try again.
channel-line-bind-rate-limited = ⏳ Too many attempts. Retry in { $secs }s.

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
# Diagnostics emitted by `zeroclaw doctor` cross-checking OpenAI Codex
# (OAuth/subscription) credentials against OpenAI provider slots that opt in
# via `requires_openai_auth = true`. A signed-in credential with no slot — or a
# slot with no credential — otherwise stays silent until the first model call,
# where it surfaces as a confusing auth error. Companion runtime check in
# `crates/zeroclaw-runtime/src/doctor/mod.rs`.
cli-doctor-codex-auth-profile-no-slot = OpenAI Codex credentials are signed in but no model provider slot uses them. Set `requires_openai_auth = true` on an OpenAI provider slot and point an agent's `model_provider` at it, or run `zeroclaw quickstart`.
cli-doctor-codex-auth-slot-no-profile = OpenAI slot(s) {$slots} set `requires_openai_auth = true` but no OpenAI Codex credentials are signed in. Run `zeroclaw auth login --provider openai-codex`.
cli-doctor-codex-auth-ok = OpenAI Codex credentials are signed in and referenced by a model provider slot.
cli-doctor-systemd-linger-enabled = systemd user lingering enabled
cli-doctor-systemd-linger-disabled = systemd user lingering disabled; user service may stop after logout. Enable with: loginctl enable-linger {$user}
cli-doctor-systemd-linger-unknown = systemd user lingering could not be checked with loginctl
cli-self-test-web-dist-dir-name = web_dist_dir
cli-self-test-web-dist-dir-pass-unset = not set (using auto-detect)
cli-self-test-web-dist-dir-pass-literal = {$path} (literal path)
cli-self-test-web-dist-dir-fail-expansion = WARNING: {$path} — {$reason}; gateway.web_dist_dir is read verbatim, so expand the value yourself (e.g. an absolute path)

# Service lifecycle warnings.
cli-service-systemd-linger-disabled-warning = systemd user lingering is disabled. ZeroClaw's user service may stop after logout. Enable it with: loginctl enable-linger {$user}

# ── skills (zeroclaw skills list) ──
cli-skills-none-installed = No skills installed.
cli-skills-create-hint = {"  "}Create one: zeroclaw skills add my-skill --bundle <alias>
cli-skills-install-hint = {"  "}Or install: zeroclaw skills install <source>
cli-skills-installed-header = Installed skills ({$count}):
cli-skills-list-group-bundle = bundle: {$alias}
cli-skills-list-group-agent = loaded by agent '{$alias}'
cli-skills-list-group-global = global / open-skills / plugins (not from a bundle)
cli-skills-agent-not-configured = agent '{$alias}' is not configured
cli-skills-agent-multiple-bundles = agent '{$alias}' has multiple skill bundles ({$bundles}); pass --bundle to choose one
cli-skills-multiple-locations-bundle = skill '{$name}' exists in multiple locations ({$locations}); pass --bundle to choose one
cli-skills-multiple-locations-path = skill '{$name}' exists in multiple locations ({$locations}); pass an explicit path to disambiguate
cli-skills-tags = Tags:  {$tags}
cli-skills-skipped-header = Skipped ({$count}):
cli-skills-skipped-reason = {"    "}Reason: {$reason}
cli-skills-skipped-scripts-hint = {"    "}Set `skills.allow_scripts = true` in your zeroclaw config to enable it.

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
cron-shell-output-truncated = ...[output truncated]
cron-shell-output-limit = Shell job stopped at its output limit; it will not be retried automatically.
daemon-cron-shutdown-incomplete = Cron tasks did not stop before the shutdown deadline. Reload is stopped to prevent overlapping scheduled jobs; restart the daemon process.
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
cli-cron-update-no-field = At least one of --expression, --tz, --command, --name, --allowed-tool, or --uses-memory must be provided
cli-cron-removed = ✅ Removed cron job {$id}
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
cli-quickstart-row-model-provider = {$glyph} Model provider     — {$summary}
cli-quickstart-row-risk-profile = {$glyph} Risk profile       — {$summary}
cli-quickstart-row-memory = {$glyph} Memory             — {$summary}
cli-quickstart-row-channels = {$glyph} Channels (0..N)    — {$summary}
cli-quickstart-row-peer-groups = {$glyph} Peer groups        — {$summary}
cli-quickstart-row-agent-identity = {$glyph} Agent identity     — {$summary}
cli-quickstart-summary-not-yet-chosen = not yet chosen
cli-quickstart-summary-not-yet-visited = not yet visited
cli-quickstart-summary-not-yet-named = not yet named
cli-quickstart-summary-provider-fresh = {$name} (alias: {$alias}, model: {$model})
cli-quickstart-summary-use-existing = use existing {$reference}
cli-quickstart-summary-preset-fresh = preset: {$name}
cli-quickstart-summary-channels-none = none (chat via `zeroclaw agent` only)
cli-quickstart-summary-agent = alias: {$alias}, system prompt: {$chars} chars, {$files} personality file(s)
cli-quickstart-summary-peer-groups-none = none — channels accept no peers
cli-quickstart-channel-remove-row = {"  "}{$reference} (remove)
cli-quickstart-peer-group-row = {$channel} → {$name} ({$count} peers)
cli-quickstart-provider-local-label = {$name} (local)
cli-quickstart-provider-type-prompt = Provider type
cli-quickstart-alias-for = Alias for {$name}
cli-quickstart-openai-auth-mode-label = Authentication
cli-quickstart-openai-auth-mode-help = Choose `codex` to use a ChatGPT/Codex subscription auth profile. If you already signed in with the Codex CLI, run `zeroclaw auth login --model-provider openai-codex --import ~/.codex/auth.json`; otherwise run `zeroclaw auth login --model-provider openai-codex`.
cli-quickstart-anthropic-auth-mode-label = Authentication
cli-quickstart-anthropic-auth-mode-help = Choose `api_key` for an Anthropic Console key, or `setup_token` if you will run `claude setup-token` for Claude Max and paste the generated token.
cli-quickstart-anthropic-api-key-help = Paste an Anthropic Console API key or the token generated by `claude setup-token`.
cli-quickstart-auth-codex-prompt = Sign in to OpenAI Codex with your ChatGPT account now?
cli-quickstart-auth-codex-import-prompt = Found an existing Codex login (~/.codex/auth.json) — import it now?
cli-quickstart-auth-codex-skip-hint = {"  "}Finish later with: zeroclaw auth login --model-provider openai-codex
cli-quickstart-auth-anthropic-prompt = Run `claude setup-token` for Anthropic provider `{$alias}` now?
cli-quickstart-auth-anthropic-token-prompt = Paste the token from `claude setup-token`
cli-quickstart-auth-anthropic-saved = {"  "}Saved Claude setup token for anthropic.{$alias}
cli-quickstart-auth-anthropic-skip-hint = {"  "}Finish later with: claude setup-token, then zeroclaw config set providers.models.anthropic.{$alias}.api_key <token>
cli-quickstart-auth-failed = {"  "}Auth setup didn't complete: {$error}
cli-quickstart-model-field-missing-warning = WARN: schema produced no `model` field for `{$provider}` — falling back to manual entry. Please report this.
cli-quickstart-model-id-for = Model id for {$name}
cli-quickstart-risk-profile-prompt = Risk profile
cli-quickstart-memory-backend-prompt = Memory backend
cli-quickstart-add-channel = + Add a channel
cli-quickstart-channels-done = Done (channels selector counts as visited)
cli-quickstart-channels-prompt = Channels (optional, 0..N)
cli-quickstart-channel-source-prompt = Channel source
cli-quickstart-all-channels-bound = {"  "}Every configured channel is already bound to an agent. Free one with `zeroclaw config set agents.<alias>.channels ...` before reusing it here.
cli-quickstart-pick-configured-channel = Pick a configured channel
cli-quickstart-channel-type-prompt = Channel type
cli-quickstart-add-peer-group = + Add peer group
cli-quickstart-done = Done
cli-quickstart-peer-groups-prompt = Peer groups (Enter on a row to remove, + Add to create)
cli-quickstart-channel-to-authorize-prompt = Channel to authorize
cli-quickstart-external-peers-prompt = External peers (comma- or newline-separated, blank for none)
cli-quickstart-agent-alias-prompt = Agent alias
cli-quickstart-edit-system-prompt = Edit system prompt in $EDITOR? (blank if you skip)
cli-quickstart-personality-start-template = Start with template (open in $EDITOR)
cli-quickstart-personality-start-current = Start from current content (open in $EDITOR)
cli-quickstart-personality-start-scratch = Start from scratch (open in $EDITOR)
cli-quickstart-personality-skip = Skip
cli-quickstart-esc-go-back = {" "}(Esc to go back)
cli-quickstart-esc-return-checklist = {" "}(Esc to return to checklist)
cli-quickstart-personality-file-prompt = {$filename}{$position} — what next?{$back_hint}
cli-quickstart-next-agent-command = {"  "}zeroclaw agent -a {$alias}  # chat with this agent in your terminal
cli-quickstart-fix-and-rerun = Your existing config is untouched. Fix the following and run quickstart again:
cli-quickstart-could-not-finish = quickstart could not finish: {$count} problem(s) to fix
cli-quickstart-pick-preset = Pick a preset
cli-quickstart-pick-existing-prompt = Pick an existing {$prompt}
cli-quickstart-pick-preset-prompt = Pick a {$prompt} preset
cli-quickstart-step-model-provider = Model provider
cli-quickstart-step-risk-profile = Risk profile
cli-quickstart-step-runtime-profile = Runtime profile
cli-quickstart-step-memory = Memory
cli-quickstart-step-channels = Channels
cli-quickstart-step-peer-groups = Peer groups
cli-quickstart-step-agent = Agent
cli-quickstart-error-internal-no-result = internal error: apply_into returned no result despite no validation errors
cli-quickstart-error-completion-flag = failed to flip quickstart-completed: {$err}
cli-quickstart-error-persist-config = failed to persist config: {$err}
cli-quickstart-error-not-type-alias-ref = `{$reference}` is not a `<type>.<alias>` reference
cli-quickstart-error-no-configured-path = no `{$path}` configured
cli-quickstart-error-tachi-not-compiled = memory backend 'tachi' requires a build with `memory-tachi` enabled
cli-quickstart-error-provider-required = provider type, alias, and model are required
cli-quickstart-error-unknown-provider-type = unknown model provider type `{$provider}` — pick one from the provider list
cli-quickstart-error-unknown-openai-auth-mode = unknown OpenAI auth mode `{$mode}` — pick `api_key` or `codex`
cli-quickstart-error-unknown-anthropic-auth-mode = unknown Anthropic auth mode `{$mode}` — pick `api_key` or `setup_token`
cli-quickstart-error-alias-exists = alias `{$alias}` already exists
cli-quickstart-error-no-profile = no `{$alias}` profile configured
cli-quickstart-error-unknown-risk-preset = unknown risk preset `{$preset}`
cli-quickstart-error-unknown-runtime-preset = unknown runtime preset `{$preset}`
cli-quickstart-error-channel-bound = channel `{$reference}` is already bound to agent `{$owner}`
cli-quickstart-error-channel-required = channel type and alias are required
cli-quickstart-error-channel-retired = channel type { $channel_type } is retired; it has no runner and cannot be configured
cli-quickstart-error-channel-field-not-advertised = channel field `{$field}` is not available in Quickstart
cli-quickstart-error-channel-token-required = Telegram bot token is required
cli-quickstart-error-peer-group-name-required = peer-group name is required
cli-quickstart-error-peer-group-channel-required = peer-group channel ref is required
cli-quickstart-error-peer-group-unknown-channel = peer-group `{$name}` references unknown channel `{$channel}`
cli-quickstart-error-peer-group-exists = peer-group `{$name}` already exists
cli-quickstart-error-personality-workspace = could not create agent workspace: {$err}
cli-quickstart-error-personality-filename-required = filename is required
cli-quickstart-error-personality-not-editable = `{$filename}` is not an editable personality file
cli-quickstart-error-personality-too-large = content exceeds {$limit} char limit
cli-quickstart-error-personality-stage-failed = stage {$filename} failed: {$err}
cli-quickstart-error-personality-write-failed = write {$path} failed: {$err}
cli-quickstart-error-agent-name-required = agent name is required
cli-quickstart-error-agent-exists = agent `{$name}` already exists
cli-no-channels-compiled = {"  "}No channel types are compiled into this binary.
cli-quickstart-complete = Quickstart complete. Created agent `{$alias}`.
cli-next-steps = Next steps:
cli-agent-not-created = Your agent was not created — and nothing on disk was changed.
cli-onboard-deprecated = `zeroclaw onboard` is deprecated — use `zeroclaw quickstart`.
cli-otp-initialized = Initialized OTP secret for ZeroClaw.
cli-otp-enrollment-uri = Enrollment URI: {$uri}
cli-otp-received = {"  "}✓ OTP received
cli-secret-captured = {"  "}● Value captured — press Enter to save
cli-secret-received = {"  "}✓ Secret received
cli-pairing-enabled = 🔐 Gateway pairing is enabled.
cli-pairing-use-code = {"  "}Use this one-time code to pair a new device:
cli-pairing-post = {"    "}POST /pair with header X-Pairing-Code: {$code}
cli-pairing-restart = {"   "}Restart the gateway to generate a new pairing code.
cli-pairing-disabled = ⚠️  Gateway pairing is disabled in config.
cli-gateway-running-q = {"   "}Is the gateway running? Start it with:
cli-bridge-added = Bridge "{$name}" saved in [gateway.bridges.{$name}].
cli-bridge-token-once = Bridge token (shown only once; give it to the bridge, e.g. as ZEROCLAW_GATEWAY_TOKEN):
cli-bridge-scope = Chat sessions this token may open: {$scope}
cli-bridge-scope-none = This token may open no chat session; add --session or --session-prefix to allow some.
cli-bridge-apply = Restart the gateway (or POST /admin/reload) to apply.
cli-bridge-removed = Bridge "{$name}" removed; its token no longer works.
cli-bridge-exists = Bridge "{$name}" already exists; pass --rotate to replace its token.
cli-bridge-unknown = No bridge named "{$name}" is configured.
cli-bridge-invalid-name = Invalid bridge name "{$name}": use letters, digits, '-' and '_'.
cli-bridge-list-empty = No bridges configured. Add one with: zeroclaw gateway bridge add <name>
cli-bridge-list-row = {$name}: sessions {$scope}
cli-bridge-list-no-sessions = (none)
cli-status-title = 🦀 ZeroClaw Status
cli-security-status-title = ZeroClaw Security Status
cli-security-status-source = Source:      {$v}
cli-security-status-agent = Agent:       {$v}
cli-security-status-agent-enabled = Agent enabled: {$enabled}
cli-security-status-risk-profile = Risk profile: {$v}
cli-security-status-autonomy = Autonomy:   {$v}
cli-security-status-approvals = Approvals:  medium-risk approval required: {$medium}, high-risk commands blocked: {$high}
cli-security-status-sandbox = Sandbox:    requested {$requested}, active {$active} ({$description})
cli-security-status-workspace = Workspace:  {$dir}; workspace-only: {$workspace_only}; rw roots: {$read_write_roots}; read-only roots: {$read_only_roots}; write-only roots: {$write_only_roots}; env passthrough: {$env_passthrough}
cli-security-status-credentials = Credentials: encryption: {$encryption}; secrets set: {$secrets_set}/{$secrets_total}; classified fields: {$classified_total}; classes: {$classification_summary}
cli-security-status-credentials-classes-none = none
cli-security-status-gateway = Gateway:    {$host}:{$port}; pairing required: {$pairing}; public bind: {$public_bind}; TLS: {$tls}
cli-security-status-warnings = Warnings:   {$v}
cli-security-status-warnings-none = Warnings:   none
cli-security-status-warning-agent-disabled = agent is disabled
cli-security-status-warning-sandbox-disabled = sandboxing is disabled for this agent risk profile
cli-security-status-warning-sandbox-none = active sandbox is application-layer only
cli-security-status-warning-sandbox-fallback = requested sandbox backend `{$requested}` fell back to `{$active}`
cli-security-sandbox-backend-unavailable = Requested sandbox backend `{$backend}` is unavailable; command execution is blocked.
cli-security-status-warning-workspace-not-restricted = workspace-only filesystem policy is disabled
cli-security-status-warning-shell-env-passthrough = {$count} shell environment variable(s) are passed through
cli-security-status-warning-secrets-unencrypted = config secret encryption is disabled
cli-security-status-warning-credential-follow-up = some credential-shaped config surfaces still require follow-up
cli-security-status-warning-pairing-disabled = gateway pairing is not required
cli-security-status-warning-public-bind-no-tls = gateway allows public bind without TLS enabled
cli-status-provider-none = 🤖 ModelProvider:      (none configured)
cli-status-agents-none = 🛡️  Agents:        (none configured)
cli-status-service-running = 🟢 Service:       running
cli-status-service-stopped = 🔴 Service:       stopped
cli-status-channels = Channels:
cli-status-cli-always = {"  "}CLI:      ✅ always
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
cli-status-trace-storage = 🧾 Trace storage:  {$mode} ({$path})
cli-status-agents = 🛡️  Agents:        {$v}
cli-status-runtime = ⚙️  Runtime:       {$v}
cli-status-heartbeat = 💓 Heartbeat:      {$v}
cli-status-heartbeat-every-minutes = every {$minutes}min
heartbeat-delivery-missing-field = {$required} is required when {$configured} is set.
heartbeat-delivery-bridge-required = {$field} names '{$bridge}', but no [gateway.bridges.{$bridge}] is configured. Heartbeat delivery requires an explicit gateway bridge and recipient; legacy [channels.*] entries no longer provide outbound delivery. Register the delivery bridge with zeroclaw gateway bridge add, then set the bridge name and recipient in [heartbeat].
heartbeat-delivery-scoped-bridge = {$field} names scoped bridge '{$bridge}', which cannot connect to /ws/bridge or receive outbox deliveries. Select a delivery bridge without a device or owner scope; keep the scoped credential's permissions unchanged.
cli-status-memory = 🧠 Memory:         {$backend} (auto-save: {$auto_save})
cli-status-companion-outbox-not-configured = Companion outbox: not configured
cli-status-companion-outbox-pending = Companion outbox: pending ({$pending} events)
cli-status-companion-outbox-pending-oldest = Companion outbox: pending ({$pending} events, oldest {$age}s)
cli-status-security-noprofile = Security ({$alias}): <no risk_profile>
cli-status-security = Security ({$alias}):
cli-status-workspace-only = {"  "}Workspace only:    {$v}
cli-status-allowed-roots = {"  "}Allowed roots:     {$v}
cli-status-allowed-commands = {"  "}Allowed commands:  {$v}
cli-status-max-actions = {"  "}Max actions/hour:  {$v}
cli-status-cost-tracking = {"  "}Cost tracking:     {$v}
cli-status-max-cost-day = {"  "}Max cost/day:      ${$v}
cli-status-max-cost-month = {"  "}Max cost/month:    ${$v}
cli-status-spent-today = {"  "}Spent today:       ${$spent} / ${$limit}
cli-status-spent-month = {"  "}Spent this month:  ${$spent} / ${$limit}
cli-status-otp = {"  "}OTP enabled:       {$v}
cli-status-estop = {"  "}E-stop enabled:    {$v}
cli-status-word-enabled = enabled
cli-status-word-disabled = disabled
cli-status-word-on = on
cli-status-word-off = off
cli-status-word-none = (none)
cli-status-word-configured = configured
cli-status-word-not-configured = not configured
cli-status-channel-not-compiled = 🚫 configured, not compiled
cli-status-channel-retired = ⚠️ section parses, nothing runs; remove it

# ── desktop / config / estop / auth ──
cli-desktop-not-installed = ZeroClaw companion app is not installed.
cli-desktop-blurb1 = The companion app is a lightweight menu bar app that
cli-desktop-blurb2 = connects to the same gateway as the CLI.
cli-config-all-configured = All sections already configured.
cli-config-schema-current = Config already at current schema version.
cli-config-applied-ops = Applied {$count} operation(s):
cli-config-section-degraded = warning: config section `{$section}` in {$path} is malformed and was reset to defaults for this run. Values in that section are NOT in effect. Run `zeroclaw config migrate` to see the parse error, then repair the file.
cli-estop-resume-done = Estop resume completed.
cli-estop-engaged = Estop engaged.
cli-estop-status = Estop status:
cli-auth-none = No auth profiles configured.
cli-auth-active = Active profiles:

# ── misc main (errors, config, estop fields, auth) ──
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
cli-auth-xai-imported = Imported xAI auth profile from {$path}
cli-auth-xai-device-code-started = xAI device-code login started.
cli-auth-oauth-visit = Visit: {$uri}
cli-auth-oauth-code = Code:  {$code}
cli-auth-oauth-fast-link = Fast link: {$uri}
cli-auth-xai-open-oauth-url = Open this xAI OAuth URL in your browser and authorize access:
cli-auth-callback-capture-failed = Callback capture failed: {$error}
cli-auth-run-paste-redirect = Run `zeroclaw auth paste-redirect --model-provider {$provider} --profile {$profile}`
cli-auth-xai-no-pending-login = No pending xAI login found. Run `zeroclaw auth login --model-provider xai` first.
cli-auth-paste-redirect-requires-input = paste-redirect requires the redirect URL or OAuth code

# ── locales fetch ──
cli-locales-about = Restore the locale files shipped with this build
cli-locales-long-about =
    Restore this build's Fluent (.ftl) catalogues under <config-dir>/data/ftl/<locale>/.
    The compatibility command name is fetch; it needs no network access and never downloads upstream catalogues.

    Choose en or zh-CN. Restore every catalogue by default, or select cli,tools with --catalog.

    Examples:
      zeroclaw locales fetch zh-CN
      zeroclaw locales fetch zh-CN --catalog tools
cli-locales-fetch-about = Restore this build's translated FTL files without network access
cli-locales-restored = {"  "}restored bundled {$name} -> {$path}
cli-locales-missing-builtin = Catalogue {$name} for {$locale} is not included in this build
cli-locales-none-selected = Select at least one catalogue: cli, tools
cli-locales-installed = Installed {$count} catalogue(s) for '{$locale}' under {$dir}

# ── browse (zeroclaw browse) ──
cli-browse-header = {$path} ({$count} entries)
cli-browse-empty = (empty)
cli-browse-file-bytes = {$name} ({$bytes} bytes)

# ── update (zeroclaw update) ──

# ── self-test (zeroclaw self-test) ──
cli-selftest-all-passed = All {$total} checks passed.
cli-selftest-some-failed = {$failed}/{$total} checks failed.
cli-selftest-channel-config-uncompiled = {$compiled} compiled channel types, {$configured} compiled/configured; configured but not compiled: {$names}. Build from source with `./install.sh --source --preset full`, `--features channels-full`, or the specific `channel-*` feature.
cli-selftest-channel-config-retired = {$compiled} channel types, {$configured} configured; configured sections of retired transports (nothing runs): {$names}. Remove those [channels.*] sections; live surfaces are the Telegram bridge, webhook ingress, Email tools, and Discord archive search.

# ── channels (zeroclaw channel list) ──
cli-channels-header = Channels:
cli-channels-cli-always = {"  "}✅ CLI (always available)
cli-channels-notion = {"  "}{$status} Notion
cli-channels-not-compiled-header = {"  "}Configured but not compiled in this binary:
cli-channels-not-compiled-entry = {"  "}🚫 {$name} (configured, not compiled)
cli-channels-build-hint = {"  "}Build from source with `./install.sh --source --preset full`, `--features channels-full`, or the specific `channel-*` feature.
cli-channels-start-hint = To start channels: zeroclaw channel start
cli-channels-doctor-hint = To check health:    zeroclaw channel doctor
cli-channels-configure-hint = To configure:      zeroclaw config set channels.<name>.<field>=<value>
cli-channels-retired-header = {"  "}Configured sections of retired transports (no runner exists):
cli-channels-retired-entry = {"  "}⚠️ {$name} (section parses, nothing runs; remove it)
cli-channels-retired-hint = {"  "}Live surfaces: Telegram bridge, webhook ingress, Email tools, Discord archive search.

cli-models-set-ok = Default model set to "{ $model }" on { $provider }.
cli-models-status-current = Default model: { $model } (provider: { $provider })
cli-models-status-none = No default model configured.

# ── Agent turn-engine user-visible markers (#7415) ────────────────────
# Appended to (or persisted as) assistant output when a turn is cut short;
# shown to end users across every transport (channels, WS, RPC, ACP, CLI).
turn-interrupted-by-user = [interrupted by user]
# Shown when a turn ends because the client RPC channel cancelled it. The actor
# is not verified: human interrupt and programmatic client cancels both arrive
# on this path, so the wording names the channel, not a user.
turn-cancelled-client-rpc = [turn cancelled via client]
turn-stream-interrupted = [stream interrupted]
# Trailing notice appended (and streamed as a final chunk) when the resilient
# provider wrapper served the turn with a different model or provider than the
# one requested, so silent model downgrades stay visible on direct-turn
# surfaces (WS, RPC/ZeroCode, ACP).
turn-model-fallback-notice = ⚡ { $requested_model } ({ $requested_provider }) was unavailable; this reply was served by { $actual_model } ({ $actual_provider }).
# Shown at the end of agent output when the tool call loop exhausted its
# iteration budget and the agent cannot continue without exceeding limits.
turn-max-iterations-reached = *Turn stopped: reached maximum tool iterations ({ $max_iterations }).*
# Breadcrumb injected into history where older turns were dropped to fit the
# context budget; user-visible across channels, WS, RPC, ACP.
history-trim-breadcrumb = [earlier turns omitted to fit the context window]
# Reason carried on every history_trimmed event (WS, SSE, ACP).
history-trim-reason-budget = context token budget exceeded
history-trim-reason-message-cap = history message limit exceeded
# Remediation surfaced when the system prompt + inlined tool definitions alone
# meet or exceed the context budget, so no amount of conversation trimming can
# fit the request (#5808).
history-trim-floor-exceeds-budget = system prompt and tool definitions ({$floor} tokens) alone meet or exceed the context budget ({$budget} tokens); raise [runtime_profiles.<name>] max_context_tokens or reduce the tool surface by disabling unused integrations
turn-tool-interrupted-before-result = [interrupted by user before this tool produced a result]
# Safe reply delivered when the model repeatedly emits malformed internal
# tool-call protocol and the turn gives up retrying.
channel-runtime-malformed-tool-output = I generated an internal tool-call format error and could not complete this request. Please try again.
channel-runtime-new-session = Conversation history cleared. Starting fresh.
channel-runtime-stop-sent = Stop signal sent.
channel-runtime-stop-no-task = No in-flight task for this sender scope.
channel-runtime-model-empty = Model ID cannot be empty. Use `/model <model-id>`.
channel-runtime-model-switched = Model switched to `{ $model }` (model_provider: `{ $provider }`). Context preserved.
channel-runtime-agent-scope-rejected = Sender `{ $sender }` is not authorized for `/model --agent` on agent `{ $agent }`. Use `/model --user { $model }` for a session-only override, or ask an admin to mark a peer group `admin_for_agent_scope = true` with you as a member.
channel-runtime-request-timeout = ⚠️ Request timed out while waiting for the model. Please try again.
channel-runtime-current-model-status =
    Current model_provider: `{ $provider }`
    Current model: `{ $model }`
channel-runtime-model-switch-hint = Switch model with `/model <model-id>` or `/model <hint>`.
channel-runtime-provider-switch-hint = Switch model_provider with `/models <model_provider>`.
channel-runtime-available-providers-header = Available model_providers:
channel-runtime-configured-routes-header = Configured model routes:
channel-runtime-no-cached-models = No cached model list found for `{ $provider }`. Ask the operator to run `zeroclaw models refresh --model-provider { $provider }`.
channel-runtime-cached-model-ids-header = Cached model IDs (top { $count }):
channel-runtime-config-switch-hints =
    Use `/models <model_provider>` to switch model_provider.
    Use `/model <model-id>` to switch model.
channel-runtime-config-block-title =
    { "*" }Model Configuration{ "*" }
    Current: `{ $provider }` / `{ $model }`
channel-runtime-config-select-provider-placeholder = Select model_provider
channel-runtime-config-select-model-placeholder = Select model
channel-runtime-config-provider-label = *ModelProvider*
channel-runtime-config-model-label = *Model*
channel-runtime-scope-user = user
channel-runtime-scope-agent = agent
channel-runtime-scope-overrides-summary =
    { "**" }Model overrides{ "**" } (session-only; precedence user > agent > session > default):
    • user: { $user }
    • agent: { $agent }
    • session (this chat): { $session }
    • default (config): { $default }
    Set a scope with `/model --user|--agent <model-id>`; clear by setting it back to the default.
channel-runtime-set-provider-switched =
    ModelProvider switched to `{ $provider }` for this sender session. Current model is `{ $model }`.
    Use `/model <model-id>` to set a provider-compatible model.
channel-runtime-set-provider-init-failed =
    Failed to initialize model_provider `{ $provider }`. Route unchanged.
    Details: { $error }
channel-runtime-provider-ambiguous = ModelProvider `{ $family }` has multiple configured aliases. Qualify which one with `/models { $family }.<alias>`: { $list }
channel-runtime-provider-no-alias = No configured provider entry for `{ $provider }`. Add `[providers.models.{ $provider }]` (with its api_key/uri) or select a configured provider — `/models` lists valid ones.
channel-runtime-provider-unknown = Unknown model_provider `{ $provider }`. Use `/models` to list valid model_providers.
channel-runtime-scoped-model-empty = Model ID cannot be empty. Use `/model --user|--agent <model-id>`.
channel-runtime-scoped-model-switched = Model set to `{ $model }` (model_provider: `{ $provider }`) for the **{ $scope }** scope. Session-only — resets on restart.
channel-runtime-shadow-note = ⚠️ A higher-precedence override is active, so messages will use `{ $model }` (`{ $provider }`) instead — see `/model`.
channel-runtime-provider-turn-init-failed =
    ⚠️ Failed to initialize model_provider `{ $provider }`. Please run `/models` to choose another model_provider.
    Details: { $error }
channel-runtime-fallback-footer =
    ⚡ `{ $requested }` unavailable — response from **{ $actual }** (`{ $model }`)
    Switch model: /models

# ── Alias CRUD CLI — zeroclaw {agents,providers,channels} {create,list,rename,delete} (#7468 / #7175) ──
cli-alias-list-empty = (no entries under {$section})
cli-alias-created = created {$section}.{$alias}
cli-alias-exists = {$section}.{$alias} already exists (no change)
cli-alias-impact-scrub-header = deleting {$section}.{$alias} would scrub {$count} reference(s):
cli-alias-impact-blocked-header = deleting {$section}.{$alias} is BLOCKED by {$count} hard reference(s):
cli-alias-impact-blocker = ✗ {$path} (hard reference)
cli-alias-impact-scrub = • {$path} (would be scrubbed)
cli-alias-no-changes = No changes made. Re-run with --yes to apply (or --dry-run to preview).
cli-alias-warn-workspace-archive = warning: workspace archive failed: {$error}
cli-alias-owned-cascaded = owned-state cascaded: memory {$memory} · cron {$cron} · acp {$acp} · sessions {$sessions} → {$archive}
cli-alias-owned-repointed = owned-state re-pointed: memory {$memory} · cron {$cron} · acp {$acp} · sessions {$sessions}
cli-alias-warn-workspace-move = warning: workspace move failed: {$error}
cli-alias-warn = warning: {$warning}
cli-alias-deleted = deleted {$section}.{$alias} (scrubbed {$count} reference(s))
cli-alias-delete-refused-header = refused: {$count} hard reference(s) block the delete:
cli-alias-delete-refused-hint = delete refused — resolve the hard references first
cli-alias-not-configured = {$path} is not configured
cli-alias-delete-failed = delete failed: {$error}
cli-alias-delete-reserved-default = the `default` agent is reserved and cannot be deleted
cli-alias-create-reserved-default = the `default` agent is reserved and cannot be created
cli-alias-renamed = renamed {$section}.{$from} → {$section}.{$to} (rewrote {$count} reference path(s))
cli-alias-rename-invalid = invalid new alias: {$message}
cli-alias-rename-reserved = alias `{$alias}` is reserved and cannot be renamed
cli-alias-rename-postcondition = rename cascade post-condition failed: {$message}
cli-alias-unknown-provider-category = unknown provider category `{$category}` (expected models | tts | transcription)
cli-alias-no-such-section = no such config section: {$section}
cli-alias-live-acp-sessions = {$count} live ACP session(s) for `{$alias}` — end them first
cli-alias-owned-state-unavailable = note: config references were updated, but the agent's owned state (memory rows, workspace dir, cron/acp/session rows) was NOT cascaded by this CLI yet — use the gateway API for the full owned-state cascade.
cli-bundle-not-configured = skill bundle '{$alias}' is not configured
cli-bundle-rename-failed = rename failed: {$error}

# ── Skill-bundle CLI — zeroclaw skills bundle {add,remove,rename} (#7468 / #7175) ──
cli-bundle-exists = skill bundle '{$alias}' already exists (no change)
cli-bundle-created = created skill_bundles.{$alias} (dir: {$dir})
cli-bundle-created-warn = created skill_bundles.{$alias} (warning: dir resolve failed: {$error})
cli-bundle-impact-header = deleting skill_bundles.{$alias} would strip it from {$count} agent reference(s):
cli-bundle-no-changes = No changes made. Re-run with --yes to apply.
cli-bundle-archived = archived bundle directory → {$path}
cli-bundle-warn-archive = warning: bundle directory archive failed: {$error}
cli-bundle-deleted = deleted skill_bundles.{$alias} (stripped from {$count} agent(s))
cli-bundle-warn-move = warning: bundle directory move failed: {$error}
cli-bundle-renamed = renamed skill_bundles.{$from} → skill_bundles.{$to}

# ── Web dashboard restart hints — RestartInfo.hint shown after an in-app upgrade (PR #8173) ──
# The first four are shell command templates shown verbatim; they are not translated.
cli-gateway-restart-hint-kubernetes = kubectl rollout restart deployment/zeroclaw
cli-gateway-restart-hint-container = docker compose restart
cli-gateway-restart-hint-systemd = systemctl restart zeroclaw
cli-gateway-restart-hint-launchd = launchctl kickstart -k <your-zeroclaw-label>
cli-gateway-restart-hint-process = restart the `zeroclaw daemon` process

# ── daemon gateway bind pre-flight — zeroclaw daemon (#7895) ──
# Emitted by the daemon startup guard in src/main.rs when the configured gateway
# address is already bound. The daemon supervises its own in-process gateway
# (shared event bus / canvas / reload channel) and cannot adopt a separate
# process, so it fails fast with an actionable message instead of degrading into
# a supervisor retry loop. The two variants differ only by who holds the port.
cli-daemon-gateway-already-running = A ZeroClaw gateway is already running on {$host}:{$port}. The daemon supervises its own gateway and will not start a second one on the same address. Stop that gateway (or point the daemon at a free port with `zeroclaw config set gateway.port <port>`), then run the daemon again.
cli-daemon-gateway-port-occupied = Gateway address {$host}:{$port} is already in use by another process. Free the port or point the daemon at a free port (`zeroclaw config set gateway.port <port>`), then run the daemon again.

# ── Context window (doctor update-context-windows, agent interactive) ──
cli-doctor-context-window-ok = {$provider_ref}: context window: {$context_window} tokens
cli-doctor-context-window-zero = {$provider_ref}: context_window is 0 (invalid; set it to the model's real context limit)
cli-doctor-context-window-unset = {$provider_ref}: no context_window set — will use {$fallback} token fallback when selected; likely far below this model's real limit; set context_window on this profile
cli-agent-context-bar = ctx: {$used} / {$max}  {$bar}  {$pct}%
cli-agent-context-bar-unknown = ctx: unknown / {$max}
cli-doctor-ctxwin-already-set = {$provider_ref}: already has context_window = {$ctx}
cli-doctor-ctxwin-no-model = {$provider_ref}: no model configured, skipping
cli-doctor-ctxwin-would-set = {$provider_ref}: would set context_window = {$ctx} (dry run)
cli-doctor-ctxwin-set = {$provider_ref}: set context_window = {$ctx}
cli-doctor-ctxwin-not-found = {$provider_ref}: could not find entry to update
cli-doctor-ctxwin-fetch-failed = {$provider_ref}: provider does not expose context window or fetch failed
cli-doctor-ctxwin-saved = Saved {$updated} updates to config.toml
cli-doctor-ctxwin-dry-run = Dry run complete — no changes written. Run without --dry-run to apply.
cli-doctor-ctxwin-none = No updates needed.
cli-doctor-ctxwin-write-failed = {$provider_ref}: failed to write context_window: {$error}

# ── Degraded config sections (doctor diagnose, #8835) ──
cli-doctor-degraded-security = SECURITY-CRITICAL config section `{$path}` is invalid and was reset to its default so the daemon can boot; the running posture may be WEAKER than intended. Run `zeroclaw config migrate` to see the parse error, then repair the file.
cli-doctor-degraded-section = config section `{$path}` is malformed and was reset to defaults; values in that section are NOT in effect. Run `zeroclaw config migrate` to see the parse error, then repair the file.

# ── Personality file truncation (doctor diagnose, #310) ──
cli-doctor-personality-file-truncated = {$alias}/{$filename}: personality cap {$cap} vs {$total} chars ({$discarded} would be discarded)

# ── Runtime command construction — shell and skill shell tools ──
tool-runtime-command-build-failed = Failed to build runtime command: {$error}
tool-runtime-command-docker-workspace-path = Failed to build runtime command: Failed to canonicalize Docker workspace path {$path}: {$cause}
tool-runtime-command-docker-allowed-root = Failed to build runtime command: Failed to canonicalize Docker workspace root {$path}: {$cause}

# ── Telegram operator tooling ──
cli-telegram-about = Compatibility diagnostics for retired Telegram operator tooling
cli-telegram-long-about =
    The in-process Telegram listener is retired. Telegram intake and cursor recovery belong to zeroclaw-bridge-telegram.

    skip-update remains parseable for compatibility and returns a migration error. It does not write a skip marker or advance the bridge cursor.
cli-telegram-skip-update-about = Explain migration of the retired Telegram skip command
cli-telegram-skip-update-long-about =
    The in-process Telegram listener and its skip markers are retired. This compatibility command returns a migration error without writing a marker. Telegram intake and cursor recovery belong to zeroclaw-bridge-telegram; this command cannot skip a bridge update.
telegram-skip-update-retired = The in-process Telegram listener and telegram skip-update are retired. No skip marker was written. Telegram intake and cursor recovery belong to zeroclaw-bridge-telegram; this command cannot skip a bridge update.

# ── Task-scoped preference runtime command ──
channel-runtime-task-pref-set = Task preference recorded for this session only: {$kind}: {$statement}
channel-runtime-task-pref-invalid = Invalid task preference. Usage: /task-pref <value|goal|preference|habit|constraint> <semantic-key> <statement...>

# zeroclaw chat
cli-chat-connect-failed = Could not attach to the gateway at {$gateway}: {$error}. Start it with `zeroclaw daemon` or `zeroclaw gateway start`, or pass --gateway.
cli-chat-connect-refused = The gateway at {$gateway} refused the chat: {$error}
cli-chat-attached = Session {$session} with {$agent}, {$history} earlier messages. /quit leaves, /cancel or Ctrl+C stops a running turn.
cli-chat-approval-prompt = Allow {$tool}? {$summary} [y]es / [a]lways / [N]o:
cli-chat-tool-call = → {$tool}
cli-chat-aborted = (turn cancelled)
cli-chat-error = Error: {$message}
cli-chat-duplicate = Already handled ({$state}).
cli-chat-steered = (added to the running turn)
cli-chat-closed = The gateway closed the connection.
cli-chat-event = [{$kind}] {$text}
cli-chat-usage = [context {$context}] {$cost}

cli-chat-question = Answer using /answer { $id } <text or choice number>
cli-chat-answer-ack = Answer { $id }: { $status }
cli-chat-question-closed = Question { $id } is closed.
cli-chat-answer-usage = Usage: /answer <request_id> <text or choice number>

# Gateway HTTP attachment protocol
gateway-attachment-invalid-ids = Attachments must be opaque upload IDs
gateway-attachment-repeated = Repeated attachment IDs are not allowed
gateway-attachment-count = At most four upload IDs may accompany a message
gateway-attachment-unauthorized = Attachments require current device or bridge authority
gateway-attachment-unavailable = Attachment is expired, unavailable or outside this session

gateway-intake-unavailable = The input could not be safely resumed. Reconnect to check its durable receipt before sending it again.
gateway-pair-request-limit = Too many pairing requests. Please retry later.
gateway-pair-auth-limit = Too many authentication attempts. Please retry later.
gateway-cwd-workspace-unavailable = Agent workspace is not a usable directory
gateway-cwd-not-directory = cwd is not a usable directory
gateway-cwd-outside-workspace = cwd must be within the configured agent workspace
gateway-invalid-surface = Invalid chat surface. Use web, telegram or cli; query and connect values must agree.

# Generic review notice. Never interpolate Soul or owner evidence content.
soul-weekly-review-notice = Weekly reflection created { $soul_count } Soul proposals and { $user_count } User Model candidates. Review: { $review_path }
# Soul drift report. Counts only; never interpolate Soul or owner content.
soul-drift-notice = Soul drift report: { $dials_moved } Tone dials moved from your anchor (largest move: { $max_steps } steps); { $story_added } Story entries added, { $story_retired } retired. Keep it or roll back: { $review_path }
soul-tone-unavailable = Current configured Tone is unavailable.
soul-tone-step-limit = Tone proposals may move a dial by at most one level.
soul-tone-challenge-up-only = The challenge dial may only be proposed upward; only your owner can lower it.
soul-tone-dial-cooldown = The { $dial } dial was changed by an approved proposal less than { $days } days ago, so it cannot be proposed again yet.
soul-tone-exemplars-too-many = A Tone can hold at most four exemplars.
soul-tone-exemplar-empty = Each exemplar needs a situation, your line, and the agent's line.
soul-tone-exemplar-single-line = An exemplar field must be a single line of text without line breaks or control characters.
soul-tone-exemplar-too-long = An exemplar field is over its limit: situation 80 bytes, your line 160 bytes, the agent's line 320 bytes.

# Soul check (ADR-018 section 5). Labels and counts only; never interpolate Soul or owner content.
cli-soul-about = Inspect and check the agent Soul
cli-soul-check-about = Check that the Soul behaves the same on the selected models (spends real model calls)
soul-check-notice = Soul check for agent { $agent }: this makes real model calls, up to { $calls } ({ $replies } scenario replies plus { $judgements } judge calls). Each selected target receives the agent's real Soul projection as its system prompt — only choose targets you want to send it to. Results are printed and not stored (a data directory without a seeded Soul gets one, same as the first chat turn).
soul-check-title = Soul check for agent { $agent } (judge: { $judge })
soul-check-target = Target { $target }
soul-check-col-scenario = SCENARIO
soul-check-col-check = CHECK
soul-check-col-expected = EXPECTED
soul-check-col-result = RESULT
soul-check-col-delta = DELTA
soul-check-col-flags = FLAGS
soul-check-disagree-title = Targets two or more levels apart
soul-check-summary = Summary: { $targets } targets, { $scenarios } scenarios, { $calls } model calls; { $off_by_two } off by two, { $floor_failed } floors failed, { $disagree } providers disagree, { $errors } errors.
soul-check-caveat = The judge is a model: treat these results as a signal, not proof. Nothing is stored.
cli-doctor-soul-check-hint = Agent { $alias } has a Soul. After changing its provider or model, run `zeroclaw soul check --agent { $alias }` to confirm it still behaves like itself.

# Scoped chat turn cancellation. Requested is not a completed cancellation.
gateway-cancel-requested = Stop requested for that turn; it has not yet confirmed cancellation.
gateway-cancel-stale = That turn has ended. The newer turn was not stopped.
gateway-cancel-no-active = That turn is no longer running.
gateway-cancel-unauthorized = Current device or scoped bridge authority is required to stop this turn.
gateway-cancel-invalid = This stop request has no supported version or valid turn target.
gateway-cancel-idle = No running turn was observed; no stop was sent.
gateway-cancel-unknown = The stop outcome is unknown. It will not be sent again automatically.
gateway-cancel-not-sent = The stop was not sent. Reconnect or wait for pending stop outcomes before trying again.

# Restricted device bridges never inherit owner approval authority.
gateway-bridge-access-denied = This bridge is not authorized for the requested binding or operation.

# Spoken presentation retains semantic Markdown status.
spoken-task-complete = Completed:
spoken-task-pending = Pending:
spoken-deleted = Deleted text:
voice-html-omitted = This reply contains markup and has been skipped for speech.

acp-bridge-allow-once = Allow once
acp-bridge-reject-once = Reject once
cli-paircode-local-only = Pairing-code requests must target loopback. Run this on the gateway host with --host 127.0.0.1.
cli-paircode-admin-token-missing = No gateway admin token at { $path }. Run this on the gateway host, as the user running it, while the gateway is running.
delegation-result-notice = Delegated task {$request} finished with outcome {$outcome}. Ask the agent or query tachi_result for dispatch {$dispatch}.
task-reminder-notice = Reminder: {$task}

# Completed-dispatch repetition recovery within one Agent API turn.
turn-repeated-success-advisory = The same tool call returned the same successful result repeatedly. Reconsider whether another call will help. Polling may continue when needed.
turn-repeated-failure-recovery = The same tool call failed with the same result repeatedly. Change the approach or explain the blocker. One further identical retry is available before this turn stops.
turn-repeated-failure-exhausted = The recovery retry failed with the same result, so this turn has stopped. Completed tool results have been preserved. You can retry in a new turn.
turn-repeated-failure-retry-skipped = Skipped an additional identical call in this recovery batch. Only one recovery retry is available.

cli-update-source-only = This fork has no verified binary update channel. Update a checkout of https://github.com/kckylechen1/zeroclaw and run ./install.sh --source with your existing preset/features. Upstream releases cannot update this fork.
