//! Workspace registry and lifecycle management.
//!
//! PR1 scope: local filesystem registry + CLI lifecycle commands.
//! Gateway/agent routing integration is intentionally out of scope here.

use anyhow::{bail, Context, Result};
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

const WORKSPACE_METADATA_FILE: &str = "workspace.toml";
const WORKSPACE_CONFIG_FILE: &str = "config.toml";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WorkspaceMetadata {
    id: String,
    display_name: String,
    enabled: bool,
    created_at: String,
    token_hash: String,
}

#[derive(Debug, Clone)]
struct WorkspaceRecord {
    metadata: WorkspaceMetadata,
    path: PathBuf,
}

/// Read-only summary for CLI table output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceSummary {
    pub id: String,
    pub display_name: String,
    pub enabled: bool,
    pub created_at: String,
}

/// Resolved workspace identity for token lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceHandle {
    pub id: String,
    pub display_name: String,
    pub path: PathBuf,
}

/// In-process workspace registry loaded from `<root>/*/workspace.toml`.
#[derive(Debug, Clone)]
pub struct WorkspaceRegistry {
    root: PathBuf,
    workspaces: HashMap<String, WorkspaceRecord>,
    token_index: HashMap<String, String>,
}

impl WorkspaceRegistry {
    /// Load registry state from workspace metadata files under `root`.
    pub fn load(root: &Path) -> Result<Self> {
        fs::create_dir_all(root)
            .with_context(|| format!("failed to create workspace root {}", root.display()))?;

        let mut registry = Self {
            root: root.to_path_buf(),
            workspaces: HashMap::new(),
            token_index: HashMap::new(),
        };

        let mut workspace_dirs = Vec::new();
        for entry in fs::read_dir(root)
            .with_context(|| format!("failed to read workspace root {}", root.display()))?
        {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                workspace_dirs.push(entry.path());
            }
        }
        workspace_dirs.sort();

        for workspace_dir in workspace_dirs {
            let metadata_path = workspace_dir.join(WORKSPACE_METADATA_FILE);
            if !metadata_path.exists() {
                continue;
            }

            let raw = fs::read_to_string(&metadata_path)
                .with_context(|| format!("failed to read {}", metadata_path.display()))?;
            let mut metadata: WorkspaceMetadata = toml::from_str(&raw)
                .with_context(|| format!("failed to parse {}", metadata_path.display()))?;

            let normalized_id = normalize_workspace_id(&metadata.id)?;
            metadata.id = normalized_id.clone();

            if metadata.token_hash.trim().is_empty() {
                bail!(
                    "workspace {} has empty token_hash in {}",
                    metadata.id,
                    metadata_path.display()
                );
            }

            if registry.workspaces.contains_key(&normalized_id) {
                bail!(
                    "duplicate workspace id {normalized_id} in {}",
                    root.display()
                );
            }

            if metadata.enabled {
                if registry
                    .token_index
                    .insert(metadata.token_hash.clone(), normalized_id.clone())
                    .is_some()
                {
                    bail!(
                        "duplicate enabled token hash detected while loading {}",
                        metadata_path.display()
                    );
                }
            }

            registry.workspaces.insert(
                normalized_id,
                WorkspaceRecord {
                    metadata,
                    path: workspace_dir,
                },
            );
        }

        Ok(registry)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn list(&self) -> Vec<WorkspaceSummary> {
        let mut summaries: Vec<_> = self
            .workspaces
            .values()
            .map(|record| WorkspaceSummary {
                id: record.metadata.id.clone(),
                display_name: record.metadata.display_name.clone(),
                enabled: record.metadata.enabled,
                created_at: record.metadata.created_at.clone(),
            })
            .collect();
        summaries.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        summaries
    }

    /// Resolve a bearer token to a workspace identity.
    pub fn resolve(&self, raw_token: &str) -> Option<WorkspaceHandle> {
        let token = raw_token.trim();
        if token.is_empty() {
            return None;
        }
        let token_hash = hash_token(token);
        let workspace_id = self.token_index.get(&token_hash)?;
        let record = self.workspaces.get(workspace_id)?;
        if !record.metadata.enabled {
            return None;
        }
        Some(WorkspaceHandle {
            id: record.metadata.id.clone(),
            display_name: record.metadata.display_name.clone(),
            path: record.path.clone(),
        })
    }

    /// Create a new workspace and return `(workspace_id, plaintext_token)`.
    pub fn create(&mut self, display_name: &str) -> Result<(String, String)> {
        let display_name = display_name.trim();
        if display_name.is_empty() {
            bail!("workspace display name cannot be empty");
        }

        let workspace_id = Uuid::new_v4().to_string();
        let token = generate_token();
        let token_hash = hash_token(&token);
        let created_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
        let workspace_dir = self.root.join(&workspace_id);

        fs::create_dir_all(workspace_dir.join("memory"))
            .with_context(|| format!("failed to create {}", workspace_dir.display()))?;
        fs::create_dir_all(workspace_dir.join("identity"))
            .with_context(|| format!("failed to create {}", workspace_dir.display()))?;
        fs::create_dir_all(workspace_dir.join("channels"))
            .with_context(|| format!("failed to create {}", workspace_dir.display()))?;
        fs::write(
            workspace_dir.join(WORKSPACE_CONFIG_FILE),
            default_workspace_config(),
        )
        .with_context(|| format!("failed to write {}", workspace_dir.display()))?;

        let metadata = WorkspaceMetadata {
            id: workspace_id.clone(),
            display_name: display_name.to_string(),
            enabled: true,
            created_at,
            token_hash: token_hash.clone(),
        };
        write_workspace_metadata(&workspace_dir, &metadata)?;

        self.token_index.insert(token_hash, workspace_id.clone());
        self.workspaces.insert(
            workspace_id.clone(),
            WorkspaceRecord {
                metadata,
                path: workspace_dir,
            },
        );

        Ok((workspace_id, token))
    }

    /// Disable a workspace while preserving its on-disk data.
    pub fn disable(&mut self, workspace_id: &str) -> Result<()> {
        let workspace_id = normalize_workspace_id(workspace_id)?;
        let record = self
            .workspaces
            .get_mut(&workspace_id)
            .with_context(|| format!("workspace {} not found", workspace_id))?;

        if !record.metadata.enabled {
            return Ok(());
        }

        record.metadata.enabled = false;
        write_workspace_metadata(&record.path, &record.metadata)?;
        self.token_index.retain(|_, id| id != &workspace_id);
        Ok(())
    }

    /// Rotate workspace token and return the new plaintext token once.
    pub fn rotate_token(&mut self, workspace_id: &str) -> Result<String> {
        let workspace_id = normalize_workspace_id(workspace_id)?;
        let record = self
            .workspaces
            .get_mut(&workspace_id)
            .with_context(|| format!("workspace {} not found", workspace_id))?;

        let token = generate_token();
        let token_hash = hash_token(&token);
        record.metadata.token_hash = token_hash.clone();
        write_workspace_metadata(&record.path, &record.metadata)?;

        self.token_index.retain(|_, id| id != &workspace_id);
        if record.metadata.enabled {
            self.token_index.insert(token_hash, workspace_id);
        }

        Ok(token)
    }

    /// Delete workspace data recursively.
    pub fn delete(&mut self, workspace_id: &str) -> Result<()> {
        let workspace_id = normalize_workspace_id(workspace_id)?;
        let record = self
            .workspaces
            .remove(&workspace_id)
            .with_context(|| format!("workspace {} not found", workspace_id))?;

        self.token_index.retain(|_, id| id != &workspace_id);

        let root_canon = self
            .root
            .canonicalize()
            .unwrap_or_else(|_| self.root.clone());
        let workspace_canon = record
            .path
            .canonicalize()
            .unwrap_or_else(|_| record.path.clone());
        if !workspace_canon.starts_with(&root_canon) {
            bail!(
                "refusing to delete workspace outside root: {}",
                record.path.display()
            );
        }

        fs::remove_dir_all(&record.path)
            .with_context(|| format!("failed to delete workspace {}", record.path.display()))?;
        Ok(())
    }
}

/// Resolve workspace registry root from runtime config.
pub fn registry_root_from_config(config: &crate::config::Config) -> Result<PathBuf> {
    let config_dir = config
        .config_path
        .parent()
        .context("config path must have a parent directory")?;
    Ok(config.workspaces.resolve_root(config_dir))
}

fn default_workspace_config() -> &'static str {
    "# Workspace-scoped config overrides.\n"
}

fn write_workspace_metadata(workspace_dir: &Path, metadata: &WorkspaceMetadata) -> Result<()> {
    let metadata_path = workspace_dir.join(WORKSPACE_METADATA_FILE);
    let serialized =
        toml::to_string(metadata).context("failed to serialize workspace metadata to TOML")?;
    fs::write(&metadata_path, serialized)
        .with_context(|| format!("failed to write {}", metadata_path.display()))?;
    Ok(())
}

fn normalize_workspace_id(workspace_id: &str) -> Result<String> {
    let parsed = Uuid::parse_str(workspace_id.trim()).context(
        "workspace_id must be a valid UUID (for example 550e8400-e29b-41d4-a716-446655440000)",
    )?;
    Ok(parsed.to_string())
}

fn generate_token() -> String {
    let bytes: [u8; 32] = rand::random();
    format!("zc_ws_{}", hex::encode(bytes))
}

fn hash_token(token: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(token.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_registry_load_empty_dir() {
        let tmp = tempfile::TempDir::new().expect("tempdir should be created");
        let registry = WorkspaceRegistry::load(tmp.path()).expect("registry should load");
        assert!(registry.list().is_empty());
    }

    #[test]
    fn workspace_registry_resolve_valid_token() {
        let tmp = tempfile::TempDir::new().expect("tempdir should be created");
        let mut registry = WorkspaceRegistry::load(tmp.path()).expect("registry should load");

        let (id, token) = registry
            .create("workspace-a")
            .expect("workspace should be created");
        let resolved = registry.resolve(&token).expect("token should resolve");
        assert_eq!(resolved.id, id);
    }

    #[test]
    fn workspace_registry_resolve_unknown_token() {
        let tmp = tempfile::TempDir::new().expect("tempdir should be created");
        let mut registry = WorkspaceRegistry::load(tmp.path()).expect("registry should load");
        registry
            .create("workspace-a")
            .expect("workspace should be created");
        assert!(registry.resolve("zc_ws_unknown").is_none());
    }

    #[test]
    fn workspace_registry_token_hash_not_plain() {
        let tmp = tempfile::TempDir::new().expect("tempdir should be created");
        let mut registry = WorkspaceRegistry::load(tmp.path()).expect("registry should load");
        let (workspace_id, token) = registry
            .create("workspace-a")
            .expect("workspace should be created");

        let metadata_path = tmp.path().join(workspace_id).join(WORKSPACE_METADATA_FILE);
        let metadata_raw = fs::read_to_string(&metadata_path).expect("metadata file should exist");
        let metadata: WorkspaceMetadata =
            toml::from_str(&metadata_raw).expect("metadata should parse");

        assert_ne!(metadata.token_hash, token);
        assert!(metadata.token_hash.starts_with("sha256:"));
    }

    #[test]
    fn workspace_id_path_traversal_rejected() {
        let tmp = tempfile::TempDir::new().expect("tempdir should be created");
        let mut registry = WorkspaceRegistry::load(tmp.path()).expect("registry should load");

        assert!(registry.disable("../evil").is_err());
        assert!(registry.rotate_token("../evil").is_err());
        assert!(registry.delete("../evil").is_err());
    }
}
