# Agent Tool Output Presentation Layer & Consistency Enforcement

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a centralized presentation layer to zeroclaw that processes all tool output before the LLM sees it (ANSI stripping, overflow with file-save and exploration hints, duration/exit-code footer), then enforce consistency in agent documentation across both Sam and Goose.

**Architecture:** A new `presentation` module in the zeroclaw agent loop sits between tool execution/hooks and LLM-facing result formatting. It applies three transformations in order: (1) strip ANSI escape codes, (2) truncate at 200 lines / 50KB with full output saved to a temp file and exploration hints appended, (3) append metadata footer — `[exit:N | Xms]` for shell-like tools, `[ok | Xms]` / `[err | Xms]` for all others. Thresholds are configurable via `config.toml`. Separately, Sam and Goose ConfigMaps are updated to fix documentation gaps identified in the consistency audit.

**Tech Stack:** Rust (zeroclaw runtime), `strip-ansi-escapes` crate, YAML/TOML (ConfigMaps and config)

**Repos:**
- Runtime changes: `~/github_projects/zeroclaw/`
- Config/doc changes: `~/github_projects/scrapyard-applications/`

**Origin:** Comparison analysis of our Sam/Goose agent configurations against tool design principles from an ex-Manus backend lead's posts on r/LocalLLaMA (reference material at `/tmp/morrohsu`, indexed in memory as `agent-tool-output-reference.md`).

---

## Chunk 1: Centralized Presentation Layer (zeroclaw runtime)

All file paths in this chunk are relative to `~/github_projects/zeroclaw/`.

**Status: COMPLETE** — Committed as `f98326ea`, included in image `citizendaniel/zeroclaw-sam:v1.4.10`.

### Task 1: Add dependency and create module skeleton

**Files:**
- Modify: `Cargo.toml`
- Create: `src/agent/presentation.rs`
- Modify: `src/agent/mod.rs`

- [x] **Step 1: Add strip-ansi-escapes to Cargo.toml**

Added under `[dependencies]`:
```toml
strip-ansi-escapes = "0.2"
```

- [x] **Step 2: Create presentation module**
- [x] **Step 3: Register module in `src/agent/mod.rs`**

Added `pub mod presentation;` to module declarations.

- [x] **Step 4: Verify it compiles**

---

### Task 2: Implement ANSI stripping with tests

**Files:**
- Modify: `src/agent/presentation.rs`

- [x] **Step 1-4: TDD cycle for ANSI stripping**

Implementation uses `strip_ansi_escapes::strip_str()` (not `strip()` + `from_utf8_lossy` — cleaner API).

```rust
pub fn strip_ansi(s: &str) -> String {
    strip_ansi_escapes::strip_str(s).to_string()
}
```

5 tests: color codes, cursor movement, plain text passthrough, empty string, nested codes.

---

### Task 3: Implement overflow mode with tests

**Files:**
- Modify: `src/agent/presentation.rs`

- [x] **Step 1-4: TDD cycle for overflow handling**

Key implementation detail — after line-based truncation, also enforces byte limit for cases where a few long lines still exceed `max_output_bytes`:

```rust
// Safety: if a few long lines still exceed the byte limit, byte-truncate
if truncated.len() > config.max_output_bytes {
    let mut cutoff = config.max_output_bytes;
    while cutoff > 0 && !truncated.is_char_boundary(cutoff) {
        cutoff -= 1;
    }
    truncated.truncate(cutoff);
}
```

Overflow format:
```
[first 200 lines]

--- output truncated (5000 lines, 198.5KB) ---
Full output: /tmp/cmd-output/cmd-3.txt
Explore: cat /tmp/cmd-output/cmd-3.txt | grep <pattern>
         cat /tmp/cmd-output/cmd-3.txt | tail 100
```

4 tests: short output unchanged, line limit, byte limit, file save verification.

---

### Task 4: Implement metadata footer with tests

**Files:**
- Modify: `src/agent/presentation.rs`

- [x] **Step 1-4: TDD cycle for metadata footer**

Shell-like tools (`shell`, `bg_run`, `bg_status`, `process`) use `[exit:N | Xms]` format. All other tools use `[ok | Xms]` / `[err | Xms]` — "exit code" is semantically wrong for `file_read` or `memory_recall`.

```rust
const SHELL_LIKE_TOOLS: &[&str] = &["shell", "bg_run", "bg_status", "process"];
```

The `present_for_llm` function takes `tool_name: &str` to determine which format to use.

6 tests: duration formatting (ms/s boundary), shell exit codes, non-shell ok/err, disabled metadata, full pipeline integration.

---

### Task 5: Add PresentationConfig to config schema

**Files:**
- Modify: `src/config/schema.rs`
- Modify: `src/onboard/wizard.rs` (3 `Config` construction sites needed the new field)

- [x] **Step 1: Add PresentationConfig struct**

**Critical: Must derive `JsonSchema`** — all config structs in `schema.rs` derive it. Missing this causes compilation failure because the top-level `Config` struct derives `JsonSchema`.

```rust
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PresentationConfig { ... }
```

Defaults: `max_output_lines: 200`, `max_output_bytes: 51_200`, `strip_ansi: true`, `show_metadata: true`, `overflow_dir: "/tmp/cmd-output"`.

- [x] **Step 2: Add field to Config struct**
- [x] **Step 3: Replace local type with re-export from schema**
- [x] **Step 4: Fix all Config construction sites** (schema.rs Default impl, wizard.rs x2)

---

### Task 6: Integrate presentation layer into agent loop

**Files:**
- Modify: `src/agent/loop_.rs`

**CRITICAL DESIGN DECISIONS:**

1. **Insertion point: AFTER loop detection, BEFORE ordered_results.** The presentation call goes after `loop_detector.record_call()` (line ~2404) and before `ordered_results[*idx] = Some(...)` (line ~2407). Placing it before loop detection would cause false-negative loop detection because the duration metadata footer would make identical outputs appear different (e.g., `42ms` vs `43ms`).

2. **Config threading via task-local variable.** `run_tool_call_loop` does NOT receive a `Config` parameter. The codebase uses `tokio::task_local!` scoped variables. Added `TOOL_LOOP_PRESENTATION_CONFIG` to the existing `task_local!` block, scoped from `run()` at both call sites. Read inside the loop with `try_with()` + `unwrap_or_default()` fallback.

- [x] **Step 1: Add task-local declaration**

```rust
tokio::task_local! {
    // ... existing task-locals ...
    static TOOL_LOOP_PRESENTATION_CONFIG: super::presentation::PresentationConfig;
}
```

- [x] **Step 2: Scope from run() at both call sites**

Added `TOOL_LOOP_PRESENTATION_CONFIG.scope(config.presentation.clone(), ...)` wrapping `run_tool_call_loop` at both scope chains in `run()`.

- [x] **Step 3: Insert presentation call in tool processing loop**

```rust
// ── Presentation: prepare output for LLM ──
{
    let pres_cfg = TOOL_LOOP_PRESENTATION_CONFIG
        .try_with(|c| c.clone())
        .unwrap_or_default();
    outcome.output = super::presentation::present_for_llm(
        &outcome.output,
        &call.name,
        outcome.success,
        outcome.duration,
        &pres_cfg,
    );
}
```

- [x] **Step 4-6: Compile, test, commit**

All 16 presentation tests pass. All 15 agent e2e tests pass (0 regressions).

---

### Task 7: Build, test full suite, tag

- [x] **Full test suite passes**
- [x] **Release build succeeds**
- [x] **Committed as `f98326ea`**
- [x] **Included in `citizendaniel/zeroclaw-sam:v1.4.10`**

---

## Chunk 2: Consistency Enforcement (scrapyard-applications)

All file paths in this chunk are relative to `~/github_projects/scrapyard-applications/`.

**Status: COMPLETE** — Committed across 3 commits. ConfigMaps **not yet re-applied** to cluster.

### Task 8: Fix Sam's TOOLS.md — deduplication and navigation

**Files:**
- Modify: `k8s/sam/05_zeroclaw_identity_configmap.yaml`

- [x] **Step 1: Remove excluded tools from quick reference table**

Removed Model/Provider row, Config row. Removed `schedule` from Scheduling row. Removed `manage_outbound_queue`, `channel_ack_config` from Messaging row.

- [x] **Step 2: Mark browser_open as subset and screenshot as non-functional**
- [x] **Step 3: Add "When to Use What" section** — file editing (file_edit vs apply_patch vs Serena), running commands (shell vs git_operations), background execution (shell vs bg_run)
- [x] **Step 4: Add "Which Memory System?" section** — native SQLite (session-local) vs Serena (cross-session persistent)
- [x] **Step 5: Move cron_run infinite-loop warning from skill to TOOLS.md**
- [x] **Step 6: Add desire path propagation rule to Key Principles**

> 6. **Propagate tool pitfalls.** When you discover a tool behavior that would prevent future mistakes — wrong tool choice, dangerous operation, performance trap — flag it for inclusion in this TOOLS.md file. Don't bury it in a skill or session note where future sessions won't find it.

- [x] **Step 7: Commit** — `f06ca2d`

---

### Task 9: Add context discipline and output handling guidance

**Files:**
- Modify: `k8s/sam/05_zeroclaw_identity_configmap.yaml`

- [x] **Step 1: Add "Output Size Discipline" section** — explains overflow mode, proactive measures (--tail=100, offset/limit, specific patterns, disk-first chunking)
- [x] **Step 2: Add "Reading Tool Output Metadata" section** — explains `[exit:0 | 42ms]` format and how to use duration for behavior calibration
- [x] **Step 3: Commit** — included in `f06ca2d`

---

### Task 10: Fix Goose's TOOLS.md and AGENTS.md

**Files:**
- Modify: `k8s/goose/04_zeroclaw_k8s_agent/02_identity_configmap.yaml`

- [x] **Step 1: Add "Output Size Discipline" with kubectl specifics** (--tail=100, jsonpath, --no-headers)
- [x] **Step 2: Add "When kubectl Fails"** — error patterns (already exists, forbidden, conflict, Pending, CrashLoopBackOff) with remediation
- [x] **Step 3: Add memory routing** to Serena section
- [x] **Step 4: Add production namespace escalation path** to AGENTS.md Scope & Boundaries
- [x] **Step 5: Add desire path propagation rule** — kubectl/RBAC/infrastructure variant
- [x] **Step 6: Commit** — `8a95d15`

---

### Task 11: Update Sam's non_cli_excluded_tools

- [x] **Step 1: Verified** — all 12 tools removed from TOOLS.md are already in the exclusion list. No changes needed.

---

### Task 12: Add presentation config to agent config.toml files

**Files:**
- Modify: `k8s/sam/03_zeroclaw_configmap.yaml`
- Modify: `k8s/goose/04_zeroclaw_k8s_agent/01_configmap.yaml`

- [x] **Step 1-2: Add `[presentation]` section to both configs**

```toml
[presentation]
max_output_lines = 200
max_output_bytes = 51200
strip_ansi = true
show_metadata = true
overflow_dir = "/tmp/cmd-output"
```

- [x] **Step 3: Commit** — `7000a15`

---

### Task 13: Document KV cache awareness principle

**Files:**
- Modify: `k8s/sam/05_zeroclaw_identity_configmap.yaml`

- [x] **Step 1: Add KV cache note to AGENTS.md**

> Tool definitions are static — they never change mid-conversation. This preserves the KV cache prefix across the entire session, avoiding expensive recomputation. If we ever add dynamic tool loading, tool changes must be batched at conversation boundaries, never mid-turn.

- [x] **Step 2: Commit** — included in `f06ca2d`

---

## Deployment Status

| Step | Status |
|---|---|
| Build and push zeroclaw image (v1.4.10) | Done |
| Update image tag in sandbox YAML | Done |
| Apply Sam's ConfigMap (config + identity) | **Pending** |
| Apply Goose's ConfigMap (config + identity) | **Pending** |
| Restart pods | Sam restarted (3h ago), but with old ConfigMaps |
| Verify presentation layer active | Binary active with defaults; docs not yet live |

**To complete deployment:**
```bash
kubectl apply -f k8s/sam/03_zeroclaw_configmap.yaml
kubectl apply -f k8s/sam/05_zeroclaw_identity_configmap.yaml
kubectl apply -f k8s/goose/04_zeroclaw_k8s_agent/01_configmap.yaml
kubectl apply -f k8s/goose/04_zeroclaw_k8s_agent/02_identity_configmap.yaml
# Pod restart needed for identity docs (read at session start)
```

Note: The `[presentation]` config values match the compiled defaults, so the binary is already operating correctly. The ConfigMap apply makes it explicit and allows future tuning without rebuilding. The identity ConfigMap apply is required for the TOOLS.md/AGENTS.md documentation improvements to take effect.

---

## Design Notes

**Two-layer truncation:** Shell tool truncation at 1MB (`MAX_OUTPUT_BYTES` in `shell.rs`) and the presentation layer truncation at 50KB serve different purposes. Shell's 1MB limit is an OOM protection that runs during tool execution. The presentation layer's 50KB limit is a context quality measure that runs after all hooks. Both coexist — a 2MB output would be shell-truncated to 1MB, then presentation-truncated to 50KB. The shell's truncation message may be lost in the second truncation, which is acceptable. In a future cleanup, consider lowering `MAX_OUTPUT_BYTES` or updating its message to match the presentation layer's format.

**Overflow file cleanup:** Overflow files accumulate in `/tmp/cmd-output/` across a session. For container-based deployments (Sandbox CRDs), pod restarts clean `/tmp` automatically. For long-running sessions, consider adding a bounded ring buffer (e.g., keep last 50 files) or session-end cleanup in a future iteration.

**Metadata footer on non-shell tools:** Shell-like tools (`shell`, `bg_run`, `bg_status`, `process`) use `[exit:0 | Xms]` format, matching Unix conventions familiar from LLM training data. All other tools use `[ok | Xms]` / `[err | Xms]` since "exit code" is semantically meaningless for `file_read` or `memory_recall`.

**Loop detection ordering:** The presentation layer runs AFTER `loop_detector.record_call()` to prevent duration jitter in the metadata footer from causing false-negative loop detection. The loop detector sees raw output; the LLM sees presented output.

**Config threading:** Uses `tokio::task_local!` (matching existing patterns like `TOOL_LOOP_STRIP_PRIOR_REASONING`) rather than adding a parameter to `run_tool_call_loop`, which already has too many arguments. Falls back to `PresentationConfig::default()` if the task-local isn't scoped (e.g., in wrapper functions that don't have access to `Config`).
