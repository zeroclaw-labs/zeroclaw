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
    mount_id: String,
    protocol: MountedProtocol,
    expected_source: String,
}

#[derive(Debug, Eq, PartialEq)]
pub struct DirectoryListing {
    pub names: Vec<String>,
    pub truncated: bool,
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
        let bound = Self {
            root,
            mount_id: mount_id.to_owned(),
            protocol,
            expected_source: expected_source.to_owned(),
        };
        bound.verify_mount()?;
        Ok(bound)
    }

    fn verify_mount(&self) -> io::Result<()> {
        let mountinfo = std::fs::read_to_string("/proc/self/mountinfo")?;
        if !matches_mount(
            &mountinfo,
            &self.mount_id,
            self.protocol,
            &self.expected_source,
        ) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "mount identity mismatch",
            ));
        }
        Ok(())
    }

    pub fn metadata(&self, path: &str) -> io::Result<Metadata> {
        self.verify_mount()?;
        File::from(open_beneath(&self.root, path, OFlags::PATH)?).metadata()
    }

    pub fn open_regular(&self, path: &str) -> io::Result<File> {
        self.verify_mount()?;
        open_regular(&self.root, path)
    }

    /// List a bounded sample. None lists the authorized root. Unsupported
    /// file names are skipped but still count against the scan limit.
    pub fn list(&self, path: Option<&str>, limit: usize) -> io::Result<DirectoryListing> {
        self.verify_mount()?;
        list_beneath(&self.root, path, limit)
    }
}

fn open_regular(root: impl AsFd, path: &str) -> io::Result<File> {
    let handle = File::from(open_beneath(root, path, OFlags::PATH)?);
    if !handle.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    // Reopen the verified inode through the process-owned descriptor.
    File::open(format!("/proc/self/fd/{}", handle.as_raw_fd()))
}

fn list_beneath(root: impl AsFd, path: Option<&str>, limit: usize) -> io::Result<DirectoryListing> {
    if !(1..=1024).contains(&limit) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid listing limit",
        ));
    }
    let root = root.as_fd();
    let directory = match path {
        Some(path) => open_beneath(root, path, OFlags::RDONLY | OFlags::DIRECTORY)?,
        None => fs::openat(
            root,
            ".",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )?,
    };
    let mut listing = DirectoryListing {
        names: Vec::new(),
        truncated: false,
    };
    let mut scanned = 0;
    for entry in fs::Dir::read_from(&directory)? {
        let entry = entry?;
        let name = entry.file_name();
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        if scanned == limit {
            listing.truncated = true;
            break;
        }
        scanned += 1;
        if let Ok(name) = name.to_str()
            && is_relative_resource_path(name)
        {
            listing.names.push(name.to_owned());
        }
    }
    Ok(listing)
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
        let mut value = String::new();
        open_regular(&root_dir, "report.txt")?.read_to_string(&mut value)?;
        assert_eq!(value, "demo");
        assert!(open_beneath(&root_dir, "escape/self", OFlags::PATH).is_err());
        assert!(open_beneath(&root_dir, "../outside", OFlags::PATH).is_err());
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
    #[test]
    fn listing_is_bounded_and_fails_when_mount_binding_disappears() -> io::Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::create_dir(root.path().join("reports"))?;
        std::fs::write(root.path().join("reports/a"), "")?;
        std::fs::write(root.path().join("reports/b"), "")?;
        std::os::unix::fs::symlink("reports", root.path().join("link"))?;
        let handle = File::open(root.path())?;
        let listing = list_beneath(&handle, Some("reports"), 1)?;
        assert_eq!(listing.names.len(), 1);
        assert!(listing.truncated);
        assert!(list_beneath(&handle, Some("link"), 1).is_err());
        assert!(list_beneath(&handle, None, 0).is_err());

        let unavailable = MountedRoot {
            root: handle.into(),
            mount_id: String::from("0"),
            protocol: MountedProtocol::Smb,
            expected_source: String::from("//files.test/reports"),
        };
        assert_eq!(
            unavailable.list(None, 1).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        Ok(())
    }
}
