@Audacity88 compile failures on `b14ea90c6` are fixed on head `435abbb` (single commit lineage, rebased onto upstream/master `a674d03d2`, MERGEABLE).

**Restore the `ChatMessage` import in test scope:** done. The rebase onto `a674d03d2` resolved `rpc/dispatch.rs` in favor of upstream's import layout, which dropped the per-test `ChatMessage` imports the branch's tests relied on. The tests module now imports `zeroclaw_api::model_provider::ChatMessage` once at the top of `mod tests`, which covers every affected test (the `E0433`/`E0422` sites at dispatch.rs 10401-13109). Per-function imports in tests that already have their own are untouched.

**Two more rebase fallout sites found and fixed during the focused self-review:**

- `E0063` at dispatch.rs:10826: upstream added a `safeguard_fallback` field to `rpc::turn::TurnOutcome`; the `TurnOutcome::Completed` test initializer now passes `safeguard_fallback: None` (the acp persistence test does not exercise the safeguard path, matching the existing initializer at 10784).
- `E0063` at orchestrator/mod.rs:31173: upstream's `history_crumb_flags` field on `ChannelRuntimeContext` was missing from the `shared_session_new_message_interrupts_only_same_sender` test initializer; added with the same `MAX_CONVERSATION_SENDERS` capacity pattern the other test initializers use.

Self-review of the delta: 2 files, +7/-0, test-only initializers and imports, no production code paths touched, `git diff --check` clean, no `#NNNN` in comments. Local cargo builds stay off (small constrained machine), so exact-head hosted CI is the source of truth: a fresh run is in progress on `435abbb`, and I also opened a fork-internal test PR (Project516/zeroclaw#14) so the full matrix runs on the public fork first. Will report back when the required gate settles.

On your merge-order note for the estimator: agreed, no image weighting on this branch. The gate keeps reading `estimate_history_tokens` as-is, and I'll watch for your estimator PR landing ahead of this one.
