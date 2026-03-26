# Aspect-Oriented Refactoring of ZeroClaw Crosscutting Concerns

This directory contains standalone Rust source files demonstrating how
[aspect-rs](https://github.com/yijunyu/aspect-rs-priv) patterns can remove
scattered crosscutting concerns from ZeroClaw tool implementations.

## Background

An analysis of ZeroClaw v3 (192 files, 129,040 LOC) based on the RE2026 paper
*"Aspect-Oriented Patterns for AI Agent Systems"* identified critical concern scattering:

| Concern | Files affected | Scattering score | LOC scattered |
|---------|---------------|-----------------|---------------|
| Rate limiting | 28/192 | 14.6% | ~224 LOC |
| Path validation | 65/192 | 33.9% | ~390 LOC |
| Approval/HITL | 17+ files | 5.9% centralized | ~340 LOC |
| Audit logging | 12+ files | manual 3-hook | ~60 LOC |

Each concern is currently duplicated as inline code in every tool that needs it,
creating tangling (crosscutting LOC / total LOC ≈ 42% in `shell.rs`) and
correctness risk (e.g., forgetting one of the 3 audit hooks).

## Files

### `aop_shell_tool.rs`

Refactors `src/tools/shell.rs` rate limiting + path scope + audit into 3 composable aspects:
- **RateLimitAspect** — replaces the 2 inline `is_rate_limited()` / `record_action()` calls (28 files)
- **ToolScopeAspect** — replaces `forbidden_path_argument()` per-call checks (65 files)
- **AuditAspect** — wraps `before`/`after`/`error` hooks consistently (12 files)

Before: `execute()` has ~120 LOC with ~50 LOC crosscutting.
After: `execute()` has ~8 LOC of pure business logic.

### `aop_approval_gate.rs`

Refactors `ApprovalManager` (scattered across 17+ files) into a single
`HumanApprovalAspect` with pluggable channels (Stdio, Handler closure,
AutoApprove, AutoDeny) and a session allowlist.

Mirrors `src/approval/mod.rs` behavior while centralizing it in one reusable aspect.

### `aop_audit_trail.rs`

Refactors manual `AuditLogger` calls (12 tool files × ~5 LOC each) into a
`ToolCallAuditAspect` with pluggable storage. Guarantees consistent
before+after+error coverage — eliminates the correctness risk of partial
manual instrumentation.

## Running the examples

These files use only existing ZeroClaw `Cargo.toml` dependencies
(`tokio`, `serde_json`, `parking_lot`, `async-trait`, `anyhow`, `chrono`).

To run:
```bash
# Compile and run (requires adding [[example]] entries to Cargo.toml, or run as binaries)
rustc --edition 2021 contrib/aop/aop_shell_tool.rs
```

Or copy the file to `examples/` and run:
```bash
cargo run --example aop_shell_tool
```

## Full implementation

The complete aspect-rs implementation with all 14 RE2026 patterns,
test harness, and equivalence measurements is at:
<https://github.com/yijunyu/aspect-rs-priv>

Relevant branches:
- `feat/aspect-agent` — 6 agent-specific aspects (54 tests)
- `feat/zeroclaw-examples` — working v5 examples (8 tests)
- `feat/zeroclaw-harness` — 4-measurement evaluation harness (29 tests)
