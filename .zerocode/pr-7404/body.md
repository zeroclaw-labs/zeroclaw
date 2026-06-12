## Summary

**Base branch:** `master` (all contributions)

**What changed and why:**
The `matrix-sdk` defaults to a 30-second per-request HTTP timeout while `SyncSettings::default()` sends no `?timeout=` parameter, so the homeserver returns immediately and the SDK busy-polls — every 30-second window then races the HTTP deadline and idle `/sync` requests error out at exactly 30s.

**The fix has two parts:**
1. Set an explicit 60-second `RequestConfig` timeout on the `Client::builder()` so the HTTP layer doesn't fire before a long-poll can complete (new `CLIENT_REQUEST_TIMEOUT` const).
2. Pass a 30-second long-poll timeout to both `sync_once` and the long-running `sync` call so the homeserver holds idle requests open and returns before the HTTP deadline (new `SYNC_LONGPOLL_TIMEOUT` const).

The two constants are documented relative to each other: `SYNC_LONGPOLL_TIMEOUT` must stay strictly below `CLIENT_REQUEST_TIMEOUT` so the HTTP request deadline never fires before the long-poll completes server-side. The existing integration test in the file is also updated to pass `SYNC_LONGPOLL_TIMEOUT` to `sync_once`.

**Scope boundary:** only `crates/zeroclaw-channels/src/matrix.rs`; no runtime, agent, provider, schema, session persistence, or CLI surface touched.

**Blast radius:** Matrix channel only. Worst case if the timeout values are wrong is `/sync` behaves the same way it does today (request times out at 30s) — i.e. the fix can only equal or improve the current behavior on the Matrix path. No other channel is affected.

**Split note:** this is the second half of the now-closed #7119, split per @singlerider's review. The other half (the `trim_history` runtime fix) is in its own PR (linked below). Each PR's body now honestly covers its own diff.

**Linked issue(s):** None — diagnosed from observed 30-second timeout pattern in Matrix sync logs.
**Related PRs:** Companion split-out — `trim_history` orphan-cascade guard in #7403. Half of the original #7119 (to be closed).
**Labels:** `bug`, `risk: medium`, `channel`, `channel:matrix`, `size: XS`.

## Reviewer feedback addressed (since @Audacity88's CHANGES_REQUESTED)

@Audacity88 blocked on the lack of live-or-near-live Matrix evidence that an idle `/sync` no longer errors at the 30-second cadence. Two follow-ups in commit `cb2ccc3a`:

1. **Live daemon evidence** — `matrix.todixuclawbot` ran against a real homeserver after the fix; sync loop logs show no 30s-cadence errors. Excerpt + Element X end-to-end screenshot are in the comments below.
2. **Smoke test added** — new `matrix::tests::live_smoke::idle_sync_does_not_error_at_30s_cadence` (file: `crates/zeroclaw-channels/src/matrix.rs`). `#[ignore]`'d, reuses the existing `ZEROCLAW_MATRIX_SMOKE_*` env contract, soaks an idle channel for `> 30s` (default 35s, tunable via `ZEROCLAW_MATRIX_SMOKE_IDLE_SECS`), and asserts:
   - no `sync_once` call returns an error (primary anti-regression for the 30s HTTP-deadline bug),
   - at least one round-trip durably long-polls (anti-regression for the pre-fix busy-poll pattern),
   - sub-threshold returns stay inside a small budget (defense-in-depth).

   It also emits a one-line `eprintln!` summary so a captured `cargo test -- --ignored --nocapture` run produces the "short Matrix smoke result" log line the reviewer asked for.

Scope still single-file: the new test is inside the existing `#[cfg(test)] mod tests { mod live_smoke { ... } }` block; no production code changed since the original review.

## Validation Evidence (required)

```
cargo check -p zeroclaw-channels
```

**Commands run and tail output:**

- `cargo check -p zeroclaw-channels` against current `upstream/master`:
  ```
  Checking zeroclaw-channels v0.8.0-beta-2 (…/crates/zeroclaw-channels)
  Finished `dev` profile [unoptimized + debuginfo] target(s) in 44.52s
  ```

- Branch is based on `upstream/master` post-#7231 (the `ollama.rs` E0308 revert that was breaking CI on #7119 has landed), so CI on this branch should be green rather than inheriting the prior master breaker.

- Smoke test commit (`cb2ccc3a`) validation locally:
  ```
  cargo fmt   -p zeroclaw-channels --check                                          # clean
  cargo clippy -p zeroclaw-channels --tests --features channel-matrix -- -D warnings # clean
  cargo test  -p zeroclaw-channels --lib  --features channel-matrix matrix::
    # test result: ok. 121 passed; 0 failed; 2 ignored (was 1 — new live smoke is the +1)
  cargo test  -p zeroclaw-channels --lib  --features channel-matrix -- --list --ignored | grep matrix
    matrix::tests::live_smoke::idle_sync_does_not_error_at_30s_cadence: test
    matrix::tests::live_smoke::same_room_partial_draft_lifecycle_uses_real_draft_ids: test
  ```

**Beyond CI — what did you manually verify?**
Confirmed the two constants are ordered correctly (`SYNC_LONGPOLL_TIMEOUT = 30s < CLIENT_REQUEST_TIMEOUT = 60s`) so the homeserver-side long-poll always completes before the HTTP deadline. The `?timeout=` parameter is now sent on every sync call (both `sync_once` for the initial boot and the long-running `sync` loop), and the integration test in the file (`matrix.rs` tests module) was updated to use the same long-poll timeout so it exercises the same path as production.

Live-Matrix soak was run against `matrix.todixuclawbot` after the fix: idle sync loop started cleanly, no 30s-cadence errors observed, end-to-end inbound message through Element X confirmed in the comments. The new `idle_sync_does_not_error_at_30s_cadence` smoke codifies that observation as a runnable test (`#[ignore]`'d so it stays out of CI by default).

**If any command was intentionally skipped, why:** Full `cargo test` on the channels crate was not run in this prep pass; I'd appreciate CI confirming green before merge.

## Security & Privacy Impact (required)

- New permissions, capabilities, or file system access scope? **No**
- New external network calls? **No** (changes timeout parameters on existing Matrix `/sync` calls)
- Secrets / tokens / credentials handling changed? **No**
- PII, real identities, or personal data in diff, tests, fixtures, or docs? **No**

## Compatibility (required)

- Backward compatible? **Yes** — only changes timeout values on existing call sites; no API or config surface change.
- Config / env / CLI surface changed? **No** (the new smoke test reads `ZEROCLAW_MATRIX_SMOKE_IDLE_SECS` / `ZEROCLAW_MATRIX_SMOKE_MIN_LONGPOLL_MS` from env, but these are test-only opt-ins gated behind `#[ignore]`; no production code path reads them.)

## Rollback (required for risk: medium and risk: high)

- **Fast rollback command/path:** `git revert <commit-sha-of-this-PR>` (revert the runtime commit; the test-only commit `cb2ccc3a` is safe to leave in place or revert independently)
- **Feature flags or config toggles:** None
- **Observable failure symptoms:** After revert, idle Matrix sessions will resume the pre-fix behavior: `/sync` requests erroring out at exactly 30 seconds and the SDK busy-polling. Look for repeated `WARN`/`ERROR` entries from `crates/zeroclaw-channels/src/matrix.rs` around the `sync`/`sync_once` paths with a ~30s cadence.
