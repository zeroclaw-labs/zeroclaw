# AGENTS.md — security/

> Policy enforcement, containment, secrets, and audit. **CRITICAL RISK** subsystem.
> Every change here can silently disable protection for the entire runtime.

## Overview

This subsystem enforces all access-control, isolation, and credential-protection guarantees in ZeroClaw. The agent loop, tool execution, and gateway all depend on types exported from here. There is no "bypass for testing" — `NoopSandbox` is the explicit fallback and is logged.

## Key Files

| File | Role |
|---|---|
| `policy.rs` | `SecurityPolicy` + `AutonomyLevel` + `ActionTracker` — gate for every tool call |
| `traits.rs` | `Sandbox` trait — OS-level isolation interface (`wrap_command`, `is_available`) |
| `detect.rs` | `create_sandbox` — runtime auto-detection: Landlock > Firejail > Bubblewrap > Docker > Noop |
| `pairing.rs` | `PairingGuard` — one-time code + SHA-256 hashed bearer tokens, brute-force lockout |
| `secrets.rs` | `SecretStore` — ChaCha20-Poly1305 AEAD encryption, legacy XOR migration path |
| `audit.rs` | `AuditLogger` — Merkle-chained (SHA-256) tamper-evident event log |
| `estop.rs` | `EstopManager` — emergency stop: kill-all, network-kill, domain-block, tool-freeze |
| `prompt_guard.rs` | `PromptGuard` — prompt injection detection (role confusion, tool-call injection, jailbreaks) |
| `leak_detector.rs` | `LeakDetector` — outbound credential leak scanning (regex + entropy) |
| `workspace_boundary.rs` | `WorkspaceBoundary` — per-workspace tool/domain allowlists |
| `iam_policy.rs` | `IamPolicy` — Nevis role-to-permission mapping, deny-by-default |
| `otp.rs` | `OtpValidator` — TOTP validation for e-stop resume |
| `domain_matcher.rs` | `DomainMatcher` — glob/suffix matching for domain allowlists |

## SecurityPolicy Contract

`SecurityPolicy` is the central gate. Every tool execution checks it. Key invariants:

- **Default is `Supervised`** — acts but requires approval for medium+ risk. Never weaken this default.
- `workspace_only: true` confines file access to `workspace_dir` + `allowed_roots`. Path traversal (`../`) is rejected by `rootless_path()`.
- `forbidden_paths` blocks system dirs (`/etc`, `/proc`, `~/.ssh`, `~/.aws`). Removal requires security review.
- `allowed_commands` is a whitelist. Commands not listed are denied. The shell lexer (`skip_env_assignments`, quote-aware parsing) prevents bypass via `FOO=bar rm -rf /`.
- `ActionTracker` enforces sliding-window rate limits (default 20/hour). Uses `parking_lot::Mutex`.
- `max_cost_per_day_cents` caps LLM spend. `block_high_risk_commands` is a hard kill switch.
- `ToolOperation::Read` vs `Act` classification determines whether `ReadOnly` autonomy allows execution.

## Containment Backends

Priority order in `detect_best_sandbox()`: Landlock (kernel-native, Linux) > Firejail (Linux) > Bubblewrap (Linux/macOS) > Docker > NoopSandbox.

- All implement `Sandbox` trait: `wrap_command(&mut Command)`, `is_available()`, `name()`.
- `wrap_command` mutates `Command` in-place — prepends wrapper binary, sets seccomp filters, namespaces.
- Feature-gated: `sandbox-landlock`, `sandbox-bubblewrap`. Firejail/Docker are always compiled but runtime-detected.
- **Fallback is always `NoopSandbox`** — logged as `"none"` in audit. Never silently fail to a weaker backend without logging.
- Adding a new backend: implement `Sandbox`, register in `detect.rs`, add feature flag if platform-specific.

## Pairing Protocol

`PairingGuard` authenticates gateway clients:

1. First startup with `require_pairing: true` and no tokens: generates one-time code printed to terminal.
2. Client sends `POST /pair` with `X-Pairing-Code` header. Server returns bearer token (plaintext, once).
3. Only SHA-256 hash is stored. Token verified via `hash_token()` comparison — no timing side-channel.
4. Brute-force protection: `MAX_PAIR_ATTEMPTS=5`, lockout `300s`, per-client tracking with `MAX_TRACKED_CLIENTS=10000` cap and `900s` retention sweep.
5. Legacy plaintext tokens (`zc_...`) are hash-upgraded on load. 64-char hex strings treated as pre-hashed.

## Secret Management & Leak Detection

**SecretStore**: ChaCha20-Poly1305 with random 12-byte nonce per encryption. Key stored at `~/.zeroclaw/.secret_key` (0600 perms). Format: `enc2:<hex(nonce || ciphertext || tag)>`. Legacy `enc:` (XOR) auto-migrated via `decrypt_and_migrate()`.

**LeakDetector**: Scans outbound messages before send. Regex patterns for known API key formats + Shannon entropy check for tokens >= 24 chars. Sensitivity 0.0-1.0 (default 0.7). Returns `LeakResult::Detected` with redacted content.

**PromptGuard**: Detects injection patterns (system prompt override, role confusion, tool-call JSON injection, secret extraction, jailbreaks). Three actions: `Warn` (default, log only), `Block` (reject message), `Sanitize` (strip patterns). Sensitivity-tunable.

## Audit Logging

Merkle hash-chain: `entry_hash = SHA-256(prev_hash || canonical_json)`. Genesis uses zero-hash. Tamper with any entry and all subsequent hashes break. Events: `CommandExecution`, `FileAccess`, `ConfigChange`, `AuthSuccess/Failure`, `PolicyViolation`, `SecurityEvent`. Each event records `Actor` (channel, user), `Action` (command, risk, approved/allowed), `ExecutionResult`, and `SecurityContext` (sandbox backend, rate-limit remaining).

## E-Stop (Emergency Stop)

`EstopManager` provides graduated shutdown: `KillAll` (halt everything), `NetworkKill` (block all outbound), `DomainBlock` (selective), `ToolFreeze` (disable specific tools). State persisted to disk. Resume requires OTP validation. `fail_closed()` defaults to `kill_all: true`.

## Threat Model

**Attack surfaces**: (1) Prompt injection via user messages — `PromptGuard` detects, `LeakDetector` prevents exfil. (2) Path traversal in tool args — `rootless_path()` rejects `..`, `forbidden_paths` blocks sensitive dirs. (3) Command injection via chained operators — quote-aware shell lexer parses structure, not flat string. (4) Brute-force pairing — per-client lockout. (5) Config file credential theft — `SecretStore` encrypts at rest. (6) Audit log tampering — Merkle chain makes silent modification detectable. (7) Sandbox escape — defense-in-depth: app-layer policy + OS-level containment.

**Failure modes**: Sandbox unavailable falls to `NoopSandbox` (logged). Key file corrupted = decryption fails loudly (no silent fallback to plaintext). Rate limiter uses `Instant` — immune to wall-clock manipulation. E-stop defaults fail-closed.

## Testing Patterns

- Adversarial path tests: `../`, symlinks, Unicode normalization, null bytes in paths.
- Pairing brute-force: verify lockout triggers at exactly `MAX_PAIR_ATTEMPTS`, verify lockout duration.
- Secret roundtrip: encrypt-decrypt cycle, legacy `enc:` migration, corrupted ciphertext rejection.
- Audit chain: verify hash chain integrity after multi-event sequences, detect single-bit tampering.
- Shell lexer: chained commands (`&&`, `||`, `;`, `|`), env assignments, quoted strings, backticks.
- Leak detector: known API key formats (AWS, GitHub, Stripe), high-entropy random strings, false-positive rate.
- E-stop: verify `fail_closed()` state, OTP-gated resume, partial resume (unblock domains while tools stay frozen).

## Mandatory Review Rules

- Any change to `forbidden_paths`, `allowed_commands` defaults, or `AutonomyLevel::default()` requires explicit security review.
- Changes to `Sandbox::wrap_command` implementations must include adversarial escape tests.
- `SecretStore` cipher changes require migration path from previous format — never break existing encrypted configs.
- Audit chain format changes must maintain backward-compatible verification of existing logs.
- `PromptGuard` pattern additions must include both true-positive and false-positive test cases.
- E-stop changes must preserve `fail_closed()` semantics — never default to an open state.

## Cross-Subsystem Coupling

- **Agent loop** (`src/agent/`): checks `SecurityPolicy` before every tool dispatch; consults `PromptGuard` on inbound messages; calls `LeakDetector` on outbound.
- **Tools** (`src/tools/`): `Sandbox::wrap_command` called before shell execution; `WorkspaceBoundary` gates file/domain access.
- **Gateway** (`src/gateway/`): `PairingGuard` authenticates all HTTP requests; `EstopManager` can halt processing.
- **Config** (`src/config/`): `SecurityConfig` deserializes into `SecurityPolicy`; `SecretStore` decrypts credential fields during config load.
- **Channels** (`src/channels/`): IAM identity from Nevis flows through `IamPolicy` to determine per-user tool permissions.
