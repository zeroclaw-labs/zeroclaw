# Audit Workbook — 2026-05-03 (in progress)

> **Status:** Working draft. The user is away for ~6h; this file accumulates raw findings as they arrive from sub-agents and direct grep audits. The polished report (`audit-2026-05-03.md`) will be assembled from this once all inputs are in.
>
> **Scope baseline:** Last full audit was `docs/project/production-completion-plan.md` dated 2026-03-02. This audit covers everything since, with emphasis on the second-brain (vault) addition that landed across 2026-04-22 to 2026-05-02.
>
> **Authoritative spec used as the bar:**
> - `docs/ARCHITECTURE-v8.md` (canonical, English) — especially §10 First Brain, §11 Second Brain, §12 Multi-Device Sync, §1590+ Hybrid Relay Security, §923+ Phase 3 SLM-as-Executor.
> - `docs/i18n/ko/ARCHITECTURE-v8.md` (Korean v8, partial Korean) — for §11 / §12 Korean original sections.
> - `docs/ephemeral-relay-sync-patent.md` — for the 3-tier sync protocol invariants.
>
> **Privacy invariants enforced as critical-class** (per user statement 2026-05-03):
> 1. No persistent external storage of user secrets ever.
> 2. Multi-device sync must work even when devices are not concurrently online.
> 3. Any temporary relay is encrypted end-to-end (zero-knowledge to operator).
> 4. The relay does the minimum possible work; no bundling with full-data-path roles.

---

## Inputs collected so far

### A. Sub-agent: src/memory/ + src/sync/ — COMPLETE
- Verdict: **Zero critical, zero medium**. Privacy invariants verified.
- Key positive: Patent 1 (zero-knowledge sync) is wired correctly. RelayClient↔Gateway wiring is closed (`src/gateway/mod.rs:715-761`) — production-completion-plan.md gap is resolved as of 2026-05-02.
- DeltaOperation v3 (TimelineAppend / PhoneCallRecord / CompiledTruthUpdate) integrated into sync journal without re-record loops.
- `RelayEntry.encrypted_payload: String` ciphertext, no plaintext leakage in relay path.
- HLC clock monotonicity preserved with `saturating_add` and lock-free CAS.
- LWW conflict resolution deterministic.
- ✅ One minor: `src/sync/relay.rs:16` Korean comment in English-comment area. Cosmetic.
- ✅ One minor: `ORDER_BUFFER_MAX = 1000` hardcoded; not configurable. Consider making it a config key if very-high-throughput sync becomes a goal.

### B. Sub-agent: src/vault/ — COMPLETE
- Verdict: **Zero critical, zero medium**.
- All vault mutations route through `VaultStore::ingest_markdown()` and atomically record sync deltas (`record_vault_doc_upsert()`). Tags/aliases/links serialized in delta — receiving side gets full metadata in one shot.
- Domain delta apply (post-PR #225) is UUID-keyed, not id-keyed → no id-allocation conflicts; temp `delta_id_map` correctly remaps aux tables; transaction-wrapped with crash-safety via SQLite WAL.
- Korean encoding (`legal/encoding.rs`): UTF-8 BOM-stripped, UTF-16 LE/BE, CP949/EUC-KR fallback in order; reports detected encoding + `had_errors` flag.
- All `Result`-returning functions propagate; test-only `unwrap()`s isolated under `#[cfg(test)]`.
- 📋 **Out of scope (intentional, per design):**
  - Vault doc deletion absent in P1 (`SUMMARY §1` — append-only second brain). Soft delete planned for later phases.
  - Tag/alias/link updates after ingest not exposed in public API — must go through `ingest_markdown()` (idempotent via checksum).
  - Hub note engine schema pre-created; compile logic present (`hub.rs`); briefing/backfill pipeline deferred.
  - **BUG #16 (domain auto-update scheduler not wired)** — re-confirmed deferred for consumer build. Domain DB is fork-only (legal/medical), per `project_domain_db_distribution.md`.

### C. Sub-agent: src/gateway/ + src/config/schema.rs — COMPLETE

#### 🚨 Critical
- **C1. [`src/config/schema.rs:17511-17521`] `SyncConfig::default().enabled = false` but serde annotation gives `true`.** Type of bug: BUG #15 sibling (Default-vs-serde drift). Effect: any code that builds `SyncConfig::default()` instead of deserializing from TOML silently disables sync. Need to grep for callers of `SyncConfig::default()` to determine real impact, but the drift itself is the same class as the already-fixed BUG #15. Recommended fix: change `Default` impl to `enabled: true` to match serde, OR change serde to `default_false` and update doc; the safer choice for this codebase is to make Default match serde so sync stays on by default for fresh installs.

#### 🟡 Medium
- **C2. [`src/config/schema.rs:2856-2865`] Orphaned `GatewayConfig::owner_username/owner_password` fields.** Doc claims clients must supply these in pairing handshake; grep finds zero consumers. Either implement validation in `src/gateway/pair.rs` or remove the fields. Risk: users may set them expecting auth enforcement that never happens.
- **C3. [`src/gateway/api.rs` lines 146,175,186,198,207,332,420,526,653,…] Internal error string leaked to clients.** All `format!("...{e}")` patterns in error responses expose anyhow/serde/TOML internals. Standardize: log full error server-side, return generic `{"error": "config_save_failed"}` codes. Highest priority on routes that touch workspace, config, auth.
- **C4. [`src/gateway/mod.rs:1828-1836`] CORS defaults to `Any` when `CORS_ALLOWED_ORIGINS` env var is unset.** Acceptable with default 127.0.0.1 bind, but **🔴 CSRF risk if `allow_public_bind=true`.** Fail-secure recommendation: when `allow_public_bind=true`, require an explicit origin allow-list; reject if unset. Add CSRF tokens on PUT/DELETE.

#### 🟢 Minor
- **C5. [`src/config/schema.rs:17453`] Doc says "(default: true)" but `Default` impl says false.** Fix in tandem with C1 above.
- **C6. [`src/gateway/mod.rs:1067`] Idempotency cleanup TTL configurable but no startup tracing log confirms TTL/sweep interval.** Cosmetic.

#### ✅ Verified clean
- All 4 sync endpoints (`/api/sync/push|pull|status`, `/ws/sync`) require auth when pairing is on. ✅
- Hybrid relay TTL = 15 min; `handle_llm_proxy()` validates session + credits + keeps operator key in env; no API key exits server. ✅
- **RelayClient wired (`src/gateway/mod.rs:715-755`)** — production-completion-plan gap CLOSED. ✅
- WhatsApp webhook signature verification present (`verify_whatsapp_signature()` line 3281). Idempotency caching present.
- Static file serving uses `rust-embed` (compile-time bundled), no path traversal possible.
- Per-device gateway defaults: `127.0.0.1` bind, `allow_public_bind=false`, `require_pairing=true`. ✅

### D. Sub-agent: src/security/ + src/auth/ — COMPLETE

#### 🔐 E2E encryption verdict for sync (mandatory section)
- **Path:** `src/memory/sync.rs` (SyncEngine encrypt/decrypt) → `src/sync/relay.rs` (RelayClient/RelayEntry).
- **Cipher:** ChaCha20-Poly1305 (AEAD) for sync deltas. AES-256-GCM separately used for vault/document encryption (`src/security/encryption.rs`).
- **Nonce strategy:** Random 12-byte per message via `rand::fill` (`src/memory/sync.rs:1212`); fresh nonce per ciphertext, no reuse. ✅
- **Encryption flow:** DeltaEntry vec → JSON → ChaCha20Poly1305 AEAD encrypt → base64 → wrapped in `SyncPayload { nonce, ciphertext, sender, version }` → over WebSocket → `RelayClient.store()` → relay holds ciphertext only. ✅ Server never sees plaintext.
- **VERDICT:** ✅ **Transport encryption is sound.** ⚠️ **BUT key derivation does NOT match patent spec** — see D1.

#### 🚨 Critical
- **D1. [`src/memory/sync.rs:669-679`] Sync key is randomly generated, not PBKDF2-derived from passphrase.** Patent claim 8 in `docs/ephemeral-relay-sync-patent.md §174` mandates `PBKDF2(passphrase, …)` with no per-user passphrase binding. Current code: `rand::fill` into a 32-byte array stored in `.sync_key` (file mode 0600). Effect: if `.sync_key` file is exfiltrated (or cloned), decryption succeeds without user passphrase → **violates zero-knowledge property in the threat model where local file is compromised.** Recommended fix: prompt for sync passphrase at first launch, derive key with `PBKDF2(passphrase, salt=device_id, iterations≥100k)`, store only the salt + verifier; the key is re-derived on demand or held in memory only.
- **D2. Architectural ambiguity: per-device key vs per-user key.** Each device generates its own random `.sync_key` → **devices CANNOT decrypt each other's deltas.** Multi-device sync must therefore be either (a) using a per-user master key shared via a secure pairing channel (not currently implemented), or (b) re-encrypted at the relay (which would violate zero-knowledge). Either the implementation is incomplete or the patent claim is unmet. Resolution requires a design decision and matching code: derive per-device sync keys from a per-user master key via HKDF, share the master key during pairing.
- **D3. [`src/auth/store.rs:28-29`] `HASH_ITERATIONS = 100_000` for SHA-256 + salt — weak by modern standards.** Modern recommendations: Argon2id (`memory=19MiB, time=2`) or scrypt (`cost_log=15`). 100k SHA-256 ≈ 1-10ms on modern GPU → offline cracking of a stolen `users` table is feasible. **Per CLAUDE.md §3.6 secure-by-default, this is a critical hardening gap.**

#### 🟡 Medium
- **D4. [`src/auth/store.rs:276-290`] `set_password()` allows 4+ chars but `register()` requires 8+.** Inconsistent — users can downgrade after registration. Enforce 8+ in both paths.
- **D5. [`src/auth/store.rs:254` + impl at 1143-1151] `constant_time_eq` returns early on length mismatch — timing oracle on password-hash length.** Refactor to always iterate to `max(a.len, b.len)`, accumulate diff bytes + length-diff into a single `u8`, return at end.
- **D6. [`src/security/secrets.rs:109-113`] Legacy XOR enc: format warning is informational only — no migration prompt at config-load.** Users may not act on the warning; legacy insecure values persist. Add a one-time auto-migrate prompt.

#### ✅ Verified clean
- **Pairing tokens** (`src/security/pairing.rs`): CSPRNG entropy (256-bit), SHA-256 storage (plaintext returned once), constant-time comparison **here is correct** (does NOT short-circuit on length, lines 439-456), 5-failure 5-min lockout per IP, single-use consumption.
- **Session tokens**: CSPRNG, configurable TTL (default 30d, can drop to 24h for web), SHA-256 storage, per-device revocation.
- **Secret store** (`src/security/secrets.rs`): ChaCha20-Poly1305 AEAD, 0o600 on Unix + `icacls /grant:r USER:F` on Windows, 12-byte random nonce, OsRng key generation.
- **Sync relay E2E**: relay never decrypts, TTL auto-delete, no plaintext in logs/error messages.
- **Operator API key isolation**: `ADMIN_*_API_KEY` env vars read server-side only, never returned in HTTP responses; only short-lived (15-min) proxy tokens reach clients.

### E. Sub-agent: src/tools/ + src/agent/ + clients/ — COMPLETE

#### 🚨 Critical
- **E1. [`src/advisor/slm_executor.rs:135-295`] SLM Executor passes ALL tools through unfiltered — `safe_for_slm` filtering is missing.** ARCHITECTURE-v8 §923+ explicitly mandates that the SLM executor filter to tools where `safe_for_slm()=true`. Currently `find_tool()` (line 293) dispatches with no check. **An SLM can directly invoke `shell` / `file_write` / `delegate` / `apply_patch` / `cron_*`** — bypassing the entire safety design. Recommended fix: at the top of `slm_executor::run()`, filter `let safe_tools: Vec<_> = tools.iter().filter(|t| t.safe_for_slm()).copied().collect();`, pass `&safe_tools` to `build_system_prompt()` and `find_tool()`. Add a regression test that verifies `shell` is not present in the SLM's tool list.
- **E2. [`src/advisor/slm_executor.rs`] SLM Executor is defined but NEVER instantiated/called anywhere.** Grep finds zero `SlmExecutor::new()` or `.run()` usages. Either:
  - This is incomplete integration (Phase 3 not yet wired into `gateway/openclaw_compat.rs::handle_api_chat()` or `gateway/ws.rs::handle_socket()`).
  - It is intentionally deferred but not documented as such.
  Resolution required before merge: either wire it up per ARCHITECTURE-v8 §923+, or annotate with `// TODO: Phase 3 SLM-as-Executor wiring deferred` AND add to the deferred-work list. **Note: with E1 unfixed and E2 unwired, the practical risk is currently zero (the unsafe code path is dead), but if E2 is fixed without E1, the unsafe path activates immediately.** Treat E1 as a hard prerequisite for any work that closes E2.

#### 🟡 Medium
- **E3. [`src/tools/credential_vault.rs:99`] `credential_store` correctly `safe_for_slm=false`. Verify any companion `credential_recall` (read path) returns reference tokens, not plaintext.** If plaintext is returned, set `safe_for_slm=false`.
- **E4. [`clients/tauri/src-tauri/src/lib.rs:30-50`] Tauri sidecar spawn: no explicit timeout, no graceful shutdown hook, no auto-restart.** Production-completion-plan item 1 calls for 30s timeout + `on_window_event` shutdown + 3-retry restart. Currently absent.
- **E5. [`clients/tauri/src-tauri/src/lib.rs:110-150`] IPC is HTTP POST, not WebSocket.** Plan requires `ws://127.0.0.1:{PORT}/ws/chat` with persistent connection + heartbeat for sub-1ms latency.
- **E6. [`clients/ios-bridge/src/lib.rs:83-150`] iOS bridge has C-FFI scaffolding but unclear whether zeroclaw is built as a static library or as a child process.** iOS cannot launch child processes. Add a `#[cfg(target_os = "ios")]` compile-time assertion preventing any child-spawn path.
- **E7. [`clients/android-bridge/src/lib.rs:148-150`] Android bridge assumes a zeroclaw binary is available, but no NDK cross-compile is wired.** Without `aarch64-linux-android` build, the bridge fails at runtime. Need `scripts/android/build-ndk.sh` + `clients/android/app/src/main/jniLibs/arm64-v8a/libzeroclaw.so` packaging.

#### ✅ Verified clean
- **All 7 risky tools correctly declare `safe_for_slm=false`:** `file_write` (`tools/file_write.rs:46-48`), `file_edit` (50-52), `apply_patch` (62-64), `shell` (`tools/shell.rs:129-131`), `delegate` (`tools/delegate.rs:181-183`), `cron_add/remove/update` (61-63 / 57-59 / 58-60), `credential_store` (99-101). ✅
- Read-only tools default to `safe_for_slm=true`: `vault_graph`, `smart_search`, `legal_*`. ✅
- **Shell command safety:** `tools/shell.rs:126-131` clears `env()` and re-adds only `SAFE_ENV_VARS`. argv-only invocation — no `sh -c` interpolation.
- **Agent loop** (`src/agent/loop_.rs`): no `unwrap()` on data paths; error propagation correct.

### F. Direct grep audit — channels / providers / observability / cross-cutting

[See section above; key items: F.1 BUG #14 sibling at `src/economic/classifier.rs:674`, F.2 WhatsApp QR plaintext log at `whatsapp_web.rs:725`, F.3 channel `unwrap()` density warrants follow-up sweep, F.4 968 `println!` total — small subset in runtime paths is a tracing-hygiene lint.]

### G. Build / clippy / test results

#### G.1 `cargo build --all-targets` — ✅ PASSED (exit 0)
- Wall time: 10m 35s.
- 0 errors, several warnings:
  - `src/security/pii_redaction.rs:543` — non-snake-case test fn name `api_key_prefix_redacts_sk_and_AKIA`. Cosmetic.
  - `src/billing/mod.rs:23` — unused imports `CheckoutRequest`, `CheckoutResponse`, `SubscriptionPlan`, `USD_PACKAGES`, `UsdCreditPackage`.
  - `src/skills/symlink_tests.rs:3` — unused `crate::config::Config`.
- These map to 🟢 minor cleanups. Listed under H below.

#### G.2 `cargo clippy --all-targets --workspace` — ✅ exited 0 BUT compile errors hidden inside
- The exit code is 0 because the workspace top-level lib + bin compiled, but `--all-targets` revealed an error in a workspace member that does not block top-level cargo check:
  - **G4. 🚨 [`clients/android-bridge/uniffi-bindgen.rs:4`] `uniffi::uniffi_bindgen_main()` not found.** Root cause: `clients/android-bridge/Cargo.toml:15` depends on `uniffi = { version = "0.27" }` with no features, but the `uniffi_bindgen_main` symbol is gated behind the `cli` feature in uniffi 0.27.3. **This blocks `cargo clippy --all-targets` in CI and any Android NDK build that exercises this binary.** Fix: change to `uniffi = { version = "0.27", features = ["cli"] }`. (Or, if the binary is unused in current build flow, delete the `[[bin]]` entry.)
  - 8 other clippy warnings — non-snake-case test name, unused vars, field_reassign_with_default. All 🟢 minor.

#### G.3 `cargo test --lib --workspace` — RUNNING (background)

## H. Cross-cutting findings ledger (running)

| ID | Severity | File:line | Finding |
|----|----------|-----------|---------|
| C1 | 🚨 Critical | `src/config/schema.rs:17511-17521` | `SyncConfig::default().enabled=false` drifts from serde `default_true` (BUG #15 sibling) |
| C2 | 🟡 Medium | `src/config/schema.rs:2856-2865` | Orphaned `owner_username/owner_password` config fields |
| C3 | 🟡 Medium | `src/gateway/api.rs` (~12 sites) | Internal error strings leaked to clients in error responses |
| C4 | 🟡 Medium | `src/gateway/mod.rs:1828-1836` | CORS `Any` default is CSRF risk under `allow_public_bind=true` |
| D1 | 🚨 Critical | `src/memory/sync.rs:669-679` | Sync key is random, not PBKDF2-from-passphrase — patent spec mismatch |
| D2 | 🚨 Critical | `src/memory/sync.rs` (architectural) | Per-device sync key prevents cross-device decryption — multi-device sync silently broken? |
| D3 | 🚨 Critical | `src/auth/store.rs:28-29` | `HASH_ITERATIONS=100_000` SHA-256 — weak vs Argon2id/scrypt |
| D4 | 🟡 Medium | `src/auth/store.rs:276-290` | `set_password` min 4 chars vs `register` min 8 — downgrade path |
| D5 | 🟡 Medium | `src/auth/store.rs:254 + 1143-1151` | `constant_time_eq` early-returns on length — timing oracle |
| D6 | 🟡 Medium | `src/security/secrets.rs:109-113` | Legacy XOR enc: warning only, no migration prompt |
| E1 | 🚨 Critical | `src/advisor/slm_executor.rs:135-295` | SLM executor missing `safe_for_slm` filtering — risky tools reachable from SLM |
| E2 | 🚨 Critical | `src/advisor/slm_executor.rs` | SLM executor defined but never instantiated — dead-code or incomplete wiring |
| E3 | 🟡 Medium | `src/tools/credential_vault.rs` | Verify `credential_recall` returns reference tokens not plaintext |
| E4 | 🟡 Medium | `clients/tauri/src-tauri/src/lib.rs:30-50` | Tauri sidecar lacks timeout / shutdown hook / auto-restart |
| E5 | 🟡 Medium | `clients/tauri/src-tauri/src/lib.rs:110-150` | Tauri IPC is HTTP, not WebSocket (latency contract gap) |
| E6 | 🟡 Medium | `clients/ios-bridge/src/lib.rs:83-150` | iOS bridge: confirm cdylib/static lib, not child process |
| E7 | 🟡 Medium | `clients/android-bridge/src/lib.rs:148-150` | Android NDK cross-compile not wired |
| F1 | 🚨 Critical | `src/economic/classifier.rs:674` | `partial_cmp().unwrap()` panic surface — BUG #14 sibling |
| F2 | 🚨 Critical | `src/channels/whatsapp_web.rs:725` | `tracing::info!("WhatsApp Web QR payload: {}", code)` — pairing-credential plaintext log |
| F3 | 🟡 Medium | `src/gateway/auth_api.rs:706` | Kakao OAuth error logs raw `body_text` — possible token leak |
| C5 | 🟢 Minor | `src/config/schema.rs:17453` | Doc says "(default: true)" but Default impl is false |
| C6 | 🟢 Minor | `src/gateway/mod.rs:1067` | Idempotency TTL config: no startup tracing line |
| G1 | 🟢 Minor | `src/security/pii_redaction.rs:543` | Non-snake-case test fn name `api_key_prefix_redacts_sk_and_AKIA` |
| G2 | 🟢 Minor | `src/billing/mod.rs:23-25` | Unused imports |
| G3 | 🟢 Minor | `src/skills/symlink_tests.rs:3` | Unused `Config` import |
| G4 | 🚨 Critical | `clients/android-bridge/Cargo.toml:15 + uniffi-bindgen.rs:4` | uniffi `cli` feature missing → `uniffi_bindgen_main` not found, breaks `clippy --all-targets` and Android NDK build |
| G5 | 🟢 Minor | `clients/android-bridge/src/lib.rs:514` | Unused `result` var (test) |
| G6 | 🟢 Minor | `clients/android-bridge/src/lib.rs:117-118` | `field_reassign_with_default` clippy lint |

**Critical count: 9.** **Medium count: 11.** **Minor count: 8+.**

### F. Direct grep audit — channels / providers / observability / cross-cutting

#### F.1 BUG #14 (`partial_cmp().unwrap()`) sibling sweep

Goal: find any remaining `partial_cmp()` chained with `.unwrap()` (the panic-surface pattern fixed in PR #225 for `legal::graph_query::issue_analysis`).

Findings:

| File:line | Pattern | Verdict |
|-----------|---------|---------|
| `src/economic/classifier.rs:674` | `.max_by(|a, b| a.1.partial_cmp(b.1).unwrap())` | **🚨 SIBLING REGRESSION — UNSAFE** — same panic surface BUG #14 fixed elsewhere. Should be `unwrap_or(std::cmp::Ordering::Equal)`. |
| `src/memory/chunk_semantic.rs:179` | `partial_cmp(b).unwrap_or(Equal)` | ✅ Safe |
| `src/memory/cross_recall.rs:288,357` | `partial_cmp(&a.1).unwrap_or(Equal)` | ✅ Safe |
| `src/rag/mod.rs:283` | `partial_cmp(&a.1).unwrap_or(Equal)` | ✅ Safe |
| `src/memory/sqlite.rs:1498,2468` | `partial_cmp(&a.1).unwrap_or(Equal)` | ✅ Safe |
| `src/session_search/store.rs:221` | `partial_cmp(&a.rank).unwrap_or(Equal)` | ✅ Safe |
| `src/vault/hub.rs:221` | `partial_cmp(&b.1).unwrap_or(Equal)` | ✅ Safe |
| `src/vault/store.rs:393,500` | `partial_cmp(&a.2).unwrap_or(Equal)` | ✅ Safe |
| `src/vault/unified_search.rs:365` | `partial_cmp(&a.score).unwrap_or(Equal)` | ✅ Safe |
| `src/vault/wikilink/cross_validate.rs:62` | `partial_cmp(&a.1).unwrap_or(Equal)` | ✅ Safe |

**Recommendation:** Add `src/economic/classifier.rs:674` to the next fix PR. Same one-line change as BUG #14 in `legal::graph_query::issue_analysis`.

#### F.2 Plaintext logging of credentials / payloads (privacy invariant #4)

Findings:

| File:line | Log line | Verdict |
|-----------|----------|---------|
| `src/channels/whatsapp_web.rs:725` | `tracing::info!("WhatsApp Web QR payload: {}", code);` | **🚨 CRITICAL — pairing-credential plaintext log** — WhatsApp QR pairing code grants full session access; logging it at `info!` level (default-visible) lets anyone with log access pair their own client. Must be downgraded to `trace!` AND redacted (e.g. log only length / first 4 chars). |
| `src/gateway/auth_api.rs:706` | `tracing::error!("Kakao token exchange error ({status}): {body_text}");` | **🟡 MEDIUM — possible OAuth body leak** — if Kakao returns an error body containing partial tokens or refresh material, this logs them. Mitigate by logging `status` only at `error!`, body at `debug!` redacted. |
| `src/providers/gemini.rs:704` | `tracing::info!("Gemini CLI OAuth token refreshed successfully (runtime)");` | ✅ No token content logged, just success event. |
| `src/gateway/mod.rs:1952,2971,3237,3702` | various pairing/auth warning logs | ✅ Logs the rejection event, not the token. |
| `src/gateway/ws.rs:884` | `Failed to create proxy token for hybrid relay` | ✅ Error event only, no token. |
| Other channel/payload warnings | parse-error level only | ✅ Acceptable |

#### F.3 `unwrap()` density per channel

Total `unwrap()/expect/panic!` occurrences in `src/channels/`: **441**. Highest-density files:

- `src/channels/mod.rs:123`
- `src/channels/imessage.rs:62`
- `src/channels/telegram.rs:60`
- `src/channels/case_session.rs:26`

These were not individually inspected in this round. Per CLAUDE.md §3.5 ("explicit `bail!`/errors for unsafe states") and the channel surface being internet-adjacent (CLAUDE.md §2 risk), a follow-up sweep is warranted to confirm each unwrap is in test code or on infallible operations. Listed as 📋 follow-up rather than a finding because the actual risk depends on which paths are user-input-driven.

#### F.4 `println!`/`eprintln!` outside CLI surface

Total: **968 occurrences across 30 files**. Most are legitimate (CLI tools, `bin/`, `main.rs`, `migration.rs`, `onboard/wizard.rs`). Concerning:

- `src/agent/loop_.rs:18` — agent loop should use `tracing` not `println!`
- `src/agent/agent.rs:8` — same
- `src/gateway/mod.rs:41` — same
- `src/channels/mod.rs:47`, `src/channels/telegram.rs:3`, `src/channels/whatsapp_web.rs:4` — channel runtime uses `println!`

Each is potentially a `tracing` lint candidate (does not break correctness; affects observability hygiene). Listed as 🟢 minor.

---

## Decisions taken without confirmation (autonomous mode)

Per memory `feedback_autonomous_mode_for_audits.md`:

- D1. Sub-agents launched in parallel for vault, memory+sync, gateway+config, security+auth, tools+agent+clients. No confirmation requested.
- D2. Direct grep audit is being done in the main context to cover channels/providers/observability without burning sub-agent budget.
- D3. Build/clippy/test running in background (started 2026-05-03 ~04:10).
- D4. **No code changes will be made.** Only the report is being produced. The user said "수정해줘" is the trigger for code changes; that has not been given yet.
- D5. **No PR / no push / no merge** during this autonomous window.
- D6. Workbook (`audit-2026-05-03-workbook.md`) is the raw findings store. Final report (`audit-2026-05-03.md`) will be polished from this and presented when the user returns.
