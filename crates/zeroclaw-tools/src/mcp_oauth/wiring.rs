//! Process-wide wiring for the MCP OAuth manager (design §5).
//!
//! Exactly one [`McpOAuthManager`] may exist per process: it is the live
//! view over the encrypted auth-profile store, and its refresh
//! single-flight only holds within one instance. Connect paths pass
//! `&Config` and get the shared handle; construction happens at most once,
//! lazily, and only when at least one configured MCP server actually
//! requires OAuth.
//!
//! This state lives in `zeroclaw-tools` beside the manager it serves: the
//! runtime's scoped-assembly and daemon paths hold no OAuth state of their
//! own, they call here (see the holding-crate rule in ADR-016). The
//! interactivity posture stays a runtime concern (it is about who owns the
//! human at a terminal, not about tokens).

use std::sync::{Arc, OnceLock};
use zeroclaw_config::schema::Config;

use crate::mcp_oauth::manager::McpOAuthManager;

static MANAGER: OnceLock<Arc<McpOAuthManager>> = OnceLock::new();
/// Latch for the fail-closed construction error: the heartbeat worker
/// re-attempts every cycle while an oauth server stays missing, and the
/// doc below promises the failure is logged ONCE.
static BUILD_FAILURE_LOGGED: OnceLock<()> = OnceLock::new();

/// Whether `configs` contains a server that needs OAuth at all (stdio is
/// exempt: a local child process has no HTTP surface to authorize).
fn any_oauth_server(config: &Config) -> bool {
    config.mcp.enabled
        && config.mcp.servers.iter().any(|server| {
            server.oauth && server.transport != zeroclaw_config::schema::McpTransport::Stdio
        })
}

/// Re-apply `security.nat64_prefixes` to the installed manager (greptile
/// C9). A daemon reload changes this list without rebuilding the
/// process-wide instance, and a stale list would keep classifying newly
/// declared NAT64 translations as ordinary IPv6 (a guard the operator
/// updated would not bite). The parse is idempotent; a malformed reload is
/// skipped with a warn so the last-known-good rules stay in force -
/// dropping the network guard because of a typo would be strictly worse
/// than applying the previous list.
fn refresh_network_rules(manager: &McpOAuthManager, config: &Config) {
    let parsed = match zeroclaw_infra::net_guard::parse_nat64_prefixes(
        &config.security.nat64_prefixes,
        "security.nat64_prefixes",
    ) {
        Ok(parsed) => parsed,
        Err(err) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_attrs(::serde_json::json!({
                        "mcp_oauth": "nat64-reload",
                    })),
                &format!(
                    "MCP OAuth kept the previous NAT64 prefix rules; the reloaded \
                     security.nat64_prefixes are malformed: {err:#}"
                )
            );
            return;
        }
    };
    if parsed != manager.current_nat64_prefixes() {
        manager.refresh_nat64_prefixes(parsed);
    }
}

/// The shared manager, if MCP OAuth is configured. `None` means either no
/// server needs OAuth (nothing to build) or construction failed closed —
/// `secrets.encrypt` off — in which case the failure is logged once and
/// each `oauth = true` connection later surfaces its own actionable error
/// (the registry keeps its per-server fail-open skip).
///
/// The build is lazy so a config reload that adds the first `oauth = true`
/// server after boot still gets a manager. A concurrent-init race resolves
/// by keeping the winner; both contenders read the same durable store, and
/// the losing instance only serves one connect, so the refresh
/// single-flight remains effectively singular.
pub fn shared_oauth_manager(config: &Config) -> Option<Arc<McpOAuthManager>> {
    if let Some(manager) = MANAGER.get() {
        refresh_network_rules(manager, config);
        return Some(Arc::clone(manager));
    }
    if !any_oauth_server(config) {
        return None;
    }
    let built = match McpOAuthManager::from_config(config) {
        Ok(manager) => Arc::new(manager),
        Err(err) => {
            if BUILD_FAILURE_LOGGED.set(()).is_ok() {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "mcp_oauth": "manager-init",
                        })),
                    &format!(
                        "MCP OAuth manager could not be built; oauth servers will fail closed: {err:#}"
                    )
                );
            }
            return None;
        }
    };
    let winner = MANAGER.get_or_init(|| Arc::clone(&built));
    Some(Arc::clone(winner))
}

#[cfg(test)]
mod tests {
    use super::*;

    // `any_oauth_server` is checked as a pure predicate: `shared_oauth_manager`
    // installs a process-wide instance, so only ONE test below may touch it.

    #[test]
    fn detection_covers_default_stdio_and_http() {
        // Default config: nothing to manage.
        assert!(!any_oauth_server(&Config::default()));

        // A stdio child has no HTTP surface to authorize; `oauth = true` on
        // it alone must not build a manager (or its state dir).
        let mut stdio_only = Config::default();
        stdio_only.mcp.enabled = true;
        stdio_only
            .mcp
            .servers
            .push(zeroclaw_config::schema::McpServerConfig {
                name: "local".to_string(),
                transport: zeroclaw_config::schema::McpTransport::Stdio,
                command: "/bin/true".to_string(),
                oauth: true,
                ..Default::default()
            });
        assert!(!any_oauth_server(&stdio_only));

        // The flag on a remote transport is what arms OAuth.
        let mut http = stdio_only.clone();
        http.mcp.servers[0].transport = zeroclaw_config::schema::McpTransport::Http;
        http.mcp.servers[0].url = Some("https://mcp.example.test/mcp".to_string());
        assert!(any_oauth_server(&http));

        // Master switch off overrides everything.
        http.mcp.enabled = false;
        assert!(!any_oauth_server(&http));
    }

    #[test]
    fn manager_lifecycle_encrypted_builds_once_unencrypted_fails_open() {
        // The unencrypted posture must be exercised BEFORE the encrypted
        // one: this is the only test that installs the process-wide
        // instance, and the None path must not install anything either.
        let mut config = Config::default();
        config.mcp.enabled = true;
        config
            .mcp
            .servers
            .push(zeroclaw_config::schema::McpServerConfig {
                name: "remote".to_string(),
                transport: zeroclaw_config::schema::McpTransport::Http,
                url: Some("https://mcp.example.test/mcp".to_string()),
                oauth: true,
                ..Default::default()
            });

        // Secrets unencrypted: the token store refuses to persist, the
        // wiring logs and returns None so each oauth server later reports
        // its own actionable error instead of the registry dying here.
        config.secrets.encrypt = false;
        assert!(shared_oauth_manager(&config).is_none());

        // Encryption on: the manager builds and is installed exactly once.
        config.secrets.encrypt = true;
        let first = shared_oauth_manager(&config).expect("oauth server builds the manager");
        let second = shared_oauth_manager(&config).expect("same for the next connect");
        assert!(
            Arc::ptr_eq(&first, &second),
            "one process-wide instance, one refresh single-flight"
        );

        // Even a config with no oauth servers now gets the installed
        // handle: the connect path ignores it unless a server asks.
        let plain = Config::default();
        assert!(Arc::ptr_eq(
            &shared_oauth_manager(&plain).expect("installed stays shared"),
            &first
        ));
        assert!(
            first.current_nat64_prefixes().is_empty(),
            "default config declares no NAT64 translators"
        );

        // C9: a reload that edits security.nat64_prefixes must take effect
        // on the cached instance (no restart), while a malformed reload
        // keeps the last-known-good rules.
        let mut reloaded = plain.clone();
        reloaded
            .security
            .nat64_prefixes
            .push("64:ff9b:1::/48".to_string());
        shared_oauth_manager(&reloaded).expect("shared");
        assert_eq!(
            first.current_nat64_prefixes().len(),
            1,
            "the reloaded prefix list must be live without a restart"
        );

        let mut broken = plain.clone();
        broken
            .security
            .nat64_prefixes
            .push("not-a-prefix".to_string());
        shared_oauth_manager(&broken).expect("shared");
        assert_eq!(
            first.current_nat64_prefixes().len(),
            1,
            "a malformed reload must not drop the working guard"
        );

        // Clearing the setting is a valid reload, not a malformed one.
        shared_oauth_manager(&plain).expect("shared");
        assert!(
            first.current_nat64_prefixes().is_empty(),
            "an explicit empty list must clear the rules"
        );
    }
}
