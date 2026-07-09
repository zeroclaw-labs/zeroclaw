zc-pane-dashboard = Dashboard
zc-pane-config = Config
zc-pane-doctor = Doctor
zc-pane-code = Code
zc-pane-chat = Chat
zc-pane-logs = Logs
zc-pane-quickstart = Quickstart

zc-app-help-cycle-mode = Cycle mode
zc-app-help-reload = Reload daemon
zc-app-help-quit = Quit

zc-app-press-any-key-to-close = Press any key to close
zc-app-reload-line-1 = The daemon process stays running (same PID), but every
zc-app-reload-line-2 = subsystem tears down and re-initializes from the on-disk
zc-app-reload-line-3 = config:
zc-app-reload-bullet-gateway = { "  " }• Gateway listener stops and rebinds
zc-app-reload-bullet-channels = { "  " }• Channel listeners (Matrix, Slack, etc.) respawn
zc-app-reload-bullet-mcp = { "  " }• MCP servers, scheduler, heartbeat re-init
zc-app-reload-bullet-provider = { "  " }• Provider clients pick up new API keys / model defaults
zc-app-reload-socket-note = The RPC socket will briefly drop. The TUI will reconnect.
zc-app-quit-prompt = Quit zerocode?
zc-app-quit-explainer = The TUI closes. The daemon keeps running; reconnect anytime.
zc-app-reload-status-signalled = Daemon reload signalled — reconnecting…
zc-app-reload-confirm-row = { $confirm_chord } = reload   { $cancel_chord } = cancel
zc-error-daemon-version-mismatch = Version mismatch: zerocode is { $client_version } but the daemon is { $server_version }. Rebuild and restart the daemon from the same checkout as zerocode.

zc-zerocode-tab-theme = Theme
zc-zerocode-tab-agent-theme = Agent Themes
zc-zerocode-tab-presets = Presets
zc-zerocode-tab-bindings = Keybindings
zc-zerocode-tab-locale = Locale
zc-zerocode-tab-connection = Connection
zc-zerocode-conn-title = Connection ([wss] — Enter to edit)
zc-zerocode-conn-uri = WSS URI
zc-zerocode-conn-skip-verify = TLS skip verify
zc-zerocode-conn-skip-verify-routes = Skip-verify routes
zc-zerocode-conn-unset = unset
zc-zerocode-conn-no-routes = none
zc-zerocode-conn-saved = Saved
zc-zerocode-conn-edit-text = Enter to save, Esc to cancel.
zc-zerocode-conn-edit-bool = Enter toggles; this field saves on toggle.
zc-zerocode-conn-edit-routes = One route per line. Enter for a new line, Ctrl+S to save, Esc to cancel.
zc-zerocode-locale-loading = loading locales…
zc-zerocode-locale-download = ⬇ Download selected locale file
zc-zerocode-locale-set = Locale set to { $locale }. Download its locale file if needed, then restart to apply.
zc-zerocode-locale-fetching = Downloading locale files for { $locale }…
zc-zerocode-locale-downloaded = Downloaded { $written } for { $locale }. Skipped: { $skipped }
zc-zerocode-locale-fetch-failed = Locale download failed for { $locale }: { $err }
zc-zerocode-locale-list-failed = Failed to load locale list: { $err }
zc-zerocode-locale-pick-first = Select a locale row first, then download its locale file.
zc-zerocode-help-locale = select / download locale
zc-zerocode-help-conn = edit connection field

zc-zerocode-capture-prompt = Press a key combination…
zc-zerocode-capture-modal-title = Assign Key
zc-zerocode-hint-cancel = { $keys } to cancel

zc-zerocode-capture-assign = Assign as the new binding
zc-zerocode-capture-cancel = Cancel capture

zc-zerocode-help-switch-pane = Switch pane (Theme/Presets/Keybindings)
zc-zerocode-help-navigate = Navigate
zc-zerocode-help-apply-theme = Apply theme (live + saved)
zc-zerocode-help-assign-agent-theme = Assign theme to the selected agent
zc-zerocode-help-pick-agent = Choose theme for this agent
zc-zerocode-help-clear-agent-theme = Clear the agent's override
zc-zerocode-help-apply-preset = Apply preset (overwrites keybindings)
zc-zerocode-help-rebind = Rebind selected action
zc-zerocode-help-reset-default = Reset action to default
zc-zerocode-help-mouse-label = Mouse
zc-zerocode-help-mouse-desc = Click pane / row, scroll, click section tab
zc-zerocode-help-choose-section = Choose section
zc-zerocode-help-open-section = Open section
zc-zerocode-help-navigate-rows = Navigate rows
zc-zerocode-help-back-to-sections = Back to sections

zc-zerocode-agent-modal-title = Select Agent
zc-zerocode-agent-modal-pick = Pick agent, then choose a theme
zc-zerocode-agent-theme-loading = Loading agents…
zc-zerocode-agent-theme-no-agents = No agents configured
zc-zerocode-agent-theme-none = No override on this agent
zc-zerocode-agent-theme-set = Override set: { $agent } → { $theme }
zc-zerocode-agent-theme-cleared = Override cleared for { $agent }
zc-zerocode-agent-theme-hint = { $assign } assign theme   ·   { $clear } clear override

zc-input-no-pending-attachments = No pending attachments.
zc-input-no-clipboard-image = Clipboard is empty.
zc-input-placeholder-chat = Type to chat

zc-input-help-slash-commands = Slash commands
zc-input-help-completions-navigate = Navigate completions
zc-input-help-completions-accept = Accept
zc-input-help-completions-dismiss = Dismiss


zc-input-attached = Attached: { $label }
zc-input-attach-error = Attach error: { $error }
zc-input-detached = Detached: { $name }
zc-input-invalid-index = Invalid index: { $index }
zc-input-pending-attachments-header = Pending attachments:
zc-input-clipboard-error = Clipboard error: { $error }

zc-queue-empty = Nothing to send.
zc-cancel-timed-out = Cancel timed out; turn settled locally.
zc-queue-full = Queue is full ({ $cap } max). Wait for messages to send.
zc-queue-title = Queue ({ $count })
zc-queue-empty-list = No queued messages.
zc-queue-paused-ghost = Queue paused — press { $key } or send a message to resume
zc-queue-item-injected = (inject)
zc-queue-resumed = Queue resumed.
zc-queue-clear-empty = Queue is already empty.
zc-queue-cleared-all = Cleared { $count } queued message(s).
zc-queue-cleared-one = Cleared queued message { $index }.
zc-queue-clear-invalid = No queued message at index { $index } (queue has { $count }).
zc-queue-help-resume = Pause/resume queue
zc-queue-help-nav = Select queued
zc-queue-help-delete = Delete queued
zc-queue-help-clear = Clear queue ([N] = position)
zc-queue-help-edit = Edit queued
zc-queue-help-resize = Resize queue
zc-queue-help-enqueue = Queue message
zc-queue-help-inject = Send now (skip queue)
zc-queue-edit-busy = Finish or clear the current message before editing a queued one.
zc-queue-dispatch-failed = Could not send queued message: { $error }

zc-logs-label-timestamp = Timestamp
zc-logs-label-severity = Severity
zc-logs-label-category = Category
zc-logs-label-action = Action
zc-logs-label-outcome = Outcome
zc-logs-label-duration = Duration
zc-logs-section-message = Message
zc-logs-section-trace = Trace
zc-logs-section-attribution = Attribution
zc-logs-section-attributes = Attributes
zc-logs-preview-only = Full payload unavailable — showing preview fields only.
zc-logs-no-event-selected = No event selected
zc-logs-loading = Loading…
zc-logs-search-action-apply = apply
zc-logs-search-action-cancel = cancel

zc-logs-help-mouse-label = Mouse
zc-logs-help-mouse-desc = Click to select, scroll wheel, double-click detail

zc-doctor-title = Doctor
zc-doctor-loading = Loading diagnostics...
zc-doctor-error = Doctor failed: { $error }
zc-doctor-error-unsupported-daemon = Doctor RPC is not available on this daemon yet. Restart the daemon with a build that includes doctor/run.
zc-doctor-no-results = No diagnostics yet
zc-doctor-summary = { $ok } ok  { $warnings } warnings  { $errors } errors
zc-doctor-filter-status = filter: { $filter }
zc-doctor-filter-all = All
zc-doctor-filter-problems = Problems
zc-doctor-filter-errors = Errors
zc-doctor-list-title = Diagnostics ({ $filter })
zc-doctor-detail-title = Detail
zc-doctor-no-selection = No diagnostic selected
zc-doctor-label-message = Message
zc-doctor-help-mouse = Mouse: click filter/select, scroll wheel

zc-dashboard-tab-overview = Overview
zc-dashboard-tab-sessions = Sessions
zc-dashboard-tab-agents = Agents
zc-dashboard-tab-memories = Memories
zc-dashboard-tab-health = Health
zc-dashboard-tab-cost = Cost
zc-dashboard-tab-cron = Cron

zc-dashboard-memory-not-configured = Memory is not configured yet. Use Quickstart or Config to add a memory backend, or ignore this tab until you need persistent memory.
zc-dashboard-search-action-apply = apply
zc-dashboard-search-action-cancel = cancel
zc-dashboard-search-prefix = search:

zc-dashboard-label-connected = Connected
zc-dashboard-label-server = Server
zc-dashboard-label-protocol = Protocol
zc-dashboard-label-sessions = Sessions
zc-dashboard-label-memory = Memory
zc-dashboard-label-cpu = CPU
zc-dashboard-label-insecure-tls = ⚠ unverified TLS — certificate not checked
zc-dashboard-label-uptime = Uptime
zc-dashboard-label-pid = PID

zc-dashboard-no-tuis = No TUIs connected
zc-dashboard-no-session = No session selected
zc-dashboard-no-agent = No agent selected
zc-dashboard-no-entry = No entry selected
zc-dashboard-no-job = No job selected

zc-dashboard-detail-key = Key
zc-dashboard-detail-agent = Agent
zc-dashboard-detail-channel = Channel
zc-dashboard-detail-name = Name
zc-dashboard-detail-messages = Messages
zc-dashboard-detail-created = Created
zc-dashboard-detail-activity = Activity
zc-dashboard-detail-alias = Alias
zc-dashboard-detail-enabled = Enabled
zc-dashboard-detail-category = Category
zc-dashboard-detail-namespace = Namespace
zc-dashboard-detail-timestamp = Timestamp
zc-dashboard-detail-score = Score
zc-dashboard-detail-importance = Importance
zc-dashboard-detail-session = Session
zc-dashboard-detail-daily = Daily
zc-dashboard-detail-monthly = Monthly
zc-dashboard-detail-tokens = Tokens
zc-dashboard-detail-requests = Requests
zc-dashboard-detail-schedule = Schedule
zc-dashboard-detail-next-run = Next Run
zc-dashboard-detail-last-run = Last Run
zc-dashboard-detail-last-status = Last Status
zc-dashboard-detail-sessions = Sessions
zc-dashboard-detail-persisted-sessions = Persisted Sessions
zc-dashboard-cost-not-available = Cost tracking is not available. Configure a cost backend to see usage data.
zc-dashboard-cost-org-error = Organization billing snapshot is present but could not be read. Check the org_cost.json sync.

zc-dashboard-message-history = Message History ({ $count })
zc-dashboard-loading-messages = Loading messages…
zc-dashboard-loading = Loading…

zc-dashboard-section-channels = Channels
zc-dashboard-section-content = Content
zc-dashboard-section-process = Process
zc-dashboard-section-components = Components
zc-dashboard-section-details = Details
zc-dashboard-section-summary = Summary
zc-dashboard-section-by-period = By Period (your account)
zc-dashboard-section-org = Organization (billed)
zc-dashboard-period-today = Today
zc-dashboard-period-month = Month
zc-dashboard-period-quarter-prefix = Q
zc-dashboard-period-ytd = YTD
zc-dashboard-col-period = period
zc-dashboard-col-cost = cost($)
zc-dashboard-col-paid-tok = paid tok
zc-dashboard-col-free-tok = free tok
zc-dashboard-col-reqs = reqs
zc-dashboard-org-tok = tok
zc-dashboard-org-projyr = proj/yr
zc-dashboard-org-name = Organization
zc-dashboard-org-personal = You (billed)
zc-dashboard-org-fy-prefix = FY
zc-dashboard-org-asof = as of
zc-dashboard-section-by-model = By Model
zc-dashboard-section-by-agent = By Agent
zc-dashboard-section-command = Command
zc-dashboard-section-prompt = Prompt
zc-dashboard-section-last-output = Last Output
zc-dashboard-section-manual-run = Manual Run
zc-dashboard-section-recent-runs = Recent Runs
zc-dashboard-actions = Actions
zc-dashboard-cron-action-hint = { $run } run now · { $refresh } refresh
zc-dashboard-loading-runs = Loading runs…
zc-dashboard-runs-error = Failed to load runs
zc-dashboard-no-runs = No runs recorded yet
zc-dashboard-run-succeeded = Run succeeded
zc-dashboard-run-failed = Run failed
zc-dashboard-run-running = Run started: { $id }
zc-dashboard-run-already-running = Run already in progress: { $id }
zc-dashboard-run-pending-status = running
zc-dashboard-run-pending-window = starting now
zc-dashboard-run-pending-output = Manual trigger in progress…


zc-dashboard-yes = yes
zc-dashboard-no = no
zc-dashboard-enabled = enabled
zc-dashboard-disabled = disabled

zc-quickstart-title = Quickstart
zc-quickstart-selector-model-provider = Model provider
zc-quickstart-selector-risk-profile = Risk profile
zc-quickstart-selector-runtime-profile = Runtime profile
zc-quickstart-selector-memory = Memory
zc-quickstart-selector-channels = Channels (optional)
zc-quickstart-selector-peer-groups = Peer groups (optional)
zc-quickstart-selector-agent = Agent
zc-quickstart-selector-submit = Submit

zc-quickstart-reuse-alias-help = Reuse this alias instead of creating a new one.
zc-quickstart-existing-providers-heading = Existing providers
zc-quickstart-existing-providers-show-more = [+ Show { $count } more existing providers]
zc-quickstart-existing-providers-show-fewer = [- Show fewer existing providers]
zc-quickstart-new-provider-heading = Create new provider
zc-quickstart-provider-alias-exists = Alias `{ $alias }` already exists. Pick another alias or use the existing provider.

zc-quickstart-risk-locked-down = Locked Down
zc-quickstart-risk-locked-down-desc = Tight defaults. Workspace-only fs, approval on med/high risk.
zc-quickstart-risk-balanced = Balanced
zc-quickstart-risk-balanced-desc = Day-to-day defaults. Approval on risky ops. Recommended.
zc-quickstart-risk-yolo = YOLO
zc-quickstart-risk-yolo-desc = Full autonomy. No approval gates. Use on disposable machines only.

zc-quickstart-runtime-tight = Tight
zc-quickstart-runtime-tight-desc = Low ceilings on iterations and tokens.
zc-quickstart-runtime-balanced = Balanced
zc-quickstart-runtime-balanced-desc = Sensible ceilings. Recommended.
zc-quickstart-runtime-unbounded = Unbounded
zc-quickstart-runtime-unbounded-desc = No artificial caps.

zc-quickstart-provider-local = Local. No credential required.
zc-quickstart-provider-cloud = Cloud. Provide an API key when prompted.

zc-quickstart-submit-create = Create the agent


zc-quickstart-modal-action-move = move
zc-quickstart-modal-action-pick = pick
zc-quickstart-modal-action-cancel = cancel
zc-quickstart-modal-action-accept = accept
zc-quickstart-modal-action-pick-on-enum = pick on ‹enum›
zc-quickstart-modal-action-activate = activate
zc-quickstart-modal-action-delete = delete
zc-quickstart-modal-action-close = close
zc-quickstart-modal-action-edit-name = type to edit name
zc-quickstart-modal-action-on-file-rows = on file rows
zc-quickstart-modal-action-save = save
zc-quickstart-modal-type-prefix = Type:
zc-quickstart-field-label-alias = Alias
zc-quickstart-field-help-alias = Unique identifier for this provider instance. Change from "default" if your config already has one with that alias.
zc-quickstart-model-loading = Loading models for { $provider }...
zc-quickstart-model-retrying = Retrying model catalog for { $provider }...
zc-quickstart-model-catalog-empty = catalog unavailable - type a model name
zc-quickstart-action-done = Done
zc-quickstart-no-peer-groups = No peer groups configured. Optional — agents can still send messages to channels.

zc-quickstart-help-external-peers = Comma- or newline-separated. Blank = no external peers.

zc-quickstart-status-submitting = Submitting…
zc-quickstart-status-created = Created `{ $alias }`. Reloading daemon — Chat will open when reconnected…
zc-quickstart-status-errors = { $count } error(s) — fix selectors and resubmit
zc-quickstart-status-first-error = { $where }{ $field }: { $message }{ $more }
zc-quickstart-status-more-errors = { " " }(+{ $count } more)
zc-quickstart-status-can-create = All required selectors ✓. Press `{ $chord }` to Create.
zc-quickstart-status-hint = ↑/↓ to move, Enter to open. `{ $chord }` enables when all required selectors are ✓.
zc-quickstart-missing-model-provider = Choose a model provider before creating.
zc-quickstart-missing-risk-profile = Choose a risk profile before creating.
zc-quickstart-missing-runtime-profile = Choose a runtime profile before creating.
zc-quickstart-missing-memory = Choose a memory backend before creating.
zc-quickstart-missing-agent = Name the agent before creating.

zc-quickstart-channels-empty = No channels configured. An agent without channels still works via `zeroclaw agent <name>` from the CLI.
zc-quickstart-channels-add = + Add channel
zc-quickstart-peers-add = + Add peer group
zc-quickstart-block-channels = Channels
zc-quickstart-block-peers = Peer groups
zc-quickstart-block-agent = Agent
zc-quickstart-personality-help = Personality files (e=edit, t=use template, c=clear)
zc-quickstart-save-and-close = Save & close
zc-quickstart-no-template = No template is available for `{ $filename }`
zc-quickstart-agent-name-field = name
zc-quickstart-file-bytes = { $bytes } bytes

zc-chat-pane-chat = Chat
zc-chat-pane-acp = ACP

zc-chat-no-agents = No enabled agents yet. Open Quickstart to create one, or use Config to add and enable an agent.
zc-chat-error-fetch-agents = Failed to fetch agents: { $error }
zc-chat-error-create-session = Failed to create session: { $error }
zc-chat-elicitation-dropped = A prompt from the agent was dropped before it could be shown (the client fell behind). The agent's question may be waiting; try again.
zc-chat-session-restarted = New session started.
zc-chat-session-restart-error = Failed to start a new session: { $error }

zc-chat-thinking-visible = Thinking output: visible
zc-chat-thinking-hidden = Thinking output: hidden

# Model picker slash commands
zc-model-picker-hint = Type a model name after /model, or Tab to autocomplete.
zc-model-provider-picker-hint = Type a model_provider after /model-provider, then pick a model with /model.
zc-model-picker-title = Select model
zc-model-provider-picker-title = Select model_provider
zc-model-switch-applying = Applying model change…
zc-model-switch-model-ok = Model switched to { $model }.
zc-model-switch-provider-ok = model_provider switched to { $provider } (model: { $model }).
zc-model-switch-failed = Model switch failed: { $error }
zc-model-catalog-no-provider = Could not resolve this agent's model_provider from config.
zc-model-catalog-empty = No models available for the active model_provider.
zc-model-catalog-loading = Loading models…
zc-model-provider-catalog-failed = Could not load model_providers: { $error }

zc-chat-label-you = You:
zc-chat-label-agent = Agent:

zc-chat-loading-agents = Loading agents…
zc-chat-loading-agents-msg = Loading agents...
zc-chat-picker-header = Select an agent
zc-chat-picker-header-hint = ({ $keys })

zc-chat-help-navigate = Navigate
zc-chat-help-select-agent = Select agent
zc-chat-help-quit = Quit
zc-chat-help-switch-session = Switch session
zc-chat-help-close = Close
zc-chat-help-cancel = Cancel
zc-chat-help-approve = Approve
zc-chat-help-always-approve = Always approve
zc-chat-help-deny = Deny
zc-chat-help-cancel-turn = Cancel turn
zc-chat-help-move-up = Move cursor up
zc-chat-help-move-down = Move cursor down
zc-chat-help-extend-selection = Extend selection
zc-chat-help-yank-selection = Yank selection
zc-chat-help-return-to-input = Return to input
zc-chat-help-browse-mode = Browse mode
zc-chat-help-scroll-conversation = Scroll conversation
zc-chat-help-toggle-thoughts = Toggle thoughts
zc-chat-help-new-session = New session
zc-elicit-help-toggle = Toggle choice
zc-elicit-help-confirm = Confirm choice
zc-elicit-help-cancel = Cancel

zc-chat-approval-title = Approve tool call: { $tool }  [{ $secs }s]
zc-chat-approval-action-allow = Allow
zc-chat-approval-action-always = Always
zc-chat-approval-action-reject = Reject
zc-chat-approval-action-edit = Edit


zc-chat-clipboard-you = You: { $text }
zc-chat-clipboard-agent = Agent: { $text }
zc-chat-copied-clipboard = Copied to clipboard

zc-config-breadcrumb-root = Config
zc-config-section-detail-hint = { $open } or { $into } to open this section
zc-config-breadcrumb-new = New

zc-config-personality-over-limit = Over { $limit } char limit — cannot save
zc-config-alias-create-hint = Enter a name for the new alias
zc-config-personality-help-blurb = Personality files shape your agent's voice and context.
zc-config-skills-help-blurb = Skills in this bundle. { $enter_chord } to edit SKILL.md, { $archive_chord } to archive.

zc-config-field-type-prefix = Type:
zc-config-field-type-secret-suffix = (secret — input hidden)
zc-config-field-type-string-array-suffix = (one entry per line; { $newline_chord }=new line, { $save_chord }=save)

zc-config-help-navigate = Navigate
zc-config-help-switch-section = Switch config section
zc-config-help-open-section = Open section
zc-config-help-clear-filter = Clear filter
zc-config-help-this-help = This help
zc-config-help-filter = Filter
zc-config-help-quit = Quit
zc-config-help-mouse-label = Mouse
zc-config-help-mouse-open = Click, scroll, double-click to open
zc-config-help-mouse-tabs-edit = Click, scroll, click tabs, double-click to edit
zc-config-help-mouse-edit = Click, scroll, double-click to edit
zc-config-help-mouse-save = Click, scroll, double-click to save
zc-config-help-mouse-tabs = Click, scroll, click tabs
zc-config-help-open-type = Open type
zc-config-help-back = Back
zc-config-help-open-alias = Open alias
zc-config-help-delete-alias = Delete alias
zc-config-help-create-alias = Create alias
zc-config-help-cancel = Cancel
zc-config-help-edit-field = Edit field
zc-config-help-save = Save
zc-config-help-back-to-files = Back to files
zc-config-help-switch-tabs = Switch tabs
zc-config-help-edit-file = Edit file
zc-config-help-fill-from-template = Fill from template
zc-config-help-edit-skill = Edit skill
zc-config-help-archive-skill = Archive skill
zc-config-help-back-to-skills = Back to skills
zc-config-help-save-selection = Save selection
zc-config-help-new-line-entry = New line (new entry)
zc-config-help-save-array = Save array
zc-config-help-save-value = Save value
zc-config-help-reset-default = Reset to default

zc-config-status-alias-empty = Alias name cannot be empty
zc-config-status-alias-deleted = Deleted { $alias }
zc-config-status-alias-create-failed = Create failed: { $err }
zc-config-status-delete-failed = Delete failed: { $err }
zc-config-status-field-reset = Reset { $prop }
zc-config-status-load-failed = Load failed: { $err }
zc-config-status-save-failed = Save failed: { $err }
zc-config-status-personality-loading-file = Loading { $filename }...
zc-config-status-personality-saving-file = Saving { $filename }...
zc-config-status-personality-saved-file = Saved { $filename }
zc-config-status-template-loaded = Template loaded for { $filename }
zc-config-status-template-missing = No template available for { $filename }
zc-config-status-template-fetch-failed = Template fetch failed: { $err }
zc-config-status-skill-loading = Loading { $name }...
zc-config-status-skill-saving = Saving { $name }...
zc-config-status-skill-saved = Saved { $name }
zc-config-status-skill-deleting = Deleting { $name }...
zc-config-status-skill-archived = Archived { $name }
zc-config-status-fetching-models = Fetching models for { $family }...
zc-config-status-field-set = Set { $prop }
zc-config-status-invalid-float = Invalid value for { $prop }: expected a number
zc-config-status-invalid-integer = Invalid value for { $prop }: expected an integer
zc-config-status-set-failed = Set failed: { $err }
zc-config-status-loading-personality = Loading personality files...
zc-config-status-loading-skills = Loading skills...
zc-config-status-fetching-templates = Fetching templates...
zc-config-status-unsaved-discarded = Unsaved changes discarded
zc-config-status-no-models = No models returned — enter manually
zc-config-status-model-fetch-failed = Model fetch failed — enter manually
zc-config-status-loading-aliases = Loading options...
zc-config-status-no-aliases = None configured yet. Enter manually.
zc-config-status-alias-fetch-failed = Options fetch failed. Enter manually.

zc-config-footer-action-create = create
zc-config-footer-action-cancel = cancel
zc-config-footer-action-save = save
zc-config-footer-action-edit = edit
zc-config-footer-action-reset = reset
zc-config-footer-action-clear-filter = clear filter
zc-config-footer-action-back-to-files = back to files
zc-config-footer-action-back-to-skills = back to skills
zc-config-footer-action-help = help
zc-config-footer-action-new-line = new line

## Inline hint shown on the selected config field row. The { $keys } placeholder
## is resolved from the current keybinding for ConfigTabAction::Enter.
zc-config-field-edit-hint = { $keys } → press to edit
