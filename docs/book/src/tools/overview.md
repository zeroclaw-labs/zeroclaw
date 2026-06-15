# Tools: Overview

**Tools** are the agent's hands. A tool is a capability the model can invoke mid-conversation, run a shell command, fetch an HTTP URL, extract a PDF, open a browser, write a file, read a sensor. Every tool call is subject to [security policy](../security/overview.md) and produces a [tool receipt](../security/tool-receipts.md).

Tools are not to be confused with `zeroclaw` CLI subcommands. CLI commands are for operators; tools are for the agent.

An agent gets its tools through the skill, knowledge, and MCP bundles it references; see [Agents](../agents/overview.md) for how bundles attach to an agent.

## Built-in tools

A minimal build ships with:

| Tool | What it does |
|---|---|
| `shell` | Execute a shell command in the workspace directory. Subject to command allow/deny lists |
| `file_read` | Read a file with line numbers; supports partial reads and PDF text extraction (path must be inside the workspace unless autonomy permits otherwise) |
| `file_write` | Write a file (same path constraint) |
| `file_edit` | Replace an exact string match in a file with new content |
| `glob_search` | List files matching a glob pattern within the workspace |
| `content_search` | Search file contents by regex within the workspace (ripgrep with grep fallback) |
| `http_request` | HTTP GET/POST/PUT/DELETE/PATCH/HEAD/OPTIONS to allowlisted domains |
| `web_search_tool` | Web search. Provider is configurable: DuckDuckGo (default, no key), Brave, Tavily, SearXNG, or Jina |
| `web_fetch` | Fetch a page and return clean plain text |
| `browser` | Headless-browser automation. See [Browser automation](./browser.md) |
| `memory_recall` | Search long-term memory for relevant facts, preferences, or context |
| `memory_store` | Store a fact, preference, or note in long-term memory |
| `ask_user` | Send a question to the active channel and wait for a reply. Supports optional `choices` for structured responses (inline keyboard on Telegram, numbered list on CLI). On ACP, `choices` are required: free-form ask awaits the ACP elicitation RFD. Parameters: `question` (required), `choices` (optional list), `timeout_secs` (default 600). |
| `escalate_to_human` | Send a structured escalation message with urgency routing. `high` / `critical` urgency additionally notifies any channels listed in `[escalation] alert_channels`. Parameters: `summary` (required), `context` (optional), `urgency` (`low`/`medium`/`high`/`critical`, default `medium`), `wait_for_response` (bool, default false), `timeout_secs` (default 600). On ACP, `wait_for_response: true` fails immediately if the channel cannot receive free-form replies (awaits ACP elicitation RFD). |

Always registered alongside the built-ins:

| Tool | Notes |
|---|---|
| `cron_*` | Manage scheduled jobs: `cron_add`, `cron_list`, `cron_remove`, `cron_update`, `cron_run`, `cron_runs` |
| `schedule` | Shell-only one-shot/recurring scheduling |
| `memory_forget`, `memory_export`, `memory_purge` | Long-term memory management |
| `spawn_subagent`, `delegate` | Run a subtask in a child agent |

Conditionally registered:

| Tool | Enabled by |
|---|---|
| Hardware probes | `--features hardware`: GPIO, I2C, SPI reads/writes |
| `pdf_read` | `--features rag-pdf` |
| `sop_*` tools | Registered when `sop.sops_dir` is configured: run and inspect SOPs |
| `discord_search` | Registered when a Discord alias has `archive` enabled |

## Extension protocols

Beyond built-in tools, ZeroClaw supports the **[MCP](./mcp.md)** (Model Context Protocol) extension surface. Connect any MCP server (Claude Code's filesystem, Playwright, your own) and the agent picks up its tools at startup.

For IDE-side integration where an editor drives ZeroClaw as a subprocess, see [ACP](../channels/acp.md): Agent Client Protocol lives under channels since it's an inbound session-management surface, not a tool the agent invokes.

## Authoring a tool

Implement the `Tool` trait in `zeroclaw-api`:

```rust
#[async_trait]
pub trait Tool: Send + Sync + Attributable {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> serde_json::Value;   // JSON Schema for args
    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult>;
}
```

Every `Tool` is also `Attributable`, so a tool call's log emissions and audit traces carry the same `<kind>.<alias>` attribution the rest of the runtime uses.

Register via the runtime's tool factory. See [Developing → Plugin protocol](../developing/plugin-protocol.md) for the full pattern.

## Describing tools to the model

Tool descriptions are [Mozilla Fluent](https://projectfluent.org/) strings: one per tool, localised per locale. This keeps tool descriptions terse in the model's context window while allowing UI localisation.

Source of truth: `crates/zeroclaw-runtime/locales/en/tools.ftl`. Translations are generated and maintained via `cargo fluent fill --locale <code>` (see [Maintainers → Docs & Translations](../maintainers/docs-and-translations.md)).

## Risk and approval

Every tool invocation is classified by risk:

- **Low** (read-only, no side effects): `file_read`, `memory_recall`, `http_request GET` to allowed domains
- **Medium** (mutates local state): `file_write`, `shell` with known safe commands
- **High** (destructive or remote side effects): `shell` with unknown commands, `http_request POST` to unconstrained URLs

The [autonomy level](../security/autonomy.md) determines what each risk tier can do without operator approval. Default (`Supervised`): low runs, medium asks, high blocks.

Every tool invocation, approved or blocked, produces a [tool receipt](../security/tool-receipts.md) in the audit log.

## Disabling tools on non-CLI channels

The schema has no per-channel `tools_allow` / `tools_deny` field. Tool gating lives on the agent's risk profile (`[risk_profiles.<alias>]`):

- `excluded_tools` removes the listed tools from every non-CLI channel (Discord, Telegram, Bluesky, Matrix, Slack, etc.) while leaving the local CLI untouched. The granularity is binary (CLI vs non-CLI), not per-channel. It also subtracts from the agentic-delegate allow-list resolved at runtime, which is the only way to block individual `<server>__<tool>` MCP names that would otherwise be auto-admitted by the rule below.
- `allowed_tools` is the inverse: an allowlist of tools the agent may call in agentic mode (empty means no authorization constraint).
- **MCP exception**: when `allowed_tools` is non-empty, runtime-discovered MCP tools (any name containing `__`, the `<server>__<tool>` convention) are auto-admitted into the effective allow-list without having to be listed there individually. This keeps the post-#7464 eager-MCP default usable for agents that already pin an explicit allow-list. To block individual MCP tools, list them in `excluded_tools`. An explicit empty list (`allowed_tools = []`) still denies everything, MCP included.
- The MCP exception is scoped to the **risk profile**'s `allowed_tools` only. Caller-supplied per-run allow-lists (cron job `allowed_tools`, narrowed delegate invocations, etc.) are still treated as strict explicit-list intersections — a job that narrows itself to `allowed_tools = ["cron_add"]` will not surface runtime-discovered MCP wrappers it did not name, even when the agent's risk profile would auto-admit them.

If you need finer-grained gating, drop the profile's `level` to `read_only` or `supervised` and rely on the per-profile `auto_approve` / `always_ask` lists to gate sensitive tools behind operator approval.

See [Autonomy levels](../security/autonomy.md) for the full set of per-profile fields.

## See also

- [MCP](./mcp.md)
- [ACP](../channels/acp.md)
- [Browser automation](./browser.md)
- [Security → Overview](../security/overview.md)
