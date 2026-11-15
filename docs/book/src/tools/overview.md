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
| `web_search_tool` | Web search. Provider is configurable: DuckDuckGo (default, no key), Brave, Tavily, SearXNG, Jina, or Bocha |
| `web_fetch` | Fetch a page and return clean plain text |
| `browser` | Headless-browser automation. See [Browser automation](./browser.md) |
| `memory_recall` | Search long-term memory for relevant facts, preferences, or context |
| `memory_store` | Store a fact, preference, or note in long-term memory |
| `ask_user` | Send a question to the active channel and wait for a reply. Supports optional `choices` for structured responses (inline keyboard on Telegram, numbered list on CLI). On ACP, `choices` are required: free-form ask awaits the ACP elicitation RFD. Parameters: `question` (required), `choices` (optional list), `timeout_secs` (default 600). |
| `escalate_to_human` | Send a structured escalation message with urgency routing. `high` / `critical` urgency additionally notifies any channels listed in `[escalation] alert_channels`. Parameters: `summary` (required), `context` (optional), `urgency` (`low`/`medium`/`high`/`critical`, default `medium`), `wait_for_response` (bool, default false), `timeout_secs` (default 600). On ACP, `wait_for_response: true` fails immediately if the channel cannot receive free-form replies (awaits ACP elicitation RFD). |

Registered alongside the built-ins under `full`/`legacy` composition (the minimal composition assembles its own curated set; see [Install-wide composition](#install-wide-composition)):

| Tool | Notes |
|---|---|
| `cron_*` | Manage scheduled jobs: `cron_add`, `cron_list`, `cron_remove`, `cron_update`, `cron_run`, `cron_runs` |
| `schedule` | Shell-only one-shot/recurring scheduling |
| `memory_forget`, `memory_export`, `memory_purge` | Long-term memory management |
| `reasoning_subagent` | V1 SubAgent entry point: one bounded, contract-admitted ReasoningSubAgent run returning a structured report. Also the SubAgent entry point of the minimal composition. |

Conditionally registered:

| Tool | Enabled by |
|---|---|
| `sop_*` tools | Registered when `sop.sops_dir` is configured: run and inspect SOPs |
| `discord_search` | Registered when a Discord alias has `archive` enabled |

## Install-wide composition

The root `composition` key selects which tool surface an install assembles:

- `composition = "minimal"` assembles only the curated companion primitives: workspace basics (`shell`, `file_read`, `file_write`, `file_edit`, `glob_search`, `content_search`), personal memory (`memory_recall`, `memory_store`), skill discovery (`read_skill`), extension discovery (`tool_search`), universal web primitives (`web_search_tool`, `web_fetch`), bounded interaction (`ask_user`), the V1 SubAgent entry point (`reasoning_subagent`), and scheduling (`schedule`). Individual tool `enabled` flags do not widen it back. The legacy `spawn_subagent` entry point is retired on every composition (see [Delegation & SubAgents](../agents/delegation.md)).
- `composition = "full"` (alias `"legacy"`) keeps the complete built-in registry. This is a **transitional opt-in compatibility profile**, not a target: new specialist capabilities should stay out of the ordinary provider-wire surface until they are measured in.
- An absent key behaves as `full`: configs written before the field existed keep today's tool surface across upgrades, and existing installs are not migrated.

Fresh installs start on `minimal`: the install bootstrap (`Config::load_or_init` on a first run, and the shipped config template used by the dev container) pins `composition = "minimal"` into the brand-new config file. Set `composition = "full"` explicitly to opt an install back into the complete registry.

The membership table is an allowlist, so a newly registered tool stays out of the minimal surface until it is deliberately added. CI guards the minimal profile from both directions: a wire-budget ceiling on the provider-wire `tools[]` array, and a banned-category tripwire (direct coding-harness launchers, repo/git mutation, operator/admin mutation, concrete SaaS adapters, hardware tools) on the membership table.

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

Register via the runtime's tool factory. Capabilities that should live outside the core binary go through an MCP server instead (see [MCP](./mcp.md)).

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

## Named HTTP authorization secrets

Bind a named `auth_secret` to its destination hosts with a policy alongside the
existing secret entry:

```toml
[http_request.secrets]
example_api = "${EXAMPLE_API_TOKEN}"

[http_request.auth_secret_policies.example_api]
allowed_hosts = ["api.example.com"]
```

The tool compares the validated request host before injecting Authorization.
Host matching is exact and case-insensitive: use canonical DNS names (ASCII
punycode for international names) or IP addresses without brackets, ports,
schemes, paths or wildcards. Subdomains need their own entries. The binding is
host-only; it does not restrict the scheme or port. Existing URL/DNS/private-host
checks still apply, and redirects remain disabled.

An explicit empty `allowed_hosts = []` rejects all destinations for that secret.
A request without `auth_secret` does not load or inject a named secret. Policies
and values are reloaded together, so a later policy change applies to the next
request. Secret encryption and environment references keep their existing format.

**Transition behavior:** secrets without an `allowed_hosts` declaration keep the
legacy unbound behavior. Existing installations must add policies for each named
secret to gain destination binding; upgrading alone does not bind old secrets.
