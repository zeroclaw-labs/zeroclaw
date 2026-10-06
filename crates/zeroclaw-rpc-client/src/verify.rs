//! Who serves the daemon endpoint, checked before a credential is sent.
//!
//! A bearer written to the wrong listener is a bearer handed to whoever runs
//! it, and a listener can claim any pid or version inside `initialize`. So
//! [`crate::RpcClient::connect_local`] asks the kernel, not the peer: on Unix
//! the connected socket's peer uid must be the expected account, and the
//! directory holding the socket must belong to that account with no group or
//! other write access, so no other account can have placed or swapped the
//! socket. Both checks run on every dial that carries a credential (a
//! bearer, a TUI signature, or forwarded environment), reconnects included,
//! and there is no switch that skips them. Windows cannot prove the pipe
//! server's account from here yet, so it refuses those dials instead.
//!
//! What this cannot tell apart: two processes of the same account. Malware
//! running as the core's own user can read its configuration and tokens
//! anyway, so the check draws the line at the OS account.

use std::fmt;
use std::path::{Path, PathBuf};

/// The OS account that must serve a local endpoint before
/// [`crate::RpcClient::connect_local`] sends it a credential.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EndpointOwner {
    /// The account this client runs as (its effective uid on Unix). Right
    /// for every deployment where the client and the core share an account.
    #[default]
    SameAccount,
    /// A specific Unix uid, for a launcher that runs the core under its own
    /// account and hands the client that account.
    Uid(u32),
}

/// Why a local endpoint was refused. No byte of the handshake was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndpointRejection {
    /// The kernel reports that another account serves the socket.
    PeerUid { expected: u32, actual: u32 },
    /// The kernel would not report who serves the socket.
    PeerUnknown(String),
    /// The directory holding the socket belongs to another account.
    DirectoryOwner {
        dir: PathBuf,
        expected: u32,
        actual: u32,
    },
    /// Other accounts can create or replace entries in the directory
    /// holding the socket. `mode` is the directory's permission bits.
    DirectoryWritable { dir: PathBuf, mode: u32 },
    /// The socket's location could not be resolved or inspected.
    DirectoryUnreadable { dir: PathBuf, error: String },
    /// This platform cannot yet prove who serves the endpoint, so
    /// credential-bearing dials are refused outright.
    Unsupported,
}

impl fmt::Display for EndpointRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PeerUid { expected, actual } => {
                write!(f, "served by uid {actual}, expected {expected}")
            }
            Self::PeerUnknown(error) => {
                write!(f, "the kernel reported no peer credential: {error}")
            }
            Self::DirectoryOwner {
                dir,
                expected,
                actual,
            } => write!(
                f,
                "its directory {} is owned by uid {actual}, expected {expected}",
                dir.display()
            ),
            Self::DirectoryWritable { dir, mode } => write!(
                f,
                "its directory {} is writable by other accounts (mode {mode:o})",
                dir.display()
            ),
            Self::DirectoryUnreadable { dir, error } => {
                write!(f, "cannot inspect its directory {}: {error}", dir.display())
            }
            Self::Unsupported => {
                f.write_str("this platform cannot verify which account serves a local endpoint yet")
            }
        }
    }
}

/// The peer uid the kernel reported must be the expected account's.
#[cfg(unix)]
fn check_peer_uid(expected: u32, actual: u32) -> Result<(), EndpointRejection> {
    if actual == expected {
        Ok(())
    } else {
        Err(EndpointRejection::PeerUid { expected, actual })
    }
}

/// The directory holding the socket must belong to the expected account and
/// be writable by nobody else. The sticky bit is no exemption: it stops
/// others from removing the socket, not from binding the path first while
/// the daemon is down. A root-owned directory is refused too, although only
/// root could plant a socket there; a launcher that places the socket in one
/// must pass root's uid or move the socket.
#[cfg(unix)]
fn check_socket_dir(
    dir: &Path,
    expected: u32,
    owner: u32,
    mode: u32,
) -> Result<(), EndpointRejection> {
    if owner != expected {
        return Err(EndpointRejection::DirectoryOwner {
            dir: dir.to_path_buf(),
            expected,
            actual: owner,
        });
    }
    if mode & 0o022 != 0 {
        return Err(EndpointRejection::DirectoryWritable {
            dir: dir.to_path_buf(),
            mode: mode & 0o7777,
        });
    }
    Ok(())
}

#[cfg(unix)]
impl EndpointOwner {
    fn expected_uid(self) -> u32 {
        match self {
            // SAFETY: `geteuid` is a parameter-free process query with no
            // pointer arguments or memory ownership requirements.
            Self::SameAccount => unsafe { libc::geteuid() },
            Self::Uid(uid) => uid,
        }
    }
}

/// Prove `stream`, dialed at `path`, is served by `owner` before anything is
/// written to it.
#[cfg(unix)]
pub(crate) async fn verify_local_endpoint(
    stream: &tokio::net::UnixStream,
    path: &Path,
    owner: EndpointOwner,
) -> Result<(), EndpointRejection> {
    use std::os::unix::fs::MetadataExt;

    let expected = owner.expected_uid();
    // The kernel reports the account of the process that called `listen()`.
    // The daemon binds and listens itself, so that is the daemon. A socket
    // handed over by an activating supervisor running as root would report
    // uid 0 and be refused here.
    let peer = stream
        .peer_cred()
        .map_err(|e| EndpointRejection::PeerUnknown(e.to_string()))?;
    check_peer_uid(expected, peer.uid())?;

    // Judge the directory that holds the socket itself, not a symlink that
    // points at it from somewhere tidier.
    let socket = tokio::fs::canonicalize(path).await.map_err(|e| {
        EndpointRejection::DirectoryUnreadable {
            dir: path.to_path_buf(),
            error: e.to_string(),
        }
    })?;
    let dir = socket.parent().unwrap_or_else(|| Path::new("/"));
    let metadata =
        tokio::fs::metadata(dir)
            .await
            .map_err(|e| EndpointRejection::DirectoryUnreadable {
                dir: dir.to_path_buf(),
                error: e.to_string(),
            })?;
    check_socket_dir(dir, expected, metadata.uid(), metadata.mode())
}

/// Windows cannot prove the pipe server's account from this crate yet, so a
/// credential-bearing dial is refused rather than sent unverified.
#[cfg(windows)]
pub(crate) async fn verify_local_endpoint(
    _stream: &tokio::net::windows::named_pipe::NamedPipeClient,
    _path: &Path,
    _owner: EndpointOwner,
) -> Result<(), EndpointRejection> {
    Err(EndpointRejection::Unsupported)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn a_peer_of_another_account_is_refused_and_both_uids_are_named() {
        let rejection = check_peer_uid(501, 1002).expect_err("uid mismatch");
        assert_eq!(
            rejection,
            EndpointRejection::PeerUid {
                expected: 501,
                actual: 1002
            }
        );
        assert_eq!(rejection.to_string(), "served by uid 1002, expected 501");
    }

    #[test]
    fn a_peer_of_the_expected_account_passes() {
        assert_eq!(check_peer_uid(501, 501), Ok(()));
        assert_eq!(check_peer_uid(0, 0), Ok(()));
    }

    #[test]
    fn a_directory_of_another_account_is_refused() {
        let dir = Path::new("/srv/zc");
        for owner in [0, 1002] {
            assert_eq!(
                check_socket_dir(dir, 501, owner, 0o700),
                Err(EndpointRejection::DirectoryOwner {
                    dir: dir.to_path_buf(),
                    expected: 501,
                    actual: owner,
                }),
                "owner {owner}"
            );
        }
    }

    #[test]
    fn a_directory_others_can_write_is_refused_even_when_sticky() {
        let dir = Path::new("/srv/zc");
        // `metadata.mode()` carries the file-type bits (0o40000 for a
        // directory); the check must ignore them.
        for mode in [0o720, 0o702, 0o770, 0o777, 0o1777, 0o40770] {
            let rejection = check_socket_dir(dir, 501, 501, mode).expect_err("writable");
            assert_eq!(
                rejection,
                EndpointRejection::DirectoryWritable {
                    dir: dir.to_path_buf(),
                    mode: mode & 0o7777,
                },
                "mode {mode:o}"
            );
        }
    }

    #[test]
    fn a_private_or_read_only_shared_directory_passes() {
        let dir = Path::new("/srv/zc");
        for mode in [0o700, 0o750, 0o755, 0o40700, 0o1755] {
            assert_eq!(
                check_socket_dir(dir, 501, 501, mode),
                Ok(()),
                "mode {mode:o}"
            );
        }
    }

    #[test]
    fn the_default_owner_is_this_account() {
        assert_eq!(EndpointOwner::default(), EndpointOwner::SameAccount);
    }
}
