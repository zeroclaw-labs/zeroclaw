//! Linux adapter for operator-mounted SMB and NFS roots.

use rustix::fs::{self, Mode, OFlags, ResolveFlags};
use std::fs::{File, Metadata};
use std::io;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use zeroclaw_config::remote_files::is_relative_resource_path;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MountedProtocol {
    Smb,
    Nfs,
}

pub struct MountedRoot {
    root: OwnedFd,
}

impl MountedRoot {
    /// Bind a directory handle acquired from an authorized operator resource.
    /// Verify the mount in this process's namespace before accepting it.
    pub fn bind(
        root: OwnedFd,
        protocol: MountedProtocol,
        expected_source: &str,
    ) -> io::Result<Self> {
        if expected_source.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "missing mount source",
            ));
        }
        if fs::FileType::from_raw_mode(fs::fstat(&root)?.st_mode) != fs::FileType::Directory {
            return Err(io::ErrorKind::NotADirectory.into());
        }
        let fdinfo = std::fs::read_to_string(format!("/proc/self/fdinfo/{}", root.as_raw_fd()))?;
        let mount_id = fdinfo
            .lines()
            .find_map(|line| line.strip_prefix("mnt_id:"))
            .map(str::trim)
            .ok_or(io::ErrorKind::InvalidData)?;
        let mountinfo = std::fs::read_to_string("/proc/self/mountinfo")?;
        if !matches_mount(&mountinfo, mount_id, protocol, expected_source) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "mount identity mismatch",
            ));
        }
        Ok(Self { root })
    }

    pub fn metadata(&self, path: &str) -> io::Result<Metadata> {
        Ok(File::from(open_beneath(&self.root, path, OFlags::PATH)?).metadata()?)
    }

    /// Open an ordinary file without activating special files. Reopen the
    /// verified inode through its process-owned descriptor, not its name.
    pub fn open_regular(&self, path: &str) -> io::Result<File> {
        let handle = File::from(open_beneath(&self.root, path, OFlags::PATH)?);
        if !handle.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a regular file",
            ));
        }
        File::open(format!("/proc/self/fd/{}", handle.as_raw_fd()))
    }
}

fn open_beneath(root: impl AsFd, path: &str, flags: OFlags) -> io::Result<OwnedFd> {
    if !is_relative_resource_path(path) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    fs::openat2(
        root,
        path,
        flags | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_XDEV,
    )
    .map_err(Into::into)
}

fn matches_mount(info: &str, id: &str, protocol: MountedProtocol, source: &str) -> bool {
    info.lines().any(|line| {
        let Some((header, tail)) = line.split_once(" - ") else {
            return false;
        };
        if header.split_whitespace().next() != Some(id) {
            return false;
        }
        let mut fields = tail.split_whitespace();
        let kind_matches = match protocol {
            MountedProtocol::Smb => matches!(fields.next(), Some("cifs" | "smb3")),
            MountedProtocol::Nfs => matches!(fields.next(), Some("nfs" | "nfs4")),
        };
        kind_matches
            && fields.next() == Some(source)
            && (protocol != MountedProtocol::Smb
                || fields.next().is_some_and(|options| {
                    options
                        .split(',')
                        .any(|option| matches!(option, "vers=3.0" | "vers=3.02" | "vers=3.1.1"))
                }))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_binding_requires_id_protocol_and_source() {
        let info = "40 20 0:42 / /mnt/reports ro - cifs //files.test/reports ro,vers=3.1.1\n41 20 0:43 / /mnt/nfs ro - nfs4 files.test:/exports ro";
        assert!(matches_mount(
            info,
            "40",
            MountedProtocol::Smb,
            "//files.test/reports"
        ));
        assert!(matches_mount(
            info,
            "41",
            MountedProtocol::Nfs,
            "files.test:/exports"
        ));
        assert!(!matches_mount(
            info,
            "41",
            MountedProtocol::Smb,
            "//files.test/reports"
        ));
        assert!(!matches_mount(
            info,
            "40",
            MountedProtocol::Smb,
            "//other.test/reports"
        ));
    }

    #[test]
    fn contained_open_refuses_link_and_parent_escape() -> io::Result<()> {
        use std::io::Read;
        let root = tempfile::tempdir()?;
        std::fs::write(root.path().join("report.txt"), "demo")?;
        std::os::unix::fs::symlink("/proc", root.path().join("escape"))?;
        let root_dir = File::open(root.path())?;
        // Bind directly for the containment fixture, which is not an SMB/NFS mount.
        let adapter = MountedRoot {
            root: root_dir.try_clone()?.into(),
        };
        let mut value = String::new();
        adapter
            .open_regular("report.txt")?
            .read_to_string(&mut value)?;
        assert_eq!(value, "demo");
        assert!(adapter.metadata("escape/self").is_err());
        assert!(adapter.metadata("../outside").is_err());
        assert!(
            MountedRoot::bind(
                root_dir.into(),
                MountedProtocol::Smb,
                "//files.test/reports"
            )
            .is_err()
        );
        Ok(())
    }
}
