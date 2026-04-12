# Reasoning Content Display Fix

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make reasoning/thinking content from Gemma 4 (via vLLM's `--reasoning-parser`) visible in the final response, not just during streaming draft updates.

**Architecture:** The `reasoning_content` field is already captured at `src/agent/loop_.rs:1601` and preserved in conversation history. The fix: before the final return at line 2059, prepend the reasoning content to `display_text` in a formatted block. This is gated by a new `show_reasoning: bool` config field (default false). When enabled, the final message includes reasoning as an italic/indented block above the answer. The streaming path already shows reasoning during drafts — this fix ensures it persists in the finalized message.

**Tech Stack:** Rust

---

### Task 1: Add `show_reasoning` config field

**Files:**
- Modify: `src/config/schema.rs`

The new field goes in the top-level `Config` struct (near `strip_prior_reasoning` at line 408), not in `PresentationConfig`, because it controls conversation behavior rather than tool output formatting.

- [ ] **Step 1: Add the field**

In `src/config/schema.rs`, find `strip_prior_reasoning` (line 408). Add after it:

```rust
    /// Include the model's reasoning/thinking content in the final response
    /// visible to the user. When enabled, reasoning is prepended as an italic
    /// block above the answer text. Useful for thinking-mode models like
    /// Gemma 4 (vLLM --reasoning-parser) where reasoning is separated into
    /// its own field. Default: false.
    #[serde(default)]
    pub show_reasoning: bool,
```

Update the `Default` impl for `Config` to include `show_reasoning: false`.

- [ ] **Step 2: Run tests**

Run: `cargo test --lib config -- --nocapture 2>&1 | tail -5`
Expected: Pass (backward compatible via `#[serde(default)]`).

- [ ] **Step 3: Commit**

```bash
git add src/config/schema.rs
git commit -m "feat(config): add show_reasoning option for thinking-mode models"
```

---

### Task 2: Prepend reasoning to display text in agent loop

**Files:**
- Modify: `src/agent/loop_.rs`

The reasoning content variable (`reasoning_content: Option<String>`) is defined at line 1601 and is in scope at the final return point (line 2059). We need to prepend it to `display_text` before returning.

- [ ] **Step 1: Find the config access pattern**

The agent loop accesses config via task-locals. Search for how `show_reasoning` can be accessed. The function `run_tool_call_loop` doesn't take a Config reference directly. Check if there's a task-local for the full config, or if we need to add a parameter.

Search for `TOOL_LOOP` task-locals to find the existing pattern:

```bash
grep "TOOL_LOOP.*task_local\|task_local.*TOOL_LOOP" src/agent/loop_.rs | head -10
```

If there's no existing config task-local, the simplest approach is to add `show_reasoning: bool` as a field on one of the existing task-local context structs, or add a new one.

Alternative: since the `TOOL_LOOP_PRESENTATION_CONFIG` task-local already exists (used by schema simplification), we could put `show_reasoning` in `PresentationConfig` instead. This is pragmatically easier even if architecturally it's a stretch. Choose whichever approach matches the existing patterns.

- [ ] **Step 2: Format and prepend reasoning content**

Just before line 2059 (`return Ok(display_text)`), add:

```rust
            // Prepend reasoning content if configured to show it.
            let show_reasoning = TOOL_LOOP_PRESENTATION_CONFIG
                .try_with(|c| c.show_reasoning)
                .unwrap_or(false);
            if show_reasoning {
                if let Some(ref reasoning) = reasoning_content {
                    if !reasoning.is_empty() {
                        let formatted = format_reasoning_for_display(reasoning);
                        let display_with_reasoning = format!("{formatted}\n\n{display_text}");
                        return Ok(display_with_reasoning);
                    }
                }
            }
            return Ok(display_text);
```

Note: if `show_reasoning` goes in the top-level Config instead of PresentationConfig, use whichever task-local provides access to it.

- [ ] **Step 3: Add the formatting function**

Add a helper function in `src/agent/loop_.rs` (near the other helper functions):

```rust
/// Format reasoning content for user-visible display.
///
/// Wraps reasoning text in a visually distinct block that works across
/// Signal, Telegram, Discord, and CLI. Uses italic markers for platforms
/// that support them, with a "Reasoning:" label.
fn format_reasoning_for_display(reasoning: &str) -> String {
    let trimmed = reasoning.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    // Use a simple label + indented block.
    // Signal supports *italic*, Telegram/Discord support _italic_.
    // A plain-text approach works everywhere.
    let mut output = String::from("Reasoning:\n");
    for line in trimmed.lines() {
        output.push_str("> ");
        output.push_str(line);
        output.push('\n');
    }
    output
}
```

The `> ` blockquote prefix renders as indented on most chat platforms and as a markdown blockquote in CLI.

- [ ] **Step 4: Also stream reasoning in the final response chunks**

At line 2020-2042, the final response is streamed to the channel as `display_text` chunks. If reasoning is prepended, the streaming path will automatically include it since we modify `display_text` before this point. Verify this by checking that our insertion point is BEFORE the streaming code.

If the insertion is after streaming (lines 2020-2042), move it before:

The return is at line 2059. The streaming is at 2020-2042. So we need the reasoning prepend to happen BEFORE the streaming block at line 2020. Change the insertion point to right after line 1924 (after `display_text` is set) and before line 1926 (progress emission):

```rust
        let display_text = if parsed_text.is_empty() {
            response_text.clone()
        } else {
            parsed_text
        };

        // Prepend reasoning content if configured.
        let show_reasoning = TOOL_LOOP_PRESENTATION_CONFIG
            .try_with(|c| c.show_reasoning)
            .unwrap_or(false);
        let display_text = if show_reasoning {
            if let Some(ref reasoning) = reasoning_content {
                if !reasoning.is_empty() {
                    let formatted = format_reasoning_for_display(reasoning);
                    format!("{formatted}\n\n{display_text}")
                } else {
                    display_text
                }
            } else {
                display_text
            }
        } else {
            display_text
        };
```

This way both the streaming path (lines 2020-2042) and the direct return (line 2059) include reasoning.

- [ ] **Step 5: Run tests**

Run: `cargo test --lib loop_ -- --nocapture 2>&1 | tail -10`
Expected: All existing tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/agent/loop_.rs
git commit -m "feat(agent): display reasoning content in final response

When show_reasoning is enabled, prepends the model's reasoning/thinking
content as a blockquote above the answer text. Works with Gemma 4's
--reasoning-parser which separates reasoning into its own API field.
Reasoning appears in both streamed drafts and the finalized message."
```

---

### Task 3: Add unit tests

**Files:**
- Modify: `src/agent/loop_.rs`

- [ ] **Step 1: Add tests for the formatting function**

Add to the test module in `src/agent/loop_.rs`:

```rust
    #[test]
    fn format_reasoning_for_display_wraps_in_blockquote() {
        let reasoning = "The user wants to know about cron jobs.\nI should call cron_list.";
        let result = format_reasoning_for_display(reasoning);
        assert!(result.starts_with("Reasoning:\n"));
        assert!(result.contains("> The user wants to know about cron jobs."));
        assert!(result.contains("> I should call cron_list."));
    }

    #[test]
    fn format_reasoning_for_display_empty_returns_empty() {
        assert_eq!(format_reasoning_for_display(""), "");
        assert_eq!(format_reasoning_for_display("   \n  "), "");
    }

    #[test]
    fn format_reasoning_for_display_trims_whitespace() {
        let reasoning = "\n  Some thinking here.  \n";
        let result = format_reasoning_for_display(reasoning);
        assert!(result.starts_with("Reasoning:\n"));
        assert!(result.contains("> Some thinking here."));
    }
```

- [ ] **Step 2: Run tests**

Run: `cargo test --lib format_reasoning -- --nocapture`
Expected: 3 tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/agent/loop_.rs
git commit -m "test(agent): add unit tests for reasoning display formatting"
```

---

### Task 4: Enable in Sam's config and deploy

**Files:**
- Modify: `k8s/sam/03_zeroclaw_configmap.yaml`

- [ ] **Step 1: Add config setting**

Find the appropriate section in Sam's config.toml. If `show_reasoning` is a top-level config field (near `strip_prior_reasoning`), add it at the same level:

```toml
show_reasoning = true
```

If it ended up in `[presentation]`, add it there instead.

- [ ] **Step 2: Validate YAML**

Run: `python3 -c "import yaml; yaml.safe_load(open('k8s/sam/03_zeroclaw_configmap.yaml')); print('OK')"`
Expected: `OK`

- [ ] **Step 3: Commit**

```bash
git add k8s/sam/03_zeroclaw_configmap.yaml
git commit -m "feat(k8s/sam): enable show_reasoning for Gemma 4 thinking mode"
```

- [ ] **Step 4: Build and deploy**

Note: This requires a new image build since we changed Rust code. Either:
- Build `citizendaniel/zeroclaw-sam:v1.5.1` and update the sandbox image tag, or
- If v1.5.0 hasn't been deployed yet, rebuild it

---

## How It Will Look

Before (current — reasoning lost):
```
Sam: Here are your cron jobs: speakr-daily-summary (noon + 5pm weekdays), morning-project-status (8am weekdays).
```

After (with show_reasoning=true):
```
Reasoning:
> The user is asking about their scheduled tasks.
> I should call cron_list to get the current cron jobs.
> The results show 2 active agent crons.

Here are your cron jobs: speakr-daily-summary (noon + 5pm weekdays), morning-project-status (8am weekdays).
```

## Key Design Decisions

1. **Blockquote format** (`> `) — works across Signal, Telegram, Discord, and CLI. No platform-specific rendering needed.
2. **Prepend, not append** — reasoning comes before the answer (natural reading order: "here's how I thought about it, and here's the answer").
3. **Config-gated** — `show_reasoning: false` by default. Most users don't want to see thinking. Dan specifically wants it for observability.
4. **Insertion before streaming** — ensures both draft updates and the finalized message include reasoning.
5. **No change to history** — reasoning is already stored in history correctly. This only affects the user-facing display.
