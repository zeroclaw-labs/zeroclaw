# Tools — Overview

**Tools** are the agent's hands. A tool is a capability the model can invoke mid-conversation — run a shell command, fetch an HTTP URL, extract a PDF, open a browser, write a file, read a sensor. Every tool call is subject to [security policy](../security/overview.md) and produces a [tool receipt](../security/tool-receipts.md).

Tools are not to be confused with `zeroclaw` CLI subcommands. CLI commands are for operators; tools are for the agent.

## Built-in tools

A minimal build ships with:

| Tool | What it does |
|---|---|
| `shell` | Execute a shell command. Subject to command allow/deny lists |
| `file_read` | Read a file (path must be inside the workspace unless autonomy permits otherwise) |
| `file_write` | Write a file (same path constraint) |
| `file_list` | Directory listing |
| `http` | HTTP GET/POST/... |
| `web_search` | Programmable web search (Brave, Google CSE, Serper) |
| `browser` | Headless-browser automation. See [Browser automation](./browser.md) |
| `pdf_extract` | PDF text extraction |
| `time` | Current date/time (agents are surprisingly bad at knowing this otherwise) |
| `memory_search` | Semantic search across stored conversations |
| `memory_pin` | Mark a fact for long-term retention |
| `ask_user` | Send a question to the active channel and wait for a reply. Supports optional `choices` for structured responses (inline keyboard on Telegram, numbered list on CLI). On ACP, `choices` are required — free-form ask awaits the ACP elicitation RFD. Parameters: `question` (required), `choices` (optional list), `timeout_secs` (default 600). |
| `escalate_to_human` | Send a structured escalation message with urgency routing. `high` / `critical` urgency additionally notifies any channels listed in `[escalation] alert_channels`. Parameters: `summary` (required), `context` (optional), `urgency` (`low`/`medium`/`high`/`critical`, default `medium`), `wait_for_response` (bool, default false), `timeout_secs` (default 600). On ACP, `wait_for_response: true` fails immediately if the channel cannot receive free-form replies (awaits ACP elicitation RFD). |

Optional, feature-gated:

| Tool | Enabled by |
|---|---|
| Hardware probes | `--features hardware` — GPIO, I2C, SPI reads/writes |
| `sop_*` tools | Always on if SOP is configured — run and inspect SOPs |
| `cron_*` tools | Manage scheduled jobs |

## Extension protocols

Beyond built-in tools, ZeroClaw supports the **[MCP](./mcp.md)** (Model Context Protocol) extension surface. Connect any MCP server (Claude Code's filesystem, Playwright, your own) and the agent picks up its tools at startup.

For IDE-side integration where an editor drives ZeroClaw as a subprocess, see [ACP](../channels/acp.md) — Agent Client Protocol lives under channels since it's an inbound session-management surface, not a tool the agent invokes.

## Authoring a tool

Implement the `Tool` trait in `zeroclaw-api`:

```rust
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn schema(&self) -> serde_json::Value;         // JSON Schema for args
    async fn invoke(&self, args: Value, ctx: ToolContext) -> ToolResult;
}
```

Register via the runtime's tool factory. See [Developing → Plugin protocol](../developing/plugin-protocol.md) for the full pattern.

## Describing tools to the model

Tool descriptions are [Mozilla Fluent](https://projectfluent.org/) strings — one per tool, localised per locale. This keeps tool descriptions terse in the model's context window while allowing UI localisation.

Source of truth: `crates/zeroclaw-runtime/locales/en/tools.ftl`. Translations are generated and maintained via `cargo fluent fill --locale <code>` (see [Maintainers → Docs & Translations](../maintainers/docs-and-translations.md)).

## Risk and approval

Every tool invocation is classified by risk:

- **Low** (read-only, no side effects): `file_read`, `memory_search`, `time`, `http GET` to allowed domains
- **Medium** (mutates local state): `file_write`, `shell` with known safe commands
- **High** (destructive or remote side effects): `shell` with unknown commands, `http POST` to unconstrained URLs

The [autonomy level](../security/autonomy.md) determines what each risk tier can do without operator approval. Default (`Supervised`): low runs, medium asks, high blocks.

Every tool invocation — approved or blocked — produces a [tool receipt](../security/tool-receipts.md) in the audit log.

## Disabling tools on non-CLI channels

The schema has no per-channel `tools_allow` / `tools_deny` field. The available mechanism is the global `[autonomy].non_cli_excluded_tools` list, which removes the listed tools from every non-CLI channel (Discord, Telegram, Bluesky, Matrix, Slack, etc.) while leaving the local CLI untouched:

```toml
[autonomy]
non_cli_excluded_tools = ["shell", "file_write", "browser"]
```

The granularity is binary (CLI vs non-CLI), not per-channel. If you need finer-grained gating, drop the global `[autonomy].level` to `read_only` or `supervised` and rely on the per-tool `auto_approve` / `always_ask` lists to gate sensitive tools behind operator approval.

## See also

- [MCP](./mcp.md)
- [ACP](../channels/acp.md)
- [Browser automation](./browser.md)
- [Security → Overview](../security/overview.md)
