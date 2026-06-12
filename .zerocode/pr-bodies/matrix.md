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

**Beyond CI — what did you manually verify?**
Confirmed the two constants are ordered correctly (`SYNC_LONGPOLL_TIMEOUT = 30s < CLIENT_REQUEST_TIMEOUT = 60s`) so the homeserver-side long-poll always completes before the HTTP deadline. The `?timeout=` parameter is now sent on every sync call (both `sync_once` for the initial boot and the long-running `sync` loop), and the integration test in the file (`matrix.rs` tests module) was updated to use the same long-poll timeout so it exercises the same path as production.

A full live-Matrix soak (idle channel for > 30s without busy-poll cycling) was not run in this PR — happy to add if reviewers want, but the failure mode is mechanical (request deadline < long-poll window) and the math is now correct.

**If any command was intentionally skipped, why:** Full `cargo test` on the channels crate was not run in this prep pass; I'd appreciate CI confirming green before merge.

## Security & Privacy Impact (required)

- New permissions, capabilities, or file system access scope? **No**
- New external network calls? **No** (changes timeout parameters on existing Matrix `/sync` calls)
- Secrets / tokens / credentials handling changed? **No**
- PII, real identities, or personal data in diff, tests, fixtures, or docs? **No**

## Compatibility (required)

- Backward compatible? **Yes** — only changes timeout values on existing call sites; no API or config surface change.
- Config / env / CLI surface changed? **No**

## Rollback (required for risk: medium and risk: high)

- **Fast rollback command/path:** `git revert <commit-sha-of-this-PR>` (single commit on the branch)
- **Feature flags or config toggles:** None
- **Observable failure symptoms:** After revert, idle Matrix sessions will resume the pre-fix behavior: `/sync` requests erroring out at exactly 30 seconds and the SDK busy-polling. Look for repeated `WARN`/`ERROR` entries from `crates/zeroclaw-channels/src/matrix.rs` around the `sync`/`sync_once` paths with a ~30s cadence.
