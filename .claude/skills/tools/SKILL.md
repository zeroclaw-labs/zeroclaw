---
name: tools
description: "Tool skill discovery and usage guide index for ZeroClaw. Each tool has an associated skill file that documents usage patterns, best practices, and implementation details. Trigger when working on tool behavior, adding new tools, or modifying existing tool implementations."
---

# Tool Skills

Each ZeroClaw tool has an associated skill file documenting its location, purpose, and configuration. Use this index to find the right skill.

## How Tool Skills Work

Tool skills serve two purposes:

1. **For the ZeroClaw runtime agent**: Tool skill files appear as regular skills in `<available_skills>` in the system prompt. The agent calls `read_skill("tool:<name>")` to load full instructions.

2. **For Claude Code (this context)**: Tool skills live in `.claude/skills/tools/` and document implementations for easy developer reference.

## Tool Index

### Core

| Tool | File | Description |
|------|------|-------------|
| `shell` | shell.md | Shell command execution |
| `file_read` | file_read.md | Read file contents |
| `file_write` | file_write.md | Write file contents |
| `file_edit` | file_edit.md | Edit files |
| `glob_search` | glob_search.md | Find files by glob pattern |
| `content_search` | content_search.md | Search file contents by regex |
| `tool_search` | tool_search.md | Search available tools |
| `read_skill` | read_skill.md | Read skill instructions |

### Cron

| Tool | File | Description |
|------|------|-------------|
| `cron_add` | cron_add.md | Create cron job |
| `cron_list` | cron_list.md | List cron jobs |
| `cron_remove` | cron_remove.md | Remove cron job |
| `cron_update` | cron_update.md | Update cron job |
| `cron_run` | cron_run.md | Run cron job manually |
| `cron_runs` | cron_runs.md | View cron job history |

### Memory

| Tool | File | Description |
|------|------|-------------|
| `memory_store` | memory_store.md | Store a memory |
| `memory_recall` | memory_recall.md | Recall memories |
| `memory_forget` | memory_forget.md | Delete a memory |
| `memory_export` | memory_export.md | Export all memories |
| `memory_purge` | memory_purge.md | Purge all memories |

### Sessions

| Tool | File | Description |
|------|------|-------------|
| `sessions_list` | sessions_list.md | List sessions |
| `sessions_history` | sessions_history.md | View session history |
| `sessions_send` | sessions_send.md | Send message to session |
| `sessions_current` | sessions_current.md | Show current session |
| `sessions_reset` | sessions_reset.md | Reset session |
| `sessions_delete` | sessions_delete.md | Delete session |

### Communication

| Tool | File | Description |
|------|------|-------------|
| `ask_user` | ask_user.md | Ask user a question |
| `escalate_to_human` | escalate_to_human.md | Escalate to human |
| `reaction` | reaction.md | Add reaction |
| `channel_room` | channel_room.md | Manage rooms |
| `poll` | poll.md | Create poll |
| `pushover` | pushover.md | Push notification |

### Browser

| Tool | File | Description |
|------|------|-------------|
| `browser_open` | browser_open.md | Open URL (simple) |
| `browser` | browser.md | Full browser automation |
| `browser_delegate` | browser_delegate.md | Delegate browser |

### Web

| Tool | File | Description |
|------|------|-------------|
| `http_request` | http_request.md | HTTP requests |
| `web_fetch` | web_fetch.md | Fetch web content |
| `web_search` | web_search.md | Web search |
| `text_browser` | text_browser.md | Text browser |

### Schedule / Agent

| Tool | File | Description |
|------|------|-------------|
| `schedule` | schedule.md | Schedule future task |
| `spawn_subagent` | spawn_subagent.md | Spawn child agent |
| `send_message_to_peer` | send_message_to_peer.md | Message peer agent |
| `delegate` | delegate.md | Delegate to agent |

### Config

| Tool | File | Description |
|------|------|-------------|
| `model_routing_config` | model_routing_config.md | Model routing config |
| `model_switch` | model_switch.md | Switch model |
| `proxy_config` | proxy_config.md | Proxy config |
| `skill_manage` | skill_manage.md | Manage skills |
| `skill_view` | skill_view.md | View skill |
| `skills_list` | skills_list.md | List skills |

### Integrations

| Tool | File | Description |
|------|------|-------------|
| `notion` | notion.md | Notion API |
| `jira` | jira.md | Jira API |
| `discord_search` | discord_search.md | Discord archive search |
| `email_search` | email_search.md | Email search |
| `email_read` | email_read.md | Email read |
| `google_workspace` | google_workspace.md | Google Workspace |
| `microsoft365` | microsoft365.md | Microsoft 365 |
| `linkedin` | linkedin.md | LinkedIn |
| `composio` | composio.md | Composio integrations |
| `knowledge` | knowledge.md | Knowledge graph |

### Delegation CLIs

| Tool | File | Description |
|------|------|-------------|
| `claude_code` | claude_code.md | Claude Code CLI |
| `claude_code_runner` | claude_code_runner.md | Claude Code runner |
| `codex_cli` | codex_cli.md | Codex CLI |
| `gemini_cli` | gemini_cli.md | Gemini CLI |
| `opencode_cli` | opencode_cli.md | OpenCode CLI |

### Ops

| Tool | File | Description |
|------|------|-------------|
| `backup` | backup.md | Workspace backup |
| `data_management` | data_management.md | Data retention |
| `cloud_ops` | cloud_ops.md | Cloud operations |
| `cloud_patterns` | cloud_patterns.md | Cloud patterns |
| `project_intel` | project_intel.md | Project intelligence |
| `report_template` | report_template.md | Report templates |
| `security_ops` | security_ops.md | Security operations |

### SOP

| Tool | File | Description |
|------|------|-------------|
| `sop_list` | sop_list.md | List SOPs |
| `sop_execute` | sop_execute.md | Execute SOP |
| `sop_advance` | sop_advance.md | Advance SOP step |
| `sop_approve` | sop_approve.md | Approve SOP step |
| `sop_status` | sop_status.md | SOP status |

### Utility

| Tool | File | Description |
|------|------|-------------|
| `calculator` | calculator.md | Math expressions |
| `screenshot` | screenshot.md | Desktop screenshot |
| `image_info` | image_info.md | Image metadata |
| `image_gen` | image_gen.md | Image generation |
| `weather` | weather.md | Weather forecast |
| `canvas` | canvas.md | Visual canvas |
| `execute_pipeline` | execute_pipeline.md | Multi-step pipeline |
| `llm_task` | llm_task.md | LLM sub-task |
| `vi_verify` | vi_verify.md | Verifiable intent |
| `git_operations` | git_operations.md | Git operations |
| `file_upload` | file_upload.md | File upload |
| `file_upload_bundle` | file_upload_bundle.md | Bundle upload |
| `file_download` | file_download.md | File download |
| `pdf_read` | pdf_read.md | PDF extraction |

### Hardware

| Tool | File | Description |
|------|------|-------------|
| `hardware_board_info` | hardware_board_info.md | Board info |
| `hardware_memory_map` | hardware_memory_map.md | Memory map |
| `hardware_memory_read` | hardware_memory_read.md | Memory read |

## ZeroClaw Agent Discovery

The ZeroClaw runtime agent discovers tool skills through:
1. The `<available_skills>` section in its system prompt
2. The `read_skill(name)` tool
3. The `## Tool Skills` section in `system_prompt.rs`

_Note: Tool skills in `.claude/skills/tools/` are for Claude Code developer reference. They are NOT loadable by the ZeroClaw runtime agent's `read_skill` — runtime tool skills must be placed in the agent workspace `skills/` directory following the `tool:<name>` pattern._
