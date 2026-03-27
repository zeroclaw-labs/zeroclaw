# ZeroClaw TOTP — Deep Review & Research Document v2

Final architecture review before code generation.
Covers: consistency check, dependency audit, security
findings, design decisions, autonomy model, prompt
injection analysis, identity mapping, approval queue,
and complete module spec.

---

## 1. Consistency check — all decisions cross-referenced

### 1.1 Decisions made in this conversation

| # | Decision | Status | Notes |
| --- | --- | --- | --- |
| D1 | TOTP: RFC 6238, SHA1, 6dig, 30s, skew+-1 | FINAL | Max compat |
| D2 | ChaCha20-Poly1305 AEAD, fresh nonce | FINAL | Matches existing scheme |
| D3 | HKDF-SHA256 from .secret_key w/ context | FINAL | Prevents key reuse |
| D4 | Replay: last_used_step persisted/user | FINAL | RFC 6238 s5.2 |
| D5 | Memory zero: zeroize + secrecy crates | FINAL | Prevents mem leaks |
| D6 | Recovery: 10 codes, 8 alphanum, CSPRNG | FINAL | Industry standard |
| D7 | Audit: ZeroClaw SQLite (rusqlite) | FINAL | Queryable, exportable |
| D8 | Audit integrity: hash chain per entry | FINAL | Tamper-evident |
| D9 | Config reload: explicit CLI command | FINAL | Audit-logged |
| D10 | Dashboard: CLI+TOML v1, API later | FINAL | Security-first |
| D11 | Multi-user: per-user secrets, roles | FINAL | Role inheritance |
| D12 | Admin ops: require admin's own TOTP | FINAL | Self-verification |
| D13 | Emergency: 3 paths (recovery/admin/BG) | FINAL | Break-glass |
| D14 | Maintainer key: separate, on their machine | FINAL | Isolated key |
| D15 | Rules/roles: 100% TOML, domain-agnostic | FINAL | User-defined |
| D16 | Config validation: serde + error wrapper | FINAL | Human-readable |
| D17 | Lockout: persistent, auto-expiry | FINAL | Survives restart |
| D18 | Debug: secret_base32 as [REDACTED] | FINAL | Custom Debug |
| D19 | Clock drift: track+persist compensation | FINAL | RFC 6238 s6 |
| D20 | Global rate limit: configurable per minute | FINAL | All users |
| D21 | Pattern: substring v1, regex v2 | FINAL | regex in deps |
| D22 | Phishing: documented, FIDO2 future | FINAL | Architectural |
| D23 | Autonomy: additive=auto, destructive=human | FINAL | Cron/SelfHeal |
| D24 | Approval queue: HMAC-signed proposals | FINAL | Anti-manipulation |
| D25 | Identity: generic field, multi-source | FINAL | Source-agnostic |
| D26 | Web auth: login+pw first, TOTP second | FINAL | Secure cookie |
| D27 | E-Stop: TOTP-exempt, immediate | FINAL | Kill switch |
| D28 | TOTP timeout: configurable, default 120s | FINAL | Reject on expiry |
| D29 | Store: write-to-temp-then-rename + backup | FINAL | Atomic writes |
| D30 | Concurrency: flock or parking_lot::Mutex | FINAL | No races |
| D31 | Gate signing: HMAC prevents TOCTOU | FINAL | Cmd integrity |
| D32 | Skill create/install: always totp_required | FINAL | No bypass tools |
| D33 | Config downgrade: needs admin TOTP | FINAL | No silent reduce |
| D34 | Notifications: critical events alert admin | FINAL | Break-glass etc |
| D35 | Lockout: session-based, not user-based | FINAL | Prevents DoS |

### 1.2 Contradictions found and resolved

| Issue | Where | Resolution |
| --- | --- | --- |
| ChaCha20Poly1305 mismatch | ZC uses 0.10, latest 0.11-rc | Use existing 0.10 |
| rand version | ZC: 0.10, prototype: 0.8 | Update to 0.10 |
| sha2 vs sha1 | ZC has sha2 0.10, need sha1 | Add sha1, compatible |
| qrcode optional vs required | ZC: optional (whatsapp) | New `totp` flag |
| Lockout in-mem vs persistent | Prototype was in-mem | Persist in enc store |
| Single nonce space | 96-bit random per write | Safe at <1 write/s |
| Lockout DoS | Original: per user_id | D35: per session |

---

## 2. Dependency audit — what we add to ZeroClaw

### 2.1 New dependencies (not already in Cargo.toml)

| Crate | Ver | Purpose | Size | Audited? |
| --- | --- | --- | --- | --- |
| sha1 | 0.10 | HMAC-SHA1 for TOTP | ~15KB | RustCrypto |
| data-encoding | 2.6 | Base32 encode/decode | ~30KB | 38M dl, no unsafe |
| hkdf | 0.12 | Key derivation | ~5KB | Pure Rust |
| zeroize | 1.x | Secure memory clear | ~10KB | Transitive dep |
| secrecy | 0.10 | Prevent secret realloc | ~8KB | RustCrypto |

### 2.2 Already in ZeroClaw (reused, no additions)

chacha20poly1305 0.10, hmac 0.12, sha2 0.10,
rand 0.10, serde 1.0, serde_json 1.0, toml 1.0,
chrono 0.4, clap 4.5, dialoguer 0.12, anyhow 1.0,
thiserror 2.0, tracing 0.1, parking_lot 0.12,
rusqlite (bundled), qrcode 0.14 (optional),
regex 1.10.

### 2.3 Transitive dependency note

zeroize is already pulled in transitively by
chacha20poly1305. Making it a direct dependency pins
the version explicitly and allows us to use
`#[derive(Zeroize, ZeroizeOnDrop)]` on our structs.

---

## 3. Security findings — complete list with status

### 3.1 CRITICAL (must fix before any deployment)

#### F1: Memory zeroization

- Problem: TOTP secret stays in heap after use.
  Rust's Drop does not zero memory.
- Fix: Wrap secret_base32 in `Zeroizing<String>`.
  Implement `ZeroizeOnDrop` on `TotpSecret`.
  Use `secrecy::SecretString` for the base32 field.
- Research confirms: chacha20poly1305 already uses
  zeroize internally for key material. The totp-rs
  reference crate implements Zeroize on its TOTP
  struct as precedent.
- Status: WILL FIX in code.

#### F2: Recovery codes missing

- Problem: No recovery path if authenticator device
  is lost.
- Fix: Generate 10 codes at setup. Format: 8
  alphanumeric chars (a-z0-9, no ambiguous chars
  like 0/O, 1/l). CSPRNG-generated. Stored encrypted
  alongside TOTP secret. Each code is consumed on use.
  Warn user when < 3 remaining.
  `zeroclaw auth totp recovery refresh` to regenerate
  all 10.
- Industry precedent: GitHub (10 codes, 8 chars),
  Google (10 codes, 8 digits), GitLab (10 codes).
- Status: WILL FIX in code.

#### F3: Audit log missing

- Problem: No record of TOTP verifications, gate
  decisions, or admin actions. GDPR Art. 32 requires
  demonstrable technical safeguards.
- Fix: SQLite table `totp_audit` with hash chain for
  tamper evidence. Append-only: application role has
  INSERT + SELECT only, no UPDATE/DELETE.
  Export:
  `zeroclaw audit export --format jsonl --from 2026-01`
  Retention: configurable, default 365 days. For legal
  domains: match profession's retention rules.
- Status: WILL FIX in code.

#### F4: Phishing / real-time proxy (architectural)

- Problem: TOTP is not phishing-resistant.
  Evilginx-style AiTM attacks can relay codes in
  real time.
- Mitigation (v1): Document the limitation clearly.
  Recommend E2E-encrypted channels (Matrix) for TOTP
  prompts instead of Telegram Bot API (not E2E). Add
  `channel_security` field to config so users can mark
  channels as E2E or not, and the engine can warn if
  TOTP prompts go over a non-E2E channel.
- Mitigation (future): FIDO2/WebAuthn as
  `SecurityLevel::Fido2Required`.
- Status: DOCUMENTED in code + config.

#### F15: TOCTOU — Time-of-Check vs Time-of-Use

- Problem: Gate says "allowed" at time T. Between T
  and actual execution, the command could be modified
  (e.g. by prompt injection during wait time). Gate
  checked `sudo apt update`, agent executes
  `sudo rm -rf /`.
- Fix: Gate returns a `SignedDecision` that includes
  an HMAC of the checked command string. The execution
  engine verifies the HMAC before running the command.
  If the command was modified between gate and
  execution, the signature does not match and execution
  is blocked. HMAC key: derived via HKDF with
  info="totp-gate-signing" from the master .secret_key.
- Status: WILL FIX in code.

#### F16: Agent creates new tools/skills that bypass gating

- Problem: ZeroClaw's skill system allows the agent
  to autonomously create skills. A malicious or
  prompt-injected agent could create a skill that
  internally calls `db.delete` but is not covered by
  any gating pattern. The wrapper skill name does not
  match any rule.
- Fix: Two-part mitigation:
  1. Skill creation and skill installation are always
     `totp_required` (cannot be autonomous).
  2. The gate must evaluate the innermost tool calls
     recursively, not just the top-level wrapper. If a
     skill internally dispatches `db.delete`, the
     pattern matches there.
- Status: WILL FIX in code (skill-creation gating) +
  ARCHITECTURAL (recursive gate evaluation depends on
  ZeroClaw's skill dispatch implementation).

#### F17: Config downgrade attack

- Problem: Someone with config file access sets
  `enabled = false` or changes all levels to `none`.
  Entire gating system silently disabled.
- Fix: Config reload detects security downgrades by
  comparing new config to current running config.
  Detected downgrades:
  - `enabled` changed from true to false
  - Any rule level decreased
    (totp_and_confirm -> totp_required -> confirm)
  - `max_attempts` increased significantly
    (e.g. 3 -> 999)
  - `lockout_seconds` decreased significantly
    (e.g. 300 -> 1)
  - User removed from registry while TOTP was active

- When a downgrade is detected: require admin TOTP
  confirmation before applying the change. Without
  TOTP confirmation: change is rejected, audit log
  entry `config_downgrade_rejected`.
- Status: WILL FIX in code.

### 3.2 HIGH (fix before first customer deployment)

#### F5: Secret in Debug output

- Problem: `#[derive(Debug)]` on `TotpSecret` leaks
  `secret_base32` in logs/crash dumps.
- Fix: Manual `impl fmt::Debug` that prints
  `secret_base32: [REDACTED]`.
- Status: WILL FIX in code.

#### F6: Rate limiting is per-account only

- Problem: Attacker can try 3 codes per account * N
  accounts without global throttle.
- Fix: Add `global_rate_limit_per_minute` config
  field. Sliding window counter
  (AtomicU32 + timestamp).
- Status: WILL FIX in code.

#### F7: No clock drift compensation

- Problem: RFC 6238 section 6 recommends tracking
  drift. Skew=1 handles +-30s, but systematic drift
  causes permanent failures.
- Fix: Track offset on each successful verify. If
  same offset N times consecutively (default 3),
  persist as `clock_drift_steps: i64` and apply to
  future verifications.
- Status: WILL FIX in code.

#### F8: Lockout not persistent

- Problem: Process restart resets the lockout counter.
- Fix: Persist `failed_attempts` and
  `lockout_until_timestamp` in the encrypted store.
  AtomicU32 as hot cache, synced to disk on each
  failure.
- Status: WILL FIX in code.

#### F13: Prompt injection — indirect execution via shell

- Problem: Agent is prompted to bypass gating by
  using shell commands instead of named tools.
  Example: instead of `akte.loeschen(id)`, agent
  writes a Python script that deletes the DB row
  directly.
- Fix: Shell execution itself is gated at
  `totp_required` level. Any shell tool call goes
  through the gate regardless of what the shell
  command contains. In practice, the shell tool is
  the highest-risk tool and should always require
  TOTP.
- Status: WILL FIX in config defaults
  (shell pattern = totp_required).

#### F14: Prompt injection — poisoned document triggers tool call

- Problem: A document the agent processes contains
  hidden instructions ("delete case file 42"). Agent
  parses it as an instruction and calls
  `akte.loeschen`.
- Fix: The tool call goes through the gate.
  `akte.loeschen` matches the pattern, TOTP is
  required. The real user sees the TOTP prompt and
  knows they did not request this action. This is the
  core value of TOTP for AI agents: it is an
  out-of-band verification channel that prompt
  injection cannot control.
- Status: COVERED by existing gate design. No
  additional code needed.

#### F18: Intentional lockout (DoS against another user)

- Problem: Attacker knows a user's ID. Submits 3
  wrong TOTP codes. User is locked out for 5 minutes.
  Repeatable indefinitely = permanent denial of
  service.
- Fix: Bind lockout to session, not to user_id.
  Failed attempts from session A do not lock out
  session B of the same user. Alternative: only count
  failures from authenticated sessions (the attacker
  would need to be logged in as the target user,
  which requires knowing the password).
- Status: WILL FIX in code.

#### F19: No notifications on critical events

- Problem: Everything is logged but nobody is actively
  notified. Break-glass at 3 AM, repeated TOTP
  failures, config downgrade attempts —
  admin/maintainer finds out days later.
- Fix: Notification hook in the audit module. Events
  with `severity: critical` trigger an alert via the
  configured channel. Config:
  `[security.totp.alerts]` with channel and severity
  filter. Critical events: break_glass,
  lockout_triggered, config_downgrade_attempted,
  recovery_codes_low, repeated_verify_fail (threshold
  configurable).
- Status: WILL FIX in code.

#### F20: Approval queue manipulation

- Problem: Agent queues a proposal at night: "delete
  12 orphaned rows." Prompt injection modifies the
  proposal in the queue before the user sees it.
  User approves the manipulated proposal.
- Fix: Each approval queue entry gets an HMAC over
  the original command + timestamp + proposer. When
  the user approves, the HMAC is verified. If the
  command was modified between queuing and approval,
  verification fails and the approval is rejected.
  Same HMAC approach as F15.
- Status: WILL FIX in code.

#### F21: E-Stop must be TOTP-exempt

- Problem: ZeroClaw has an E-Stop (emergency
  shutdown). If the agent is doing something wrong
  autonomously, the kill switch must work immediately.
  An E-Stop that requires a 6-digit code is not an
  E-Stop.
- Fix: E-Stop is the ONE explicit exception to TOTP
  gating. It executes immediately if the session is
  authenticated (logged in) but does not require TOTP
  verification. In code:
  `if action == Action::EStop { execute_immediately() }`
  before the gate check. The E-Stop itself is
  audit-logged with maximum severity.
- Status: WILL FIX in code.

### 3.3 MEDIUM (track as issues, implement before v2)

#### F9: Pattern matching is substring-based

- Problem: Bypassable via Unicode tricks, shell
  aliases, indirect execution.
- Fix (v2): Add optional `pattern_regex` field.
  Apply NFKC Unicode normalization before matching.
- Status: KNOWN, v2 roadmap.

#### F10: No secret rotation mechanism

- Problem: Once set up, TOTP secret is never rotated.
- Fix (v2): `zeroclaw auth totp rotate` command.
- Status: KNOWN, v2 roadmap.

#### F11: QR code visible in terminal

- Problem: Shoulder-surfing, screen recording,
  terminal scrollback can capture the secret.
- Fix: Print warning before showing QR. After
  verification succeeds, issue terminal clear.
- Status: WILL FIX in code (warning + clear).

#### F12: Encryption key not derived

- Problem: Using .secret_key directly means TOTP
  store and auth-profiles share the same key.
- Fix: HKDF-SHA256(secret_key,
  salt="zeroclaw-totp-v1",
  info="totp-store-encryption"). Different info
  strings for different purposes.
- Status: WILL FIX in code.

#### F22: Audit log storage / rotation

- Problem: SQLite audit log grows unbounded. On
  constrained devices (Raspberry Pi, 16GB SD), this
  becomes a problem after months.
- Fix: Configurable `audit_retention_days`
  (default: 365). Entries older than the threshold
  are archived (exported to JSONL) then deleted.
  Deleted entries get a tombstone hash so the chain
  remains verifiable for the retained portion.
- Status: WILL FIX in code.

#### F23: Concurrent cron jobs on same data

- Problem: Two autonomous jobs run simultaneously,
  both writing to the same database.
- Fix: SQLite WAL mode (concurrent reads + serial
  writes). Already the default in ZeroClaw's rusqlite
  usage. For the TOTP store specifically: file lock
  (flock) serializes access.
- Status: COVERED by existing SQLite WAL + file lock
  design.

#### F24: Server migration / TOTP export

- Problem: User changes server. Encrypted TOTP store
  must be migrated. Without export, all users must
  re-enroll.
- Fix: `zeroclaw totp export --encrypted` exports the
  store in a portable format that can be imported on a
  new server with the same `.secret_key`. Key rotation
  on migration is recommended.
- Status: KNOWN, v2 roadmap.

---

## 4. Autonomy model — cron jobs and self-healing

### 4.1 Principle

Additive operations are autonomous. Destructive
operations require a human. The gate evaluates three
contexts: Human, Cron, SelfHeal.

### 4.2 Autonomous operations (no TOTP, no human)

```text
db.optimize          # VACUUM, REINDEX, ANALYZE
db.insert            # new rows
db.update            # modify existing rows
db.label             # add/change tags, metadata
db.sort              # reorder, reorganize
db.index_create      # add new indexes
db.backup_create     # create a backup (additive)
ai.self_learn        # update embeddings
ai.knowledge_update  # add new knowledge entries
ai.prompt_optimize   # improve system prompts
config.patch_minor   # non-security config tweaks
bugfix.non_critical  # typo fixes, format, retry
cron.schedule        # add new scheduled tasks
report.generate      # create reports, summaries
```

### 4.3 Never-autonomous operations (always need human)

```text
db.delete            # any row deletion
db.drop              # table/index drop
db.truncate          # bulk delete
db.schema_alter      # column add/remove/rename
config.security.*    # any security config change
config.totp.*        # any TOTP config change
user.*               # any user management
bugfix.critical      # fixes touching auth, crypto
backup.restore       # overwrites current data
backup.delete        # removes safety net
cron.delete          # removes scheduled tasks
system.shutdown      # system power
system.update        # binary updates
skill.create         # new skill creation (F16)
skill.install        # skill installation (F16)
```

### 4.4 Approval queue for blocked autonomous operations

When the agent wants to perform a never-autonomous
operation during Cron/SelfHeal context:

1. Agent creates a proposal: command, reason,
   urgency, timestamp.
2. Proposal is HMAC-signed (F20 mitigation).
3. Stored in `pending_approvals` SQLite table.
4. User sees proposals on next login (dashboard or
   channel notification).
5. User approves with TOTP or rejects.
6. On approval: HMAC verified, command executed,
   audit logged.
7. On rejection: proposal archived, audit logged.

Unknown operations (not in either list) default to
`queue_for_approval`.

### 4.5 Config schema for autonomy

```toml
[security.totp.autonomy]
enabled = true
unknown_default = "queue_for_approval"
approval_expiry_hours = 72

# Users can extend these lists in config
extra_autonomous_ops = ["custom.safe_op"]
extra_blocked_ops = ["custom.dangerous_op"]
```

### 4.6 Approval queue schema (SQLite)

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

## 5. Prompt injection analysis

### 5.1 Why TOTP is effective against prompt injection

The fundamental defense: prompt injection manipulates
the agent (software). TOTP verifies the human
(hardware token in their hand). As long as the gating
decision runs in Rust (not in the LLM) and the TOTP
input comes from a separate physical device
(authenticator app), a prompt injection can make the
agent REQUEST a dangerous command, but cannot SUPPLY
the TOTP code. The human must physically enter the
code and sees what the agent wants to execute.

### 5.2 Attack vectors analyzed

| Vec | Description | Mitigation | Status |
| --- | --- | --- | --- |
| V1 | Indirect exec via shell | Shell is totp_required (F13) | COVERED |
| V2 | Multi-step injection | Gate checks final tool call | COVERED |
| V3 | Data exfil via output | Not a TOTP problem | OUT OF SCOPE |
| V4 | Identity spoofing | ID from session, not chat | COVERED |
| V5 | Poisoned document | Gate catches tool call (F14) | COVERED |
| V6 | Skill creation bypass | Skill create is gated (F16) | COVERED |
| V7 | TOCTOU manipulation | HMAC-signed decisions (F15) | COVERED |
| V8 | Queue manipulation | HMAC-signed proposals (F20) | COVERED |
| V9 | Config injection | Config changes gated (F17) | COVERED |

### 5.3 Residual prompt injection risks (not solvable by TOTP)

- Data exfiltration through chat output (V3):
  requires separate output guardrails.
- Agent that convincingly explains why a dangerous
  action is needed: user might approve the TOTP
  prompt based on the agent's (manipulated) reasoning.
  Mitigation: clear display of exactly WHAT will be
  executed, not just the agent's explanation of WHY.

---

## 6. Identity and authentication mapping

### 6.1 Identity field format

```toml
identity = "<type>:<identifier>"
```

Supported types:

- `pairing:<session_id>` — ZeroClaw's built-in
  pairing system (recommended)
- `os_user:<username>` — Unix/Linux username
  (strongest for local setups)
- `telegram:<user_id>` — Telegram numeric user ID
- `matrix:<mxid>` — Matrix user ID (@user:server)
- `web:<email_or_username>` — Web dashboard login
- `discord:<user_id>` — Discord numeric user ID
- `slack:<user_id>` — Slack user ID

### 6.2 Engine interface

The TOTP engine does not care how identity is
established. It receives a resolved user_id from
ZeroClaw's session/channel layer:

```rust
fn resolve_user(session: &Session) -> Option<UserId>
```

Everything above (how sessions are created, how
channels authenticate, how pairing works) is
ZeroClaw's responsibility, not the TOTP engine's.

### 6.3 Web UI specific requirements

When users connect via web subdomain:

- HTTPS required (Let's Encrypt / Caddy / nginx)
- Session cookie flags: `Secure`, `HttpOnly`,
  `SameSite=Strict`
- Session timeout: configurable, default 30 min idle
- Login flow: username + password -> TOTP prompt ->
  fully authenticated session
- TOTP-gated actions during session: code
  re-requested per action (not per login)
- Optional: session binding to IP + User-Agent
  (config: `session_binding = "ip+ua"`)

---

## 7. Module specification — final file list (14 files)

```text
src/security/totp/
  mod.rs           -- module declarations + re-exports
  core.rs          -- RFC 6238 TOTP generation/verify
  types.rs         -- TotpSecret, RecoveryCode,
                      UserTotpData, LockoutState,
                      SignedDecision
  encrypted.rs     -- ChaCha20-Poly1305 store, HKDF,
                      atomic writes, file locking
  gating.rs        -- CommandGate: evaluate(),
                      verify_code(), lockout,
                      context-aware, recursive eval
  config.rs        -- TOML schema: TotpConfig,
                      GatingRule, SecurityLevel,
                      RoleConfig, AutonomyConfig
  users.rs         -- UserRegistry: role resolution,
                      inheritance, identity mapping
  recovery.rs      -- Recovery code gen, validate,
                      consume, rate limiting
  audit.rs         -- SQLite audit log: hash chain,
                      append-only, notifications,
                      retention, JSONL export
  approval_queue.rs -- Queued autonomous ops: HMAC,
                      approve/reject, expiry
  autonomy.rs      -- Cron/SelfHeal context,
                      auto vs never-auto, config
  cli.rs           -- CLI subcommands: setup, verify,
                      status, disable, check, etc.
  emergency.rs     -- Break-glass: maintainer key,
                      E-Stop: TOTP-exempt (F21)
  validate.rs      -- Config validator: schema check,
                      downgrade detection (F17)
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
totp = [
    "dep:sha1",
    "dep:data-encoding",
    "dep:hkdf",
    "dep:qrcode",
]
```

### 7.2 Config TOML schema (complete)

```toml
[security.totp]
enabled = true
max_attempts = 3
lockout_seconds = 300
global_rate_limit_per_minute = 10
clock_drift_auto_compensate = true
clock_drift_threshold = 3
totp_prompt_timeout_seconds = 120

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
channel = "telegram:123456789"
severity_filter = "critical"
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
level = "none|confirm|totp_required"
reason = "<human readable>"

[[security.users]]
id = "<unique_id>"
name = "<display_name>"
role = "<role_name>"
identity = "<type>:<identifier>"
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
    context     TEXT,
    severity    TEXT    NOT NULL,
    detail      TEXT,
    prev_hash   TEXT    NOT NULL,
    row_hash    TEXT    NOT NULL
);

CREATE TRIGGER IF NOT EXISTS totp_audit_no_update
    BEFORE UPDATE ON totp_audit
    BEGIN
        SELECT RAISE(ABORT, 'totp_audit is append-only');
    END;

CREATE TRIGGER IF NOT EXISTS totp_audit_no_delete
    BEFORE DELETE ON totp_audit
    BEGIN
        SELECT RAISE(ABORT, 'totp_audit is append-only');
    END;
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

| # | Threat | Mitigation | Residual |
| --- | --- | --- | --- |
| T1 | Brute force TOTP | Lockout + global limit | Negligible |
| T2 | Replay within 30s | last_used_step | Eliminated |
| T3 | Replay after 30s | Code expires | Eliminated |
| T4 | Secret in mem dump | Zeroize + SecretString | CPU regs |
| T5 | Secret on disk | ChaCha20 + HKDF + 0600 | Need both keys |
| T6 | Secret in terminal | Warning + clear + audit | Shoulder-surf |
| T7 | Secret in Debug | Custom: [REDACTED] | Eliminated |
| T8 | Phishing (Evilginx) | Documented; E2E channel | Fundamental |
| T9 | Admin compromise | Admin self-TOTP | Need creds+dev |
| T10 | All admins locked | Break-glass + maint key | SSH + key |
| T11 | All recovery gone | Physical maintainer key | Last resort |
| T12 | Audit tampering | Hash chain + append-only | Detect only |
| T13 | Config manipulation | Downgrade detect (F17) | Blocked |
| T14 | Clock drift > 90s | Auto-compensate (D19) | Sudden jump |
| T15 | Crash resets lockout | Persistent lockout (F8) | Eliminated |
| T16 | Unicode bypass | NFKC in v2, substr in v1 | Partial v1 |
| T17 | TOCTOU cmd swap | HMAC decisions (F15) | Eliminated |
| T18 | Skill bypass | Skill ops gated (F16) | Eliminated |
| T19 | Config downgrade | Admin TOTP needed (F17) | Eliminated |
| T20 | Injection via shell | Shell is gated (F13) | Covered |
| T21 | Poisoned document | Gate catches call (F14) | Covered |
| T22 | Data exfil via chat | OUT OF SCOPE for TOTP | Need guardrails |
| T23 | Identity spoofing | ID from session | Eliminated |
| T24 | Lockout DoS | Session-bound (F18) | Eliminated |
| T25 | Queue manipulation | HMAC proposals (F20) | Eliminated |
| T26 | E-Stop blocked | TOTP-exempt (F21) | By design |
| T27 | Auto destructive ops | Never-auto + queue | Covered |
| T28 | Store corruption | Atomic writes (D29) | Reduced |
| T29 | Concurrent access | File lock/mutex (D30) | Eliminated |
| T30 | Critical event miss | Alert notifs (F19) | Covered |

---

## 9. Test plan (updated)

### 9.1 Unit tests (in each module)

#### core.rs (7 tests)

- RFC 6238 test vectors
  (T=59, T=1111111109, T=1234567890)
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
- Atomic write survives simulated crash
  (write temp, verify rename)

#### gating.rs (12 tests)

- Safe commands pass (ls, cat, cargo build)
- Dangerous commands require TOTP (sudo, rm -rf)
- Destructive commands require TOTP+confirm
  (rm -rf /, mkfs)
- Case-insensitive matching
- Disabled config allows everything
- Lockout triggers after max_attempts
- Lockout persists across simulated restart
- Lockout auto-expires after lockout_seconds
- Lockout is session-bound (F18)
- Global rate limit blocks after threshold
- SignedDecision HMAC passes for unchanged cmd (F15)
- SignedDecision HMAC fails for modified cmd (F15)

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
- Human context bypasses autonomy check
  (uses normal rules)
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
- Multi-user: two users with different roles get
  different gate decisions
- Admin reset: admin verifies self, resets target,
  target re-enrolls
- Recovery code: setup -> "lose" device -> use
  recovery code -> re-enroll
- Config reload: change rules at runtime, verify
  new rules apply
- Config downgrade: attempt to disable TOTP, verify
  admin TOTP required
- Break-glass: maintainer can reset admin who is
  locked out
- Autonomous cron: additive op succeeds, destructive
  op queued for approval

Total: 60+ tests.

---

## 10. Open items — consciously deferred to v2

| Item | Reason for deferral |
| --- | --- |
| FIDO2/WebAuthn | Requires browser/device integration |
| Regex pattern matching | Substring sufficient for v1 |
| Secret rotation command | Disable+setup works manually |
| Web Dashboard API (write) | Security risk without auth |
| Config file watcher | Explicit reload is safer |
| Shamir's Secret Sharing | Single key works for small |
| Multi-device TOTP | Single device is simpler |
| Delegation (4-eyes) | Complex, needs separate design |
| TOTP channel security | Needs channel metadata |
| Server migration/export | Manual backup for now |
| Output guardrails (V3) | Not a TOTP problem |
| Session binding (IP+UA) | Nice-to-have, not critical |

---

## 11. Research sources consulted

- RFC 6238 (IETF): TOTP algorithm specification
- RFC 4226 (IETF): HOTP base algorithm
- RFC 8439 (IETF): ChaCha20-Poly1305 AEAD
- NIST SP 800-63B: Digital Identity Guidelines
- DSGVO/GDPR Article 32: Security of processing
- ENISA MFA Recommendations: 2FA for high-risk
- Evilginx/AiTM research (Kuba Gretzky): Real-time
  proxy attacks on TOTP
- Authgear "5 Common TOTP Mistakes" (2026):
  Implementation pitfalls
- Rust Foundation "Secure App Development with
  Rust's Memory Model": zeroize best practices
- RustCrypto ecosystem: chacha20poly1305, zeroize,
  secrecy, hkdf crate documentation
- totp-rs crate (docs.rs): Reference Rust TOTP
  implementation with Zeroize
- Microsoft AuthQuake disclosure (2024): Rate
  limiting vulnerabilities in TOTP
- SOC 2 Trust Services Criteria CC6.x: Access
  control and audit trail requirements
- Tamper-evident audit logging: hash chains,
  append-only patterns, SQLite triggers
- RustCrypto/KDFs (hkdf): HKDF key derivation
  documentation and examples
- Attest (Rust): Tamper-evident audit logging with
  cryptographic verification
