//! Where the daemon listens.
//!
//! Mirrors the daemon's own resolution (`zeroclaw_runtime::rpc::local::
//! socket_path`) so a client that does not link the runtime lands on the
//! same endpoint: the `ZEROCLAW_SOCKET` override wins, otherwise the
//! platform default under the data directory.

use std::path::{Path, PathBuf};

/// Environment variable that overrides the daemon endpoint on every platform.
pub const SOCKET_ENV: &str = "ZEROCLAW_SOCKET";

/// Resolve the daemon's local IPC endpoint for `data_dir`.
pub fn resolve_socket_path(data_dir: &Path) -> PathBuf {
    if let Ok(path) = std::env::var(SOCKET_ENV) {
        return PathBuf::from(path);
    }
    default_endpoint(data_dir)
}

/// The platform default endpoint under `data_dir`, ignoring the override.
#[cfg(unix)]
pub fn default_endpoint(data_dir: &Path) -> PathBuf {
    data_dir.join("daemon.sock")
}

/// The platform default endpoint under `data_dir`, ignoring the override.
///
/// Named pipes live in a flat kernel namespace, so the daemon derives the
/// pipe name from a hash of the data directory.
#[cfg(windows)]
pub fn default_endpoint(data_dir: &Path) -> PathBuf {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    data_dir.hash(&mut hasher);
    PathBuf::from(format!(r"\\.\pipe\zeroclaw-{:x}", hasher.finish()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_endpoint_lives_under_the_data_dir() {
        let path = default_endpoint(Path::new("/tmp/zc-data"));
        #[cfg(unix)]
        assert_eq!(path, PathBuf::from("/tmp/zc-data/daemon.sock"));
        #[cfg(windows)]
        assert!(path.to_string_lossy().starts_with(r"\\.\pipe\zeroclaw-"));
    }

    #[test]
    fn default_endpoint_is_deterministic() {
        assert_eq!(
            default_endpoint(Path::new("/a/b")),
            default_endpoint(Path::new("/a/b"))
        );
        assert_ne!(
            default_endpoint(Path::new("/a/b")),
            default_endpoint(Path::new("/a/c"))
        );
    }
}
