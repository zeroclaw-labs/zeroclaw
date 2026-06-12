## Summary

**Base branch:** `master` (all contributions)

**What changed and why:**
Added a safety guard in `trim_history` (`agent.rs`) that detects when the orphan-removal cascade would drain all non-system messages from conversation history and skips the trim instead of producing an empty `messages: []` array.

**Root cause:** with `max_history_messages` defaulting to 50 (the runtime-profile default when `[runtime_profiles.<profile>]` does not set it), a long single-turn tool loop accumulates `[user_msg, AC1, TR1, …, AC25, TR25]` = 51 entries. `initial_drop_count = 1` removes `user_msg`; the orphan-`AssistantToolCalls` cascade then sweeps every remaining AC/TR pair to end-of-list, leaving only the system message. `convert_messages` extracts the system message as `system_prompt`, producing `messages: []`, which Anthropic rejects with HTTP 400 `"messages: at least one message is required."`

**The fix:** after the cascade loops advance `drop_count` to `other_messages.len()`, restore history unchanged and emit a `WARN` log. The session stays temporarily over the message limit but continues to function. A companion config fix (outside this diff) adds `max_history_messages = 500` to the relevant runtime profile so the default no longer triggers this edge case under normal workloads.

**Scope boundary:** only `trim_history` in `crates/zeroclaw-runtime/src/agent/agent.rs`; no provider code, schema, session persistence, or CLI surface touched.

**Blast radius:** agents running long single-turn tool loops with a low effective `max_history_messages`. The change turns a hard-crash 400 into a temporary over-limit; no other code path is affected.

**Split note:** this replaces #7119, which inadvertently bundled an unrelated Matrix `/sync` long-poll fix in the same branch. The Matrix change is now its own PR (linked below). Per @singlerider's review on #7119, each PR's body now honestly covers its own diff.

**Linked issue(s):** None — diagnosed from `runtime-trace.jsonl` (session `ce3e5d7b`, 2026-06-02).
**Related PRs:** Companion split-out — Matrix sync fix in #7404. Supersedes #7119 (to be closed).
**Labels:** `bug`, `risk: high` (matches runtime-path auto-label from #7119), `agent`, `runtime`, `size: XS`.

## Validation Evidence (required)

```
cargo check -p zeroclaw-runtime
cargo test -p zeroclaw-runtime --lib agent::agent::tests::trim_history
```

**Commands run and tail output:**

- `cargo check -p zeroclaw-runtime` against current `upstream/master`:
  ```
  Checking zeroclaw-runtime v0.8.0-beta-2 (…/crates/zeroclaw-runtime)
  Finished `dev` profile [unoptimized + debuginfo] target(s) in 43.26s
  ```

- `cargo test -p zeroclaw-runtime --lib agent::agent::tests::trim_history` against current `upstream/master`:
  ```
  running 2 tests
  test agent::agent::tests::trim_history_does_not_leave_orphan_assistant_tool_calls ... ok
  test agent::agent::tests::trim_history_does_not_leave_orphan_tool_results ... ok
  test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 2013 filtered out; finished in 0.00s
  ```

- Branch is based on `upstream/master` post-#7231 (the `ollama.rs` E0308 revert that was breaking CI on #7119 has landed), so CI on this branch should be green rather than inheriting the prior master breaker.

**Beyond CI — what did you manually verify?**
Reproduced the failure sequence from `runtime-trace.jsonl`: session `ce3e5d7b` on 2026-06-02 ran 24+ tool-call iterations in Turn 2, causing 400 at iteration 25. Traced through `trim_history` cascade logic to confirm `drop_count` reaches `other_messages.len()` when `other_messages = [user_msg, AC1…AC25, TR1…TR25]` and `initial_drop_count = 1`. Did not exercise the WARN log path in a live session; that requires a session with exactly `max + 1` non-system history entries where the `+1` is the sole user message.

**If any command was intentionally skipped, why:** None skipped.

## Security & Privacy Impact (required)

- New permissions, capabilities, or file system access scope? **No**
- New external network calls? **No**
- Secrets / tokens / credentials handling changed? **No**
- PII, real identities, or personal data in diff, tests, fixtures, or docs? **No**

## Compatibility (required)

- Backward compatible? **Yes** — the only behavioral change is in the edge case where `trim_history` previously emptied history (causing a 400); now it skips the trim instead.
- Config / env / CLI surface changed? **No**

## Rollback (required for risk: medium and risk: high)

- **Fast rollback command/path:** `git revert <commit-sha-of-this-PR>` (single commit on the branch)
- **Feature flags or config toggles:** None
- **Observable failure symptoms:** After revert, sessions with long single-turn tool loops may again produce `WARN "Streaming error:"` followed by `ERROR "turn failed"` with body `"messages: at least one message is required"` in `runtime-trace.jsonl`. Look for `"_file": "crates/zeroclaw-providers/src/reliable.rs"` + `"status": 400` entries.
