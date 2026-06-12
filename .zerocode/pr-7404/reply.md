@Audacity88 thanks for the clear blocker — addressed in `cb2ccc3a`.

**Smoke test added.** New `matrix::tests::live_smoke::idle_sync_does_not_error_at_30s_cadence` in `crates/zeroclaw-channels/src/matrix.rs`, joining the existing `#[ignore]`'d live-smoke lane (same `ZEROCLAW_MATRIX_SMOKE_*` env contract as the sibling draft-lifecycle test, so it stays out of CI by default and is opt-in locally).

It does exactly the loop you asked for:

- builds a `MatrixChannel` from env creds and calls `ensure_client()` — this exercises the new `CLIENT_REQUEST_TIMEOUT` on `Client::builder()`'s `RequestConfig`,
- primes the sync token with one bounded `sync_once`,
- then loops `sync_once(SyncSettings::default().timeout(SYNC_LONGPOLL_TIMEOUT))` for `> 30s` of wall-time (default 35s, tunable via `ZEROCLAW_MATRIX_SMOKE_IDLE_SECS`) against an otherwise-idle room,
- asserts **no `sync_once` call returns an error** — that's the primary anti-regression for the 30s HTTP-deadline bug,
- and as defense-in-depth asserts that at least one round-trip durably long-polls (≥ `ZEROCLAW_MATRIX_SMOKE_MIN_LONGPOLL_MS`, default 1s) and that sub-threshold returns stay inside a small budget, so a regression that reintroduces the busy-poll pattern also trips the test.

It also emits a one-line `eprintln!` summary so a captured `cargo test -- --ignored --nocapture` run produces the "short Matrix smoke result" log line you asked for.

**Near-live evidence** is already in the previous comments — daemon log excerpt showing the sync loop entering cleanly with no 30s-cadence errors, plus the Element X end-to-end screenshot.

**Local validation of the test commit:**

```
cargo fmt   -p zeroclaw-channels --check                                          # clean
cargo clippy -p zeroclaw-channels --tests --features channel-matrix -- -D warnings # clean
cargo test  -p zeroclaw-channels --lib  --features channel-matrix matrix::
  # test result: ok. 121 passed; 0 failed; 2 ignored (was 1 — new live smoke is the +1)
cargo test  -p zeroclaw-channels --lib  --features channel-matrix -- --list --ignored | grep matrix
  matrix::tests::live_smoke::idle_sync_does_not_error_at_30s_cadence: test
  matrix::tests::live_smoke::same_room_partial_draft_lifecycle_uses_real_draft_ids: test
```

Scope is still single-file (the new test sits inside the existing `#[cfg(test)] mod tests { mod live_smoke { ... } }` block in `matrix.rs`); no production code changed since your review. PR body updated to reflect the new evidence and to drop the now-stale "happy to add if reviewers want" note.
