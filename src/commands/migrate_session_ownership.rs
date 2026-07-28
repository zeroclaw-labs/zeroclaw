//! Backfill session ownership metadata for pre-migration sessions.
//!
//! This is an operator-facing admin command. Its human-readable strings are
//! sourced from the Fluent CLI catalog (`cli-migrate-session-ownership-*`);
//! only machine-oriented punctuation/format scaffolding is inline.

use crate::config::Config;
use anyhow::{Context, Result, bail};
use std::io::Write;
use zeroclaw_infra::session_backend::ClaimOutcome;
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

    if list {
        return list_unowned(&*backend);
    }

    if let Some(key) = claim {
        let alias = agent_alias.expect("clap requires --agent-alias with --claim");
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
/// Extracted from `handle` for testability. The backend's native
/// `claim_session_agent_alias` is preferred; when the backend returns
/// `Unsupported`, this falls back to a non-atomic read-then-write path
/// (best effort for backends without atomic claim support).
fn claim_session_ownership(
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
        Err(e) if e.kind() == std::io::ErrorKind::Unsupported => {
            // Backend doesn't track ownership. Fall back to the
            // non-atomic read-then-write path (best effort for
            // backends without atomic claim support).
            match backend.get_session_agent_alias(key) {
                Ok(Some(ref existing)) if existing != alias => {
                    bail!(
                        "{}",
                        get_required_cli_string_with_args(
                            "cli-migrate-session-ownership-err-already-owned",
                            &[("key", key), ("existing", existing)],
                        )
                    );
                }
                Err(e) => return Err(e.into()),
                _ => {}
            }
            backend
                .set_session_agent_alias(key, alias)
                .with_context(|| {
                    get_required_cli_string_with_args(
                        "cli-migrate-session-ownership-err-write",
                        &[("key", key)],
                    )
                })
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
            Err(e) if e.kind() == std::io::ErrorKind::Unsupported => {
                // Fall back to non-atomic get+set for backends without
                // atomic claim support (best effort).
                match backend.get_session_agent_alias(key) {
                    Ok(Some(ref existing)) if existing != &alias => {
                        eprintln!(
                            "{}",
                            get_required_cli_string_with_args(
                                "cli-migrate-session-ownership-skip-owned",
                                &[("key", key), ("existing", existing)],
                            )
                        );
                        skipped += 1;
                        continue;
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
                        continue;
                    }
                    _ => {}
                }
                match backend.set_session_agent_alias(key, &alias) {
                    Ok(()) => {
                        println!(
                            "  {}",
                            get_required_cli_string_with_args(
                                "cli-migrate-session-ownership-claimed-one",
                                &[("key", key), ("alias", &alias)],
                            )
                        );
                        claimed += 1;
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
    let mut result = Vec::new();
    for key in backend.list_sessions() {
        // Skip sessions that already have an owner.
        match backend.get_session_agent_alias(&key) {
            Ok(Some(_)) => continue,
            Err(e) if e.kind() == std::io::ErrorKind::Unsupported => {}
            Err(e) => return Err(e.into()),
            Ok(None) => {}
        }
        let msgs = backend.load(&key);
        if msgs.is_empty() {
            continue;
        }
        result.push((key, msgs.len()));
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
    /// fallback path uses `get_session_agent_alias` + `set_session_agent_alias`
    /// and successfully records the owner.
    #[test]
    fn claim_fallback_on_unsupported_sets_alias() {
        let backend = UnsupportedBackend::new();
        backend.seed("gw_fallback");

        let result = claim_session_ownership(&backend, "gw_fallback", "default");
        assert!(
            result.is_ok(),
            "Unsupported fallback should succeed: {:?}",
            result.err()
        );
        assert_eq!(
            backend
                .get_session_agent_alias("gw_fallback")
                .unwrap()
                .as_deref(),
            Some("default")
        );
    }

    /// Test 3b: When `claim_session_agent_alias` returns `Unsupported` BUT the
    /// session already has a different owner, the fallback path detects the
    /// conflict via `get_session_agent_alias` and bails.
    #[test]
    fn claim_fallback_on_unsupported_conflict_bails() {
        let backend = UnsupportedBackend::new();
        backend.seed("gw_conflict");
        backend
            .set_session_agent_alias("gw_conflict", "alice")
            .unwrap();

        let result = claim_session_ownership(&backend, "gw_conflict", "bob");
        assert!(result.is_err(), "conflict should bail");
        let err_msg = format!("{}", result.unwrap_err());
        // The i18n message varies by locale; the key and existing owner
        // appear in every translation.
        assert!(
            err_msg.contains("gw_conflict"),
            "error should mention session key 'gw_conflict', got: {err_msg}"
        );
        assert!(
            err_msg.contains("alice"),
            "error should mention existing owner 'alice', got: {err_msg}"
        );
        // Session must still be owned by "alice" (not overwritten).
        assert_eq!(
            backend
                .get_session_agent_alias("gw_conflict")
                .unwrap()
                .as_deref(),
            Some("alice")
        );
    }
}
