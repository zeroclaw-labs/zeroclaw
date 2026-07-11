# English tool descriptions (default locale, embedded at compile time)
#
# Keys follow the pattern: tool-{name-with-hyphens}
# e.g. "file_read" → "tool-file-read", "web_search_tool" → "tool-web-search-tool"
#
# Literal { and } in values must be escaped as {"{"}  and  {"}"} respectively.

tool-backup = Create, list, verify, and restore workspace backups

tool-browser = Web/browser automation with pluggable backends (agent-browser, rust-native, computer_use). Supports DOM actions plus optional OS-level actions (mouse_move, mouse_click, mouse_drag, key_type, key_press, screen_capture) through a computer-use sidecar. Use 'snapshot' to map interactive elements to refs (@e1, @e2). Enforces browser.allowed_domains for open actions.

tool-browser-delegate = Delegate browser-based tasks to a browser-capable CLI for interacting with web applications like Teams, Outlook, Jira, Confluence

tool-browser-open = Open an approved HTTPS URL in the system browser. Security constraints: allowlist-only domains, no local/private hosts, no scraping.

tool-channel-room = Create rooms and invite users through an active channel. Provide a channel key such as 'matrix.default', action 'create_room' or 'invite_user', and the action-specific room fields.
tool-channel-room-param-action = Room-management action to perform.
tool-channel-room-param-channel = Active channel key such as 'matrix.default'.
tool-channel-room-param-name = Optional room name for create_room.
tool-channel-room-param-topic = Optional room topic for create_room.
tool-channel-room-param-invites = Optional user IDs to invite while creating the room.
tool-channel-room-param-visibility = Optional room visibility for create_room.
tool-channel-room-param-encryption = Whether to request room encryption during create_room.
tool-channel-room-param-room-id = Existing room ID for invite_user.
tool-channel-room-param-user-id = User ID to invite for invite_user.
tool-channel-room-error-security = Action blocked: { $err }
tool-channel-room-error-invalid-action = Invalid action '{ $action }': must be 'create_room' or 'invite_user'.
tool-channel-room-error-not-initialized = No channels available yet (channels not initialized).
tool-channel-room-error-channel-not-found = Channel '{ $channel }' not found. Available channels: { $available }
tool-channel-room-error-create-failed = Failed to create room: { $err }
tool-channel-room-error-invite-failed = Failed to invite user: { $err }
tool-channel-room-error-invites-array = 'invites' must be an array of strings.
tool-channel-room-error-invites-item = 'invites' must be an array of non-empty strings.
tool-channel-room-error-invalid-visibility = Invalid room visibility: { $err }
tool-channel-room-error-missing-param = Missing '{ $param }' parameter.
tool-channel-room-error-string-param = '{ $param }' must be a string.
tool-channel-room-error-bool-param = '{ $param }' must be a boolean.

tool-cloud-ops = Cloud transformation advisory tool. Analyzes IaC plans, assesses migration paths, reviews costs, and checks architecture against Well-Architected Framework pillars. Read-only: does not create or modify cloud resources.

tool-cloud-patterns = Cloud pattern library. Given a workload description, suggests applicable cloud-native architectural patterns (containerization, serverless, database modernization, etc.).

tool-composio = Execute actions on 1000+ apps via Composio (Gmail, Notion, GitHub, Slack, etc.). Use action='list' to see available actions (includes parameter names). action='execute' with action_name/tool_slug and params to run an action. If you are unsure of the exact params, pass 'text' instead with a natural-language description of what you want (Composio will resolve the correct parameters via NLP). action='list_accounts' or action='connected_accounts' to list OAuth-connected accounts. action='connect' with app/auth_config_id to get OAuth URL. connected_account_id is auto-resolved when omitted.

tool-content-search = Search file contents by regex pattern within the workspace. Supports ripgrep (rg) with grep or internal fallback. Output modes: 'content' (matching lines with context), 'files_with_matches' (file paths only), 'count' (match counts per file). Example: pattern='fn main', include='*.rs', output_mode='content'.

tool-cron-add = Create a scheduled cron job (shell or agent) with cron/at/every schedules. Use job_type='agent' with a prompt to run the AI agent on schedule. To deliver output to a channel (Discord, Telegram, Slack, Mattermost, Matrix), set delivery={"{"}"mode":"announce","channel":"discord","to":"<channel_id_or_chat_id>"{"}"}. This is the preferred tool for sending scheduled/delayed messages to users via channels.

tool-cron-list = List all scheduled cron jobs

tool-cron-remove = Remove a cron job by id

tool-cron-run = Force-run a cron job immediately and record run history

tool-cron-runs = List recent run history for a cron job

tool-cron-update = Patch an existing cron job (schedule, command, prompt, enabled, delivery, model, etc.)

tool-data-management = Workspace data retention, purge, and storage statistics

tool-delegate = Delegate a subtask to a specialized agent. Use when: a task benefits from a different model (e.g. fast summarization, deep reasoning, code generation). The sub-agent runs a single prompt by default; with agentic=true it can iterate with a filtered tool-call loop.
tool-delegate-error-background-goal-context = Background delegation is disabled while a durable goal is active; use synchronous delegation until parent-linked completion and usage reporting are available.

tool-file-edit = Edit a file by replacing an exact string match with new content

tool-file-download = Download a file from the configured remote endpoint and write it to the agent's workspace. Supply the identifier of the document to fetch and a workspace-relative destination path; the endpoint URL is fixed by host config and is never model-controlled. Bytes are streamed straight to disk and are not loaded into model context. Returns the HTTP status, the number of bytes written, and the destination path.
tool-file-download-param-document-id = Identifier of the document to fetch from the configured endpoint.
tool-file-download-param-dest-path = Workspace-relative path to write the file to. The parent directory must already exist.
tool-file-download-error-disabled = file_download is disabled: [file_download].url is not configured
tool-file-download-error-read-only = Action blocked: autonomy is read-only
tool-file-download-error-rate-limited-hour = Rate limit exceeded: too many actions in the last hour
tool-file-download-error-rate-limited-budget = Rate limit exceeded: action budget exhausted
tool-file-download-error-missing-document-id = Missing 'document_id' parameter
tool-file-download-error-missing-dest-path = Missing 'dest_path' parameter
tool-file-download-error-invalid-file-name = Invalid dest_path '{ $dest_path }': must end in a concrete file name
tool-file-download-error-no-parent = Invalid dest_path '{ $dest_path }': has no parent directory
tool-file-download-error-resolve-dir = Cannot resolve destination directory for '{ $dest_path }': { $err }
tool-file-download-error-client-build = Failed to build download client: { $err }
tool-file-download-error-request = Download request failed: { $err }
tool-file-download-error-status = Download endpoint returned status { $status }
tool-file-download-error-too-large-reported = Download too large: endpoint reports { $len } bytes (limit: { $limit } bytes)
tool-file-download-error-too-large-stream = Download too large: exceeded limit of { $limit } bytes
tool-file-download-error-temp-create = Failed to create temporary download file: { $err }
tool-file-download-error-read-body = Failed while reading response body: { $err }
tool-file-download-error-write-body = Failed while writing downloaded bytes: { $err }
tool-file-download-error-flush = Failed to flush downloaded file: { $err }
tool-file-download-error-move = Failed to move downloaded file into place: { $err }
tool-file-download-success = Downloaded { $written } bytes to { $dest_path } ({ $status })

tool-file-read = Read file contents with line numbers. Supports partial reading via offset and limit. Binary and image files are rejected (use the image_info tool for images). Set encoding="base64" to return raw bytes base64-encoded (for binary files such as .pdf/.xlsx/.docx); offset/limit are ignored in that mode.

tool-file-write = Write contents to a file in the workspace

tool-git-operations = Perform structured Git operations (status, diff, log, branch, commit, add, checkout, stash). Provides parsed JSON output and integrates with security policy for autonomy controls.
tool-git-operations-error-not-in-repo = Not in a Git repository at '{ $path }'. Choose a path inside a Git worktree, pass 'path' for a repository subdirectory, or initialize a repository before running git_operations.

tool-goal-start = Start a durable goal run. The objective is untrusted user/model text; runtime-owned agent, route, owner, and principal facts are supplied by ZeroClaw.
tool-goal-start-objective-description = Goal objective to pursue.
tool-goal-start-success-continue =
    {$message}
    Continue working on this active goal now; do not stop at an acknowledgement.
tool-goal-start-success-paused =
    {$message}
    Do not continue this goal now; it is not admitted for another autonomous turn.
tool-goal-start-error-empty-objective = goal_start requires a non-empty objective
tool-goal-start-error-missing-context = goal_start is unavailable because trusted runtime admission context was not supplied.
tool-goal-start-error-agent-context-mismatch = goal_start is unavailable because trusted runtime admission context does not match this agent.

tool-goal-objective = Amend the current durable goal objective. The replacement objective is untrusted user/model text; runtime-owned agent, route, owner, and principal facts are supplied by ZeroClaw.
tool-goal-objective-objective-description = Replacement objective text for the current durable goal. Use when new evidence changes the goal scope without requiring cancellation.
tool-goal-objective-success =
    {$message}
    Continue under the amended objective when you are already working in the goal turn; do not start a separate acknowledgement-only response.
tool-goal-objective-error-empty-objective = goal_objective requires a non-empty objective
tool-goal-objective-error-missing-context = goal_objective is unavailable because trusted runtime admission context was not supplied.
tool-goal-objective-error-agent-context-mismatch = goal_objective is unavailable because trusted runtime admission context does not match this agent.

tool-goal-resume = Resume the current paused durable goal run. The optional reason is untrusted user/model text; runtime-owned agent, route, owner, and principal facts are supplied by ZeroClaw.
tool-goal-resume-reason-description = Optional untrusted note explaining what changed since the pause, for example that the external blocker was fixed.
tool-goal-resume-success-continue =
    {$message}
    Continue working on this resumed goal now; do not stop at an acknowledgement.
tool-goal-resume-success-paused =
    {$message}
    Do not continue this goal now; it is not admitted for another autonomous turn.
tool-goal-resume-error-missing-context = goal_resume is unavailable because trusted runtime admission context was not supplied.
tool-goal-resume-error-agent-context-mismatch = goal_resume is unavailable because trusted runtime admission context does not match this agent.

tool-git-forge-error-requires-field = { $resource }.{ $action } requires '{ $field }'.
tool-git-forge-error-requires-number = { $resource }.{ $action } requires 'number'.
tool-git-forge-error-issue-close-reason = issue.close 'reason' must be 'completed' or 'not_planned'.
tool-git-forge-error-pull-merge-method = pull.merge 'method' must be 'merge', 'squash', or 'rebase'.
tool-git-forge-error-review-verdict = review.create 'verdict' must be approve|request_changes|comment, got '{ $verdict }'.
tool-git-forge-error-unknown-cell = unknown or unsupported resource/action '{ $resource }.{ $action }'. Call action 'describe' for the supported grid, or use 'raw' for anything unlisted.
tool-git-forge-error-no-channels = No channels available yet (channels not initialized).
tool-git-forge-error-raw-requires-method = 'raw' requires 'method'.
tool-git-forge-error-raw-requires-path = 'raw' requires 'path'.
tool-git-forge-error-requires-resource = a typed call requires 'resource' (or use action 'raw'/'describe').
tool-git-forge-error-missing-repo = Missing 'repo' (expected 'owner/repo').

tool-glob-search = Search for files matching a glob pattern within the workspace. Returns a sorted list of matching file paths relative to the workspace root. Examples: '**/*.rs' (all Rust files), 'src/**/mod.rs' (all mod.rs in src).

tool-google-workspace = Interact with Google Workspace services (Drive, Gmail, Calendar, Sheets, Docs, etc.) via the gws CLI. Requires gws to be installed and authenticated.

tool-hardware-board-info = Return full board info (chip, architecture, memory map) for connected hardware. Use when: user asks for 'board info', 'what board do I have', 'connected hardware', 'chip info', 'what hardware', or 'memory map'.

tool-hardware-memory-map = Return the memory map (flash and RAM address ranges) for connected hardware. Use when: user asks for 'upper and lower memory addresses', 'memory map', 'address space', or 'readable addresses'. Returns flash/RAM ranges from datasheets.

tool-hardware-memory-read = Read actual memory/register values from Nucleo via USB. Use when: user asks to 'read register values', 'read memory at address', 'dump memory', 'lower memory 0-126', or 'give address and value'. Returns hex dump. Requires Nucleo connected via USB and probe feature. Params: address (hex, e.g. 0x20000000 for RAM start), length (bytes, default 128).

tool-http-request = Make HTTP requests to external APIs. Supports GET, POST, PUT, DELETE, PATCH, HEAD, OPTIONS methods. Security constraints: allowlist-only domains, no local/private hosts, configurable timeout and response size limits.

tool-image-info = Read image file metadata (format, dimensions, size) and optionally return base64-encoded data.

tool-jira = Interact with Jira: read tickets, search with JQL, add comments, list projects and per-issue transitions, transition an issue through its workflow, and create new issues.

tool-knowledge = Manage a knowledge graph of architecture decisions, solution patterns, lessons learned, experts, and relationship links.

tool-linkedin = Manage LinkedIn: create posts, list your posts, comment, react, delete posts, view engagement, get profile info, and read the configured content strategy. Requires LINKEDIN_* credentials in .env file.

tool-discord-search = Search Discord message history stored in discord.db. Use to find past messages, summarize channel activity, or look up what users said. Supports keyword search and optional filters: channel_id, since, until.

tool-memory-forget = Remove a memory by key. Use to delete outdated facts or sensitive data. Returns whether the memory was found and removed.

tool-memory-recall = Search long-term memory for relevant facts, preferences, or context. Returns scored results ranked by relevance. Omit the query or pass bare * to return recent memories.

tool-memory-store = Store a fact, preference, or note in long-term memory. Use category 'core' for permanent facts, 'daily' for session notes, 'conversation' for chat context, or a custom category name.

tool-microsoft365 = Microsoft 365 integration: manage Outlook mail, Teams messages, Calendar events, OneDrive files, and SharePoint search via Microsoft Graph API

tool-model-routing-config = Manage default model settings, scenario-based provider/model routes, classification rules, and aliased agent profiles

tool-notion = Interact with Notion: query databases, read/create/update pages, and search the workspace.


tool-project-intel = Project delivery intelligence: generate status reports, detect risks, draft client updates, summarize sprints, and estimate effort. Read-only analysis tool.

tool-proxy-config = Manage ZeroClaw proxy settings (scope: environment | zeroclaw | services), including runtime and process env application

tool-pushover = Send a Pushover notification to your device. Requires PUSHOVER_TOKEN and PUSHOVER_USER_KEY in .env file.

tool-schedule = Manage scheduled shell-only tasks. Actions: create/add/once/list/get/cancel/remove/pause/resume. WARNING: This tool creates shell jobs whose output is only logged, NOT delivered to any channel. To send a scheduled message to Discord/Telegram/Slack/Matrix, use the cron_add tool with job_type='agent' and a delivery config like {"{"}"mode":"announce","channel":"discord","to":"<channel_id>"{"}"}.

tool-screenshot = Capture a screenshot of the current screen. Returns the file path and base64-encoded PNG data.

tool-security-ops = Security operations tool for managed cybersecurity services. Actions: triage_alert (classify/prioritize alerts), run_playbook (execute incident response steps), parse_vulnerability (parse scan results), generate_report (create security posture reports), list_playbooks (list available playbooks), alert_stats (summarize alert metrics).

tool-shell = Execute a shell command in the workspace directory

tool-sop-advance = Report the result of the current SOP step and advance to the next step. Provide the run_id, whether the step succeeded or failed, and a brief output summary.

tool-sop-approve = Approve a pending SOP step that is waiting for operator approval. Returns the step instruction to execute. Use sop_status to see which runs are waiting.

tool-sop-execute = Manually trigger a Standard Operating Procedure (SOP) by name. Returns the run ID and first step instruction. Use sop_list to see available SOPs.

tool-sop-list = List all loaded Standard Operating Procedures (SOPs) with their triggers, priority, step count, and active run count. Optionally filter by name or priority.

tool-sop-status = Query SOP execution status. Provide run_id for a specific run, or sop_name to list runs for that SOP. With no arguments, shows all active runs.

tool-tool-search = Fetch full schema definitions for deferred MCP tools so they can be called. Use "select:name1,name2" for exact match or keywords to search.

tool-web-fetch = Fetch a web page and return its content as clean plain text. HTML pages are automatically converted to readable text. JSON and plain text responses are returned as-is. Only GET requests; follows redirects. Security: allowlist-only domains, no local/private hosts.

tool-web-search-tool = Search the web for information. Returns relevant search results with titles, URLs, and descriptions. Use this to find current information, news, or research topics.

tool-workspace = Manage multi-client workspaces. Subcommands: list, switch, create, info, export. Each workspace provides isolated memory, audit, secrets, and tool restrictions.

tool-weather = Get current weather conditions and forecast for any location worldwide. Supports city names (in any language or script), IATA airport codes (e.g. 'LAX'), GPS coordinates (e.g. '51.5,-0.1'), postal/zip codes, and domain-based geolocation. Returns temperature, feels-like, humidity, wind speed/direction, precipitation, visibility, pressure, UV index, and cloud cover. Optional 0-3 day forecast with hourly breakdown. Units default to metric (°C, km/h, mm) but can be set to imperial (°F, mph, inches) per request. No API key required.
