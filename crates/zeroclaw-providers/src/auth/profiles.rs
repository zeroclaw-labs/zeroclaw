use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::fs::{self, OpenOptions};
use tokio::io::AsyncWriteExt;
use tokio::time::sleep;
use zeroclaw_config::secrets::SecretStore;

const CURRENT_SCHEMA_VERSION: u32 = 1;
const PROFILES_FILENAME: &str = "auth-profiles.json";
const LOCK_FILENAME: &str = "auth-profiles.lock";
const LOCK_WAIT_MS: u64 = 50;
const LOCK_TIMEOUT_MS: u64 = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthProfileKind {
    OAuth,
    Token,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenSet {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub id_token: Option<String>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub token_type: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
}

impl TokenSet {
    pub fn is_expiring_within(&self, skew: Duration) -> bool {
        match self.expires_at {
            Some(expires_at) => {
                let now_plus_skew =
                    Utc::now() + chrono::Duration::from_std(skew).unwrap_or_default();
                expires_at <= now_plus_skew
            }
            None => false,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct AuthProfile {
    pub id: String,
    pub model_provider: String,
    pub profile_name: String,
    pub kind: AuthProfileKind,
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub token_set: Option<TokenSet>,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl std::fmt::Debug for AuthProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthProfile")
            .field("id", &self.id)
            .field("model_provider", &self.model_provider)
            .field("profile_name", &self.profile_name)
            .field("kind", &self.kind)
            .field("workspace_id", &self.workspace_id)
            .field("metadata", &self.metadata)
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .finish_non_exhaustive()
    }
}

/// Profile-metadata key marking a credential that must never be rewritten as
/// plaintext, whichever writer saves this store. A writer configured with
/// `secrets.encrypt = false` decrypts every profile it reloads and saves the
/// whole file back verbatim, so a credential whose owner refused plaintext has
/// to carry the refusal in the row itself: without it, an unrelated credential
/// change (pasting a provider token, or logging out a different sign-in)
/// silently drops the protection that was promised at sign-in. MCP OAuth sets
/// it on its grants; [`AuthProfilesStore`] writes a marked row sealed whatever
/// its own setting is.
pub const REQUIRES_ENCRYPTED_STORAGE: &str = "requires_encrypted_storage";

impl AuthProfile {
    pub fn new_oauth(model_provider: &str, profile_name: &str, token_set: TokenSet) -> Self {
        let now = Utc::now();
        let id = profile_id(model_provider, profile_name);
        Self {
            id,
            model_provider: model_provider.to_string(),
            profile_name: profile_name.to_string(),
            kind: AuthProfileKind::OAuth,
            account_id: None,
            workspace_id: None,
            token_set: Some(token_set),
            token: None,
            metadata: BTreeMap::new(),
            created_at: now,
            updated_at: now,
        }
    }

    pub fn new_token(model_provider: &str, profile_name: &str, token: String) -> Self {
        let now = Utc::now();
        let id = profile_id(model_provider, profile_name);
        Self {
            id,
            model_provider: model_provider.to_string(),
            profile_name: profile_name.to_string(),
            kind: AuthProfileKind::Token,
            account_id: None,
            workspace_id: None,
            token_set: None,
            token: Some(token),
            metadata: BTreeMap::new(),
            created_at: now,
            updated_at: now,
        }
    }

    /// Whether this row declared [`REQUIRES_ENCRYPTED_STORAGE`] when it was
    /// created. The declaration travels in `metadata`, which is persisted as
    /// itself, so a writer that never decrypts anything can still honour it.
    fn requires_encrypted_storage(&self) -> bool {
        self.metadata
            .get(REQUIRES_ENCRYPTED_STORAGE)
            .is_some_and(|value| value.as_str() == "true")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthProfilesData {
    pub schema_version: u32,
    pub updated_at: DateTime<Utc>,
    pub active_profiles: BTreeMap<String, String>,
    pub profiles: BTreeMap<String, AuthProfile>,
}

impl Default for AuthProfilesData {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            updated_at: Utc::now(),
            active_profiles: BTreeMap::new(),
            profiles: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AuthProfilesStore {
    path: PathBuf,
    lock_path: PathBuf,
    secret_store: SecretStore,
}

impl AuthProfilesStore {
    pub fn new(state_dir: &Path, encrypt_secrets: bool) -> Self {
        Self {
            path: state_dir.join(PROFILES_FILENAME),
            lock_path: state_dir.join(LOCK_FILENAME),
            secret_store: SecretStore::new(state_dir, encrypt_secrets),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn load(&self) -> Result<AuthProfilesData> {
        let _lock = self.acquire_lock().await?;
        self.load_locked().await
    }

    pub async fn list_profile_ids(&self) -> Result<Vec<String>> {
        let _lock = self.acquire_lock().await?;
        let persisted = self.read_persisted_locked().await?;
        Ok(persisted.profiles.into_keys().collect())
    }

    pub async fn upsert_profile(&self, mut profile: AuthProfile, set_active: bool) -> Result<()> {
        let _lock = self.acquire_lock().await?;
        let mut data = self.load_locked().await?;

        profile.updated_at = Utc::now();
        if let Some(existing) = data.profiles.get(&profile.id) {
            profile.created_at = existing.created_at;
        }

        if set_active {
            data.active_profiles
                .insert(profile.model_provider.clone(), profile.id.clone());
        }

        data.profiles.insert(profile.id.clone(), profile);
        data.updated_at = Utc::now();

        self.save_locked(&data).await
    }

    pub async fn remove_profile(&self, profile_id: &str) -> Result<bool> {
        let _lock = self.acquire_lock().await?;
        let mut data = self.load_locked().await?;

        let removed = data.profiles.remove(profile_id).is_some();
        if !removed {
            return Ok(false);
        }

        data.active_profiles
            .retain(|_, active| active != profile_id);
        data.updated_at = Utc::now();
        self.save_locked(&data).await?;
        Ok(true)
    }

    /// Remove `profile_id` only when `authorize_removal` agrees, evaluated
    /// against the profile read under the same file lock
    /// [`Self::remove_profile`] takes. A caller that proved a credential dead
    /// must not delete a profile another process re-minted in the meantime;
    /// a read followed by an unconditional delete cannot promise that, this
    /// can. Returns whether the profile was removed: a vanished profile and
    /// a profile the predicate refused both report `false` without touching
    /// the file.
    pub async fn remove_profile_if<F>(&self, profile_id: &str, authorize_removal: F) -> Result<bool>
    where
        F: FnOnce(&AuthProfile) -> bool,
    {
        let _lock = self.acquire_lock().await?;
        let mut data = self.load_locked().await?;

        let Some(profile) = data.profiles.get(profile_id) else {
            return Ok(false);
        };
        if !authorize_removal(profile) {
            return Ok(false);
        }

        let removed = data.profiles.remove(profile_id).is_some();
        if removed {
            data.active_profiles
                .retain(|_, active| active != profile_id);
            data.updated_at = Utc::now();
            self.save_locked(&data).await?;
        }
        Ok(removed)
    }

    pub async fn set_active_profile(&self, model_provider: &str, profile_id: &str) -> Result<()> {
        let _lock = self.acquire_lock().await?;
        let mut data = self.load_locked().await?;

        if !data.profiles.contains_key(profile_id) {
            anyhow::bail!("Auth profile not found: {profile_id}");
        }

        data.active_profiles
            .insert(model_provider.to_string(), profile_id.to_string());
        data.updated_at = Utc::now();
        self.save_locked(&data).await
    }

    pub async fn clear_active_profile(&self, model_provider: &str) -> Result<()> {
        let _lock = self.acquire_lock().await?;
        let mut data = self.load_locked().await?;
        data.active_profiles.remove(model_provider);
        data.updated_at = Utc::now();
        self.save_locked(&data).await
    }

    pub async fn update_profile<F>(&self, profile_id: &str, mut updater: F) -> Result<AuthProfile>
    where
        F: FnMut(&mut AuthProfile) -> Result<()>,
    {
        let _lock = self.acquire_lock().await?;
        let mut data = self.load_locked().await?;

        let profile = data.profiles.get_mut(profile_id).ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"profile_id": profile_id})),
                "auth_profiles: profile not found for update"
            );
            anyhow::Error::msg(format!("Auth profile not found: {profile_id}"))
        })?;

        updater(profile)?;
        profile.updated_at = Utc::now();
        let updated_profile = profile.clone();
        data.updated_at = Utc::now();
        self.save_locked(&data).await?;
        Ok(updated_profile)
    }

    async fn load_locked(&self) -> Result<AuthProfilesData> {
        let mut persisted = self.read_persisted_locked().await?;
        let mut migrated = false;

        let mut profiles = BTreeMap::new();
        for (id, p) in &mut persisted.profiles {
            let (access_token, access_migrated) =
                self.decrypt_optional(p.access_token.as_deref())?;
            let (refresh_token, refresh_migrated) =
                self.decrypt_optional(p.refresh_token.as_deref())?;
            let (id_token, id_migrated) = self.decrypt_optional(p.id_token.as_deref())?;
            let (token, token_migrated) = self.decrypt_optional(p.token.as_deref())?;

            if let Some(value) = access_migrated {
                p.access_token = Some(value);
                migrated = true;
            }
            if let Some(value) = refresh_migrated {
                p.refresh_token = Some(value);
                migrated = true;
            }
            if let Some(value) = id_migrated {
                p.id_token = Some(value);
                migrated = true;
            }
            if let Some(value) = token_migrated {
                p.token = Some(value);
                migrated = true;
            }

            let kind = parse_profile_kind(&p.kind)?;
            let token_set = match kind {
                AuthProfileKind::OAuth => {
                    let access = access_token.ok_or_else(|| {
                        ::zeroclaw_log::record!(
                            ERROR,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Reject
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "profile_id": id,
                                "missing": "access_token",
                            })),
                            "auth_profiles: OAuth profile missing access_token"
                        );
                        anyhow::Error::msg(format!("OAuth profile missing access_token: {id}"))
                    })?;
                    Some(TokenSet {
                        access_token: access,
                        refresh_token,
                        id_token,
                        expires_at: parse_optional_datetime(p.expires_at.as_deref())?,
                        token_type: p.token_type.clone(),
                        scope: p.scope.clone(),
                    })
                }
                AuthProfileKind::Token => None,
            };

            profiles.insert(
                id.clone(),
                AuthProfile {
                    id: id.clone(),
                    model_provider: p.model_provider.clone(),
                    profile_name: p.profile_name.clone(),
                    kind,
                    account_id: p.account_id.clone(),
                    workspace_id: p.workspace_id.clone(),
                    token_set,
                    token,
                    metadata: p.metadata.clone(),
                    created_at: parse_datetime_with_fallback(&p.created_at),
                    updated_at: parse_datetime_with_fallback(&p.updated_at),
                },
            );
        }

        // Migration re-encrypts with this store's setting, so a marked row
        // could in principle be carried here as plaintext. It is not: MCP rows
        // are born `enc2:` under an encrypting writer, and this is the only
        // write that does not consult `keep_sealed`. If that ever changes,
        // route migrated marked values through `encrypt_required` here the way
        // `save_locked` does.
        if migrated {
            self.write_persisted_locked(&persisted).await?;
        }

        Ok(AuthProfilesData {
            schema_version: persisted.schema_version,
            updated_at: parse_datetime_with_fallback(&persisted.updated_at),
            active_profiles: persisted.active_profiles,
            profiles,
        })
    }

    async fn save_locked(&self, data: &AuthProfilesData) -> Result<()> {
        let mut persisted = PersistedAuthProfiles {
            schema_version: CURRENT_SCHEMA_VERSION,
            updated_at: data.updated_at.to_rfc3339(),
            active_profiles: data.active_profiles.clone(),
            profiles: BTreeMap::new(),
        };

        for (id, profile) in &data.profiles {
            // A row that declared [`REQUIRES_ENCRYPTED_STORAGE`] keeps that
            // promise whatever this writer's setting is. The operator may have
            // turned encryption off for other credentials, and an unrelated
            // write (pasting a provider token, or logging out one sign-in
            // while another remains) re-saves this file too: it must not
            // rewrite these tokens as plaintext.
            let keep_sealed = profile.requires_encrypted_storage();
            let (access_token, refresh_token, id_token, expires_at, token_type, scope) =
                match (&profile.kind, &profile.token_set) {
                    (AuthProfileKind::OAuth, Some(token_set)) => (
                        self.encrypt_optional(Some(&token_set.access_token), keep_sealed)?,
                        self.encrypt_optional(token_set.refresh_token.as_deref(), keep_sealed)?,
                        self.encrypt_optional(token_set.id_token.as_deref(), keep_sealed)?,
                        token_set.expires_at.as_ref().map(DateTime::to_rfc3339),
                        token_set.token_type.clone(),
                        token_set.scope.clone(),
                    ),
                    _ => (None, None, None, None, None, None),
                };

            let token = self.encrypt_optional(profile.token.as_deref(), keep_sealed)?;

            persisted.profiles.insert(
                id.clone(),
                PersistedAuthProfile {
                    model_provider: profile.model_provider.clone(),
                    profile_name: profile.profile_name.clone(),
                    kind: profile_kind_to_string(profile.kind).to_string(),
                    account_id: profile.account_id.clone(),
                    workspace_id: profile.workspace_id.clone(),
                    access_token,
                    refresh_token,
                    id_token,
                    token,
                    expires_at,
                    token_type,
                    scope,
                    metadata: profile.metadata.clone(),
                    created_at: profile.created_at.to_rfc3339(),
                    updated_at: profile.updated_at.to_rfc3339(),
                },
            );
        }

        self.write_persisted_locked(&persisted).await
    }

    async fn read_persisted_locked(&self) -> Result<PersistedAuthProfiles> {
        if !self.path.exists() {
            return Ok(PersistedAuthProfiles::default());
        }

        let bytes = fs::read(&self.path).await.with_context(|| {
            format!(
                "Failed to read auth profile store at {}",
                self.path.display()
            )
        })?;

        if bytes.is_empty() {
            return Ok(PersistedAuthProfiles::default());
        }

        let mut persisted: PersistedAuthProfiles =
            serde_json::from_slice(&bytes).with_context(|| {
                format!(
                    "Failed to parse auth profile store at {}",
                    self.path.display()
                )
            })?;

        if persisted.schema_version == 0 {
            persisted.schema_version = CURRENT_SCHEMA_VERSION;
        }

        if persisted.schema_version > CURRENT_SCHEMA_VERSION {
            anyhow::bail!(
                "Unsupported auth profile schema version {} (max supported: {})",
                persisted.schema_version,
                CURRENT_SCHEMA_VERSION
            );
        }

        Ok(persisted)
    }

    async fn write_persisted_locked(&self, persisted: &PersistedAuthProfiles) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).await.with_context(|| {
                format!(
                    "Failed to create auth profile directory at {}",
                    parent.display()
                )
            })?;
        }

        let json =
            serde_json::to_vec_pretty(persisted).context("Failed to serialize auth profiles")?;
        let tmp_name = format!(
            "{}.tmp.{}.{}",
            PROFILES_FILENAME,
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        );
        let tmp_path = self.path.with_file_name(tmp_name);

        fs::write(&tmp_path, &json).await.with_context(|| {
            format!(
                "Failed to write temporary auth profile file at {}",
                tmp_path.display()
            )
        })?;

        fs::rename(&tmp_path, &self.path).await.with_context(|| {
            format!(
                "Failed to replace auth profile store at {}",
                self.path.display()
            )
        })?;

        Ok(())
    }

    /// Encrypt one secret field for storage. `keep_sealed` overrides this
    /// store's own setting for a row that promised encryption: the value is
    /// sealed with the key it was sealed with before, or the write fails
    /// rather than putting the credential on disk in plaintext.
    fn encrypt_optional(&self, value: Option<&str>, keep_sealed: bool) -> Result<Option<String>> {
        match value {
            Some(value) if !value.is_empty() => {
                if keep_sealed {
                    self.secret_store.encrypt_required(value).map(Some)
                } else {
                    self.secret_store.encrypt(value).map(Some)
                }
            }
            Some(_) | None => Ok(None),
        }
    }

    fn decrypt_optional(&self, value: Option<&str>) -> Result<(Option<String>, Option<String>)> {
        match value {
            Some(value) if !value.is_empty() => {
                let (plaintext, migrated) = self.secret_store.decrypt_and_migrate(value)?;
                Ok((Some(plaintext), migrated))
            }
            Some(_) | None => Ok((None, None)),
        }
    }

    async fn acquire_lock(&self) -> Result<AuthProfileLockGuard> {
        if let Some(parent) = self.lock_path.parent() {
            fs::create_dir_all(parent).await.with_context(|| {
                format!(
                    "Failed to create lock directory at {}",
                    parent.display().to_string()
                )
            })?;
        }

        let mut waited = 0_u64;
        loop {
            match OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&self.lock_path)
                .await
            {
                Ok(mut file) => {
                    let mut buffer = Vec::new();
                    writeln!(&mut buffer, "pid={}", std::process::id())?;
                    if let Err(e) = file.write_all(&buffer).await {
                        fs::remove_file(&self.lock_path)
                            .await
                            .inspect(|e| {
                                ::zeroclaw_log::record!(
                                    ERROR,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Fail
                                    )
                                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                                    .with_attrs(::serde_json::json!({"e": format!("{:?}", e)})),
                                    "Failed to remove auth profile lock file: "
                                );
                            })
                            .ok();
                        return Err(e).with_context(|| {
                            format!(
                                "Failed to write auth profile lock at {}",
                                self.lock_path.display()
                            )
                        });
                    }
                    return Ok(AuthProfileLockGuard {
                        lock_path: self.lock_path.clone(),
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if waited >= LOCK_TIMEOUT_MS {
                        anyhow::bail!(
                            "Timed out waiting for auth profile lock at {}",
                            self.lock_path.display()
                        );
                    }
                    sleep(Duration::from_millis(LOCK_WAIT_MS)).await;
                    waited = waited.saturating_add(LOCK_WAIT_MS);
                }
                Err(e) => {
                    return Err(e).with_context(|| {
                        format!(
                            "Failed to create auth profile lock at {}",
                            self.lock_path.display()
                        )
                    });
                }
            }
        }
    }
}

struct AuthProfileLockGuard {
    lock_path: PathBuf,
}

impl Drop for AuthProfileLockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.lock_path);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedAuthProfiles {
    #[serde(default = "default_schema_version")]
    schema_version: u32,
    #[serde(default = "default_now_rfc3339")]
    updated_at: String,
    #[serde(default)]
    active_profiles: BTreeMap<String, String>,
    #[serde(default)]
    profiles: BTreeMap<String, PersistedAuthProfile>,
}

impl Default for PersistedAuthProfiles {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            updated_at: default_now_rfc3339(),
            active_profiles: BTreeMap::new(),
            profiles: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct PersistedAuthProfile {
    #[serde(alias = "provider")]
    model_provider: String,
    profile_name: String,
    kind: String,
    #[serde(default)]
    account_id: Option<String>,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    expires_at: Option<String>,
    #[serde(default)]
    token_type: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default = "default_now_rfc3339")]
    created_at: String,
    #[serde(default = "default_now_rfc3339")]
    updated_at: String,
    #[serde(default)]
    metadata: BTreeMap<String, String>,
}

fn default_schema_version() -> u32 {
    CURRENT_SCHEMA_VERSION
}

fn default_now_rfc3339() -> String {
    Utc::now().to_rfc3339()
}

fn parse_profile_kind(value: &str) -> Result<AuthProfileKind> {
    match value {
        "oauth" => Ok(AuthProfileKind::OAuth),
        "token" => Ok(AuthProfileKind::Token),
        other => anyhow::bail!("Unsupported auth profile kind: {other}"),
    }
}

fn profile_kind_to_string(kind: AuthProfileKind) -> &'static str {
    match kind {
        AuthProfileKind::OAuth => "oauth",
        AuthProfileKind::Token => "token",
    }
}

fn parse_optional_datetime(value: Option<&str>) -> Result<Option<DateTime<Utc>>> {
    value.map(parse_datetime).transpose()
}

fn parse_datetime(value: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&Utc))
        .with_context(|| format!("Invalid RFC3339 timestamp: {value}"))
}

fn parse_datetime_with_fallback(value: &str) -> DateTime<Utc> {
    parse_datetime(value).unwrap_or_else(|_| Utc::now())
}

pub fn profile_id(model_provider: &str, profile_name: &str) -> String {
    format!("{}:{}", model_provider.trim(), profile_name.trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn profile_id_format() {
        assert_eq!(
            profile_id("openai-codex", "default"),
            "openai-codex:default"
        );
    }

    #[tokio::test]
    async fn remove_profile_if_refuses_a_profile_the_caller_does_not_own() {
        // A caller that proved one credential dead must not delete a profile
        // another process re-minted in the meantime: the predicate and the
        // removal share one lock acquisition, so what it checked cannot go
        // stale before the write lands.
        let tmp = TempDir::new().expect("tmp");
        let store = AuthProfilesStore::new(tmp.path(), false);
        let id = profile_id("mcp", "oauth-bound");
        let tokens = TokenSet {
            access_token: "at-old".to_string(),
            refresh_token: Some("rt-old".to_string()),
            id_token: None,
            expires_at: None,
            token_type: None,
            scope: None,
        };
        store
            .upsert_profile(AuthProfile::new_oauth("mcp", "oauth-bound", tokens), true)
            .await
            .expect("seed");

        assert!(
            !store
                .remove_profile_if(&id, |profile| {
                    profile
                        .token_set
                        .as_ref()
                        .is_some_and(|set| set.access_token == "at-other")
                })
                .await
                .expect("declined removal"),
            "a profile the predicate refuses stays"
        );
        assert!(
            store.list_profile_ids().await.expect("ids").contains(&id),
            "the row must survive the declined removal"
        );

        assert!(
            store
                .remove_profile_if(&id, |profile| {
                    profile
                        .token_set
                        .as_ref()
                        .is_some_and(|set| set.access_token == "at-old")
                })
                .await
                .expect("owned removal"),
            "the profile the predicate names is removed"
        );
        assert!(
            !store.list_profile_ids().await.expect("ids").contains(&id),
            "the removal must delete the row"
        );

        // A vanished profile is not an error, and nothing is written.
        assert!(
            !store
                .remove_profile_if(&id, |_| true)
                .await
                .expect("missing removal"),
            "a vanished profile reports false"
        );
    }

    #[test]
    fn persisted_profile_accepts_legacy_provider_key() {
        let raw = r#"{
            "schema_version": 2,
            "updated_at": "2026-07-11T00:00:00Z",
            "active_profiles": {
                "openai-codex": "openai-codex:default"
            },
            "profiles": {
                "openai-codex:default": {
                    "provider": "openai-codex",
                    "profile_name": "default",
                    "kind": "oauth",
                    "access_token": "access-token"
                }
            }
        }"#;

        let parsed: PersistedAuthProfiles = serde_json::from_str(raw).unwrap();
        let profile = parsed.profiles.get("openai-codex:default").unwrap();

        assert_eq!(profile.model_provider, "openai-codex");
        assert_eq!(profile.profile_name, "default");
    }

    #[test]
    fn token_expiry_math() {
        let token_set = TokenSet {
            access_token: "token".into(),
            refresh_token: Some("refresh".into()),
            id_token: None,
            expires_at: Some(Utc::now() + chrono::Duration::seconds(10)),
            token_type: Some("Bearer".into()),
            scope: None,
        };

        assert!(token_set.is_expiring_within(Duration::from_secs(15)));
        assert!(!token_set.is_expiring_within(Duration::from_secs(1)));
    }

    #[tokio::test]
    async fn an_unencrypted_writer_keeps_a_marked_profile_sealed() {
        // MCP OAuth keeps its grants in this file and refuses to run with
        // encryption off; the writer that pastes a provider token must not be
        // free to rewrite those rows as plaintext while saving the whole file.
        let tmp = TempDir::new().unwrap();
        let encrypted = AuthProfilesStore::new(tmp.path(), true);
        let mut mcp = AuthProfile::new_oauth(
            "mcp",
            "resource",
            TokenSet {
                access_token: "mcp-access".into(),
                refresh_token: Some("mcp-refresh".into()),
                id_token: None,
                expires_at: Some(Utc::now() + chrono::Duration::hours(1)),
                token_type: Some("Bearer".into()),
                scope: None,
            },
        );
        mcp.metadata
            .insert(REQUIRES_ENCRYPTED_STORAGE.to_string(), "true".into());
        encrypted.upsert_profile(mcp.clone(), false).await.unwrap();

        let plain = AuthProfilesStore::new(tmp.path(), false);
        plain
            .upsert_profile(
                AuthProfile::new_token("openai", "default", "sk-new".into()),
                false,
            )
            .await
            .expect("an unrelated credential can still be saved");

        // The granted tokens are still ciphertext on disk and still decrypt,
        // while the credential this writer owns follows the writer's setting.
        let raw = tokio::fs::read_to_string(plain.path()).await.unwrap();
        assert!(!raw.contains("mcp-access"), "the grant must not leak");
        assert!(
            !raw.contains("mcp-refresh"),
            "neither may its refresh token"
        );
        assert!(
            raw.contains("sk-new"),
            "this writer's own credential follows its setting"
        );
        let data = encrypted.load().await.unwrap();
        let tokens = data
            .profiles
            .get(&mcp.id)
            .and_then(|profile| profile.token_set.as_ref())
            .expect("the grant survives the unrelated write");
        assert_eq!(tokens.access_token, "mcp-access");
        assert_eq!(tokens.refresh_token.as_deref(), Some("mcp-refresh"));
    }

    #[tokio::test]
    async fn logging_out_one_marked_sign_in_leaves_the_other_sealed() {
        // Two MCP resources signed in, then the operator turns encryption off
        // for other credentials. Logging one out rewrites this file too: it
        // must neither be blocked by the other marked profile nor expose it.
        let tmp = TempDir::new().unwrap();
        let encrypted = AuthProfilesStore::new(tmp.path(), true);
        let first = {
            let mut profile = AuthProfile::new_oauth(
                "mcp",
                "resource-a",
                TokenSet {
                    access_token: "at-a".into(),
                    refresh_token: None,
                    id_token: None,
                    expires_at: Some(Utc::now() + chrono::Duration::hours(1)),
                    token_type: Some("Bearer".into()),
                    scope: None,
                },
            );
            profile
                .metadata
                .insert(REQUIRES_ENCRYPTED_STORAGE.to_string(), "true".into());
            profile
        };
        let second = {
            let mut profile = AuthProfile::new_oauth(
                "mcp",
                "resource-b",
                TokenSet {
                    access_token: "at-b".into(),
                    refresh_token: None,
                    id_token: None,
                    expires_at: Some(Utc::now() + chrono::Duration::hours(1)),
                    token_type: Some("Bearer".into()),
                    scope: None,
                },
            );
            profile
                .metadata
                .insert(REQUIRES_ENCRYPTED_STORAGE.to_string(), "true".into());
            profile
        };
        encrypted
            .upsert_profile(first.clone(), false)
            .await
            .unwrap();
        encrypted
            .upsert_profile(second.clone(), false)
            .await
            .unwrap();

        let plain = AuthProfilesStore::new(tmp.path(), false);
        assert!(
            plain.remove_profile(&first.id).await.unwrap(),
            "logging out one sign-in cannot be blocked by another marked profile"
        );

        let raw = tokio::fs::read_to_string(plain.path()).await.unwrap();
        assert!(!raw.contains("at-a"), "the signed-out grant is gone");
        assert!(!raw.contains("at-b"), "the surviving grant must not leak");
        let data = encrypted.load().await.unwrap();
        assert_eq!(data.profiles.len(), 1);
        assert_eq!(
            data.profiles
                .get(&second.id)
                .and_then(|profile| profile.token_set.as_ref())
                .map(|tokens| tokens.access_token.as_str()),
            Some("at-b"),
            "the remaining sign-in still decrypts for the next connect"
        );
    }

    #[tokio::test]
    async fn store_roundtrip_with_encryption() {
        let tmp = TempDir::new().unwrap();
        let store = AuthProfilesStore::new(tmp.path(), true);

        let mut profile = AuthProfile::new_oauth(
            "openai-codex",
            "default",
            TokenSet {
                access_token: "access-123".into(),
                refresh_token: Some("refresh-123".into()),
                id_token: None,
                expires_at: Some(Utc::now() + chrono::Duration::hours(1)),
                token_type: Some("Bearer".into()),
                scope: Some("openid offline_access".into()),
            },
        );
        profile.account_id = Some("acct_123".into());

        store.upsert_profile(profile.clone(), true).await.unwrap();

        let data = store.load().await.unwrap();
        let loaded = data.profiles.get(&profile.id).unwrap();

        assert_eq!(loaded.model_provider, "openai-codex");
        assert_eq!(loaded.profile_name, "default");
        assert_eq!(loaded.account_id.as_deref(), Some("acct_123"));
        assert_eq!(
            loaded
                .token_set
                .as_ref()
                .and_then(|t| t.refresh_token.as_deref()),
            Some("refresh-123")
        );

        let raw = tokio::fs::read_to_string(store.path()).await.unwrap();
        assert!(raw.contains("enc2:"));
        assert!(!raw.contains("refresh-123"));
        assert!(!raw.contains("access-123"));
    }

    #[tokio::test]
    async fn atomic_write_replaces_file() {
        let tmp = TempDir::new().unwrap();
        let store = AuthProfilesStore::new(tmp.path(), false);

        let profile = AuthProfile::new_token("anthropic", "default", "token-abc".into());
        store.upsert_profile(profile, true).await.unwrap();

        let path = store.path().to_path_buf();
        assert!(path.exists());

        let contents = tokio::fs::read_to_string(path).await.unwrap();
        assert!(contents.contains("\"schema_version\": 1"));
    }

    #[tokio::test]
    async fn list_profile_ids_lists_without_decrypting_or_rewriting() {
        let tmp = TempDir::new().unwrap();
        let store = AuthProfilesStore::new(tmp.path(), true);

        let codex = AuthProfile::new_oauth(
            "openai-codex",
            "default",
            TokenSet {
                access_token: "access-xyz".into(),
                refresh_token: Some("refresh-xyz".into()),
                id_token: None,
                expires_at: Some(Utc::now() + chrono::Duration::hours(1)),
                token_type: Some("Bearer".into()),
                scope: None,
            },
        );
        let anthropic = AuthProfile::new_token("anthropic", "default", "token-abc".into());
        store.upsert_profile(codex.clone(), true).await.unwrap();
        store.upsert_profile(anthropic, false).await.unwrap();

        let before = tokio::fs::read(store.path()).await.unwrap();

        let ids = store.list_profile_ids().await.unwrap();
        assert!(ids.iter().any(|id| id == "openai-codex:default"));
        assert!(ids.iter().any(|id| id == "anthropic:default"));

        // No decrypt-and-migrate side effect: the store bytes are untouched.
        let after = tokio::fs::read(store.path()).await.unwrap();
        assert_eq!(before, after, "list_profile_ids must not rewrite the store");
    }

    #[tokio::test]
    async fn legacy_oauth_store_loads_with_flat_tokens_reconstructed() {
        // A store written by a release before the `provider` -> `model_provider`
        // rename: it carries the legacy `provider` key and flat OAuth token fields
        // rather than a nested `token_set`. Loading through the real store path must
        // map the alias and rebuild the token set, not silently yield `token_set:
        // None` (an authenticated profile that holds no credentials).
        let tmp = TempDir::new().unwrap();
        let store = AuthProfilesStore::new(tmp.path(), false);

        let legacy = r#"{
            "schema_version": 1,
            "updated_at": "2026-01-01T00:00:00Z",
            "active_profiles": {
                "openai-codex": "openai-codex:default"
            },
            "profiles": {
                "openai-codex:default": {
                    "provider": "openai-codex",
                    "profile_name": "default",
                    "kind": "oauth",
                    "account_id": "acct_legacy",
                    "workspace_id": "ws_legacy",
                    "access_token": "legacy-access",
                    "refresh_token": "legacy-refresh",
                    "id_token": "legacy-id",
                    "expires_at": "2030-01-01T00:00:00Z",
                    "token_type": "Bearer",
                    "scope": "openid offline_access"
                }
            }
        }"#;
        tokio::fs::write(store.path(), legacy).await.unwrap();

        let data = store.load().await.unwrap();
        let profile = data
            .profiles
            .get("openai-codex:default")
            .expect("legacy profile loads");

        // Legacy `provider` key resolves to the canonical field.
        assert_eq!(profile.model_provider, "openai-codex");
        assert_eq!(profile.kind, AuthProfileKind::OAuth);
        assert_eq!(profile.account_id.as_deref(), Some("acct_legacy"));
        assert_eq!(profile.workspace_id.as_deref(), Some("ws_legacy"));

        // Flat token fields are reassembled into the token set.
        let token_set = profile.token_set.as_ref().expect("flat tokens rebuilt");
        assert_eq!(token_set.access_token, "legacy-access");
        assert_eq!(token_set.refresh_token.as_deref(), Some("legacy-refresh"));
        assert_eq!(token_set.id_token.as_deref(), Some("legacy-id"));
        assert_eq!(token_set.token_type.as_deref(), Some("Bearer"));
        assert_eq!(token_set.scope.as_deref(), Some("openid offline_access"));
        assert_eq!(
            token_set.expires_at,
            Some(
                DateTime::parse_from_rfc3339("2030-01-01T00:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc)
            )
        );

        // The active-profile pointer survives the load unchanged.
        assert_eq!(
            data.active_profiles.get("openai-codex").map(String::as_str),
            Some("openai-codex:default")
        );
    }

    #[tokio::test]
    async fn legacy_token_store_loads_flat_token_field() {
        // Token-kind sibling of the OAuth case: a legacy `provider` key with a flat
        // `token` field (no OAuth token set) must load with the token preserved.
        let tmp = TempDir::new().unwrap();
        let store = AuthProfilesStore::new(tmp.path(), false);

        let legacy = r#"{
            "schema_version": 1,
            "updated_at": "2026-01-01T00:00:00Z",
            "active_profiles": {
                "anthropic": "anthropic:default"
            },
            "profiles": {
                "anthropic:default": {
                    "provider": "anthropic",
                    "profile_name": "default",
                    "kind": "token",
                    "token": "legacy-api-key"
                }
            }
        }"#;
        tokio::fs::write(store.path(), legacy).await.unwrap();

        let data = store.load().await.unwrap();
        let profile = data
            .profiles
            .get("anthropic:default")
            .expect("legacy token profile loads");

        assert_eq!(profile.model_provider, "anthropic");
        assert_eq!(profile.kind, AuthProfileKind::Token);
        assert!(profile.token_set.is_none());
        assert_eq!(profile.token.as_deref(), Some("legacy-api-key"));
    }
}
