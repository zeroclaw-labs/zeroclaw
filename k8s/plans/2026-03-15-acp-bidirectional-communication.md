# ACP Bidirectional Communication — Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Enable Sam and Walter to communicate bidirectionally during ACP tasks — Sam can inject mid-task messages into Walter's context (like Dan can with Sam via Signal), and Sam can see Walter's progress without blindly blocking.

**Architecture:** Thread the existing channel injection system through the ACP server path. Add a `session/inject` JSON-RPC method to the ACP server. Enhance the acp-client Python script with `inject` and `progress` commands. Update both agents' skills to use the new communication patterns.

**Tech Stack:** Rust (zeroclaw core), Python (acp-client), Kubernetes ConfigMaps (skills)

---

## Context

From the RCA of the 2026-03-15 timeout incident: Sam's agent loop timed out (1200s) while Walter was still working (39 min). Sam was blindly blocking on `acp-client wait` with zero visibility into Walter's progress and no way to send corrections mid-task.

The Signal channel already solves this for Sam ↔ Dan via two mechanisms:
1. **Draft updates** — Sam edits her message with progress (Walter → Sam direction)
2. **Channel injection** — Dan sends messages while Sam is working, injected into her context between tool calls (Sam → Walter direction)

The ACP path has **neither**:
- SSE notifications exist but `acp-client wait` doesn't surface them to Sam's LLM
- No injection mechanism at all — `process_message_with_history` passes `None` for `injection_rx`

## Critical Files

### Rust changes
- `src/gateway/acp_server.rs` — Add `session/inject` method, thread injection channel through session store
- `src/agent/loop_.rs` — Add `injection_rx` parameter to `process_message_with_history` and `agent_turn`

### Python client changes
- `k8s/goose/03_goose_acp/04_acp_client_configmap.yaml` — Add `inject` and `progress` commands

### Skill updates (scrapyard-applications)
- `04_scrapyard_test_projects/32_zeroclaw/13_zeroclaw_skills_configmap.yaml` — Update Sam's k8s-delegation skill
- `04_scrapyard_test_projects/33_goose_todolist_sandbox/04_zeroclaw_k8s_agent/04_skills_configmap.yaml` — Update Walter's k8s-manifest-builder skill

### Reference files (patterns to follow)
- `src/channels/injection.rs` — `InjectedMessage`, `InjectionSender`, `InjectionReceiver` types
- `src/channels/mod.rs:3877-3891` — How channels register injection queues
- `src/channels/mod.rs:4790-4830` — How channels dispatch to injection queue
- `src/agent/loop_.rs:2467-2490` — How the tool loop drains injection queue between iterations

---

## Chunk 1: Thread injection_rx through the ACP agent path

### Task 1: Add injection_rx to process_message_with_history

**Files:**
- Modify: `src/agent/loop_.rs:3627-3878`

- [ ] **Step 1: Add injection_rx parameter to process_message_with_history**

Change the function signature from:
```rust
pub async fn process_message_with_history(
    config: Config,
    message: &str,
    existing_history: Vec<ChatMessage>,
) -> Result<(String, Vec<ChatMessage>)> {
```
to:
```rust
pub async fn process_message_with_history(
    config: Config,
    message: &str,
    existing_history: Vec<ChatMessage>,
    injection_rx: Option<crate::channels::injection::InjectionReceiver>,
) -> Result<(String, Vec<ChatMessage>)> {
```

- [ ] **Step 2: Add injection_rx to agent_turn**

Change `agent_turn` (line 930) signature to accept `injection_rx`:
```rust
pub(crate) async fn agent_turn(
    provider: &dyn Provider,
    history: &mut Vec<ChatMessage>,
    tools_registry: &[Box<dyn Tool>],
    observer: &dyn Observer,
    provider_name: &str,
    model: &str,
    temperature: f64,
    silent: bool,
    multimodal_config: &crate::config::MultimodalConfig,
    max_tool_iterations: usize,
    injection_rx: Option<crate::channels::injection::InjectionReceiver>,
) -> Result<String> {
```

Pass it through to `run_tool_call_loop` — replace the final `None` (line 959) with `injection_rx`.

- [ ] **Step 3: Thread injection_rx through process_message_with_history to agent_turn**

In `process_message_with_history`, pass the new parameter to the `agent_turn` call (around line 3847):
```rust
agent_turn(
    provider.as_ref(),
    &mut history,
    &tools_registry,
    observer.as_ref(),
    provider_name,
    &model_name,
    config.default_temperature,
    true,
    &config.multimodal,
    config.agent.max_tool_iterations,
    injection_rx,  // NEW — was implicitly None via agent_turn's hardcoded None
),
```

- [ ] **Step 4: Update the pub use in agent/mod.rs if needed**

Check `src/agent/mod.rs` — the re-export should still work since we only added an optional parameter.

- [ ] **Step 5: Fix all callers of process_message_with_history**

The only caller is `run_acp_agent_loop` in `src/gateway/acp_server.rs:659`. Update it to pass the injection_rx (handled in Task 2). For now, ensure it compiles by passing `None`:
```rust
crate::agent::process_message_with_history(config, message, existing_history, None).await;
```

- [ ] **Step 6: Run tests**

```bash
cargo test --lib agent::loop_::tests
cargo clippy --all-targets -- -D warnings
```

- [ ] **Step 7: Commit**

```bash
git add src/agent/loop_.rs src/agent/mod.rs
git commit -m "feat(agent): thread injection_rx through process_message_with_history and agent_turn"
```

---

### Task 2: Add session/inject to the ACP server

**Files:**
- Modify: `src/gateway/acp_server.rs`

- [ ] **Step 1: Add InjectionSender to AcpTransportSession**

In the `AcpTransportSession` struct, add a field to hold the injection sender:
```rust
pub(crate) struct AcpTransportSession {
    pub id: String,
    pub agent_session_id: Option<String>,
    pub history: Vec<crate::providers::ChatMessage>,
    pub created_at: std::time::Instant,
    /// Sender for mid-task message injection (mirrors channel injection).
    pub injection_tx: Option<crate::channels::injection::InjectionSender>,
}
```

- [ ] **Step 2: Create injection channel in handle_session_prompt**

In `handle_session_prompt` (line 459), before spawning the background task, create the injection channel and store the sender in the session:

```rust
// Create injection channel for mid-task messages
let (injection_tx, injection_rx) =
    tokio::sync::mpsc::unbounded_channel::<crate::channels::injection::InjectedMessage>();

// Store sender in session so session/inject can find it
if let Some(mut session) = store.get(&transport_id) {
    session.injection_tx = Some(injection_tx);
    store.update(session);
}
```

Pass `Some(injection_rx)` to `run_acp_agent_loop`:
```rust
run_acp_agent_loop(config, &prompt_text, existing_history, Some(injection_rx), inner_tx).await
```

- [ ] **Step 3: Update run_acp_agent_loop to accept and forward injection_rx**

```rust
async fn run_acp_agent_loop(
    config: Config,
    message: &str,
    existing_history: Vec<crate::providers::ChatMessage>,
    injection_rx: Option<crate::channels::injection::InjectionReceiver>,
    tx: tokio::sync::mpsc::Sender<String>,
) -> anyhow::Result<(String, Vec<crate::providers::ChatMessage>)> {
    // ... existing notification code ...
    crate::agent::process_message_with_history(config, message, existing_history, injection_rx).await
}
```

- [ ] **Step 4: Clear injection_tx when task completes**

In the task completion handler (line 556-611), after persisting history, clear the injection_tx:
```rust
if let Some(mut session) = store_clone.get(&transport_id) {
    session.history = updated_history;
    session.injection_tx = None;  // Task done, no more injection
    store_clone.update(session);
}
```

- [ ] **Step 5: Add handle_session_inject method**

Add a new handler for the `session/inject` JSON-RPC method:

```rust
/// `session/inject` — inject a mid-task message into a running agent loop.
async fn handle_session_inject(
    req: &JsonRpcRequest,
    headers: &HeaderMap,
    store: &AcpSessionStore,
) -> Response {
    let transport_id = headers
        .get("Acp-Session-Id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let session = match store.get(&transport_id) {
        Some(s) => s,
        None => {
            let err = jsonrpc_error(&req.id, -32000, "Invalid or expired Acp-Session-Id");
            return sse_response(sse_line(&err));
        }
    };

    // Parse inject params (same structure as session/prompt params)
    let params: SessionPromptParams = match serde_json::from_value(req.params.clone()) {
        Ok(p) => p,
        Err(e) => {
            let err = jsonrpc_error(&req.id, -32602, &format!("Invalid params: {e}"));
            return sse_response(sse_line(&err));
        }
    };

    let inject_text = params
        .prompt
        .iter()
        .filter_map(|p| p.text.as_deref())
        .collect::<Vec<_>>()
        .join("\n");

    if inject_text.is_empty() {
        let err = jsonrpc_error(&req.id, -32602, "Empty inject message");
        return sse_response(sse_line(&err));
    }

    // Send to the injection channel
    match &session.injection_tx {
        Some(tx) => {
            let msg = crate::channels::injection::InjectedMessage {
                content: inject_text.clone(),
                channel: "acp".to_string(),
                sender: transport_id.clone(),
            };
            match tx.send(msg) {
                Ok(()) => {
                    tracing::info!(
                        acp_session_id = %transport_id,
                        inject_len = inject_text.len(),
                        "ACP session/inject: message queued"
                    );
                    let result = jsonrpc_result(
                        &req.id,
                        serde_json::json!({"injected": true}),
                    );
                    sse_response(sse_line(&result))
                }
                Err(_) => {
                    let err = jsonrpc_error(&req.id, -32000, "Injection channel closed — task may have completed");
                    sse_response(sse_line(&err))
                }
            }
        }
        None => {
            let err = jsonrpc_error(&req.id, -32000, "No running task to inject into — send a session/prompt first");
            sse_response(sse_line(&err))
        }
    }
}
```

- [ ] **Step 6: Register session/inject in the method dispatch**

In the main `handle_acp_request` function, add the new method to the match:
```rust
"session/inject" => handle_session_inject(&req, &headers, &store).await,
```

- [ ] **Step 7: Run tests and add new test**

```bash
cargo test --lib gateway::acp_server::tests
cargo clippy --all-targets -- -D warnings
```

Add a test verifying injection flow: create session → send prompt → inject message → verify message appears in agent context.

- [ ] **Step 8: Commit**

```bash
git add src/gateway/acp_server.rs
git commit -m "feat(acp): add session/inject for mid-task message injection"
```

---

## Chunk 2: acp-client Python changes

### Task 3: Add inject and progress commands to acp-client

**Files:**
- Modify: `k8s/goose/03_goose_acp/04_acp_client_configmap.yaml`

- [ ] **Step 1: Add cmd_inject function**

```python
def cmd_inject(args):
    """Inject a mid-task message into a running ACP session."""
    if len(args) < 2:
        print("Usage: acp-client inject <acp_session_id> \"message\"", file=sys.stderr)
        sys.exit(1)
    acp_session_id = args[0]
    message = " ".join(args[1:])

    # Read the session's stored info
    session_dir = os.path.join(SESSION_DIR, acp_session_id)
    status_file = os.path.join(session_dir, "status.json")
    if not os.path.exists(status_file):
        print(f"ERROR: No session found at {session_dir}", file=sys.stderr)
        sys.exit(1)

    with open(status_file) as f:
        status = json.load(f)
    session_id = status.get("session_id", "")

    # Send session/inject via JSON-RPC
    payload = {
        "jsonrpc": "2.0",
        "method": "session/inject",
        "id": 3,
        "params": {
            "sessionId": session_id,
            "prompt": [{"type": "text", "text": message}]
        }
    }
    resp = _post_acp(payload, headers={"Acp-Session-Id": acp_session_id}, timeout=30)
    result = _extract_result_text(resp)
    if result:
        print(f"INJECTED into {acp_session_id}")
    else:
        print(f"INJECT FAILED: {resp}", file=sys.stderr)
        sys.exit(1)
```

- [ ] **Step 2: Add cmd_progress function**

```python
def cmd_progress(args):
    """Show the current progress of a running ACP session (partial response)."""
    if len(args) < 1:
        print("Usage: acp-client progress <acp_session_id>", file=sys.stderr)
        sys.exit(1)
    acp_session_id = args[0]
    session_dir = os.path.join(SESSION_DIR, acp_session_id)
    response_file = os.path.join(session_dir, "response.txt")
    status_file = os.path.join(session_dir, "status.json")

    if not os.path.exists(status_file):
        print(f"No session: {acp_session_id}", file=sys.stderr)
        sys.exit(1)

    with open(status_file) as f:
        status = json.load(f)

    state = status.get("status", "UNKNOWN")
    elapsed = time.time() - status.get("started_at", time.time())

    if os.path.exists(response_file):
        with open(response_file) as f:
            content = f.read()
        # Show last 2000 chars of progress
        if len(content) > 2000:
            content = "...\n" + content[-2000:]
        print(f"[{state}] {elapsed:.0f}s elapsed")
        print(content)
    else:
        print(f"[{state}] {elapsed:.0f}s elapsed, no output yet")
```

- [ ] **Step 3: Register new commands in main dispatch**

```python
commands = {
    "health": cmd_health,
    "send": cmd_send,
    "poll": cmd_poll,
    "wait": cmd_wait,
    "result": cmd_result,
    "send-sync": cmd_send_sync,
    "delete": cmd_delete,
    "cleanup": cmd_cleanup,
    "inject": cmd_inject,      # NEW
    "progress": cmd_progress,   # NEW
    "_collect": cmd_collect,
}
```

- [ ] **Step 4: Validate Python syntax**

```bash
python3 -c "import ast; ast.parse(open('acp-client.py').read()); print('OK')"
```

- [ ] **Step 5: Commit**

```bash
git add k8s/goose/03_goose_acp/04_acp_client_configmap.yaml
git commit -m "feat(acp-client): add inject and progress commands for bidirectional ACP"
```

---

## Chunk 3: Skill updates

### Task 4: Update Sam's k8s-delegation skill

**Files:**
- Modify: `scrapyard-applications/04_scrapyard_test_projects/32_zeroclaw/13_zeroclaw_skills_configmap.yaml`

- [ ] **Step 1: Add ACP progress monitoring and injection to the skill**

Add a new section after "After Each Work Order":

```markdown
## Monitoring Walter's Progress

Don't blindly wait for Walter to finish. Use progress checks:

    # Check what Walter is doing (shows partial output)
    acp-client progress <session_id>

If Walter has been working for more than 5 minutes with no progress, or if you
see him going in the wrong direction, inject a course correction:

    # Send a mid-task message — Walter sees it on his next tool iteration
    acp-client inject <session_id> "CORRECTION: Use SQLite instead of Postgres for this deployment."

Injected messages appear in Walter's context as [Mid-turn message from user].
He will see them after his current tool call completes.

## Recommended Flow (replaces blind wait)

    # 1. Send the work order
    acp-client send "TASK: ..."
    # → PENDING acp_session_id=abc-123

    # 2. Wait with timeout — but check progress if it's taking long
    acp-client wait abc-123 --timeout 600

    # If wait times out or you need to redirect:
    acp-client progress abc-123
    acp-client inject abc-123 "Walter, prioritize getting the deployment running first. Skip external access for now."

    # 3. Get the final result
    acp-client result abc-123

    # 4. Cleanup
    acp-client delete abc-123
```

- [ ] **Step 2: Update "What NOT to Do" section**

Add:
```markdown
- Don't let Walter run for 20+ minutes without checking progress
```

- [ ] **Step 3: Commit**

```bash
cd scrapyard-applications
git add 04_scrapyard_test_projects/32_zeroclaw/13_zeroclaw_skills_configmap.yaml
git commit -m "feat(skills): add ACP progress monitoring and injection to Sam's k8s-delegation skill"
```

### Task 5: Update Walter's k8s-manifest-builder skill

**Files:**
- Modify: `scrapyard-applications/04_scrapyard_test_projects/33_goose_todolist_sandbox/04_zeroclaw_k8s_agent/04_skills_configmap.yaml`

- [ ] **Step 1: Add mid-task message awareness**

Add a section after the "Recovery: If You Lose Track" section:

```markdown
## Mid-Task Messages from Sam

Sam may send you corrections or priority changes while you're working.
These appear in your context as:

    [Mid-turn message from user]
    CORRECTION: Use SQLite instead of Postgres.

When you see one:
1. **Read it immediately** — it may change your current task
2. **Acknowledge it** in your internal reasoning
3. **Adjust your plan** — Sam's corrections override the original work order
4. Don't restart from scratch unless the correction invalidates everything you've done
```

- [ ] **Step 2: Commit**

```bash
cd scrapyard-applications
git add 04_scrapyard_test_projects/33_goose_todolist_sandbox/04_zeroclaw_k8s_agent/04_skills_configmap.yaml
git commit -m "feat(skills): add mid-task injection awareness to Walter's k8s-manifest-builder skill"
```

### Task 6: Update Sam's TOOLS.md with new acp-client commands

**Files:**
- Modify: `scrapyard-applications/04_scrapyard_test_projects/32_zeroclaw/05_zeroclaw_identity_configmap.yaml`

- [ ] **Step 1: Add inject and progress to the ACP commands table**

In Sam's TOOLS.md section, update the acp-client commands table:

```markdown
| `acp-client inject <id> "msg"` | Inject a mid-task message into Walter's context (he sees it on next tool iteration) |
| `acp-client progress <id>` | Show Walter's partial output so far (last 2000 chars) |
```

- [ ] **Step 2: Commit**

```bash
cd scrapyard-applications
git add 04_scrapyard_test_projects/32_zeroclaw/05_zeroclaw_identity_configmap.yaml
git commit -m "docs(identity): add inject and progress to Sam's ACP commands table"
```

---

## Verification

### Unit tests
```bash
cargo test --lib agent::loop_::tests
cargo test --lib gateway::acp_server::tests
cargo clippy --all-targets -- -D warnings
```

### Integration test (manual)
1. Build and deploy zeroclaw with the new injection support
2. Apply updated ConfigMaps (skills + acp-client)
3. Restart both pods
4. Send Sam a task that triggers Walter delegation
5. While Walter is working, verify:
   - `acp-client progress <id>` shows partial output
   - `acp-client inject <id> "test message"` succeeds
   - Walter's response reflects the injected message
6. Verify Sam's skill teaches her to use the new commands

### Success criteria
- Sam can see Walter's progress mid-task (not blind blocking)
- Sam can inject corrections that Walter receives between tool calls
- Walter's skill explains how to handle mid-turn messages
- No regression in existing ACP request/response flow
