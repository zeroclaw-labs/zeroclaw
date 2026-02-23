# Codex-CLI Provider + Full MCP Tool Bridge Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add a new `codex-cli` provider and expose the full ZeroClaw native tool registry over an MCP stdio server so Codex CLI (including `gpt-5.3-codex-spark`) can use ZeroClaw tools.

**Architecture:** Introduce a first-party MCP server module in ZeroClaw that wraps the existing in-process `Tool` registry and serves MCP methods (`initialize`, `tools/list`, `tools/call`) over stdio JSON-RPC. Add a new `codex-cli` provider that shells out to `codex exec --json`, parses event output, and returns final assistant text through the existing `Provider` trait. Keep `openai-codex` unchanged; `codex-cli` is additive.

**Tech Stack:** Rust, tokio (`process`, `io`), serde/serde_json, clap, existing `Tool` trait + registries, existing provider factory/reliability wrappers.

---

## Overview

1. Add MCP server core (protocol framing + request dispatcher + tool adapter).
2. Add top-level CLI command `zeroclaw mcp-server` that loads full tool registry (including peripherals) and serves MCP over stdio.
3. Add `codex-cli` provider module and register it in provider factory/aliases.
4. Integrate onboarding/provider docs/tests for `codex-cli`.
5. Add end-to-end verification with fake-codex tests and MCP request/response tests.

---

## Task 0: Baseline and branch

**Files:**
- Working dir: `/home/admin/whiskey`

**Step 1: Baseline provider + tool tests**

Run:

```bash
cargo test providers::mod::factory_all_providers_create_successfully providers::mod::listed_providers_and_aliases_are_constructible tools::mod::default_tools_names -q
```

Expected: PASS.

**Step 2: Create branch**

```bash
git checkout -b feature/codex-cli-mcp-bridge
```

**Step 3: Snapshot clean starting point**

```bash
git status --short
```

Expected: no unexpected changes beyond known local state.

---

## Task 1: Create MCP protocol framing primitives

**Files:**
- Create: `src/mcp/mod.rs`
- Create: `src/mcp/protocol.rs`
- Modify: `src/lib.rs` (export module)
- Test: `src/mcp/protocol.rs`

**Step 1: Write failing tests for stdio framing**

Add tests first in `src/mcp/protocol.rs`:

```rust
#[test]
fn decodes_content_length_frame() {
    let raw = b"Content-Length: 27\r\n\r\n{\"jsonrpc\":\"2.0\",\"id\":1}";
    let msg = decode_frame(raw).expect("frame should decode");
    assert_eq!(msg["jsonrpc"], "2.0");
}

#[test]
fn encodes_content_length_frame() {
    let v = serde_json::json!({"jsonrpc":"2.0","id":1,"result":{}});
    let bytes = encode_frame(&v).expect("frame should encode");
    let s = String::from_utf8(bytes).expect("valid utf8");
    assert!(s.starts_with("Content-Length:"));
    assert!(s.contains("\r\n\r\n{\"jsonrpc\":\"2.0\""));
}
```

**Step 2: Run test to verify RED**

```bash
cargo test mcp::protocol::tests::decodes_content_length_frame -q
```

Expected: FAIL (missing module/functions).

**Step 3: Implement minimal protocol helpers**

Implement in `src/mcp/protocol.rs`:

```rust
pub fn decode_frame(raw: &[u8]) -> anyhow::Result<serde_json::Value> { /* parse headers + body */ }
pub fn encode_frame(value: &serde_json::Value) -> anyhow::Result<Vec<u8>> { /* write Content-Length */ }
```

Also add `src/mcp/mod.rs`:

```rust
pub mod protocol;
pub mod server;
pub mod tool_bridge;
```

And in `src/lib.rs`:

```rust
pub mod mcp;
```

**Step 4: Run tests to verify GREEN**

```bash
cargo test mcp::protocol -q
```

Expected: PASS.

**Step 5: Commit**

```bash
git add src/mcp/mod.rs src/mcp/protocol.rs src/lib.rs
git commit -m "feat(mcp): add stdio content-length protocol framing helpers"
```

---

## Task 2: Build tool-registry-to-MCP adapter

**Files:**
- Create: `src/mcp/tool_bridge.rs`
- Test: `src/mcp/tool_bridge.rs`

**Step 1: Write failing adapter tests**

```rust
#[tokio::test]
async fn list_tools_exports_name_description_and_schema() {
    let tools = vec![Box::new(DummyTool) as Box<dyn crate::tools::Tool>];
    let bridge = McpToolBridge::new(tools);
    let listed = bridge.list_tools();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "dummy_tool");
}

#[tokio::test]
async fn call_tool_routes_by_name() {
    let tools = vec![Box::new(DummyTool) as Box<dyn crate::tools::Tool>];
    let bridge = McpToolBridge::new(tools);
    let out = bridge
        .call_tool("dummy_tool", serde_json::json!({"value":"ok"}))
        .await
        .expect("call should succeed");
    assert!(out.success);
    assert_eq!(out.output, "ok");
}
```

**Step 2: Run test to verify RED**

```bash
cargo test mcp::tool_bridge::tests::list_tools_exports_name_description_and_schema -q
```

Expected: FAIL (missing types).

**Step 3: Implement bridge**

In `src/mcp/tool_bridge.rs` implement:

```rust
pub struct McpToolBridge { /* name -> Arc<dyn Tool> */ }

impl McpToolBridge {
    pub fn new(tools: Vec<Box<dyn Tool>>) -> Self { /* index tools */ }
    pub fn list_tools(&self) -> Vec<McpToolDescriptor> { /* ToolSpec -> MCP descriptor */ }
    pub async fn call_tool(&self, name: &str, args: serde_json::Value) -> anyhow::Result<ToolResult> { /* dispatch */ }
}
```

Include normalized MCP list/call DTOs:

```rust
pub struct McpToolDescriptor { pub name: String, pub description: String, pub input_schema: serde_json::Value }
```

**Step 4: Run tests to verify GREEN**

```bash
cargo test mcp::tool_bridge -q
```

Expected: PASS.

**Step 5: Commit**

```bash
git add src/mcp/tool_bridge.rs
git commit -m "feat(mcp): add adapter from zeroclaw tool registry to MCP tool descriptors"
```

---

## Task 3: Implement MCP JSON-RPC server dispatcher

**Files:**
- Create: `src/mcp/server.rs`
- Test: `src/mcp/server.rs`

**Step 1: Write failing dispatcher tests**

```rust
#[tokio::test]
async fn initialize_returns_server_capabilities() {
    let resp = handle_request(&req("initialize", serde_json::json!({})), &bridge()).await;
    assert_eq!(resp["result"]["capabilities"]["tools"].is_object(), true);
}

#[tokio::test]
async fn tools_list_returns_registered_tools() {
    let resp = handle_request(&req("tools/list", serde_json::json!({})), &bridge()).await;
    assert_eq!(resp["result"]["tools"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn tools_call_executes_and_returns_content() {
    let resp = handle_request(
        &req("tools/call", serde_json::json!({"name":"dummy_tool","arguments":{"value":"ok"}})),
        &bridge(),
    ).await;
    assert!(resp["result"]["content"][0]["text"].as_str().unwrap().contains("ok"));
}
```

**Step 2: Run tests to verify RED**

```bash
cargo test mcp::server::tests::initialize_returns_server_capabilities -q
```

Expected: FAIL.

**Step 3: Implement dispatcher + stdio loop**

In `src/mcp/server.rs` implement:

```rust
pub async fn run_stdio_server(bridge: Arc<McpToolBridge>) -> anyhow::Result<()> { /* read frame, dispatch, write frame */ }
pub async fn handle_request(req: &serde_json::Value, bridge: &McpToolBridge) -> serde_json::Value { /* method match */ }
```

Support methods:
- `initialize`
- `tools/list`
- `tools/call`
- `resources/list` -> empty
- `resources/templates/list` -> empty
- `ping`
- unknown method -> JSON-RPC error `-32601`

**Step 4: Run tests to verify GREEN**

```bash
cargo test mcp::server -q
```

Expected: PASS.

**Step 5: Commit**

```bash
git add src/mcp/server.rs
git commit -m "feat(mcp): implement MCP request dispatcher and stdio server loop"
```

---

## Task 4: Wire `zeroclaw mcp-server` top-level command

**Files:**
- Modify: `src/main.rs`
- Test: `src/main.rs` (CLI parsing tests)

**Step 1: Write failing CLI parse test**

Add test in `src/main.rs` tests module:

```rust
#[test]
fn parses_mcp_server_command() {
    let cli = Cli::parse_from(["zeroclaw", "mcp-server"]);
    match cli.command {
        Commands::McpServer => {}
        _ => panic!("expected mcp-server command"),
    }
}
```

**Step 2: Run test to verify RED**

```bash
cargo test parses_mcp_server_command -q
```

Expected: FAIL (enum variant missing).

**Step 3: Implement command wiring**

In `src/main.rs`:

```rust
enum Commands {
    // ...
    /// Start ZeroClaw as an MCP stdio server exposing native tool registry
    McpServer,
}
```

In command match:

```rust
Commands::McpServer => {
    let security = Arc::new(SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir));
    let mem: Arc<dyn zeroclaw::memory::Memory> = Arc::from(zeroclaw::memory::create_memory_with_storage(
        &config.memory,
        Some(&config.storage.provider.config),
        &config.workspace_dir,
        Some((config.default_provider.as_deref(), config.api_key.as_deref())),
    )?);

    let mut tools = zeroclaw::tools::all_tools(
        Arc::new(config.clone()),
        &security,
        mem,
        config.composio.api_key.as_deref(),
        config.composio.entity_id.as_deref(),
        &config.browser,
        &config.http_request,
        &config.workspace_dir,
        &config.agents,
        config.api_key.as_deref(),
        &config,
    );
    tools.extend(zeroclaw::peripherals::create_peripheral_tools(&config.peripherals).await?);

    let bridge = std::sync::Arc::new(zeroclaw::mcp::tool_bridge::McpToolBridge::new(tools));
    zeroclaw::mcp::server::run_stdio_server(bridge).await?;
    return Ok(());
}
```

**Step 4: Run parse + compile checks**

```bash
cargo test parses_mcp_server_command -q
cargo check -q
```

Expected: PASS.

**Step 5: Commit**

```bash
git add src/main.rs
git commit -m "feat(cli): add zeroclaw mcp-server command exposing full tool registry"
```

---

## Task 5: Add `codex-cli` provider implementation

**Files:**
- Create: `src/providers/codex_cli.rs`
- Modify: `src/providers/mod.rs` (module export only in this task)
- Test: `src/providers/codex_cli.rs`

**Step 1: Write failing parser tests**

In `src/providers/codex_cli.rs` tests module:

```rust
#[test]
fn parses_last_agent_message_from_jsonl_events() {
    let lines = [
        r#"{"type":"item.completed","item":{"type":"agent_message","text":"first"}}"#,
        r#"{"type":"item.completed","item":{"type":"agent_message","text":"final"}}"#,
    ];
    let out = extract_last_agent_message(&lines.join("\n")).unwrap();
    assert_eq!(out, "final");
}

#[test]
fn builds_codex_exec_args_with_model_and_json() {
    let args = build_codex_exec_args("gpt-5.3-codex-spark");
    assert!(args.contains(&"exec".to_string()));
    assert!(args.contains(&"--json".to_string()));
    assert!(args.contains(&"--model".to_string()));
}
```

**Step 2: Run test to verify RED**

```bash
cargo test codex_cli::tests::parses_last_agent_message_from_jsonl_events -q
```

Expected: FAIL.

**Step 3: Implement provider**

Implement in `src/providers/codex_cli.rs`:

```rust
pub struct CodexCliProvider {
    codex_bin: String,
    sandbox_mode: String,
}

impl CodexCliProvider {
    pub fn new() -> Self { /* env-driven defaults */ }
    async fn run_exec(&self, messages: &[ChatMessage], model: &str) -> anyhow::Result<ProviderChatResponse> { /* spawn codex exec */ }
}

#[async_trait]
impl Provider for CodexCliProvider {
    async fn chat_with_system(...) -> anyhow::Result<String> { /* compose + run */ }
    async fn chat_with_history(...) -> anyhow::Result<String> { /* run */ }
    async fn chat(...) -> anyhow::Result<ProviderChatResponse> { /* run */ }
    fn supports_native_tools(&self) -> bool { true }
}
```

Command shape:

```text
codex exec --json --skip-git-repo-check --sandbox <mode> --model <model> -
```

**Step 4: Run tests**

```bash
cargo test codex_cli::tests -q
```

Expected: PASS.

**Step 5: Commit**

```bash
git add src/providers/codex_cli.rs src/providers/mod.rs
git commit -m "feat(provider): add codex-cli provider backed by codex exec json stream"
```

---

## Task 6: Register `codex-cli` in provider factory and catalog

**Files:**
- Modify: `src/providers/mod.rs`
- Modify: `src/main.rs` (agent help text list)
- Test: `src/providers/mod.rs`

**Step 1: Write failing provider factory test**

Add in `src/providers/mod.rs` tests:

```rust
#[test]
fn factory_codex_cli_provider() {
    assert!(create_provider("codex-cli", None).is_ok());
    assert!(create_provider("codex_cli", None).is_ok());
}
```

**Step 2: Run test to verify RED**

```bash
cargo test providers::mod::tests::factory_codex_cli_provider -q
```

Expected: FAIL.

**Step 3: Implement registry changes**

In `src/providers/mod.rs`:

```rust
pub mod codex_cli;

match name {
    "codex-cli" | "codex_cli" => Ok(Box::new(codex_cli::CodexCliProvider::new())),
    // existing cases...
}
```

In `list_providers()` add:

```rust
ProviderInfo {
    name: "codex-cli",
    display_name: "Codex CLI (local)",
    aliases: &["codex_cli"],
    local: true,
}
```

Update `src/main.rs` provider help text for `agent --provider` to include `codex-cli`.

**Step 4: Run tests**

```bash
cargo test providers::mod::tests::factory_codex_cli_provider providers::mod::tests::listed_providers_and_aliases_are_constructible -q
```

Expected: PASS.

**Step 5: Commit**

```bash
git add src/providers/mod.rs src/main.rs
git commit -m "feat(provider): register codex-cli in provider factory, aliases, and CLI help"
```

---

## Task 7: Onboarding integration for `codex-cli`

**Files:**
- Modify: `src/onboard/wizard.rs`
- Test: `src/onboard/wizard.rs`

**Step 1: Write failing onboarding tests**

Add tests:

```rust
#[test]
fn default_model_for_codex_cli_is_spark() {
    assert_eq!(default_model_for_provider("codex-cli"), "gpt-5.3-codex-spark");
}

#[test]
fn provider_supports_keyless_local_usage_includes_codex_cli() {
    assert!(provider_supports_keyless_local_usage("codex-cli"));
}
```

**Step 2: Run test to verify RED**

```bash
cargo test default_model_for_codex_cli_is_spark provider_supports_keyless_local_usage_includes_codex_cli -q
```

Expected: FAIL.

**Step 3: Implement onboarding mappings**

Update `src/onboard/wizard.rs`:

- `default_model_for_provider("codex-cli") => "gpt-5.3-codex-spark"`
- curated model list entry for `codex-cli` including:
  - `gpt-5.3-codex-spark`
  - `gpt-5.3-codex`
  - `gpt-5-codex`
- `provider_supports_keyless_local_usage` includes `codex-cli`
- provider selection list includes `codex-cli` label (local Codex CLI runner)
- do **not** enable live model fetch for `codex-cli` (no `/models` endpoint)

**Step 4: Run tests**

```bash
cargo test onboard::wizard::tests::default_model_for_provider_uses_latest_defaults onboard::wizard::tests::provider_supports_keyless_local_usage_for_local_providers -q
```

Expected: PASS.

**Step 5: Commit**

```bash
git add src/onboard/wizard.rs
git commit -m "feat(onboard): add codex-cli provider defaults and keyless local selection"
```

---

## Task 8: Document MCP bridge + codex-cli usage

**Files:**
- Modify: `README.md`
- Modify: `docs/commands-reference.md`
- Modify: `docs/providers-reference.md`

**Step 1: Write doc assertions as failing checks (manual)**

Manual checklist to validate after edits:
- `README.md` includes `codex-cli` provider config + MCP bridge quickstart.
- `docs/commands-reference.md` top-level includes `mcp-server`.
- `docs/providers-reference.md` includes `codex-cli` row and setup notes.

**Step 2: Implement docs updates**

Add to `README.md`:

```toml
default_provider = "codex-cli"
default_model = "gpt-5.3-codex-spark"
```

Add MCP wiring section:

```bash
codex mcp add zeroclaw -- zeroclaw mcp-server
codex mcp list
```

Add command reference:
- `zeroclaw mcp-server` description and usage notes.

Add provider reference row:
- `codex-cli` / alias `codex_cli` / local / no API key required.

**Step 3: Run docs quality checks**

```bash
bash scripts/ci/docs_quality_gate.sh
bash scripts/ci/docs_links_gate.sh
```

Expected: PASS.

**Step 4: Commit**

```bash
git add README.md docs/commands-reference.md docs/providers-reference.md
git commit -m "docs: add codex-cli provider and zeroclaw MCP tool bridge setup guide"
```

---

## Task 9: Add integration tests for provider process behavior

**Files:**
- Create: `tests/codex_cli_provider.rs`
- Modify: `src/providers/codex_cli.rs` (injectable binary path for tests)

**Step 1: Write failing integration tests with fake codex binary**

Create test script fixture from test body (tempdir) that emits JSON lines.

```rust
#[tokio::test]
async fn codex_cli_provider_returns_last_agent_message() {
    let provider = CodexCliProvider::new_for_test(fake_codex_path());
    let text = provider
        .chat_with_system(None, "hi", "gpt-5.3-codex-spark", 0.0)
        .await
        .unwrap();
    assert_eq!(text, "final");
}

#[tokio::test]
async fn codex_cli_provider_surfaces_stderr_on_nonzero_exit() {
    let provider = CodexCliProvider::new_for_test(fake_failing_codex_path());
    let err = provider
        .chat_with_system(None, "hi", "gpt-5.3-codex-spark", 0.0)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not supported"));
}
```

**Step 2: Run test to verify RED**

```bash
cargo test --test codex_cli_provider -q
```

Expected: FAIL before constructor/test hooks are added.

**Step 3: Implement minimal test hooks + process handling hardening**

In `src/providers/codex_cli.rs`, add test-only constructor:

```rust
#[cfg(test)]
pub fn new_for_test(path: impl Into<String>) -> Self { /* inject fake binary */ }
```

Ensure nonzero exit includes stderr and captured stdout tail for diagnostics.

**Step 4: Run tests to verify GREEN**

```bash
cargo test --test codex_cli_provider -q
```

Expected: PASS.

**Step 5: Commit**

```bash
git add tests/codex_cli_provider.rs src/providers/codex_cli.rs
git commit -m "test(provider): add codex-cli process and error-path integration tests"
```

---

## Task 10: End-to-end verification matrix

**Files:**
- Modify: `RUN_TESTS.md` (add manual E2E section)

**Step 1: Add E2E commands to runbook**

Add commands:

```bash
# 1) Start zeroclaw MCP server in one terminal
zeroclaw mcp-server

# 2) Register server in codex CLI
codex mcp add zeroclaw -- zeroclaw mcp-server

# 3) Verify codex sees MCP server
codex mcp list

# 4) Verify codex-cli provider path
zeroclaw agent --provider codex-cli --model gpt-5.3-codex-spark -m "say OK"
```

**Step 2: Full verification run**

```bash
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

Expected: PASS (or explicitly documented pre-existing failures only).

**Step 3: Final commit**

```bash
git add RUN_TESTS.md
git commit -m "chore(test): document codex-cli + mcp bridge end-to-end verification matrix"
```

---

## Risks and Mitigations

- **Risk:** MCP method mismatch with Codex CLI expectations.
  - **Mitigation:** Keep server method set minimal + spec-conformant (`initialize`, `tools/list`, `tools/call`, `ping`) and add strict JSON-RPC tests.

- **Risk:** `codex exec` output schema drift.
  - **Mitigation:** Parse defensively; rely only on stable `item.completed` + `agent_message` text and nonzero exit handling.

- **Risk:** Tool side effects bypass ZeroClaw approval workflow when invoked via external MCP client.
  - **Mitigation:** Document explicitly; rely on existing `SecurityPolicy` checks inside each tool implementation; add follow-up work item for optional approval hook in MCP server.

- **Risk:** Recursive delegation loops if delegate agents also use `codex-cli` and same MCP bridge.
  - **Mitigation:** Add guardrails in docs and recommend explicit delegate provider/model settings.

---

## Acceptance Criteria

- `zeroclaw mcp-server` starts and returns valid MCP `tools/list` with full current tool registry.
- `codex mcp add zeroclaw -- zeroclaw mcp-server` works and tools are callable from Codex CLI.
- `zeroclaw agent --provider codex-cli --model gpt-5.3-codex-spark` runs successfully.
- Existing providers (`openai-codex`, etc.) remain unchanged and passing tests.
- Docs include setup and limitations clearly.

---

## Post-Plan Execution Order

Implement tasks strictly in order (`Task 0` -> `Task 10`), preserving RED/GREEN discipline and committing after each task.
