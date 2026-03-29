use crate::providers::{is_glm_alias, is_zai_alias};
use crate::security::DomainMatcher;
use anyhow::{Context, Result};
use directories::UserDirs;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
#[cfg(unix)]
use tokio::fs::File;
use tokio::fs::{self, OpenOptions};
use tokio::io::AsyncWriteExt;

use super::*;

fn default_config_and_workspace_dirs() -> Result<(PathBuf, PathBuf)> {
    let config_dir = default_config_dir()?;
    Ok((config_dir.clone(), config_dir.join("workspace")))
}

pub(super) const ACTIVE_WORKSPACE_STATE_FILE: &str = "active_workspace.toml";

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct ActiveWorkspaceState {
    pub(super) config_dir: String,
}

fn default_config_dir() -> Result<PathBuf> {
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return Ok(PathBuf::from(home).join(".zeroclaw"));
        }
    }

    let home = UserDirs::new()
        .map(|u| u.home_dir().to_path_buf())
        .context("Could not find home directory")?;
    Ok(home.join(".zeroclaw"))
}

fn active_workspace_state_path(default_dir: &Path) -> PathBuf {
    default_dir.join(ACTIVE_WORKSPACE_STATE_FILE)
}

/// Returns `true` if `path` lives under the OS temp directory.
fn is_temp_directory(path: &Path) -> bool {
    let temp = std::env::temp_dir();
    // Canonicalize when possible to handle symlinks (macOS /var → /private/var)
    let canon_temp = temp.canonicalize().unwrap_or_else(|_| temp.clone());
    let canon_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    canon_path.starts_with(&canon_temp)
}

async fn load_persisted_workspace_dirs(
    default_config_dir: &Path,
) -> Result<Option<(PathBuf, PathBuf)>> {
    let state_path = active_workspace_state_path(default_config_dir);
    if !state_path.exists() {
        return Ok(None);
    }

    let contents = match fs::read_to_string(&state_path).await {
        Ok(contents) => contents,
        Err(error) => {
            tracing::warn!(
                "Failed to read active workspace marker {}: {error}",
                state_path.display()
            );
            return Ok(None);
        }
    };

    let state: ActiveWorkspaceState = match toml::from_str(&contents) {
        Ok(state) => state,
        Err(error) => {
            tracing::warn!(
                "Failed to parse active workspace marker {}: {error}",
                state_path.display()
            );
            return Ok(None);
        }
    };

    let raw_config_dir = state.config_dir.trim();
    if raw_config_dir.is_empty() {
        tracing::warn!(
            "Ignoring active workspace marker {} because config_dir is empty",
            state_path.display()
        );
        return Ok(None);
    }

    let parsed_dir = expand_tilde_path(raw_config_dir);
    let config_dir = if parsed_dir.is_absolute() {
        parsed_dir
    } else {
        default_config_dir.join(parsed_dir)
    };
    Ok(Some((config_dir.clone(), config_dir.join("workspace"))))
}

pub(crate) async fn persist_active_workspace_config_dir(config_dir: &Path) -> Result<()> {
    persist_active_workspace_config_dir_in(config_dir, &default_config_dir()?).await
}

/// Inner implementation that accepts the default config directory explicitly,
/// so callers (including tests) control where the marker is written without
/// manipulating process-wide environment variables.
pub(super) async fn persist_active_workspace_config_dir_in(
    config_dir: &Path,
    default_config_dir: &Path,
) -> Result<()> {
    let state_path = active_workspace_state_path(default_config_dir);

    // Guard: refuse to write a temp-directory config_dir into a non-temp
    // default location. This prevents transient test runs or one-off
    // invocations from hijacking the real user's daemon config resolution.
    // When both paths are temp (e.g. in tests), the write is harmless.
    if is_temp_directory(config_dir) && !is_temp_directory(default_config_dir) {
        tracing::warn!(
            path = %config_dir.display(),
            "Refusing to persist temp directory as active workspace marker"
        );
        return Ok(());
    }

    if config_dir == default_config_dir {
        if state_path.exists() {
            fs::remove_file(&state_path).await.with_context(|| {
                format!(
                    "Failed to clear active workspace marker: {}",
                    state_path.display()
                )
            })?;
        }
        return Ok(());
    }

    fs::create_dir_all(&default_config_dir)
        .await
        .with_context(|| {
            format!(
                "Failed to create default config directory: {}",
                default_config_dir.display()
            )
        })?;

    let state = ActiveWorkspaceState {
        config_dir: config_dir.to_string_lossy().into_owned(),
    };
    let serialized =
        toml::to_string_pretty(&state).context("Failed to serialize active workspace marker")?;

    let temp_path = default_config_dir.join(format!(
        ".{ACTIVE_WORKSPACE_STATE_FILE}.tmp-{}",
        uuid::Uuid::new_v4()
    ));
    fs::write(&temp_path, serialized).await.with_context(|| {
        format!(
            "Failed to write temporary active workspace marker: {}",
            temp_path.display()
        )
    })?;

    if let Err(error) = fs::rename(&temp_path, &state_path).await {
        let _ = fs::remove_file(&temp_path).await;
        anyhow::bail!(
            "Failed to atomically persist active workspace marker {}: {error}",
            state_path.display()
        );
    }

    sync_directory(default_config_dir).await?;
    Ok(())
}

pub(crate) fn resolve_config_dir_for_workspace(workspace_dir: &Path) -> (PathBuf, PathBuf) {
    let workspace_config_dir = workspace_dir.to_path_buf();
    if workspace_config_dir.join("config.toml").exists() {
        return (
            workspace_config_dir.clone(),
            workspace_config_dir.join("workspace"),
        );
    }

    let legacy_config_dir = workspace_dir
        .parent()
        .map(|parent| parent.join(".zeroclaw"));
    if let Some(legacy_dir) = legacy_config_dir {
        if legacy_dir.join("config.toml").exists() {
            return (legacy_dir, workspace_config_dir);
        }

        if workspace_dir
            .file_name()
            .is_some_and(|name| name == std::ffi::OsStr::new("workspace"))
        {
            return (legacy_dir, workspace_config_dir);
        }
    }

    (
        workspace_config_dir.clone(),
        workspace_config_dir.join("workspace"),
    )
}

/// Resolve the current runtime config/workspace directories for onboarding flows.
///
/// This mirrors the same precedence used by `Config::load_or_init()`:
/// `ZEROCLAW_CONFIG_DIR` > `ZEROCLAW_WORKSPACE` > active workspace marker > defaults.
pub async fn resolve_runtime_dirs_for_onboarding() -> Result<(PathBuf, PathBuf)> {
    let (default_zeroclaw_dir, default_workspace_dir) = default_config_and_workspace_dirs()?;
    let (config_dir, workspace_dir, _) =
        resolve_runtime_config_dirs(&default_zeroclaw_dir, &default_workspace_dir).await?;
    Ok((config_dir, workspace_dir))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ConfigResolutionSource {
    EnvConfigDir,
    EnvWorkspace,
    ActiveWorkspaceMarker,
    DefaultConfigDir,
}

impl ConfigResolutionSource {
    const fn as_str(self) -> &'static str {
        match self {
            Self::EnvConfigDir => "ZEROCLAW_CONFIG_DIR",
            Self::EnvWorkspace => "ZEROCLAW_WORKSPACE",
            Self::ActiveWorkspaceMarker => "active_workspace.toml",
            Self::DefaultConfigDir => "default",
        }
    }
}

/// Expand tilde in paths, falling back to `UserDirs` when HOME is unset.
///
/// In non-TTY environments (e.g. cron), HOME may not be set, causing
/// `shellexpand::tilde` to return the literal `~` unexpanded. This helper
/// detects that case and uses `directories::UserDirs` as a fallback.
pub(super) fn expand_tilde_path(path: &str) -> PathBuf {
    let expanded = shellexpand::tilde(path);
    let expanded_str = expanded.as_ref();

    // If the path still starts with '~', tilde expansion failed (HOME unset)
    if expanded_str.starts_with('~') {
        if let Some(user_dirs) = UserDirs::new() {
            let home = user_dirs.home_dir();
            // Replace leading ~ with home directory
            if let Some(rest) = expanded_str.strip_prefix('~') {
                return home.join(rest.trim_start_matches(['/', '\\']));
            }
        }
        // If UserDirs also fails, log a warning and use the literal path
        tracing::warn!(
            path = path,
            "Failed to expand tilde: HOME environment variable is not set and UserDirs failed. \
             In cron/non-TTY environments, use absolute paths or set HOME explicitly."
        );
    }

    PathBuf::from(expanded_str)
}

pub(super) async fn resolve_runtime_config_dirs(
    default_zeroclaw_dir: &Path,
    default_workspace_dir: &Path,
) -> Result<(PathBuf, PathBuf, ConfigResolutionSource)> {
    if let Ok(custom_config_dir) = std::env::var("ZEROCLAW_CONFIG_DIR") {
        let custom_config_dir = custom_config_dir.trim();
        if !custom_config_dir.is_empty() {
            let zeroclaw_dir = expand_tilde_path(custom_config_dir);
            return Ok((
                zeroclaw_dir.clone(),
                zeroclaw_dir.join("workspace"),
                ConfigResolutionSource::EnvConfigDir,
            ));
        }
    }

    if let Ok(custom_workspace) = std::env::var("ZEROCLAW_WORKSPACE") {
        if !custom_workspace.is_empty() {
            let expanded = expand_tilde_path(&custom_workspace);
            let (zeroclaw_dir, workspace_dir) = resolve_config_dir_for_workspace(&expanded);
            return Ok((
                zeroclaw_dir,
                workspace_dir,
                ConfigResolutionSource::EnvWorkspace,
            ));
        }
    }

    if let Some((zeroclaw_dir, workspace_dir)) =
        load_persisted_workspace_dirs(default_zeroclaw_dir).await?
    {
        return Ok((
            zeroclaw_dir,
            workspace_dir,
            ConfigResolutionSource::ActiveWorkspaceMarker,
        ));
    }

    Ok((
        default_zeroclaw_dir.to_path_buf(),
        default_workspace_dir.to_path_buf(),
        ConfigResolutionSource::DefaultConfigDir,
    ))
}

pub(super) fn decrypt_optional_secret(
    store: &crate::security::SecretStore,
    value: &mut Option<String>,
    field_name: &str,
) -> Result<()> {
    if let Some(raw) = value.clone() {
        if crate::security::SecretStore::is_encrypted(&raw) {
            *value = Some(
                store
                    .decrypt(&raw)
                    .with_context(|| format!("Failed to decrypt {field_name}"))?,
            );
        }
    }
    Ok(())
}

pub(super) fn decrypt_secret(
    store: &crate::security::SecretStore,
    value: &mut String,
    field_name: &str,
) -> Result<()> {
    if crate::security::SecretStore::is_encrypted(value) {
        *value = store
            .decrypt(value)
            .with_context(|| format!("Failed to decrypt {field_name}"))?;
    }
    Ok(())
}

fn encrypt_optional_secret(
    store: &crate::security::SecretStore,
    value: &mut Option<String>,
    field_name: &str,
) -> Result<()> {
    if let Some(raw) = value.clone() {
        if !crate::security::SecretStore::is_encrypted(&raw) {
            *value = Some(
                store
                    .encrypt(&raw)
                    .with_context(|| format!("Failed to encrypt {field_name}"))?,
            );
        }
    }
    Ok(())
}

fn encrypt_secret(
    store: &crate::security::SecretStore,
    value: &mut String,
    field_name: &str,
) -> Result<()> {
    if !crate::security::SecretStore::is_encrypted(value) {
        *value = store
            .encrypt(value)
            .with_context(|| format!("Failed to encrypt {field_name}"))?;
    }
    Ok(())
}

pub(super) fn config_dir_creation_error(path: &Path) -> String {
    format!(
        "Failed to create config directory: {}. If running as an OpenRC service, \
         ensure this path is writable by user 'zeroclaw'.",
        path.display()
    )
}

fn is_local_ollama_endpoint(api_url: Option<&str>) -> bool {
    let Some(raw) = api_url.map(str::trim).filter(|value| !value.is_empty()) else {
        return true;
    };

    reqwest::Url::parse(raw)
        .ok()
        .and_then(|url| url.host_str().map(|host| host.to_ascii_lowercase()))
        .is_some_and(|host| matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1" | "0.0.0.0"))
}

fn has_ollama_cloud_credential(config_api_key: Option<&str>) -> bool {
    let config_key_present = config_api_key
        .map(str::trim)
        .is_some_and(|value| !value.is_empty());
    if config_key_present {
        return true;
    }

    ["OLLAMA_API_KEY", "ZEROCLAW_API_KEY", "API_KEY"]
        .iter()
        .any(|name| {
            std::env::var(name)
                .ok()
                .is_some_and(|value| !value.trim().is_empty())
        })
}

/// Parse the `ZEROCLAW_EXTRA_HEADERS` environment variable value.
///
/// Format: `Key:Value,Key2:Value2`
///
/// Entries without a colon or with an empty key are silently skipped.
/// Leading/trailing whitespace on both key and value is trimmed.
pub fn parse_extra_headers_env(raw: &str) -> Vec<(String, String)> {
    let mut result = Vec::new();
    for entry in raw.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        if let Some((key, value)) = entry.split_once(':') {
            let key = key.trim();
            let value = value.trim();
            if key.is_empty() {
                tracing::warn!("Ignoring extra header with empty name in ZEROCLAW_EXTRA_HEADERS");
                continue;
            }
            result.push((key.to_string(), value.to_string()));
        } else {
            tracing::warn!("Ignoring malformed extra header entry (missing ':'): {entry}");
        }
    }
    result
}

fn normalize_wire_api(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "responses" | "openai-responses" | "open-ai-responses" => Some("responses"),
        "chat_completions"
        | "chat-completions"
        | "chat"
        | "chatcompletions"
        | "openai-chat-completions"
        | "open-ai-chat-completions" => Some("chat_completions"),
        _ => None,
    }
}

fn read_codex_openai_api_key() -> Option<String> {
    let home = UserDirs::new()?.home_dir().to_path_buf();
    let auth_path = home.join(".codex").join("auth.json");
    let raw = std::fs::read_to_string(auth_path).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&raw).ok()?;

    parsed
        .get("OPENAI_API_KEY")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

/// Ensure that essential bootstrap files exist in the workspace directory.
///
/// When the workspace is created outside of `zeroclaw onboard` (e.g., non-tty
/// daemon/cron sessions), these files would otherwise be missing. This function
/// creates sensible defaults that allow the agent to operate with a basic identity.
pub(super) async fn ensure_bootstrap_files(workspace_dir: &Path) -> Result<()> {
    let defaults: &[(&str, &str)] = &[
        (
            "IDENTITY.md",
            "# IDENTITY.md — Who Am I?\n\n\
             I am ZeroClaw, an autonomous AI agent.\n\n\
             ## Traits\n\
             - Helpful, precise, and safety-conscious\n\
             - I prioritize clarity and correctness\n",
        ),
        (
            "SOUL.md",
            "# SOUL.md — Who You Are\n\n\
             You are ZeroClaw, an autonomous AI agent.\n\n\
             ## Core Principles\n\
             - Be helpful and accurate\n\
             - Respect user intent and boundaries\n\
             - Ask before taking destructive actions\n\
             - Prefer safe, reversible operations\n",
        ),
    ];

    for (filename, content) in defaults {
        let path = workspace_dir.join(filename);
        if !path.exists() {
            fs::write(&path, content)
                .await
                .with_context(|| format!("Failed to create default {filename} in workspace"))?;
        }
    }

    Ok(())
}

impl Config {
    pub async fn load_or_init() -> Result<Self> {
        let (default_zeroclaw_dir, default_workspace_dir) = default_config_and_workspace_dirs()?;

        let (zeroclaw_dir, workspace_dir, resolution_source) =
            resolve_runtime_config_dirs(&default_zeroclaw_dir, &default_workspace_dir).await?;

        let config_path = zeroclaw_dir.join("config.toml");

        fs::create_dir_all(&zeroclaw_dir)
            .await
            .with_context(|| config_dir_creation_error(&zeroclaw_dir))?;
        fs::create_dir_all(&workspace_dir)
            .await
            .context("Failed to create workspace directory")?;

        ensure_bootstrap_files(&workspace_dir).await?;

        if config_path.exists() {
            // Warn if config file is world-readable (may contain API keys)
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(meta) = fs::metadata(&config_path).await {
                    if meta.permissions().mode() & 0o004 != 0 {
                        tracing::warn!(
                            "Config file {:?} is world-readable (mode {:o}). \
                             Consider restricting with: chmod 600 {:?}",
                            config_path,
                            meta.permissions().mode() & 0o777,
                            config_path,
                        );
                    }
                }
            }

            let contents = fs::read_to_string(&config_path)
                .await
                .context("Failed to read config file")?;

            // Deserialize the config with the standard TOML parser.
            //
            // Previously this used `serde_ignored::deserialize` for both
            // deserialization and unknown-key detection.  However,
            // `serde_ignored` silently drops field values inside nested
            // structs that carry `#[serde(default)]` (e.g. the entire
            // `[autonomy]` table), causing user-supplied values to be
            // replaced by defaults.  See #4171.
            //
            // We now deserialize with `toml::from_str` (which is correct)
            // and run `serde_ignored` separately just for diagnostics.
            let mut config: Config =
                toml::from_str(&contents).context("Failed to deserialize config file")?;

            // Ensure the built-in default auto_approve entries are always
            // present.  When a user specifies `auto_approve` in their TOML
            // (e.g. to add a custom tool), serde replaces the default list
            // instead of merging.  This caused default-safe tools like
            // `weather` or `calculator` to lose their auto-approve status
            // and get silently denied in non-interactive channel runs.
            // See #4247.
            //
            // Users who want to require approval for a default tool can
            // add it to `always_ask`, which takes precedence over
            // `auto_approve` in the approval decision (see approval/mod.rs).
            config.autonomy.ensure_default_auto_approve();

            // Detect unknown top-level config keys by comparing the raw
            // TOML table keys against what Config actually deserializes.
            // This replaces the previous serde_ignored-based approach which
            // had false-positive issues with #[serde(default)] nested structs.
            if let Ok(raw) = contents.parse::<toml::Table>() {
                // Build the set of known top-level keys from a default Config
                // serialization round-trip.  This is computed once and cached.
                static KNOWN_KEYS: OnceLock<Vec<String>> = OnceLock::new();
                let known = KNOWN_KEYS.get_or_init(|| {
                    toml::to_string(&Config::default())
                        .ok()
                        .and_then(|s| s.parse::<toml::Table>().ok())
                        .map(|t| t.keys().cloned().collect())
                        .unwrap_or_default()
                });
                for key in raw.keys() {
                    if !known.contains(key) {
                        tracing::warn!(
                            "Unknown config key ignored: \"{key}\". Check config.toml for typos or deprecated options.",
                        );
                    }
                }
            }
            // Set computed paths that are skipped during serialization
            config.config_path = config_path.clone();
            config.workspace_dir = workspace_dir;
            let store = crate::security::SecretStore::new(&zeroclaw_dir, config.secrets.encrypt);
            decrypt_optional_secret(&store, &mut config.api_key, "config.api_key")?;
            decrypt_optional_secret(
                &store,
                &mut config.composio.api_key,
                "config.composio.api_key",
            )?;
            if let Some(ref mut pinggy) = config.tunnel.pinggy {
                decrypt_optional_secret(&store, &mut pinggy.token, "config.tunnel.pinggy.token")?;
            }
            decrypt_optional_secret(
                &store,
                &mut config.microsoft365.client_secret,
                "config.microsoft365.client_secret",
            )?;

            decrypt_optional_secret(
                &store,
                &mut config.browser.computer_use.api_key,
                "config.browser.computer_use.api_key",
            )?;

            decrypt_optional_secret(
                &store,
                &mut config.web_search.brave_api_key,
                "config.web_search.brave_api_key",
            )?;

            decrypt_optional_secret(
                &store,
                &mut config.storage.provider.config.db_url,
                "config.storage.provider.config.db_url",
            )?;

            for agent in config.agents.values_mut() {
                decrypt_optional_secret(&store, &mut agent.api_key, "config.agents.*.api_key")?;
            }

            // Decrypt TTS provider API keys
            if let Some(ref mut openai) = config.tts.openai {
                decrypt_optional_secret(&store, &mut openai.api_key, "config.tts.openai.api_key")?;
            }
            if let Some(ref mut elevenlabs) = config.tts.elevenlabs {
                decrypt_optional_secret(
                    &store,
                    &mut elevenlabs.api_key,
                    "config.tts.elevenlabs.api_key",
                )?;
            }
            if let Some(ref mut google) = config.tts.google {
                decrypt_optional_secret(&store, &mut google.api_key, "config.tts.google.api_key")?;
            }

            // Decrypt nested STT provider API keys
            decrypt_optional_secret(
                &store,
                &mut config.transcription.api_key,
                "config.transcription.api_key",
            )?;
            if let Some(ref mut openai) = config.transcription.openai {
                decrypt_optional_secret(
                    &store,
                    &mut openai.api_key,
                    "config.transcription.openai.api_key",
                )?;
            }
            if let Some(ref mut deepgram) = config.transcription.deepgram {
                decrypt_optional_secret(
                    &store,
                    &mut deepgram.api_key,
                    "config.transcription.deepgram.api_key",
                )?;
            }
            if let Some(ref mut assemblyai) = config.transcription.assemblyai {
                decrypt_optional_secret(
                    &store,
                    &mut assemblyai.api_key,
                    "config.transcription.assemblyai.api_key",
                )?;
            }
            if let Some(ref mut google) = config.transcription.google {
                decrypt_optional_secret(
                    &store,
                    &mut google.api_key,
                    "config.transcription.google.api_key",
                )?;
            }
            if let Some(ref mut local) = config.transcription.local_whisper {
                decrypt_optional_secret(
                    &store,
                    &mut local.bearer_token,
                    "config.transcription.local_whisper.bearer_token",
                )?;
            }

            #[cfg(feature = "channel-nostr")]
            if let Some(ref mut ns) = config.channels_config.nostr {
                decrypt_secret(
                    &store,
                    &mut ns.private_key,
                    "config.channels_config.nostr.private_key",
                )?;
            }
            if let Some(ref mut fs) = config.channels_config.feishu {
                decrypt_secret(
                    &store,
                    &mut fs.app_secret,
                    "config.channels_config.feishu.app_secret",
                )?;
                decrypt_optional_secret(
                    &store,
                    &mut fs.encrypt_key,
                    "config.channels_config.feishu.encrypt_key",
                )?;
                decrypt_optional_secret(
                    &store,
                    &mut fs.verification_token,
                    "config.channels_config.feishu.verification_token",
                )?;
            }

            // Decrypt channel secrets
            if let Some(ref mut tg) = config.channels_config.telegram {
                decrypt_secret(
                    &store,
                    &mut tg.bot_token,
                    "config.channels_config.telegram.bot_token",
                )?;
            }
            if let Some(ref mut dc) = config.channels_config.discord {
                decrypt_secret(
                    &store,
                    &mut dc.bot_token,
                    "config.channels_config.discord.bot_token",
                )?;
            }
            if let Some(ref mut sl) = config.channels_config.slack {
                decrypt_secret(
                    &store,
                    &mut sl.bot_token,
                    "config.channels_config.slack.bot_token",
                )?;
                decrypt_optional_secret(
                    &store,
                    &mut sl.app_token,
                    "config.channels_config.slack.app_token",
                )?;
            }
            if let Some(ref mut mm) = config.channels_config.mattermost {
                decrypt_secret(
                    &store,
                    &mut mm.bot_token,
                    "config.channels_config.mattermost.bot_token",
                )?;
            }
            if let Some(ref mut mx) = config.channels_config.matrix {
                decrypt_secret(
                    &store,
                    &mut mx.access_token,
                    "config.channels_config.matrix.access_token",
                )?;
                decrypt_optional_secret(
                    &store,
                    &mut mx.recovery_key,
                    "config.channels_config.matrix.recovery_key",
                )?;
            }
            if let Some(ref mut wa) = config.channels_config.whatsapp {
                decrypt_optional_secret(
                    &store,
                    &mut wa.access_token,
                    "config.channels_config.whatsapp.access_token",
                )?;
                decrypt_optional_secret(
                    &store,
                    &mut wa.app_secret,
                    "config.channels_config.whatsapp.app_secret",
                )?;
                decrypt_optional_secret(
                    &store,
                    &mut wa.verify_token,
                    "config.channels_config.whatsapp.verify_token",
                )?;
            }
            if let Some(ref mut lq) = config.channels_config.linq {
                decrypt_secret(
                    &store,
                    &mut lq.api_token,
                    "config.channels_config.linq.api_token",
                )?;
                decrypt_optional_secret(
                    &store,
                    &mut lq.signing_secret,
                    "config.channels_config.linq.signing_secret",
                )?;
            }
            if let Some(ref mut wt) = config.channels_config.wati {
                decrypt_secret(
                    &store,
                    &mut wt.api_token,
                    "config.channels_config.wati.api_token",
                )?;
            }
            if let Some(ref mut nc) = config.channels_config.nextcloud_talk {
                decrypt_secret(
                    &store,
                    &mut nc.app_token,
                    "config.channels_config.nextcloud_talk.app_token",
                )?;
                decrypt_optional_secret(
                    &store,
                    &mut nc.webhook_secret,
                    "config.channels_config.nextcloud_talk.webhook_secret",
                )?;
            }
            if let Some(ref mut em) = config.channels_config.email {
                decrypt_secret(
                    &store,
                    &mut em.password,
                    "config.channels_config.email.password",
                )?;
            }
            if let Some(ref mut gp) = config.channels_config.gmail_push {
                decrypt_secret(
                    &store,
                    &mut gp.oauth_token,
                    "config.channels_config.gmail_push.oauth_token",
                )?;
            }
            if let Some(ref mut irc) = config.channels_config.irc {
                decrypt_optional_secret(
                    &store,
                    &mut irc.server_password,
                    "config.channels_config.irc.server_password",
                )?;
                decrypt_optional_secret(
                    &store,
                    &mut irc.nickserv_password,
                    "config.channels_config.irc.nickserv_password",
                )?;
                decrypt_optional_secret(
                    &store,
                    &mut irc.sasl_password,
                    "config.channels_config.irc.sasl_password",
                )?;
            }
            if let Some(ref mut lk) = config.channels_config.lark {
                decrypt_secret(
                    &store,
                    &mut lk.app_secret,
                    "config.channels_config.lark.app_secret",
                )?;
                decrypt_optional_secret(
                    &store,
                    &mut lk.encrypt_key,
                    "config.channels_config.lark.encrypt_key",
                )?;
                decrypt_optional_secret(
                    &store,
                    &mut lk.verification_token,
                    "config.channels_config.lark.verification_token",
                )?;
            }
            if let Some(ref mut fs) = config.channels_config.feishu {
                decrypt_secret(
                    &store,
                    &mut fs.app_secret,
                    "config.channels_config.feishu.app_secret",
                )?;
                decrypt_optional_secret(
                    &store,
                    &mut fs.encrypt_key,
                    "config.channels_config.feishu.encrypt_key",
                )?;
                decrypt_optional_secret(
                    &store,
                    &mut fs.verification_token,
                    "config.channels_config.feishu.verification_token",
                )?;
            }
            if let Some(ref mut dt) = config.channels_config.dingtalk {
                decrypt_secret(
                    &store,
                    &mut dt.client_secret,
                    "config.channels_config.dingtalk.client_secret",
                )?;
            }
            if let Some(ref mut wc) = config.channels_config.wecom {
                decrypt_secret(
                    &store,
                    &mut wc.webhook_key,
                    "config.channels_config.wecom.webhook_key",
                )?;
            }
            if let Some(ref mut qq) = config.channels_config.qq {
                decrypt_secret(
                    &store,
                    &mut qq.app_secret,
                    "config.channels_config.qq.app_secret",
                )?;
            }
            if let Some(ref mut wh) = config.channels_config.webhook {
                decrypt_optional_secret(
                    &store,
                    &mut wh.secret,
                    "config.channels_config.webhook.secret",
                )?;
            }
            if let Some(ref mut ct) = config.channels_config.clawdtalk {
                decrypt_secret(
                    &store,
                    &mut ct.api_key,
                    "config.channels_config.clawdtalk.api_key",
                )?;
                decrypt_optional_secret(
                    &store,
                    &mut ct.webhook_secret,
                    "config.channels_config.clawdtalk.webhook_secret",
                )?;
            }

            // Decrypt gateway paired tokens
            for token in &mut config.gateway.paired_tokens {
                decrypt_secret(&store, token, "config.gateway.paired_tokens[]")?;
            }

            // Decrypt Nevis IAM secret
            decrypt_optional_secret(
                &store,
                &mut config.security.nevis.client_secret,
                "config.security.nevis.client_secret",
            )?;

            // Notion API key (top-level, not in ChannelsConfig)
            if !config.notion.api_key.is_empty() {
                decrypt_secret(&store, &mut config.notion.api_key, "config.notion.api_key")?;
            }

            // Jira API token
            if !config.jira.api_token.is_empty() {
                decrypt_secret(&store, &mut config.jira.api_token, "config.jira.api_token")?;
            }

            config.apply_env_overrides();
            config.validate()?;
            tracing::info!(
                path = %config.config_path.display(),
                workspace = %config.workspace_dir.display(),
                source = resolution_source.as_str(),
                initialized = true,
                "Config loaded"
            );
            Ok(config)
        } else {
            let mut config = Config::default();
            config.config_path = config_path.clone();
            config.workspace_dir = workspace_dir;
            config.save().await?;

            // Restrict permissions on newly created config file (may contain API keys)
            #[cfg(unix)]
            {
                use std::{fs::Permissions, os::unix::fs::PermissionsExt};
                let _ = fs::set_permissions(&config_path, Permissions::from_mode(0o600)).await;
            }

            config.apply_env_overrides();
            config.validate()?;
            tracing::info!(
                path = %config.config_path.display(),
                workspace = %config.workspace_dir.display(),
                source = resolution_source.as_str(),
                initialized = true,
                "Config loaded"
            );
            Ok(config)
        }
    }

    fn lookup_model_provider_profile(
        &self,
        provider_name: &str,
    ) -> Option<(String, ModelProviderConfig)> {
        let needle = provider_name.trim();
        if needle.is_empty() {
            return None;
        }

        self.model_providers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(needle))
            .map(|(name, profile)| (name.clone(), profile.clone()))
    }

    fn apply_named_model_provider_profile(&mut self) {
        let Some(current_provider) = self.default_provider.clone() else {
            return;
        };

        let Some((profile_key, profile)) = self.lookup_model_provider_profile(&current_provider)
        else {
            return;
        };

        let base_url = profile
            .base_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);

        if self
            .api_url
            .as_deref()
            .map(str::trim)
            .is_none_or(|value| value.is_empty())
        {
            if let Some(base_url) = base_url.as_ref() {
                self.api_url = Some(base_url.clone());
            }
        }

        // Propagate api_path from the profile when not already set at top level.
        if self.api_path.is_none() {
            if let Some(ref path) = profile.api_path {
                let trimmed = path.trim();
                if !trimmed.is_empty() {
                    self.api_path = Some(trimmed.to_string());
                }
            }
        }

        // Propagate max_tokens from the profile when not already set at top level.
        if self.provider_max_tokens.is_none() {
            if let Some(max_tokens) = profile.max_tokens {
                self.provider_max_tokens = Some(max_tokens);
            }
        }

        if profile.requires_openai_auth
            && self
                .api_key
                .as_deref()
                .map(str::trim)
                .is_none_or(|value| value.is_empty())
        {
            let codex_key = std::env::var("OPENAI_API_KEY")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .or_else(read_codex_openai_api_key);
            if let Some(codex_key) = codex_key {
                self.api_key = Some(codex_key);
            }
        }

        let normalized_wire_api = profile.wire_api.as_deref().and_then(normalize_wire_api);
        let profile_name = profile
            .name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());

        if normalized_wire_api == Some("responses") {
            self.default_provider = Some("openai-codex".to_string());
            return;
        }

        if let Some(profile_name) = profile_name {
            if !profile_name.eq_ignore_ascii_case(&profile_key) {
                self.default_provider = Some(profile_name.to_string());
                return;
            }
        }

        if let Some(base_url) = base_url {
            self.default_provider = Some(format!("custom:{base_url}"));
        }
    }

    /// Validate configuration values that would cause runtime failures.
    ///
    /// Called after TOML deserialization and env-override application to catch
    /// obviously invalid values early instead of failing at arbitrary runtime points.
    pub fn validate(&self) -> Result<()> {
        // Tunnel — OpenVPN
        if self.tunnel.provider.trim() == "openvpn" {
            let openvpn = self.tunnel.openvpn.as_ref().ok_or_else(|| {
                anyhow::anyhow!("tunnel.provider='openvpn' requires [tunnel.openvpn]")
            })?;

            if openvpn.config_file.trim().is_empty() {
                anyhow::bail!("tunnel.openvpn.config_file must not be empty");
            }
            if openvpn.connect_timeout_secs == 0 {
                anyhow::bail!("tunnel.openvpn.connect_timeout_secs must be greater than 0");
            }
        }

        // Gateway
        if self.gateway.host.trim().is_empty() {
            anyhow::bail!("gateway.host must not be empty");
        }
        if let Some(ref prefix) = self.gateway.path_prefix {
            // Validate the raw value — no silent trimming so the stored
            // value is exactly what was validated.
            if !prefix.is_empty() {
                if !prefix.starts_with('/') {
                    anyhow::bail!("gateway.path_prefix must start with '/'");
                }
                if prefix.ends_with('/') {
                    anyhow::bail!("gateway.path_prefix must not end with '/' (including bare '/')");
                }
                // Reject characters unsafe for URL paths or HTML/JS injection.
                // Whitespace is intentionally excluded from the allowed set.
                if let Some(bad) = prefix.chars().find(|c| {
                    !matches!(c, '/' | '-' | '_' | '.' | '~'
                        | 'a'..='z' | 'A'..='Z' | '0'..='9'
                        | '!' | '$' | '&' | '\'' | '(' | ')' | '*' | '+' | ',' | ';' | '='
                        | ':' | '@')
                }) {
                    anyhow::bail!(
                        "gateway.path_prefix contains invalid character '{bad}'; \
                         only unreserved and sub-delim URI characters are allowed"
                    );
                }
            }
        }

        // Autonomy
        if self.autonomy.max_actions_per_hour == 0 {
            anyhow::bail!("autonomy.max_actions_per_hour must be greater than 0");
        }
        for (i, env_name) in self.autonomy.shell_env_passthrough.iter().enumerate() {
            if !is_valid_env_var_name(env_name) {
                anyhow::bail!(
                    "autonomy.shell_env_passthrough[{i}] is invalid ({env_name}); expected [A-Za-z_][A-Za-z0-9_]*"
                );
            }
        }

        // Security OTP / estop
        if self.security.otp.challenge_max_attempts == 0 {
            anyhow::bail!("security.otp.challenge_max_attempts must be greater than 0");
        }
        if self.security.otp.token_ttl_secs == 0 {
            anyhow::bail!("security.otp.token_ttl_secs must be greater than 0");
        }
        if self.security.otp.cache_valid_secs == 0 {
            anyhow::bail!("security.otp.cache_valid_secs must be greater than 0");
        }
        if self.security.otp.cache_valid_secs < self.security.otp.token_ttl_secs {
            anyhow::bail!(
                "security.otp.cache_valid_secs must be greater than or equal to security.otp.token_ttl_secs"
            );
        }
        if self.security.otp.challenge_max_attempts == 0 {
            anyhow::bail!("security.otp.challenge_max_attempts must be greater than 0");
        }
        for (i, action) in self.security.otp.gated_actions.iter().enumerate() {
            let normalized = action.trim();
            if normalized.is_empty() {
                anyhow::bail!("security.otp.gated_actions[{i}] must not be empty");
            }
            if !normalized
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                anyhow::bail!(
                    "security.otp.gated_actions[{i}] contains invalid characters: {normalized}"
                );
            }
        }
        DomainMatcher::new(
            &self.security.otp.gated_domains,
            &self.security.otp.gated_domain_categories,
        )
        .with_context(
            || "Invalid security.otp.gated_domains or security.otp.gated_domain_categories",
        )?;
        if self.security.estop.state_file.trim().is_empty() {
            anyhow::bail!("security.estop.state_file must not be empty");
        }

        // Scheduler
        if self.scheduler.max_concurrent == 0 {
            anyhow::bail!("scheduler.max_concurrent must be greater than 0");
        }
        if self.scheduler.max_tasks == 0 {
            anyhow::bail!("scheduler.max_tasks must be greater than 0");
        }

        // Model routes
        for (i, route) in self.model_routes.iter().enumerate() {
            if route.hint.trim().is_empty() {
                anyhow::bail!("model_routes[{i}].hint must not be empty");
            }
            if route.provider.trim().is_empty() {
                anyhow::bail!("model_routes[{i}].provider must not be empty");
            }
            if route.model.trim().is_empty() {
                anyhow::bail!("model_routes[{i}].model must not be empty");
            }
        }

        // Embedding routes
        for (i, route) in self.embedding_routes.iter().enumerate() {
            if route.hint.trim().is_empty() {
                anyhow::bail!("embedding_routes[{i}].hint must not be empty");
            }
            if route.provider.trim().is_empty() {
                anyhow::bail!("embedding_routes[{i}].provider must not be empty");
            }
            if route.model.trim().is_empty() {
                anyhow::bail!("embedding_routes[{i}].model must not be empty");
            }
        }

        for (profile_key, profile) in &self.model_providers {
            let profile_name = profile_key.trim();
            if profile_name.is_empty() {
                anyhow::bail!("model_providers contains an empty profile name");
            }

            let has_name = profile
                .name
                .as_deref()
                .map(str::trim)
                .is_some_and(|value| !value.is_empty());
            let has_base_url = profile
                .base_url
                .as_deref()
                .map(str::trim)
                .is_some_and(|value| !value.is_empty());

            if !has_name && !has_base_url {
                anyhow::bail!(
                    "model_providers.{profile_name} must define at least one of `name` or `base_url`"
                );
            }

            if let Some(base_url) = profile.base_url.as_deref().map(str::trim) {
                if !base_url.is_empty() {
                    let parsed = reqwest::Url::parse(base_url).with_context(|| {
                        format!("model_providers.{profile_name}.base_url is not a valid URL")
                    })?;
                    if !matches!(parsed.scheme(), "http" | "https") {
                        anyhow::bail!(
                            "model_providers.{profile_name}.base_url must use http/https"
                        );
                    }
                }
            }

            if let Some(wire_api) = profile.wire_api.as_deref().map(str::trim) {
                if !wire_api.is_empty() && normalize_wire_api(wire_api).is_none() {
                    anyhow::bail!(
                        "model_providers.{profile_name}.wire_api must be one of: responses, chat_completions"
                    );
                }
            }
        }

        // Ollama cloud-routing safety checks
        if self
            .default_provider
            .as_deref()
            .is_some_and(|provider| provider.trim().eq_ignore_ascii_case("ollama"))
            && self
                .default_model
                .as_deref()
                .is_some_and(|model| model.trim().ends_with(":cloud"))
        {
            if is_local_ollama_endpoint(self.api_url.as_deref()) {
                anyhow::bail!(
                    "default_model uses ':cloud' with provider 'ollama', but api_url is local or unset. Set api_url to a remote Ollama endpoint (for example https://ollama.com)."
                );
            }

            if !has_ollama_cloud_credential(self.api_key.as_deref()) {
                anyhow::bail!(
                    "default_model uses ':cloud' with provider 'ollama', but no API key is configured. Set api_key or OLLAMA_API_KEY."
                );
            }
        }

        // Microsoft 365
        if self.microsoft365.enabled {
            let tenant = self
                .microsoft365
                .tenant_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty());
            if tenant.is_none() {
                anyhow::bail!(
                    "microsoft365.tenant_id must not be empty when microsoft365 is enabled"
                );
            }
            let client = self
                .microsoft365
                .client_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty());
            if client.is_none() {
                anyhow::bail!(
                    "microsoft365.client_id must not be empty when microsoft365 is enabled"
                );
            }
            let flow = self.microsoft365.auth_flow.trim();
            if flow != "client_credentials" && flow != "device_code" {
                anyhow::bail!(
                    "microsoft365.auth_flow must be 'client_credentials' or 'device_code'"
                );
            }
            if flow == "client_credentials"
                && self
                    .microsoft365
                    .client_secret
                    .as_deref()
                    .map_or(true, |s| s.trim().is_empty())
            {
                anyhow::bail!(
                    "microsoft365.client_secret must not be empty when auth_flow is 'client_credentials'"
                );
            }
        }

        // Microsoft 365
        if self.microsoft365.enabled {
            let tenant = self
                .microsoft365
                .tenant_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty());
            if tenant.is_none() {
                anyhow::bail!(
                    "microsoft365.tenant_id must not be empty when microsoft365 is enabled"
                );
            }
            let client = self
                .microsoft365
                .client_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty());
            if client.is_none() {
                anyhow::bail!(
                    "microsoft365.client_id must not be empty when microsoft365 is enabled"
                );
            }
            let flow = self.microsoft365.auth_flow.trim();
            if flow != "client_credentials" && flow != "device_code" {
                anyhow::bail!("microsoft365.auth_flow must be client_credentials or device_code");
            }
            if flow == "client_credentials"
                && self
                    .microsoft365
                    .client_secret
                    .as_deref()
                    .map_or(true, |s| s.trim().is_empty())
            {
                anyhow::bail!(
                    "microsoft365.client_secret must not be empty when auth_flow is client_credentials"
                );
            }
        }

        // MCP
        if self.mcp.enabled {
            validate_mcp_config(&self.mcp)?;
        }

        // Knowledge graph
        if self.knowledge.enabled {
            if self.knowledge.max_nodes == 0 {
                anyhow::bail!("knowledge.max_nodes must be greater than 0");
            }
            if self.knowledge.db_path.trim().is_empty() {
                anyhow::bail!("knowledge.db_path must not be empty");
            }
        }

        // Google Workspace allowed_services validation
        let mut seen_gws_services = std::collections::HashSet::new();
        for (i, service) in self.google_workspace.allowed_services.iter().enumerate() {
            let normalized = service.trim();
            if normalized.is_empty() {
                anyhow::bail!("google_workspace.allowed_services[{i}] must not be empty");
            }
            if !normalized
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
            {
                anyhow::bail!(
                    "google_workspace.allowed_services[{i}] contains invalid characters: {normalized}"
                );
            }
            if !seen_gws_services.insert(normalized.to_string()) {
                anyhow::bail!(
                    "google_workspace.allowed_services contains duplicate entry: {normalized}"
                );
            }
        }

        // Build the effective allowed-services set for cross-validation.
        // When the operator leaves allowed_services empty the tool falls back to
        // DEFAULT_GWS_SERVICES; use the same constant here so validation is
        // consistent in both cases.
        let effective_services: std::collections::HashSet<&str> =
            if self.google_workspace.allowed_services.is_empty() {
                DEFAULT_GWS_SERVICES.iter().copied().collect()
            } else {
                self.google_workspace
                    .allowed_services
                    .iter()
                    .map(|s| s.trim())
                    .collect()
            };

        let mut seen_gws_operations = std::collections::HashSet::new();
        for (i, operation) in self.google_workspace.allowed_operations.iter().enumerate() {
            let service = operation.service.trim();
            let resource = operation.resource.trim();

            if service.is_empty() {
                anyhow::bail!("google_workspace.allowed_operations[{i}].service must not be empty");
            }
            if resource.is_empty() {
                anyhow::bail!(
                    "google_workspace.allowed_operations[{i}].resource must not be empty"
                );
            }

            if !effective_services.contains(service) {
                anyhow::bail!(
                    "google_workspace.allowed_operations[{i}].service '{service}' is not in the \
                     effective allowed_services; this entry can never match at runtime"
                );
            }
            if !service
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
            {
                anyhow::bail!(
                    "google_workspace.allowed_operations[{i}].service contains invalid characters: {service}"
                );
            }
            if !resource
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
            {
                anyhow::bail!(
                    "google_workspace.allowed_operations[{i}].resource contains invalid characters: {resource}"
                );
            }

            if let Some(ref sub_resource) = operation.sub_resource {
                let sub = sub_resource.trim();
                if sub.is_empty() {
                    anyhow::bail!(
                        "google_workspace.allowed_operations[{i}].sub_resource must not be empty when present"
                    );
                }
                if !sub
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
                {
                    anyhow::bail!(
                        "google_workspace.allowed_operations[{i}].sub_resource contains invalid characters: {sub}"
                    );
                }
            }

            if operation.methods.is_empty() {
                anyhow::bail!("google_workspace.allowed_operations[{i}].methods must not be empty");
            }

            let mut seen_methods = std::collections::HashSet::new();
            for (j, method) in operation.methods.iter().enumerate() {
                let normalized = method.trim();
                if normalized.is_empty() {
                    anyhow::bail!(
                        "google_workspace.allowed_operations[{i}].methods[{j}] must not be empty"
                    );
                }
                if !normalized
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
                {
                    anyhow::bail!(
                        "google_workspace.allowed_operations[{i}].methods[{j}] contains invalid characters: {normalized}"
                    );
                }
                if !seen_methods.insert(normalized.to_string()) {
                    anyhow::bail!(
                        "google_workspace.allowed_operations[{i}].methods contains duplicate entry: {normalized}"
                    );
                }
            }

            let sub_key = operation
                .sub_resource
                .as_deref()
                .map(str::trim)
                .unwrap_or("");
            let operation_key = format!("{service}:{resource}:{sub_key}");
            if !seen_gws_operations.insert(operation_key.clone()) {
                anyhow::bail!(
                    "google_workspace.allowed_operations contains duplicate service/resource/sub_resource entry: {operation_key}"
                );
            }
        }

        // Project intelligence
        if self.project_intel.enabled {
            let lang = &self.project_intel.default_language;
            if !["en", "de", "fr", "it"].contains(&lang.as_str()) {
                anyhow::bail!(
                    "project_intel.default_language must be one of: en, de, fr, it (got '{lang}')"
                );
            }
            let sens = &self.project_intel.risk_sensitivity;
            if !["low", "medium", "high"].contains(&sens.as_str()) {
                anyhow::bail!(
                    "project_intel.risk_sensitivity must be one of: low, medium, high (got '{sens}')"
                );
            }
            if let Some(ref tpl_dir) = self.project_intel.templates_dir {
                let path = std::path::Path::new(tpl_dir);
                if !path.exists() {
                    anyhow::bail!("project_intel.templates_dir path does not exist: {tpl_dir}");
                }
            }
        }

        // Proxy (delegate to existing validation)
        self.proxy.validate()?;
        self.cloud_ops.validate()?;

        // Notion
        if self.notion.enabled {
            if self.notion.database_id.trim().is_empty() {
                anyhow::bail!("notion.database_id must not be empty when notion.enabled = true");
            }
            if self.notion.poll_interval_secs == 0 {
                anyhow::bail!("notion.poll_interval_secs must be greater than 0");
            }
            if self.notion.max_concurrent == 0 {
                anyhow::bail!("notion.max_concurrent must be greater than 0");
            }
            if self.notion.status_property.trim().is_empty() {
                anyhow::bail!("notion.status_property must not be empty");
            }
            if self.notion.input_property.trim().is_empty() {
                anyhow::bail!("notion.input_property must not be empty");
            }
            if self.notion.result_property.trim().is_empty() {
                anyhow::bail!("notion.result_property must not be empty");
            }
        }

        // Pinggy tunnel region — validate allowed values (case-insensitive, auto-lowercased at runtime).
        if let Some(ref pinggy) = self.tunnel.pinggy {
            if let Some(ref region) = pinggy.region {
                let r = region.trim().to_ascii_lowercase();
                if !r.is_empty() && !matches!(r.as_str(), "us" | "eu" | "ap" | "br" | "au") {
                    anyhow::bail!(
                        "tunnel.pinggy.region must be one of: us, eu, ap, br, au (or omitted for auto)"
                    );
                }
            }
        }

        // Jira
        if self.jira.enabled {
            if self.jira.base_url.trim().is_empty() {
                anyhow::bail!("jira.base_url must not be empty when jira.enabled = true");
            }
            if self.jira.email.trim().is_empty() {
                anyhow::bail!("jira.email must not be empty when jira.enabled = true");
            }
            if self.jira.api_token.trim().is_empty()
                && std::env::var("JIRA_API_TOKEN")
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
            {
                anyhow::bail!(
                    "jira.api_token must be set (or JIRA_API_TOKEN env var) when jira.enabled = true"
                );
            }
            let valid_actions = ["get_ticket", "search_tickets", "comment_ticket"];
            for action in &self.jira.allowed_actions {
                if !valid_actions.contains(&action.as_str()) {
                    anyhow::bail!(
                        "jira.allowed_actions contains unknown action: '{}'. \
                         Valid: get_ticket, search_tickets, comment_ticket",
                        action
                    );
                }
            }
        }

        // Nevis IAM — delegate to NevisConfig::validate() for field-level checks
        if let Err(msg) = self.security.nevis.validate() {
            anyhow::bail!("security.nevis: {msg}");
        }

        // Delegate agent timeouts
        const MAX_DELEGATE_TIMEOUT_SECS: u64 = 3600;
        for (name, agent) in &self.agents {
            if let Some(timeout) = agent.timeout_secs {
                if timeout == 0 {
                    anyhow::bail!("agents.{name}.timeout_secs must be greater than 0");
                }
                if timeout > MAX_DELEGATE_TIMEOUT_SECS {
                    anyhow::bail!(
                        "agents.{name}.timeout_secs exceeds max {MAX_DELEGATE_TIMEOUT_SECS}"
                    );
                }
            }
            if let Some(timeout) = agent.agentic_timeout_secs {
                if timeout == 0 {
                    anyhow::bail!("agents.{name}.agentic_timeout_secs must be greater than 0");
                }
                if timeout > MAX_DELEGATE_TIMEOUT_SECS {
                    anyhow::bail!(
                        "agents.{name}.agentic_timeout_secs exceeds max {MAX_DELEGATE_TIMEOUT_SECS}"
                    );
                }
            }
        }

        // Transcription
        {
            let dp = self.transcription.default_provider.trim();
            match dp {
                "groq" | "openai" | "deepgram" | "assemblyai" | "google" | "local_whisper" => {}
                other => {
                    anyhow::bail!(
                        "transcription.default_provider must be one of: groq, openai, deepgram, assemblyai, google, local_whisper (got '{other}')"
                    );
                }
            }
        }

        // Delegate tool global defaults
        if self.delegate.timeout_secs == 0 {
            anyhow::bail!("delegate.timeout_secs must be greater than 0");
        }
        if self.delegate.agentic_timeout_secs == 0 {
            anyhow::bail!("delegate.agentic_timeout_secs must be greater than 0");
        }

        // Per-agent delegate timeout overrides
        for (name, agent) in &self.agents {
            if let Some(t) = agent.timeout_secs {
                if t == 0 {
                    anyhow::bail!("agents.{name}.timeout_secs must be greater than 0");
                }
            }
            if let Some(t) = agent.agentic_timeout_secs {
                if t == 0 {
                    anyhow::bail!("agents.{name}.agentic_timeout_secs must be greater than 0");
                }
            }
        }

        Ok(())
    }

    /// Apply environment variable overrides to config
    pub fn apply_env_overrides(&mut self) {
        // API Key: ZEROCLAW_API_KEY or API_KEY (generic)
        if let Ok(key) = std::env::var("ZEROCLAW_API_KEY").or_else(|_| std::env::var("API_KEY")) {
            if !key.is_empty() {
                self.api_key = Some(key);
            }
        }
        // API Key: GLM_API_KEY overrides when provider is a GLM/Zhipu variant.
        if self.default_provider.as_deref().is_some_and(is_glm_alias) {
            if let Ok(key) = std::env::var("GLM_API_KEY") {
                if !key.is_empty() {
                    self.api_key = Some(key);
                }
            }
        }

        // API Key: ZAI_API_KEY overrides when provider is a Z.AI variant.
        if self.default_provider.as_deref().is_some_and(is_zai_alias) {
            if let Ok(key) = std::env::var("ZAI_API_KEY") {
                if !key.is_empty() {
                    self.api_key = Some(key);
                }
            }
        }

        // Provider override precedence:
        // 1) ZEROCLAW_PROVIDER always wins when set.
        // 2) ZEROCLAW_MODEL_PROVIDER/MODEL_PROVIDER (Codex app-server style).
        // 3) Legacy PROVIDER is honored only when config still uses default provider.
        if let Ok(provider) = std::env::var("ZEROCLAW_PROVIDER") {
            if !provider.is_empty() {
                self.default_provider = Some(provider);
            }
        } else if let Ok(provider) =
            std::env::var("ZEROCLAW_MODEL_PROVIDER").or_else(|_| std::env::var("MODEL_PROVIDER"))
        {
            if !provider.is_empty() {
                self.default_provider = Some(provider);
            }
        } else if let Ok(provider) = std::env::var("PROVIDER") {
            let should_apply_legacy_provider =
                self.default_provider.as_deref().map_or(true, |configured| {
                    configured.trim().eq_ignore_ascii_case("openrouter")
                });
            if should_apply_legacy_provider && !provider.is_empty() {
                self.default_provider = Some(provider);
            }
        }

        // Model: ZEROCLAW_MODEL or MODEL
        if let Ok(model) = std::env::var("ZEROCLAW_MODEL").or_else(|_| std::env::var("MODEL")) {
            if !model.is_empty() {
                self.default_model = Some(model);
            }
        }

        // Provider HTTP timeout: ZEROCLAW_PROVIDER_TIMEOUT_SECS
        if let Ok(timeout_secs) = std::env::var("ZEROCLAW_PROVIDER_TIMEOUT_SECS") {
            if let Ok(timeout_secs) = timeout_secs.parse::<u64>() {
                if timeout_secs > 0 {
                    self.provider_timeout_secs = timeout_secs;
                }
            }
        }

        // Extra provider headers: ZEROCLAW_EXTRA_HEADERS
        // Format: "Key:Value,Key2:Value2"
        // Env var headers override config file headers with the same name.
        if let Ok(raw) = std::env::var("ZEROCLAW_EXTRA_HEADERS") {
            for header in parse_extra_headers_env(&raw) {
                self.extra_headers.insert(header.0, header.1);
            }
        }

        // Apply named provider profile remapping (Codex app-server compatibility).
        self.apply_named_model_provider_profile();

        // Workspace directory: ZEROCLAW_WORKSPACE
        if let Ok(workspace) = std::env::var("ZEROCLAW_WORKSPACE") {
            if !workspace.is_empty() {
                let expanded = expand_tilde_path(&workspace);
                let (_, workspace_dir) = resolve_config_dir_for_workspace(&expanded);
                self.workspace_dir = workspace_dir;
            }
        }

        // Open-skills opt-in flag: ZEROCLAW_OPEN_SKILLS_ENABLED
        if let Ok(flag) = std::env::var("ZEROCLAW_OPEN_SKILLS_ENABLED") {
            if !flag.trim().is_empty() {
                match flag.trim().to_ascii_lowercase().as_str() {
                    "1" | "true" | "yes" | "on" => self.skills.open_skills_enabled = true,
                    "0" | "false" | "no" | "off" => self.skills.open_skills_enabled = false,
                    _ => tracing::warn!(
                        "Ignoring invalid ZEROCLAW_OPEN_SKILLS_ENABLED (valid: 1|0|true|false|yes|no|on|off)"
                    ),
                }
            }
        }

        // Open-skills directory override: ZEROCLAW_OPEN_SKILLS_DIR
        if let Ok(path) = std::env::var("ZEROCLAW_OPEN_SKILLS_DIR") {
            let trimmed = path.trim();
            if !trimmed.is_empty() {
                self.skills.open_skills_dir = Some(trimmed.to_string());
            }
        }

        // Skills script-file audit override: ZEROCLAW_SKILLS_ALLOW_SCRIPTS
        if let Ok(flag) = std::env::var("ZEROCLAW_SKILLS_ALLOW_SCRIPTS") {
            if !flag.trim().is_empty() {
                match flag.trim().to_ascii_lowercase().as_str() {
                    "1" | "true" | "yes" | "on" => self.skills.allow_scripts = true,
                    "0" | "false" | "no" | "off" => self.skills.allow_scripts = false,
                    _ => tracing::warn!(
                        "Ignoring invalid ZEROCLAW_SKILLS_ALLOW_SCRIPTS (valid: 1|0|true|false|yes|no|on|off)"
                    ),
                }
            }
        }

        // Skills prompt mode override: ZEROCLAW_SKILLS_PROMPT_MODE
        if let Ok(mode) = std::env::var("ZEROCLAW_SKILLS_PROMPT_MODE") {
            if !mode.trim().is_empty() {
                if let Some(parsed) = parse_skills_prompt_injection_mode(&mode) {
                    self.skills.prompt_injection_mode = parsed;
                } else {
                    tracing::warn!(
                        "Ignoring invalid ZEROCLAW_SKILLS_PROMPT_MODE (valid: full|compact)"
                    );
                }
            }
        }

        // Gateway port: ZEROCLAW_GATEWAY_PORT or PORT
        if let Ok(port_str) =
            std::env::var("ZEROCLAW_GATEWAY_PORT").or_else(|_| std::env::var("PORT"))
        {
            if let Ok(port) = port_str.parse::<u16>() {
                self.gateway.port = port;
            }
        }

        // Gateway host: ZEROCLAW_GATEWAY_HOST or HOST
        if let Ok(host) = std::env::var("ZEROCLAW_GATEWAY_HOST").or_else(|_| std::env::var("HOST"))
        {
            if !host.is_empty() {
                self.gateway.host = host;
            }
        }

        // Allow public bind: ZEROCLAW_ALLOW_PUBLIC_BIND
        if let Ok(val) = std::env::var("ZEROCLAW_ALLOW_PUBLIC_BIND") {
            self.gateway.allow_public_bind = val == "1" || val.eq_ignore_ascii_case("true");
        }

        // Require pairing: ZEROCLAW_REQUIRE_PAIRING
        if let Ok(val) = std::env::var("ZEROCLAW_REQUIRE_PAIRING") {
            self.gateway.require_pairing = val == "1" || val.eq_ignore_ascii_case("true");
        }

        // Temperature: ZEROCLAW_TEMPERATURE
        if let Ok(temp_str) = std::env::var("ZEROCLAW_TEMPERATURE") {
            match temp_str.parse::<f64>() {
                Ok(temp) if TEMPERATURE_RANGE.contains(&temp) => {
                    self.default_temperature = temp;
                }
                Ok(temp) => {
                    tracing::warn!(
                        "Ignoring ZEROCLAW_TEMPERATURE={temp}: \
                         value out of range (expected {}..={})",
                        TEMPERATURE_RANGE.start(),
                        TEMPERATURE_RANGE.end()
                    );
                }
                Err(_) => {
                    tracing::warn!(
                        "Ignoring ZEROCLAW_TEMPERATURE={temp_str:?}: not a valid number"
                    );
                }
            }
        }

        // Reasoning override: ZEROCLAW_REASONING_ENABLED or REASONING_ENABLED
        if let Ok(flag) = std::env::var("ZEROCLAW_REASONING_ENABLED")
            .or_else(|_| std::env::var("REASONING_ENABLED"))
        {
            let normalized = flag.trim().to_ascii_lowercase();
            match normalized.as_str() {
                "1" | "true" | "yes" | "on" => self.runtime.reasoning_enabled = Some(true),
                "0" | "false" | "no" | "off" => self.runtime.reasoning_enabled = Some(false),
                _ => {}
            }
        }

        if let Ok(raw) = std::env::var("ZEROCLAW_REASONING_EFFORT")
            .or_else(|_| std::env::var("REASONING_EFFORT"))
            .or_else(|_| std::env::var("ZEROCLAW_CODEX_REASONING_EFFORT"))
        {
            match normalize_reasoning_effort(&raw) {
                Ok(effort) => self.runtime.reasoning_effort = Some(effort),
                Err(message) => tracing::warn!("Ignoring reasoning effort env override: {message}"),
            }
        }

        // Web search enabled: ZEROCLAW_WEB_SEARCH_ENABLED or WEB_SEARCH_ENABLED
        if let Ok(enabled) = std::env::var("ZEROCLAW_WEB_SEARCH_ENABLED")
            .or_else(|_| std::env::var("WEB_SEARCH_ENABLED"))
        {
            self.web_search.enabled = enabled == "1" || enabled.eq_ignore_ascii_case("true");
        }

        // Web search provider: ZEROCLAW_WEB_SEARCH_PROVIDER or WEB_SEARCH_PROVIDER
        if let Ok(provider) = std::env::var("ZEROCLAW_WEB_SEARCH_PROVIDER")
            .or_else(|_| std::env::var("WEB_SEARCH_PROVIDER"))
        {
            let provider = provider.trim();
            if !provider.is_empty() {
                self.web_search.provider = provider.to_string();
            }
        }

        // Brave API key: ZEROCLAW_BRAVE_API_KEY or BRAVE_API_KEY
        if let Ok(api_key) =
            std::env::var("ZEROCLAW_BRAVE_API_KEY").or_else(|_| std::env::var("BRAVE_API_KEY"))
        {
            let api_key = api_key.trim();
            if !api_key.is_empty() {
                self.web_search.brave_api_key = Some(api_key.to_string());
            }
        }

        // SearXNG instance URL: ZEROCLAW_SEARXNG_INSTANCE_URL or SEARXNG_INSTANCE_URL
        if let Ok(instance_url) = std::env::var("ZEROCLAW_SEARXNG_INSTANCE_URL")
            .or_else(|_| std::env::var("SEARXNG_INSTANCE_URL"))
        {
            let instance_url = instance_url.trim();
            if !instance_url.is_empty() {
                self.web_search.searxng_instance_url = Some(instance_url.to_string());
            }
        }

        // Web search max results: ZEROCLAW_WEB_SEARCH_MAX_RESULTS or WEB_SEARCH_MAX_RESULTS
        if let Ok(max_results) = std::env::var("ZEROCLAW_WEB_SEARCH_MAX_RESULTS")
            .or_else(|_| std::env::var("WEB_SEARCH_MAX_RESULTS"))
        {
            if let Ok(max_results) = max_results.parse::<usize>() {
                if (1..=10).contains(&max_results) {
                    self.web_search.max_results = max_results;
                }
            }
        }

        // Web search timeout: ZEROCLAW_WEB_SEARCH_TIMEOUT_SECS or WEB_SEARCH_TIMEOUT_SECS
        if let Ok(timeout_secs) = std::env::var("ZEROCLAW_WEB_SEARCH_TIMEOUT_SECS")
            .or_else(|_| std::env::var("WEB_SEARCH_TIMEOUT_SECS"))
        {
            if let Ok(timeout_secs) = timeout_secs.parse::<u64>() {
                if timeout_secs > 0 {
                    self.web_search.timeout_secs = timeout_secs;
                }
            }
        }

        // Storage provider key (optional backend override): ZEROCLAW_STORAGE_PROVIDER
        if let Ok(provider) = std::env::var("ZEROCLAW_STORAGE_PROVIDER") {
            let provider = provider.trim();
            if !provider.is_empty() {
                self.storage.provider.config.provider = provider.to_string();
            }
        }

        // Storage connection URL (for remote backends): ZEROCLAW_STORAGE_DB_URL
        if let Ok(db_url) = std::env::var("ZEROCLAW_STORAGE_DB_URL") {
            let db_url = db_url.trim();
            if !db_url.is_empty() {
                self.storage.provider.config.db_url = Some(db_url.to_string());
            }
        }

        // Storage connect timeout: ZEROCLAW_STORAGE_CONNECT_TIMEOUT_SECS
        if let Ok(timeout_secs) = std::env::var("ZEROCLAW_STORAGE_CONNECT_TIMEOUT_SECS") {
            if let Ok(timeout_secs) = timeout_secs.parse::<u64>() {
                if timeout_secs > 0 {
                    self.storage.provider.config.connect_timeout_secs = Some(timeout_secs);
                }
            }
        }
        // Proxy enabled flag: ZEROCLAW_PROXY_ENABLED
        let explicit_proxy_enabled = std::env::var("ZEROCLAW_PROXY_ENABLED")
            .ok()
            .as_deref()
            .and_then(parse_proxy_enabled);
        if let Some(enabled) = explicit_proxy_enabled {
            self.proxy.enabled = enabled;
        }

        // Proxy URLs: ZEROCLAW_* wins, then generic *PROXY vars.
        let mut proxy_url_overridden = false;
        if let Ok(proxy_url) =
            std::env::var("ZEROCLAW_HTTP_PROXY").or_else(|_| std::env::var("HTTP_PROXY"))
        {
            self.proxy.http_proxy = normalize_proxy_url_option(Some(&proxy_url));
            proxy_url_overridden = true;
        }
        if let Ok(proxy_url) =
            std::env::var("ZEROCLAW_HTTPS_PROXY").or_else(|_| std::env::var("HTTPS_PROXY"))
        {
            self.proxy.https_proxy = normalize_proxy_url_option(Some(&proxy_url));
            proxy_url_overridden = true;
        }
        if let Ok(proxy_url) =
            std::env::var("ZEROCLAW_ALL_PROXY").or_else(|_| std::env::var("ALL_PROXY"))
        {
            self.proxy.all_proxy = normalize_proxy_url_option(Some(&proxy_url));
            proxy_url_overridden = true;
        }
        if let Ok(no_proxy) =
            std::env::var("ZEROCLAW_NO_PROXY").or_else(|_| std::env::var("NO_PROXY"))
        {
            self.proxy.no_proxy = normalize_no_proxy_list(vec![no_proxy]);
        }

        if explicit_proxy_enabled.is_none()
            && proxy_url_overridden
            && self.proxy.has_any_proxy_url()
        {
            self.proxy.enabled = true;
        }

        // Proxy scope and service selectors.
        if let Ok(scope_raw) = std::env::var("ZEROCLAW_PROXY_SCOPE") {
            if let Some(scope) = parse_proxy_scope(&scope_raw) {
                self.proxy.scope = scope;
            } else {
                tracing::warn!(
                    scope = %scope_raw,
                    "Ignoring invalid ZEROCLAW_PROXY_SCOPE (valid: environment|zeroclaw|services)"
                );
            }
        }

        if let Ok(services_raw) = std::env::var("ZEROCLAW_PROXY_SERVICES") {
            self.proxy.services = normalize_service_list(vec![services_raw]);
        }

        if let Err(error) = self.proxy.validate() {
            tracing::warn!("Invalid proxy configuration ignored: {error}");
            self.proxy.enabled = false;
        }

        if self.proxy.enabled && self.proxy.scope == ProxyScope::Environment {
            self.proxy.apply_to_process_env();
        }

        set_runtime_proxy_config(self.proxy.clone());

        if self.conversational_ai.enabled {
            tracing::warn!(
                "conversational_ai.enabled = true but conversational AI features are not yet \
                 implemented; this section is reserved for future use and will be ignored"
            );
        }
    }

    async fn resolve_config_path_for_save(&self) -> Result<PathBuf> {
        if self
            .config_path
            .parent()
            .is_some_and(|parent| !parent.as_os_str().is_empty())
        {
            return Ok(self.config_path.clone());
        }

        let (default_zeroclaw_dir, default_workspace_dir) = default_config_and_workspace_dirs()?;
        let (zeroclaw_dir, _workspace_dir, source) =
            resolve_runtime_config_dirs(&default_zeroclaw_dir, &default_workspace_dir).await?;
        let file_name = self
            .config_path
            .file_name()
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| std::ffi::OsStr::new("config.toml"));
        let resolved = zeroclaw_dir.join(file_name);
        tracing::warn!(
            path = %self.config_path.display(),
            resolved = %resolved.display(),
            source = source.as_str(),
            "Config path missing parent directory; resolving from runtime environment"
        );
        Ok(resolved)
    }

    pub async fn save(&self) -> Result<()> {
        // Encrypt secrets before serialization
        let mut config_to_save = self.clone();
        let config_path = self.resolve_config_path_for_save().await?;
        let zeroclaw_dir = config_path
            .parent()
            .context("Config path must have a parent directory")?;
        let store = crate::security::SecretStore::new(zeroclaw_dir, self.secrets.encrypt);

        encrypt_optional_secret(&store, &mut config_to_save.api_key, "config.api_key")?;
        encrypt_optional_secret(
            &store,
            &mut config_to_save.composio.api_key,
            "config.composio.api_key",
        )?;
        if let Some(ref mut pinggy) = config_to_save.tunnel.pinggy {
            encrypt_optional_secret(&store, &mut pinggy.token, "config.tunnel.pinggy.token")?;
        }
        encrypt_optional_secret(
            &store,
            &mut config_to_save.microsoft365.client_secret,
            "config.microsoft365.client_secret",
        )?;

        encrypt_optional_secret(
            &store,
            &mut config_to_save.browser.computer_use.api_key,
            "config.browser.computer_use.api_key",
        )?;

        encrypt_optional_secret(
            &store,
            &mut config_to_save.web_search.brave_api_key,
            "config.web_search.brave_api_key",
        )?;

        encrypt_optional_secret(
            &store,
            &mut config_to_save.storage.provider.config.db_url,
            "config.storage.provider.config.db_url",
        )?;

        for agent in config_to_save.agents.values_mut() {
            encrypt_optional_secret(&store, &mut agent.api_key, "config.agents.*.api_key")?;
        }

        // Encrypt TTS provider API keys
        if let Some(ref mut openai) = config_to_save.tts.openai {
            encrypt_optional_secret(&store, &mut openai.api_key, "config.tts.openai.api_key")?;
        }
        if let Some(ref mut elevenlabs) = config_to_save.tts.elevenlabs {
            encrypt_optional_secret(
                &store,
                &mut elevenlabs.api_key,
                "config.tts.elevenlabs.api_key",
            )?;
        }
        if let Some(ref mut google) = config_to_save.tts.google {
            encrypt_optional_secret(&store, &mut google.api_key, "config.tts.google.api_key")?;
        }

        // Encrypt nested STT provider API keys
        encrypt_optional_secret(
            &store,
            &mut config_to_save.transcription.api_key,
            "config.transcription.api_key",
        )?;
        if let Some(ref mut openai) = config_to_save.transcription.openai {
            encrypt_optional_secret(
                &store,
                &mut openai.api_key,
                "config.transcription.openai.api_key",
            )?;
        }
        if let Some(ref mut deepgram) = config_to_save.transcription.deepgram {
            encrypt_optional_secret(
                &store,
                &mut deepgram.api_key,
                "config.transcription.deepgram.api_key",
            )?;
        }
        if let Some(ref mut assemblyai) = config_to_save.transcription.assemblyai {
            encrypt_optional_secret(
                &store,
                &mut assemblyai.api_key,
                "config.transcription.assemblyai.api_key",
            )?;
        }
        if let Some(ref mut google) = config_to_save.transcription.google {
            encrypt_optional_secret(
                &store,
                &mut google.api_key,
                "config.transcription.google.api_key",
            )?;
        }
        if let Some(ref mut local) = config_to_save.transcription.local_whisper {
            encrypt_optional_secret(
                &store,
                &mut local.bearer_token,
                "config.transcription.local_whisper.bearer_token",
            )?;
        }

        #[cfg(feature = "channel-nostr")]
        if let Some(ref mut ns) = config_to_save.channels_config.nostr {
            encrypt_secret(
                &store,
                &mut ns.private_key,
                "config.channels_config.nostr.private_key",
            )?;
        }
        if let Some(ref mut fs) = config_to_save.channels_config.feishu {
            encrypt_secret(
                &store,
                &mut fs.app_secret,
                "config.channels_config.feishu.app_secret",
            )?;
            encrypt_optional_secret(
                &store,
                &mut fs.encrypt_key,
                "config.channels_config.feishu.encrypt_key",
            )?;
            encrypt_optional_secret(
                &store,
                &mut fs.verification_token,
                "config.channels_config.feishu.verification_token",
            )?;
        }

        // Encrypt channel secrets
        if let Some(ref mut tg) = config_to_save.channels_config.telegram {
            encrypt_secret(
                &store,
                &mut tg.bot_token,
                "config.channels_config.telegram.bot_token",
            )?;
        }
        if let Some(ref mut dc) = config_to_save.channels_config.discord {
            encrypt_secret(
                &store,
                &mut dc.bot_token,
                "config.channels_config.discord.bot_token",
            )?;
        }
        if let Some(ref mut sl) = config_to_save.channels_config.slack {
            encrypt_secret(
                &store,
                &mut sl.bot_token,
                "config.channels_config.slack.bot_token",
            )?;
            encrypt_optional_secret(
                &store,
                &mut sl.app_token,
                "config.channels_config.slack.app_token",
            )?;
        }
        if let Some(ref mut mm) = config_to_save.channels_config.mattermost {
            encrypt_secret(
                &store,
                &mut mm.bot_token,
                "config.channels_config.mattermost.bot_token",
            )?;
        }
        if let Some(ref mut mx) = config_to_save.channels_config.matrix {
            encrypt_secret(
                &store,
                &mut mx.access_token,
                "config.channels_config.matrix.access_token",
            )?;
            encrypt_optional_secret(
                &store,
                &mut mx.recovery_key,
                "config.channels_config.matrix.recovery_key",
            )?;
        }
        if let Some(ref mut wa) = config_to_save.channels_config.whatsapp {
            encrypt_optional_secret(
                &store,
                &mut wa.access_token,
                "config.channels_config.whatsapp.access_token",
            )?;
            encrypt_optional_secret(
                &store,
                &mut wa.app_secret,
                "config.channels_config.whatsapp.app_secret",
            )?;
            encrypt_optional_secret(
                &store,
                &mut wa.verify_token,
                "config.channels_config.whatsapp.verify_token",
            )?;
        }
        if let Some(ref mut lq) = config_to_save.channels_config.linq {
            encrypt_secret(
                &store,
                &mut lq.api_token,
                "config.channels_config.linq.api_token",
            )?;
            encrypt_optional_secret(
                &store,
                &mut lq.signing_secret,
                "config.channels_config.linq.signing_secret",
            )?;
        }
        if let Some(ref mut wt) = config_to_save.channels_config.wati {
            encrypt_secret(
                &store,
                &mut wt.api_token,
                "config.channels_config.wati.api_token",
            )?;
        }
        if let Some(ref mut nc) = config_to_save.channels_config.nextcloud_talk {
            encrypt_secret(
                &store,
                &mut nc.app_token,
                "config.channels_config.nextcloud_talk.app_token",
            )?;
            encrypt_optional_secret(
                &store,
                &mut nc.webhook_secret,
                "config.channels_config.nextcloud_talk.webhook_secret",
            )?;
        }
        if let Some(ref mut em) = config_to_save.channels_config.email {
            encrypt_secret(
                &store,
                &mut em.password,
                "config.channels_config.email.password",
            )?;
        }
        if let Some(ref mut gp) = config_to_save.channels_config.gmail_push {
            encrypt_secret(
                &store,
                &mut gp.oauth_token,
                "config.channels_config.gmail_push.oauth_token",
            )?;
        }
        if let Some(ref mut irc) = config_to_save.channels_config.irc {
            encrypt_optional_secret(
                &store,
                &mut irc.server_password,
                "config.channels_config.irc.server_password",
            )?;
            encrypt_optional_secret(
                &store,
                &mut irc.nickserv_password,
                "config.channels_config.irc.nickserv_password",
            )?;
            encrypt_optional_secret(
                &store,
                &mut irc.sasl_password,
                "config.channels_config.irc.sasl_password",
            )?;
        }
        if let Some(ref mut lk) = config_to_save.channels_config.lark {
            encrypt_secret(
                &store,
                &mut lk.app_secret,
                "config.channels_config.lark.app_secret",
            )?;
            encrypt_optional_secret(
                &store,
                &mut lk.encrypt_key,
                "config.channels_config.lark.encrypt_key",
            )?;
            encrypt_optional_secret(
                &store,
                &mut lk.verification_token,
                "config.channels_config.lark.verification_token",
            )?;
        }
        if let Some(ref mut fs) = config_to_save.channels_config.feishu {
            encrypt_secret(
                &store,
                &mut fs.app_secret,
                "config.channels_config.feishu.app_secret",
            )?;
            encrypt_optional_secret(
                &store,
                &mut fs.encrypt_key,
                "config.channels_config.feishu.encrypt_key",
            )?;
            encrypt_optional_secret(
                &store,
                &mut fs.verification_token,
                "config.channels_config.feishu.verification_token",
            )?;
        }
        if let Some(ref mut dt) = config_to_save.channels_config.dingtalk {
            encrypt_secret(
                &store,
                &mut dt.client_secret,
                "config.channels_config.dingtalk.client_secret",
            )?;
        }
        if let Some(ref mut wc) = config_to_save.channels_config.wecom {
            encrypt_secret(
                &store,
                &mut wc.webhook_key,
                "config.channels_config.wecom.webhook_key",
            )?;
        }
        if let Some(ref mut qq) = config_to_save.channels_config.qq {
            encrypt_secret(
                &store,
                &mut qq.app_secret,
                "config.channels_config.qq.app_secret",
            )?;
        }
        if let Some(ref mut wh) = config_to_save.channels_config.webhook {
            encrypt_optional_secret(
                &store,
                &mut wh.secret,
                "config.channels_config.webhook.secret",
            )?;
        }
        if let Some(ref mut ct) = config_to_save.channels_config.clawdtalk {
            encrypt_secret(
                &store,
                &mut ct.api_key,
                "config.channels_config.clawdtalk.api_key",
            )?;
            encrypt_optional_secret(
                &store,
                &mut ct.webhook_secret,
                "config.channels_config.clawdtalk.webhook_secret",
            )?;
        }

        // Encrypt gateway paired tokens
        for token in &mut config_to_save.gateway.paired_tokens {
            encrypt_secret(&store, token, "config.gateway.paired_tokens[]")?;
        }

        // Encrypt Nevis IAM secret
        encrypt_optional_secret(
            &store,
            &mut config_to_save.security.nevis.client_secret,
            "config.security.nevis.client_secret",
        )?;

        // Notion API key (top-level, not in ChannelsConfig)
        if !config_to_save.notion.api_key.is_empty() {
            encrypt_secret(
                &store,
                &mut config_to_save.notion.api_key,
                "config.notion.api_key",
            )?;
        }

        // Jira API token
        if !config_to_save.jira.api_token.is_empty() {
            encrypt_secret(
                &store,
                &mut config_to_save.jira.api_token,
                "config.jira.api_token",
            )?;
        }

        let toml_str =
            toml::to_string_pretty(&config_to_save).context("Failed to serialize config")?;

        let parent_dir = config_path
            .parent()
            .context("Config path must have a parent directory")?;

        fs::create_dir_all(parent_dir).await.with_context(|| {
            format!(
                "Failed to create config directory: {}",
                parent_dir.display()
            )
        })?;

        let file_name = config_path
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or("config.toml");
        let temp_path = parent_dir.join(format!(".{file_name}.tmp-{}", uuid::Uuid::new_v4()));
        let backup_path = parent_dir.join(format!("{file_name}.bak"));

        let mut temp_file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp_path)
            .await
            .with_context(|| {
                format!(
                    "Failed to create temporary config file: {}",
                    temp_path.display()
                )
            })?;
        temp_file
            .write_all(toml_str.as_bytes())
            .await
            .context("Failed to write temporary config contents")?;
        temp_file
            .sync_all()
            .await
            .context("Failed to fsync temporary config file")?;
        drop(temp_file);

        let had_existing_config = config_path.exists();
        if had_existing_config {
            fs::copy(&config_path, &backup_path)
                .await
                .with_context(|| {
                    format!(
                        "Failed to create config backup before atomic replace: {}",
                        backup_path.display()
                    )
                })?;
        }

        if let Err(e) = fs::rename(&temp_path, &config_path).await {
            let _ = fs::remove_file(&temp_path).await;
            if had_existing_config && backup_path.exists() {
                fs::copy(&backup_path, &config_path)
                    .await
                    .context("Failed to restore config backup")?;
            }
            anyhow::bail!("Failed to atomically replace config file: {e}");
        }

        #[cfg(unix)]
        {
            use std::{fs::Permissions, os::unix::fs::PermissionsExt};
            if let Err(err) = fs::set_permissions(&config_path, Permissions::from_mode(0o600)).await
            {
                tracing::warn!(
                    "Failed to harden config permissions to 0600 at {}: {}",
                    config_path.display(),
                    err
                );
            }
        }

        sync_directory(parent_dir).await?;

        if had_existing_config {
            let _ = fs::remove_file(&backup_path).await;
        }

        Ok(())
    }
}

#[allow(clippy::unused_async)] // async needed on unix for tokio File I/O; no-op on other platforms
pub(super) async fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        let dir = File::open(path)
            .await
            .with_context(|| format!("Failed to open directory for fsync: {}", path.display()))?;
        dir.sync_all()
            .await
            .with_context(|| format!("Failed to fsync directory metadata: {}", path.display()))?;
        Ok(())
    }

    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x02000000;
        let dir = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .with_context(|| format!("Failed to open directory for fsync: {}", path.display()))?;
        dir.sync_all()
            .with_context(|| format!("Failed to fsync directory metadata: {}", path.display()))?;
        Ok(())
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Ok(())
    }
}
