# English tool descriptions (default locale, embedded at compile time)
#
# Keys follow the pattern: tool-{name-with-hyphens}
# e.g. "file_read" → "tool-file-read", "web_search_tool" → "tool-web-search-tool"
#
# Literal { and } in values must be escaped as {"{"}  and  {"}"} respectively.

# Android UI bridge (android_* tools). Only registered when [android]
# enabled = true and the process is running on Android.
tool-android-screenshot = Capture the Android screen through the in-app bridge and attach it for visual inspection. Use this to see what is currently on screen before acting.
tool-android-screenshot-param-max-width = Maximum capture width in pixels; the image is downscaled to fit. Defaults to the configured android.screenshot_max_width.
tool-android-screenshot-captured = Captured Android screen: { $width }x{ $height }, { $kb } KB, saved to { $path }
tool-android-screenshot-error-missing-png = The Android bridge returned no image data for the screenshot.
tool-android-screenshot-error-too-large = The Android bridge returned an oversized screenshot ({ $bytes } base64 bytes, maximum { $max }).
tool-android-screenshot-error-decode = Could not decode the screenshot returned by the Android bridge: { $error }
tool-android-screenshot-error-write = Could not write the screenshot to { $path }: { $error }

tool-android-ui-read = Read the Android screen's accessibility tree: visible text, content descriptions, resource IDs, and precomputed tap coordinates for every interactive element. Use this to locate what to tap before calling android_action.
tool-android-ui-read-param-max-depth = Maximum accessibility-tree depth to walk. Defaults to 15.
tool-android-ui-read-foreground = Foreground app: { $package }
tool-android-ui-read-dialog = System dialog present (kind: { $kind }); buttons: { $buttons }
tool-android-ui-read-node-count = { $count } interactive or labelled nodes:
tool-android-ui-read-truncated = Output truncated after { $shown } of { $total } nodes to stay within the size limit.

tool-android-action = Control an ordinary Android app: tap, swipe, scroll, type text, or press a navigation key. Every call must name the expected foreground package and is refused if the screen moved.
tool-android-action-param-action = Which UI action to perform.
tool-android-action-param-x = X coordinate for tap, or an optional scroll anchor.
tool-android-action-param-y = Y coordinate for tap, or an optional scroll anchor.
tool-android-action-param-x1 = Swipe start X coordinate.
tool-android-action-param-y1 = Swipe start Y coordinate.
tool-android-action-param-x2 = Swipe end X coordinate.
tool-android-action-param-y2 = Swipe end Y coordinate.
tool-android-action-param-duration = Swipe duration in milliseconds. Defaults to 300.
tool-android-action-param-direction = Scroll direction.
tool-android-action-param-text = Text to type for the 'text' action, or the visible label to tap for the 'tap' action.
tool-android-action-param-key = Hardware or navigation key to press.
tool-android-action-did-tap-xy = Tapped ({ $x }, { $y }).
tool-android-action-did-tap-text = Tapped the element labelled "{ $text }".
tool-android-action-did-swipe = Swiped from ({ $x1 }, { $y1 }) to ({ $x2 }, { $y2 }).
tool-android-action-did-scroll = Scrolled { $direction }.
tool-android-action-did-text = Typed { $chars } characters into the focused field.
tool-android-action-did-key = Pressed the { $key } key.
tool-android-action-error-missing-int = The '{ $param }' parameter is required and must be an integer.
tool-android-action-error-missing-str = The '{ $param }' parameter is required and must be a non-empty string.
tool-android-action-error-coord-range = The '{ $param }' coordinate { $value } is out of range (0 to { $max }).
tool-android-action-error-enum = '{ $value }' is not a valid { $param }. Choose one of: { $allowed }.
tool-android-action-error-text-too-long = Text is too long ({ $bytes } bytes, maximum { $max }).
tool-android-action-error-bad-package = '{ $package }' is not a valid expected Android package name.

tool-android-dialog = Confirm or deny a currently visible Android system permission/install dialog. This privileged action is separate from ordinary UI control and must remain operator-approved.
tool-android-dialog-param-button = Decision to make: allow or deny.
tool-android-dialog-param-expect-package = Expected system-dialog package, such as com.android.permissioncontroller.
tool-android-dialog-error-button = The dialog button must be 'allow' or 'deny'.
tool-android-dialog-error-package = android_dialog requires the expected foreground system package.
tool-android-dialog-did-click = Clicked the privileged system dialog's "{ $button }" decision.

tool-android-launch = Launch an Android app by package name, optionally targeting a specific activity.
tool-android-launch-param-package = Application package name, such as 'com.android.settings'.
tool-android-launch-param-activity = Optional fully-qualified activity to launch within the package.
tool-android-launch-launched = Launched { $package }.
tool-android-launch-error-missing-package = The 'package' parameter is required.
tool-android-launch-error-bad-package = '{ $package }' is not a valid Android package name.

tool-android-error-connect = Could not reach the Android UI bridge at { $path }: { $error }. Check that the Android app is running and that its accessibility service is enabled.
tool-android-error-connect-timeout = Timed out connecting to the Android UI bridge at { $path }.
tool-android-error-read = Failed reading the Android UI bridge response: { $error }
tool-android-error-timeout = The Android UI bridge did not respond within { $secs } seconds.
tool-android-error-empty-response = The Android UI bridge closed the connection without responding.
tool-android-error-bad-response = The Android UI bridge returned a malformed response: { $error }
tool-android-error-request-too-large = Request is too large for the Android UI bridge ({ $bytes } bytes, maximum { $max }).
tool-android-error-response-too-large = The Android UI bridge response exceeded the { $max } byte limit.
tool-android-error-bridge = Android UI bridge error [{ $code }]: { $message }

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
tool-browser-screenshot-error-path-not-allowed = Screenshot path '{ $path }' is not in the workspace allowlist
tool-browser-screenshot-error-parent-not-exist = Screenshot path '{ $path }' parent directory '{ $parent }' does not exist
tool-browser-screenshot-error-path-outside-workspace = Screenshot path '{ $path }' resolves to '{ $canonical }' which is outside the workspace
tool-browser-screenshot-error-missing-filename = Screenshot path '{ $path }' is missing a filename component
tool-browser-screenshot-error-runtime-config-target = Cannot write screenshot to runtime config path '{ $target }'
tool-browser-screenshot-error-symlink-target = Cannot write screenshot to symlink target '{ $target }'
tool-browser-screenshot-error-path-not-utf8 = Screenshot path '{ $path }' resolves to a non-UTF-8 pathname; refusing to write through a lossy conversion
tool-browser-screenshot-error-computeruse-non-string-path = Screenshot 'path' parameter must be a string, got { $path }
tool-browser-screenshot-error-non-string-path = Screenshot 'path' must be a string or absent
tool-browser-screenshot-error-args-not-object = Screenshot arguments must be a JSON object
tool-browser-screenshot-error-sidecar-no-png-data = computer-use sidecar did not return PNG data
tool-browser-screenshot-error-sidecar-empty-png = computer-use sidecar returned an empty screenshot payload
tool-browser-screenshot-error-sidecar-not-png = computer-use sidecar returned a non-PNG screenshot payload
tool-browser-screenshot-error-sidecar-non-json-success = computer-use sidecar returned a non-JSON success response for a path-bearing screenshot; the requested file was not written

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
tool-web-search-tool-error-duckduckgo-blocked = DuckDuckGo is rate-limiting this machine. Do not retry or rephrase the search; wait a few minutes, fetch known URLs directly with web_fetch, or configure SearXNG, Brave, or Tavily as the web_search provider.
tool-web-search-tool-error-searxng-not-configured = SearXNG instance URL not configured. Set [web_search] searxng_instance_url in config.toml, or override it with the ZEROCLAW_web_search__searxng_instance_url environment variable.
tool-web-search-tool-note-truncated-results = (further results omitted)

tool-workspace = Manage multi-client workspaces. Subcommands: list, switch, create, info, export. Each workspace provides isolated memory, audit, secrets, and tool restrictions.

tool-weather = Get current weather conditions and forecast for any location worldwide. Supports city names (in any language or script), IATA airport codes (e.g. 'LAX'), GPS coordinates (e.g. '51.5,-0.1'), postal/zip codes, and domain-based geolocation. Returns temperature, feels-like, humidity, wind speed/direction, precipitation, visibility, pressure, UV index, and cloud cover. Optional 0-3 day forecast with hourly breakdown. Units default to metric (°C, km/h, mm) but can be set to imperial (°F, mph, inches) per request. No API key required.

tool-android-action-param-expect-package = Required package name the caller believes is on screen (for example com.android.settings). The bridge revalidates it immediately before acting so a tap never lands in an app that drifted forward.
tool-android-error-wrong-foreground = Refused: expected { $expected } to be on screen but { $actual } is foreground. Take a screenshot and re-orient before acting.

tool-android-device = Read a device fact from the Android phone: attached sensors, last known location, or telephony/carrier state. Reads only; it never changes the device or touches the screen.
tool-android-device-param-what = Which fact to read: sensors, location, or telephony.
tool-android-device-error-missing-what = android_device needs 'what' (sensors, location, or telephony).
tool-android-device-error-bad-what = Unsupported device fact '{ $what }'. Supported: { $supported }.
tool-android-device-error-refused = Could not read { $what }: { $reason }
tool-android-device-read = Read Android { $what }.
