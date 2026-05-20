pub mod anthropic_token;
pub mod gemini_oauth;
pub mod oauth_common;
pub mod openai_oauth;
pub mod profiles;

use crate::auth::openai_oauth::refresh_access_token;
use crate::auth::profiles::{
    AuthProfile, AuthProfileKind, AuthProfilesData, AuthProfilesStore, TokenSet, profile_id,
};
use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use zeroclaw_config::schema::Config;

const OPENAI_CODEX_PROVIDER: &str = "openai-codex";
const ANTHROPIC_PROVIDER: &str = "anthropic";
const GEMINI_PROVIDER: &str = "gemini";
const DEFAULT_PROFILE_NAME: &str = "default";
const OPENAI_REFRESH_SKEW_SECS: u64 = 90;
const OPENAI_REFRESH_FAILURE_BACKOFF_SECS: u64 = 10;
const OAUTH_REFRESH_MAX_ATTEMPTS: usize = 3;
const OAUTH_REFRESH_RETRY_BASE_DELAY_MS: u64 = 350;
static REFRESH_BACKOFFS: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();

#[derive(Clone)]
pub struct AuthService {
    store: AuthProfilesStore,
    client: reqwest::Client,
}

impl AuthService {
    pub fn from_config(config: &Config) -> Self {
        let state_dir = state_dir_from_config(config);
        Self::new(&state_dir, config.secrets.encrypt)
    }

    pub fn new(state_dir: &Path, encrypt_secrets: bool) -> Self {
        Self {
            store: AuthProfilesStore::new(state_dir, encrypt_secrets),
            client: reqwest::Client::new(),
        }
    }

    pub async fn load_profiles(&self) -> Result<AuthProfilesData> {
        self.store.load().await
    }

    pub async fn store_openai_tokens(
        &self,
        profile_name: &str,
        token_set: crate::auth::profiles::TokenSet,
        account_id: Option<String>,
        set_active: bool,
    ) -> Result<AuthProfile> {
        let mut profile = AuthProfile::new_oauth(OPENAI_CODEX_PROVIDER, profile_name, token_set);
        profile.account_id = account_id;
        self.store
            .upsert_profile(profile.clone(), set_active)
            .await?;
        Ok(profile)
    }

    pub async fn store_gemini_tokens(
        &self,
        profile_name: &str,
        token_set: crate::auth::profiles::TokenSet,
        account_id: Option<String>,
        set_active: bool,
    ) -> Result<AuthProfile> {
        let mut profile = AuthProfile::new_oauth(GEMINI_PROVIDER, profile_name, token_set);
        profile.account_id = account_id;
        self.store
            .upsert_profile(profile.clone(), set_active)
            .await?;
        Ok(profile)
    }

    pub async fn store_model_provider_token(
        &self,
        model_provider: &str,
        profile_name: &str,
        token: &str,
        metadata: HashMap<String, String>,
        set_active: bool,
    ) -> Result<AuthProfile> {
        let mut profile = AuthProfile::new_token(model_provider, profile_name, token.to_string());
        profile.metadata.extend(metadata);
        self.store
            .upsert_profile(profile.clone(), set_active)
            .await?;
        Ok(profile)
    }

    pub async fn set_active_profile(
        &self,
        model_provider: &str,
        requested_profile: &str,
    ) -> Result<String> {
        let model_provider = normalize_model_provider(model_provider)?;
        let data = self.store.load().await?;
        let profile_id = resolve_requested_profile_id(&model_provider, requested_profile);

        let profile = data.profiles.get(&profile_id).ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "profile_id": &profile_id,
                        "reason": "auth_profile_not_found",
                    })),
                "auth: profile not found"
            );
            anyhow::Error::msg(format!("Auth profile not found: {profile_id}"))
        })?;

        if profile.model_provider != model_provider {
            anyhow::bail!(
                "Profile {profile_id} belongs to model_provider {}, not {}",
                profile.model_provider,
                model_provider
            );
        }

        self.store
            .set_active_profile(&model_provider, &profile_id)
            .await?;
        Ok(profile_id)
    }

    pub async fn remove_profile(
        &self,
        model_provider: &str,
        requested_profile: &str,
    ) -> Result<bool> {
        let model_provider = normalize_model_provider(model_provider)?;
        let profile_id = resolve_requested_profile_id(&model_provider, requested_profile);
        self.store.remove_profile(&profile_id).await
    }

    pub async fn get_profile(
        &self,
        model_provider: &str,
        profile_override: Option<&str>,
    ) -> Result<Option<AuthProfile>> {
        let model_provider = normalize_model_provider(model_provider)?;
        let data = self.store.load().await?;
        let Some(profile_id) = select_profile_id(&data, &model_provider, profile_override) else {
            return Ok(None);
        };
        Ok(data.profiles.get(&profile_id).cloned())
    }

    pub async fn get_provider_bearer_token(
        &self,
        model_provider: &str,
        profile_override: Option<&str>,
    ) -> Result<Option<String>> {
        let profile = self.get_profile(model_provider, profile_override).await?;
        let Some(profile) = profile else {
            return Ok(None);
        };

        let credential = match profile.kind {
            AuthProfileKind::Token => profile.token,
            AuthProfileKind::OAuth => profile.token_set.map(|t| t.access_token),
        };

        Ok(credential.filter(|t| !t.trim().is_empty()))
    }

    pub async fn get_valid_openai_access_token(
        &self,
        profile_override: Option<&str>,
    ) -> Result<Option<String>> {
        let data = self.store.load().await?;
        let Some(profile_id) = select_profile_id(&data, OPENAI_CODEX_PROVIDER, profile_override)
        else {
            return Ok(None);
        };

        let Some(profile) = data.profiles.get(&profile_id) else {
            return Ok(None);
        };

        let Some(token_set) = profile.token_set.as_ref() else {
            anyhow::bail!("OpenAI Codex auth profile is not OAuth-based: {profile_id}");
        };

        if !token_set.is_expiring_within(Duration::from_secs(OPENAI_REFRESH_SKEW_SECS)) {
            return Ok(Some(token_set.access_token.clone()));
        }

        let Some(refresh_token) = token_set.refresh_token.clone() else {
            return Ok(Some(token_set.access_token.clone()));
        };

        let refresh_lock = refresh_lock_for_profile(&profile_id);
        let _guard = refresh_lock.lock().await;

        // Re-load after waiting for lock to avoid duplicate refreshes.
        let data = self.store.load().await?;
        let Some(latest_profile) = data.profiles.get(&profile_id) else {
            return Ok(None);
        };

        let Some(latest_tokens) = latest_profile.token_set.as_ref() else {
            anyhow::bail!("OpenAI Codex auth profile is missing token set: {profile_id}");
        };

        if !latest_tokens.is_expiring_within(Duration::from_secs(OPENAI_REFRESH_SKEW_SECS)) {
            return Ok(Some(latest_tokens.access_token.clone()));
        }

        let refresh_token = latest_tokens.refresh_token.clone().unwrap_or(refresh_token);

        if let Some(remaining) = refresh_backoff_remaining(&profile_id) {
            anyhow::bail!(
                "OpenAI token refresh is in backoff for {remaining}s due to previous failures"
            );
        }

        let mut refreshed =
            match refresh_openai_access_token_with_retries(&self.client, &refresh_token).await {
                Ok(tokens) => {
                    clear_refresh_backoff(&profile_id);
                    tokens
                }
                Err(err) => {
                    set_refresh_backoff(
                        &profile_id,
                        Duration::from_secs(OPENAI_REFRESH_FAILURE_BACKOFF_SECS),
                    );
                    return Err(err);
                }
            };
        if refreshed.refresh_token.is_none() {
            refreshed
                .refresh_token
                .clone_from(&latest_tokens.refresh_token);
        }

        let account_id = openai_oauth::extract_account_id_from_jwt(&refreshed.access_token)
            .or_else(|| latest_profile.account_id.clone());

        let updated = self
            .store
            .update_profile(&profile_id, |profile| {
                profile.kind = AuthProfileKind::OAuth;
                profile.token_set = Some(refreshed.clone());
                profile.account_id.clone_from(&account_id);
                Ok(())
            })
            .await?;

        Ok(updated.token_set.map(|t| t.access_token))
    }

    /// Get a valid Gemini OAuth access token, refreshing if necessary.
    ///
    /// `client_id` and `client_secret` are the OAuth app credentials from
    /// the per-alias `[model_providers.gemini.<alias>]` typed config —
    /// required when a refresh is triggered. Required when the cached
    /// access token is near expiry; ignored when the access token is
    /// still valid. Pass empty strings only if the caller is certain
    /// the token won't need refresh in this call.
    ///
    /// Returns `None` if no Gemini profile exists.
    pub async fn get_valid_gemini_access_token(
        &self,
        profile_override: Option<&str>,
        client_id: &str,
        client_secret: &str,
    ) -> Result<Option<String>> {
        let data = self.store.load().await?;
        let Some(profile_id) = select_profile_id(&data, GEMINI_PROVIDER, profile_override) else {
            return Ok(None);
        };

        let Some(profile) = data.profiles.get(&profile_id) else {
            return Ok(None);
        };

        let Some(token_set) = profile.token_set.as_ref() else {
            anyhow::bail!("Gemini auth profile is not OAuth-based: {profile_id}");
        };

        if !token_set.is_expiring_within(Duration::from_secs(OPENAI_REFRESH_SKEW_SECS)) {
            return Ok(Some(token_set.access_token.clone()));
        }

        let Some(refresh_token) = token_set.refresh_token.clone() else {
            return Ok(Some(token_set.access_token.clone()));
        };

        let refresh_lock = refresh_lock_for_profile(&profile_id);
        let _guard = refresh_lock.lock().await;

        // Re-load after waiting for lock to avoid duplicate refreshes.
        let data = self.store.load().await?;
        let Some(latest_profile) = data.profiles.get(&profile_id) else {
            return Ok(None);
        };

        let Some(latest_tokens) = latest_profile.token_set.as_ref() else {
            anyhow::bail!("Gemini auth profile is missing token set: {profile_id}");
        };

        if !latest_tokens.is_expiring_within(Duration::from_secs(OPENAI_REFRESH_SKEW_SECS)) {
            return Ok(Some(latest_tokens.access_token.clone()));
        }

        let refresh_token = latest_tokens.refresh_token.clone().unwrap_or(refresh_token);

        if let Some(remaining) = refresh_backoff_remaining(&profile_id) {
            anyhow::bail!(
                "Gemini token refresh is in backoff for {remaining}s due to previous failures"
            );
        }

        let mut refreshed = match refresh_gemini_access_token_with_retries(
            &self.client,
            client_id,
            client_secret,
            &refresh_token,
        )
        .await
        {
            Ok(tokens) => {
                clear_refresh_backoff(&profile_id);
                tokens
            }
            Err(err) => {
                set_refresh_backoff(
                    &profile_id,
                    Duration::from_secs(OPENAI_REFRESH_FAILURE_BACKOFF_SECS),
                );
                return Err(err);
            }
        };
        if refreshed.refresh_token.is_none() {
            refreshed
                .refresh_token
                .clone_from(&latest_tokens.refresh_token);
        }

        let account_id = refreshed
            .id_token
            .as_deref()
            .and_then(gemini_oauth::extract_account_email_from_id_token)
            .or_else(|| latest_profile.account_id.clone());

        let updated = self
            .store
            .update_profile(&profile_id, |profile| {
                profile.kind = AuthProfileKind::OAuth;
                profile.token_set = Some(refreshed.clone());
                profile.account_id.clone_from(&account_id);
                Ok(())
            })
            .await?;

        Ok(updated.token_set.map(|t| t.access_token))
    }

    /// Get Gemini profile info (for model_provider initialization).
    pub async fn get_gemini_profile(
        &self,
        profile_override: Option<&str>,
    ) -> Result<Option<AuthProfile>> {
        self.get_profile(GEMINI_PROVIDER, profile_override).await
    }
}

/// Auth-flow provider — the finite set the `auth login` /
/// `auth paste-redirect` / `auth status` commands dispatch on. Synonym
/// collapse and canonical-name rendering are both serde-driven via the
/// `rename_all` + `alias` attributes, so no string-literal pattern match
/// is needed at the parsing boundary or any dispatch site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthProvider {
    #[serde(alias = "openai_codex", alias = "codex")]
    OpenaiCodex,
    #[serde(alias = "claude")]
    Anthropic,
    #[serde(alias = "google", alias = "vertex")]
    Gemini,
}

impl std::str::FromStr for AuthProvider {
    type Err = anyhow::Error;

    fn from_str(raw: &str) -> Result<Self> {
        let normalized = raw.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            anyhow::bail!("ModelProvider name cannot be empty");
        }
        serde_json::from_value(serde_json::Value::String(normalized.clone())).map_err(|_| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"normalized": &normalized})),
                "auth: unknown auth provider"
            );
            anyhow::Error::msg(format!(
                "Unknown auth provider `{normalized}`. Supported: openai-codex, anthropic, gemini.",
            ))
        })
    }
}

impl AuthProvider {
    /// Canonical lowercase name for storage, profile lookup, and on-the-wire
    /// references. Each arm is enum-variant dispatch — adding a variant
    /// requires updating this match (compile-time enforced).
    pub fn as_canonical(&self) -> &'static str {
        match self {
            Self::OpenaiCodex => OPENAI_CODEX_PROVIDER,
            Self::Anthropic => ANTHROPIC_PROVIDER,
            Self::Gemini => GEMINI_PROVIDER,
        }
    }
}

/// Permissive string-returning normalizer for token-storage callers
/// (paste-token, setup-token, set-active-profile, …) that accept
/// arbitrary provider names. Known OAuth-flow providers collapse to
/// their canonical form via [`AuthProvider`]; unknown names lower-case
/// and pass through unchanged so storage works for any bearer-token
/// provider operators want to support. Empty input is rejected.
///
/// OAuth-dispatch sites (`auth login` / `auth refresh`) parse via
/// [`AuthProvider`] directly — that path is strict by design.
pub fn normalize_model_provider(model_provider: &str) -> Result<String> {
    if let Ok(provider) = model_provider.parse::<AuthProvider>() {
        return Ok(provider.as_canonical().to_string());
    }
    let normalized = model_provider.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        anyhow::bail!("ModelProvider name cannot be empty");
    }
    Ok(normalized)
}

pub fn state_dir_from_config(config: &Config) -> PathBuf {
    config
        .config_path
        .parent()
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
}

pub fn default_profile_id(model_provider: &str) -> String {
    profile_id(model_provider, DEFAULT_PROFILE_NAME)
}

fn resolve_requested_profile_id(model_provider: &str, requested: &str) -> String {
    if requested.contains(':') {
        requested.to_string()
    } else {
        profile_id(model_provider, requested)
    }
}

pub fn select_profile_id(
    data: &AuthProfilesData,
    model_provider: &str,
    profile_override: Option<&str>,
) -> Option<String> {
    if let Some(override_profile) = profile_override {
        let requested = resolve_requested_profile_id(model_provider, override_profile);
        if data.profiles.contains_key(&requested) {
            return Some(requested);
        }
        return None;
    }

    if let Some(active) = data.active_profiles.get(model_provider)
        && data.profiles.contains_key(active)
    {
        return Some(active.clone());
    }

    let default = default_profile_id(model_provider);
    if data.profiles.contains_key(&default) {
        return Some(default);
    }

    data.profiles
        .iter()
        .find_map(|(id, profile)| (profile.model_provider == model_provider).then(|| id.clone()))
}

async fn refresh_openai_access_token_with_retries(
    client: &reqwest::Client,
    refresh_token: &str,
) -> Result<TokenSet> {
    let mut last_error: Option<anyhow::Error> = None;

    for attempt in 1..=OAUTH_REFRESH_MAX_ATTEMPTS {
        match refresh_access_token(client, refresh_token).await {
            Ok(tokens) => return Ok(tokens),
            Err(err) => {
                let should_retry = attempt < OAUTH_REFRESH_MAX_ATTEMPTS;
                ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"attempt": attempt, "max_attempts": OAUTH_REFRESH_MAX_ATTEMPTS, "retry": should_retry, "error": format!("{}", err)})), "OpenAI token refresh failed");
                last_error = Some(err);
                if should_retry {
                    tokio::time::sleep(Duration::from_millis(
                        OAUTH_REFRESH_RETRY_BASE_DELAY_MS * attempt as u64,
                    ))
                    .await;
                }
            }
        }
    }

    Err(last_error.unwrap_or_else(|| {
        ::zeroclaw_log::record!(
            ERROR,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({"oauth_provider": "openai"})),
            "auth: OpenAI token refresh exhausted retries"
        );
        anyhow::Error::msg("OpenAI token refresh failed")
    }))
}

async fn refresh_gemini_access_token_with_retries(
    client: &reqwest::Client,
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
) -> Result<TokenSet> {
    let mut last_error: Option<anyhow::Error> = None;

    for attempt in 1..=OAUTH_REFRESH_MAX_ATTEMPTS {
        match gemini_oauth::refresh_access_token(client, client_id, client_secret, refresh_token)
            .await
        {
            Ok(tokens) => return Ok(tokens),
            Err(err) => {
                let should_retry = attempt < OAUTH_REFRESH_MAX_ATTEMPTS;
                ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"attempt": attempt, "max_attempts": OAUTH_REFRESH_MAX_ATTEMPTS, "retry": should_retry, "error": format!("{}", err)})), "Gemini token refresh failed");
                last_error = Some(err);
                if should_retry {
                    tokio::time::sleep(Duration::from_millis(
                        OAUTH_REFRESH_RETRY_BASE_DELAY_MS * attempt as u64,
                    ))
                    .await;
                }
            }
        }
    }

    Err(last_error.unwrap_or_else(|| {
        ::zeroclaw_log::record!(
            ERROR,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({"oauth_provider": "gemini"})),
            "auth: Gemini token refresh exhausted retries"
        );
        anyhow::Error::msg("Gemini token refresh failed")
    }))
}

fn refresh_lock_for_profile(profile_id: &str) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();

    let table = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = table.lock().expect("refresh lock table poisoned");

    guard
        .entry(profile_id.to_string())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

fn refresh_backoff_remaining(profile_id: &str) -> Option<u64> {
    let map = REFRESH_BACKOFFS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = map.lock().ok()?;
    let now = Instant::now();
    let deadline = guard.get(profile_id).copied()?;
    if deadline <= now {
        guard.remove(profile_id);
        return None;
    }
    Some((deadline - now).as_secs().max(1))
}

fn set_refresh_backoff(profile_id: &str, duration: Duration) {
    let map = REFRESH_BACKOFFS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut guard) = map.lock() {
        guard.insert(profile_id.to_string(), Instant::now() + duration);
    }
}

fn clear_refresh_backoff(profile_id: &str) {
    let map = REFRESH_BACKOFFS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut guard) = map.lock() {
        guard.remove(profile_id);
    }
}

// ════════════════════════════════════════════════════════════════════════
// PendingOAuthLogin — encrypted on-disk state for browser/paste-redirect
// fallback. Moved here from `src/main.rs` so the AuthProviderFlow trait
// impls below can save/load/clear without crossing the bin/lib boundary.
// ════════════════════════════════════════════════════════════════════════

/// Generic pending OAuth login state, shared across model providers.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PendingOAuthLogin {
    /// Canonical model-provider name as stored on disk. Kept as `String`
    /// for serialization compatibility with already-saved files written
    /// before the [`AuthProvider`] enum existed.
    pub model_provider: String,
    pub profile: String,
    pub code_verifier: String,
    pub state: String,
    pub created_at: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct PendingOAuthLoginFile {
    #[serde(default)]
    model_provider: Option<String>,
    profile: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    code_verifier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    encrypted_code_verifier: Option<String>,
    state: String,
    created_at: String,
}

fn pending_oauth_login_path(config: &Config, model_provider: &str) -> PathBuf {
    let filename = format!("auth-{}-pending.json", model_provider);
    state_dir_from_config(config).join(filename)
}

fn pending_oauth_secret_store(config: &Config) -> zeroclaw_config::secrets::SecretStore {
    zeroclaw_config::secrets::SecretStore::new(
        &state_dir_from_config(config),
        config.secrets.encrypt,
    )
}

#[cfg(unix)]
fn set_owner_only_permissions(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_owner_only_permissions(_path: &std::path::Path) -> Result<()> {
    Ok(())
}

pub fn save_pending_oauth_login(config: &Config, pending: &PendingOAuthLogin) -> Result<()> {
    let path = pending_oauth_login_path(config, &pending.model_provider);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let secret_store = pending_oauth_secret_store(config);
    let encrypted_code_verifier = secret_store.encrypt(&pending.code_verifier)?;
    let persisted = PendingOAuthLoginFile {
        model_provider: Some(pending.model_provider.clone()),
        profile: pending.profile.clone(),
        code_verifier: None,
        encrypted_code_verifier: Some(encrypted_code_verifier),
        state: pending.state.clone(),
        created_at: pending.created_at.clone(),
    };
    let tmp = path.with_extension(format!(
        "tmp.{}.{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let json = serde_json::to_vec_pretty(&persisted)?;
    std::fs::write(&tmp, json)?;
    set_owner_only_permissions(&tmp)?;
    std::fs::rename(tmp, &path)?;
    set_owner_only_permissions(&path)?;
    Ok(())
}

pub fn load_pending_oauth_login(
    config: &Config,
    model_provider: &str,
) -> Result<Option<PendingOAuthLogin>> {
    let path = pending_oauth_login_path(config, model_provider);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&path)?;
    if bytes.is_empty() {
        return Ok(None);
    }
    let persisted: PendingOAuthLoginFile = serde_json::from_slice(&bytes)?;
    let secret_store = pending_oauth_secret_store(config);
    let code_verifier = if let Some(encrypted) = persisted.encrypted_code_verifier {
        secret_store.decrypt(&encrypted)?
    } else if let Some(plaintext) = persisted.code_verifier {
        plaintext
    } else {
        anyhow::bail!("Pending {} login is missing code verifier", model_provider);
    };
    Ok(Some(PendingOAuthLogin {
        model_provider: persisted
            .model_provider
            .unwrap_or_else(|| model_provider.to_string()),
        profile: persisted.profile,
        code_verifier,
        state: persisted.state,
        created_at: persisted.created_at,
    }))
}

pub fn clear_pending_oauth_login(config: &Config, model_provider: &str) {
    let path = pending_oauth_login_path(config, model_provider);
    if let Ok(file) = std::fs::OpenOptions::new().write(true).open(&path) {
        let _ = file.set_len(0);
        let _ = file.sync_all();
    }
    let _ = std::fs::remove_file(path);
}

// ════════════════════════════════════════════════════════════════════════
// AuthProviderFlow — per-provider auth flow trait, dispatched via
// `AuthProvider::flow()`. Replaces the string-keyed `match
// model_provider.as_str() { ... }` blocks formerly in `src/main.rs` —
// every dispatch now goes through enum-variant matching followed by
// trait-object virtual call.
// ════════════════════════════════════════════════════════════════════════

/// Shared context for auth-flow trait methods. Carries the runtime
/// dependencies each flow needs (config for OAuth client creds, auth
/// service for token storage, http client for OAuth round-trips).
pub struct AuthFlowContext<'a> {
    pub config: &'a Config,
    pub auth_service: &'a AuthService,
    pub client: &'a reqwest::Client,
}

/// Result of [`AuthProviderFlow::refresh_status`] — caller renders the
/// outcome (CLI message, gateway JSON, etc.) without doing its own
/// provider-aware formatting.
pub enum RefreshStatus {
    /// Token was valid or successfully refreshed; `profile` is the active
    /// profile name (caller-friendly for printing).
    Refreshed { profile: String },
    /// No auth profile exists for this provider; caller decides whether
    /// to surface a hint to run `auth login`.
    NoProfile,
}

#[async_trait::async_trait]
pub trait AuthProviderFlow: Send + Sync {
    /// Run the OAuth login flow. The default impl bails — only providers
    /// with an OAuth login flow override. `import` is a path to an
    /// existing token-set JSON file for providers that support importing
    /// already-issued credentials (OpenAI Codex `~/.codex/auth.json`).
    async fn login(
        &self,
        _ctx: &AuthFlowContext<'_>,
        _profile: &str,
        _device_code: bool,
        _import: Option<&std::path::Path>,
    ) -> Result<()> {
        anyhow::bail!(
            "`auth login` is not supported for this provider. Use `auth paste-token` or \
             `auth setup-token` for bearer-token providers.",
        )
    }

    /// Resume an OAuth login from a paste-redirect URL/code. The default
    /// impl bails for providers that don't expose a browser flow.
    async fn paste_redirect(
        &self,
        _ctx: &AuthFlowContext<'_>,
        _profile: &str,
        _input: Option<&str>,
    ) -> Result<()> {
        anyhow::bail!(
            "`auth paste-redirect` is not supported for this provider. Only OpenAI Codex and \
             Gemini expose a browser-based OAuth flow.",
        )
    }

    /// Refresh the access token for `profile_override` (or active
    /// profile) and report status. Default impl bails for providers
    /// without a refresh flow.
    async fn refresh_status(
        &self,
        _ctx: &AuthFlowContext<'_>,
        _profile_override: Option<&str>,
    ) -> Result<RefreshStatus> {
        anyhow::bail!(
            "`auth refresh` is not supported for this provider. Only OpenAI Codex and Gemini \
             have an in-process token-refresh flow.",
        )
    }
}

impl AuthProvider {
    /// Resolve the per-variant `AuthProviderFlow` impl for trait dispatch.
    /// The `match self` here is on enum variants — the only place an
    /// auth-flow dispatch exists, every other call site routes through
    /// the returned trait object.
    pub fn flow(&self) -> Box<dyn AuthProviderFlow> {
        match self {
            Self::OpenaiCodex => Box::new(OpenaiCodexFlow),
            Self::Gemini => Box::new(GeminiFlow),
            Self::Anthropic => Box::new(AnthropicFlow),
        }
    }
}

// ── OpenAI Codex impl ──────────────────────────────────────────────────

pub struct OpenaiCodexFlow;

#[async_trait::async_trait]
impl AuthProviderFlow for OpenaiCodexFlow {
    async fn login(
        &self,
        ctx: &AuthFlowContext<'_>,
        profile: &str,
        device_code: bool,
        import: Option<&std::path::Path>,
    ) -> Result<()> {
        if let Some(import_path) = import {
            crate::auth::openai_oauth::import_codex_auth_profile(
                ctx.auth_service,
                profile,
                import_path,
            )
            .await?;
            println!(
                "Imported auth profile from {}",
                import_path.display().to_string()
            );
            println!("Active profile for openai-codex: {profile}");
            return Ok(());
        }

        if device_code {
            match crate::auth::openai_oauth::start_device_code_flow(ctx.client).await {
                Ok(device) => {
                    println!("OpenAI device-code login started.");
                    println!("Visit: {}", device.verification_uri);
                    println!("Code:  {}", device.user_code);
                    if let Some(uri_complete) = &device.verification_uri_complete {
                        println!("Fast link: {uri_complete}");
                    }
                    if let Some(message) = &device.message {
                        println!("{message}");
                    }
                    let token_set =
                        crate::auth::openai_oauth::poll_device_code_tokens(ctx.client, &device)
                            .await?;
                    let account_id = crate::auth::openai_oauth::extract_account_id_from_jwt(
                        &token_set.access_token,
                    );
                    ctx.auth_service
                        .store_openai_tokens(profile, token_set, account_id, true)
                        .await?;
                    println!("Saved profile {profile}");
                    println!("Active profile for openai-codex: {profile}");
                    return Ok(());
                }
                Err(e) => {
                    println!("Device-code flow unavailable: {e}. Falling back to browser flow.");
                }
            }
        }

        let pkce = crate::auth::openai_oauth::generate_pkce_state();
        let authorize_url = crate::auth::openai_oauth::build_authorize_url(&pkce);

        let pending = PendingOAuthLogin {
            model_provider: "openai".into(),
            profile: profile.to_string(),
            code_verifier: pkce.code_verifier.clone(),
            state: pkce.state.clone(),
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        save_pending_oauth_login(ctx.config, &pending)?;

        println!("Open this URL in your browser and authorize access:");
        println!("{authorize_url}");
        println!();

        let code = match crate::auth::openai_oauth::receive_loopback_code(
            &pkce.state,
            std::time::Duration::from_secs(180),
        )
        .await
        {
            Ok(code) => {
                clear_pending_oauth_login(ctx.config, "openai");
                code
            }
            Err(e) => {
                println!("Callback capture failed: {e}");
                println!(
                    "Run `zeroclaw auth paste-redirect --model-provider openai-codex --profile {profile}`"
                );
                return Ok(());
            }
        };

        let token_set =
            crate::auth::openai_oauth::exchange_code_for_tokens(ctx.client, &code, &pkce).await?;
        let account_id =
            crate::auth::openai_oauth::extract_account_id_from_jwt(&token_set.access_token);
        ctx.auth_service
            .store_openai_tokens(profile, token_set, account_id, true)
            .await?;
        println!("Saved profile {profile}");
        println!("Active profile for openai-codex: {profile}");
        Ok(())
    }

    async fn paste_redirect(
        &self,
        ctx: &AuthFlowContext<'_>,
        profile: &str,
        input: Option<&str>,
    ) -> Result<()> {
        let pending = load_pending_oauth_login(ctx.config, "openai")?.ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "oauth_provider": "openai",
                        "profile": profile,
                    })),
                "auth: no pending OpenAI login"
            );
            anyhow::Error::msg(
                "No pending OpenAI login found. Run `zeroclaw auth login --model-provider openai-codex` first.",
            )
        })?;
        if pending.profile != profile {
            anyhow::bail!(
                "Pending login profile mismatch: pending={}, requested={}",
                pending.profile,
                profile,
            );
        }
        let redirect_input = input.ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"oauth_provider": "openai"})),
                "auth: paste-redirect requires URL or code"
            );
            anyhow::Error::msg("paste-redirect requires the redirect URL or OAuth code")
        })?;
        let code = crate::auth::openai_oauth::parse_code_from_redirect(
            redirect_input,
            Some(&pending.state),
        )?;
        let pkce = crate::auth::openai_oauth::PkceState {
            code_verifier: pending.code_verifier.clone(),
            code_challenge: String::new(),
            state: pending.state.clone(),
        };
        let token_set =
            crate::auth::openai_oauth::exchange_code_for_tokens(ctx.client, &code, &pkce).await?;
        let account_id =
            crate::auth::openai_oauth::extract_account_id_from_jwt(&token_set.access_token);
        ctx.auth_service
            .store_openai_tokens(profile, token_set, account_id, true)
            .await?;
        clear_pending_oauth_login(ctx.config, "openai");
        println!("Saved profile {profile}");
        println!("Active profile for openai-codex: {profile}");
        Ok(())
    }

    async fn refresh_status(
        &self,
        ctx: &AuthFlowContext<'_>,
        profile_override: Option<&str>,
    ) -> Result<RefreshStatus> {
        match ctx
            .auth_service
            .get_valid_openai_access_token(profile_override)
            .await?
        {
            Some(_) => Ok(RefreshStatus::Refreshed {
                profile: profile_override.unwrap_or("default").to_string(),
            }),
            None => Ok(RefreshStatus::NoProfile),
        }
    }
}

// ── Gemini impl ────────────────────────────────────────────────────────

pub struct GeminiFlow;

impl GeminiFlow {
    /// Look up the per-alias OAuth client credentials. The auth profile
    /// name doubles as the Gemini family alias key
    /// (`[model_providers.gemini.<profile>]`); the alias config carries
    /// the operator's Google Cloud OAuth app credentials.
    fn alias_creds<'a>(config: &'a Config, profile: &str) -> Result<(&'a str, &'a str)> {
        let alias_cfg = config.providers.models.gemini.get(profile).ok_or_else(|| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "oauth_provider": "gemini",
                        "profile": profile,
                        "missing": "alias_cfg",
                    })),
                "auth: gemini OAuth missing alias config"
            );
            anyhow::Error::msg(format!(
                "Gemini OAuth requires `[model_providers.gemini.{profile}]` to exist with \
                 `oauth_client_id` and `oauth_client_secret` set. Register a Google Cloud \
                 OAuth app and configure the credentials before running this auth flow.",
            ))
        })?;
        let client_id = alias_cfg
            .oauth_client_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "oauth_provider": "gemini",
                            "profile": profile,
                            "missing": "oauth_client_id",
                        })),
                    "auth: gemini OAuth missing oauth_client_id"
                );
                anyhow::Error::msg(format!(
                    "Gemini OAuth requires `oauth_client_id` on `[model_providers.gemini.{profile}]`.",
                ))
            })?;
        let client_secret = alias_cfg
            .oauth_client_secret
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "oauth_provider": "gemini",
                            "profile": profile,
                            "missing": "oauth_client_secret",
                        })),
                    "auth: gemini OAuth missing oauth_client_secret"
                );
                anyhow::Error::msg(format!(
                    "Gemini OAuth requires `oauth_client_secret` on `[model_providers.gemini.{profile}]`.",
                ))
            })?;
        Ok((client_id, client_secret))
    }
}

#[async_trait::async_trait]
impl AuthProviderFlow for GeminiFlow {
    async fn login(
        &self,
        ctx: &AuthFlowContext<'_>,
        profile: &str,
        device_code: bool,
        import: Option<&std::path::Path>,
    ) -> Result<()> {
        if import.is_some() {
            anyhow::bail!(
                "`auth login --import` currently supports only --model-provider openai-codex.",
            );
        }
        let (client_id, client_secret) = Self::alias_creds(ctx.config, profile)?;

        if device_code {
            match crate::auth::gemini_oauth::start_device_code_flow(ctx.client, client_id).await {
                Ok(device) => {
                    println!("Google/Gemini device-code login started.");
                    println!("Visit: {}", device.verification_uri);
                    println!("Code:  {}", device.user_code);
                    if let Some(uri_complete) = &device.verification_uri_complete {
                        println!("Fast link: {uri_complete}");
                    }
                    let token_set = crate::auth::gemini_oauth::poll_device_code_tokens(
                        ctx.client,
                        client_id,
                        client_secret,
                        &device,
                    )
                    .await?;
                    let account_id = token_set
                        .id_token
                        .as_deref()
                        .and_then(crate::auth::gemini_oauth::extract_account_email_from_id_token);
                    ctx.auth_service
                        .store_gemini_tokens(profile, token_set, account_id, true)
                        .await?;
                    println!("Saved profile {profile}");
                    println!("Active profile for gemini: {profile}");
                    return Ok(());
                }
                Err(e) => {
                    println!("Device-code flow unavailable: {e}. Falling back to browser flow.");
                }
            }
        }

        let pkce = crate::auth::gemini_oauth::generate_pkce_state();
        let authorize_url = crate::auth::gemini_oauth::build_authorize_url(client_id, &pkce)?;

        let pending = PendingOAuthLogin {
            model_provider: "gemini".into(),
            profile: profile.to_string(),
            code_verifier: pkce.code_verifier.clone(),
            state: pkce.state.clone(),
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        save_pending_oauth_login(ctx.config, &pending)?;

        println!("Open this URL in your browser and authorize access:");
        println!("{authorize_url}");
        println!();

        let code = match crate::auth::gemini_oauth::receive_loopback_code(
            &pkce.state,
            std::time::Duration::from_secs(180),
        )
        .await
        {
            Ok(code) => {
                clear_pending_oauth_login(ctx.config, "gemini");
                code
            }
            Err(e) => {
                println!("Callback capture failed: {e}");
                println!(
                    "Run `zeroclaw auth paste-redirect --model-provider gemini --profile {profile}`",
                );
                return Ok(());
            }
        };

        let token_set = crate::auth::gemini_oauth::exchange_code_for_tokens(
            ctx.client,
            client_id,
            client_secret,
            &code,
            &pkce,
        )
        .await?;
        let account_id = token_set
            .id_token
            .as_deref()
            .and_then(crate::auth::gemini_oauth::extract_account_email_from_id_token);
        ctx.auth_service
            .store_gemini_tokens(profile, token_set, account_id, true)
            .await?;
        println!("Saved profile {profile}");
        println!("Active profile for gemini: {profile}");
        Ok(())
    }

    async fn paste_redirect(
        &self,
        ctx: &AuthFlowContext<'_>,
        profile: &str,
        input: Option<&str>,
    ) -> Result<()> {
        let (client_id, client_secret) = Self::alias_creds(ctx.config, profile)?;
        let pending = load_pending_oauth_login(ctx.config, "gemini")?.ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "oauth_provider": "gemini",
                        "profile": profile,
                    })),
                "auth: no pending Gemini login"
            );
            anyhow::Error::msg(
                "No pending Gemini login found. Run `zeroclaw auth login --model-provider gemini` first.",
            )
        })?;
        if pending.profile != profile {
            anyhow::bail!(
                "Pending login profile mismatch: pending={}, requested={}",
                pending.profile,
                profile,
            );
        }
        let redirect_input = input.ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"oauth_provider": "gemini"})),
                "auth: paste-redirect requires URL or code"
            );
            anyhow::Error::msg("paste-redirect requires the redirect URL or OAuth code")
        })?;
        let code = crate::auth::gemini_oauth::parse_code_from_redirect(
            redirect_input,
            Some(&pending.state),
        )?;
        let pkce = crate::auth::gemini_oauth::PkceState {
            code_verifier: pending.code_verifier.clone(),
            code_challenge: String::new(),
            state: pending.state.clone(),
        };
        let token_set = crate::auth::gemini_oauth::exchange_code_for_tokens(
            ctx.client,
            client_id,
            client_secret,
            &code,
            &pkce,
        )
        .await?;
        let account_id = token_set
            .id_token
            .as_deref()
            .and_then(crate::auth::gemini_oauth::extract_account_email_from_id_token);
        ctx.auth_service
            .store_gemini_tokens(profile, token_set, account_id, true)
            .await?;
        clear_pending_oauth_login(ctx.config, "gemini");
        println!("Saved profile {profile}");
        println!("Active profile for gemini: {profile}");
        Ok(())
    }

    async fn refresh_status(
        &self,
        ctx: &AuthFlowContext<'_>,
        profile_override: Option<&str>,
    ) -> Result<RefreshStatus> {
        let alias_name = profile_override.unwrap_or("default");
        let alias_cfg = ctx.config.providers.models.gemini.get(alias_name);
        let client_id = alias_cfg
            .and_then(|c| c.oauth_client_id.as_deref())
            .unwrap_or("");
        let client_secret = alias_cfg
            .and_then(|c| c.oauth_client_secret.as_deref())
            .unwrap_or("");
        match ctx
            .auth_service
            .get_valid_gemini_access_token(profile_override, client_id, client_secret)
            .await?
        {
            Some(_) => Ok(RefreshStatus::Refreshed {
                profile: alias_name.to_string(),
            }),
            None => Ok(RefreshStatus::NoProfile),
        }
    }
}

// ── Anthropic impl ─────────────────────────────────────────────────────
//
// Anthropic auth is bearer-token only (long-lived subscription tokens
// from claude.ai). All three OAuth-flow methods rely on the trait's
// default `bail!()` impls — Anthropic operators use `auth paste-token`
// or `auth setup-token` instead.

pub struct AnthropicFlow;

impl AuthProviderFlow for AnthropicFlow {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::profiles::{AuthProfile, AuthProfileKind};

    #[test]
    fn normalize_provider_aliases() {
        assert_eq!(normalize_model_provider("codex").unwrap(), "openai-codex");
        assert_eq!(normalize_model_provider("claude").unwrap(), "anthropic");
        assert_eq!(normalize_model_provider("openai").unwrap(), "openai");
    }

    #[test]
    fn select_profile_prefers_override_then_active_then_default() {
        let mut data = AuthProfilesData::default();
        let id_active = profile_id("openai-codex", "work");
        let id_default = profile_id("openai-codex", "default");

        data.profiles.insert(
            id_default.clone(),
            AuthProfile {
                id: id_default.clone(),
                model_provider: "openai-codex".into(),
                profile_name: "default".into(),
                kind: AuthProfileKind::Token,
                account_id: None,
                workspace_id: None,
                token_set: None,
                token: Some("x".into()),
                metadata: std::collections::BTreeMap::default(),
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            },
        );
        data.profiles.insert(
            id_active.clone(),
            AuthProfile {
                id: id_active.clone(),
                model_provider: "openai-codex".into(),
                profile_name: "work".into(),
                kind: AuthProfileKind::Token,
                account_id: None,
                workspace_id: None,
                token_set: None,
                token: Some("y".into()),
                metadata: std::collections::BTreeMap::default(),
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            },
        );

        data.active_profiles
            .insert("openai-codex".into(), id_active.clone());

        assert_eq!(
            select_profile_id(&data, "openai-codex", Some("default")),
            Some(id_default)
        );
        assert_eq!(
            select_profile_id(&data, "openai-codex", None),
            Some(id_active)
        );
    }
}
