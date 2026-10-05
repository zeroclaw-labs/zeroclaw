//! Where the daemon listens.
//!
//! The resolution itself lives in `zeroclaw_api::rpc_endpoint`, shared with
//! the daemon and zerocode, so every client lands on the endpoint the daemon
//! bound: a non-blank `ZEROCLAW_SOCKET` wins, otherwise the platform default
//! under the data directory.

use std::path::{Path, PathBuf};

pub use zeroclaw_api::rpc_endpoint::{
    ClientEndpoints, SOCKET_ENV, client_endpoints, client_endpoints_with, default_endpoint,
};

/// Resolve the daemon's local IPC endpoint for `data_dir`.
pub fn resolve_socket_path(data_dir: &Path) -> PathBuf {
    zeroclaw_api::rpc_endpoint::resolve_endpoint(data_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_api::rpc_endpoint::{AGREEMENT_DATA_DIRS, resolve_endpoint};

    #[test]
    fn resolves_what_the_shared_resolver_resolves() {
        for dir in AGREEMENT_DATA_DIRS {
            let dir = Path::new(dir);
            assert_eq!(
                resolve_socket_path(dir),
                resolve_endpoint(dir),
                "{}",
                dir.display()
            );
            assert_eq!(client_endpoints(dir).primary, resolve_endpoint(dir));
        }
    }
}
