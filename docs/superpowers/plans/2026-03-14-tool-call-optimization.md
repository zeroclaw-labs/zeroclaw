# Tool Call Optimization Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate infrastructure failures (session contention, token overflow, malformed tool calls, raw error dumps), extract skill-specific logic into SKILL.toml config, and prevent LLM link hallucination.

**Architecture:** 5 blocks executed in order 1→4→2→3→5, plus Block 6 (link hallucination). Blocks 1+4 are parallel limits + token budget (fix root cause). Block 2 fixes malformed XML tool calls. Block 3 sanitizes error output. Block 5 extracts config. Block 6 validates source_url against real search results. Each block is independently testable.

## Status

| Block | Task | Status | Commit |
|-------|------|--------|--------|
| 1+4 | Parallel limits + per-tool cap + token budget | **DONE** | `52713807` |
| 2 | Malformed `<tool_call>` stripping | **DONE** | `cb0488bb` |
| 3 | Provider error sanitization | **DONE** | `be961f84` |
| 5 | Skill config extraction (env vars) | **DONE** | `fbc216bf` |
| 6 | Link hallucination prevention | **DONE** | (Python-only, no Rust commit) |

### E2E Validation (b1-b4, 2026-03-14)

| Test | Before | After | Status |
|------|--------|-------|--------|
| b1 | 33s | 12.5s | PASS |
| b2 | 50s | 50.5s | PASS |
| b3 | 22s | 11.8s | PASS |
| b4 | 900s+ timeout | 17.9s | PASS |

**Tech Stack:** Rust (tokio semaphore, serde), Python (env var reads), TOML config

**Spec:** `docs/superpowers/specs/2026-03-14-tool-call-optimization-design.md`

---

## Chunk 1: Parallel Tool Call Limits + Token Budget (Blocks 1 & 4)

### Task 1: Add config fields to AgentConfig and SkillTool

**Files:**
- Modify: `src/config/schema.rs:606-628` (AgentConfig struct)
- Modify: `src/skills/mod.rs:40-59` (SkillTool struct)

- [ ] **Step 1: Add fields to AgentConfig**

In `src/config/schema.rs`, after the `parallel_tools` field (line ~620), add:

```rust
    /// Maximum number of tool calls executed in parallel per iteration. Default: `5`.
    /// If the LLM requests more calls, they are batched into sequential groups.
    #[serde(default = "default_max_parallel_tool_calls")]
    pub max_parallel_tool_calls: usize,
    /// Maximum chars kept per tool result in conversation history. Default: `4000`.
    /// Results exceeding this are truncated with a `...(truncated)` suffix.
    #[serde(default = "default_max_tool_result_chars")]
    pub max_tool_result_chars: usize,
```

Add default functions near the other `default_agent_*` functions:

```rust
fn default_max_parallel_tool_calls() -> usize { 5 }
fn default_max_tool_result_chars() -> usize { 4000 }
```

- [ ] **Step 2: Add fields to SkillTool**

In `src/skills/mod.rs`, in the `SkillTool` struct (line ~40), after the `terminal` field, add:

```rust
    /// Maximum concurrent executions of this tool. Overrides global `max_parallel_tool_calls`.
    #[serde(default)]
    pub max_parallel: Option<usize>,
    /// Maximum chars kept in result for conversation history. Overrides global `max_tool_result_chars`.
    #[serde(default)]
    pub max_result_chars: Option<usize>,
```

- [ ] **Step 3: Verify compilation**

Run: `cargo check 2>&1 | tail -5`
Expected: compiles with no errors (new fields have defaults, all existing code unaffected)

- [ ] **Step 4: Commit**

```bash
git add src/config/schema.rs src/skills/mod.rs
git commit -m "feat(config): add max_parallel_tool_calls and max_tool_result_chars fields"
```

---

### Task 2: Implement parallel batching in tool loop

**Files:**
- Modify: `src/agent/loop_.rs:2673-2689` (tool execution dispatch)

- [ ] **Step 1: Read current execution dispatch code**

Read `src/agent/loop_.rs` lines 2660-2700 to understand the current parallel/sequential branching.

- [ ] **Step 2: Add batching logic before execution**

Replace the parallel execution block at lines 2673-2689. The new logic:
- Get `max_parallel` from config (passed through function params or read from a shared config ref)
- If `executable_calls.len() > max_parallel`, split into batches
- Execute each batch with `execute_tools_parallel`, collect results sequentially

```rust
        let max_parallel = agent_config
            .as_ref()
            .map(|c| c.max_parallel_tool_calls)
            .unwrap_or(5);

        let executed_outcomes = if allow_parallel_execution && executable_calls.len() > 1 {
            if executable_calls.len() <= max_parallel {
                // Small batch — run all in parallel
                execute_tools_parallel(
                    &executable_calls,
                    tools_registry,
                    observer,
                    cancellation_token.as_ref(),
                )
                .await?
            } else {
                // Large batch — split into sequential groups of max_parallel
                tracing::info!(
                    total = executable_calls.len(),
                    batch_size = max_parallel,
                    "Batching tool calls to limit parallelism"
                );
                let mut all_outcomes = Vec::new();
                for chunk in executable_calls.chunks(max_parallel) {
                    let batch = execute_tools_parallel(
                        chunk,
                        tools_registry,
                        observer,
                        cancellation_token.as_ref(),
                    )
                    .await?;
                    all_outcomes.extend(batch);
                }
                all_outcomes
            }
        } else {
            execute_tools_sequential(
                &executable_calls,
                tools_registry,
                observer,
                cancellation_token.as_ref(),
            )
            .await?
        };
```

Note: `agent_config` needs to be threaded into `run_tool_call_loop`. Check the function signature — it likely already has access to config or can get it from a parameter. If not, pass `max_parallel_tool_calls: usize` as a new parameter.

- [ ] **Step 3: Verify compilation**

Run: `cargo check 2>&1 | tail -5`
Expected: compiles

- [ ] **Step 4: Run unit tests**

Run: `cargo test --lib 2>&1 | tail -10`
Expected: all existing tests pass

- [ ] **Step 5: Commit**

```bash
git add src/agent/loop_.rs
git commit -m "feat(agent): batch parallel tool calls to max_parallel_tool_calls limit"
```

---

### Task 3: Implement per-tool semaphore in SkillToolHandler

**Files:**
- Modify: `src/skills/tool_handler.rs:40-48` (struct fields)
- Modify: `src/skills/tool_handler.rs:328-343` (execute method)
- Modify: `src/skills/mod.rs:105-150` (factory function)

- [ ] **Step 1: Add semaphore to SkillToolHandler**

In `src/skills/tool_handler.rs`, add a semaphore field to the struct:

```rust
use tokio::sync::Semaphore;
use std::sync::Arc;

pub struct SkillToolHandler {
    skill_name: String,
    tool_def: SkillTool,
    parameters: Vec<SkillToolParameter>,
    security: Arc<SecurityPolicy>,
    skill_dir: Option<PathBuf>,
    /// Per-tool concurrency limiter. None = unlimited.
    concurrency_limit: Option<Arc<Semaphore>>,
}
```

- [ ] **Step 2: Initialize semaphore in constructor**

In the `new()` or factory function where `SkillToolHandler` is created (in `mod.rs` create_skill_tools), set:

```rust
let concurrency_limit = tool_def.max_parallel.map(|n| Arc::new(Semaphore::new(n)));
```

- [ ] **Step 3: Acquire permit in execute()**

At the top of `execute()` method (line 328), before rate limit check:

```rust
    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        // Per-tool concurrency limit
        let _permit = match &self.concurrency_limit {
            Some(sem) => Some(
                sem.acquire()
                    .await
                    .map_err(|e| anyhow::anyhow!("Semaphore closed: {e}"))?,
            ),
            None => None,
        };

        if self.security.is_rate_limited() {
            // ... existing code
```

The `_permit` is held for the duration of execute() and dropped automatically when the function returns.

- [ ] **Step 4: Verify compilation**

Run: `cargo check 2>&1 | tail -5`

- [ ] **Step 5: Commit**

```bash
git add src/skills/tool_handler.rs src/skills/mod.rs
git commit -m "feat(skills): per-tool concurrency semaphore from max_parallel config"
```

---

### Task 4: Implement tool result truncation

**Files:**
- Modify: `src/agent/loop_.rs:2750-2756` (result accumulation)

- [ ] **Step 1: Add truncation helper**

Near the top of `loop_.rs` (after the existing helper functions around line 100), add:

```rust
/// Truncate a tool result to `max_chars`, preserving UTF-8 boundaries.
fn truncate_tool_result(output: &str, max_chars: usize) -> String {
    if output.len() <= max_chars {
        return output.to_string();
    }
    let boundary = output.floor_char_boundary(max_chars);
    format!(
        "{}\n...(truncated, {} chars total)",
        &output[..boundary],
        output.len()
    )
}
```

- [ ] **Step 2: Apply truncation when accumulating results**

At line 2750, where `individual_results.push()` happens, truncate the output:

```rust
            // Per-tool max_result_chars override, then global default
            let tool_max_chars = tools_registry
                .iter()
                .find(|t| t.name() == tool_name)
                .and_then(|t| t.max_result_chars())
                .unwrap_or(max_tool_result_chars);

            let truncated_output = truncate_tool_result(&outcome.output, tool_max_chars);
            individual_results.push((tool_call_id, truncated_output.clone()));
            let _ = writeln!(
                tool_results,
                "<tool_result name=\"{}\">\n{}\n</tool_result>",
                tool_name, truncated_output
            );
```

Note: `max_tool_result_chars` comes from config (threaded through function params). The `max_result_chars()` method needs to be added to the `Tool` trait or accessed via downcasting. Simplest approach: add `fn max_result_chars(&self) -> Option<usize> { None }` to `Tool` trait with default impl, override in `SkillToolHandler`.

- [ ] **Step 3: Add max_result_chars to Tool trait**

In `src/tools/traits.rs`, add default method:

```rust
    /// Maximum chars to keep in tool result for conversation history.
    /// Returns None to use the global default.
    fn max_result_chars(&self) -> Option<usize> { None }
```

In `src/skills/tool_handler.rs`, override:

```rust
    fn max_result_chars(&self) -> Option<usize> {
        self.tool_def.max_result_chars
    }
```

- [ ] **Step 4: Verify compilation**

Run: `cargo check 2>&1 | tail -5`

- [ ] **Step 5: Run tests**

Run: `cargo test --lib 2>&1 | tail -10`

- [ ] **Step 6: Commit**

```bash
git add src/agent/loop_.rs src/tools/traits.rs src/skills/tool_handler.rs
git commit -m "feat(agent): truncate tool results to max_tool_result_chars"
```

---

### Task 5: Implement token budget compaction

**Files:**
- Modify: `src/agent/loop_.rs` (after tool results assembled, before next iteration)

- [ ] **Step 1: Add budget compaction after result assembly**

After the history push block (around line 2849), before the loop continues to next iteration, add:

```rust
        // Token budget safety net — compact history if context is too large
        let total_chars: usize = history.iter().map(|m| m.content.len()).sum();
        let approx_tokens = total_chars / 3;

        if approx_tokens > 800_000 {
            tracing::warn!(
                approx_tokens,
                history_len = history.len(),
                "Token budget critical — aggressive compaction"
            );
            compact_history_for_budget(history, 1000, 10);
        } else if approx_tokens > 500_000 {
            tracing::info!(
                approx_tokens,
                history_len = history.len(),
                "Token budget high — soft compaction"
            );
            compact_history_for_budget(history, 2000, 15);
        }
```

- [ ] **Step 2: Add compact_history_for_budget helper**

```rust
/// Emergency compaction: truncate tool results in history and keep only recent messages.
fn compact_history_for_budget(
    history: &mut Vec<ChatMessage>,
    max_result_chars: usize,
    keep_recent: usize,
) {
    // Drop old messages, keep system prompt (first) + recent
    if history.len() > keep_recent + 1 {
        let system = history[0].clone();
        let recent: Vec<_> = history.iter().rev().take(keep_recent).cloned().collect();
        history.clear();
        history.push(system);
        history.extend(recent.into_iter().rev());
    }
    // Truncate tool results in remaining messages
    for msg in history.iter_mut() {
        if msg.role == "tool" && msg.content.len() > max_result_chars {
            msg.content = truncate_tool_result(&msg.content, max_result_chars);
        }
    }
}
```

- [ ] **Step 3: Verify compilation and tests**

Run: `cargo check && cargo test --lib 2>&1 | tail -10`

- [ ] **Step 4: Commit**

```bash
git add src/agent/loop_.rs
git commit -m "feat(agent): token budget compaction safety net at 500K/800K thresholds"
```

---

### Task 6: Update SKILL.toml with per-tool limits

**Files:**
- Modify: `~/.zeroclaw/workspace/skills/telegram-reader/SKILL.toml`

- [ ] **Step 1: Add max_parallel and max_result_chars to skill tools**

Add to `telegram_search_messages` tool section:
```toml
max_parallel = 3
max_result_chars = 2000
```

Add to `telegram_search_global` tool section:
```toml
max_result_chars = 3000
```

Add to `submit_contacts` tool section:
```toml
max_result_chars = 8000
```

- [ ] **Step 2: Commit SKILL.toml changes**

```bash
cd ~/.zeroclaw && git add workspace/skills/telegram-reader/SKILL.toml && git commit -m "feat: add max_parallel and max_result_chars to telegram-reader tools"
```

---

### Task 7: Build, restart daemon, E2E test Block 1+4

- [ ] **Step 1: Build release binary**

Run: `cargo build --release 2>&1 | tail -5`

- [ ] **Step 2: Restart daemon**

```bash
kill $(pgrep -f "target/release/zeroclaw") 2>/dev/null; sleep 2
set -a && source .env && set +a && nohup ./target/release/zeroclaw daemon >> /tmp/zeroclaw_daemon.log 2>&1 &
sleep 5 && pgrep -f "target/release/zeroclaw" && echo "OK"
```

- [ ] **Step 3: Run b4 test (was timing out from 30+ parallel calls)**

```bash
source .env && cargo test --test telegram_search_quality -- --ignored b4 --test-threads=1 --nocapture
```
Expected: PASS (no more session contention or token overflow)

- [ ] **Step 4: Run b10 test (was timing out from session contention)**

```bash
sleep 90 && source .env && cargo test --test telegram_search_quality -- --ignored b10 --test-threads=1 --nocapture
```
Expected: PASS

---

## Chunk 2: Malformed Tool Call Recovery (Block 2)

### Task 8: Fix malformed `<tool_call>` parsing

**Files:**
- Modify: `src/agent/loop_.rs:1399-1403` (malformed warning)
- Modify: `src/agent/loop_.rs:2417-2423` (final response with no tool calls)

- [ ] **Step 1: Read the parse_tool_calls function**

Read `src/agent/loop_.rs` lines 1323-1420 to understand current parsing logic and why it fails on the malformed format.

- [ ] **Step 2: Fix the parser to handle bare JSON in `<tool_call>` tags**

The current parser expects XML attributes or specific formats inside `<tool_call>`. The model generates:
```
<tool_call>
{"name": "submit_contacts", "arguments": {...}}
```

In the `parse_tool_calls` function, after existing tag parsing (around line 1399 where the malformed warning is), add a fallback:

```rust
            if !parsed_any {
                // Try bare JSON inside <tool_call> tags
                let body = body.trim();
                if body.starts_with('{') {
                    if let Ok(json_val) = serde_json::from_str::<serde_json::Value>(body) {
                        if let (Some(name), Some(args)) = (
                            json_val.get("name").and_then(|v| v.as_str()),
                            json_val.get("arguments"),
                        ) {
                            calls.push(ParsedToolCall {
                                name: name.to_string(),
                                arguments: args.clone(),
                            });
                            parsed_any = true;
                        }
                    }
                }
                if !parsed_any {
                    tracing::warn!(
                        "Malformed <tool_call>: expected tool-call object in tag body (JSON/XML/GLM)"
                    );
                }
            }
```

- [ ] **Step 3: Prevent `<tool_call>` text from reaching user**

At line 2417 (where "no tool calls" returns final response), add a check:

```rust
        if tool_calls.is_empty() {
            // Don't send raw <tool_call> tags to user — strip them
            let cleaned = strip_tool_result_blocks(&display_text);
            let cleaned = if cleaned.contains("<tool_call>") || cleaned.contains("<tool-call>") {
                // Model generated malformed tool calls that couldn't be parsed —
                // don't leak XML to user
                tracing::warn!("Stripping unparseable <tool_call> from final response");
                String::new()
            } else {
                cleaned
            };

            if cleaned.is_empty() {
                return Err(anyhow::anyhow!("Model produced no usable response"));
            }

            // ... existing final response logic using `cleaned` instead of `display_text`
```

- [ ] **Step 4: Verify compilation and tests**

Run: `cargo check && cargo test --lib 2>&1 | tail -10`

- [ ] **Step 5: Commit**

```bash
git add src/agent/loop_.rs
git commit -m "fix(agent): parse bare JSON in <tool_call> tags, prevent XML leaking to user"
```

---

## Chunk 3: Error Sanitization (Block 3)

### Task 9: Sanitize provider error dumps before sending to channel

**Files:**
- Modify: `src/channels/mod.rs` (before message send)

- [ ] **Step 1: Add error sanitization function**

Add near `strip_tool_call_tags` (around line 442):

```rust
/// Detect and sanitize raw provider error dumps in outgoing messages.
/// Returns None if message is clean, Some(sanitized) if errors were detected.
fn sanitize_provider_errors(message: &str) -> Option<String> {
    // Strip "(continued)\n\n" prefix from split messages
    let text = message
        .strip_prefix("(continued)")
        .map(|s| s.trim_start())
        .unwrap_or(message);

    // Detect provider error dump patterns
    let is_error_dump = text.contains("provider=")
        && (text.contains("attempt ") || text.contains("non_retryable") || text.contains("rate_limited"));

    if !is_error_dump {
        return None;
    }

    // Classify the primary error
    let sanitized = if text.contains("input token count") && text.contains("exceeds") {
        "Запрос слишком большой — попробуйте более конкретный вопрос."
    } else if text.contains("rate_limited") || text.contains("RESOURCE_EXHAUSTED") {
        "Все провайдеры перегружены, попробуйте через минуту."
    } else if text.contains("model is not supported") || text.contains("UNAUTHENTICATED") {
        "Ошибка конфигурации провайдера."
    } else {
        "Не удалось обработать запрос. Попробуйте позже."
    };

    tracing::warn!(
        original_len = message.len(),
        sanitized,
        "Sanitized provider error dump in outgoing message"
    );

    Some(sanitized.to_string())
}
```

- [ ] **Step 2: Apply sanitization before channel send**

Find the location where the final reply text is sent to the channel (likely in the channel message handler or where `channel.send_message()` is called). Apply sanitization:

```rust
    let reply_text = if let Some(sanitized) = sanitize_provider_errors(&reply_text) {
        sanitized
    } else {
        reply_text
    };
```

- [ ] **Step 3: Verify compilation**

Run: `cargo check 2>&1 | tail -5`

- [ ] **Step 4: Commit**

```bash
git add src/channels/mod.rs
git commit -m "fix(channels): sanitize provider error dumps in user-facing messages"
```

---

### Task 10: Build, restart, full E2E run

- [ ] **Step 1: Build release**

Run: `cargo build --release 2>&1 | tail -5`

- [ ] **Step 2: Restart daemon**

```bash
kill $(pgrep -f "target/release/zeroclaw") 2>/dev/null; sleep 2
set -a && source .env && set +a && nohup ./target/release/zeroclaw daemon >> /tmp/zeroclaw_daemon.log 2>&1 &
sleep 5 && pgrep -f "target/release/zeroclaw" && echo "OK"
```

- [ ] **Step 3: Run full E2E suite sequentially**

Run each test with 60-90s pause between:
```bash
source .env
cargo test --test telegram_search_quality -- --ignored b1_ --test-threads=1 --nocapture
# 60s pause between each
cargo test --test telegram_search_quality -- --ignored b2_ --test-threads=1 --nocapture
cargo test --test telegram_search_quality -- --ignored b3 --test-threads=1 --nocapture
cargo test --test telegram_search_quality -- --ignored b4 --test-threads=1 --nocapture
cargo test --test telegram_search_quality -- --ignored b5 --test-threads=1 --nocapture
cargo test --test telegram_search_quality -- --ignored b6 --test-threads=1 --nocapture
cargo test --test telegram_search_quality -- --ignored b7 --test-threads=1 --nocapture
cargo test --test telegram_search_quality -- --ignored b8 --test-threads=1 --nocapture
cargo test --test telegram_search_quality -- --ignored b9 --test-threads=1 --nocapture
cargo test --test telegram_search_quality -- --ignored b10 --test-threads=1 --nocapture
cargo test --test telegram_search_quality -- --ignored b_new1 --test-threads=1 --nocapture
cargo test --test telegram_search_quality -- --ignored b_new2 --test-threads=1 --nocapture
cargo test --test telegram_search_quality -- --ignored d1 --test-threads=1 --nocapture
cargo test --test telegram_search_quality -- --ignored d2 --test-threads=1 --nocapture
cargo test --test telegram_search_quality -- --ignored d3 --test-threads=1 --nocapture
```
Expected: 15/15 PASS, no token overflow or session contention failures

---

## Chunk 4: Skill Config Extraction (Block 5)

### Task 11: Pass SKILL.toml validation/output config as env vars

**Files:**
- Modify: `src/skills/mod.rs:40-59` (SkillTool struct — add validation/output sections)
- Modify: `src/skills/tool_handler.rs:374-386` (env var setup in execute())

- [ ] **Step 1: Add validation and output config to SkillTool**

In `src/skills/mod.rs`, add to `SkillTool`:

```rust
    /// Validation rules passed to script as SKILL_VALIDATION_* env vars.
    #[serde(default)]
    pub validation: HashMap<String, toml::Value>,
    /// Output formatting rules passed as SKILL_OUTPUT_* env vars.
    #[serde(default)]
    pub output: HashMap<String, toml::Value>,
```

- [ ] **Step 2: Pass validation/output as env vars in execute()**

In `src/skills/tool_handler.rs`, in the `execute()` method where env vars are set (around line 374-386), after `SKILL_DIR`:

```rust
        // Pass validation config as SKILL_VALIDATION_* env vars
        for (key, val) in &self.tool_def.validation {
            let env_key = format!("SKILL_VALIDATION_{}", key.to_uppercase());
            let env_val = match val {
                toml::Value::String(s) => s.clone(),
                toml::Value::Boolean(b) => if *b { "1" } else { "0" }.to_string(),
                toml::Value::Integer(i) => i.to_string(),
                other => other.to_string(),
            };
            cmd.env(&env_key, &env_val);
        }
        // Pass output config as SKILL_OUTPUT_* env vars
        for (key, val) in &self.tool_def.output {
            let env_key = format!("SKILL_OUTPUT_{}", key.to_uppercase());
            let env_val = match val {
                toml::Value::String(s) => s.clone(),
                toml::Value::Boolean(b) => if *b { "1" } else { "0" }.to_string(),
                toml::Value::Integer(i) => i.to_string(),
                other => other.to_string(),
            };
            cmd.env(&env_key, &env_val);
        }
```

- [ ] **Step 3: Verify compilation**

Run: `cargo check 2>&1 | tail -5`

- [ ] **Step 4: Commit**

```bash
git add src/skills/mod.rs src/skills/tool_handler.rs
git commit -m "feat(skills): pass SKILL.toml validation/output config as env vars"
```

---

### Task 12: Update submit_contacts.py to read from env

**Files:**
- Modify: `~/.zeroclaw/workspace/skills/telegram-reader/scripts/submit_contacts.py`

- [ ] **Step 1: Replace hardcoded values with env reads (with fallbacks)**

At the top of submit_contacts.py, after imports:

```python
import re

# Read config from env (set by tool_handler from SKILL.toml), with hardcoded fallbacks
URL_PATTERN = os.environ.get("SKILL_VALIDATION_URL_PATTERN", r'https?://t\.me/(?:c/\d+|[^/]+)/\d+')
URL_VERIFY = os.environ.get("SKILL_VALIDATION_URL_VERIFY", "1") == "1"
URL_VERIFY_TIMEOUT = int(os.environ.get("SKILL_VALIDATION_URL_VERIFY_TIMEOUT_SECS", "3"))
MIN_PHONE_DIGITS = int(os.environ.get("SKILL_VALIDATION_MIN_PHONE_DIGITS", "7"))
REJECT_SEQUENTIAL = os.environ.get("SKILL_VALIDATION_REJECT_SEQUENTIAL_PHONES", "1") == "1"
REJECT_SAME_DIGIT = os.environ.get("SKILL_VALIDATION_REJECT_SAME_DIGIT_PHONES", "1") == "1"
VERBATIM_GATE = os.environ.get("SKILL_VALIDATION_VERBATIM_GATE", "1") == "1"
MIN_AUTHOR_MSG_CHARS = int(os.environ.get("SKILL_VALIDATION_MIN_AUTHOR_MESSAGE_CHARS", "30"))
MAX_MESSAGE_TEXT = int(os.environ.get("SKILL_OUTPUT_MAX_MESSAGE_TEXT_CHARS", "300"))
```

- [ ] **Step 2: Use variables instead of hardcoded values**

Replace all hardcoded occurrences:
- `r'https?://t\.me/(?:c/\d+|[^/]+)/\d+'` → `URL_PATTERN`
- `timeout=3` in requests.get → `timeout=URL_VERIFY_TIMEOUT`
- `len(digits) < 7` → `len(digits) < MIN_PHONE_DIGITS`
- Verbatim gate checks → wrap in `if VERBATIM_GATE:`
- `[:300]` truncation → `[:MAX_MESSAGE_TEXT]`
- `len(msg_text) >= 30` → `len(msg_text) >= MIN_AUTHOR_MSG_CHARS`

- [ ] **Step 3: Update SKILL.toml with validation/output sections**

Add to `[tools.submit_contacts]` in SKILL.toml:

```toml
[tools.submit_contacts.validation]
url_pattern = 'https?://t\.me/(?:c/\d+|[^/]+)/\d+'
url_verify = true
url_verify_timeout_secs = 3
min_phone_digits = 7
reject_sequential_phones = true
reject_same_digit_phones = true
verbatim_gate = true
min_author_message_chars = 30

[tools.submit_contacts.output]
format = "structured"
max_message_text_chars = 300
```

- [ ] **Step 4: Test submit_contacts still works**

Run a quick E2E test:
```bash
source .env && cargo test --test telegram_search_quality -- --ignored b1_ --test-threads=1 --nocapture
```
Expected: PASS (Python reads env vars with same defaults as before)

- [ ] **Step 5: Commit**

```bash
git add src/skills/mod.rs src/skills/tool_handler.rs
cd ~/.zeroclaw && git add workspace/skills/telegram-reader/scripts/submit_contacts.py workspace/skills/telegram-reader/SKILL.toml
git commit -m "refactor: extract submit_contacts validation config to SKILL.toml"
```

---

### Task 13: Final full E2E validation

- [ ] **Step 1: Rebuild and restart**

```bash
cargo build --release && kill $(pgrep -f "target/release/zeroclaw") 2>/dev/null
sleep 2 && set -a && source .env && set +a && nohup ./target/release/zeroclaw daemon >> /tmp/zeroclaw_daemon.log 2>&1 &
```

- [ ] **Step 2: Run all 15 tests**

Run each with 60s pause. All 15 must pass.

- [ ] **Step 3: Final commit with test results**

```bash
git add tests/telegram_search_quality.rs
git commit -m "test: verify all 15 E2E tests pass with tool call optimization"
```

---

## Chunk 5: Link Hallucination Prevention (Block 6)

### Task 14: Validate source_url against real search results

**Problem:** The LLM sometimes constructs `source_url` by combining a real channel username with a fabricated message_id (e.g. `t.me/samui_help/138985`). The link resolves (HTTP 200) because Telegram returns a page for any channel, but the message_id is invented — it doesn't come from search results.

**Root cause:** `submit_contacts.py` trusts whatever `source_url` the LLM provides. It validates URL format and HTTP status, but cannot verify the message_id actually appeared in search results.

**Approach:** Track real `message_link` values from search tools (search_global, search_messages) in the delegate agent's context, then cross-reference in submit_contacts.

- [ ] **Step 1: Collect seen message_links in telegram_reader.py**

In `search_global` and `search_messages` output, each result already contains `message_link`. Add a dedup file that accumulates all seen links during the agent session:

```python
# At end of search_global / search_messages, append links to session file
seen_links_path = os.path.join(os.environ.get("SKILL_DIR", "/tmp"), ".seen_links")
with open(seen_links_path, "a") as f:
    for msg in results:
        if msg.get("message_link"):
            f.write(msg["message_link"] + "\n")
```

- [ ] **Step 2: Validate source_url in submit_contacts.py**

```python
# Load seen links
seen_links_path = os.path.join(os.environ.get("SKILL_DIR", "/tmp"), ".seen_links")
seen_links = set()
if os.path.exists(seen_links_path):
    with open(seen_links_path) as f:
        seen_links = {line.strip() for line in f if line.strip()}

# In contact validation:
if source_url and seen_links and source_url not in seen_links:
    # LLM hallucinated this link — strip it
    log(f"HALLUCINATED link stripped: {source_url} (not in {len(seen_links)} seen links)")
    source_url = None
```

- [ ] **Step 3: Clean up .seen_links after submit_contacts**

Delete the file after submit_contacts runs so it doesn't leak across sessions.

- [ ] **Step 4: Add E2E test for link hallucination**

Add a test that verifies all `source_url` values in the bot's response actually come from search results (by checking they match t.me/channel/id format from known channels with real message IDs).

- [ ] **Step 5: Commit**

```bash
cd ~/.zeroclaw && git add workspace/skills/telegram-reader/scripts/
git commit -m "fix: strip hallucinated source_url links not from search results"
```
