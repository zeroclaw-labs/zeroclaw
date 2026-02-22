use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

/// Set ownership of a path to `uid:uid` on Unix. Fatal on failure.
/// Skips the syscall if the file is already owned by the target uid
/// (avoids EPERM on macOS sandbox where even chown-to-self fails).
#[cfg(unix)]
fn chown_to(path: &Path, uid: u32) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.uid() == uid {
            return Ok(());
        }
    }
    use std::os::unix::fs::chown;
    chown(path, Some(uid), Some(uid))
        .with_context(|| format!("chown {} to uid={} failed", path.display(), uid))
}

/// Create tenant directory structure under `{data_dir}/{slug}/`.
///
/// Creates: workspace/, memory/, config/, zeroclaw-home/ subdirectories.
/// On Unix, sets ownership to `uid:uid`. Fails if chown is denied.
pub fn create_tenant_dirs(data_dir: &str, slug: &str, uid: u32) -> Result<()> {
    let base = Path::new(data_dir).join(slug);

    for subdir in &["workspace", "memory", "config", "zeroclaw-home"] {
        let dir = base.join(subdir);
        fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create dir: {}", dir.display()))?;
    }

    // Set ownership on Unix so the container user can write.
    // Fatal on failure — container will crash with permission denied if ownership is wrong.
    #[cfg(unix)]
    {
        chown_to(&base, uid)?;
        for subdir in &["workspace", "memory", "config", "zeroclaw-home"] {
            chown_to(&base.join(subdir), uid)?;
        }
    }

    Ok(())
}

/// Ensure all tenant directories and files are owned by `uid:uid`.
///
/// Called by `sync_and_restart` before recreating the container to fix any
/// ownership drift (e.g. config.toml rewritten by root process).
#[cfg(unix)]
pub fn ensure_tenant_ownership(data_dir: &str, slug: &str, uid: u32) -> Result<()> {
    let base = Path::new(data_dir).join(slug);
    if !base.exists() {
        return Ok(());
    }

    chown_to(&base, uid)?;
    for subdir in &["workspace", "memory", "config", "zeroclaw-home"] {
        let dir = base.join(subdir);
        if dir.exists() {
            chown_to(&dir, uid)?;
            // Also chown files inside each subdir (config.toml, etc.)
            for entry in fs::read_dir(&dir)? {
                let entry = entry?;
                chown_to(&entry.path(), uid)?;
            }
        }
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn ensure_tenant_ownership(_data_dir: &str, _slug: &str, _uid: u32) -> Result<()> {
    Ok(())
}

/// Write config.toml to `{data_dir}/{slug}/zeroclaw-home/config.toml`
/// and set ownership to `uid:uid`.
///
/// ZeroClaw reads config from `$HOME/.zeroclaw/config.toml`.
/// Inside the container, `zeroclaw-home` is mounted at `/zeroclaw-data/.zeroclaw`.
pub fn write_tenant_config(
    data_dir: &str,
    slug: &str,
    config_content: &str,
    uid: u32,
) -> Result<()> {
    let config_path = Path::new(data_dir)
        .join(slug)
        .join("zeroclaw-home")
        .join("config.toml");

    // Ensure parent dir exists
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create config dir: {}", parent.display()))?;
    }

    fs::write(&config_path, config_content)
        .with_context(|| format!("failed to write config: {}", config_path.display()))?;

    #[cfg(unix)]
    chown_to(&config_path, uid)?;

    Ok(())
}

/// Get disk usage in bytes for a tenant's data directory.
/// Uses platform-appropriate `du` command.
pub fn tenant_disk_usage(data_dir: &str, slug: &str) -> Result<u64> {
    let path = Path::new(data_dir).join(slug);
    if !path.exists() {
        return Ok(0);
    }
    let path_str = path.to_string_lossy().to_string();

    // Try `du -sb` first (Linux), fall back to `du -sk` (macOS)
    let output = std::process::Command::new("du")
        .args(["-sb", &path_str])
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            Ok(stdout
                .split_whitespace()
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0))
        }
        _ => {
            // macOS fallback: du -sk gives KB
            let out = std::process::Command::new("du")
                .args(["-sk", &path_str])
                .output()
                .with_context(|| format!("du failed for {}", path.display()))?;
            let stdout = String::from_utf8_lossy(&out.stdout);
            let kb: u64 = stdout
                .split_whitespace()
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            Ok(kb * 1024)
        }
    }
}

/// Read `paired_tokens` from an existing config.toml.
///
/// Returns the raw TOML line (e.g. `paired_tokens = ["hash1","hash2"]`)
/// so it can be appended to a freshly rendered config. Returns `None`
/// if the file doesn't exist or has no `paired_tokens` entry.
pub fn read_paired_tokens(data_dir: &str, slug: &str) -> Option<String> {
    let config_path = Path::new(data_dir)
        .join(slug)
        .join("zeroclaw-home")
        .join("config.toml");

    let content = fs::read_to_string(&config_path).ok()?;
    content
        .lines()
        .find(|line| line.trim_start().starts_with("paired_tokens"))
        .map(|line| line.to_string())
}

/// Remove the entire tenant directory tree: `{data_dir}/{slug}/`.
pub fn remove_tenant_dirs(data_dir: &str, slug: &str) -> Result<()> {
    let base = Path::new(data_dir).join(slug);
    if base.exists() {
        fs::remove_dir_all(&base)
            .with_context(|| format!("failed to remove tenant dirs: {}", base.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Get the current process UID via temp dir metadata.
    #[cfg(unix)]
    fn current_uid() -> u32 {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(std::env::temp_dir())
            .map(|m| m.uid())
            .unwrap_or(0)
    }

    #[cfg(not(unix))]
    fn current_uid() -> u32 {
        0
    }

    #[test]
    fn test_create_dirs_structure() {
        let tmp = std::env::temp_dir().join(format!("zctest-fs-{}", uuid::Uuid::new_v4()));
        let data_dir = tmp.to_string_lossy().to_string();
        let slug = "test-tenant";

        // Use current UID so chown succeeds
        create_tenant_dirs(&data_dir, slug, current_uid()).unwrap();

        assert!(tmp.join(slug).join("workspace").exists());
        assert!(tmp.join(slug).join("memory").exists());
        assert!(tmp.join(slug).join("config").exists());
        assert!(tmp.join(slug).join("zeroclaw-home").exists());

        // Cleanup
        fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn test_write_config_creates_file() {
        let tmp = std::env::temp_dir().join(format!("zctest-fs-{}", uuid::Uuid::new_v4()));
        let data_dir = tmp.to_string_lossy().to_string();
        let slug = "cfg-tenant";

        // Create dirs first
        fs::create_dir_all(tmp.join(slug).join("config")).unwrap();

        let content = "[gateway]\ntrust_proxy = true\n";
        write_tenant_config(&data_dir, slug, content, current_uid()).unwrap();

        let config_path = tmp.join(slug).join("zeroclaw-home").join("config.toml");
        assert!(config_path.exists());
        let read_back = fs::read_to_string(&config_path).unwrap();
        assert_eq!(read_back, content);

        // Cleanup
        fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn test_write_config_creates_parent_dirs() {
        let tmp = std::env::temp_dir().join(format!("zctest-fs-{}", uuid::Uuid::new_v4()));
        let data_dir = tmp.to_string_lossy().to_string();
        let slug = "autodirs-tenant";

        // Do NOT pre-create dirs — write_tenant_config must create them
        let content = "[memory]\nbackend = \"sqlite\"\n";
        write_tenant_config(&data_dir, slug, content, current_uid()).unwrap();

        let config_path = tmp.join(slug).join("zeroclaw-home").join("config.toml");
        assert!(config_path.exists());

        // Cleanup
        fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn test_remove_dirs_cleanup() {
        let tmp = std::env::temp_dir().join(format!("zctest-fs-{}", uuid::Uuid::new_v4()));
        let data_dir = tmp.to_string_lossy().to_string();
        let slug = "remove-tenant";

        create_tenant_dirs(&data_dir, slug, current_uid()).unwrap();
        assert!(tmp.join(slug).exists());

        remove_tenant_dirs(&data_dir, slug).unwrap();
        assert!(!tmp.join(slug).exists());

        // Cleanup base
        if tmp.exists() {
            fs::remove_dir_all(&tmp).unwrap();
        }
    }

    #[test]
    fn test_remove_dirs_nonexistent_is_ok() {
        let data_dir = "/tmp/zctest-nonexistent-never-created";
        let slug = "ghost-tenant";
        let result = remove_tenant_dirs(data_dir, slug);
        assert!(result.is_ok());
    }

    #[test]
    fn test_ensure_ownership() {
        let tmp = std::env::temp_dir().join(format!("zctest-fs-{}", uuid::Uuid::new_v4()));
        let data_dir = tmp.to_string_lossy().to_string();
        let slug = "own-tenant";
        let uid = current_uid();

        create_tenant_dirs(&data_dir, slug, uid).unwrap();
        write_tenant_config(&data_dir, slug, "[test]\nfoo = 1\n", uid).unwrap();

        // ensure_tenant_ownership should succeed on already-correct ownership
        ensure_tenant_ownership(&data_dir, slug, uid).unwrap();

        // Cleanup
        fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn test_ensure_ownership_nonexistent_is_ok() {
        let result = ensure_tenant_ownership("/tmp/zctest-nonexistent", "ghost", 0);
        assert!(result.is_ok());
    }

    #[test]
    fn test_disk_usage() {
        let tmp = std::env::temp_dir().join(format!("zctest-du-{}", uuid::Uuid::new_v4()));
        let data_dir = tmp.to_string_lossy().to_string();
        let slug = "du-tenant";
        fs::create_dir_all(tmp.join(slug)).unwrap();
        // Write a file to have non-zero disk usage
        fs::write(tmp.join(slug).join("testfile.txt"), "hello world").unwrap();
        let usage = tenant_disk_usage(&data_dir, slug).unwrap();
        assert!(usage > 0, "disk usage should be > 0");
        fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn test_disk_usage_nonexistent() {
        let usage = tenant_disk_usage("/tmp/zctest-nonexistent-du", "ghost").unwrap();
        assert_eq!(usage, 0);
    }
}
