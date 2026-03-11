# Notion Setup

Connect ZeroClaw to your Notion workspace for reading pages, querying databases, and optionally running an automated task queue. Uses Notion API version `2025-09-03`.

## 1. What this integration does

- **Tool mode** (default): The agent can search your workspace, read pages (properties and markdown), query data sources (databases), create/update/trash pages, manage blocks, and more — all via the `notion` tool with 14 actions.
- **Channel mode** (optional): A polling channel watches a Notion database for rows with a "Pending" status, dispatches them to the agent, and writes the response back.

## 2. Create a Notion integration

1. Go to <https://notion.com/my-integrations> and click **New integration**.
2. Name it (e.g. `zeroclaw`), select your workspace.
3. Under **Capabilities**, enable the ones you need:

| Notion Capability | Config field | Controls |
|---|---|---|
| **Read content** | `allow_read` | `search`, `list_pages`, `read_page`, `get_blocks`, `get_page_markdown`, `query_database` |
| **Insert content** | `allow_insert` | `create_page`, `create_data_source`, `append_blocks` |
| **Update content** | `allow_update` | `update_page`, `update_block`, `update_page_markdown`, `trash_page`, `delete_block` |

1. Click **Submit** and copy the token (starts with `ntn_` or `secret_`).

## 3. Share content with the integration

Notion integrations can only access pages explicitly shared with them:

1. Open each page or database you want ZeroClaw to access.
2. Click **"..."** (top-right) → **"Connections"** → search for your integration name → add it.

## 4. Configuration

Add to `~/.zeroclaw/config.toml`:

### Tool-only mode (no database polling)

```toml
[channels_config.notion]
enabled = true
api_key = "ntn_your_token_here"

# Permission scoping — match your Notion integration capabilities.
# All default to true. Set to false to block actions the key cannot perform.
allow_read = true       # Read content
allow_insert = true     # Insert content
allow_update = true     # Update content (includes trash and delete)
```

### Read-only example

```toml
[channels_config.notion]
enabled = true
api_key = "ntn_your_token_here"
allow_read = true
allow_insert = false
allow_update = false
```

### Channel + tool mode (database task queue)

```toml
[channels_config.notion]
enabled = true
api_key = "ntn_your_token_here"
database_id = "your-32-char-hex-database-id"
poll_interval_secs = 5
status_property = "Status"
input_property = "Input"
result_property = "Result"
pending_value = "Pending"
running_value = "Running"
done_value = "Done"
error_value = "Error"
status_type = "select"       # "select" or "status" (Notion property type)
recover_stale = true          # reset stuck "Running" rows on startup
```

Field reference:

| Key | Required | Default | Purpose |
|---|---|---|---|
| `enabled` | No | `false` | Enable the Notion integration |
| `api_key` | Yes | — | Notion internal integration token; falls back to `NOTION_API_KEY` env var |
| `allow_read` | No | `true` | Enable Notion "Read content" actions |
| `allow_insert` | No | `true` | Enable Notion "Insert content" actions |
| `allow_update` | No | `true` | Enable Notion "Update content" actions (includes trash/delete) |
| `database_id` | No | — | Database to poll; omit for tool-only mode |
| `data_source_id` | No | — | Data source ID for queries (auto-resolved from `database_id` if omitted) |
| `poll_interval_secs` | No | `5` | Seconds between poll cycles |
| `status_property` | No | `"Status"` | Name of the status property in the database |
| `input_property` | No | `"Input"` | Property the agent reads as the prompt |
| `result_property` | No | `"Result"` | Property where the agent writes its answer |
| `pending_value` | No | `"Pending"` | Status value meaning "ready for agent" |
| `running_value` | No | `"Running"` | Status value while processing |
| `done_value` | No | `"Done"` | Status value on success |
| `error_value` | No | `"Error"` | Status value on failure |
| `status_type` | No | `"select"` | Notion property type: `select` or `status` |
| `recover_stale` | No | `true` | Reset stuck "Running" rows to "Pending" on startup |

Environment override:

- `NOTION_API_KEY` overrides `api_key` when the config field is empty.

## 5. Database schema (channel mode only)

When using channel mode, your Notion database needs these properties:

| Property | Type | Options / Purpose |
|---|---|---|
| Input | Title or Rich text | The prompt/task for the agent |
| Status | Select or Status | Values: `Pending`, `Running`, `Done`, `Error` |
| Result | Rich text | Agent writes its response here |

The property names must match `status_property`, `input_property`, and `result_property` in your config.

## 6. Tool actions

The `notion` tool exposes 14 actions grouped by Notion capability:

### Read content (`allow_read`)

| Action | Parameters | Purpose |
|---|---|---|
| `search` | `query`, `filter`, `sort`, `start_cursor`, `page_size` | Search workspace. Filter by `{"property": "object", "value": "page"}` or `"data_source"`. |
| `list_pages` | `start_cursor`, `page_size` | List all pages the integration can access (shortcut for search with page filter) |
| `read_page` | `page_id` | Get a page's properties |
| `get_blocks` | `block_id` | Get page content (paragraphs, headings, etc.) |
| `get_page_markdown` | `page_id` | Get page content as markdown |
| `query_database` | `data_source_id` or `database_id`, `filter`, `sorts` | Query a data source with optional filters/sorts |

### Insert content (`allow_insert`)

| Action | Parameters | Purpose |
|---|---|---|
| `create_page` | `database_id`, `properties` | Create a page in a database |
| `create_data_source` | `page_id`, `title`, `properties`, `is_inline` | Create a new database inside a page |
| `append_blocks` | `block_id`, `children` | Add content blocks to a page |

### Update content (`allow_update`)

| Action | Parameters | Purpose |
|---|---|---|
| `update_page` | `page_id`, `properties` | Update a page's properties |
| `update_block` | `block_id`, `block_content` | Edit a block's content |
| `update_page_markdown` | `page_id`, `markdown_body` | Insert or replace page content via markdown |
| `trash_page` | `page_id` | Move a page to trash |
| `delete_block` | `block_id` | Move a block to trash |

All mutating actions are also blocked when `[autonomy] level = "read_only"`.

## 7. API version 2025-09-03 — key differences

In Notion API `2025-09-03`, databases are called **data sources** in certain endpoints:

- Each database has both a `database_id` and a `data_source_id`.
- Use `database_id` when creating pages (`parent: {"database_id": "..."}`).
- Use `data_source_id` when querying (`POST /data_sources/{id}/query`).
- The tool auto-resolves `data_source_id` from `database_id` if only `database_id` is provided.
- Search filter uses `"data_source"` (not `"database"`): `{"property": "object", "value": "data_source"}`.

## 8. Finding your database ID

From a Notion database URL:

```
https://www.notion.com/workspace/abc123def456...?v=...
                              ^^^^^^^^^^^^^^^^
                              this is your database_id
```

Or use the tool itself:
- Ask the agent to `search` for the database name — the response includes both IDs.
- Use `list_pages` to see all accessible pages.
- Use `search` with `{"filter": {"property": "object", "value": "data_source"}}` to list only databases.

## 9. Quick validation

### Diagnostic script

```bash
export NOTION_API_KEY="ntn_your_key_here"
./scripts/notion-check.sh
```

Runs 6 checks: token validation, accessible content, pages, data sources, read capability, and markdown read.

### Channel doctor

```bash
zeroclaw channel doctor
```

- **Channel mode**: Shows `Notion   healthy` (polls the database).
- **Tool-only mode**: Shows `Notion   healthy (tool-only)` (verifies API key via search).

### Manual checklist

1. Run `zeroclaw channel doctor` — Notion should show as `healthy`.
2. Run `zeroclaw chat` and ask: `"List my Notion pages"` or `"Search my Notion workspace"`.
3. Verify results contain pages you shared with the integration.
4. (Channel mode) Create a row with `Status = Pending` and an `Input` value, verify the agent picks it up.

## 10. Troubleshooting

- **"Notion API error 401"**: Invalid API key, or token has been revoked. Regenerate at notion.so/my-integrations.
- **Empty search results**: Pages/databases not shared with the integration. Click "..." → "Connections" → add your integration on each item.
- **"Mutating operations are not allowed in read-only autonomy mode"**: `[autonomy] level = "read_only"`. Change to `"supervised"` or `"full"`.
- **"Notion Insert content capability is disabled"**: Set `allow_insert = true` in config, and enable "Insert content" on your Notion integration.
- **"Notion Update content capability is disabled"**: Set `allow_update = true` in config, and enable "Update content" on your Notion integration.
- **Channel polls but finds nothing**: Check that `status_property`, `pending_value`, and `status_type` match your database exactly (case-sensitive).
- **"Notion API error 429"**: Rate limited. Increase `poll_interval_secs` or reduce concurrent requests.
- **Stale "Running" rows**: Enable `recover_stale = true` (default) to auto-reset on startup.
- **"No API key" in doctor**: Set `api_key` in config or export `NOTION_API_KEY` env var.
