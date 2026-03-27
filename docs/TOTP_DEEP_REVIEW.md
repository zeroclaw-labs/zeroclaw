# ZeroClaw TOTP — Deep Review & Research Document v2

Final architecture review before code generation.
Covers: consistency check, dependency audit, security findings,
design decisions, autonomy model, prompt injection analysis,
identity mapping, approval queue, and complete module spec.

---

## 1. Consistency check — all decisions cross-referenced

### 1.1 Decisions made in this conversation

| # | Decision | Status | Notes |
|---|----------|--------|-------|
| D1 | TOTP core: RFC 6238, HMAC-SHA1, 6 digits, 30s period, skew +/-1 | FINAL | Max authenticator compatibility |
| D2 | Encrypted storage: ChaCha20-Poly1305 AEAD, fresh nonce per write | FINAL | Matches ZeroClaw's existing auth-profiles.json scheme |
| D3 | Key derivation: HKDF-SHA256 from .secret_key with TOTP-specific context | FINAL | Prevents cross-store key reuse if .secret_key leaks |
| D4 | Replay protection: last_used_step persisted per user | FINAL | RFC 6238 section 5.2 recommendation |
| D5 | Memory zeroization: zeroize + secrecy crates | FINAL | Prevents secret leakage in memory dumps |
| D6 | Recovery codes: 10 per user, 8 alphanumeric chars, CSPRNG, single-use | FINAL | Industry standard (GitHub, Google, Microsoft all use this pattern) |
| D7 | Audit log: ZeroClaw's existing SQLite (rusqlite) | FINAL | Single source of truth, queryable, exportable via CLI |
| D8 | Audit log integrity: hash chain (each entry includes hash of previous) | FINAL | Tamper-evident, verifiable |
| D9 | Config reload: explicit `zeroclaw config reload` command, no auto-watcher | FINAL | Reload event is audit-logged |
| D10 | Dashboard API: CLI + TOML only for v1, read-only API later | FINAL | Security-first approach |
| D11 | Multi-user: per-user secrets, role-based rules, role inheritance | FINAL | |
| D12 | Admin operations: require admin's own TOTP (self-verification) | FINAL | |
| D13 | Emergency plan: 3 paths (user recovery codes, admin reset, maintainer break-glass) | FINAL | |
| D14 | Maintainer key: separate from .secret_key, stored on maintainer's machine | FINAL | |
| D15 | Rules/roles: 100% user-defined via TOML, engine is domain-agnostic | FINAL | |
| D16 | Config validation: serde deserialization with human-readable error wrapper | FINAL | |
| D17 | Lockout: persistent (stored in encrypted store), auto-expiry | FINAL | Survives process restart |
| D18 | Debug output: custom Debug impl, secret_base32 redacted as [REDACTED] | FINAL | |
| D19 | Clock drift: track offset on successful verify, persist as compensation | FINAL | RFC 6238 section 6 |
| D20 | Global rate limit: configurable max TOTP verifications per minute across all users | FINAL | |
| D21 | Pattern matching: substring for v1, optional regex field for v2 | FINAL | regex crate already in ZeroClaw deps |
| D22 | Phishing limitation: documented, FIDO2/WebAuthn as future upgrade path | FINAL | Architectural, not a code fix |
| D23 | Autonomy model: additive ops autonomous, destructive ops need human | FINAL | Cron/SelfHeal context |
| D24 | Approval queue: autonomous agent queues destructive proposals for human review | FINAL | Hash-signed to prevent manipulation |
| D25 | Identity mapping: generic identity field supports pairing, os_user, telegram, matrix, web | FINAL | Engine is identity-source-agnostic |
| D26 | Web UI auth: login + password as first factor, TOTP as second, session-based identity | FINAL | Cookie: Secure, HttpOnly, SameSite=Strict |
| D27 | E-Stop: TOTP-exempt, executes immediately, only requires authenticated session | FINAL | Kill switch must not be blocked by 2FA |
| D28 | TOTP prompt timeout: configurable, default 120s, command rejected on expiry | FINAL | Prevents hanging commands |
| D29 | Store atomicity: write-to-temp-then-rename, backup on each write | FINAL | Prevents corruption from interrupted writes |
| D30 | Concurrent access: file lock (flock) or parking_lot::Mutex on store | FINAL | Prevents race conditions |
| D31 | Gate decision signing: HMAC-signed command string prevents TOCTOU | FINAL | Command cannot be changed between gate and execution |
| D32 | Skill creation/installation: always totp_required | FINAL | Prevents agent from creating bypass tools |
| D33 | Config downgrade detection: TOTP downgrade requires admin TOTP confirmation | FINAL | Prevents silent security reduction |
| D34 | Notifications: critical audit events trigger alert to admin/maintainer | FINAL | Break-glass, repeated failures, config changes |
| D35 | Lockout binding: session-based, not user-based, to prevent DoS | FINAL | Attacker cannot lock out another user remotely |

### 1.2 Contradictions found and resolved

| Issue | Where | Resolution |
|-------|-------|------------|
| ChaCha20Poly1305 version mismatch | ZeroClaw uses 0.10, latest is 0.11-rc | Use ZeroClaw's existing 0.10 version. API is compatible. |
| rand version | ZeroClaw uses rand 0.10, our prototype used 0.8 | Update to rand 0.10. OsRng API changed: `rand::rngs::OsRng` is now just `OsRng` from rand::rngs. |
| sha2 vs sha1 | ZeroClaw has sha2 0.10, we need sha1 0.10 | Add sha1 as new dep. Both are from RustCrypto, same version scheme, compatible. |
| qrcode optional vs required | ZeroClaw has qrcode as optional (whatsapp-web feature) | Gate behind new `totp` feature flag: `totp = ["dep:sha1", "dep:data-encoding", "dep:qrcode", "dep:hkdf"]` |
| Lockout in-memory vs persistent | First prototype was in-memory, audit found this as a gap | RESOLVED: persist in encrypted store. AtomicU32 stays as hot cache, synced to disk on each failure. |
| Single nonce space | ChaCha20Poly1305 uses 96-bit nonce, we generate random per write | Safe. 96-bit random nonce collision probability is negligible for the write frequency of a TOTP store (< 1 write/second). Would need ~2^48 writes for 50% collision probability. |
| Lockout DoS | Original design: lockout per user_id | RESOLVED (D35): bind lockout to session, not user_id. External attacker cannot trigger lockout for another user. |

---

## 2. Dependency audit — what we add to ZeroClaw

### 2.1 New dependencies (not already in Cargo.toml)

| Crate | Version | Purpose | Size impact | Already audited? |
|-------|---------|---------|-------------|-----------------|
| sha1 | 0.10 | HMAC-SHA1 for TOTP (RFC 6238 default) | ~15KB | RustCrypto, same org as sha2 which is already in tree |
| data-encoding | 2.6 | Base32 encode/decode for TOTP secrets | ~30KB | Widely used (38M downloads), no unsafe |
| hkdf | 0.12 | Key derivation from .secret_key | ~5KB | RustCrypto, pure Rust, no deps beyond hmac |
| zeroize | 1.x | Secure memory clearing | ~10KB | RustCrypto, already a transitive dep of chacha20poly1305 |
| secrecy | 0.10 | Prevent reallocation of secret buffers | ~8KB | RustCrypto companion to zeroize |

### 2.2 Already in ZeroClaw (reused, no additions)

chacha20poly1305 0.10, hmac 0.12, sha2 0.10, rand 0.10,
serde 1.0, serde_json 1.0, toml 1.0, chrono 0.4,
clap 4.5, dialoguer 0.12, anyhow 1.0, thiserror 2.0,
tracing 0.1, parking_lot 0.12, rusqlite (bundled),
qrcode 0.14 (optional), regex 1.10.

### 2.3 Transitive dependency note

zeroize is already pulled in transitively by chacha20poly1305.
Making it a direct dependency pins the version explicitly and
allows us to use `#[derive(Zeroize, ZeroizeOnDrop)]` on our structs.

---

## 3. Security findings — complete list with status

### 3.1 CRITICAL (must fix before any deployment)

#### F1: Memory zeroization
- Problem: TOTP secret stays in heap after use. Rust's Drop does not zero memory.
- Fix: Wrap secret_base32 in `Zeroizing<String>`. Implement `ZeroizeOnDrop` on `TotpSecret`.
  Use `secrecy::SecretString` for the base32 field.
- Research confirms: chacha20poly1305 already uses zeroize internally for key material.
  The totp-rs reference crate implements Zeroize on its TOTP struct as precedent.
- Status: WILL FIX in code.

#### F2: Recovery codes missing
- Problem: No recovery path if authenticator device is lost.
- Fix: Generate 10 codes at setup. Format: 8 alphanumeric chars (a-z0-9, no ambiguous chars like 0/O, 1/l).
  CSPRNG-generated. Stored encrypted alongside TOTP secret. Each code is consumed on use.
  Warn user when < 3 remaining. `zeroclaw auth totp recovery refresh` to regenerate all 10.
- Industry precedent: GitHub (10 codes, 8 chars), Google (10 codes, 8 digits), GitLab (10 codes).
- Status: WILL FIX in code.

#### F3: Audit log missing
- Problem: No record of TOTP verifications, gate decisions, or admin actions. GDPR Art. 32 requires demonstrable technical safeguards.
- Fix: SQLite table `totp_audit` with hash chain for tamper evidence.
  Append-only: application role has INSERT + SELECT only, no UPDATE/DELETE.
  Export: `zeroclaw audit export --format jsonl --from 2026-01 --to 2026-03`
  Retention: configurable, default 365 days. For legal domains: match profession's retention rules.
- Status: WILL FIX in code.

#### F4: Phishing / real-time proxy (architectural limitation)
- Problem: TOTP is not phishing-resistant. Evilginx-style AiTM attacks can relay codes in real time.
- Mitigation (v1): Document the limitation clearly. Recommend E2E-encrypted channels (Matrix) for TOTP prompts instead of Telegram Bot API (not E2E). Add `channel_security` field to config so users can mark channels as E2E or not, and the engine can warn if TOTP prompts go over a non-E2E channel.
- Mitigation (future): FIDO2/WebAuthn as `SecurityLevel::Fido2Required`.
- Status: DOCUMENTED in code + config.

#### F15: TOCTOU — Time-of-Check vs Time-of-Use
- Problem: Gate says "allowed" at time T. Between T and actual execution, the command could be
  modified (e.g. by prompt injection during wait time). Gate checked `sudo apt update`,
  agent executes `sudo rm -rf /`.
- Fix: Gate returns a `SignedDecision` that includes an HMAC of the checked command string.
  The execution engine verifies the HMAC before running the command. If the command was
  modified between gate and execution, the signature does not match and execution is blocked.
  HMAC key: derived via HKDF with info="totp-gate-signing" from the master .secret_key.
- Status: WILL FIX in code.

#### F16: Agent creates new tools/skills that bypass gating
- Problem: ZeroClaw's skill system allows the agent to autonomously create skills. A malicious
  or prompt-injected agent could create a skill that internally calls `db.delete` but is not
  covered by any gating pattern. The wrapper skill name does not match any rule.
- Fix: Two-part mitigation:
  1. Skill creation and skill installation are always `totp_required` (cannot be autonomous).
  2. The gate must evaluate the innermost tool calls recursively, not just the top-level
     wrapper. If a skill internally dispatches `db.delete`, the pattern matches there.
- Status: WILL FIX in code (skill-creation gating) + ARCHITECTURAL (recursive gate evaluation
  depends on ZeroClaw's skill dispatch implementation).

#### F17: Config downgrade attack
- Problem: Someone with config file access sets `enabled = false` or changes all levels to `none`.
  Entire gating system silently disabled.
- Fix: Config reload detects security downgrades by comparing new config to current running config.
  Detected downgrades:
    - `enabled` changed from true to false
    - Any rule level decreased (totp_and_confirm -> totp_required -> confirm -> none)
    - `max_attempts` increased significantly (e.g. 3 -> 999)
    - `lockout_seconds` decreased significantly (e.g. 300 -> 1)
    - User removed from registry while TOTP was active
  When a downgrade is detected: require admin TOTP confirmation before applying the change.
  Without TOTP confirmation: change is rejected, audit log entry `config_downgrade_rejected`.
- Status: WILL FIX in code.

### 3.2 HIGH (fix before first customer deployment)

#### F5: Secret in Debug output
- Problem: `#[derive(Debug)]` on `TotpSecret` leaks `secret_base32` in logs/crash dumps.
- Fix: Manual `impl fmt::Debug` that prints `secret_base32: [REDACTED]`.
- Status: WILL FIX in code.

#### F6: Rate limiting is per-account only
- Problem: Attacker can try 3 codes per account * N accounts without global throttle.
- Fix: Add `global_rate_limit_per_minute` config field. Sliding window counter (AtomicU32 + timestamp).
- Status: WILL FIX in code.

#### F7: No clock drift compensation
- Problem: RFC 6238 section 6 recommends tracking drift. Skew=1 handles +-30s, but
  systematic drift causes permanent failures.
- Fix: Track offset on each successful verify. If same offset N times consecutively (default 3),
  persist as `clock_drift_steps: i64` and apply to future verifications.
- Status: WILL FIX in code.

#### F8: Lockout not persistent
- Problem: Process restart resets the lockout counter.
- Fix: Persist `failed_attempts` and `lockout_until_timestamp` in the encrypted store.
  AtomicU32 as hot cache, synced to disk on each failure.
- Status: WILL FIX in code.

#### F13: Prompt injection — indirect execution via shell
- Problem: Agent is prompted to bypass gating by using shell commands instead of named tools.
  Example: instead of `akte.loeschen(id)`, agent writes a Python script that deletes the DB row directly.
- Fix: Shell execution itself is gated at `totp_required` level. Any shell tool call goes through
  the gate regardless of what the shell command contains. In practice, the shell tool is the
  highest-risk tool and should always require TOTP.
- Status: WILL FIX in config defaults (shell pattern = totp_required).

#### F14: Prompt injection — poisoned document triggers tool call
- Problem: A document the agent processes contains hidden instructions ("delete case file 42").
  Agent parses it as an instruction and calls `akte.loeschen`.
- Fix: The tool call goes through the gate. `akte.loeschen` matches the pattern, TOTP is required.
  The real user sees the TOTP prompt and knows they did not request this action.
  This is the core value of TOTP for AI agents: it is an out-of-band verification channel
  that prompt injection cannot control.
- Status: COVERED by existing gate design. No additional code needed.

#### F18: Intentional lockout (DoS against another user)
- Problem: Attacker knows a user's ID. Submits 3 wrong TOTP codes. User is locked out for 5 minutes.
  Repeatable indefinitely = permanent denial of service.
- Fix: Bind lockout to session, not to user_id. Failed attempts from session A do not lock out
  session B of the same user. Alternative: only count failures from authenticated sessions
  (the attacker would need to be logged in as the target user, which requires knowing the password).
- Status: WILL FIX in code.

#### F19: No notifications on critical events
- Problem: Everything is logged but nobody is actively notified. Break-glass at 3 AM, repeated
  TOTP failures, config downgrade attempts — admin/maintainer finds out days later.
- Fix: Notification hook in the audit module. Events with `severity: critical` trigger an alert
  via the configured channel. Config: `[security.totp.alerts]` with channel and severity filter.
  Critical events: break_glass, lockout_triggered, config_downgrade_attempted, recovery_codes_low,
  repeated_verify_fail (threshold configurable).
- Status: WILL FIX in code.

#### F20: Approval queue manipulation
- Problem: Agent queues a proposal at night: "delete 12 orphaned rows." Prompt injection modifies
  the proposal in the queue before the user sees it. User approves the manipulated proposal.
- Fix: Each approval queue entry gets an HMAC over the original command + timestamp + proposer.
  When the user approves, the HMAC is verified. If the command was modified between queuing
  and approval, verification fails and the approval is rejected. Same HMAC approach as F15.
- Status: WILL FIX in code.

#### F21: E-Stop must be TOTP-exempt
- Problem: ZeroClaw has an E-Stop (emergency shutdown). If the agent is doing something wrong
  autonomously, the kill switch must work immediately. An E-Stop that requires a 6-digit code
  is not an E-Stop.
- Fix: E-Stop is the ONE explicit exception to TOTP gating. It executes immediately if the
  session is authenticated (logged in) but does not require TOTP verification.
  In code: `if action == Action::EStop { execute_immediately(); }` before the gate check.
  The E-Stop itself is audit-logged with maximum severity.
- Status: WILL FIX in code.

### 3.3 MEDIUM (track as issues, implement before v2)

#### F9: Pattern matching is substring-based
- Problem: Bypassable via Unicode tricks, shell aliases, indirect execution.
- Fix (v2): Add optional `pattern_regex` field. Apply NFKC Unicode normalization before matching.
- Status: KNOWN, v2 roadmap.

#### F10: No secret rotation mechanism
- Problem: Once set up, TOTP secret is never rotated.
- Fix (v2): `zeroclaw auth totp rotate` command.
- Status: KNOWN, v2 roadmap.

#### F11: QR code visible in terminal
- Problem: Shoulder-surfing, screen recording, terminal scrollback can capture the secret.
- Fix: Print warning before showing QR. After verification succeeds, issue terminal clear.
- Status: WILL FIX in code (warning + clear).

#### F12: Encryption key not derived
- Problem: Using .secret_key directly means TOTP store and auth-profiles share the same key.
- Fix: HKDF-SHA256(secret_key, salt="zeroclaw-totp-v1", info="totp-store-encryption").
  Different info strings for different purposes.
- Status: WILL FIX in code.

#### F22: Audit log storage / rotation
- Problem: SQLite audit log grows unbounded. On constrained devices (Raspberry Pi, 16GB SD),
  this becomes a problem after months.
- Fix: Configurable `audit_retention_days` (default: 365). Entries older than the threshold are
  archived (exported to JSONL) then deleted. Deleted entries get a tombstone hash so the chain
  remains verifiable for the retained portion.
- Status: WILL FIX in code.

#### F23: Concurrent cron jobs on same data
- Problem: Two autonomous jobs run simultaneously, both writing to the same database.
- Fix: SQLite WAL mode (concurrent reads + serial writes). Already the default in ZeroClaw's
  rusqlite usage. For the TOTP store specifically: file lock (flock) serializes access.
- Status: COVERED by existing SQLite WAL + file lock design.

#### F24: Server migration / TOTP export
- Problem: User changes server. Encrypted TOTP store must be migrated. Without export, all users
  must re-enroll.
- Fix: `zeroclaw totp export --encrypted` exports the store in a portable format that can be
  imported on a new server with the same `.secret_key`. Key rotation on migration is recommended.
- Status: KNOWN, v2 roadmap.

---

## 4. Autonomy model — cron jobs and self-healing

### 4.1 Principle

Additive operations are autonomous. Destructive operations require a human.
The gate evaluates three contexts: Human, Cron, SelfHeal.

### 4.2 Autonomous operations (no TOTP, no human)

```
db.optimize          # VACUUM, REINDEX, ANALYZE
db.insert            # new rows
db.update            # modify existing rows (content, not schema)
db.label             # add/change tags, categories, metadata
db.sort              # reorder, reorganize
db.index_create      # add new indexes
db.backup_create     # create a backup (additive)
ai.self_learn        # update embeddings, retrain weights
ai.knowledge_update  # add new knowledge entries
ai.prompt_optimize   # improve system prompts
config.patch_minor   # non-security config tweaks (logging, cache)
bugfix.non_critical  # typo fixes, format corrections, retry logic
cron.schedule        # add new scheduled tasks
report.generate      # create reports, summaries
```

### 4.3 Never-autonomous operations (always need human)

```
db.delete            # any row deletion
db.drop              # table/index drop
db.truncate          # bulk delete
db.schema_alter      # column add/remove/rename
config.security.*    # any security config change
config.totp.*        # any TOTP config change
user.*               # any user management
bugfix.critical      # fixes touching auth, crypto, permissions
backup.restore       # overwrites current data
backup.delete        # removes safety net
cron.delete          # removes scheduled tasks
system.shutdown      # system power
system.update        # binary updates
skill.create         # new skill creation (F16)
skill.install        # skill installation (F16)
```

### 4.4 Approval queue for blocked autonomous operations

When the agent wants to perform a never-autonomous operation during Cron/SelfHeal context:
1. Agent creates a proposal: command, reason, urgency, timestamp.
2. Proposal is HMAC-signed (F20 mitigation).
3. Stored in `pending_approvals` SQLite table.
4. User sees proposals on next login (dashboard or channel notification).
5. User approves with TOTP or rejects.
6. On approval: HMAC verified, command executed, audit logged.
7. On rejection: proposal archived, audit logged.

Unknown operations (not in either list) default to `queue_for_approval`.

### 4.5 Config schema for autonomy

```toml
[security.totp.autonomy]
enabled = true
unknown_default = "queue_for_approval"   # or "block" for stricter setups
approval_expiry_hours = 72               # proposals older than this auto-expire

# Users can extend these lists in config
extra_autonomous_ops = ["custom.safe_op"]
extra_blocked_ops = ["custom.dangerous_op"]
```

### 4.6 Approval queue schema (SQLite)

```sql
CREATE TABLE IF NOT EXISTS pending_approvals (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at  TEXT    NOT NULL,   -- ISO 8601 UTC
    actor       TEXT    NOT NULL,   -- cron/job-name or selfheal/component
    command     TEXT    NOT NULL,   -- the proposed operation
    reason      TEXT,               -- why the agent wants to do this
    urgency     TEXT    NOT NULL,   -- low, medium, high
    status      TEXT    NOT NULL DEFAULT 'pending',  -- pending, approved, rejected, expired
    resolved_at TEXT,               -- when approved/rejected
    resolved_by TEXT,               -- user who approved/rejected
    hmac        TEXT    NOT NULL,   -- HMAC-SHA256 of (command|created_at|actor)
    detail      TEXT               -- JSON blob for extra context
);
```

---

## 5. Prompt injection analysis

### 5.1 Why TOTP is effective against prompt injection

The fundamental defense: prompt injection manipulates the agent (software).
TOTP verifies the human (hardware token in their hand). As long as the
gating decision runs in Rust (not in the LLM) and the TOTP input comes
from a separate physical device (authenticator app), a prompt injection
can make the agent REQUEST a dangerous command, but cannot SUPPLY the
TOTP code. The human must physically enter the code and sees what the
agent wants to execute.

### 5.2 Attack vectors analyzed

| Vector | Description | Mitigation | Status |
|--------|------------|------------|--------|
| V1: Indirect execution | Agent uses shell instead of named tool to bypass pattern | Shell itself is totp_required (F13) | COVERED |
| V2: Multi-step injection | Multiple innocent steps combine to form dangerous action | Gate checks final tool call, not intermediate steps | COVERED |
| V3: Data exfiltration via output | Agent leaks data in chat response (no tool call) | NOT a TOTP problem; requires output filtering on prompt level | OUT OF SCOPE |
| V4: Identity spoofing | Attacker claims to be admin in chat text | Identity resolved from session/channel ID, not chat content | COVERED |
| V5: Poisoned document | Document contains hidden instructions for agent | Tool call goes through gate, user sees TOTP prompt for action they didn't request (F14) | COVERED |
| V6: Skill creation bypass | Agent creates new skill that wraps destructive ops | Skill creation is totp_required (F16) | COVERED |
| V7: TOCTOU manipulation | Command changed between gate check and execution | HMAC-signed gate decisions (F15) | COVERED |
| V8: Queue manipulation | Queued proposal modified before approval | HMAC-signed proposals (F20) | COVERED |
| V9: Config injection | Agent modifies config.toml to disable TOTP | Config changes are totp_required; downgrades need admin TOTP (F17) | COVERED |

### 5.3 Residual prompt injection risks (not solvable by TOTP)

- Data exfiltration through chat output (V3): requires separate output guardrails.
- Agent that convincingly explains why a dangerous action is needed: user might approve
  the TOTP prompt based on the agent's (manipulated) reasoning. Mitigation: clear display
  of exactly WHAT will be executed, not just the agent's explanation of WHY.

---

## 6. Identity and authentication mapping

### 6.1 Identity field format

```toml
identity = "<type>:<identifier>"
```

Supported types:
- `pairing:<session_id>` — ZeroClaw's built-in pairing system (recommended)
- `os_user:<username>` — Unix/Linux username (strongest for local setups)
- `telegram:<user_id>` — Telegram numeric user ID
- `matrix:<mxid>` — Matrix user ID (@user:server)
- `web:<email_or_username>` — Web dashboard login
- `discord:<user_id>` — Discord numeric user ID
- `slack:<user_id>` — Slack user ID

### 6.2 Engine interface

The TOTP engine does not care how identity is established. It receives
a resolved user_id from ZeroClaw's session/channel layer:

```rust
fn resolve_user(session: &Session) -> Option<UserId>
```

Everything above (how sessions are created, how channels authenticate,
how pairing works) is ZeroClaw's responsibility, not the TOTP engine's.

### 6.3 Web UI specific requirements

When users connect via web subdomain:
- HTTPS required (Let's Encrypt / Caddy / nginx reverse proxy)
- Session cookie flags: `Secure`, `HttpOnly`, `SameSite=Strict`
- Session timeout: configurable, default 30 minutes idle
- Login flow: username + password -> TOTP prompt -> fully authenticated session
- TOTP-gated actions during session: code re-requested per action (not per login)
- Optional: session binding to IP + User-Agent (config: `session_binding = "ip+ua"`)

---

## 7. Module specification — final file list (14 files)

```
src/security/totp/
  mod.rs              -- module declarations + re-exports
  core.rs             -- RFC 6238 TOTP generation/verification
  types.rs            -- TotpSecret, RecoveryCode, UserTotpData, LockoutState (with Zeroize)
                         SignedDecision (HMAC-signed gate output)
  encrypted.rs        -- ChaCha20-Poly1305 encrypted store with HKDF key derivation
                         atomic writes (temp file + rename), file locking
  gating.rs           -- CommandGate: evaluate() returns SignedDecision
                         verify_code() with persistent lockout
                         context-aware (Human/Cron/SelfHeal)
                         recursive tool-call evaluation for skills
  config.rs           -- TOML schema: TotpConfig, GatingRule, SecurityLevel, RoleConfig,
                         AutonomyConfig, AlertConfig
  users.rs            -- UserRegistry: role resolution, inheritance, identity mapping
                         blocked_operations enforcement
  recovery.rs         -- Recovery code generation, validation, consumption
                         rate limiting on recovery attempts
  audit.rs            -- SQLite audit log: hash chain, append-only triggers,
                         notification hooks for critical events,
                         retention/rotation, JSONL export
  approval_queue.rs   -- Queued autonomous operations: HMAC-signed proposals,
                         approve/reject with TOTP, expiry, status tracking
  autonomy.rs         -- Cron/SelfHeal context: autonomous_ops vs never_autonomous
                         unknown_default handling, extra_ops from config
  cli.rs              -- CLI subcommands: setup, verify, status, disable, check,
                         recovery, audit, approve, config-validate
  emergency.rs        -- Break-glass: maintainer key verification
                         E-Stop: TOTP-exempt immediate kill (F21)
  validate.rs         -- Config validator: schema check, human-readable errors,
                         downgrade detection (F17), TOTP confirmation for downgrades
```

### 7.1 New dependencies to add to Cargo.toml

```toml
# In [dependencies]:
sha1 = "0.10"
data-encoding = "2.6"
hkdf = "0.12"
zeroize = { version = "1", features = ["derive"] }
secrecy = "0.10"

# In [features]:
totp = ["dep:sha1", "dep:data-encoding", "dep:hkdf", "dep:qrcode"]
```

### 7.2 Config TOML schema (complete)

```toml
[security.totp]
enabled = true
max_attempts = 3                     # per-session lockout threshold
lockout_seconds = 300                # lockout duration
global_rate_limit_per_minute = 10    # across all users
clock_drift_auto_compensate = true   # enable drift tracking
clock_drift_threshold = 3            # consecutive offsets to persist
totp_prompt_timeout_seconds = 120    # timeout for user to enter code

[security.totp.emergency]
recovery_codes_count = 10
recovery_code_length = 8
recovery_warn_threshold = 3
admin_reset_grace_hours = 24

[security.totp.maintainer]
enabled = true
key_path = "/etc/zeroclaw/maintainer.key"
audit_level = "critical"

[security.totp.alerts]
enabled = true
channel = "telegram:123456789"       # or "matrix:@admin:server"
severity_filter = "critical"         # critical, high, all
events = [
    "break_glass",
    "lockout_triggered",
    "config_downgrade_attempted",
    "recovery_codes_low",
    "repeated_verify_fail",
]

[security.totp.autonomy]
enabled = true
unknown_default = "queue_for_approval"
approval_expiry_hours = 72
extra_autonomous_ops = []
extra_blocked_ops = []

[security.totp.roles.<name>]
inherits = "<parent_role>"
blocked_operations = ["op1", "op2"]

[[security.totp.rules.<ruleset>]]
pattern = "<string>"
level = "none|confirm|totp_required|totp_and_confirm"
reason = "<human readable>"

[[security.users]]
id = "<unique_id>"
name = "<display_name>"
role = "<role_name>"
identity = "<type>:<identifier>"     # pairing, os_user, telegram, matrix, web, etc.
totp_status = "inactive|pending|active"
```

### 7.3 Audit log schema (SQLite)

```sql
CREATE TABLE IF NOT EXISTS totp_audit (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp   TEXT    NOT NULL,
    user_id     TEXT    NOT NULL,
    action      TEXT    NOT NULL,
    command     TEXT,
    decision    TEXT,
    context     TEXT,               -- human, cron, selfheal, break_glass
    severity    TEXT    NOT NULL,   -- info, warning, high, critical
    detail      TEXT,
    prev_hash   TEXT    NOT NULL,
    row_hash    TEXT    NOT NULL
);

CREATE TRIGGER IF NOT EXISTS totp_audit_no_update
    BEFORE UPDATE ON totp_audit
    BEGIN SELECT RAISE(ABORT, 'totp_audit is append-only'); END;

CREATE TRIGGER IF NOT EXISTS totp_audit_no_delete
    BEFORE DELETE ON totp_audit
    BEGIN SELECT RAISE(ABORT, 'totp_audit is append-only'); END;
```

### 7.4 Approval queue schema (SQLite)

```sql
CREATE TABLE IF NOT EXISTS pending_approvals (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at  TEXT    NOT NULL,
    actor       TEXT    NOT NULL,
    command     TEXT    NOT NULL,
    reason      TEXT,
    urgency     TEXT    NOT NULL,
    status      TEXT    NOT NULL DEFAULT 'pending',
    resolved_at TEXT,
    resolved_by TEXT,
    hmac        TEXT    NOT NULL,
    detail      TEXT
);
```

---

## 8. Threat model summary (updated)

| # | Threat | Mitigation | Residual risk |
|---|--------|-----------|---------------|
| T1 | Brute force TOTP (10^6 codes) | Per-session lockout (3 attempts) + global rate limit | Negligible |
| T2 | Code replay within 30s | last_used_step prevents reuse | Eliminated |
| T3 | Code replay after 30s | Code expires naturally | Eliminated |
| T4 | Secret in memory dump | Zeroize + SecretString | CPU register residuals |
| T5 | Secret on disk | ChaCha20-Poly1305 + HKDF-derived key + 0600 | Requires disk + .secret_key |
| T6 | Secret in terminal | Warning + clear + audit | Shoulder-surfing remains |
| T7 | Secret in Debug logs | Custom Debug: [REDACTED] | Eliminated |
| T8 | Real-time phishing (Evilginx) | Documented; E2E channel recommended | TOTP is fundamentally vulnerable |
| T9 | Admin compromise | Admin self-TOTP for admin ops | Requires credentials + device |
| T10 | All admins locked out | Break-glass with maintainer key | Requires SSH + maintainer key |
| T11 | All recovery exhausted | Physical maintainer key | Last resort |
| T12 | Audit log tampering | Hash chain + append-only + WAL | Detectable, not preventable with root |
| T13 | Config manipulation | Downgrade detection + TOTP confirmation (F17) | Detectable + blocked |
| T14 | Clock drift > 90s | Auto-compensation (D19) | Sudden jump still fails |
| T15 | Process crash resets lockout | Persistent lockout (F8) | Eliminated |
| T16 | Unicode pattern bypass | NFKC in v2, substring in v1 | Partial in v1 |
| T17 | TOCTOU command swap | HMAC-signed decisions (F15) | Eliminated |
| T18 | Skill creation bypass | Skill ops are totp_required (F16) | Eliminated |
| T19 | Config downgrade | Admin TOTP required for downgrades (F17) | Eliminated |
| T20 | Prompt injection via shell | Shell is totp_required (F13) | Covered |
| T21 | Poisoned document | Gate catches tool call, user sees prompt (F14) | Covered |
| T22 | Data exfil via chat output | OUT OF SCOPE for TOTP | Needs output guardrails |
| T23 | Identity spoofing in chat | Identity from session, not chat content | Eliminated |
| T24 | Lockout DoS by attacker | Session-bound lockout (F18) | Eliminated |
| T25 | Approval queue manipulation | HMAC-signed proposals (F20) | Eliminated |
| T26 | E-Stop blocked by TOTP | E-Stop is TOTP-exempt (F21) | By design |
| T27 | Autonomous destructive ops | Never-autonomous list + approval queue | Covered |
| T28 | Store corruption | Atomic writes + backup (D29) | Reduced |
| T29 | Concurrent store access | File lock / mutex (D30) | Eliminated |
| T30 | Critical event missed | Alert notifications (F19) | Covered |

---

## 9. Test plan (updated)

### 9.1 Unit tests (in each module)

#### core.rs (7 tests)
- RFC 6238 test vectors (T=59, T=1111111109, T=1234567890)
- Verify with skew accepts adjacent window
- Verify rejects wrong code
- Replay protection rejects reused step
- Generate and verify roundtrip

#### encrypted.rs (6 tests)
- Roundtrip encrypt/decrypt
- Wrong key fails decryption
- Empty store returns defaults
- Mark verified persists
- Remove secret works
- Atomic write survives simulated crash (write temp, verify rename)

#### gating.rs (12 tests)
- Safe commands pass (ls, cat, cargo build)
- Dangerous commands require TOTP (sudo, rm -rf)
- Destructive commands require TOTP+confirm (rm -rf /, mkfs)
- Case-insensitive matching
- Disabled config allows everything
- Lockout triggers after max_attempts
- Lockout persists across simulated restart
- Lockout auto-expires after lockout_seconds
- Lockout is session-bound (F18)
- Global rate limit blocks after threshold
- SignedDecision HMAC verification passes for unchanged command (F15)
- SignedDecision HMAC verification fails for modified command (F15)

#### config.rs (5 tests)
- TOML roundtrip serialization
- Partial config uses defaults
- User can override rules completely
- Invalid level string produces clear error
- SecurityLevel serde snake_case

#### validate.rs (4 tests)
- Valid config passes
- Typo in level produces human-readable error
- Missing required field produces clear message
- Security downgrade detected (F17)

#### users.rs (4 tests)
- Role inheritance resolves correctly
- Blocked operations deny before TOTP check
- Unknown role produces error
- Identity field correctly parsed (type:identifier)

#### recovery.rs (5 tests)
- Generated codes are correct length and charset
- Code works exactly once
- Used code rejected on second attempt
- All 10 codes are unique
- Recovery attempt counted toward lockout

#### audit.rs (4 tests)
- Hash chain integrity verifiable
- Tampered entry detected
- Export produces valid JSONL
- Notification triggered on critical event

#### autonomy.rs (5 tests)
- Autonomous operation allowed in Cron context
- Never-autonomous operation blocked in Cron context
- Unknown operation defaults to queue_for_approval
- Human context bypasses autonomy check (uses normal rules)
- Extra ops from config merged correctly

#### approval_queue.rs (5 tests)
- Proposal created with valid HMAC
- Approval with valid TOTP succeeds and HMAC verifies
- Modified proposal fails HMAC verification (F20)
- Expired proposal auto-transitions to expired status
- Rejected proposal archived with audit entry

#### emergency.rs (3 tests)
- E-Stop executes without TOTP (F21)
- Break-glass requires maintainer key
- Break-glass with wrong key rejected

### 9.2 Integration tests (8 tests)

- Full setup -> verify -> gate -> execute flow
- Multi-user: two users with different roles get different gate decisions
- Admin reset: admin verifies self, resets target, target re-enrolls
- Recovery code: setup -> "lose" device -> use recovery code -> re-enroll
- Config reload: change rules at runtime, verify new rules apply
- Config downgrade: attempt to disable TOTP, verify admin TOTP required
- Break-glass: maintainer can reset admin who is locked out
- Autonomous cron: additive op succeeds, destructive op queued for approval

Total: 60+ tests.

---

## 10. Open items — consciously deferred to v2

| Item | Reason for deferral |
|------|-------------------|
| FIDO2/WebAuthn | Requires browser/device integration, significant scope |
| Regex pattern matching | Substring is sufficient for structured tool calls in v1 |
| Secret rotation command | Disable + setup achieves the same result manually |
| Web Dashboard API (write) | Security risk without hardened auth on the API itself |
| Config file watcher (hot-reload) | Explicit reload command is safer and audit-logged |
| Shamir's Secret Sharing for maintainer key | Single key works for small deployments |
| Multi-device TOTP enrollment | Single device per user is simpler and more secure |
| Delegation mechanism (4-eyes) | Complex, needs separate design |
| TOTP prompt channel security indicator | Requires channel metadata enrichment |
| Server migration / TOTP export | Can be done manually via encrypted backup for now |
| Output guardrails (V3 data exfil) | Not a TOTP problem, separate security layer |
| Session binding (IP + User-Agent) | Nice-to-have hardening, not critical for v1 |

---

## 11. Research sources consulted

- RFC 6238 (IETF): TOTP algorithm specification
- RFC 4226 (IETF): HOTP base algorithm
- RFC 8439 (IETF): ChaCha20-Poly1305 AEAD construction
- NIST SP 800-63B: Digital Identity Guidelines (authenticator requirements)
- DSGVO/GDPR Article 32: Security of processing
- ENISA MFA Recommendations: 2FA for high-risk processing
- Evilginx/AiTM research (Kuba Gretzky): Real-time proxy attacks on TOTP
- Authgear "5 Common TOTP Mistakes" (2026): Implementation pitfalls
- Rust Foundation "Secure App Development with Rust's Memory Model": zeroize best practices
- RustCrypto ecosystem: chacha20poly1305, zeroize, secrecy, hkdf crate documentation
- totp-rs crate (docs.rs): Reference Rust TOTP implementation with Zeroize
- Microsoft AuthQuake disclosure (2024): Rate limiting vulnerabilities in TOTP
- SOC 2 Trust Services Criteria CC6.x: Access control and audit trail requirements
- Tamper-evident audit logging: hash chains, append-only patterns, SQLite triggers
- RustCrypto/KDFs (hkdf): HKDF key derivation documentation and examples
- Attest (Rust): Tamper-evident audit logging with cryptographic verification
