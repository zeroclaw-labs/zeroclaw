# AGENTS.md — tools/

> Agent tool execution surface: the set of capabilities exposed to the LLM during agentic loops.

## Overview

This subsystem defines every action the agent can take beyond text generation. Each tool implements the `Tool` trait, providing a name, description, JSON parameter schema, and async `execute` method. Tools are assembled into registries (`default_tools`, `all_tools_with_runtime`) and dispatched by the agent orchestration loop via name-matching against the LLM's function-call output.

## Key Files

- `traits.rs` — `Tool` trait, `ToolResult`, `ToolSpec` structs. The contract everything implements.
- `mod.rs` — Module declarations, `pub use` re-exports, factory functions (`default_tools`, `all_tools_with_runtime`), `ArcToolRef`/`ArcDelegatingTool` wrappers, `register_skill_tools`.
- `shell.rs` — `ShellTool`: sandboxed command execution. Reference implementation for security+runtime injection.
- `file_read.rs`, `file_write.rs`, `file_edit.rs` — Filesystem tools with path policy enforcement.
- `memory_store.rs`, `memory_recall.rs`, `memory_forget.rs` — Long-term memory tools using `Memory` trait.
- `browser.rs` — Browser automation with pluggable backends and computer-use sidecar.
- `mcp_tool.rs` — `McpToolWrapper`: wraps MCP server tools as native `Tool` impls. Strips `approved` field before forwarding.
- `mcp_client.rs` — `McpRegistry`: discovery and routing for MCP server connections.
- `delegate.rs` — Sub-agent delegation tool.
- `verifiable_intent.rs` — Verifiable Intent credential verification tool.
- `schema.rs` — `SchemaCleanr` for parameter schema normalization.

## Trait Contract

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;                              // Unique, used in LLM function calling
    fn description(&self) -> &str;                       // Shown to LLM for tool selection
    fn parameters_schema(&self) -> serde_json::Value;    // JSON Schema for args validation
    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult>;
    fn spec(&self) -> ToolSpec { /* default impl */ }    // Bundles name+desc+schema
}
```

`ToolResult` has three fields: `success: bool`, `output: String`, `error: Option<String>`. Always set `error` on failure rather than returning `Err` — `Err` means infrastructure failure, not tool-level failure.

## Extension Playbook

1. Create `src/tools/my_tool.rs` with a struct holding `Arc<SecurityPolicy>` (plus any other deps).
2. Implement `Tool` trait. Name must be unique and snake_case (the agent loop matches on it).
3. `parameters_schema()` must return a valid JSON Schema object with `"type": "object"`, `"properties"`, and `"required"`.
4. In `execute()`: check `security.is_rate_limited()` first, then `security.is_path_allowed()` or `security.enforce_tool_operation()` as appropriate, then `security.record_action()` before the real work.
5. Add `pub mod my_tool;` and `pub use my_tool::MyTool;` in `mod.rs`.
6. Register in `all_tools_with_runtime()`: push `Arc::new(MyTool::new(security.clone()))` into `tool_arcs`.
7. Add tests in the same file under `#[cfg(test)] mod tests`.
8. Run `cargo clippy --all-targets -- -D warnings && cargo test`.

## Factory Registration

Two tiers exist:

- **`default_tools`** — Minimal set (shell, file_read, file_write, file_edit, glob_search, content_search). Used for lightweight/sandboxed contexts.
- **`all_tools_with_runtime`** — Full registry. Conditionally includes browser, composio, delegate, MCP, hardware, and integration tools based on config flags and feature gates.

Tools requiring shared state return handles: `DelegateParentToolsHandle` (for dynamic tool injection into sub-agents) and `ChannelMapHandle` (for reaction routing). The factory returns `(Vec<Box<dyn Tool>>, Option<DelegateParentToolsHandle>, Option<ChannelMapHandle>)`.

Skill-defined tools are registered separately via `register_skill_tools()`, which skips tools that shadow built-in names.

## Security Model

Every tool receives `Arc<SecurityPolicy>` at construction. The security enforcement pattern is:

1. **Rate limiting**: call `security.is_rate_limited()` early-return if true.
2. **Path validation**: call `security.is_path_allowed(path)` for any filesystem access.
3. **Operation authorization**: call `security.enforce_tool_operation(ToolOperation::Act, tool_name)` for side-effectful tools (composio, browser, claude_code, reaction).
4. **Action recording**: call `security.record_action()` before the actual work. Record *before* canonicalization to prevent path-probing without rate-limit cost.
5. **Sandbox**: `ShellTool` accepts `Arc<dyn Sandbox>` for OS-level command isolation. Created via `create_sandbox()` from security config.
6. **Supervised mode**: The agent loop injects an `approved: bool` field into tool call args. `McpToolWrapper` strips this before forwarding to MCP servers.

Never weaken these checks. Never skip `record_action()` — it is the rate-limit accounting step.

## Testing Patterns

- Create `SecurityPolicy` with `SecurityPolicy { autonomy: AutonomyLevel::Supervised, workspace_dir: std::env::temp_dir(), ..SecurityPolicy::default() }`.
- Create runtime with `Arc::new(NativeRuntime::new())`.
- Test the three metadata methods: `name()`, `description()`, `parameters_schema()`.
- Test `execute()` with valid args (happy path), missing required args, and policy-denied args.
- Test `ToolResult` serialization roundtrip if the tool produces structured output.
- Use `#[tokio::test]` for async execute tests.
- Hardware tools are gated behind `#[cfg(feature = "hardware")]` — CI runs them only with that feature flag.

## Common Gotchas

- Tool names must be globally unique across all registries (built-in, skill, MCP). Collisions cause silent drops.
- `execute` returning `Err(...)` signals infra failure and may crash the agent loop. Return `ToolResult { success: false, error: Some(...) }` for expected failures.
- `parameters_schema()` must match what the LLM actually sends. Mismatches cause silent argument drops via `serde_json::Value` — no compile-time check.
- `McpToolWrapper` uses prefixed names (`server__tool`). Don't hardcode assumptions about name format elsewhere.
- Shell tool allowlists only `SAFE_ENV_VARS` — never pass API keys through environment.
- `ArcToolRef` and `ArcDelegatingTool` exist because the registry uses `Box<dyn Tool>` but shared ownership needs `Arc<dyn Tool>`. Don't add a third wrapper.
- Browser tool's computer-use endpoint defaults to localhost:8787. Remote endpoints require explicit `allow_remote_endpoint: true`.

## Cross-Subsystem Coupling

| Dependency | Direction | What |
|---|---|---|
| `security::SecurityPolicy` | tools imports | Every tool holds `Arc<SecurityPolicy>` |
| `security::policy::ToolOperation` | tools imports | Side-effect authorization enum |
| `security::traits::Sandbox` | tools imports | Shell sandboxing trait |
| `memory::Memory` | tools imports | Memory tools hold `Arc<dyn Memory>` |
| `runtime::RuntimeAdapter` | tools imports | Shell tool uses for command execution |
| `config::Config` | tools imports | Factory reads config for conditional registration |
| `skills::Skill` | tools imports | `register_skill_tools` converts skills to tools |
| `agent/` | agent imports tools | Agent loop calls `tool.execute()` and reads `tool.spec()` |
| `mcp_protocol` | internal | MCP tool definitions for `McpToolWrapper` |

Tools must not import from `agent/`, `channels/`, or `gateway/`. The dependency arrow is strictly inward.
