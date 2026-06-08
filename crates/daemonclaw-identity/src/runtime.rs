//! Runtime options passed to identity providers at construction time.
//!
//! Mirrors `daemonclaw-providers::ProviderRuntimeOptions` in shape: a flat
//! bag of values the factory reads from config and passes through. The
//! factory pattern (`create_identity_provider`) is the single point where
//! these values are computed, so providers don't have to plumb
//! `Config` themselves.

use std::path::PathBuf;

/// Per-process identity runtime context.
#[derive(Debug, Clone)]
pub struct IdentityRuntimeOptions {
    /// Directory holding identity artifacts. Default:
    /// `~/.daemonclaw/identity`.
    pub identity_dir: PathBuf,
    /// Host label used to derive artifact basenames (the `<host>` in
    /// `<host>.spki.pem` and `identity_state.json`). Defaults to the
    /// system hostname.
    pub host_label: String,
    /// Whether the SecretStore is enabled in this process. When false,
    /// the state file is written as plaintext (with a warning) — useful
    /// for sovereign mode.
    pub secrets_encrypt: bool,
    /// Optional base URL for the issuer. Used by `WardTokenIdentityProvider`
    /// to construct the `/api/v1/agent/verify` endpoint. The local
    /// provider ignores it.
    pub issuer_url: Option<String>,
    /// Optional agent_user_id override. The local provider uses this
    /// only as a label hint — its actual id is generated and persisted
    /// locally on first boot.
    pub agent_user_id_hint: Option<String>,
    /// Optional grantor_user_id. The local provider stores this for
    /// assertion self-verify tests; it never goes to a network.
    pub grantor_user_id: Option<String>,
}

impl Default for IdentityRuntimeOptions {
    fn default() -> Self {
        let host = hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_else(|| "localhost".to_string());
        // Default identity_dir = $HOME/.daemonclaw/identity.
        // We resolve HOME explicitly to avoid a `directories` dep.
        let home = std::env::var("HOME")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| ".".to_string());
        let identity_dir = PathBuf::from(home).join(".daemonclaw").join("identity");
        Self {
            identity_dir,
            host_label: host,
            secrets_encrypt: true,
            issuer_url: None,
            agent_user_id_hint: None,
            grantor_user_id: None,
        }
    }
}
