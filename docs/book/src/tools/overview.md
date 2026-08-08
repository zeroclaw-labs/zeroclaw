# Tools: Overview

**Tools** are the agent's hands. A tool is a capability the model can invoke mid-conversation, run a shell command, fetch an HTTP URL, open a browser, write a file, read a sensor. Every tool call is subject to [security policy](../security/overview.md). Successful executions can include a [tool receipt](../security/tool-receipts.md) when receipts are enabled.

Tools are not to be confused with `zeroclaw` CLI subcommands. CLI commands are for operators; tools are for the agent.

An agent gets its tools through the skill, knowledge, and MCP bundles it references; see [Agents](../agents/overview.md) for how bundles attach to an agent.
For the turn-level path from provider tool call to approval, dispatch, receipt,
observer event, and history entry, see
[Tool execution lifecycle](../architecture/tool-execution-lifecycle.md).

Before adding a built-in tool or replacing one with an external integration,
use the [Built-In Tool Inventory](../developing/tool-inventory.md)
to choose the smallest durable home.

## Built-in tools

A minimal build ships with:

| Tool | What it does |
|---|---|
| `shell` | Execute a shell command in the workspace directory. Subject to command allow/deny lists |
| `file_read` | Read a file with line numbers; supports partial reads and base64 encoding for binary files (path must be inside the workspace unless autonomy permits otherwise) |
| `file_write` | Write a file (same path constraint) |
| `file_edit` | Replace an exact string match in a file with new content |
| `glob_search` | List files matching a glob pattern within the workspace |
| `content_search` | Search file contents by regex within the workspace (ripgrep with grep fallback) |
| `http_request` | HTTP GET/POST/PUT/DELETE/PATCH/HEAD/OPTIONS to allowlisted domains |
| `web_research` | Research a question on the live web and return a written briefing ending in a `Sources:` list. A bounded sub-agent decomposes the question, searches, reads the pages worth reading, and distills the result, so raw search output never enters the main conversation. Registered whenever `[web_search] enabled = true`. Parameters: `question` (required), `url` (optional starting page) |
| `web_search_tool` | Raw web search. **Not registered by default**; it is scoped inside `web_research` (see below). Provider is configurable: DuckDuckGo (default, no key), Brave, Tavily, SearXNG, Jina, or Bocha |
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
| `knowledge` | `[knowledge].enabled = true`. Stores structured relationship memory; see [Relationship memory](./relationship-memory.md) |
| Hardware probes | `--features hardware`: GPIO, I2C, SPI reads/writes |
| `sop_*` tools | Registered when `sop.sops_dir` is configured: run and inspect SOPs |
| `discord_search` | Registered when a Discord alias has `archive` enabled |

## Web research and the demoted `web_search_tool`

Web search reaches the agent through the **`web_research`** delegate rather than
as a raw search tool. The main agent asks a question; a bounded sub-agent runs
search → fetch → distill against whatever backend `[web_search]` configures, and
returns a summary with a mandatory `Sources:` list.

The point is context hygiene: raw search-engine result text (titles, blurbs,
SEO noise, and every URL on the results page) no longer lands in the primary
context window. Only the distilled briefing does.

Your `[web_search]` configuration is unchanged. It still selects the provider
and holds the keys; it configures the *backend*, not the surface. Setting
`[web_search] enabled = true` now registers `web_research`.

The sub-agent's scope is deliberately narrow: search and `web_fetch` only, no
shell and no write tools. Every run is capped on two axes: at most 8 tool
calls and a hard wall-clock ceiling that bounds nested tool calls as well as
model calls. Hitting either returns a best-effort partial briefing, marked
`[partial: outcome=...]`, with whatever sources were gathered, rather than an
error.

Three further properties are worth knowing:

- **The denylist reaches inside.** `excluded_tools` applies to the sub-agent's
  scope exactly as it applies to a registered tool, so excluding `web_fetch` or
  `web_search_tool` degrades a research run to fetch-only, search-only, or a
  refusal. Approving a `web_research` call covers the read-only searches and
  fetches inside it; they do not prompt separately.
- **Delegation is metered.** The sub-agent's model calls are checked against
  the shared spend budget before each request and recorded against it after,
  so a research run cannot spend past a limit that would have stopped the main
  agent loop.
- **`Sources:` means retrieved.** The list is rebuilt from pages the run
  actually fetched successfully. A URL the model cites that was not retrieved
  is listed separately under `Model-cited (unverified):` rather than being
  silently kept or dropped.

Both scoped tools are read-only, so `web_research` is available at the
`readonly` autonomy level, which is what keeps [web search permitted in
`readonly`](../security/autonomy.md) now that the raw tool is scoped behind the
delegate.

### Getting the raw tool back

`allowed_tools` can only narrow the registry, so the raw tool is registered
exactly when you name it explicitly:

```toml
[risk_profiles.default]
allowed_tools = ["web_search_tool", "web_research", "file_read"]
auto_approve = ["web_search_tool", "web_research", "file_read"]
```

Naming `web_search_tool` in `allowed_tools` puts it back in the main registry
alongside `web_research`. Note that an `allowed_tools` list is an allowlist for
*everything*; listing only these three tools restricts the agent to them.

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

When receipts are enabled, successful executions receive a [tool receipt](../security/tool-receipts.md). Denied, blocked, replaced, failed, or interrupted calls do not receive receipts.

## Disabling tools on non-CLI channels

The schema has no per-channel `tools_allow` / `tools_deny` field. Tool gating lives on the agent's risk profile (`[risk_profiles.<alias>]`):

- `excluded_tools` removes the listed tools from every non-CLI channel (Discord, Telegram, Bluesky, Matrix, Slack, etc.) while leaving the local CLI untouched. The granularity is binary (CLI vs non-CLI), not per-channel. It also subtracts from the agentic-delegate allow-list resolved at runtime, which is the only way to block individual `<server>__<tool>` MCP names that would otherwise be auto-admitted by the rule below.
- `allowed_tools` is the inverse: an allowlist of tools the agent may call in agentic mode (empty or omitted means no authorization constraint; the TOML config does not distinguish the two).
- **MCP exception**: when `allowed_tools` is non-empty, runtime-discovered MCP tools (any name containing `__`, the `<server>__<tool>` convention) are auto-admitted into the effective allow-list without having to be listed there individually. This keeps the post-#7464 eager-MCP default usable for agents that already pin an explicit allow-list. To block individual MCP tools, list them in `excluded_tools`.
- The MCP exception is scoped to the **risk profile**'s `allowed_tools` only. Caller-supplied per-run allow-lists (cron job `allowed_tools`, narrowed delegate invocations, etc.) are still treated as strict explicit-list intersections. A job that narrows itself to `allowed_tools = ["cron_add"]` will not surface runtime-discovered MCP wrappers it did not name, even when the agent's risk profile would auto-admit them.

If you need finer-grained gating, drop the profile's `level` to `read_only` or `supervised` and rely on the per-profile `auto_approve` / `always_ask` lists to gate sensitive tools behind operator approval.

See [Autonomy levels](../security/autonomy.md) for the full set of per-profile fields.

## See also

- [MCP](./mcp.md)
- [Tool execution lifecycle](../architecture/tool-execution-lifecycle.md)
- [ACP](../channels/acp.md)
- [Browser automation](./browser.md)
- [Security → Overview](../security/overview.md)
