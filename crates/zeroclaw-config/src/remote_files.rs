use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Individually grantable operations in the Remote Files contract.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Right {
    ListDirectory,
    GetFileInfo,
    SetFileInfo,
    ReadFile,
    CreateFile,
    OverwriteFile,
    CopyFiles,
    MoveFiles,
    DeleteFiles,
    RenameFiles,
    CreateDirectory,
    CreateParentDirectories,
    RenameDirectory,
    CopyDirectory,
    MoveDirectory,
    DeleteDirectory,
    RecursiveCopy,
    RecursiveMove,
    RecursiveDelete,
    Login,
    Logout,
}

/// Rights for one resolved resource. Callers must resolve these from live
/// policy and separately enforce the authorized root and peer identity.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Rights {
    #[serde(default)]
    pub operations: BTreeSet<Right>,
    #[serde(default)]
    pub require_human_approval: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Transfer {
    CopyFiles,
    MoveFiles,
    CopyDirectory { recursive: bool },
    MoveDirectory { recursive: bool },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Decision {
    Denied,
    RequireHumanApproval,
    Allowed,
}

impl Rights {
    pub fn check(&self, required: &[Right]) -> Decision {
        if required.is_empty() || !required.iter().all(|right| self.operations.contains(right)) {
            return Decision::Denied;
        }
        if self.require_human_approval {
            Decision::RequireHumanApproval
        } else {
            Decision::Allowed
        }
    }
}

/// Preflight for a streamed transfer. This function performs no filesystem or
/// network I/O. Authorization for removing a move source must also be
/// revalidated after destination commit.
pub fn check_streamed_transfer(
    source: &Rights,
    destination: &Rights,
    transfer: Transfer,
    overwrite: bool,
) -> Decision {
    use Right::*;

    let (operation, removal, recursive) = match transfer {
        Transfer::CopyFiles => (CopyFiles, None, None),
        Transfer::MoveFiles => (MoveFiles, Some(DeleteFiles), None),
        Transfer::CopyDirectory { recursive } => {
            (CopyDirectory, None, recursive.then_some(RecursiveCopy))
        }
        Transfer::MoveDirectory { recursive } => (
            MoveDirectory,
            Some(DeleteDirectory),
            recursive.then_some(RecursiveMove),
        ),
    };
    let mut source_required = vec![operation, ReadFile];
    let destination_creation = match transfer {
        Transfer::CopyFiles | Transfer::MoveFiles => CreateFile,
        Transfer::CopyDirectory { .. } | Transfer::MoveDirectory { .. } => CreateDirectory,
    };
    let mut destination_required = vec![operation, destination_creation];
    if recursive.is_some() {
        destination_required.push(CreateFile);
        source_required.push(ListDirectory);
    }
    source_required.extend(removal);
    if matches!(transfer, Transfer::MoveDirectory { recursive: true }) {
        source_required.push(DeleteFiles);
    }
    source_required.extend(recursive);
    destination_required.extend(recursive);
    if overwrite {
        destination_required.push(OverwriteFile);
    }

    match (
        source.check(&source_required),
        destination.check(&destination_required),
    ) {
        (Decision::Denied, _) | (_, Decision::Denied) => Decision::Denied,
        (Decision::RequireHumanApproval, _) | (_, Decision::RequireHumanApproval) => {
            Decision::RequireHumanApproval
        }
        (Decision::Allowed, Decision::Allowed) => Decision::Allowed,
    }
}

/// Validate model-supplied lexical paths before calling any backend. Use a
/// portable subset rather than interpreting platform-specific path syntax.
/// Backend containment and subtree authorization remain mandatory.
pub fn is_relative_resource_path(path: &str) -> bool {
    path.split('/').all(|component| {
        let stem = component
            .split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        let reserved_device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || ["COM", "LPT"].iter().any(|prefix| {
                stem.strip_prefix(prefix).is_some_and(|suffix| {
                    matches!(
                        suffix,
                        "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"
                    )
                })
            });
        !component.is_empty()
            && !component.starts_with(' ')
            && !component.ends_with(['.', ' '])
            && !reserved_device
            && component.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b' ')
            })
    })
}

/// An operator-approved identity reference. Values are aliases, never secrets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteIdentity<'a> {
    Vault {
        credential_alias: &'a str,
        totp_alias: Option<&'a str>,
    },
    HostManaged,
}

/// Resolved endpoint borrowed from live policy. The resource identifier is
/// canonical within the catalog and must not come directly from the model.
#[derive(Clone, Copy)]
pub struct ResolvedRemote<'a> {
    pub resource_id: &'a str,
    pub identity: RemoteIdentity<'a>,
    pub rights: &'a Rights,
}

#[derive(Clone, Copy)]
pub enum TransferEndpoint<'a> {
    Workspace { rights: &'a Rights },
    Remote(ResolvedRemote<'a>),
}

impl<'a> TransferEndpoint<'a> {
    fn rights(self) -> &'a Rights {
        match self {
            Self::Workspace { rights } => rights,
            Self::Remote(remote) => remote.rights,
        }
    }
}

/// Describes where the executor must obtain each identity. Aliases are
/// resolved against each endpoint's grant and verified peer at use time.
#[derive(Debug, Eq, PartialEq)]
pub enum TransferRoute<'a> {
    SameResource(ResolvedIdentity<'a>),
    Download(ResolvedIdentity<'a>),
    Upload(ResolvedIdentity<'a>),
    BetweenResources {
        source: ResolvedIdentity<'a>,
        destination: ResolvedIdentity<'a>,
    },
}

#[derive(Debug, Eq, PartialEq)]
pub struct ResolvedIdentity<'a> {
    pub resource_id: &'a str,
    pub identity: RemoteIdentity<'a>,
}

impl<'a> From<ResolvedRemote<'a>> for ResolvedIdentity<'a> {
    fn from(remote: ResolvedRemote<'a>) -> Self {
        Self {
            resource_id: remote.resource_id,
            identity: remote.identity,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransferHold {
    InvalidRelativePath,
    WorkspaceOnly,
    ConflictingResourceIdentity,
    Denied,
    RequireHumanApproval,
}

/// Prepare a streamed route from canonical catalog entries, before any
/// backend access. A held operation returns no route or identity reference.
/// The executor must still enforce workspace policy, peer verification,
/// root containment, and grant revalidation before dispatch and removal.
pub fn plan_streamed_transfer<'a>(
    source: TransferEndpoint<'a>,
    source_path: &str,
    destination: TransferEndpoint<'a>,
    destination_path: &str,
    transfer: Transfer,
    overwrite: bool,
) -> Result<TransferRoute<'a>, TransferHold> {
    if ![source_path, destination_path]
        .into_iter()
        .all(is_relative_resource_path)
    {
        return Err(TransferHold::InvalidRelativePath);
    }

    let route = match (source, destination) {
        (TransferEndpoint::Workspace { .. }, TransferEndpoint::Workspace { .. }) => {
            return Err(TransferHold::WorkspaceOnly);
        }
        (TransferEndpoint::Remote(remote), TransferEndpoint::Workspace { .. }) => {
            TransferRoute::Download(remote.into())
        }
        (TransferEndpoint::Workspace { .. }, TransferEndpoint::Remote(remote)) => {
            TransferRoute::Upload(remote.into())
        }
        (TransferEndpoint::Remote(source), TransferEndpoint::Remote(destination))
            if source.resource_id == destination.resource_id =>
        {
            if source.identity != destination.identity {
                return Err(TransferHold::ConflictingResourceIdentity);
            }
            TransferRoute::SameResource(source.into())
        }
        (TransferEndpoint::Remote(source), TransferEndpoint::Remote(destination)) => {
            TransferRoute::BetweenResources {
                source: source.into(),
                destination: destination.into(),
            }
        }
    };

    match check_streamed_transfer(source.rights(), destination.rights(), transfer, overwrite) {
        Decision::Denied => Err(TransferHold::Denied),
        Decision::RequireHumanApproval => Err(TransferHold::RequireHumanApproval),
        Decision::Allowed => Ok(route),
    }
}

/// Operations verified with the shared contract on each supported host OS.
/// A backend may only advertise operations present in all three sets.
#[derive(Debug, Default)]
pub struct PlatformOperations {
    pub linux: BTreeSet<Right>,
    pub windows: BTreeSet<Right>,
    pub macos: BTreeSet<Right>,
}

impl PlatformOperations {
    pub fn portable_operations(&self) -> BTreeSet<Right> {
        self.linux
            .intersection(&self.windows)
            .copied()
            .filter(|operation| self.macos.contains(operation))
            .collect()
    }
}

/// Installed host facilities are checked separately from parity. A missing
/// prerequisite must not trigger a downgrade to another backend or transport.
#[derive(Debug, Eq, PartialEq)]
pub enum BackendAvailability {
    MissingHostPrerequisite,
    AwaitingPlatformParity,
    Available(BTreeSet<Right>),
}

impl PlatformOperations {
    pub fn availability(&self, host_ready: bool) -> BackendAvailability {
        if !host_ready {
            return BackendAvailability::MissingHostPrerequisite;
        }
        let operations = self.portable_operations();
        if operations.is_empty() {
            BackendAvailability::AwaitingPlatformParity
        } else {
            BackendAvailability::Available(operations)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn availability_requires_host_prerequisites_and_all_three_platforms() {
        use Right::*;
        let mut operations = PlatformOperations {
            linux: BTreeSet::from([ListDirectory, GetFileInfo, DeleteFiles]),
            windows: BTreeSet::from([ListDirectory, GetFileInfo]),
            ..PlatformOperations::default()
        };
        assert_eq!(
            operations.availability(true),
            BackendAvailability::AwaitingPlatformParity
        );
        operations.macos = BTreeSet::from([ListDirectory, DeleteFiles]);
        assert_eq!(
            operations.availability(true),
            BackendAvailability::Available(BTreeSet::from([ListDirectory]))
        );
        assert_eq!(
            operations.availability(false),
            BackendAvailability::MissingHostPrerequisite
        );
    }

    #[test]
    fn route_preserves_endpoint_bindings_even_when_aliases_match() {
        use Right::*;
        let source_rights = rights(&[CopyFiles, ReadFile], false);
        let destination_rights = rights(&[CopyFiles, CreateFile], false);
        let identity = RemoteIdentity::Vault {
            credential_alias: "TEST_LOGIN",
            totp_alias: Some("TEST_TOTP"),
        };
        let source = ResolvedRemote {
            resource_id: "server-a",
            identity,
            rights: &source_rights,
        };
        let destination = ResolvedRemote {
            resource_id: "server-b",
            identity,
            rights: &destination_rights,
        };
        let plan = |source, destination| {
            plan_streamed_transfer(
                source,
                "report.pdf",
                destination,
                "inbox/report.pdf",
                Transfer::CopyFiles,
                false,
            )
        };
        assert_eq!(
            plan(
                TransferEndpoint::Remote(source),
                TransferEndpoint::Remote(destination)
            ),
            Ok(TransferRoute::BetweenResources {
                source: source.into(),
                destination: destination.into()
            })
        );
        assert_eq!(
            plan(
                TransferEndpoint::Remote(source),
                TransferEndpoint::Workspace {
                    rights: &destination_rights
                }
            ),
            Ok(TransferRoute::Download(source.into()))
        );
        assert_eq!(
            plan(
                TransferEndpoint::Workspace {
                    rights: &source_rights
                },
                TransferEndpoint::Remote(destination)
            ),
            Ok(TransferRoute::Upload(destination.into()))
        );
        let same_resource = ResolvedRemote {
            resource_id: "server-a",
            ..destination
        };
        assert_eq!(
            plan(
                TransferEndpoint::Remote(source),
                TransferEndpoint::Remote(same_resource)
            ),
            Ok(TransferRoute::SameResource(source.into()))
        );
        let conflicting = ResolvedRemote {
            identity: RemoteIdentity::HostManaged,
            ..same_resource
        };
        assert_eq!(
            plan(
                TransferEndpoint::Remote(source),
                TransferEndpoint::Remote(conflicting)
            ),
            Err(TransferHold::ConflictingResourceIdentity)
        );
    }

    #[test]
    fn invalid_destination_and_approval_hold_return_no_route() {
        use Right::*;
        let source_rights = rights(&[CopyFiles, ReadFile], false);
        let destination_rights = rights(&[CopyFiles, CreateFile], true);
        let source = TransferEndpoint::Workspace {
            rights: &source_rights,
        };
        let destination = TransferEndpoint::Remote(ResolvedRemote {
            resource_id: "server-a",
            identity: RemoteIdentity::HostManaged,
            rights: &destination_rights,
        });
        let plan = |path| {
            plan_streamed_transfer(
                source,
                "report.pdf",
                destination,
                path,
                Transfer::CopyFiles,
                false,
            )
        };
        assert_eq!(plan("../inbox"), Err(TransferHold::InvalidRelativePath));
        assert_eq!(
            plan("inbox/report.pdf"),
            Err(TransferHold::RequireHumanApproval)
        );
    }

    fn rights(operations: &[Right], require_human_approval: bool) -> Rights {
        Rights {
            operations: operations.iter().copied().collect(),
            require_human_approval,
        }
    }

    #[test]
    fn rejects_ambiguous_paths_with_no_backend() {
        for path in [
            "",
            "/a",
            "../a",
            "a/../b",
            "a//b",
            "a/",
            r"\\server\share",
            "C:foo",
            "report.txt:stream",
            "a/COM1.txt",
            "a/.",
            "a. ",
            "a/%2e",
        ] {
            assert!(!is_relative_resource_path(path), "{path}");
        }
        assert!(is_relative_resource_path("reports/Week 2.csv"));
    }

    #[test]
    fn move_needs_source_removal_and_approval_on_either_side() {
        use Right::*;

        let mut source = rights(&[MoveFiles, ReadFile], false);
        let destination = rights(&[MoveFiles, CreateFile], true);
        assert_eq!(
            check_streamed_transfer(&source, &destination, Transfer::MoveFiles, false),
            Decision::Denied
        );
        source.operations.insert(DeleteFiles);
        assert_eq!(
            check_streamed_transfer(&source, &destination, Transfer::MoveFiles, false),
            Decision::RequireHumanApproval
        );
        assert_eq!(
            check_streamed_transfer(&source, &destination, Transfer::MoveFiles, true),
            Decision::Denied
        );
    }

    #[test]
    fn recursive_copy_requires_rights_at_both_ends() {
        use Right::*;

        let source = rights(
            &[CopyDirectory, ListDirectory, ReadFile, RecursiveCopy],
            false,
        );
        let mut destination = rights(&[CopyDirectory, CreateDirectory, CreateFile], false);
        let copy = Transfer::CopyDirectory { recursive: true };
        assert_eq!(
            check_streamed_transfer(&source, &destination, copy, false),
            Decision::Denied
        );
        destination.operations.insert(RecursiveCopy);
        assert_eq!(
            check_streamed_transfer(&source, &destination, copy, false),
            Decision::Allowed
        );
    }
}
