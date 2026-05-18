## Summary

- **Base branch:** `master`
- **What changed and why:** Fixed nested Tokio runtime panic in `PostgresMemory::new()` that occurred during gateway startup when using the Postgres memory backend. The bug affected **all** Postgres memory configurations — both `vector_enabled = false` (the reported issue #6472) and `vector_enabled = true` paths. The root cause was that the previous implementation performed PostgreSQL connection and schema initialization on the caller thread. When the gateway invoked `PostgresMemory::new()` from a Tokio worker thread, the sync `postgres` crate's internal `Runtime::block_on()` call triggered a panic. The fix bundles **all** postgres operations (connect, schema init, pgvector setup) into a single `std::thread::spawn` so callers can safely construct `PostgresMemory` from any context, including Tokio runtime threads.
- **Scope boundary:** No changes to async methods (`store`, `recall`, etc.) which already used `run_on_os_thread`. No changes to `PgKnowledgeGraph` (currently unused). No config or API changes.
- **Blast radius:** PostgreSQL memory backend users. Gateway startup with `[memory].backend = "postgres"` and `[storage.provider.config].db_url` will now complete all initialization inside the OS thread spawn.
- **Linked issue(s):** Closes #6472

### Why this fixes the reported bug (vector_enabled = false)

Issue #6472 reported a gateway panic with `[memory.postgres].vector_enabled = false`. The stack trace showed:

```
thread 'tokio-rt-worker' panicked at postgres-0.19.13/src/connection.rs:66:22:
Cannot start a runtime from within a runtime.
```

This panic occurred **before** any pgvector-specific code ran — during the initial PostgreSQL connection and schema setup. The previous code path called `initialize_client()` on the caller thread, which internally used the sync `postgres` crate's `Runtime::block_on()`. Moving **all** postgres operations (not just pgvector setup) into the OS thread fixes this for both the `vector_enabled = false` and `vector_enabled = true` cases.

## Validation Evidence (required)

Local validation is the signal CI cannot replace. Run the full battery and paste literal output (tails, failures, warnings — not "all passed").

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Docs-only changes: replace with markdown lint + link-integrity (`scripts/ci/docs_quality_gate.sh`). Bootstrap scripts: add `bash -n install.sh`.

- **Commands run and tail output:** Unable to run cargo commands locally (no Rust toolchain available in this environment). CI validation will confirm correctness.
- **Beyond CI — what did you manually verified?**
  - Reviewed the diff to ensure all postgres operations (connect, init_schema, try_enable_pgvector) are now inside the OS thread spawn.
  - Added two new regression tests that specifically cover the #6472 bug path:
    - `new_with_pgvector_disabled_does_not_panic()` — tests the exact configuration from #6472 (`vector_enabled = false`)
    - `new_with_pgvector_enabled_does_not_panic()` — tests the pgvector-enabled path
  - Both tests use `std::panic::catch_unwind` around `PostgresMemory::new()` called from within a Tokio runtime context, verifying that the nested runtime panic no longer occurs.
- **If any command was intentionally skipped, why:** No Rust toolchain installed in this session.

## Security & Privacy Impact (required)

Yes/No for each. Answer any `Yes` with a 1–2 sentence explanation.

- New permissions, capabilities, or file system access scope? (`No`)
- New external network calls? (`No`)
- Secrets / tokens / credentials handling changed? (`No`)
- PII, real identities, or personal data in diff, tests, fixtures, or docs? (`No`)

## Compatibility (required)

- Backward compatible? (`Yes`)
- Config / env / CLI surface changed? (`No`)

## Rollback (required for `risk: medium` and `risk: high`)

Low-risk PRs: `git revert <sha>` is the plan unless otherwise noted.

- **Fast rollback command/path:** `git revert <sha>`
- **Feature flags or config toggles:** None
- **Observable failure symptoms:** If rollback needed, PostgreSQL memory backend with pgvector enabled would again panic on gateway startup with `Cannot start a runtime from within a runtime`.