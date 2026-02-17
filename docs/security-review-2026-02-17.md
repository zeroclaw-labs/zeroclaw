# ZeroClaw Security Review — 2026-02-17

## Executive Summary

Comprehensive security review of the ZeroClaw codebase covering secrets management, injection vulnerabilities, authentication/authorization, input validation, cryptography, dependency supply chain, and error handling/logging. The codebase demonstrates strong security foundations (rustls-only TLS, zero application `unsafe`, SHA-pinned CI, defense-in-depth policy enforcement) with several actionable findings across severity tiers.

**Total findings**: 3 Critical, 1 High, 7 Medium, 7 Low

---

## CRITICAL Findings

### C1 — LLM Self-Approval of Shell Commands

**File**: `src/tools/shell.rs:49-52, 64-67, 77`

The shell tool's parameter schema includes an `approved` boolean field. The LLM can set `approved: true` when calling the tool, directly passing it to `security.validate_command_execution(command, approved)`. In `Supervised` mode, medium-risk commands require `approved == true` to execute. Since the LLM controls this parameter, it can bypass the approval requirement for any medium-risk command.

**Remediation**: Remove `approved` from the shell tool's parameter schema. Inject approval status from the approval manager at the agent loop level, outside of LLM control.

---

### C2 — Non-CLI Channels Auto-Approve All Tool Calls

**File**: `src/agent/loop_.rs:692-697`

```rust
// Only prompt interactively on CLI; auto-approve on other channels.
let decision = if channel_name == "cli" {
    mgr.prompt_cli(&request)
} else {
    ApprovalResponse::Yes
};
```

When a message arrives from any non-CLI channel (Telegram, Discord, Slack, WhatsApp, etc.), ALL tool calls are automatically approved without human review. The `Supervised` mode's human-in-the-loop guarantee only applies to CLI usage.

Additionally, `agent_turn()` (called for channel-dispatched messages) passes `approval: None` (~line 541). When `approval` is `None`, tools execute unconditionally, structurally bypassing the approval system entirely.

**Remediation**: Implement asynchronous approval for non-CLI channels (e.g., send a confirmation message to the channel user and wait for reply) rather than auto-approving all tool calls.

---

### C3 — Shell `sh -c` Execution Model

**File**: `src/runtime/native.rs:42-43`, `src/runtime/docker.rs:131-134`

All shell commands are passed through `sh -c`, making the command allowlist in `SecurityPolicy` the sole defense against injection. If an attacker can influence the command string after allowlist validation, arbitrary shell execution is possible.

**Remediation**: Consider direct `Command::new(executable).args(...)` invocation where possible, reserving `sh -c` for cases that genuinely need shell features (pipes, redirections).

---

## HIGH Findings

### H1 — Config Structs Derive Debug with Decrypted Secrets

**File**: `src/config/schema.rs` (multiple locations)

All config structs derive `Debug`, including those holding decrypted secrets after `Config::load_or_init()`:

- `Config` with `api_key: Option<String>` (line 21)
- `GatewayConfig` with `paired_tokens: Vec<String>` (line 472)
- `ComposioConfig` with `api_key: Option<String>` (line 535)
- `TelegramConfig` with `bot_token: String` (line 1319)
- `DiscordConfig` with `bot_token: String` (line 1325)
- `SlackConfig` with `bot_token: String` (line 1341)
- `WhatsAppConfig` with `access_token: String` (line 1371)
- `IrcConfig` with `server_password`, `nickserv_password`, `sasl_password` (lines 1434-1438)

A single `debug!("{:?}", config)` or `{:#?}` format would dump all secrets to logs in plaintext.

**Remediation**: Replace `#[derive(Debug)]` with custom `Debug` implementations that redact secret fields (e.g., print `[REDACTED]` for tokens/keys/passwords).

---

## MEDIUM Findings

### M1 — FTS5 Query Injection via Unescaped Double Quotes

**File**: `src/memory/sqlite.rs:242-244`

FTS5 full-text search query construction does not escape double quotes in user-provided search terms. While SQLite FTS5 injection cannot achieve SQL injection, it can cause query syntax errors or alter search semantics.

**Remediation**: Escape or strip double quotes from FTS5 query input.

---

### M2 — Browser Tool Passes LLM-Controlled Values to External CLI

**File**: `src/tools/browser.rs`

The browser tool invokes an external `agent-browser` CLI process. LLM-controlled CSS selectors and form values are passed as command-line arguments. The external process is opaque to security review.

**Remediation**: Validate/sanitize selector and value inputs before passing to the external CLI. Consider allowlisting valid selector patterns.

---

### M3 — Git Checkout Missing `--` Separator

**File**: `src/tools/git_operations.rs:372`

The `git checkout` command does not use `--` to separate branch/ref names from file paths. A maliciously crafted branch name could be interpreted as a flag or file path.

**Remediation**: Add `--` separator: `git checkout -- <branch>`.

---

### M4 — Tool Error Messages Not Scrubbed for Credentials

**File**: `src/agent/loop_.rs:724-740`

Successful tool output is scrubbed via `scrub_credentials()`, but error paths skip scrubbing entirely. OS error messages, file paths, permission errors, and anyhow error chains are passed directly to the AI model and could leak to end users via channel responses.

**Remediation**: Apply `scrub_credentials()` to error messages as well as success output.

---

### M5 — WhatsApp HMAC Verification Optional

**File**: `src/gateway/mod.rs:667`

WhatsApp HMAC-SHA256 signature verification is gated by `if let Some(ref app_secret) = whatsapp_app_secret`. If `whatsapp_app_secret` is not configured, all incoming WhatsApp webhooks are accepted without signature verification. No warning is logged.

**Remediation**: Log a warning at startup when WhatsApp webhook signature verification is disabled. Consider requiring the secret when WhatsApp channel is enabled.

---

### M6 — Audit Logs Lack Tamper Resistance

**File**: `src/security/audit.rs:177-196`

Audit logs are written as append-only JSON lines with `sync_all()` for durability, but:
- No cryptographic hash chain or HMAC signatures
- No file permission restrictions after creation
- Stored locally on the same filesystem as the agent
- The `sign_events` config field is dead code
- The `buffer` field (line 150) is never used (dead code)

An attacker with file access can modify, delete, or forge entries without detection.

**Remediation**: Implement hash chaining (each entry includes hash of previous entry) and/or HMAC signing. Set restrictive file permissions (0600) on audit log files.

---

### M7 — No JSON Schema Enforcement on Gateway Input Payloads

**File**: `src/gateway/mod.rs`

The generic webhook endpoint accepts JSON payloads with a 64KB body limit but does not enforce a schema. Unexpected fields are silently ignored, which could mask injection or abuse attempts.

**Remediation**: Add schema validation for expected webhook payload structures.

---

## LOW Findings

### L1 — IRC `verify_tls: false` Option

**File**: `src/channels/irc.rs:277-281`

The `verify_tls: false` config option disables all TLS certificate validation. While it defaults to `true`, the option exists and could be inadvertently enabled.

**Remediation**: Log a warning when TLS verification is disabled.

---

### L2 — 3 CodeQL Workflow Actions Not SHA-Pinned

**File**: `.github/workflows/codeql.yml:27, 33, 39`

```yaml
uses: github/codeql-action/init@v4
uses: dtolnay/rust-toolchain@stable
uses: github/codeql-action/analyze@v4
```

69 of 72 CI action references are SHA-pinned. These 3 use mutable tags (`@v4`, `@stable`), inconsistent with the rest of the repository.

**Remediation**: Pin to specific SHA hashes, consistent with all other workflows.

---

### L3 — `file_write` Tool Has No Content Size Limit

**File**: `src/tools/file_write.rs`

The file write tool has path traversal and symlink protections, but no cap on write size. The LLM could write arbitrarily large files.

**Remediation**: Add a configurable maximum write size (e.g., 10MB to match `file_read`).

---

### L4 — HTTP Tool Buffers Full Response Before Truncation

**File**: `src/tools/http_request.rs`

The HTTP request tool buffers the full response body into memory before applying truncation. A malicious server returning a very large response could cause memory pressure.

**Remediation**: Use streaming with an early cutoff (e.g., read only the first N bytes).

---

### L5 — DNS Rebinding Gap in SSRF Protection

**File**: `src/tools/http_request.rs`

SSRF protection validates the domain against private IP ranges at request time, but DNS resolution could return a different IP between validation and the actual connection (DNS rebinding / TOCTOU).

**Remediation**: Resolve DNS once and use the resolved IP for both validation and connection, or use a custom DNS resolver that validates at connection time.

---

### L6 — Cron Commands Not Re-Validated at Execution Time

**File**: `src/tools/cron_add.rs`

Commands are validated against the security policy at creation time but not re-validated when the cron job fires. If the security policy changes between scheduling and execution, the stale validation applies.

**Remediation**: Re-validate commands against current policy at execution time.

---

### L7 — Composio Tool URL Path Traversal Risk

**File**: `src/tools/composio.rs:139, 199`

User-provided values are interpolated into URL paths for the Composio API. If not properly encoded, path traversal sequences could alter the target endpoint.

**Remediation**: URL-encode user-provided path segments before interpolation.

---

## Positive Security Observations

These practices are commendable and should be maintained:

| Area | Details |
|------|---------|
| **TLS** | Rustls-only (zero OpenSSL), TLS 1.2+ enforced, no h2 in dependency tree |
| **Unsafe code** | Zero `unsafe` blocks in application code |
| **Supply chain** | `cargo-deny` enforces advisory checks, license compliance, and source allowlist; Cargo.lock committed; `--locked` in CI |
| **CI pinning** | 69/72 actions SHA-pinned; no git or custom registry dependencies |
| **Secret storage** | ChaCha20-Poly1305 AEAD encryption, OsRng nonce generation, 0600 key file permissions |
| **Pairing auth** | 256-bit bearer tokens, SHA-256 hashed storage, constant-time comparison, brute-force lockout |
| **Shell env** | `env_clear()` with explicit safe-variable allowlist prevents API key leakage |
| **Credential scrubbing** | `scrub_credentials()` on successful tool output; `sanitize_api_error()` on provider errors |
| **Observability** | Observer events deliberately exclude message content and prompts |
| **Gateway** | 64KB body limit, 30s timeout, rate limiting, localhost-only default bind, generic error responses to clients |
| **Path security** | Path traversal blocking, symlink escape detection, workspace-only enforcement |
| **Policy enforcement** | Multi-layer defense: autonomy levels, command allowlist, injection pattern blocking, dangerous argument detection, rate limiting |
| **Feature gating** | Consistent `default-features = false`, proper `#[cfg(feature)]` guards, CI feature matrix testing |
| **Gateway binding** | Refuses `0.0.0.0` without explicit `allow_public_bind` or tunnel configuration |

---

## Priority Remediation Roadmap

### Immediate (Critical)
1. Remove `approved` from shell tool parameter schema (`src/tools/shell.rs:49-52`)
2. Implement async approval for non-CLI channels (`src/agent/loop_.rs:692-697`)

### Short-term (High + Medium)
3. Custom `Debug` impls on secret-containing config structs (`src/config/schema.rs`)
4. Apply `scrub_credentials()` to tool error paths (`src/agent/loop_.rs:726-728`)
5. Escape FTS5 query double quotes (`src/memory/sqlite.rs:242-244`)
6. Add `--` to git checkout (`src/tools/git_operations.rs:372`)
7. SHA-pin remaining 3 CodeQL actions (`.github/workflows/codeql.yml`)

### Medium-term (Low)
8. Add content size limit to `file_write` tool
9. Stream HTTP responses with early cutoff
10. Re-validate cron commands at execution time
11. URL-encode Composio path segments
12. Log warnings for disabled TLS verification and WhatsApp HMAC

---

*Report generated by automated security review. All findings include file:line references for verification. No code changes were made during this review.*
