## 🐛 BUG-001: agent_loop_robustness fails to build [SEVERITY: High]

**Status**: 🐛 Open
**Discovered**: 2026-03-07 during Agent Stability investigation
**Impact**: Blocks validation of agent loop stability fixes.

**Reproduction**:
1. Run `bazel test //tests:agent_loop_robustness`
2. Observer compiler errors: `unresolved import async_trait` and lifetime mismatches.

**Root Cause**:
1. `tests/BUILD.bazel` is missing `@crates//:async-trait` in `proc_macro_deps` for the `agent_loop_robustness` target.
2. Lifetime parameters on trait methods in `MockProvider` and other mocks in the test file do not match the current `Provider` and `Tool` trait definitions exactly.

**Files Affected** (2 files):
- `tests/BUILD.bazel` - needs dependency update
- `tests/agent_loop_robustness.rs` - needs mock method signature updates

**Fix Approach**:
1. Update `tests/BUILD.bazel` to include `async-trait`.
2. Align method signatures in `tests/agent_loop_robustness.rs` with `src/providers/traits.rs` and `src/tools/traits.rs`.

**Verification**:
Run `bazel test //tests:agent_loop_robustness` and ensure it builds and passes.

**Related Tasks**: [Agent Stability and Persistence](docs/tasks/agent-stability-persistence.md)
