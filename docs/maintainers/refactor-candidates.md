# Refactor Candidates

Largest source files in `src/`, ranked by severity. Each does multiple jobs in a single file, hurting readability, testability, and merge conflict frequency.

| File | Lines | Problem |
|---|---|---|
| `config/schema.rs` | 7,647 | Every config struct for the entire system in one file |
| `onboard/wizard.rs` | 7,200 | Entire onboarding flow in one function-like blob |
| `channels/mod.rs` | 6,591 | Channel factory + shared logic + all wiring |
| `agent/loop_.rs` | 5,599 | The entire agent orchestration loop |
| `channels/telegram.rs` | 4,606 | One channel impl shouldn't be this big |
| `providers/mod.rs` | 2,903 | Provider factory + shared conversion logic |
| `gateway/mod.rs` | 2,777 | HTTP server setup + middleware + routing |

## Additional Notes

- `tools/mod.rs` (635 lines) has a 13-parameter `all_tools_with_runtime()` factory function that will get worse as tool count grows. Consider a registry/builder pattern.
- `security/policy.rs` (2,338 lines) mixes policy definition, action tracking, and validation — could split by concern.
- `providers/compatible.rs` (2,892 lines) and `providers/gemini.rs` (2,142 lines) are large for single provider implementations — likely mixing HTTP client logic, response parsing, and tool conversion.

### Misplaced module: `channels/tts.rs` → `tools/`

`channels/tts.rs` (642 lines, merged in PR #2994) is a multi-provider TTS synthesis system. It is not a channel — it does not implement `Channel` or provide a bidirectional messaging interface. TTS is a capability the agent invokes to produce audio output, which fits the `Tool` trait (`src/tools/traits.rs`). It should be moved to `src/tools/tts.rs` with a corresponding `Tool` implementation, and its config types extracted from the `channels` section of `schema.rs` into a `[tools.tts]` config namespace. As of merge, the module is not integrated into any calling code (re-exports are `#[allow(unused_imports)]`), so this move has zero runtime impact.

---

## Best Practices Audit Findings

Findings from a general Rust/Python best-practices review (not project-specific conventions).

### Critical: `.unwrap()` in production code (~2,800 instances)

`.unwrap()` appears in I/O paths, serialization, and security-sensitive modules beyond test code. Example:

```rust
// cost/tracker.rs
writeln!(file, "{}", serde_json::to_string(&old_record).unwrap()).unwrap();
file.sync_all().unwrap();
```

Rust best practice: use `.context("msg")?` or handle errors explicitly. Each unwrap is a potential runtime panic on transient failures.

### Critical: `panic!` in production paths (28+ instances)

Providers, pairing, and CLI routing use `panic!` instead of returning errors:

```rust
// providers/bedrock.rs
panic!("Expected ToolResult block");
// security/pairing.rs
panic!("Generated 10 pairs of codes and all were collisions — CSPRNG failure");
```

These should be `bail!()` or typed error variants — panics are unrecoverable and crash the process.

### Critical: Blanket clippy suppression (32+ lints globally)

`main.rs` and `lib.rs` suppress `too_many_lines`, `similar_names`, `dead_code`, `missing_errors_doc`, and many others at crate level. This hides new violations as they accumulate. Best practice: suppress per-function with a justification comment, not globally.

### High: Silent error swallowing (`let _ = ...` on Results, 30+ instances)

Gateway, WebSocket, and skill sync paths discard `Result` values silently:

```rust
let _ = state.event_tx.send(serde_json::json!({...})).await;
let _ = sender.send(Message::Text(err.to_string().into())).await;
let _ = mark_open_skills_synced(&repo_dir);
```

At minimum these should `tracing::warn!` on failure. Silent drops make distributed debugging nearly impossible.

### High: God struct — `Config` with 30+ fields

Every subsystem that needs any configuration must hold the entire `Config` struct, creating implicit coupling and bloated test setup. Best practice: pass narrow config slices or trait-bounded config objects.

### High: Security code not isolated

Shell command validation (300+ lines of quote-aware parsing), webhook signature verification, and pairing logic are embedded in large multipurpose files rather than isolated modules. This complicates security audits and increases regression risk from unrelated changes.

### Medium: Excessive `.clone()` (~1,227 instances)

Auth/token refresh paths clone large structs on every branch. Hot paths like token access could use `Cow<'_>` or `Arc` instead of full clones.

### Medium: Test depth — mostly smoke tests

193 test modules exist (good structural coverage), but most are simple value assertions. Missing:

- Property-based testing for parsers/validators
- Integration tests for multi-module flows
- Fuzz testing for the shell command parser (security surface)
- Mock-based tests for network-dependent paths

### Medium: Dependency count (82 direct)

The project claims size optimization as a goal (`opt-level = "z"`, `lto = "fat"`) while accumulating heavy optional deps like `matrix-sdk` (full E2EE crypto) and `probe-rs` (50+ transitive deps). The tension between size goals and feature breadth is unresolved.

### Low: `unsafe` without safety comments

Two instances in `src/service/mod.rs` for `libc::getuid()` — no `// SAFETY:` comment. Could use the `nix` crate's safe wrapper instead.

### Low: Minimal `rustfmt.toml`

Only sets `edition = "2021"`. For a project this size, configuring `max_width`, `imports_granularity`, `group_imports` would enforce consistency as contributor count grows.

### Resolved: CI/CD security hardening (P1/P2)

~~Third-party actions pinned to mutable tags; release workflows granted overly broad write permissions; no composite gate job for branch protection; security tools compiled from source on every PR.~~

**Fixed in** `cicd-best-practices` **branch:**
- All third-party actions SHA-pinned (P1)
- Release workflow permissions scoped per-job (P1)
- Composite `Gate` job added to PR checks (P2)
- Security tools installed via pre-built binaries (P2)

## Priority Recommendations

1. **Replace unwraps/panics in non-test code** with proper error propagation — highest stability impact.
2. **Split god modules** — extract runtime orchestration from `channels/mod.rs`, isolate security parsing, break `Config` into sub-configs.
3. **Remove global clippy suppressions** — fix violations individually or add per-item `#[allow]` with reasoning.
4. **Replace `let _ =` on Results** with at minimum `tracing::warn!` logging.
5. **Add property/fuzz tests** for security-surface parsers (shell command validation, webhook signatures).

---

## Deferred Structural Refactorings

Changes deferred from the project-cleanup pass. Each entry includes rationale and scope.

### Rename `src/sop/` to `src/runbooks/`

**Why:** "SOP" is jargon-heavy and doesn't communicate what the module does. "Runbooks" is the industry-standard term for trigger-driven automated procedures with approval gates.

**Scope:** Rename module (`src/sop/` → `src/runbooks/`), update config keys (`[sop]` → `[runbooks]`), CLI subcommand (`zeroclaw sop` → `zeroclaw runbook`), all internal types (`Sop*` → `Runbook*`), docs (`docs/sop/` → matching new structure), and references in CLAUDE.md.

### TODO: Fuzz testing — upgrade stubs to real coverage

**Current state:** 5 fuzz targets exist in `fuzz/fuzz_targets/`, but only `fuzz_command_validation` tests real ZeroClaw code. The other 4 (`fuzz_config_parse`, `fuzz_tool_params`, `fuzz_webhook_payload`, `fuzz_provider_response`) just fuzz `serde_json::from_str::<Value>` or `toml::from_str::<Value>` — they test third-party crate internals, not ZeroClaw logic.

**Wire existing stubs to real code paths:**

- `fuzz_config_parse`: deserialize into `Config`, not `toml::Value`
- `fuzz_tool_params`: pass through actual `Tool::execute` input validation
- `fuzz_webhook_payload`: run through webhook signature verification + body parsing
- `fuzz_provider_response`: parse into actual provider response types (Anthropic, OpenAI, etc.)

**Add missing targets for security surfaces:**

- Shell command parser (quote-aware parsing, beyond just `validate_command_execution`)
- Credential scrubbing (`scrub_credentials` — already had a UTF-8 boundary panic in #3024)
- Pairing code generation/validation
- Domain matcher
- Prompt guard scoring
- Leak detector regex

**Infrastructure improvements:**

- Add seed corpora (`fuzz/corpus/<target>/`) with known-good and edge-case inputs; commit to repo
- Consider `Arbitrary` derive for structured fuzzing instead of raw `&[u8]`
- Set up scheduled CI fuzzing (nightly/weekly) — OSS-Fuzz is free for open-source projects
- Use `cargo fuzz coverage <target>` to generate lcov reports from corpus runs and track which code paths the fuzzer actually reaches
- Track crash artifacts (`fuzz/artifacts/<target>/`) as issues

### TODO: Test infrastructure follow-ups from `e2e-testing` branch

Issues identified during quality review of the test restructuring work.

**1. ~~`#[path]` attribute pattern in runner files~~ (resolved)**

~~Runner files used `#[path]` attributes as a workaround for E0761.~~ Fixed: runner files renamed to `test_component.rs` etc., directories use standard `mod.rs` files. `Cargo.toml` `[[test]]` entries updated to match. `cargo test --test component` commands unchanged.

**2. Dead infrastructure: `TestChannel`, `TraceLlmProvider`, trace fixtures, `verify_expects()`**

These were built as scaffolding but have no consumers:
- `tests/support/mock_channel.rs` (`TestChannel`) — planned for channel-driven system tests, but the agent has no public channel-driven loop API, so system tests use `agent.turn()` directly.
- `tests/support/mock_provider.rs` (`TraceLlmProvider`) — replays JSON fixture traces, but no test loads or runs a fixture.
- `tests/fixtures/traces/*.json` (3 files) — never loaded by any test.
- `tests/support/assertions.rs` (`verify_expects()`) — never called.

Either write tests that exercise this infrastructure or remove it to avoid dead code confusion.

**3. Gateway component tests overlap with existing `whatsapp_webhook_security.rs`**

`tests/component/gateway.rs` has 6 HMAC signature verification tests for `verify_whatsapp_signature()` — the same function tested by 8 tests in `tests/component/whatsapp_webhook_security.rs`. Only the 3 gateway constants tests (`MAX_BODY_SIZE`, `REQUEST_TIMEOUT_SECS`, `RATE_LIMIT_WINDOW_SECS`) provide genuinely new coverage. Consider consolidating the signature tests into one file or removing the duplicates from `gateway.rs`.

**4. Security component tests are config-only — no behavioral coverage**

The 10 security tests validate config defaults and TOML serialization only (`AutonomyConfig::default()`, `SecretsConfig`, round-trips). They don't test security *behavior* (policy enforcement, credential scrubbing, action rate limiting) because `src/security/` is `pub(crate)`. The `security_config_debug_does_not_leak_api_key` test is a no-op — it checks for a leak but has no assertion on failure (just a comment). To get real behavioral coverage, either:
- Make targeted security functions `pub` for testing (e.g. `scrub_credentials`, `SecurityPolicy::evaluate`)
- Add `#[cfg(test)] pub` escape hatches in `src/security/`
- Write in-crate unit tests in `src/security/tests.rs` instead

**5. `pub(crate)` visibility blocks integration testing of critical subsystems**

The `security` and `gateway` modules use `pub(crate)` visibility, preventing integration tests from exercising core logic like `SecurityPolicy`, `GatewayRateLimiter`, and `IdempotencyStore`. This forced the new component tests to test only through the narrow public API surface (config structs, one signature function, constants). Consider whether key security types should expose a test-only public interface or whether these tests belong as in-crate unit tests.

### TODO: Automated release announcements — Twitter/X integration

**Current state:** Releases are published on GitHub only. No automated cross-posting to social channels.

**Plan:**

- Add `.github/workflows/release-tweet.yml` triggered on `release: [published]`
- Use `nearform-actions/github-action-notify-twitter` (OAuth 1.0a, v1.1 API) or direct X API v2 `curl` with OAuth signing
- Tweet template: release tag, one-line summary, link to GitHub release
- Skip prereleases (`if: "!github.event.release.prerelease"`)

**Required secrets (Settings > Secrets > Actions):**

- `TWITTER_API_KEY`, `TWITTER_API_KEY_SECRET`
- `TWITTER_ACCESS_TOKEN`, `TWITTER_ACCESS_TOKEN_SECRET`

**Considerations:**

- Review against `docs/contributing/actions-source-policy.md` — pin third-party action to commit SHA or vendor
- X free tier: 1,500 tweets/month (sufficient for releases)
- Truncate release body to 280 chars if including highlights in tweet
