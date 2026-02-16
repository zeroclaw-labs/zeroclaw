# ZeroClaw Comprehensive Code Review

**Date:** 2026-02-14
**Scope:** Full security-focused review of ZeroClaw repository (~25K LoC Rust)
**Reviewer:** Claude Code (automated)
**Branch:** `main` (commit `3656928`)

---

## Table of Contents

- [Executive Summary](#executive-summary)
- [Critical Security Issues (1-7)](#critical-security-issues)
- [Medium Security Issues (8-11)](#medium-security-issues)
- [Code Quality Issues (12-17)](#code-quality-issues)
- [Testing Gaps (18-21)](#testing-gaps)
- [CI/CD & Infrastructure (22-25)](#cicd--infrastructure)
- [Dependency & Architecture Notes (26-28)](#dependency--architecture-notes)
- [Open PR Coverage Matrix](#open-pr-coverage-matrix)
- [Recommended Priority Order](#recommended-priority-order)

---

## Executive Summary

This review identified **28 findings** across security, code quality, testing, and infrastructure categories. Four existing PRs (#36, #38, #39, #40) already address some issues. The remaining findings require **new PRs**, each addressing a single issue for clean review.

**Severity breakdown:**
- HIGH: 2
- MEDIUM: 9
- LOW/INFO: 17

---

## Critical Security Issues

---

### Finding 1: SQL Injection Pattern in iMessage Channel

| Field | Value |
|-------|-------|
| **Severity** | HIGH |
| **CWE** | [CWE-89: SQL Injection](https://cwe.mitre.org/data/definitions/89.html) |
| **File** | `src/channels/imessage.rs:220-237` |
| **Addressed by PR** | None |

#### Description

The `fetch_new_messages` function constructs a SQL query by directly interpolating the `since_rowid` parameter via `format!()`, then passes it to the `sqlite3` CLI:

```rust
// src/channels/imessage.rs:220-229
let query = format!(
    "SELECT m.ROWID, h.id, m.text \
     FROM message m \
     JOIN handle h ON m.handle_id = h.ROWID \
     WHERE m.ROWID > {since_rowid} \
     AND m.is_from_me = 0 \
     AND m.text IS NOT NULL \
     ORDER BY m.ROWID ASC \
     LIMIT 20;"
);
```

The query is then executed by spawning `sqlite3` as a subprocess:

```rust
// src/channels/imessage.rs:231-237
let output = tokio::process::Command::new("sqlite3")
    .arg("-separator")
    .arg("|")
    .arg(db_path)
    .arg(&query)
    .output()
    .await?;
```

**Current risk:** `since_rowid` is `i64`, so it is not string-injectable today. Rust's type system prevents non-integer values from being passed. However, this pattern is fragile:
- If the function signature is ever changed to accept `&str` for flexibility, it becomes a classic SQL injection.
- The `format!()` pattern for SQL queries is a well-known anti-pattern that code scanners will flag.
- The project already depends on `rusqlite` (line 50 of `Cargo.toml`), making parameterized queries trivial.

#### Remediation

Replace the `sqlite3` subprocess with `rusqlite` parameterized queries. The crate is already a dependency (`Cargo.toml:50`):

```rust
use rusqlite::Connection;

async fn fetch_new_messages(
    db_path: &std::path::Path,
    since_rowid: i64,
) -> anyhow::Result<Vec<(i64, String, String)>> {
    let path = db_path.to_path_buf();
    let results = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let conn = Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let mut stmt = conn.prepare(
            "SELECT m.ROWID, h.id, m.text \
             FROM message m \
             JOIN handle h ON m.handle_id = h.ROWID \
             WHERE m.ROWID > ?1 \
             AND m.is_from_me = 0 \
             AND m.text IS NOT NULL \
             ORDER BY m.ROWID ASC \
             LIMIT 20"
        )?;
        let rows = stmt.query_map([since_rowid], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }).await??;
    Ok(results)
}
```

This also resolves Finding #3 (subprocess security bypass).

---

### Finding 2: WhatsApp Webhook Signature Not Verified

| Field | Value |
|-------|-------|
| **Severity** | HIGH |
| **CWE** | [CWE-345: Insufficient Verification of Data Authenticity](https://cwe.mitre.org/data/definitions/345.html) |
| **File** | `src/gateway/mod.rs:347-416` |
| **Addressed by PR** | None |

#### Description

Meta sends an `X-Hub-Signature-256` header (HMAC-SHA256) on every webhook POST to prove the request originated from Meta's servers. The current `handle_whatsapp_message` handler does **not** verify this signature:

```rust
// src/gateway/mod.rs:347-416
async fn handle_whatsapp_message(State(state): State<AppState>, body: Bytes) -> impl IntoResponse {
    let Some(ref wa) = state.whatsapp else {
        return (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "WhatsApp not configured"})));
    };

    // Parse JSON body -- NO signature verification before this point
    let Ok(payload) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": "Invalid JSON payload"})));
    };

    // Messages are immediately processed and responded to
    let messages = wa.parse_webhook_payload(&payload);
    // ... processes messages, calls LLM, sends replies ...
}
```

**Impact:** Anyone who discovers the webhook URL (e.g., through DNS enumeration, log leaks, or brute force) can send spoofed messages that the bot will process and respond to via the LLM. This could:
- Cause the bot to send unwanted messages to real WhatsApp users
- Exhaust LLM API credits
- Extract information through prompt injection

#### Remediation

Add HMAC-SHA256 signature verification before processing:

```rust
use hmac::{Hmac, Mac};
use sha2::Sha256;

fn verify_whatsapp_signature(app_secret: &str, body: &[u8], signature_header: &str) -> bool {
    // Header format: "sha256=<hex>"
    let Some(hex_sig) = signature_header.strip_prefix("sha256=") else {
        return false;
    };
    let Ok(expected) = hex::decode(hex_sig) else {
        return false;
    };
    let mut mac = Hmac::<Sha256>::new_from_slice(app_secret.as_bytes())
        .expect("HMAC accepts any key length");
    mac.update(body);
    mac.verify_slice(&expected).is_ok()
}
```

Then in the handler, extract the header and verify before parsing:

```rust
async fn handle_whatsapp_message(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    // Verify signature FIRST
    if let Some(ref app_secret) = state.whatsapp_app_secret {
        let sig = headers.get("X-Hub-Signature-256")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !verify_whatsapp_signature(app_secret, &body, sig) {
            return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error": "Invalid signature"})));
        }
    }
    // ... rest of handler
}
```

**Note:** This requires adding `hmac` and `sha2` crates to `Cargo.toml`, or using a lightweight alternative.

---

### Finding 3: iMessage Channel Bypasses Security Policy via External Process

| Field | Value |
|-------|-------|
| **Severity** | MEDIUM |
| **CWE** | [CWE-78: OS Command Injection](https://cwe.mitre.org/data/definitions/78.html) |
| **File** | `src/channels/imessage.rs:231-237` |
| **Addressed by PR** | None |

#### Description

The `sqlite3` CLI is spawned directly via `tokio::process::Command` without going through the `ShellTool`/`SecurityPolicy` command allowlist:

```rust
// src/channels/imessage.rs:231-237
let output = tokio::process::Command::new("sqlite3")
    .arg("-separator")
    .arg("|")
    .arg(db_path)
    .arg(&query)
    .output()
    .await?;
```

**Issues:**
1. **No security policy check:** The `ShellTool` validates commands against an allowlist before execution. This subprocess bypasses that entirely.
2. **Full environment inheritance:** The subprocess inherits the parent process's environment, including `API_KEY`, `ZEROCLAW_API_KEY`, and any other secrets in the environment.
3. **PATH manipulation:** An attacker who can modify `PATH` (e.g., through a `.env` file or compromised shell profile) could redirect `sqlite3` to a malicious binary.

#### Remediation

**Preferred:** Replace with `rusqlite` (see Finding #1 remediation). This eliminates the subprocess entirely.

**If subprocess must be kept:**
```rust
let output = tokio::process::Command::new("/usr/bin/sqlite3")  // Absolute path
    .arg("-separator")
    .arg("|")
    .arg(db_path)
    .arg(&query)
    .env_clear()  // Clear all inherited env vars
    .env("PATH", "/usr/bin:/bin")  // Minimal PATH
    .output()
    .await?;
```

---

### Finding 4: Shell Tool Leaks Environment Variables

| Field | Value |
|-------|-------|
| **Severity** | MEDIUM |
| **CWE** | [CWE-200: Exposure of Sensitive Information](https://cwe.mitre.org/data/definitions/200.html) |
| **File** | `src/tools/shell.rs:63-70` |
| **Addressed by PR** | None |

#### Description

Shell commands executed by the `ShellTool` inherit the full parent process environment:

```rust
// src/tools/shell.rs:63-70
let result = tokio::time::timeout(
    Duration::from_secs(SHELL_TIMEOUT_SECS),
    tokio::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(&self.security.workspace_dir)
        .output(),  // No .env_clear() -- full env inherited
)
.await;
```

**Impact:** If an LLM-driven agent loop executes commands like `env`, `printenv`, `echo $API_KEY`, or `cat /proc/self/environ`, the full set of environment variables (including `API_KEY`, `ZEROCLAW_API_KEY`, and any other secrets) is exposed in the command output. This output is then fed back to the LLM and potentially logged.

Even with the command allowlist in `SecurityPolicy`, an allowed command like `python3` could print environment variables via `import os; print(os.environ)`.

#### Remediation

Add `.env_clear()` to the Command builder, then selectively pass only safe variables:

```rust
const SAFE_ENV_VARS: &[&str] = &["PATH", "HOME", "TERM", "LANG", "USER", "SHELL", "TMPDIR"];

// In execute():
let mut cmd = tokio::process::Command::new("sh");
cmd.arg("-c")
   .arg(command)
   .current_dir(&self.security.workspace_dir)
   .env_clear();

// Re-add only safe vars
for var in SAFE_ENV_VARS {
    if let Ok(val) = std::env::var(var) {
        cmd.env(var, val);
    }
}

let result = tokio::time::timeout(
    Duration::from_secs(SHELL_TIMEOUT_SECS),
    cmd.output(),
)
.await;
```

---

### Finding 5: Key Generation Uses UUID Instead of Direct CSPRNG

| Field | Value |
|-------|-------|
| **Severity** | MEDIUM |
| **CWE** | [CWE-330: Use of Insufficiently Random Values](https://cwe.mitre.org/data/definitions/330.html) |
| **File** | `src/security/secrets.rs:221-230` |
| **Addressed by PR** | None |

#### Description

The encryption key for the secret store is generated by concatenating two UUID v4 values:

```rust
// src/security/secrets.rs:220-230
/// Generate a random key using system entropy (UUID v4 + process ID + timestamp).
fn generate_random_key() -> Vec<u8> {
    // Use two UUIDs (32 random bytes) as our key material
    let u1 = uuid::Uuid::new_v4();
    let u2 = uuid::Uuid::new_v4();
    let mut key = Vec::with_capacity(KEY_LEN);
    key.extend_from_slice(u1.as_bytes());
    key.extend_from_slice(u2.as_bytes());
    key.truncate(KEY_LEN);
    key
}
```

**Issues:**
1. **Reduced entropy:** UUID v4 has only 122 random bits per 128-bit value (6 bits are fixed version/variant markers: version nibble = `0100`, variant bits = `10xx`). Two UUIDs yield 244 bits of entropy in a 256-bit key, not the full 256 bits.
2. **Predictable bit patterns:** Bytes 6 and 8 of each UUID contain predictable bits. An attacker knowing the key format can reduce brute-force search space.
3. **Misleading comment:** The docstring mentions "process ID + timestamp" which are not actually used, suggesting the implementation may have deviated from an earlier (worse) design.
4. **Unnecessary complexity:** Generating random bytes directly is simpler and more correct.

#### Remediation

Use the CSPRNG directly:

```rust
fn generate_random_key() -> Vec<u8> {
    use chacha20poly1305::aead::OsRng;
    use rand_core::RngCore;
    let mut key = vec![0u8; KEY_LEN];
    OsRng.fill_bytes(&mut key);
    key
}
```

Note: `OsRng` is already available through the `chacha20poly1305` dependency. The `rand_core` crate may need to be added explicitly, or `getrandom` (transitively available) can be used:

```rust
fn generate_random_key() -> Vec<u8> {
    let mut key = vec![0u8; KEY_LEN];
    getrandom::getrandom(&mut key).expect("OS RNG failed");
    key
}
```

---

### Finding 6: Unicode Truncation Panics (Multiple Locations)

| Field | Value |
|-------|-------|
| **Severity** | MEDIUM |
| **CWE** | [CWE-119: Buffer Errors](https://cwe.mitre.org/data/definitions/119.html) (panic = denial of service) |
| **Files** | `src/gateway/mod.rs:376-379`, `src/agent/loop_.rs:153-154`, `src/agent/loop_.rs:196-197` |
| **Addressed by PR** | None |

#### Description

String slicing with byte indices panics when the index falls within a multi-byte UTF-8 character:

**Location 1 - Gateway WhatsApp handler:**
```rust
// src/gateway/mod.rs:376-379
if msg.content.len() > 50 {
    format!("{}...", &msg.content[..50])  // PANIC if byte 50 is mid-character
} else {
    msg.content.clone()
}
```

**Location 2 - Agent loop (single-shot mode):**
```rust
// src/agent/loop_.rs:153-154
let summary = if response.len() > 100 {
    format!("{}...", &response[..100])  // PANIC if byte 100 is mid-character
```

**Location 3 - Agent loop (interactive mode):**
```rust
// src/agent/loop_.rs:196-197
let summary = if response.len() > 100 {
    format!("{}...", &response[..100])  // PANIC if byte 100 is mid-character
```

**Impact:** Any non-ASCII input (Chinese, Japanese, Korean characters, emoji, Arabic, etc.) that happens to have a multi-byte character spanning the truncation boundary will cause a `panic!` with message `byte index N is not a char boundary`. In a gateway context, this crashes the request handler. In the agent loop, it terminates the session.

**Reproduction:**
```rust
let s = "Hello \u{1F600} world"; // emoji is 4 bytes
let _ = &s[..8]; // panics: byte 8 is inside the emoji
```

#### Remediation

Extract a reusable safe truncation helper:

```rust
/// Truncate a string to at most `max_chars` characters, appending "..." if truncated.
fn truncate_with_ellipsis(s: &str, max_chars: usize) -> String {
    match s.char_indices().nth(max_chars) {
        Some((idx, _)) => format!("{}...", &s[..idx]),
        None => s.to_string(),
    }
}
```

Then replace all three locations:
```rust
// Before:
if msg.content.len() > 50 { format!("{}...", &msg.content[..50]) } else { msg.content.clone() }

// After:
truncate_with_ellipsis(&msg.content, 50)
```

---

### Finding 7: Windows Key File Permissions Silently Ignored

| Field | Value |
|-------|-------|
| **Severity** | MEDIUM |
| **CWE** | [CWE-732: Incorrect Permission Assignment for Critical Resource](https://cwe.mitre.org/data/definitions/732.html) |
| **File** | `src/security/secrets.rs:191-202` |
| **Addressed by PR** | None |

#### Description

On Windows, the key file permissions are set via `icacls`, but the result is silently discarded:

```rust
// src/security/secrets.rs:191-202
#[cfg(windows)]
{
    // On Windows, use icacls to restrict permissions to current user only
    let _ = std::process::Command::new("icacls")
        .arg(&self.key_path)
        .args(["/inheritance:r", "/grant:r"])
        .arg(format!(
            "{}:F",
            std::env::var("USERNAME").unwrap_or_default()
        ))
        .output();
}
```

**Impact:** If `icacls` fails (binary not found, permission denied, `USERNAME` env var empty), the encryption key file remains world-readable with default Windows ACLs. No warning or error is logged, so the operator has no indication that the key is exposed.

Note: On Unix, the equivalent `fs::set_permissions` call (`secrets.rs:188-189`) properly returns an error via `?`.

#### Remediation

Log a warning on failure instead of silently discarding:

```rust
#[cfg(windows)]
{
    let username = std::env::var("USERNAME").unwrap_or_default();
    if username.is_empty() {
        tracing::warn!("Cannot set key file permissions: USERNAME env var is empty");
    } else {
        match std::process::Command::new("icacls")
            .arg(&self.key_path)
            .args(["/inheritance:r", "/grant:r"])
            .arg(format!("{username}:F"))
            .output()
        {
            Ok(o) if !o.status.success() => {
                tracing::warn!(
                    "Failed to restrict key file permissions (icacls exit {})",
                    o.status
                );
            }
            Err(e) => {
                tracing::warn!("Could not set key file permissions: {e}");
            }
            _ => {}
        }
    }
}
```

---

## Medium Security Issues

---

### Finding 8: `constant_time_eq` Leaks Length via Early Return

| Field | Value |
|-------|-------|
| **Severity** | LOW |
| **CWE** | [CWE-208: Observable Timing Discrepancy](https://cwe.mitre.org/data/definitions/208.html) |
| **File** | `src/security/pairing.rs:178-186` |
| **Addressed by PR** | None |

#### Description

The `constant_time_eq` function returns `false` immediately when lengths differ:

```rust
// src/security/pairing.rs:178-186
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;  // Leaks length information via timing
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}
```

**Context:** This function is used for:
1. **Pairing code comparison** - 6-digit codes are always the same length, so length leakage is moot.
2. **Webhook secret comparison** (`constant_time_eq(val, secret.as_ref())` at gateway authentication) - secrets could have variable length, making timing leakage meaningful.

For webhook secrets, an attacker could determine the secret length by measuring response times for different-length inputs, then focus brute-force on the correct length.

#### Remediation

Use the `subtle` crate for proper constant-time comparison, or pad to equal lengths:

```rust
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    // Compare in constant time regardless of length
    let len_eq = a.len() == b.len();
    let byte_eq = a.bytes()
        .zip(b.bytes())
        .chain(std::iter::repeat((0u8, 1u8)).take(if len_eq { 0 } else { 1 }))
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0;
    len_eq & byte_eq  // Bitwise AND, not short-circuit &&
}
```

Or simply use `subtle::ConstantTimeEq` (add `subtle = "2"` to dependencies).

---

### Finding 9: Bearer Tokens Stored in Config as Plaintext

| Field | Value |
|-------|-------|
| **Severity** | MEDIUM |
| **CWE** | [CWE-256: Plaintext Storage of a Password](https://cwe.mitre.org/data/definitions/256.html) |
| **File** | `src/config/schema.rs:98-100` |
| **Addressed by PR** | PR #39 (partial - encrypts API keys but not bearer tokens) |

#### Description

Paired bearer tokens are stored as plaintext strings in the config file:

```rust
// src/config/schema.rs:98-100
/// Paired bearer tokens (managed automatically, not user-edited)
#[serde(default)]
pub paired_tokens: Vec<String>,
```

The token format (from `pairing.rs:174`) is `zc_<uuid>`:
```rust
fn generate_token() -> String {
    format!("zc_{}", uuid::Uuid::new_v4().as_simple())
}
```

**Impact:** Anyone with read access to `config.toml` (e.g., backup systems, shared filesystem, accidental commit) can extract bearer tokens and impersonate paired clients. PR #39 migrates API keys to encrypted `enc2:` format but does not address bearer tokens.

#### Remediation

Store SHA-256 hashes of tokens instead of plaintext. The token is returned to the client once during pairing, and subsequent authentication compares hashes:

```rust
use sha2::{Sha256, Digest};

fn hash_token(token: &str) -> String {
    let hash = Sha256::digest(token.as_bytes());
    hex::encode(hash)
}

// During pairing: store hash_token(&token) in config
// During auth: compare hash_token(incoming_token) against stored hashes
```

This requires `sha2` (which may already be needed for Finding #2).

---

### Finding 10: LLM Error Details Leaked to Clients

| Field | Value |
|-------|-------|
| **Severity** | MEDIUM |
| **CWE** | [CWE-209: Generation of Error Message Containing Sensitive Information](https://cwe.mitre.org/data/definitions/209.html) |
| **File** | `src/gateway/mod.rs:304-306`, `src/gateway/mod.rs:407-409` |
| **Addressed by PR** | None |

#### Description

LLM errors are forwarded directly to HTTP clients and WhatsApp users:

**Location 1 - Webhook handler:**
```rust
// src/gateway/mod.rs:304-306
Err(e) => {
    let err = serde_json::json!({"error": format!("LLM error: {e}")});
    (StatusCode::INTERNAL_SERVER_ERROR, Json(err))
}
```

**Location 2 - WhatsApp handler:**
```rust
// src/gateway/mod.rs:407-409
Err(e) => {
    tracing::error!("LLM error for WhatsApp message: {e}");
    let _ = wa.send(&format!("Error: {e}"), &msg.sender).await;
}
```

**Impact:** The `anyhow` error chain could include:
- Provider API URLs (revealing which LLM provider is used)
- HTTP status codes and response bodies from the provider
- Partial request payloads or headers
- Internal file paths or configuration details

#### Remediation

Return generic error messages to clients, log full details server-side:

```rust
// Webhook handler:
Err(e) => {
    tracing::error!("LLM error: {e:#}");
    let err = serde_json::json!({"error": "Internal error processing your request"});
    (StatusCode::INTERNAL_SERVER_ERROR, Json(err))
}

// WhatsApp handler:
Err(e) => {
    tracing::error!("LLM error for WhatsApp message: {e:#}");
    let _ = wa.send("Sorry, I encountered an error processing your message.", &msg.sender).await;
}
```

---

### Finding 11: `REQUEST_TIMEOUT_SECS` Defined But Not Applied

| Field | Value |
|-------|-------|
| **Severity** | MEDIUM |
| **CWE** | [CWE-400: Uncontrolled Resource Consumption](https://cwe.mitre.org/data/definitions/400.html) |
| **File** | `src/gateway/mod.rs:31-32, 167-178` |
| **Addressed by PR** | PR #40 (adds `TimeoutLayer`) |

#### Description

The `REQUEST_TIMEOUT_SECS` constant is defined but no timeout middleware is applied to the router:

```rust
// src/gateway/mod.rs:31
pub const REQUEST_TIMEOUT_SECS: u64 = 30;
```

```rust
// src/gateway/mod.rs:167-178
// Note: ... Timeout is handled by tokio's TcpListener accept timeout and hyper's built-in timeouts
let app = Router::new()
    .route("/health", get(handle_health))
    .route("/pair", post(handle_pair))
    .route("/webhook", post(handle_webhook))
    .route("/whatsapp", get(handle_whatsapp_verify))
    .route("/whatsapp", post(handle_whatsapp_message))
    .with_state(state)
    .layer(RequestBodyLimitLayer::new(MAX_BODY_SIZE));
    // No TimeoutLayer applied!

axum::serve(listener, app).await?;  // No graceful shutdown either
```

The comment claims timeout is "handled by tokio's TcpListener accept timeout and hyper's built-in timeouts" but:
- TCP accept timeout only limits how long to wait for new connections, not request processing time.
- Hyper does not have built-in request processing timeouts.
- Slow/hung LLM requests can hold connections indefinitely.

**Note:** PR #40 addresses this by adding `TimeoutLayer`. This finding is documented for completeness and to ensure the fix is merged.

#### Remediation

Already addressed by PR #40. Ensure it is merged. The fix is:

```rust
use tower_http::timeout::TimeoutLayer;

let app = Router::new()
    // ... routes ...
    .layer(RequestBodyLimitLayer::new(MAX_BODY_SIZE))
    .layer(TimeoutLayer::new(Duration::from_secs(REQUEST_TIMEOUT_SECS)));
```

---

## Code Quality Issues

---

### Finding 12: Forbidden Path Check Uses String Prefix (False Positives)

| Field | Value |
|-------|-------|
| **Severity** | LOW |
| **CWE** | [CWE-280: Improper Handling of Insufficient Permissions](https://cwe.mitre.org/data/definitions/280.html) |
| **File** | `src/security/policy.rs:248-253` |
| **Addressed by PR** | None |

#### Description

The forbidden path check uses `starts_with` on the raw string, which doesn't respect path component boundaries:

```rust
// src/security/policy.rs:248-253
// Block forbidden paths
for forbidden in &self.forbidden_paths {
    if path.starts_with(forbidden.as_str()) {
        return false;
    }
}
```

**Example:** If `/etc` is in `forbidden_paths`:
- `/etc/passwd` is correctly blocked
- `/etc2/safe-file` is **incorrectly blocked** (false positive)
- `/etcetera/config` is **incorrectly blocked** (false positive)

#### Remediation

Use `std::path::Path::starts_with` which is component-aware:

```rust
for forbidden in &self.forbidden_paths {
    if Path::new(path).starts_with(forbidden.as_str()) {
        return false;
    }
}
```

Note: `std::path::Path::starts_with` checks component boundaries (unlike `str::starts_with`), so `/etc2` does NOT start with `/etc`.

---

### Finding 13: Empty Path Allowed by Security Policy

| Field | Value |
|-------|-------|
| **Severity** | LOW |
| **File** | `src/security/policy.rs:232-256` |
| **Addressed by PR** | None |

#### Description

`is_path_allowed("")` returns `true` because an empty string:
- Does not contain `\0`
- Does not contain `..`
- Is not absolute (when `workspace_only` is true)
- Does not start with any forbidden path

```rust
// src/security/policy.rs:232-256
pub fn is_path_allowed(&self, path: &str) -> bool {
    if path.contains('\0') { return false; }
    if path.contains("..") { return false; }
    if self.workspace_only && Path::new(path).is_absolute() { return false; }
    for forbidden in &self.forbidden_paths {
        if path.starts_with(forbidden.as_str()) { return false; }
    }
    true  // Empty string reaches here
}
```

**Impact:** When an empty path is joined with `workspace_dir`, it resolves to the workspace directory itself. Depending on how the caller uses this, it could allow unintended operations on the workspace root directory.

#### Remediation

```rust
pub fn is_path_allowed(&self, path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    // ... rest of checks
}
```

---

### Finding 14: Duplicate Parent Directory Check in FileWriteTool

| Field | Value |
|-------|-------|
| **Severity** | INFO |
| **File** | `src/tools/file_write.rs:67-78` |
| **Addressed by PR** | None |

#### Description

`full_path.parent()` is called twice in sequence:

```rust
// src/tools/file_write.rs:67-78
// Ensure parent directory exists
if let Some(parent) = full_path.parent() {
    tokio::fs::create_dir_all(parent).await?;
}

let Some(parent) = full_path.parent() else {
    return Ok(ToolResult {
        success: false,
        output: String::new(),
        error: Some("Invalid path: missing parent directory".into()),
    });
};
```

The first `if let Some(parent)` creates the directory. The second `let Some(parent) else` checks for the same `None` case that the first block already handles implicitly (if `parent()` returns `None`, `create_dir_all` is skipped, and then the second check catches it).

However, the second check also binds `parent` for use in the symlink escape check below (line 81: `tokio::fs::canonicalize(parent)`). So the duplicate call is not purely redundant - it serves a structural purpose.

#### Remediation

Combine into a single check:

```rust
let parent = match full_path.parent() {
    Some(p) => {
        tokio::fs::create_dir_all(p).await?;
        p
    }
    None => {
        return Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some("Invalid path: missing parent directory".into()),
        });
    }
};
```

---

### Finding 15: Response Truncation Pattern Duplicated

| Field | Value |
|-------|-------|
| **Severity** | INFO |
| **File** | `src/agent/loop_.rs:152-157, 195-200` |
| **Addressed by PR** | None |

#### Description

The exact same truncation logic appears twice in the agent loop:

```rust
// src/agent/loop_.rs:152-157 (single-shot mode)
let summary = if response.len() > 100 {
    format!("{}...", &response[..100])
} else {
    response.clone()
};

// src/agent/loop_.rs:195-200 (interactive mode)
let summary = if response.len() > 100 {
    format!("{}...", &response[..100])
} else {
    response.clone()
};
```

This duplication also carries the unicode truncation bug (Finding #6).

#### Remediation

Extract to the helper function proposed in Finding #6, then use it in both locations:

```rust
let summary = truncate_with_ellipsis(&response, 100);
```

---

### Finding 16: `#[allow(clippy::too_many_lines)]` Suppressions

| Field | Value |
|-------|-------|
| **Severity** | INFO |
| **Files** | `src/gateway/mod.rs:47`, `src/agent/loop_.rs:31`, `src/providers/mod.rs:15` |
| **Addressed by PR** | None |

#### Description

Three functions suppress the `too_many_lines` clippy lint:

```rust
// src/gateway/mod.rs:47
#[allow(clippy::too_many_lines)]
pub async fn run_gateway(host: &str, port: u16, config: Config) -> Result<()> {

// src/agent/loop_.rs:31
#[allow(clippy::too_many_lines)]
pub async fn run(config: Config, message: Option<String>, ...) -> Result<()> {

// src/providers/mod.rs:15
#[allow(clippy::too_many_lines)]
pub fn create_provider(name: &str, api_key: Option<&str>) -> anyhow::Result<Box<dyn Provider>> {
```

These are not bugs, but suppressing lint warnings without addressing the underlying issue makes code harder to maintain. Long functions are harder to test, review, and reason about.

#### Remediation

Consider extracting logical sub-functions:
- `run_gateway`: Extract provider setup, state construction, and router building into separate functions.
- `run`: Extract system prompt building, single-shot handling, and interactive loop into separate functions.
- `create_provider`: This is a match-based factory which is inherently long; the suppression is more justified here.

**Priority:** Low. Address when touching these functions for other reasons.

---

### Finding 17: Graceful Shutdown Not Implemented

| Field | Value |
|-------|-------|
| **Severity** | MEDIUM |
| **File** | `src/gateway/mod.rs:178` |
| **Addressed by PR** | None |

#### Description

The server starts without graceful shutdown handling:

```rust
// src/gateway/mod.rs:178
axum::serve(listener, app).await?;
```

**Impact:** When the process receives SIGTERM (e.g., `docker stop`, Kubernetes pod termination), in-flight HTTP requests are immediately dropped. This means:
- Webhook responses may not be sent (Meta will retry, causing duplicate processing)
- LLM responses in progress are lost
- Memory stores may not be flushed

#### Remediation

```rust
use tokio::signal;

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c().await.expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::info!("Shutdown signal received, finishing in-flight requests...");
}

// In run_gateway:
axum::serve(listener, app)
    .with_graceful_shutdown(shutdown_signal())
    .await?;
```

Note: The `tokio` dependency already includes the `signal` feature (`Cargo.toml:18`).

---

## Testing Gaps

---

### Finding 18: No Tests for Agent Loop

| Field | Value |
|-------|-------|
| **Severity** | MEDIUM |
| **File** | `src/agent/loop_.rs` |
| **Addressed by PR** | None |

#### Description

The core agent execution logic has **zero** test coverage. There is no `#[cfg(test)]` module in `src/agent/loop_.rs`.

The following critical logic paths are untested:
- `build_context()` function - system prompt assembly from config
- Single-shot mode (`message.is_some()` branch)
- Interactive loop (`loop { ... }` branch)
- Memory auto-save behavior
- `/quit` command handling
- Response truncation (which has a bug - Finding #6)

#### Remediation

Add unit tests for at minimum:
1. `build_context()` output with various config combinations
2. Response truncation helper (once extracted per Finding #6/15)
3. The `/quit` command detection logic

Integration tests for the full loop are harder but could use a mock `Provider`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_context_includes_model() {
        let config = Config::default();
        let ctx = build_context(&config);
        assert!(ctx.contains("Model:"));
    }

    #[test]
    fn build_context_includes_tools() {
        let mut config = Config::default();
        config.tools.shell = true;
        let ctx = build_context(&config);
        assert!(ctx.contains("shell"));
    }
}
```

---

### Finding 19: Weak Pairing Test

| Field | Value |
|-------|-------|
| **Severity** | LOW |
| **File** | `src/security/pairing.rs:232-239` |
| **Addressed by PR** | None |

#### Description

The `try_pair_wrong_code` test doesn't assert anything meaningful:

```rust
// src/security/pairing.rs:232-239
#[test]
fn try_pair_wrong_code() {
    let guard = PairingGuard::new(true, &[]);
    let result = guard.try_pair("000000").unwrap();
    // Might succeed if code happens to be 000000, but extremely unlikely
    // Just check it returns Ok(None) normally
    let _ = result;  // Result is discarded without assertion!
}
```

The test creates a `PairingGuard`, attempts pairing with `"000000"`, and then **discards the result** without asserting anything. It essentially only tests that `try_pair` doesn't panic.

#### Remediation

Either assert the expected outcome or use a deterministic approach:

```rust
#[test]
fn try_pair_wrong_code() {
    let guard = PairingGuard::new(true, &[]);
    // Try a series of wrong codes - at most one can match the random code
    let results: Vec<_> = ["000000", "111111", "222222"]
        .iter()
        .filter_map(|code| guard.try_pair(code).ok().flatten())
        .collect();
    // At most one of these could match by chance
    assert!(results.len() <= 1, "Multiple wrong codes matched");
}
```

Or better, expose the generated code for testing (behind `#[cfg(test)]`) and test against a provably wrong code.

---

### Finding 20: No Gateway Integration Tests

| Field | Value |
|-------|-------|
| **Severity** | MEDIUM |
| **File** | `src/gateway/mod.rs:418-459` |
| **Addressed by PR** | PR #40 (adds 8 integration tests) |

#### Description

The gateway test module only contains trivial constant-value assertions:

```rust
// src/gateway/mod.rs:422-430
#[test]
fn security_body_limit_is_64kb() {
    assert_eq!(MAX_BODY_SIZE, 65_536);
}

#[test]
fn security_timeout_is_30_seconds() {
    assert_eq!(REQUEST_TIMEOUT_SECS, 30);
}
```

No tests exercise the actual HTTP handlers (`handle_health`, `handle_pair`, `handle_webhook`, `handle_whatsapp_message`). The remaining tests (`webhook_body_requires_message_field`, `whatsapp_query_fields_are_optional`, `app_state_is_clone`) test serialization and trait bounds, not handler behavior.

**Note:** PR #40 adds 8 integration tests covering the HTTP handlers. Ensure it is merged.

#### Remediation

Already addressed by PR #40. Ensure it is merged. Additionally, consider adding tests for:
- Non-ASCII input handling (related to Finding #6)
- Error response format
- Authentication/pairing flow end-to-end

---

### Finding 21: No Fuzz Testing for Security-Critical Parsers

| Field | Value |
|-------|-------|
| **Severity** | LOW |
| **File** | Multiple |
| **Addressed by PR** | None |

#### Description

Several security-critical parsing functions would benefit from fuzz testing:

| Function | File | Risk |
|----------|------|------|
| `is_command_allowed` | `src/security/policy.rs` | Command injection bypass |
| `is_path_allowed` | `src/security/policy.rs` | Path traversal bypass |
| `validate_url` | `src/security/policy.rs` | URL validation bypass |
| `escape_for_applescript` | `src/channels/imessage.rs` | AppleScript injection |
| `constant_time_eq` | `src/security/pairing.rs` | Timing leaks |
| Webhook JSON parsing | `src/gateway/mod.rs` | Panic on malformed input |

#### Remediation

Add `cargo-fuzz` targets for security-critical parsers:

```toml
# fuzz/Cargo.toml
[package]
name = "zeroclaw-fuzz"
version = "0.0.0"
edition = "2021"

[dependencies]
libfuzzer-sys = "0.4"
zeroclaw = { path = ".." }

[[bin]]
name = "fuzz_is_path_allowed"
path = "fuzz_targets/is_path_allowed.rs"
```

```rust
// fuzz/fuzz_targets/is_path_allowed.rs
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &str| {
    let policy = zeroclaw::security::SecurityPolicy::default();
    let _ = policy.is_path_allowed(data);
});
```

---

## CI/CD & Infrastructure

---

### Finding 22: Docker Rust Image Pinned to 1.83

| Field | Value |
|-------|-------|
| **Severity** | LOW |
| **File** | `Dockerfile:2` |
| **Addressed by PR** | None |

#### Description

```dockerfile
# Dockerfile:2
FROM rust:1.83-slim AS builder
```

Rust 1.83 was released in late 2024. As of February 2026, the latest stable Rust is 1.85+. Pinning to an old version means:
- Missing compiler bug fixes and optimizations
- Missing new language features (e.g., native `async fn` in traits from 1.75+)
- Potential security fixes in the compiler/stdlib

#### Remediation

Either:
1. Bump to current stable: `FROM rust:1.85-slim AS builder`
2. Track latest: `FROM rust:latest AS builder` (less reproducible but always current)
3. Pin to a specific recent version with a comment explaining when to bump

---

### Finding 23: No MSRV Declared

| Field | Value |
|-------|-------|
| **Severity** | LOW |
| **File** | `Cargo.toml` |
| **Addressed by PR** | None |

#### Description

The `Cargo.toml` does not include a `rust-version` field:

```toml
[package]
name = "zeroclaw"
version = "0.1.0"
edition = "2021"
# No rust-version field
```

Without an MSRV (Minimum Supported Rust Version), there is no documentation of which Rust version is required, and CI cannot automatically detect when a dependency requires a newer compiler.

#### Remediation

Add `rust-version` to `Cargo.toml`:

```toml
[package]
name = "zeroclaw"
version = "0.1.0"
edition = "2021"
rust-version = "1.83"  # Match Dockerfile version
```

Also add an MSRV check to CI:

```yaml
- name: Check MSRV
  run: cargo check --locked
  env:
    RUSTUP_TOOLCHAIN: "1.83"
```

---

### Finding 24: Docker Build Doesn't Cache Dependencies

| Field | Value |
|-------|-------|
| **Severity** | LOW |
| **File** | `Dockerfile:4-9` |
| **Addressed by PR** | None |

#### Description

The Dockerfile copies source files before building, which invalidates the Docker layer cache on every source change:

```dockerfile
# Dockerfile:4-9
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src/ src/

RUN cargo build --release --locked && \
    strip target/release/zeroclaw
```

While `Cargo.toml` and `Cargo.lock` are copied before `src/`, the build step follows `COPY src/` so any source change invalidates the `cargo build` layer, forcing a full rebuild including all dependencies (~5-15 minutes for a Rust project this size).

#### Remediation

Use a dummy build step to cache dependencies separately:

```dockerfile
WORKDIR /app

# Step 1: Cache dependencies
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && \
    cargo build --release --locked && \
    rm -rf src/

# Step 2: Build actual source (deps are cached)
COPY src/ src/
RUN touch src/main.rs && cargo build --release --locked && \
    strip target/release/zeroclaw
```

This separates dependency compilation from source compilation. When only `src/` changes, only the second `RUN` layer is invalidated, saving significant build time.

---

### Finding 25: CI Doesn't Run Tests on Windows/macOS

| Field | Value |
|-------|-------|
| **Severity** | LOW |
| **File** | `.github/workflows/ci.yml:13-32` |
| **Addressed by PR** | None |

#### Description

The CI pipeline has two separate jobs:

1. **`test`** job: Runs on `ubuntu-latest` only - includes `cargo fmt`, `cargo clippy`, and `cargo test`.
2. **`build`** job: Runs on a 4-platform matrix (Linux, macOS x86, macOS ARM, Windows) - only runs `cargo build`, no tests.

```yaml
# .github/workflows/ci.yml:13-32
  test:
    name: Test
    runs-on: ubuntu-latest  # Tests only on Linux
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - name: Check formatting
        run: cargo fmt -- --check
      - name: Run clippy
        run: cargo clippy -- -D warnings
      - name: Run tests
        run: cargo test --verbose
```

**Impact:** Platform-specific code is never tested on its target platform:
- iMessage channel (macOS-only) is tested on Linux where the `chat.db` doesn't exist
- Windows key file permissions (`icacls`) are never tested on Windows
- Service management code behavior may differ across platforms

#### Remediation

Add `cargo test` to the build matrix:

```yaml
  build:
    name: Build & Test
    runs-on: ${{ matrix.os }}
    strategy:
      matrix:
        include:
          - os: ubuntu-latest
            target: x86_64-unknown-linux-gnu
          - os: macos-latest
            target: aarch64-apple-darwin
          - os: windows-latest
            target: x86_64-pc-windows-msvc
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: ${{ matrix.target }}
      - uses: Swatinem/rust-cache@v2
      - name: Run tests
        run: cargo test --verbose
      - name: Build release
        run: cargo build --release --target ${{ matrix.target }}
```

---

## Dependency & Architecture Notes

---

### Finding 26: `async-trait` Crate Could Be Replaced

| Field | Value |
|-------|-------|
| **Severity** | INFO |
| **File** | `Cargo.toml:47` |
| **Addressed by PR** | None |

#### Description

```toml
# Cargo.toml:47
async-trait = "0.1"
```

Rust 1.75+ (stable since December 2023) supports `async fn` in traits natively via Return Position Impl Trait in Traits (RPITIT). The `async-trait` crate works by desugaring async trait methods into `Pin<Box<dyn Future>>`, which:
- Adds a heap allocation per async method call
- Increases compile times via proc-macro expansion
- Adds a dependency to audit

Since the project already uses Rust 1.83+ (per Dockerfile), native async traits are available.

#### Remediation

Replace `#[async_trait]` annotations with native `async fn` in trait definitions. Example:

```rust
// Before:
#[async_trait]
pub trait Provider: Send + Sync {
    async fn chat(&self, message: &str, model: &str, temp: f64) -> Result<String>;
}

// After:
pub trait Provider: Send + Sync {
    fn chat(&self, message: &str, model: &str, temp: f64)
        -> impl Future<Output = Result<String>> + Send;
}
```

**Note:** If traits are used as `dyn Provider` (trait objects), native async traits require `dyn`-compatible return types. In that case, `async-trait` remains necessary or the design needs refactoring to use `Box<dyn Provider>` with manual boxing. Check usage before migrating.

**Priority:** Low. Do this during a routine dependency cleanup, not as a standalone PR.

---

### Finding 27: Axum Version Behind

| Field | Value |
|-------|-------|
| **Severity** | LOW |
| **File** | `Cargo.toml:64` |
| **Addressed by PR** | PR #40 (upgrades to axum 0.8) |

#### Description

```toml
# Cargo.toml:64
axum = { version = "0.7", default-features = false, features = ["http1", "json", "tokio", "query"] }
```

The project uses axum 0.7. Axum 0.8 includes security fixes, API improvements, and better integration with tower middleware.

**Note:** PR #40 upgrades to axum 0.8. Ensure it is merged.

#### Remediation

Already addressed by PR #40. Ensure it is merged.

---

### Finding 28: No Rate Limiting on HTTP Endpoints

| Field | Value |
|-------|-------|
| **Severity** | MEDIUM |
| **CWE** | [CWE-770: Allocation of Resources Without Limits](https://cwe.mitre.org/data/definitions/770.html) |
| **File** | `src/gateway/mod.rs:168-175` |
| **Addressed by PR** | None |

#### Description

While tool-level rate limiting exists (20 actions/hour in `SecurityPolicy`), there is no HTTP-level rate limiting on the gateway endpoints:

```rust
// src/gateway/mod.rs:168-175
let app = Router::new()
    .route("/health", get(handle_health))
    .route("/pair", post(handle_pair))
    .route("/webhook", post(handle_webhook))
    .route("/whatsapp", get(handle_whatsapp_verify))
    .route("/whatsapp", post(handle_whatsapp_message))
    .with_state(state)
    .layer(RequestBodyLimitLayer::new(MAX_BODY_SIZE));
```

**Impact:** An attacker can:
- Flood `/pair` with brute-force attempts (the pairing lockout exists but still consumes server resources for each attempt)
- Flood `/health` to create log noise or exhaust connections
- Flood `/webhook` or `/whatsapp` to exhaust LLM API credits (each valid-looking request triggers an LLM call)

#### Remediation

Add `tower::limit::RateLimitLayer` or use `governor` for more sophisticated rate limiting:

```rust
use tower::limit::RateLimitLayer;
use std::time::Duration;

let app = Router::new()
    .route("/health", get(handle_health))
    .route("/pair", post(handle_pair))
    .route("/webhook", post(handle_webhook))
    .route("/whatsapp", get(handle_whatsapp_verify))
    .route("/whatsapp", post(handle_whatsapp_message))
    .with_state(state)
    .layer(RequestBodyLimitLayer::new(MAX_BODY_SIZE))
    .layer(RateLimitLayer::new(100, Duration::from_secs(60)));  // 100 req/min
```

For per-endpoint rate limiting (e.g., stricter limits on `/pair`), use `tower::ServiceBuilder` with different rate limits per route group.

---

## Open PR Coverage Matrix

| # | Finding | Severity | Addressed by PR | Status |
|---|---------|----------|-----------------|--------|
| 1 | SQL Injection in iMessage | HIGH | -- | **NEW - Needs PR** |
| 2 | WhatsApp Webhook Signature | HIGH | -- | **NEW - Needs PR** |
| 3 | iMessage subprocess bypass | MEDIUM | -- | **NEW - Needs PR** (fixed with #1) |
| 4 | Shell env variable leakage | MEDIUM | -- | **NEW - Needs PR** |
| 5 | Key generation weakness | MEDIUM | -- | **NEW - Needs PR** |
| 6 | Unicode truncation panics | MEDIUM | -- | **NEW - Needs PR** |
| 7 | Windows key file permissions | MEDIUM | -- | **NEW - Needs PR** |
| 8 | `constant_time_eq` length leak | LOW | -- | **NEW - Needs PR** |
| 9 | Bearer tokens as plaintext | MEDIUM | PR #39 (partial) | **NEW - Needs PR** |
| 10 | LLM error info leakage | MEDIUM | -- | **NEW - Needs PR** |
| 11 | Request timeout not applied | MEDIUM | PR #40 | Covered |
| 12 | Forbidden path prefix match | LOW | -- | **NEW - Needs PR** |
| 13 | Empty path allowed | LOW | -- | **NEW - Needs PR** |
| 14 | Duplicate parent dir check | INFO | -- | **NEW - Needs PR** |
| 15 | Response truncation duplicated | INFO | -- | Fixed with #6 |
| 16 | Clippy suppressions | INFO | -- | Low priority |
| 17 | Graceful shutdown missing | MEDIUM | -- | **NEW - Needs PR** |
| 18 | No agent loop tests | MEDIUM | -- | **NEW - Needs PR** |
| 19 | Weak pairing test | LOW | -- | **NEW - Needs PR** |
| 20 | No gateway integration tests | MEDIUM | PR #40 | Covered |
| 21 | No fuzz testing | LOW | -- | **NEW - Needs PR** |
| 22 | Docker Rust image outdated | LOW | -- | **NEW - Needs PR** |
| 23 | No MSRV declared | LOW | -- | **NEW - Needs PR** |
| 24 | Docker dep caching | LOW | -- | **NEW - Needs PR** |
| 25 | CI tests not cross-platform | LOW | -- | **NEW - Needs PR** |
| 26 | `async-trait` replaceable | INFO | -- | Low priority |
| 27 | Axum version behind | LOW | PR #40 | Covered |
| 28 | No HTTP rate limiting | MEDIUM | -- | **NEW - Needs PR** |

**Existing PRs covering findings:**
- **PR #36** - Docker context leakage (not in this review, already covered)
- **PR #38** - AppleScript injection (not in this review, already covered)
- **PR #39** - Legacy XOR encryption (partial coverage of Finding #9)
- **PR #40** - Gateway timeout/hardening (Findings #11, #20, #27)

---

## Recommended Priority Order

Fixes should be implemented as individual PRs, ordered by impact:

| Priority | Finding | PR Title | Rationale |
|----------|---------|----------|-----------|
| **P1** | #6, #15 | `fix: prevent unicode truncation panics` | Can crash production with any non-ASCII input |
| **P2** | #1, #3 | `fix: replace sqlite3 CLI with rusqlite in iMessage channel` | Eliminates SQL injection pattern + subprocess bypass |
| **P3** | #4 | `fix: clear environment variables in shell tool` | API keys exposed to shell commands |
| **P4** | #2 | `feat: verify WhatsApp webhook signatures` | Spoofed messages currently processed |
| **P5** | #5 | `fix: use direct CSPRNG for key generation` | Full 256-bit entropy |
| **P6** | #10 | `fix: don't forward LLM errors to clients` | Internal error details leaked |
| **P7** | #9 | `fix: hash bearer tokens in config` | Plaintext tokens at rest |
| **P8** | #17 | `feat: add graceful shutdown to gateway` | Production reliability |
| **P9** | #28 | `feat: add HTTP rate limiting` | Resource exhaustion protection |
| **P10** | #7 | `fix: log Windows key file permission failures` | Silent security failures |
| **P11** | #12, #13 | `fix: improve path validation in security policy` | False positives + empty path edge case |
| **P12** | #8 | `fix: constant-time comparison for variable-length secrets` | Timing side channel |
| **P13** | #18, #19 | `test: add agent loop and pairing tests` | Coverage gaps |
| **P14** | #22, #23, #24 | `chore: update Docker build and add MSRV` | Infrastructure improvements |
| **P15** | #25 | `ci: run tests on all platforms` | Cross-platform coverage |
| **P16** | #14, #16 | `refactor: minor code quality improvements` | Cleanup |
| **P17** | #21 | `test: add fuzz targets for security parsers` | Advanced testing |
| **P18** | #26 | `refactor: replace async-trait with native async` | Dependency reduction |

---

## Verification Checklist

After implementing fixes, verify:

```bash
# All tests pass
cargo test --verbose

# No clippy warnings
cargo clippy -- -D warnings

# Formatting clean
cargo fmt -- --check

# No known vulnerabilities
cargo audit

# Manual testing
# 1. Send non-ASCII text (emoji, CJK) through gateway endpoints
# 2. Run `env` command through shell tool, verify no API keys in output
# 3. Test WhatsApp webhook with invalid signature (should be rejected)
# 4. Send SIGTERM to gateway process, verify graceful shutdown
# 5. Check key file permissions on Windows after key generation
```
