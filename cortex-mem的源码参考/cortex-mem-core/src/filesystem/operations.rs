use crate::{Error, FileEntry, FileMetadata, MemoryMetadata, Result};
use async_trait::async_trait;
use chrono::Utc;
use std::path::{Path, PathBuf};
use tokio::fs;

use super::uri::UriParser;

/// Trait for filesystem operations
#[async_trait]
pub trait FilesystemOperations: Send + Sync {
    /// List directory contents
    async fn list(&self, uri: &str) -> Result<Vec<FileEntry>>;

    /// Read file content
    async fn read(&self, uri: &str) -> Result<String>;

    /// Write file content
    async fn write(&self, uri: &str, content: &str) -> Result<()>;

    /// Delete file or directory
    async fn delete(&self, uri: &str) -> Result<()>;

    /// Check if file/directory exists
    async fn exists(&self, uri: &str) -> Result<bool>;

    /// Get file metadata
    async fn metadata(&self, uri: &str) -> Result<FileMetadata>;
}

/// Cortex filesystem implementation
pub struct CortexFilesystem {
    root: PathBuf,
    tenant_id: Option<String>,
}

impl CortexFilesystem {
    /// Create a new CortexFilesystem with the given root directory (no tenant isolation)
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
            tenant_id: None,
        }
    }

    /// Create a new CortexFilesystem with tenant isolation
    pub fn with_tenant(root: impl AsRef<Path>, tenant_id: impl Into<String>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
            tenant_id: Some(tenant_id.into()),
        }
    }

    /// Get the root path
    pub fn root_path(&self) -> &Path {
        &self.root
    }

    /// Get the tenant ID
    pub fn tenant_id(&self) -> Option<&str> {
        self.tenant_id.as_deref()
    }

    /// Set the tenant ID dynamically (for runtime tenant switching)
    pub fn set_tenant(&mut self, tenant_id: Option<impl Into<String>>) {
        self.tenant_id = tenant_id.map(|id| id.into());
    }

    /// Initialize the filesystem structure
    pub async fn initialize(&self) -> Result<()> {
        // Get the base directory (with or without tenant)
        let base_dir = if let Some(tenant_id) = &self.tenant_id {
            // For tenant: /root/tenants/{tenant_id}/ (without extra cortex subfolder)
            self.root.join("tenants").join(tenant_id)
        } else {
            // For non-tenant: /root/
            self.root.clone()
        };

        // Create root directory
        fs::create_dir_all(&base_dir).await?;

        // 只有在tenant模式下才创建维度目录
        // Non-tenant模式（如cortex-mem-service全局实例）不应创建这些目录
        if self.tenant_id.is_some() {
            // Create dimension directories (style: resources, user, agent, session)
            for dimension in &["resources", "user", "agent", "session"] {
                let dir = base_dir.join(dimension);
                fs::create_dir_all(dir).await?;
            }
        }

        Ok(())
    }

    /// Get file path from URI (with tenant isolation)
    fn uri_to_path(&self, uri: &str) -> Result<PathBuf> {
        let parsed_uri = UriParser::parse(uri)?;

        // If tenant_id exists, add tenant prefix (without extra cortex subfolder)
        let path = if let Some(tenant_id) = &self.tenant_id {
            // /root/tenants/{tenant_id}/{path}
            let tenant_base = self.root.join("tenants").join(tenant_id);
            parsed_uri.to_file_path(&tenant_base)
        } else {
            // /root/{path}
            parsed_uri.to_file_path(&self.root)
        };

        Ok(path)
    }

    /// Load metadata from .metadata.json
    #[allow(dead_code)]
    async fn load_metadata(&self, dir_path: &Path) -> Result<Option<MemoryMetadata>> {
        let metadata_path = dir_path.join(".metadata.json");
        if !metadata_path.try_exists()? {
            return Ok(None);
        }

        let content = fs::read_to_string(metadata_path).await?;
        let metadata: MemoryMetadata = serde_json::from_str(&content)?;
        Ok(Some(metadata))
    }

    /// Save metadata to .metadata.json
    #[allow(dead_code)]
    async fn save_metadata(&self, dir_path: &Path, metadata: &MemoryMetadata) -> Result<()> {
        let metadata_path = dir_path.join(".metadata.json");
        let content = serde_json::to_string_pretty(metadata)?;
        fs::write(metadata_path, content).await?;
        Ok(())
    }
}

#[async_trait]
impl FilesystemOperations for CortexFilesystem {
    async fn list(&self, uri: &str) -> Result<Vec<FileEntry>> {
        let path = self.uri_to_path(uri)?;

        if !path.try_exists()? {
            return Err(Error::NotFound {
                uri: uri.to_string(),
            });
        }

        let mut entries = Vec::new();
        let mut read_dir = fs::read_dir(&path).await?;

        while let Some(entry) = read_dir.next_entry().await? {
            let metadata = entry.metadata().await?;
            let name = entry.file_name().to_string_lossy().to_string();

            // Skip hidden files except .abstract.md and .overview.md
            if name.starts_with('.') && name != ".abstract.md" && name != ".overview.md" {
                continue;
            }

            let entry_uri = format!("{}/{}", uri.trim_end_matches('/'), name);

            entries.push(FileEntry {
                uri: entry_uri,
                name,
                is_directory: metadata.is_dir(),
                size: metadata.len(),
                modified: metadata
                    .modified()
                    .map(|t| t.into())
                    .unwrap_or_else(|_| Utc::now()),
            });
        }

        Ok(entries)
    }

    async fn read(&self, uri: &str) -> Result<String> {
        let path = self.uri_to_path(uri)?;

        if !path.try_exists()? {
            return Err(Error::NotFound {
                uri: uri.to_string(),
            });
        }

        let content = fs::read_to_string(&path).await?;
        Ok(content)
    }

    async fn write(&self, uri: &str, content: &str) -> Result<()> {
        let path = self.uri_to_path(uri)?;

        // Create parent directories
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }

        fs::write(&path, content).await?;
        Ok(())
    }

    async fn delete(&self, uri: &str) -> Result<()> {
        let path = self.uri_to_path(uri)?;

        if !path.try_exists()? {
            return Err(Error::NotFound {
                uri: uri.to_string(),
            });
        }

        if path.is_dir() {
            fs::remove_dir_all(&path).await?;
        } else {
            fs::remove_file(&path).await?;
        }

        Ok(())
    }

    async fn exists(&self, uri: &str) -> Result<bool> {
        let path = self.uri_to_path(uri)?;
        Ok(path.try_exists().unwrap_or(false))
    }

    async fn metadata(&self, uri: &str) -> Result<FileMetadata> {
        let path = self.uri_to_path(uri)?;

        if !path.try_exists()? {
            return Err(Error::NotFound {
                uri: uri.to_string(),
            });
        }

        let metadata = fs::metadata(&path).await?;

        Ok(FileMetadata {
            created_at: metadata
                .created()
                .map(|t| t.into())
                .unwrap_or_else(|_| Utc::now()),
            updated_at: metadata
                .modified()
                .map(|t| t.into())
                .unwrap_or_else(|_| Utc::now()),
            size: metadata.len(),
            is_directory: metadata.is_dir(),
        })
    }
}

// 核心功能测试已迁移至 cortex-mem-tools/tests/core_functionality_tests.rs
