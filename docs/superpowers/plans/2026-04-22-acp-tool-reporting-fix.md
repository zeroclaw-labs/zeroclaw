# ACP Tool Reporting Fix Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Align ACP server tool reporting with the official spec and ensure stable tool call IDs for correlation.

**Architecture:** 
1. Enhance `TurnEvent` to carry a stable `id`.
2. Ensure `Agent::turn_streamed` generates and maintains a stable UUID per tool call.
3. Update `AcpServer` to use standard ACP fields (`title`, `kind`, `rawInput`, `rawOutput`) and the stable `toolCallId`.
4. Implement a `map_tool_kind` helper to categorize tools for better UI display.

**Tech Stack:** Rust, tokio, serde_json, uuid

---

### Task 1: Verify and Finalize API Changes

**Files:**
- Modify: `crates/zeroclaw-api/src/agent.rs` (Verify)
- Modify: `crates/zeroclaw-gateway/src/ws.rs` (Verify)

- [ ] **Step 1: Verify TurnEvent definition**
Ensure `TurnEvent` has `id: String` in `ToolCall` and `ToolResult`.

- [ ] **Step 2: Verify WebSocket gateway**
Ensure `crates/zeroclaw-gateway/src/ws.rs` correctly serializes the `id` field.

- [ ] **Step 3: Commit verification**
```bash
git add crates/zeroclaw-api/src/agent.rs crates/zeroclaw-gateway/src/ws.rs
git commit -m "chore: verify API and WS gateway for tool id support"
```

### Task 2: Stable ID Correlation in Agent Runtime

**Files:**
- Modify: `crates/zeroclaw-runtime/src/agent/agent.rs`

- [ ] **Step 1: Update turn_streamed to pre-assign UUIDs**
Modify the tool call loop to make `calls` mutable and assign UUIDs if missing.

```rust
            let (text, mut calls) = self.tool_dispatcher.parse_response(&response);
            // ...
            for call in &mut calls {
                if call.tool_call_id.is_none() {
                    call.tool_call_id = Some(uuid::Uuid::new_v4().to_string());
                }
            }
```

- [ ] **Step 2: Use stable IDs in notifications**
Update the notification loops to use the stable `tool_call_id`.

```rust
            // Notify about each tool call
            for call in &calls {
                let call_id = call.tool_call_id.as_ref().unwrap().clone();
                let _ = event_tx
                    .send(TurnEvent::ToolCall {
                        id: call_id,
                        name: call.name.clone(),
                        args: call.arguments.clone(),
                    })
                    .await;
            }
            
            // ... (execute tools)

            // Notify about each tool result
            for result in &results {
                let result_id = result.tool_call_id.as_ref().unwrap().clone();
                let _ = event_tx
                    .send(TurnEvent::ToolResult {
                        id: result_id,
                        name: result.name.clone(),
                        output: result.output.clone(),
                    })
                    .await;
            }
```

- [ ] **Step 3: Verify PreExecuted tool events**
Ensure `PreExecutedToolCall` and `PreExecutedToolResult` also use IDs (though correlation is harder there, we should at least provide valid IDs).

- [ ] **Step 4: Commit**
```bash
git add crates/zeroclaw-runtime/src/agent/agent.rs
git commit -m "feat: ensure stable tool call IDs in turn_streamed"
```

### Task 3: ACP Server Spec Alignment

**Files:**
- Modify: `crates/zeroclaw-channels/src/orchestrator/acp_server.rs`

- [ ] **Step 1: Verify map_tool_kind helper**
Ensure the helper is present and correctly maps tool names to ACP kinds.

- [ ] **Step 2: Update ToolCall notification handler**
Use `toolCallId`, `title`, `kind`, `rawInput`, and `status: "pending"`.

- [ ] **Step 3: Update ToolResult notification handler**
Use `sessionUpdate: "tool_call"`, `toolCallId`, `status: "completed"`, and `rawOutput`.

- [ ] **Step 4: Run tests**
Run `cargo test -p zeroclaw-channels --lib orchestrator::acp_server::tests`

- [ ] **Step 5: Commit**
```bash
git add crates/zeroclaw-channels/src/orchestrator/acp_server.rs
git commit -m "feat: align ACP server with protocol spec for tool calls"
```

### Task 4: Final Validation

- [ ] **Step 1: Full build check**
Run `cargo check` to ensure no regressions.

- [ ] **Step 2: Verify tool name display logic**
Manual check (or unit test) that `map_tool_kind` works for various tool names.
