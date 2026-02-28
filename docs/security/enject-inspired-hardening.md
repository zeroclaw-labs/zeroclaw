# Enject-Inspired Hardening Notes

Date: 2026-02-28

## Scope

This document records a focused security review of `GreatScott/enject` and maps the useful controls to ZeroClaw runtime/tooling.

The goal is not feature parity with `enject` (a dedicated secret-injection CLI), but to import practical guardrail patterns for agent safety and operational reliability.

## Key Enject Security Patterns

From `enject` architecture and source review:

1. Secrets should not be plaintext in project files.
2. Runtime should fail closed on unresolved secret references.
3. Secret entry should avoid shell history and process-argument exposure.
4. Sensitive material should be zeroized or lifetime-minimized in memory.
5. Encryption/writes should be authenticated and atomic.
6. Tooling should avoid convenience features that become exfiltration channels (for example, no `get`/`export`).

## Applied to ZeroClaw

### 1) Sensitive file access policy was centralized

Implemented in:

- `src/security/sensitive_paths.rs`
- `src/tools/file_read.rs`
- `src/tools/file_write.rs`
- `src/tools/file_edit.rs`

Added shared sensitive-path detection for:

- exact names (`.env`, `.envrc`, `.git-credentials`, key filenames)
- suffixes (`.pem`, `.key`, `.p12`, `.pfx`, `.ovpn`, `.kubeconfig`, `.netrc`)
- sensitive path components (`.ssh`, `.aws`, `.gnupg`, `.kube`, `.docker`, `.azure`, `.secrets`)

Rationale: a single classifier avoids drift between tools and keeps enforcement consistent as more tools are hardened.

### 2) Sensitive file reads are blocked by default in `file_read`

Implemented in `src/tools/file_read.rs`:

- Enforced block both:
  - before canonicalization (input path)
  - after canonicalization (resolved path, including symlink targets)
- Added explicit opt-in gate:
  - `autonomy.allow_sensitive_file_reads = true`

Rationale: This mirrors `enject`'s "plaintext secret files are high-risk by default" stance while preserving operator override for controlled break-glass scenarios.

### 3) Sensitive file writes/edits are blocked by default in `file_write` + `file_edit`

Implemented in:

- `src/tools/file_write.rs`
- `src/tools/file_edit.rs`

Enforced block both:

- before canonicalization (input path)
- after canonicalization (resolved path, including symlink targets)

Added explicit opt-in gate:

- `autonomy.allow_sensitive_file_writes = true`

Rationale: unlike read-only exposure, write/edit to secret-bearing files can silently corrupt credentials, rotate values unintentionally, or create exfiltration artifacts in VCS/workspace state.

### 4) Hard-link escape guard for file tools

Implemented in:

- `src/security/file_link_guard.rs`
- `src/tools/file_read.rs`
- `src/tools/file_write.rs`
- `src/tools/file_edit.rs`

Behavior:

- All three file tools refuse existing files with link-count > 1.
- This blocks a class of path-based bypasses where a workspace file name is hard-linked to external sensitive content.

Rationale: canonicalization and symlink checks do not reveal hard-link provenance; link-count guard is a conservative fail-closed protection with low operational impact.

### 5) Config-level gates for sensitive reads/writes

Implemented in:

- `src/config/schema.rs`
- `src/security/policy.rs`
- `docs/config-reference.md`

Added:

- `autonomy.allow_sensitive_file_reads` (default: `false`)
- `autonomy.allow_sensitive_file_writes` (default: `false`)

Both are mapped into runtime `SecurityPolicy`.

### 6) Pushover credential ingestion hardening

Implemented in `src/tools/pushover.rs`:

- Environment-first credential source (`PUSHOVER_TOKEN`, `PUSHOVER_USER_KEY`)
- `.env` fallback retained for compatibility
- Hard error when only one env variable is set (partial state)
- Hard error when `.env` values are unresolved `en://` / `ev://` references
- Test env mutation isolation via `EnvGuard` + global lock

Rationale: This aligns with `enject`'s fail-closed treatment of unresolved secret references and reduces accidental plaintext handling ambiguity.

### 7) Non-CLI approval session grant now actually bypasses prompt

Implemented in `src/agent/loop_.rs`:

- `run_tool_call_loop` now honors `ApprovalManager::is_non_cli_session_granted(tool)`.
- Added runtime trace event: `approval_bypass_non_cli_session_grant`.
- Added regression test:
  - `run_tool_call_loop_uses_non_cli_session_grant_without_waiting_for_prompt`

Rationale: This fixes a reliability/safety gap where already-approved non-CLI tools could still stall on pending approval waits.

### 8) Outbound leak guard strict mode + config parity across delivery paths

Implemented in:

- `src/config/schema.rs`
- `src/channels/mod.rs`
- `src/gateway/mod.rs`
- `src/gateway/ws.rs`
- `src/gateway/openai_compat.rs`

Added outbound leak policy:

- `security.outbound_leak_guard.enabled` (default: `true`)
- `security.outbound_leak_guard.action` (`redact` or `block`, default: `redact`)
- `security.outbound_leak_guard.sensitivity` (`0.0..=1.0`, default: `0.7`)

Behavior:

- `redact`: preserve current behavior, redact detected credential material and deliver response.
- `block`: suppress original response when leak detector matches and return safe fallback text.
- Gateway and WebSocket now read runtime config for this policy rather than hard-coded defaults.
- OpenAI-compatible `/v1/chat/completions` path now uses the same leak guard for both non-streaming and streaming responses.
- For streaming, when guard is enabled, output is buffered and sanitized before SSE emission so raw deltas are not leaked pre-scan.

Rationale: this closes a consistency gap where strict outbound controls could be applied in channels but silently downgraded at gateway/ws boundaries.

## Validation Evidence

Targeted and full-library tests passed after hardening:

- `tools::file_write::tests::file_write_blocks_sensitive_file_by_default`
- `tools::file_write::tests::file_write_allows_sensitive_file_when_configured`
- `tools::file_edit::tests::file_edit_blocks_sensitive_file_by_default`
- `tools::file_edit::tests::file_edit_allows_sensitive_file_when_configured`
- `tools::file_read::tests::file_read_blocks_hardlink_escape`
- `tools::file_write::tests::file_write_blocks_hardlink_target_file`
- `tools::file_edit::tests::file_edit_blocks_hardlink_target_file`
- `channels::tests::process_channel_message_executes_tool_calls_instead_of_sending_raw_json`
- `channels::tests::process_channel_message_telegram_does_not_persist_tool_summary_prefix`
- `channels::tests::process_channel_message_streaming_hides_internal_progress_by_default`
- `channels::tests::process_channel_message_streaming_shows_internal_progress_on_explicit_request`
- `channels::tests::process_channel_message_executes_tool_calls_with_alias_tags`
- `channels::tests::process_channel_message_respects_configured_max_tool_iterations_above_default`
- `channels::tests::process_channel_message_reports_configured_max_tool_iterations_limit`
- `agent::loop_::tests::run_tool_call_loop_uses_non_cli_session_grant_without_waiting_for_prompt`
- `channels::tests::sanitize_channel_response_blocks_detected_credentials_when_configured`
- `gateway::mod::tests::sanitize_gateway_response_blocks_detected_credentials_when_configured`
- `gateway::ws::tests::sanitize_ws_response_blocks_detected_credentials_when_configured`
- `cargo test -q --lib` => passed (`3760 passed; 0 failed; 4 ignored`)

## Residual Risks and Next Hardening Steps

1. Runtime exfiltration remains possible if a model is induced to print secrets from tool output.
2. Secrets in child-process environment remain readable to processes with equivalent host privileges.
3. Some tool paths outside `file_read` may still accept high-sensitivity material without uniform policy checks.

Recommended follow-up work:

1. Centralize a shared `SensitiveInputPolicy` used by all secret-adjacent tools (not just `file_read`).
2. Introduce a typed secret wrapper for tool credential flows to reduce `String` lifetime and accidental logging.
3. Extend leak-guard policy parity checks to any future outbound surfaces beyond channel/gateway/ws.
4. Add e2e tests covering "unresolved secret reference" behavior across all credential-consuming tools.
