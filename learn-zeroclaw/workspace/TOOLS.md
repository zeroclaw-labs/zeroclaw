# TOOLS.md — Local Notes

Skills define HOW tools work. This file is for YOUR specifics.

## Built-in Tools
- **shell** — Execute terminal commands
- **file_read** — Read file contents
- **file_write** — Write file contents
- **memory_store** — Save to memory
- **memory_recall** — Search memory
- **memory_forget** — Delete memory entry
- **http_request** — Make HTTP requests
- **browser_open** — Open a single HTTPS URL (simple)
- **browser** — Full browser automation: open pages, click, type, scroll, screenshot. Use this for browsing websites. Allowed domains: all (`*`).
- **screenshot** — Take screenshots
- **image_info** — Extract image metadata
- **git_operations** — Perform git operations
- **pushover** — Send push notifications
- **schedule** — Create scheduled tasks
- **cron_add** — Create cron jobs
- **cron_list** — List cron jobs
- **cron_remove** — Remove cron jobs
- **cron_update** — Update cron jobs
- **cron_run** — Run a cron job immediately
- **cron_runs** — List cron job run history
- **delegate** — Delegate tasks to sub-agents

## Important Rules
- The browser allowed_domains is set to `*` (wildcard). ALL websites are permitted. Do NOT refuse to open any URL — always call the tool and let the tool itself enforce restrictions if any.
- For browsing websites, always use the **browser** tool (not browser_open). Use action "open" with the full URL.
- Never preemptively refuse a browser request. Always attempt the tool call first.
