use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[cfg(any(test, not(unix)))]
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::fs;
#[cfg(not(unix))]
use tokio::fs::OpenOptions;
#[cfg(not(unix))]
use tokio::io::AsyncWriteExt;
use tokio::time::sleep;
use zeroclaw_config::secrets::SecretStore;

const CURRENT_SCHEMA_VERSION: u32 = 1;
const PROFILES_FILENAME: &str = "auth-profiles.json";
const LOCK_FILENAME: &str = "auth-profiles.lock";
#[cfg(unix)]
const LOCK_GATE_FILENAME: &str = "auth-profiles.guard";
const LOCK_WAIT_MS: u64 = 50;
const LOCK_TIMEOUT_MS: u64 = 10_000;

#[cfg(all(test, unix))]
tokio::task_local! { static REPLACE_GATE_BEFORE_PUBLICATION: std::cell::Cell<bool>; }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthProfileKind {
    OAuth,
    Token,
}

#[derive(Clone, Serialize, Deserialize)]
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

impl std::fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenSet")
            .field("expires_at", &self.expires_at)
            .field("token_type", &self.token_type)
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

/// Validated registration identity, distinct from any email/workspace label.
/// Credentials remain solely in TokenSet's encrypted persistence fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatGptPlanRegistration {
    pub client_id: String,
    pub subject: String,
    #[serde(default)]
    pub earliest_refresh_at: Option<DateTime<Utc>>,
    /// Durable refresh egress marker; unresolved outcomes require re-login.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_started_at: Option<DateTime<Utc>>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_registration: Option<ChatGptPlanRegistration>,
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
            plan_registration: None,
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
            plan_registration: None,
            token_set: None,
            token: Some(token),
            metadata: BTreeMap::new(),
            created_at: now,
            updated_at: now,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthProfilesData {
    pub schema_version: u32,
    pub updated_at: DateTime<Utc>,
    pub active_profiles: BTreeMap<String, String>,
    pub profiles: BTreeMap<String, AuthProfile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_host_id: Option<String>,
}

impl Default for AuthProfilesData {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            updated_at: Utc::now(),
            active_profiles: BTreeMap::new(),
            profiles: BTreeMap::new(),
            plan_host_id: None,
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
            if let Some(registration) = &existing.plan_registration {
                anyhow::ensure!(
                    profile
                        .plan_registration
                        .as_ref()
                        .is_some_and(|new| new.client_id == registration.client_id
                            && new.subject == registration.subject),
                    "ChatGPT plan registration identity changed; use a separate profile"
                );
            }
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

    /// Persist once per instance before starting authorization. UUIDs contain
    /// no account identity; legacy stores migrate lazily when opting in.
    pub async fn ensure_plan_host_id(&self) -> Result<String> {
        let _lock = self.acquire_lock().await?;
        let mut data = self.load_locked().await?;
        if let Some(id) = data.plan_host_id {
            return Ok(id);
        }
        let id = format!("urn:uuid:{}", uuid::Uuid::new_v4());
        data.plan_host_id = Some(id.clone());
        self.save_locked(&data).await?;
        Ok(id)
    }

    /// Kernel-held lock, released on process exit. The file lives in the
    /// canonical instance directory, so root aliases coordinate naturally.
    pub(super) async fn acquire_plan_refresh_lock(
        &self,
        registration: &str,
    ) -> Result<std::fs::File> {
        use sha2::{Digest, Sha256};
        let parent = self.path.parent().context("Auth store has no directory")?;
        fs::create_dir_all(parent).await?;
        let root = std::fs::canonicalize(parent)?;
        let path = root.join(format!(
            "auth-plan-refresh-{}.lock",
            hex::encode(Sha256::digest(registration.as_bytes()))
        ));
        let file = super::protected_file::lock_file(&path)?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(file),
                Err(std::fs::TryLockError::WouldBlock)
                    if tokio::time::Instant::now() < deadline =>
                {
                    sleep(Duration::from_millis(LOCK_WAIT_MS)).await
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    anyhow::bail!("Timed out waiting for ChatGPT plan refresh lock")
                }
                Err(error) => {
                    return Err(error).context("Unable to lock ChatGPT plan registration");
                }
            }
        }
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
                    plan_registration: p.plan_registration.clone(),
                    token_set,
                    token,
                    metadata: p.metadata.clone(),
                    created_at: parse_datetime_with_fallback(&p.created_at),
                    updated_at: parse_datetime_with_fallback(&p.updated_at),
                },
            );
        }

        if migrated {
            self.write_persisted_locked(&persisted).await?;
        }

        Ok(AuthProfilesData {
            schema_version: persisted.schema_version,
            updated_at: parse_datetime_with_fallback(&persisted.updated_at),
            active_profiles: persisted.active_profiles,
            profiles,
            plan_host_id: persisted.plan_host_id,
        })
    }

    async fn save_locked(&self, data: &AuthProfilesData) -> Result<()> {
        let mut persisted = PersistedAuthProfiles {
            schema_version: CURRENT_SCHEMA_VERSION,
            updated_at: data.updated_at.to_rfc3339(),
            active_profiles: data.active_profiles.clone(),
            profiles: BTreeMap::new(),
            plan_host_id: data.plan_host_id.clone(),
        };

        for (id, profile) in &data.profiles {
            // Plan credentials always use encryption, even when the legacy
            // operator preference permits plaintext API/Codex credentials.
            let plan_secrets = SecretStore::new(
                self.path.parent().context("Auth store has no directory")?,
                true,
            );
            let encrypt = |value: Option<&str>| -> Result<Option<String>> {
                if profile.plan_registration.is_some() {
                    value
                        .filter(|s| !s.is_empty())
                        .map(|s| plan_secrets.encrypt(s))
                        .transpose()
                } else {
                    self.encrypt_optional(value)
                }
            };
            let (access_token, refresh_token, id_token, expires_at, token_type, scope) =
                match (&profile.kind, &profile.token_set) {
                    (AuthProfileKind::OAuth, Some(token_set)) => (
                        encrypt(Some(&token_set.access_token))?,
                        encrypt(token_set.refresh_token.as_deref())?,
                        encrypt(token_set.id_token.as_deref())?,
                        token_set.expires_at.as_ref().map(DateTime::to_rfc3339),
                        token_set.token_type.clone(),
                        token_set.scope.clone(),
                    ),
                    _ => (None, None, None, None, None, None),
                };

            let token = self.encrypt_optional(profile.token.as_deref())?;

            persisted.profiles.insert(
                id.clone(),
                PersistedAuthProfile {
                    model_provider: profile.model_provider.clone(),
                    profile_name: profile.profile_name.clone(),
                    kind: profile_kind_to_string(profile.kind).to_string(),
                    account_id: profile.account_id.clone(),
                    workspace_id: profile.workspace_id.clone(),
                    plan_registration: profile.plan_registration.clone(),
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
        let Some(bytes) = super::protected_file::read(&self.path)? else {
            return Ok(PersistedAuthProfiles::default());
        };

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
        super::protected_file::atomic_write(&self.path, &json)
    }

    fn encrypt_optional(&self, value: Option<&str>) -> Result<Option<String>> {
        match value {
            Some(value) if !value.is_empty() => self.secret_store.encrypt(value).map(Some),
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

    #[cfg(unix)]
    async fn acquire_lock(&self) -> Result<AuthProfileLockGuard> {
        let parent = self
            .lock_path
            .parent()
            .context("Auth store has no directory")?;
        fs::create_dir_all(parent).await?;
        let root = std::fs::canonicalize(parent)?;
        let gate_path = root.join(LOCK_GATE_FILENAME);
        let lock_path = root.join(LOCK_FILENAME);
        let lease = super::protected_file::lock_file(&gate_path)?;
        let deadline = tokio::time::Instant::now() + Duration::from_millis(LOCK_TIMEOUT_MS);
        loop {
            match lease.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock)
                    if tokio::time::Instant::now() < deadline =>
                {
                    sleep(Duration::from_millis(LOCK_WAIT_MS)).await;
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    anyhow::bail!("Timed out waiting for auth profile kernel lock")
                }
                Err(error) => return Err(error).context("Unable to lock auth profile store"),
            }
        }

        // Atomic link publication makes recovery ownership knowable even if
        // killed immediately: there is no create-then-write-marker window.
        // Never delete the gate: every current peer must lock the same inode.
        loop {
            #[cfg(test)]
            if REPLACE_GATE_BEFORE_PUBLICATION
                .try_with(|replace| replace.replace(false))
                .unwrap_or(false)
            {
                std::fs::remove_file(&gate_path)?;
                std::fs::File::create(&gate_path)?;
            }
            super::protected_file::reject_symlink(&lock_path)?;
            match std::fs::hard_link(&gate_path, &lock_path) {
                Ok(()) => {
                    anyhow::ensure!(
                        super::protected_file::same_inode(&lease, &lock_path)?,
                        "Auth profile kernel lock identity changed"
                    );
                    return Ok(AuthProfileLockGuard { lock_path, lease });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if super::protected_file::same_inode(&lease, &lock_path)? {
                        // Current peers cannot replace this link while we hold
                        // the gate; legacy create_new peers cannot replace it
                        // while it exists. A matching link is a crashed owner.
                        std::fs::remove_file(&lock_path)
                            .context("Unable to recover auth profile sentinel")?;
                        continue;
                    }
                    // A foreign inode may belong to an older live writer that
                    // ignores the gate. Even a dead PID is not safe evidence:
                    // checking it then unlinking races legacy replacement.
                    if tokio::time::Instant::now() >= deadline {
                        anyhow::bail!("Timed out waiting for legacy auth profile lock");
                    }
                    sleep(Duration::from_millis(LOCK_WAIT_MS)).await;
                }
                Err(error) => return Err(error).context("Unable to publish auth profile sentinel"),
            }
        }
    }

    #[cfg(not(unix))]
    async fn acquire_lock(&self) -> Result<AuthProfileLockGuard> {
        if let Some(parent) = self.lock_path.parent() {
            fs::create_dir_all(parent).await.with_context(|| {
                format!(
                    "Failed to create lock directory at {}",
                    parent.display().to_string()
                )
            })?;
        }

        super::protected_file::reject_symlink(&self.lock_path)?;
        let mut waited = 0_u64;
        loop {
            let mut options = OpenOptions::new();
            options.create_new(true).write(true);
            #[cfg(unix)]
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
            match options.open(&self.lock_path).await {
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
    #[cfg(unix)]
    lease: std::fs::File,
}

impl Drop for AuthProfileLockGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        if !super::protected_file::same_inode(&self.lease, &self.lock_path).unwrap_or(false) {
            return;
        }
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    plan_host_id: Option<String>,
}

impl Default for PersistedAuthProfiles {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            updated_at: default_now_rfc3339(),
            active_profiles: BTreeMap::new(),
            profiles: BTreeMap::new(),
            plan_host_id: None,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    plan_registration: Option<ChatGptPlanRegistration>,
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

    #[cfg(unix)]
    #[tokio::test]
    async fn canonical_store_process_child() {
        let Ok(root) = std::env::var("ZEROCLAW_STORE_KILL_ROOT") else {
            return;
        };
        let store = AuthProfilesStore::new(Path::new(&root), false);
        match std::env::var("ZEROCLAW_STORE_KILL_ACTION")
            .unwrap()
            .as_str()
        {
            "hold" => {
                let _guard = store.acquire_lock().await.unwrap();
                std::fs::write(std::env::var("ZEROCLAW_STORE_KILL_READY").unwrap(), b"held")
                    .unwrap();
                std::future::pending::<()>().await;
            }
            "legacy-hold" => {
                let mut file = std::fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&store.lock_path)
                    .unwrap();
                writeln!(file, "pid={}", std::process::id()).unwrap();
                std::fs::write(std::env::var("ZEROCLAW_STORE_KILL_READY").unwrap(), b"held")
                    .unwrap();
                if let Ok(release) = std::env::var("ZEROCLAW_STORE_KILL_RELEASE") {
                    while !Path::new(&release).exists() {
                        sleep(Duration::from_millis(10)).await;
                    }
                    std::fs::remove_file(&store.lock_path).unwrap();
                    return;
                }
                std::future::pending::<()>().await;
            }
            "recover" => {
                let data = tokio::time::timeout(Duration::from_secs(1), store.load())
                    .await
                    .expect("fresh process must regain canonical store access")
                    .unwrap();
                assert_eq!(
                    data.profiles["anthropic:original"].token.as_deref(),
                    Some("synthetic")
                );
                store
                    .upsert_profile(
                        AuthProfile::new_token("anthropic", "recovered", "synthetic-new".into()),
                        false,
                    )
                    .await
                    .unwrap();
            }
            action => panic!("unexpected synthetic child action: {action}"),
        }
    }

    #[cfg(unix)]
    fn store_child(root: &Path, action: &str, ready: &Path) -> tokio::process::Child {
        tokio::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "auth::profiles::tests::canonical_store_process_child",
                "--exact",
                "--nocapture",
            ])
            .env("ZEROCLAW_STORE_KILL_ROOT", root)
            .env("ZEROCLAW_STORE_KILL_ACTION", action)
            .env("ZEROCLAW_STORE_KILL_READY", ready)
            .kill_on_drop(true)
            .spawn()
            .unwrap()
    }

    #[cfg(unix)]
    async fn wait_store_child_ready(ready: &Path) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !ready.exists() {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn canonical_store_owner_kill_recovers_and_alias_excludes_live_writers() {
        let tmp = TempDir::new().unwrap();
        let aliases = TempDir::new().unwrap();
        let alias = aliases.path().join("instance");
        std::os::unix::fs::symlink(tmp.path(), &alias).unwrap();
        let store = AuthProfilesStore::new(tmp.path(), false);
        store
            .upsert_profile(
                AuthProfile::new_token("anthropic", "original", "synthetic".into()),
                false,
            )
            .await
            .unwrap();
        let ready = tmp.path().join("ready");
        let mut child = store_child(&alias, "hold", &ready);
        wait_store_child_ready(&ready).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(200), store.load())
                .await
                .is_err(),
            "live alias writer must exclude this process"
        );
        assert_eq!(
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&store.lock_path)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::AlreadyExists,
            "legacy writer must also remain excluded"
        );
        child.kill().await.unwrap();
        child.wait().await.unwrap();
        let status = tokio::time::timeout(
            Duration::from_secs(5),
            store_child(tmp.path(), "recover", &ready).wait(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            status.success(),
            "fresh process failed to recover the canonical store"
        );
        assert!(
            store
                .load()
                .await
                .unwrap()
                .profiles
                .contains_key("anthropic:recovered")
        );
        // A clean downgrade can still acquire the old create_new sentinel.
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&store.lock_path)
            .unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn canonical_store_never_reclaims_legacy_live_or_dead_sentinel() {
        let tmp = TempDir::new().unwrap();
        let ready = tmp.path().join("ready");
        let store = AuthProfilesStore::new(tmp.path(), false);
        let mut child = store_child(tmp.path(), "legacy-hold", &ready);
        wait_store_child_ready(&ready).await;
        let original = std::fs::read(&store.lock_path).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(200), store.load())
                .await
                .is_err()
        );
        assert_eq!(std::fs::read(&store.lock_path).unwrap(), original);
        child.kill().await.unwrap();
        child.wait().await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(200), store.load())
                .await
                .is_err()
        );
        assert_eq!(
            std::fs::read(&store.lock_path).unwrap(),
            original,
            "a PID-only lock is not proof of safe ownership"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn canonical_store_legacy_writer_clean_release_allows_current_writer() {
        let tmp = TempDir::new().unwrap();
        let store = AuthProfilesStore::new(tmp.path(), false);
        store
            .upsert_profile(
                AuthProfile::new_token("anthropic", "original", "synthetic".into()),
                false,
            )
            .await
            .unwrap();
        let ready = tmp.path().join("ready");
        let release = tmp.path().join("release");
        let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "auth::profiles::tests::canonical_store_process_child",
                "--exact",
                "--nocapture",
            ])
            .env("ZEROCLAW_STORE_KILL_ROOT", tmp.path())
            .env("ZEROCLAW_STORE_KILL_ACTION", "legacy-hold")
            .env("ZEROCLAW_STORE_KILL_READY", &ready)
            .env("ZEROCLAW_STORE_KILL_RELEASE", &release)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        wait_store_child_ready(&ready).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(200), store.load())
                .await
                .is_err()
        );
        std::fs::write(release, b"release").unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(5), child.wait())
                .await
                .unwrap()
                .unwrap()
                .success()
        );
        assert!(
            tokio::time::timeout(
                Duration::from_secs(5),
                store_child(tmp.path(), "recover", &ready).wait()
            )
            .await
            .unwrap()
            .unwrap()
            .success()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn canonical_store_recovery_and_cleanup_require_gate_inode() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let tmp = TempDir::new().unwrap();
        let store = AuthProfilesStore::new(tmp.path(), false);
        let guard = store.acquire_lock().await.unwrap();
        let gate_path = tmp.path().join(LOCK_GATE_FILENAME);
        let gate = std::fs::metadata(&gate_path).unwrap();
        let sentinel = std::fs::metadata(&store.lock_path).unwrap();
        assert_eq!((gate.dev(), gate.ino()), (sentinel.dev(), sentinel.ino()));
        assert_eq!(gate.permissions().mode() & 0o777, 0o600);
        drop(guard);
        assert!(gate_path.exists());
        assert!(!store.lock_path.exists());

        // Matching contents/PID do not prove ownership: only the inode does.
        let contents = std::fs::read(&gate_path).unwrap();
        std::fs::write(&store.lock_path, &contents).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(200), store.load())
                .await
                .is_err()
        );
        assert_eq!(std::fs::read(&store.lock_path).unwrap(), contents);
        std::fs::remove_file(&store.lock_path).unwrap();

        // Simulate an operator replacing the sentinel while our guard lives.
        // Cleanup must leave that foreign inode intact.
        let guard = store.acquire_lock().await.unwrap();
        std::fs::remove_file(&store.lock_path).unwrap();
        std::fs::write(&store.lock_path, b"pid=foreign\n").unwrap();
        drop(guard);
        assert_eq!(std::fs::read(&store.lock_path).unwrap(), b"pid=foreign\n");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn canonical_store_rejects_gate_and_sentinel_leaf_symlinks() {
        let outside = TempDir::new().unwrap();
        let target = outside.path().join("outside");
        std::fs::write(&target, b"unchanged").unwrap();
        for filename in [LOCK_GATE_FILENAME, LOCK_FILENAME] {
            let tmp = TempDir::new().unwrap();
            let link = tmp.path().join(filename);
            std::os::unix::fs::symlink(&target, &link).unwrap();
            let store = AuthProfilesStore::new(tmp.path(), false);
            assert!(
                store.load().await.is_err(),
                "must reject {filename} symlink"
            );
            assert!(
                std::fs::symlink_metadata(link)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            assert_eq!(std::fs::read(&target).unwrap(), b"unchanged");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn canonical_store_identity_rejects_nonregular_and_unlinked_paths() {
        use std::os::unix::ffi::OsStrExt;
        let tmp = TempDir::new().unwrap();
        let gate =
            super::super::protected_file::lock_file(&tmp.path().join(LOCK_GATE_FILENAME)).unwrap();
        let sentinel = tmp.path().join(LOCK_FILENAME);
        assert!(!super::super::protected_file::same_inode(&gate, &sentinel).unwrap());
        std::fs::create_dir(&sentinel).unwrap();
        assert!(super::super::protected_file::same_inode(&gate, &sentinel).is_err());
        std::fs::remove_dir(&sentinel).unwrap();
        let fifo = std::ffi::CString::new(sentinel.as_os_str().as_bytes()).unwrap();
        // SAFETY: the CString is terminated and lives through this syscall.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(super::super::protected_file::same_inode(&gate, &sentinel).is_err());
        std::fs::remove_file(&sentinel).unwrap();
        std::os::unix::fs::symlink(tmp.path().join(LOCK_GATE_FILENAME), &sentinel).unwrap();
        assert!(super::super::protected_file::same_inode(&gate, &sentinel).is_err());
        // Cleanup must inspect a substituted symlink rather than following it.
        std::fs::remove_file(&sentinel).unwrap();
        let store = AuthProfilesStore::new(tmp.path(), false);
        let guard = store.acquire_lock().await.unwrap();
        std::fs::remove_file(&sentinel).unwrap();
        std::os::unix::fs::symlink(tmp.path().join(LOCK_GATE_FILENAME), &sentinel).unwrap();
        drop(guard);
        assert!(
            std::fs::symlink_metadata(&sentinel)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn canonical_store_detects_gate_replacement_before_publication() {
        let tmp = TempDir::new().unwrap();
        let store = AuthProfilesStore::new(tmp.path(), false);
        let error = REPLACE_GATE_BEFORE_PUBLICATION
            .scope(std::cell::Cell::new(true), async {
                store.load().await.unwrap_err()
            })
            .await;
        assert!(error.to_string().contains("kernel lock identity changed"));
        // The rejected publication belongs to the substituted gate, not the
        // file we locked. Do not unlink it under the old inode's lease.
        assert!(store.lock_path.exists());
        assert!(store.load().await.unwrap().profiles.is_empty());
    }

    #[test]
    fn token_debug_does_not_expose_credentials() {
        let tokens = TokenSet {
            access_token: "synthetic-access-secret".into(),
            refresh_token: Some("synthetic-refresh-secret".into()),
            id_token: Some("synthetic-id-secret".into()),
            expires_at: None,
            token_type: None,
            scope: None,
        };
        let debug = format!("{tokens:?}");
        for secret in [
            "synthetic-access-secret",
            "synthetic-refresh-secret",
            "synthetic-id-secret",
        ] {
            assert!(!debug.contains(secret));
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn protected_store_rejects_symlink_read_and_write() {
        use std::os::unix::fs::symlink;
        let tmp = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let target = outside.path().join("target");
        let original = b"{}";
        std::fs::write(&target, original).unwrap();
        let store = AuthProfilesStore::new(tmp.path(), true);
        symlink(&target, store.path()).unwrap();
        assert!(store.load().await.is_err());
        assert!(
            store
                .upsert_profile(
                    AuthProfile::new_token("anthropic", "default", "synthetic".into()),
                    false
                )
                .await
                .is_err()
        );
        assert_eq!(std::fs::read(target).unwrap(), original);
        assert!(
            std::fs::symlink_metadata(store.path())
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn protected_store_final_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let store = AuthProfilesStore::new(tmp.path(), true);
        store
            .upsert_profile(
                AuthProfile::new_token("anthropic", "default", "synthetic".into()),
                false,
            )
            .await
            .unwrap();
        assert_eq!(
            std::fs::metadata(store.path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[tokio::test]
    async fn protected_store_failed_replace_leaves_no_temporary_file() {
        let tmp = TempDir::new().unwrap();
        let store = AuthProfilesStore::new(tmp.path(), false);
        std::fs::create_dir(store.path()).unwrap();
        let persisted = PersistedAuthProfiles::default();
        assert!(store.write_persisted_locked(&persisted).await.is_err());
        assert!(store.path().is_dir());
        assert!(!std::fs::read_dir(tmp.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".tmp.")
        }));
    }

    #[test]
    fn profile_id_format() {
        assert_eq!(
            profile_id("openai-codex", "default"),
            "openai-codex:default"
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
