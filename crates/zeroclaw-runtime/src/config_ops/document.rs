//! Whole-document config operations shared by the gateway's
//! `/api/config/init` and `/api/config/migrate` routes and the RPC
//! `config/init` and `config/migrate` methods, plus the scoped validation
//! every config write runs before it commits.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use zeroclaw_config::api_error::{ConfigApiCode, ConfigApiError};
use zeroclaw_config::schema::Config;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct InitResponse {
    pub initialized: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct MigrateResponse {
    pub migrated: bool,
    /// Backup path written when migration ran; absent when the config was
    /// already at the current schema version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_path: Option<String>,
    pub schema_version: u32,
}

/// Validate a working copy, refusing only on errors that touch a path this
/// write dirtied (or carry no path). A pre-existing error elsewhere is saved
/// through and surfaced as a warning so one broken section cannot wedge edits
/// to every other section.
pub fn scoped_validate(
    working: &zeroclaw_config::schema::Config,
) -> Result<Vec<zeroclaw_config::validation_warnings::ValidationWarning>, ConfigApiError> {
    if let Err(e) = working.validate() {
        let api_err = ConfigApiError::from_validation(e);
        let err_path = api_err.path.as_deref().unwrap_or("");
        let touches_dirty = !err_path.is_empty()
            && working.dirty_paths.iter().any(|d| {
                err_path == d.as_str()
                    || err_path.starts_with(&format!("{d}."))
                    || d.starts_with(&format!("{err_path}."))
            });
        if touches_dirty || err_path.is_empty() {
            return Err(api_err);
        }
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"path": err_path})),
            &format!(
                "validate() failed on a path outside this PATCH's dirty set; saving anyway and \
             surfacing as a warning: {}",
                api_err.message
            )
        );
        return Ok(vec![
            zeroclaw_config::validation_warnings::ValidationWarning::new(
                "pre_existing_validation_error",
                api_err.message,
                err_path.to_string(),
            ),
        ]);
    }
    Ok(Vec::new())
}

/// Instantiate `None` nested sections with their defaults (all of them, or
/// only those under `section`), mark each dirty and validate. Returns the
/// sections it initialized; an empty list means nothing to persist.
pub fn apply_init(
    working: &mut Config,
    section: Option<&str>,
) -> Result<Vec<String>, ConfigApiError> {
    let initialized: Vec<String> = working
        .init_defaults(section)
        .into_iter()
        .map(str::to_string)
        .collect();
    if initialized.is_empty() {
        return Ok(initialized);
    }
    for section in &initialized {
        working.mark_dirty(section);
    }
    scoped_validate(working)?;
    Ok(initialized)
}

/// Result of [`migrate_config_file`].
pub struct MigrateOutcome {
    pub response: MigrateResponse,
    /// Set when the file was rewritten. The caller flags a daemon reload and
    /// does not install the parsed file as live: that parse skips secret
    /// decryption and env overrides, which only the normal loader applies,
    /// and the live config was already migrated in memory when it loaded.
    pub needs_reload: bool,
}

/// Migrate the on-disk config at `config_path` to the current schema:
/// validate the migrated snapshot, write it to a temp file, back up the
/// original to `.toml.bak`, then atomically replace it. `accept` sees the
/// migrated snapshot before anything is written and can refuse it (the RPC
/// surface rejects an invalid authorization policy there). The caller holds
/// its config write lock across this and the swap.
pub async fn migrate_config_file(
    config_path: &Path,
    data_dir: PathBuf,
    accept: impl FnOnce(&Config) -> Result<(), ConfigApiError>,
) -> Result<MigrateOutcome, ConfigApiError> {
    let raw = match tokio::fs::read_to_string(&config_path).await {
        Ok(s) => s,
        Err(e) => {
            return Err(ConfigApiError::new(
                ConfigApiCode::InternalError,
                format!("failed to read config file: {e}"),
            ));
        }
    };

    let migrated = match zeroclaw_config::migration::migrate_file(&raw) {
        Ok(out) => out,
        Err(e) => {
            return Err(ConfigApiError::new(
                ConfigApiCode::ValidationFailed,
                format!("migration failed: {e}"),
            ));
        }
    };

    match migrated {
        Some(new_content) => {
            // Validate the migrated snapshot before touching the canonical
            // file. `config_path` and `data_dir` are runtime-selected and
            // skipped by serde, so restore them explicitly on the parsed live
            // snapshot.
            let mut new_cfg: zeroclaw_config::schema::Config = match toml::from_str(&new_content) {
                Ok(c) => c,
                Err(e) => {
                    return Err(ConfigApiError::new(
                        ConfigApiCode::ReloadFailed,
                        format!("re-parse after migration failed: {e}"),
                    ));
                }
            };
            new_cfg.config_path = config_path.to_path_buf();
            new_cfg.data_dir = data_dir;
            accept(&new_cfg)?;

            let backup_path = config_path.with_extension("toml.bak");
            let parent = match config_path.parent() {
                Some(p) => p.to_path_buf(),
                None => {
                    return Err(ConfigApiError::new(
                        ConfigApiCode::InternalError,
                        format!(
                            "config path has no parent: {}",
                            config_path.display().to_string()
                        ),
                    ));
                }
            };
            let file_name = match config_path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => {
                    return Err(ConfigApiError::new(
                        ConfigApiCode::InternalError,
                        format!(
                            "config path has no file name: {}",
                            config_path.display().to_string()
                        ),
                    ));
                }
            };
            let temp_path = parent.join(format!(".{file_name}.tmp-{}", uuid::Uuid::new_v4()));

            // 1. Write migrated content to temp + fsync.
            match tokio::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temp_path)
                .await
            {
                Ok(mut temp) => {
                    use tokio::io::AsyncWriteExt;
                    if let Err(e) = temp.write_all(new_content.as_bytes()).await {
                        let _ = tokio::fs::remove_file(&temp_path).await;
                        return Err(ConfigApiError::new(
                            ConfigApiCode::InternalError,
                            format!("failed to write migrated config to temp: {e}"),
                        ));
                    }
                    if let Err(e) = temp.sync_all().await {
                        let _ = tokio::fs::remove_file(&temp_path).await;
                        return Err(ConfigApiError::new(
                            ConfigApiCode::InternalError,
                            format!("failed to fsync migrated config temp: {e}"),
                        ));
                    }
                }
                Err(e) => {
                    return Err(ConfigApiError::new(
                        ConfigApiCode::InternalError,
                        format!("failed to create temp config file: {e}"),
                    ));
                }
            }

            // 2. Backup BEFORE replacing the original.
            if let Err(e) = tokio::fs::copy(&config_path, &backup_path).await {
                let _ = tokio::fs::remove_file(&temp_path).await;
                return Err(ConfigApiError::new(
                    ConfigApiCode::InternalError,
                    format!("failed to write backup: {e}"),
                ));
            }

            // 3. Atomic rename. On failure, restore from backup.
            if let Err(e) = tokio::fs::rename(&temp_path, &config_path).await {
                let _ = tokio::fs::remove_file(&temp_path).await;
                if backup_path.exists() {
                    let _ = tokio::fs::copy(&backup_path, &config_path).await;
                }
                return Err(ConfigApiError::new(
                    ConfigApiCode::InternalError,
                    format!("failed to atomically replace config: {e}"),
                ));
            }

            // 4. Fsync the parent directory so the rename is durable.
            #[cfg(unix)]
            if let Ok(dir) = tokio::fs::File::open(&parent).await {
                let _ = dir.sync_all().await;
            }

            Ok(MigrateOutcome {
                response: MigrateResponse {
                    migrated: true,
                    backup_path: Some(backup_path.display().to_string()),
                    schema_version: zeroclaw_config::migration::CURRENT_SCHEMA_VERSION,
                },
                needs_reload: true,
            })
        }
        None => Ok(MigrateOutcome {
            response: MigrateResponse {
                migrated: false,
                backup_path: None,
                schema_version: zeroclaw_config::migration::CURRENT_SCHEMA_VERSION,
            },
            needs_reload: false,
        }),
    }
}
