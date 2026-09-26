//! Backfill session ownership metadata for pre-migration sessions.
//!
//! This is an operator-facing admin command. Its human-readable strings are
//! sourced from the Fluent CLI catalog (`cli-migrate-session-ownership-*`);
//! only machine-oriented punctuation/format scaffolding is inline.

use crate::config::Config;
use anyhow::{Context, Result, bail};
use std::io::Write;
use zeroclaw_infra::session_backend::{AdoptOutcome, ClaimOutcome};
use zeroclaw_runtime::i18n::{get_required_cli_string, get_required_cli_string_with_args};

pub fn handle(
    list: bool,
    claim: Option<String>,
    agent_alias: Option<String>,
    yes: bool,
    config: &Config,
) -> Result<()> {
    let backend =
        zeroclaw_infra::make_session_backend(&config.data_dir, &config.channels.session_backend)
            .context(get_required_cli_string(
                "cli-migrate-session-ownership-err-open-backend",
            ))?;

    // Refuse to run on a backend that does not implement atomic
    // ownership claim. The previous `get + set` fallback opened a
    // TOCTOU window (a concurrent owner could appear between the
    // two calls and be silently overwritten). For an operator
    // command whose entire safety story is the atomic claim,
    // degrading to non-atomic read-then-overwrite is worse than
    // refusing — the operator can re-run on a backend that does
    // implement the contract, or fix the backend first.
    //
    // `--list` is still allowed on unsupported backends because it
    // only reads and does not touch ownership.
    if !list && !backend.supports_atomic_claim() {
        bail!(get_required_cli_string(
            "cli-migrate-session-ownership-err-backend-unsupported"
        ));
    }

    if list {
        return list_unowned(&*backend);
    }

    if let Some(key) = claim {
        // clap's `requires = "agent_alias"` on `--claim` enforces this
        // at parse time, so a missing `--agent-alias` is rejected with
        // a clean CLI error before the handler is ever called. The
        // explicit `bail!` below is defence in depth: if the clap
        // relationship is ever removed, we still refuse to write
        // ownership without a target agent (the session would
        // otherwise be permanently inaccessible). See
        // `cli-migrate-session-ownership-err-missing-alias`.
        let alias = match agent_alias {
            Some(a) if !a.is_empty() => a,
            _ => {
                bail!(get_required_cli_string(
                    "cli-migrate-session-ownership-err-missing-alias"
                ));
            }
        };
        // Preflight: refuse to write metadata for an unconfigured agent or a
        // session that does not exist. Either would create dangling ownership
        // (a session bound to a nonexistent agent, permanently inaccessible)
        // or a ghost session row/sidecar the backend would otherwise UPSERT.
        preflight_claim(config, &*backend, &key, &alias)?;

        if !yes {
            print!(
                "{} ",
                get_required_cli_string_with_args(
                    "cli-migrate-session-ownership-confirm-claim",
                    &[("key", &key), ("alias", &alias)],
                )
            );
            std::io::stdout().flush().ok();
            let mut input = String::new();
            std::io::stdin().read_line(&mut input).ok();
            if !input.trim().eq_ignore_ascii_case("y") {
                println!(
                    "{}",
                    get_required_cli_string("cli-migrate-session-ownership-aborted")
                );
                return Ok(());
            }
        }
        // Atomically claim ownership so a concurrent gateway request
        // cannot claim the session between our read and write.
        claim_session_ownership(&*backend, &key, &alias)?;
        println!(
            "{}",
            get_required_cli_string_with_args(
                "cli-migrate-session-ownership-claimed-one",
                &[("key", &key), ("alias", &alias)],
            )
        );
        return Ok(());
    }

    // Interactive bulk claim.
    bulk_claim(config, &*backend)
}

/// Atomically claim session ownership for an agent alias.
///
/// `pub(crate)` so direct programmatic callers (and the unit tests in
/// this module) can drive the helper without going through the CLI
/// argument parser. The CLI `handle` short-circuits at entry on
/// `!backend.supports_atomic_claim()` and refuses to run, so the
/// `Err(Unsupported)` fallback in this helper is **not** reachable
/// from the CLI. It exists for direct callers that want a best-effort
/// read-then-write on backends without atomic claim support.
pub(crate) fn claim_session_ownership(
    backend: &dyn zeroclaw_infra::session_backend::SessionBackend,
    key: &str,
    alias: &str,
) -> Result<()> {
    match backend.claim_session_agent_alias(key, alias) {
        Ok(ClaimOutcome::Claimed) => Ok(()),
        Ok(ClaimOutcome::Conflict(existing)) => {
            bail!(
                "{}",
                get_required_cli_string_with_args(
                    "cli-migrate-session-ownership-err-already-owned",
                    &[("key", key), ("existing", &existing)],
                )
            );
        }
        // Ownerless (possibly non-empty) session: preflight + `-y` have already
        // confirmed the operator targets this key, so adopt it atomically—
        // only when no owner exists, never overwriting a concurrent claim,
        // resolving a deleted session to `Missing`.
        Ok(ClaimOutcome::NeedsMigration) => match backend.adopt_session_agent_alias(key, alias)? {
            AdoptOutcome::Adopted => Ok(()),
            AdoptOutcome::Conflict(existing) => bail!(
                "{}",
                get_required_cli_string_with_args(
                    "cli-migrate-session-ownership-err-already-owned",
                    &[("key", key), ("existing", &existing)],
                )
            ),
            AdoptOutcome::Missing => bail!(
                "{}",
                get_required_cli_string_with_args(
                    "cli-migrate-session-ownership-err-unknown-session",
                    &[("key", key)],
                )
            ),
        },
        Err(e) if e.kind() == std::io::ErrorKind::Unsupported => {
            // Fail closed: no read-then-write fallback here. The `+ set`
            // path had a TOCTOU window (a concurrent owner could appear
            // between the two calls and be overwritten); the CLI already
            // refuses unsupported backends at `handle`, so this only guards
            // direct callers.
            Err(e).with_context(|| {
                get_required_cli_string("cli-migrate-session-ownership-err-backend-unsupported")
            })?
        }
        Err(e) => Err(e).with_context(|| {
            get_required_cli_string_with_args(
                "cli-migrate-session-ownership-err-write",
                &[("key", key)],
            )
        }),
    }
}

/// Verify the two invariants a claim depends on before any write:
/// the target agent alias is configured, and the session already exists.
fn preflight_claim(
    config: &Config,
    backend: &dyn zeroclaw_infra::session_backend::SessionBackend,
    key: &str,
    alias: &str,
) -> Result<()> {
    if config.agent(alias).is_none() {
        bail!(
            "{}",
            get_required_cli_string_with_args(
                "cli-migrate-session-ownership-err-unknown-agent",
                &[("alias", alias)],
            )
        );
    }
    if !backend.session_exists(key) {
        bail!(
            "{}",
            get_required_cli_string_with_args(
                "cli-migrate-session-ownership-err-unknown-session",
                &[("key", key)],
            )
        );
    }
    Ok(())
}

fn bulk_claim(
    config: &Config,
    backend: &dyn zeroclaw_infra::session_backend::SessionBackend,
) -> Result<()> {
    let unowned = collect_unowned(backend)?;
    if unowned.is_empty() {
        println!(
            "{}",
            get_required_cli_string("cli-migrate-session-ownership-none-found")
        );
        return Ok(());
    }
    println!(
        "{}",
        get_required_cli_string_with_args(
            "cli-migrate-session-ownership-found-count",
            &[("count", &unowned.len().to_string())],
        )
    );
    for (key, msg_count) in &unowned {
        println!(
            "  {}",
            get_required_cli_string_with_args(
                "cli-migrate-session-ownership-list-item",
                &[("key", key), ("count", &msg_count.to_string())],
            )
        );
    }
    print!(
        "{} ",
        get_required_cli_string("cli-migrate-session-ownership-prompt-alias")
    );
    std::io::stdout().flush().ok();
    let mut alias = String::new();
    std::io::stdin().read_line(&mut alias).ok();
    let alias = alias.trim().to_string();
    if alias.is_empty() {
        println!(
            "{}",
            get_required_cli_string("cli-migrate-session-ownership-no-alias")
        );
        return Ok(());
    }

    // B1: preflight the whole batch before writing anything. The alias must be
    // a configured agent; sessions came from list_sessions so they exist. If
    // the alias is unknown, fail closed with zero writes.
    if config.agent(&alias).is_none() {
        bail!(
            "{}",
            get_required_cli_string_with_args(
                "cli-migrate-session-ownership-err-unknown-agent",
                &[("alias", &alias)],
            )
        );
    }

    // Per-item pass: never abort mid-batch on a single write failure — record
    // and continue, then summarize and exit non-zero if anything failed.
    let mut claimed = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;
    for (key, _) in &unowned {
        match backend.claim_session_agent_alias(key, &alias) {
            Ok(ClaimOutcome::Claimed) => {
                println!(
                    "  {}",
                    get_required_cli_string_with_args(
                        "cli-migrate-session-ownership-claimed-one",
                        &[("key", key), ("alias", &alias)],
                    )
                );
                claimed += 1;
            }
            Ok(ClaimOutcome::Conflict(existing)) => {
                // A transport atomically claimed this session since
                // collect_unowned ran. Don't overwrite — skip it.
                eprintln!(
                    "{}",
                    get_required_cli_string_with_args(
                        "cli-migrate-session-ownership-skip-owned",
                        &[("key", key), ("existing", &existing)],
                    )
                );
                skipped += 1;
            }
            // Bulk already collected these as ownerless and the operator
            // confirmed the alias interactively, so the migration CLI is the
            // trusted path that may adopt. `adopt_session_agent_alias` is
            // atomic and presence-checked: a concurrent transport claim is
            // skipped (never overwritten), and a session deleted since
            // collection resolves to `Missing` rather than a ghost row.
            Ok(ClaimOutcome::NeedsMigration) => {
                match backend.adopt_session_agent_alias(key, &alias) {
                    Ok(AdoptOutcome::Adopted) => {
                        println!(
                            "  {}",
                            get_required_cli_string_with_args(
                                "cli-migrate-session-ownership-claimed-one",
                                &[("key", key), ("alias", &alias)],
                            )
                        );
                        claimed += 1;
                    }
                    Ok(AdoptOutcome::Conflict(existing)) => {
                        eprintln!(
                            "{}",
                            get_required_cli_string_with_args(
                                "cli-migrate-session-ownership-skip-owned",
                                &[("key", key), ("existing", &existing)],
                            )
                        );
                        skipped += 1;
                    }
                    Ok(AdoptOutcome::Missing) => {
                        eprintln!(
                            "{}",
                            get_required_cli_string_with_args(
                                "cli-migrate-session-ownership-skip-error",
                                &[
                                    ("key", key),
                                    (
                                        "error",
                                        &get_required_cli_string_with_args(
                                            "cli-migrate-session-ownership-err-unknown-session",
                                            &[("key", key)],
                                        )
                                    )
                                ],
                            )
                        );
                        skipped += 1;
                    }
                    Err(e) => {
                        eprintln!(
                            "{}",
                            get_required_cli_string_with_args(
                                "cli-migrate-session-ownership-skip-error",
                                &[("key", key), ("error", &e.to_string())],
                            )
                        );
                        failed += 1;
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Unsupported => {
                // Fail closed uniformly (no get+set write fallback, which had
                // a TOCTOU window). `handle` already refuses unsupported
                // backends; this only guards direct callers.
                eprintln!(
                    "{}",
                    get_required_cli_string_with_args(
                        "cli-migrate-session-ownership-skip-error",
                        &[("key", key), ("error", &e.to_string())],
                    )
                );
                failed += 1;
            }
            Err(e) => {
                eprintln!(
                    "{}",
                    get_required_cli_string_with_args(
                        "cli-migrate-session-ownership-skip-error",
                        &[("key", key), ("error", &e.to_string())],
                    )
                );
                failed += 1;
            }
        }
    }

    println!(
        "{}",
        get_required_cli_string_with_args(
            "cli-migrate-session-ownership-summary",
            &[
                ("claimed", &claimed.to_string()),
                ("skipped", &skipped.to_string()),
                ("failed", &failed.to_string()),
            ],
        )
    );
    if failed > 0 {
        bail!(
            "{}",
            get_required_cli_string_with_args(
                "cli-migrate-session-ownership-err-partial",
                &[("failed", &failed.to_string())],
            )
        );
    }
    Ok(())
}

fn list_unowned(backend: &dyn zeroclaw_infra::session_backend::SessionBackend) -> Result<()> {
    let unowned = collect_unowned(backend)?;
    if unowned.is_empty() {
        println!(
            "{}",
            get_required_cli_string("cli-migrate-session-ownership-none-found")
        );
        return Ok(());
    }
    println!(
        "{}",
        get_required_cli_string("cli-migrate-session-ownership-list-header")
    );
    for (key, msg_count) in &unowned {
        println!(
            "  {}",
            get_required_cli_string_with_args(
                "cli-migrate-session-ownership-list-item",
                &[("key", key), ("count", &msg_count.to_string())],
            )
        );
    }
    Ok(())
}

fn collect_unowned(
    backend: &dyn zeroclaw_infra::session_backend::SessionBackend,
) -> Result<Vec<(String, usize)>> {
    // Use per-row metadata (`message_count`) instead of loading each full
    // transcript just to count messages. sqlite returns the count straight
    // from the `message_count` column; backends that report no metadata fall
    // back to a load.
    let mut counts: std::collections::HashMap<String, usize> = backend
        .list_sessions_with_metadata()
        .into_iter()
        .map(|m| (m.key, m.message_count))
        .collect();
    let mut result = Vec::new();
    for key in backend.list_sessions() {
        // Skip sessions that already have an owner.
        match backend.get_session_agent_alias(&key) {
            Ok(Some(_)) => continue,
            Err(e) if e.kind() == std::io::ErrorKind::Unsupported => {}
            Err(e) => return Err(e.into()),
            Ok(None) => {}
        }
        // Trust a positive metadata count (sqlite tracks it in the column);
        // a zero/absent count is not reliable (JSONL hard-codes 0), so fall
        // back to a real load to keep the empty/non-empty decision exact.
        let msgs = match counts.remove(&key) {
            Some(n) if n > 0 => n,
            _ => backend.load(&key).len(),
        };
        if msgs == 0 {
            continue;
        }
        result.push((key, msgs));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use zeroclaw_infra::session_backend::{ClaimOutcome, SessionBackend};
    use zeroclaw_providers::ChatMessage;

    type SessionData = (Vec<ChatMessage>, Option<String>);

    /// Mock backend that returns `Unsupported` from `claim_session_agent_alias`
    /// but supports `get_session_agent_alias` and `set_session_agent_alias`.
    struct UnsupportedBackend {
        sessions: Mutex<HashMap<String, SessionData>>,
    }

    impl UnsupportedBackend {
        fn new() -> Self {
            Self {
                sessions: Mutex::new(HashMap::new()),
            }
        }

        fn seed(&self, key: &str) {
            let mut sessions = self.sessions.lock().unwrap();
            sessions
                .entry(key.to_string())
                .or_insert_with(|| (vec![], None))
                .0
                .push(ChatMessage::user("hello"));
        }
    }

    impl SessionBackend for UnsupportedBackend {
        fn load(&self, session_key: &str) -> Vec<ChatMessage> {
            self.sessions
                .lock()
                .unwrap()
                .get(session_key)
                .map(|(msgs, _)| msgs.clone())
                .unwrap_or_default()
        }

        fn append(&self, _session_key: &str, _message: &ChatMessage) -> std::io::Result<()> {
            Ok(())
        }

        fn remove_last(&self, _session_key: &str) -> std::io::Result<bool> {
            Ok(false)
        }

        fn list_sessions(&self) -> Vec<String> {
            self.sessions.lock().unwrap().keys().cloned().collect()
        }

        fn session_exists(&self, session_key: &str) -> bool {
            self.sessions.lock().unwrap().contains_key(session_key)
        }

        fn get_session_agent_alias(&self, session_key: &str) -> std::io::Result<Option<String>> {
            Ok(self
                .sessions
                .lock()
                .unwrap()
                .get(session_key)
                .and_then(|(_, alias)| alias.clone()))
        }

        fn set_session_agent_alias(
            &self,
            session_key: &str,
            agent_alias: &str,
        ) -> std::io::Result<()> {
            self.sessions
                .lock()
                .unwrap()
                .entry(session_key.to_string())
                .and_modify(|(_, alias)| *alias = Some(agent_alias.to_string()));
            Ok(())
        }

        fn claim_session_agent_alias(
            &self,
            _session_key: &str,
            _agent_alias: &str,
        ) -> std::io::Result<ClaimOutcome> {
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "mock: claim unsupported",
            ))
        }
    }

    /// Test 3a: When `claim_session_agent_alias` returns `Unsupported`, the
    /// claim fails closed — no read-then-write fallback writes an owner.
    #[test]
    fn claim_unsupported_fails_closed() {
        let backend = UnsupportedBackend::new();
        backend.seed("gw_fallback");

        let result = claim_session_ownership(&backend, "gw_fallback", "default");
        let err = result.expect_err("Unsupported claim must error");
        let unsupported_in_chain = err.chain().any(|cause| {
            cause
                .downcast_ref::<std::io::Error>()
                .map(std::io::Error::kind)
                == Some(std::io::ErrorKind::Unsupported)
        });
        assert!(
            unsupported_in_chain,
            "fail-closed should propagate the Unsupported error"
        );
    }

    /// Test 3b: with a `Unsupported` claim, even a pre-existing different owner
    /// is left untouched — the fail-closed path never falls through to a
    /// non-atomic write that could silently overwrite a concurrent owner.
    #[test]
    fn claim_unsupported_does_not_write_owner() {
        let backend = UnsupportedBackend::new();
        backend.seed("gw_unsupported");
        backend
            .set_session_agent_alias("gw_unsupported", "alice")
            .unwrap();

        let result = claim_session_ownership(&backend, "gw_unsupported", "bob");
        assert!(result.is_err(), "Unsupported claim must fail closed");
        // alice is preserved (no overwrite via a get+set fallback).
        assert_eq!(
            backend
                .get_session_agent_alias("gw_unsupported")
                .unwrap()
                .as_deref(),
            Some("alice")
        );
    }
}
