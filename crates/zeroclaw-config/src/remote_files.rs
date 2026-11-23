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

#[cfg(test)]
mod tests {
    use super::*;

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
