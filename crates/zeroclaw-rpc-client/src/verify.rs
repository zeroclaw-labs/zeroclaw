//! Who serves the daemon endpoint, checked before a credential is sent.
//!
//! A bearer written to the wrong listener is a bearer handed to whoever runs
//! it, and a listener can claim any pid or version inside `initialize`. So
//! [`crate::RpcClient::connect_local`] asks the kernel, not the peer: on Unix
//! the connected socket's peer uid must be the expected account, and the
//! directory holding the socket must belong to that account with no group or
//! other write access, so no other account can have placed or swapped the
//! socket. On Windows the process the kernel names as the pipe's server
//! must run as the expected account, and the pipe itself must be owned by
//! that account (or the Administrators group). Only that account, `SYSTEM`,
//! the Administrators group and `CREATOR OWNER` may hold the right to add
//! instances to the pipe or change its security, and no broad group may
//! write to it (the `pipe` module, built for Windows). These checks run on
//! every dial that carries a credential (a bearer, a TUI signature, or
//! forwarded environment), reconnects included, and there is no switch that
//! skips them.
//!
//! The check runs after the stream is open and before anything is written.
//! On Windows the client opens the pipe with identification-level security
//! only (`SECURITY_IDENTIFICATION | SECURITY_SQOS_PRESENT`, set explicitly
//! rather than inherited from tokio's default), so whichever server answers
//! before the check can learn who this client is but cannot act as it.
//!
//! What this cannot tell apart: two processes of the same account. Malware
//! running as the core's own user can read its configuration and tokens
//! anyway, so the check draws the line at the OS account. On Windows an
//! administrator is outside the line too: an elevated process of any
//! administrator account creates pipes the Administrators group owns, and
//! those pass. Another named account or group may be granted plain read and
//! write access, which lets it connect as a client but not serve under the
//! pipe's name. An [`EndpointOwner::Uid`] has no Windows form yet, so a
//! launcher that runs the core under another Windows account cannot name
//! that account to the check.

use std::fmt;
use std::path::{Path, PathBuf};

#[cfg(any(windows, test))]
pub(crate) mod pipe;

/// The OS account that must serve a local endpoint before
/// [`crate::RpcClient::connect_local`] sends it a credential.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EndpointOwner {
    /// The account this client runs as (its effective uid on Unix). Right
    /// for every deployment where the client and the core share an account.
    #[default]
    SameAccount,
    /// A specific Unix uid, for a launcher that runs the core under its own
    /// account and hands the client that account. A uid names no Windows
    /// account, so a Windows dial that asks for one is refused.
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
    /// The requested owner cannot be checked on this platform (a Unix uid
    /// on Windows), so the credential-bearing dial is refused.
    Unsupported,
    /// The process the kernel names as the pipe's server runs as another
    /// Windows account. The accounts are SIDs.
    ServerAccount {
        pid: u32,
        expected: String,
        actual: String,
    },
    /// The pipe is owned by an account that is neither the expected one nor
    /// the Administrators group.
    PipeOwner { expected: String, actual: String },
    /// An account other than the expected one, `SYSTEM`, the Administrators
    /// group or `CREATOR OWNER` may add instances of the pipe or change its
    /// security, or a broad group (everyone, anonymous, or every signed-in
    /// user) may write to it. `access` is the access mask granted to
    /// `account`.
    PipeWritable { account: String, access: u32 },
    /// The pipe has no access list, so every account may do anything to it.
    PipeUnprotected,
    /// The pipe's owner and access list could not be read or understood.
    PipeSecurityUnreadable(String),
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
                f.write_str("the requested endpoint owner cannot be checked on this platform")
            }
            Self::ServerAccount {
                pid,
                expected,
                actual,
            } => write!(
                f,
                "served by process {pid} running as {actual}, expected {expected}"
            ),
            Self::PipeOwner { expected, actual } => write!(
                f,
                "the pipe is owned by {actual}, not by {expected} or the Administrators group"
            ),
            Self::PipeWritable { account, access } => write!(
                f,
                "the pipe lets {account} write to it, add instances or change its security \
                 (access {access:#010x})"
            ),
            Self::PipeUnprotected => {
                f.write_str("the pipe has no access list, so every account can use or replace it")
            }
            Self::PipeSecurityUnreadable(error) => {
                write!(f, "cannot read the pipe's owner and access list: {error}")
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

/// Prove the pipe `stream` holds is served, owned and kept private by
/// `owner` before anything is written to it (see [`pipe`]).
#[cfg(windows)]
pub(crate) async fn verify_local_endpoint(
    stream: &tokio::net::windows::named_pipe::NamedPipeClient,
    _path: &Path,
    owner: EndpointOwner,
) -> Result<(), EndpointRejection> {
    use std::os::windows::io::AsRawHandle;
    match owner {
        EndpointOwner::SameAccount => pipe::verify_pipe_server(
            &pipe::Win32Pipe::new(stream.as_raw_handle()),
            expected_account_override(),
        ),
        EndpointOwner::Uid(_) => Err(EndpointRejection::Unsupported),
    }
}

#[cfg(all(windows, not(test)))]
fn expected_account_override() -> Option<pipe::Sid> {
    None
}

#[cfg(all(windows, test))]
thread_local! {
    /// Under test, the account a pipe must be served by instead of this
    /// process's own, so a real pipe this process serves can stand in for
    /// one another account serves.
    static EXPECTED_ACCOUNT: std::cell::RefCell<Option<pipe::Sid>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(all(windows, test))]
fn expected_account_override() -> Option<pipe::Sid> {
    EXPECTED_ACCOUNT.with(|expected| expected.borrow().clone())
}

/// Expect `account` to serve pipes dialed on this thread, until reset with
/// `None`.
#[cfg(all(windows, test))]
pub(crate) fn expect_account_for_test(account: Option<pipe::Sid>) {
    EXPECTED_ACCOUNT.with(|expected| *expected.borrow_mut() = account);
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
