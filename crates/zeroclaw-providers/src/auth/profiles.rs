use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::pin::Pin;
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

/// Test-only, one-shot post-rename failure injection. It is keyed by the
/// profile-store path so concurrent test stores cannot consume each other's
/// fault. The hook belongs in the common writer funnel, which both direct
/// profile mutations and staged onboarding writes use.
#[cfg(test)]
static FAIL_POST_RENAME_SYNC_FOR_PATHS: std::sync::Mutex<Vec<PathBuf>> =
    std::sync::Mutex::new(Vec::new());

type DirectorySyncFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;
type PathPermissionFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;
type FileSyncFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthProfileKind {
    OAuth,
    Token,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthProfilesData {
    pub schema_version: u32,
    pub updated_at: DateTime<Utc>,
    pub active_profiles: BTreeMap<String, String>,
    pub profiles: BTreeMap<String, AuthProfile>,
}

/// Exact state for one provider/profile binding, used only to compensate a
/// failed multi-store operation. It deliberately does not expose a whole
/// profile-store replacement, so unrelated providers and profiles remain
/// outside the rollback scope.
#[derive(Debug, Clone)]
pub struct ProfileBindingSnapshot {
    pub profile: Option<AuthProfile>,
}

/// Result of replacing the profile store atomically.
///
/// A rename has already committed the new store contents. Failure to fsync the
/// containing directory is therefore reported as a warning, not as a failed
/// replace that callers might try to compensate by discarding committed state.
#[derive(Debug)]
pub enum ProfileSaveOutcome {
    Durable,
    CommittedWithDurabilityWarning(anyhow::Error),
}

impl ProfileSaveOutcome {
    fn into_legacy_result(self) -> Result<()> {
        match self {
            // A rename has already made the replacement visible. Legacy
            // callers have no warning channel, so treating this as a failed
            // write would invite compensating writes against committed state.
            Self::Durable | Self::CommittedWithDurabilityWarning(_) => Ok(()),
        }
    }
}

/// A single-lock profile write together with the state it displaced. This is
/// used to compensate a later failure in a separate store without a window in
/// which another writer can be overwritten before the rollback CAS applies.
#[derive(Debug)]
pub struct StagedProfileBinding {
    pub snapshot: ProfileBindingSnapshot,
    pub staged_profile: AuthProfile,
    pub save_outcome: ProfileSaveOutcome,
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
    #[cfg(test)]
    parent_preparation_override: Option<fn(&Path) -> Result<()>>,
}

/// Proof that this store's parent directory has been prepared for an
/// operation that can create local key material or profile files.
struct PreparedProfileDirectory;

impl AuthProfilesStore {
    pub fn new(state_dir: &Path, encrypt_secrets: bool) -> Self {
        Self {
            path: state_dir.join(PROFILES_FILENAME),
            lock_path: state_dir.join(LOCK_FILENAME),
            secret_store: SecretStore::new(state_dir, encrypt_secrets),
            #[cfg(test)]
            parent_preparation_override: None,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn load(&self) -> Result<AuthProfilesData> {
        let _lock = self.acquire_lock().await?;
        self.load_locked().await
    }

    /// Replace one profile binding and capture the state it displaced while
    /// holding the same store lock. This write never changes active selection.
    pub async fn stage_profile_binding(
        &self,
        mut profile: AuthProfile,
    ) -> Result<StagedProfileBinding> {
        let _lock = self.acquire_lock().await?;
        let mut data = self.load_locked().await?;
        let snapshot = ProfileBindingSnapshot {
            profile: data.profiles.get(&profile.id).cloned(),
        };

        profile.updated_at = Utc::now();
        if let Some(existing) = &snapshot.profile {
            profile.created_at = existing.created_at;
        }
        data.profiles.insert(profile.id.clone(), profile.clone());
        data.updated_at = Utc::now();
        let save_outcome = self.save_locked_with_outcome(&data).await?;

        Ok(StagedProfileBinding {
            snapshot,
            staged_profile: profile,
            save_outcome,
        })
    }

    /// Restore exactly one profile only when it still contains the staged
    /// value. The active selector is deliberately not restored: a staged
    /// alias-bound write never changes it, and replacing it could erase a
    /// concurrent operator choice.
    pub async fn restore_profile_binding(
        &self,
        profile_id: &str,
        snapshot: ProfileBindingSnapshot,
        expected_current: &AuthProfile,
    ) -> Result<()> {
        let _lock = self.acquire_lock().await?;
        let mut data = self.load_locked().await?;

        if data.profiles.get(profile_id) != Some(expected_current) {
            anyhow::bail!(
                "refusing auth-profile rollback because the binding changed concurrently"
            );
        }

        match snapshot.profile {
            Some(profile) => {
                data.profiles.insert(profile_id.to_string(), profile);
            }
            None => {
                data.profiles.remove(profile_id);
            }
        }
        data.updated_at = Utc::now();
        self.save_locked(&data).await
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
        // Decrypting local encrypted values can initialize `.secret_key` only
        // when it is absent. Verify the parent before that side effect, but do
        // not turn ordinary credential reads into directory-mutating checks.
        if self.secret_store.needs_key_initialization() && persisted.uses_local_secret_key() {
            self.prepare_profile_directory_for_persistence().await?;
        }
        let mut migrated = false;

        let mut profiles = BTreeMap::new();
        for (id, p) in &mut persisted.profiles {
            let decrypted = p.decrypt_credentials(&self.secret_store)?;
            migrated |= decrypted.migrated;

            let kind = parse_profile_kind(&p.kind)?;
            let token_set = match kind {
                AuthProfileKind::OAuth => {
                    let access = decrypted.access_token.ok_or_else(|| {
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
                        refresh_token: decrypted.refresh_token,
                        id_token: decrypted.id_token,
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
                    token: decrypted.token,
                    metadata: p.metadata.clone(),
                    created_at: parse_datetime_with_fallback(&p.created_at),
                    updated_at: parse_datetime_with_fallback(&p.updated_at),
                },
            );
        }

        if migrated {
            self.acknowledge_legacy_save_outcome(self.write_persisted_locked(&persisted).await?)?;
        }

        Ok(AuthProfilesData {
            schema_version: persisted.schema_version,
            updated_at: parse_datetime_with_fallback(&persisted.updated_at),
            active_profiles: persisted.active_profiles,
            profiles,
        })
    }

    async fn save_locked(&self, data: &AuthProfilesData) -> Result<()> {
        self.acknowledge_legacy_save_outcome(self.save_locked_with_outcome(data).await?)
    }

    fn acknowledge_legacy_save_outcome(&self, outcome: ProfileSaveOutcome) -> Result<()> {
        if let ProfileSaveOutcome::CommittedWithDurabilityWarning(_) = &outcome {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"path": self.path.display().to_string()})),
                "auth profile replacement committed, but directory durability could not be confirmed"
            );
        }
        outcome.into_legacy_result()
    }

    async fn save_locked_with_outcome(
        &self,
        data: &AuthProfilesData,
    ) -> Result<ProfileSaveOutcome> {
        // Encryption can initialize `.secret_key`; establish the parent
        // directory's integrity before that side effect, not merely before the
        // eventual profile-file rename.
        let prepared = self.prepare_profile_directory_for_persistence().await?;

        let mut persisted = PersistedAuthProfiles {
            schema_version: CURRENT_SCHEMA_VERSION,
            updated_at: data.updated_at.to_rfc3339(),
            active_profiles: data.active_profiles.clone(),
            profiles: BTreeMap::new(),
        };

        for (id, profile) in &data.profiles {
            let (access_token, refresh_token, id_token, expires_at, token_type, scope) =
                match (&profile.kind, &profile.token_set) {
                    (AuthProfileKind::OAuth, Some(token_set)) => (
                        self.encrypt_optional(Some(&token_set.access_token))?,
                        self.encrypt_optional(token_set.refresh_token.as_deref())?,
                        self.encrypt_optional(token_set.id_token.as_deref())?,
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

        self.write_persisted_locked_after_parent_prepared(&persisted, prepared)
            .await
    }

    /// Prepare the directory that holds both encrypted profile data and key
    /// material. This must run before serialization or encryption: the latter
    /// may initialize `.secret_key` as a persistent side effect.
    async fn prepare_profile_directory_for_persistence(&self) -> Result<PreparedProfileDirectory> {
        if let Some(parent) = self.path.parent() {
            #[cfg(test)]
            if let Some(prepare_parent) = self.parent_preparation_override {
                prepare_parent(parent)?;
                return Ok(PreparedProfileDirectory);
            }

            fs::create_dir_all(parent).await.with_context(|| {
                format!(
                    "Failed to create auth profile directory at {}",
                    parent.display()
                )
            })?;
            set_owner_only_directory_permissions(parent).await?;
        }

        Ok(PreparedProfileDirectory)
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

    async fn write_persisted_locked(
        &self,
        persisted: &PersistedAuthProfiles,
    ) -> Result<ProfileSaveOutcome> {
        self.write_persisted_locked_with_directory_sync(persisted, |parent_dir| {
            Box::pin(sync_directory(parent_dir))
        })
        .await
    }

    /// Persist after the caller has prepared the parent before encrypting
    /// credentials. The capability prevents a second directory mutation.
    async fn write_persisted_locked_after_parent_prepared(
        &self,
        persisted: &PersistedAuthProfiles,
        _prepared: PreparedProfileDirectory,
    ) -> Result<ProfileSaveOutcome> {
        self.write_persisted_locked_with_sync_and_prepared_parent(
            persisted,
            |file| {
                Box::pin(async move {
                    file.sync_all()
                        .await
                        .context("Failed to fsync temporary auth profile file")
                })
            },
            |parent_dir| Box::pin(sync_directory(parent_dir)),
            true,
        )
        .await
    }

    async fn write_persisted_locked_with_directory_sync<F>(
        &self,
        persisted: &PersistedAuthProfiles,
        sync_parent_directory: F,
    ) -> Result<ProfileSaveOutcome>
    where
        F: for<'a> FnOnce(&'a Path) -> DirectorySyncFuture<'a>,
    {
        self.write_persisted_locked_with_sync(
            persisted,
            |file| {
                Box::pin(async move {
                    file.sync_all()
                        .await
                        .context("Failed to fsync temporary auth profile file")
                })
            },
            sync_parent_directory,
        )
        .await
    }

    async fn write_persisted_locked_with_sync<F, G>(
        &self,
        persisted: &PersistedAuthProfiles,
        sync_temp_file: F,
        sync_parent_directory: G,
    ) -> Result<ProfileSaveOutcome>
    where
        F: for<'a> FnOnce(&'a tokio::fs::File) -> FileSyncFuture<'a>,
        G: for<'a> FnOnce(&'a Path) -> DirectorySyncFuture<'a>,
    {
        self.write_persisted_locked_with_sync_and_prepared_parent(
            persisted,
            sync_temp_file,
            sync_parent_directory,
            false,
        )
        .await
    }

    async fn write_persisted_locked_with_sync_and_prepared_parent<F, G>(
        &self,
        persisted: &PersistedAuthProfiles,
        sync_temp_file: F,
        sync_parent_directory: G,
        parent_prepared: bool,
    ) -> Result<ProfileSaveOutcome>
    where
        F: for<'a> FnOnce(&'a tokio::fs::File) -> FileSyncFuture<'a>,
        G: for<'a> FnOnce(&'a Path) -> DirectorySyncFuture<'a>,
    {
        self.write_persisted_locked_with_operations(
            persisted,
            sync_temp_file,
            sync_parent_directory,
            |parent| Box::pin(set_owner_only_directory_permissions(parent)),
            |file| Box::pin(set_owner_only_file_permissions(file)),
            parent_prepared,
        )
        .await
    }

    async fn write_persisted_locked_with_operations<F, G, H, I>(
        &self,
        persisted: &PersistedAuthProfiles,
        sync_temp_file: F,
        sync_parent_directory: G,
        set_parent_permissions: H,
        set_file_permissions: I,
        parent_prepared: bool,
    ) -> Result<ProfileSaveOutcome>
    where
        F: for<'a> FnOnce(&'a tokio::fs::File) -> FileSyncFuture<'a>,
        G: for<'a> FnOnce(&'a Path) -> DirectorySyncFuture<'a>,
        H: for<'a> FnOnce(&'a Path) -> PathPermissionFuture<'a>,
        I: for<'a> FnOnce(&'a Path) -> PathPermissionFuture<'a>,
    {
        if !parent_prepared && let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).await.with_context(|| {
                format!(
                    "Failed to create auth profile directory at {}",
                    parent.display()
                )
            })?;
            set_parent_permissions(parent).await?;
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

        let mut temp_file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp_path)
            .await
            .with_context(|| {
                format!(
                    "Failed to create temporary auth profile file at {}",
                    tmp_path.display()
                )
            })?;
        if let Err(err) = set_file_permissions(&tmp_path).await {
            drop(temp_file);
            let _ = fs::remove_file(&tmp_path).await;
            return Err(err);
        }

        if let Err(err) = temp_file.write_all(&json).await {
            drop(temp_file);
            let _ = fs::remove_file(&tmp_path).await;
            return Err(err).context("Failed to write temporary auth profile contents");
        }
        if let Err(err) = sync_temp_file(&temp_file).await {
            drop(temp_file);
            let _ = fs::remove_file(&tmp_path).await;
            return Err(err);
        }
        drop(temp_file);

        if let Err(err) = fs::rename(&tmp_path, &self.path).await {
            let _ = fs::remove_file(&tmp_path).await;
            return Err(err).with_context(|| {
                format!(
                    "Failed to replace auth profile store at {}",
                    self.path.display()
                )
            });
        }

        let parent_dir = self.path.parent().expect("profile path has a parent");
        #[cfg(test)]
        let post_rename_sync = if take_post_rename_sync_failure_for_test(&self.path) {
            Err(anyhow::Error::msg(
                "synthetic post-rename auth-profile directory sync failure",
            ))
        } else {
            sync_parent_directory(parent_dir).await
        };
        #[cfg(not(test))]
        let post_rename_sync = sync_parent_directory(parent_dir).await;

        match post_rename_sync {
            Ok(()) => Ok(ProfileSaveOutcome::Durable),
            Err(err) => Ok(ProfileSaveOutcome::CommittedWithDurabilityWarning(err)),
        }
    }

    fn encrypt_optional(&self, value: Option<&str>) -> Result<Option<String>> {
        match value {
            Some(value) if !value.is_empty() => self.secret_store.encrypt(value).map(Some),
            Some(_) | None => Ok(None),
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

#[cfg(test)]
pub(crate) fn arm_post_rename_sync_failure_for_test(profile_store_path: &Path) {
    FAIL_POST_RENAME_SYNC_FOR_PATHS
        .lock()
        .expect("post-rename failure injection lock")
        .push(profile_store_path.to_path_buf());
}

#[cfg(test)]
pub(crate) fn post_rename_sync_failure_armed(profile_store_path: &Path) -> bool {
    FAIL_POST_RENAME_SYNC_FOR_PATHS
        .lock()
        .expect("post-rename failure injection lock")
        .iter()
        .any(|path| path == profile_store_path)
}

#[cfg(test)]
fn take_post_rename_sync_failure_for_test(profile_store_path: &Path) -> bool {
    let mut armed = FAIL_POST_RENAME_SYNC_FOR_PATHS
        .lock()
        .expect("post-rename failure injection lock");
    if let Some(index) = armed.iter().position(|path| path == profile_store_path) {
        armed.swap_remove(index);
        true
    } else {
        false
    }
}

#[cfg(unix)]
async fn set_owner_only_directory_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .await
        .with_context(|| {
            format!(
                "Failed to set owner-only auth-profile directory permissions: {}",
                path.display()
            )
        })?;

    let metadata = fs::metadata(path).await.with_context(|| {
        format!(
            "Failed to verify auth-profile directory permissions: {}",
            path.display()
        )
    })?;
    ensure_owner_only_directory_permissions(
        path,
        metadata.permissions().mode(),
        metadata.uid(),
        nix::unistd::geteuid().as_raw(),
    )
}

#[cfg(unix)]
fn ensure_owner_only_directory_permissions(
    path: &Path,
    mode: u32,
    owner_uid: u32,
    effective_uid: u32,
) -> Result<()> {
    if owner_uid != effective_uid {
        anyhow::bail!(
            "Auth-profile directory is not owned by the effective daemon user after tightening: {} (owner uid {owner_uid}, effective uid {effective_uid})",
            path.display()
        );
    }

    if mode & 0o077 != 0 {
        anyhow::bail!(
            "Auth-profile directory permissions remain broader than owner-only after tightening: {} (mode {:o})",
            path.display(),
            mode & 0o777
        );
    }

    Ok(())
}

#[cfg(not(unix))]
async fn set_owner_only_directory_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
async fn set_owner_only_file_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .await
        .with_context(|| {
            format!(
                "Failed to set owner-only auth-profile file permissions: {}",
                path.display()
            )
        })
}

#[cfg(not(unix))]
async fn set_owner_only_file_permissions(_path: &Path) -> Result<()> {
    Ok(())
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

#[derive(Clone, Copy)]
enum PersistedCredentialField {
    AccessToken,
    RefreshToken,
    IdToken,
    Token,
}

#[derive(Default)]
struct DecryptedPersistedCredentials {
    access_token: Option<String>,
    refresh_token: Option<String>,
    id_token: Option<String>,
    token: Option<String>,
    migrated: bool,
}

impl PersistedAuthProfile {
    /// The persisted credential fields. Keeping the field inventory here makes
    /// the key-provisioning preflight and decryption path evolve together.
    fn credential_fields(&self) -> [(PersistedCredentialField, &Option<String>); 4] {
        [
            (PersistedCredentialField::AccessToken, &self.access_token),
            (PersistedCredentialField::RefreshToken, &self.refresh_token),
            (PersistedCredentialField::IdToken, &self.id_token),
            (PersistedCredentialField::Token, &self.token),
        ]
    }

    fn credential_fields_mut(&mut self) -> [(PersistedCredentialField, &mut Option<String>); 4] {
        [
            (
                PersistedCredentialField::AccessToken,
                &mut self.access_token,
            ),
            (
                PersistedCredentialField::RefreshToken,
                &mut self.refresh_token,
            ),
            (PersistedCredentialField::IdToken, &mut self.id_token),
            (PersistedCredentialField::Token, &mut self.token),
        ]
    }

    fn uses_local_secret_key(&self) -> bool {
        self.credential_fields()
            .into_iter()
            .filter_map(|(_, value)| value.as_deref())
            .any(|value| value.starts_with("enc2:") || SecretStore::needs_migration(value))
    }

    fn decrypt_credentials(
        &mut self,
        secret_store: &SecretStore,
    ) -> Result<DecryptedPersistedCredentials> {
        let mut decrypted = DecryptedPersistedCredentials::default();

        for (field, value) in self.credential_fields_mut() {
            let Some(stored) = value.as_deref().filter(|value| !value.is_empty()) else {
                continue;
            };
            let (plaintext, migrated) = secret_store.decrypt_and_migrate(stored)?;
            if let Some(migrated) = migrated {
                *value = Some(migrated);
                decrypted.migrated = true;
            }
            match field {
                PersistedCredentialField::AccessToken => decrypted.access_token = Some(plaintext),
                PersistedCredentialField::RefreshToken => decrypted.refresh_token = Some(plaintext),
                PersistedCredentialField::IdToken => decrypted.id_token = Some(plaintext),
                PersistedCredentialField::Token => decrypted.token = Some(plaintext),
            }
        }

        Ok(decrypted)
    }
}

impl PersistedAuthProfiles {
    /// Whether loading one of these profiles can provision local key material
    /// through `SecretStore::decrypt_and_migrate`.
    fn uses_local_secret_key(&self) -> bool {
        self.profiles
            .values()
            .any(PersistedAuthProfile::uses_local_secret_key)
    }
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

#[allow(clippy::unused_async)]
async fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        let dir = tokio::fs::File::open(path).await.with_context(|| {
            format!(
                "Failed to open auth-profile directory for fsync: {}",
                path.display()
            )
        })?;
        dir.sync_all().await.with_context(|| {
            format!(
                "Failed to fsync auth-profile directory metadata: {}",
                path.display()
            )
        })
    }

    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    #[tokio::test]
    async fn profile_write_fsync_failure_leaves_no_committed_store() {
        let tmp = TempDir::new().unwrap();
        let store = AuthProfilesStore::new(tmp.path(), false);

        let err = store
            .write_persisted_locked_with_sync(
                &PersistedAuthProfiles::default(),
                |_| Box::pin(async { anyhow::bail!("synthetic file fsync failure") }),
                |parent| Box::pin(sync_directory(parent)),
            )
            .await
            .expect_err("pre-rename fsync failure must fail the write");

        assert!(err.to_string().contains("synthetic file fsync failure"));
        assert!(
            !store.path().exists(),
            "a pre-rename failure must not commit the profile store"
        );
    }

    #[tokio::test]
    async fn profile_write_reports_post_rename_directory_sync_failure_as_committed() {
        let tmp = TempDir::new().unwrap();
        let store = AuthProfilesStore::new(tmp.path(), false);

        let outcome = store
            .write_persisted_locked_with_directory_sync(&PersistedAuthProfiles::default(), |_| {
                Box::pin(async { anyhow::bail!("synthetic parent fsync failure") })
            })
            .await
            .unwrap();

        assert!(matches!(
            outcome,
            ProfileSaveOutcome::CommittedWithDurabilityWarning(_)
        ));
        assert!(store.path().exists(), "rename must remain committed");
        let persisted: PersistedAuthProfiles =
            serde_json::from_slice(&tokio::fs::read(store.path()).await.unwrap()).unwrap();
        assert_eq!(persisted.schema_version, CURRENT_SCHEMA_VERSION);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn profile_write_uses_owner_only_permissions_for_parent_and_replaced_store() {
        let tmp = TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o755)).unwrap();

        let store = AuthProfilesStore::new(&state_dir, false);
        std::fs::write(store.path(), b"legacy").unwrap();
        std::fs::set_permissions(store.path(), std::fs::Permissions::from_mode(0o644)).unwrap();

        let outcome = store
            .write_persisted_locked_with_sync(
                &PersistedAuthProfiles::default(),
                |file| {
                    Box::pin(async move {
                        let mode = file.metadata().await?.permissions().mode() & 0o777;
                        assert_eq!(mode, 0o600, "temporary profile store must be owner-only");
                        Ok(())
                    })
                },
                |parent| Box::pin(sync_directory(parent)),
            )
            .await
            .expect("profile replacement must succeed");

        assert!(matches!(outcome, ProfileSaveOutcome::Durable));
        assert_eq!(
            std::fs::metadata(&state_dir).unwrap().permissions().mode() & 0o777,
            0o700,
            "auth-profile parent directory must be owner-only"
        );
        assert_eq!(
            std::fs::metadata(store.path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600,
            "replaced auth-profile store must inherit owner-only temporary-file permissions"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn profile_write_rejects_unsafe_parent_when_permission_tightening_fails() {
        let tmp = TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o777)).unwrap();

        let store = AuthProfilesStore::new(&state_dir, false);
        let err = store
            .write_persisted_locked_with_operations(
                &PersistedAuthProfiles::default(),
                |_| Box::pin(async { panic!("unsafe parent must prevent temporary-file sync") }),
                |_| Box::pin(async { panic!("unsafe parent must prevent directory sync") }),
                |_| Box::pin(async { anyhow::bail!("synthetic chmod failure") }),
                |_| {
                    Box::pin(async { panic!("unsafe parent must prevent temporary-file creation") })
                },
                false,
            )
            .await
            .expect_err("unsafe auth-profile parent must fail before profile persistence");

        assert!(err.to_string().contains("synthetic chmod failure"));
        assert_eq!(
            std::fs::metadata(&state_dir).unwrap().permissions().mode() & 0o777,
            0o777,
            "failed permission tightening must not hide the unsafe parent"
        );
        assert!(
            !store.path().exists(),
            "unsafe auth-profile parent must fail before committing the profile store"
        );
        assert!(
            std::fs::read_dir(&state_dir).unwrap().next().is_none(),
            "unsafe auth-profile parent must fail before creating a temporary profile file"
        );
    }

    #[tokio::test]
    async fn profile_write_removes_temp_file_when_file_permission_tightening_fails() {
        let tmp = TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        let store = AuthProfilesStore::new(&state_dir, false);

        let err = store
            .write_persisted_locked_with_operations(
                &PersistedAuthProfiles::default(),
                |_| {
                    Box::pin(async {
                        panic!("file-permission failure must prevent temporary-file sync")
                    })
                },
                |_| {
                    Box::pin(async {
                        panic!("file-permission failure must prevent directory sync")
                    })
                },
                |parent| Box::pin(set_owner_only_directory_permissions(parent)),
                |_| Box::pin(async { anyhow::bail!("synthetic file chmod failure") }),
                false,
            )
            .await
            .expect_err("temporary-file permission failure must fail the write");

        assert!(err.to_string().contains("synthetic file chmod failure"));
        assert!(
            !store.path().exists(),
            "temporary-file permission failure must not commit the profile store"
        );
        assert!(
            std::fs::read_dir(&state_dir).unwrap().next().is_none(),
            "temporary-file permission failure must remove the temporary profile file"
        );
    }

    #[cfg(unix)]
    #[test]
    fn owner_only_directory_permissions_reject_broader_mode_after_chmod() {
        let path = Path::new("/synthetic/auth-profile-directory");
        let err = ensure_owner_only_directory_permissions(path, 0o777, 501, 501)
            .expect_err("a chmod no-op must not be treated as owner-only enforcement");

        assert!(err.to_string().contains("remain broader than owner-only"));
        assert!(err.to_string().contains("777"));
    }

    #[cfg(unix)]
    #[test]
    fn owner_only_directory_permissions_rejects_non_owner_after_chmod() {
        let path = Path::new("/synthetic/auth-profile-directory");
        let err = ensure_owner_only_directory_permissions(path, 0o700, 501, 502)
            .expect_err("a mode-safe directory owned by another account must be rejected");

        assert!(
            err.to_string()
                .contains("not owned by the effective daemon user")
        );
        assert!(err.to_string().contains("owner uid 501"));
        assert!(err.to_string().contains("effective uid 502"));
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

    #[cfg(unix)]
    #[tokio::test]
    async fn encrypted_public_upsert_rejects_unsafe_parent_before_key_creation() {
        let tmp = TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o777)).unwrap();

        let mut store = AuthProfilesStore::new(&state_dir, true);
        store.parent_preparation_override =
            Some(|_| anyhow::bail!("synthetic parent-integrity failure"));

        let err = store
            .upsert_profile(
                AuthProfile::new_token("anthropic", "subscription", "test-token".into()),
                false,
            )
            .await
            .expect_err("public encrypted upsert must fail before key creation");

        assert!(
            err.to_string()
                .contains("synthetic parent-integrity failure"),
            "the public write must surface the integrity failure"
        );
        assert!(
            !state_dir.join(".secret_key").exists(),
            "a failed parent integrity check must not create encryption key material"
        );
        assert!(
            !store.path().exists(),
            "a failed parent integrity check must not persist an auth profile"
        );
        assert!(
            std::fs::read_dir(&state_dir).unwrap().next().is_none(),
            "the public write must leave no lock, key, or profile residue"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn encrypted_profile_load_rejects_unsafe_parent_before_key_creation() {
        let tmp = TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        let seed_store = AuthProfilesStore::new(&state_dir, true);
        seed_store
            .upsert_profile(
                AuthProfile::new_token("anthropic", "subscription", "test-token".into()),
                false,
            )
            .await
            .unwrap();
        std::fs::remove_file(state_dir.join(".secret_key")).unwrap();
        std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o777)).unwrap();

        let mut store = AuthProfilesStore::new(&state_dir, true);
        store.parent_preparation_override =
            Some(|_| anyhow::bail!("synthetic parent-integrity failure"));

        let err = store
            .load()
            .await
            .expect_err("encrypted profile load must fail before key creation");

        assert!(
            err.to_string()
                .contains("synthetic parent-integrity failure"),
            "the encrypted load must surface the integrity failure"
        );
        assert!(
            !state_dir.join(".secret_key").exists(),
            "an unsafe encrypted-profile load must not create replacement key material"
        );
        assert!(
            store.path().exists(),
            "the failed load must leave the existing encrypted profile untouched"
        );
        assert!(
            !state_dir.join(LOCK_FILENAME).exists(),
            "the failed load must release its profile lock"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn encrypted_profile_load_with_existing_key_skips_key_provisioning_preflight() {
        let tmp = TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        let seed_store = AuthProfilesStore::new(&state_dir, true);
        seed_store
            .upsert_profile(
                AuthProfile::new_token("anthropic", "subscription", "test-token".into()),
                false,
            )
            .await
            .unwrap();
        assert!(
            state_dir.join(".secret_key").is_file(),
            "the encrypted seed must create the local key before the read-path check"
        );

        let mut store = AuthProfilesStore::new(&state_dir, true);
        store.parent_preparation_override =
            Some(|_| panic!("an initialized key must not run the key-provisioning preflight"));

        let loaded = store
            .load()
            .await
            .expect("encrypted profile load with an initialized key must not prepare its parent");
        assert_eq!(
            loaded
                .profiles
                .get(&profile_id("anthropic", "subscription"))
                .and_then(|profile| profile.token.as_deref()),
            Some("test-token")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn plaintext_profile_load_skips_key_provisioning_preflight() {
        let tmp = TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        let plaintext_store = AuthProfilesStore::new(&state_dir, false);
        plaintext_store
            .upsert_profile(
                AuthProfile::new_token("anthropic", "subscription", "test-token".into()),
                false,
            )
            .await
            .unwrap();

        let mut store = AuthProfilesStore::new(&state_dir, true);
        store.parent_preparation_override =
            Some(|_| panic!("plaintext profile load must not prepare a key-material parent"));

        let loaded = store
            .load()
            .await
            .expect("plaintext profile load must not run key-provisioning preflight");
        assert_eq!(
            loaded
                .profiles
                .get(&profile_id("anthropic", "subscription"))
                .and_then(|profile| profile.token.as_deref()),
            Some("test-token")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn empty_profile_load_skips_key_provisioning_preflight() {
        let tmp = TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();

        let mut store = AuthProfilesStore::new(&state_dir, true);
        store.parent_preparation_override =
            Some(|_| panic!("empty profile load must not prepare a key-material parent"));

        let loaded = store
            .load()
            .await
            .expect("empty profile load must not run key-provisioning preflight");
        assert!(loaded.profiles.is_empty());
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
    async fn rollback_refuses_to_overwrite_a_concurrent_profile_update() {
        let tmp = TempDir::new().unwrap();
        let store = AuthProfilesStore::new(tmp.path(), false);
        let id = profile_id("anthropic", "subscription");

        let previous = AuthProfile::new_token("anthropic", "subscription", "old".into());
        store.upsert_profile(previous, false).await.unwrap();
        let staged = store
            .stage_profile_binding(AuthProfile::new_token(
                "anthropic",
                "subscription",
                "staged".into(),
            ))
            .await
            .unwrap();
        store
            .upsert_profile(
                AuthProfile::new_token("anthropic", "subscription", "concurrent".into()),
                false,
            )
            .await
            .unwrap();

        let err = store
            .restore_profile_binding(&id, staged.snapshot, &staged.staged_profile)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("changed concurrently"));

        let data = store.load().await.unwrap();
        assert_eq!(
            data.profiles
                .get(&id)
                .and_then(|profile| profile.token.as_deref()),
            Some("concurrent")
        );
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
