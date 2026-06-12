@Audacity88 Thanks — fair call. Pushed `7e9e4a334` adding two regression tests that lock the cascade-to-empty contract.

### What changed

`crates/zeroclaw-runtime/src/agent/agent.rs` (tests-only, +275 lines, no production-code change):

1. **`trim_history_does_not_empty_all_messages_on_full_cascade`** — minimal repro of the production failure you described:
   - `history = [user, AC1, TR1, AC2, TR2]`, `max_history_messages = 4`
   - `initial_drop_count = 1` drops the sole user message
   - orphan-AC cascade then sweeps `AC1 / TR1 / AC2 / TR2`, driving `drop_count` from `1` to `5` (= `other_messages.len()`)
   - asserts the guard fires, history is non-empty, and the full 5-entry history is preserved unchanged
   - also asserts `history.len() > max_history_messages` after the guard fires, codifying the "temporarily over max" trade-off so a future "tighten trim_history" refactor cannot silently regress to empty-messages without breaking a test

2. **`trim_history_full_cascade_with_system_message_preserves_full_history`** — same cascade arithmetic with a system message at the head. Verifies the guard's restore path puts the conversation back together in the right order (`system_messages` first, then non-system entries) rather than dropping the system message or returning the halves reversed. Also asserts at least one non-system message remains — without this, `convert_messages` would lift the system entry into `system_prompt` and the provider would still see `messages: []`.

### Verifying the contract holds

I manually confirmed both new tests would fail on `master` (i.e. without the guard) by locally neutering the guard's early-return and rerunning:

```
running 4 tests
test agent::agent::tests::trim_history_does_not_leave_orphan_assistant_tool_calls ... ok
test agent::agent::tests::trim_history_does_not_leave_orphan_tool_results ... ok
test agent::agent::tests::trim_history_does_not_empty_all_messages_on_full_cascade ... FAILED
test agent::agent::tests::trim_history_full_cascade_with_system_message_preserves_full_history ... FAILED

failures:

---- trim_history_does_not_empty_all_messages_on_full_cascade stdout ----
panicked at agent.rs:5691:9:
trim_history drained every non-system message; the next provider call would fail with 'messages: at least one message is required'

---- trim_history_full_cascade_with_system_message_preserves_full_history stdout ----
panicked at agent.rs:5837:9:
assertion `left == right` failed: trim_history dropped messages from the non-system half despite the orphan cascade reaching other_messages.len(); guard must preserve every entry when it fires
  left: 1
 right: 6
```

The failure messages match the documented failure shape exactly. The two pre-existing orphan tests pass either way (the new tests cover a strictly new contract). Guard was restored before the commit was staged — the staged diff is tests-only.

### Verification on the actual branch

With the guard in place:

```
cargo fmt --all -- --check     # clean
cargo clippy -p zeroclaw-runtime --all-targets -- -D warnings   # clean
cargo test -p zeroclaw-runtime --lib agent::agent::tests::trim_history
  running 4 tests
  test trim_history_does_not_leave_orphan_tool_results ... ok
  test trim_history_does_not_empty_all_messages_on_full_cascade ... ok
  test trim_history_does_not_leave_orphan_assistant_tool_calls ... ok
  test trim_history_full_cascade_with_system_message_preserves_full_history ... ok
  test result: ok. 4 passed; 0 failed
```

Let me know if you'd like a third variant exercising a longer trace (e.g. the actual 25-pair production scenario) — happy to add it, but the 2-pair minimal version drives the exact same code path and is cheaper to read.
