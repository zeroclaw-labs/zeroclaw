## Summary

- **Base branch target:** `master`
- **Problem:** When the same path is used for `shell` (absolute, e.g. `/zeroclaw-data/workspace/scripts/daily_market_news.py`) and for `file_read`/`file_write` (sometimes passed or normalized as relative `zeroclaw-data/workspace/...`), path resolution was inconsistent: `file_read` used `resolve_tool_path()` (absolute → use as-is, relative → join with workspace), while `file_write`, `file_edit`, `pdf_read`, and `image_info` used `workspace_dir.join(path)` only. That led to double-prefixed paths like `/zeroclaw-data/workspace/zeroclaw-data/workspace/scripts/...` when a logically absolute path was treated as relative.
- **Why it matters:** Paths for file_read/write/shell must remain identical; avoid inconsistent resolution between tools so that absolute paths under workspace are not re-prefixed with workspace_dir.
- **What changed:** All file tools now use `SecurityPolicy::resolve_tool_path(path)` for resolving the path: `file_write`, `file_edit`, `pdf_read`, and `image_info` now call `resolve_tool_path` instead of `workspace_dir.join(path)`. Added test `file_write_absolute_path_under_workspace_no_double_prefix` to guard against regression. Removed unused `Path` import in `image_info.rs`.
- **What did not change (scope boundary):** No changes to security policy logic, gateway, or agent parsing; only tool-side path resolution unified.

## Label Snapshot (required)

- Risk label: `risk: medium`
- Size label: auto-managed
- Scope labels: `tool`, `security`
- Module labels: `tool: shell`, `tool: file_read`, `tool: file_write`
- Contributor tier label: auto-managed
- If any auto-label is incorrect, note requested correction: —

## Change Metadata

- Change type: `bug`
- Primary scope: `tool`

## Linked Issue

- Closes #3774
- Related: —
- Depends on: —
- Supersedes: —

## Supersede Attribution (required when `Supersedes #` is used)

- N/A

## Validation Evidence (required)

- `cargo fmt --all -- --check`: pass
- `cargo clippy --all-targets -- -D warnings`: pass
- `cargo test`: pass (including `file_write`, `file_read`, `file_edit`, `pdf_read`, `image_info`, and `security::policy` tests)
- Evidence: Local build and tests; new test ensures absolute path under workspace does not double-prefix.
- If any command is intentionally skipped: None.

## Security Impact (required)

- New permissions/capabilities? **No**
- New external network calls? **No**
- Secrets/tokens handling changed? **No**
- File system access scope changed? **No** (same allowed paths; resolution is now consistent, not expanded).

## Privacy and Data Hygiene (required)

- Data-hygiene status: **pass**
- Redaction/anonymization notes: None.
- Neutral wording confirmation: No user-facing wording changes.

## Compatibility / Migration

- Backward compatible? **Yes**
- Config/env changes? **No**
- Migration needed? **No**
- If yes, exact upgrade steps: —

## i18n Follow-Through (required when docs or user-facing wording changes)

- i18n follow-through triggered? **No**

## Human Verification (required)

- Verified scenarios: file_write/file_edit/pdf_read/image_info with relative path; file_write with absolute path under workspace (new test).
- Edge cases checked: absolute path outside workspace still rejected; relative paths still resolve under workspace.
- What was not verified: Full E2E with gateway Docker.

## Side Effects / Blast Radius (required)

- Affected subsystems/workflows: file_write, file_edit, pdf_read, image_info tools only.
- Potential unintended effects: None expected; resolution logic is already used by file_read and security policy.
- Guardrails/monitoring for early detection: New unit test.

## Agent Collaboration Notes (recommended)

- Agent tools used: codebase search, grep, read_file, edit.
- Workflow/plan summary: Identified resolve_tool_path vs workspace_dir.join inconsistency; unified to resolve_tool_path; added test.
- Confirmation: naming + architecture boundaries followed (AGENTS.md + CONTRIBUTING.md).

## Rollback Plan (required)

- Fast rollback command/path: Revert PR; rebuild.
- Feature flags or config toggles: None.
- Observable failure symptoms: If regression, file operations might fail for previously working relative paths (unlikely; same join behavior for relative).

## Risks and Mitigations

- Risk: Slight behavior change if any caller relied on join() semantics for a path that looked absolute but was not under workspace (such paths were already rejected by is_path_allowed).
- Mitigation: is_path_allowed and resolve_tool_path are unchanged; only call sites unified.
