//! Windows key files retain one exclusive handle from protected creation to
//! no-replace publication. Identity comes from the process token, not an account
//! name or a path-based permission helper.

use anyhow::{Context, Result};
use std::fs::File;
use std::io;
use std::mem::{offset_of, size_of};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::time::Duration;
use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SHARING_VIOLATION, HANDLE};
use windows::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, AddAccessAllowedAce, GetLengthSid, GetTokenInformation,
    InitializeAcl, InitializeSecurityDescriptor, IsValidSid, PSECURITY_DESCRIPTOR, PSID,
    SE_DACL_PROTECTED, SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR, SetSecurityDescriptorControl,
    SetSecurityDescriptorDacl, SetSecurityDescriptorOwner, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows::Win32::Storage::FileSystem::{
    CREATE_NEW, CreateFileW, DELETE, FILE_ALL_ACCESS, FILE_ATTRIBUTE_NORMAL, FILE_DISPOSITION_INFO,
    FILE_GENERIC_WRITE, FILE_READ_ATTRIBUTES, FILE_RENAME_INFO, FILE_SHARE_MODE,
    FileDispositionInfo, FileRenameInfo, GetVolumeInformationByHandleW, SetFileInformationByHandle,
};
use windows::Win32::System::SystemServices::{FILE_PERSISTENT_ACLS, SECURITY_DESCRIPTOR_REVISION};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::PCWSTR;

// Each imported fallible Win32 binding returns an HRESULT made by from_win32.
// Decode that saved code rather than consult a potentially changed last error.
fn win32<T>(result: windows::core::Result<T>) -> io::Result<T> {
    result.map_err(|error| io::Error::from_raw_os_error(error.code().0 & 0xffff))
}

// Vec<u8> does not promise alignment for TOKEN_USER, ACL or FILE_RENAME_INFO.
// Their alignment is no larger than usize on either supported Windows ABI.
struct AlignedBuffer(Vec<usize>);

impl AlignedBuffer {
    fn new(bytes: usize) -> Self {
        Self(vec![0; bytes.div_ceil(size_of::<usize>())])
    }

    fn as_mut_ptr<T>(&mut self) -> *mut T {
        self.0.as_mut_ptr().cast()
    }
}

struct CurrentUser {
    token_user: AlignedBuffer,
}

impl CurrentUser {
    fn load() -> io::Result<Self> {
        let mut token = HANDLE::default();
        // SAFETY: the pseudo process handle is valid and token is writable.
        win32(unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) })?;
        // SAFETY: OpenProcessToken returned an owned, valid handle.
        let token = unsafe { OwnedHandle::from_raw_handle(token.0) };
        let token_handle = HANDLE(token.as_raw_handle());
        let mut bytes = 0;
        // SAFETY: this is the documented size query, without an output buffer.
        let probe =
            win32(unsafe { GetTokenInformation(token_handle, TokenUser, None, 0, &mut bytes) });
        if let Err(error) = probe {
            if error.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER.0 as i32) {
                return Err(error);
            }
        }
        if (bytes as usize) < size_of::<TOKEN_USER>() {
            return Err(io::Error::other(
                "Process token returned an invalid user buffer size",
            ));
        }
        let mut token_user = AlignedBuffer::new(bytes as usize);
        let capacity = bytes;
        // SAFETY: the aligned allocation covers capacity bytes and stays alive
        // for every later use of the SID pointing into it.
        win32(unsafe {
            GetTokenInformation(
                token_handle,
                TokenUser,
                Some(token_user.as_mut_ptr()),
                capacity,
                &mut bytes,
            )
        })?;
        if bytes > capacity || (bytes as usize) < size_of::<TOKEN_USER>() {
            return Err(io::Error::other(
                "Process token returned an invalid user buffer",
            ));
        }
        let mut user = Self { token_user };
        let sid = user.sid();
        // SAFETY: GetTokenInformation initialized TOKEN_USER and its SID.
        if sid.0.is_null() || !unsafe { IsValidSid(sid) }.as_bool() {
            return Err(io::Error::other(
                "Process token contains an invalid user SID",
            ));
        }
        Ok(user)
    }

    fn sid(&mut self) -> PSID {
        // SAFETY: load verified a full TOKEN_USER in this aligned allocation.
        unsafe { (*self.token_user.as_mut_ptr::<TOKEN_USER>()).User.Sid }
    }
}

struct RestrictedSecurity {
    user: CurrentUser,
    acl: AlignedBuffer,
    descriptor: SECURITY_DESCRIPTOR,
}

impl RestrictedSecurity {
    fn current_user() -> io::Result<Self> {
        let mut user = CurrentUser::load()?;
        let sid = user.sid();
        // SAFETY: CurrentUser validated the SID and owns its backing allocation.
        let sid_bytes = unsafe { GetLengthSid(sid) } as usize;
        let acl_bytes = size_of::<ACL>() + offset_of!(ACCESS_ALLOWED_ACE, SidStart) + sid_bytes;
        let mut acl = AlignedBuffer::new(acl_bytes);
        // SAFETY: the allocation is aligned and covers the ACL plus its one ACE.
        unsafe {
            win32(InitializeAcl(
                acl.as_mut_ptr(),
                acl_bytes as u32,
                ACL_REVISION,
            ))?;
            win32(AddAccessAllowedAce(
                acl.as_mut_ptr(),
                ACL_REVISION,
                FILE_ALL_ACCESS.0,
                sid,
            ))?;
        }
        let mut descriptor = SECURITY_DESCRIPTOR::default();
        let descriptor_ptr =
            PSECURITY_DESCRIPTOR((&mut descriptor as *mut SECURITY_DESCRIPTOR).cast());
        // SAFETY: the descriptor is writable. Its borrowed ACL and owner SID
        // remain alive in RestrictedSecurity until CreateFileW returns.
        unsafe {
            win32(InitializeSecurityDescriptor(
                descriptor_ptr,
                SECURITY_DESCRIPTOR_REVISION,
            ))?;
            win32(SetSecurityDescriptorOwner(descriptor_ptr, Some(sid), false))?;
            win32(SetSecurityDescriptorDacl(
                descriptor_ptr,
                true,
                Some(acl.as_mut_ptr()),
                false,
            ))?;
            win32(SetSecurityDescriptorControl(
                descriptor_ptr,
                SE_DACL_PROTECTED,
                SE_DACL_PROTECTED,
            ))?;
        }
        Ok(Self {
            user,
            acl,
            descriptor,
        })
    }

    fn attributes(&mut self) -> SECURITY_ATTRIBUTES {
        // Refresh borrowed addresses after moving the descriptor's owner.
        self.descriptor.Owner = self.user.sid();
        self.descriptor.Dacl = self.acl.as_mut_ptr();
        SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: (&mut self.descriptor as *mut SECURITY_DESCRIPTOR).cast(),
            bInheritHandle: false.into(),
        }
    }
}

fn wide_path(path: &Path) -> io::Result<Vec<u16>> {
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Key file path contains NUL",
        ));
    }
    wide.push(0);
    Ok(wide)
}

struct PendingKeyFile {
    file: File,
    unpublished: bool,
}

impl PendingKeyFile {
    fn create(path: &Path) -> Result<Self> {
        let mut security = RestrictedSecurity::current_user()
            .context("Failed to build current-user key file security descriptor")?;
        Self::create_with_security(path, &mut security)
            .with_context(|| format!("Failed to create temp key file at {}", path.display()))
    }

    fn create_with_security(path: &Path, security: &mut RestrictedSecurity) -> io::Result<Self> {
        let wide = wide_path(path)?;
        let attributes = security.attributes();
        // SAFETY: the path, descriptor, ACL and SID remain alive through this
        // call. CREATE_NEW never opens an existing object; zero sharing prevents
        // another reader, writer or deleter acquiring it until this handle closes.
        let handle = win32(unsafe {
            CreateFileW(
                PCWSTR(wide.as_ptr()),
                FILE_GENERIC_WRITE.0 | FILE_READ_ATTRIBUTES.0 | DELETE.0,
                FILE_SHARE_MODE(0),
                Some(&attributes),
                CREATE_NEW,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        })?;
        // SAFETY: CreateFileW returned a new owned file handle.
        let file = unsafe { File::from_raw_handle(handle.0) };
        Ok(Self {
            file,
            unpublished: true,
        })
    }

    fn publish(&mut self, destination: &Path) -> io::Result<()> {
        let absolute = std::path::absolute(destination)?;
        let wide = wide_path(&absolute)?;
        // FileName is NUL-terminated; FileNameLength excludes that terminator.
        // Keep it in the buffer rather than relying on allocation padding.
        let name_bytes = u32::try_from((wide.len() - 1).checked_mul(size_of::<u16>()).ok_or_else(
            || io::Error::new(io::ErrorKind::InvalidInput, "Key file path is too long"),
        )?)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Key file path is too long"))?;
        let bytes =
            (offset_of!(FILE_RENAME_INFO, FileName) + name_bytes as usize + size_of::<u16>())
                .max(size_of::<FILE_RENAME_INFO>());
        let bytes_u32 = u32::try_from(bytes).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "Key file path is too long")
        })?;
        let mut buffer = AlignedBuffer::new(bytes);
        let info = buffer.as_mut_ptr::<FILE_RENAME_INFO>();
        // SAFETY: the aligned buffer fits both the fixed header and all UTF-16
        // filename bytes. The API reads it only during this call.
        unsafe {
            info.write(FILE_RENAME_INFO::default());
            (*info).Anonymous.ReplaceIfExists = false;
            (*info).FileNameLength = name_bytes;
            std::ptr::copy_nonoverlapping(
                wide.as_ptr(),
                std::ptr::addr_of_mut!((*info).FileName).cast(),
                wide.len(),
            );
            win32(SetFileInformationByHandle(
                HANDLE(self.file.as_raw_handle()),
                FileRenameInfo,
                info.cast(),
                bytes_u32,
            ))?;
        }
        // Bytes were synced before rename. This API does not establish a
        // stronger crash-durability guarantee for the new directory entry.
        self.unpublished = false;
        Ok(())
    }

    fn abort(&mut self, error: anyhow::Error) -> anyhow::Error {
        match delete_by_handle(&self.file) {
            Ok(()) => {
                self.unpublished = false;
                error
            }
            // Do not retain a typed collision source here. The caller may read
            // the winner only if the unpublished losing key was removed.
            Err(cleanup_error) => anyhow::Error::new(io::Error::other(format!(
                "Failed to delete unpublished key file: {cleanup_error}; original operation failed: {error:#}"
            ))),
        }
    }
}

fn delete_by_handle(file: &File) -> io::Result<()> {
    let info = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: the live file handle has DELETE access; info covers the fixed
    // structure. No path lookup can redirect this cleanup to another object.
    win32(unsafe {
        SetFileInformationByHandle(
            HANDLE(file.as_raw_handle()),
            FileDispositionInfo,
            (&info as *const FILE_DISPOSITION_INFO).cast(),
            size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    })
}

fn require_persistent_acls(flags: u32) -> io::Result<()> {
    if flags & FILE_PERSISTENT_ACLS == 0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Key file filesystem does not support persistent ACLs",
        ));
    }
    Ok(())
}

fn verify_persistent_acls(file: &File) -> io::Result<()> {
    let mut flags = 0;
    // SAFETY: this live file handle has FILE_READ_ATTRIBUTES access. The flags
    // output is writable; the unneeded volume and filesystem names are omitted.
    win32(unsafe {
        GetVolumeInformationByHandleW(
            HANDLE(file.as_raw_handle()),
            None,
            None,
            None,
            Some(&mut flags),
            None,
        )
    })?;
    require_persistent_acls(flags)
}

impl Drop for PendingKeyFile {
    fn drop(&mut self) {
        if self.unpublished {
            // Normal errors report deletion failure via abort. Drop also covers
            // panics and retries failed deletion while retaining the same handle.
            if let Err(error) = delete_by_handle(&self.file) {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    &format!("Failed to delete unpublished key file by handle: {error}")
                );
            }
        }
    }
}

pub(super) fn write_and_publish_with(
    temp_path: &Path,
    key_path: &Path,
    bytes: &[u8],
    write: impl FnOnce(&mut File, &[u8]) -> io::Result<()>,
) -> Result<()> {
    let mut pending = PendingKeyFile::create(temp_path)?;
    // Some filesystems ignore creation security attributes. Refuse to write any
    // key bytes unless the volume owning this same handle supports stored ACLs.
    if let Err(error) = verify_persistent_acls(&pending.file)
        .context("Failed to verify persistent ACL support for key file")
    {
        return Err(pending.abort(error));
    }
    if let Err(error) =
        write(&mut pending.file, bytes).context("Failed to write key data to temp file")
    {
        return Err(pending.abort(error));
    }
    if let Err(error) = pending.publish(key_path) {
        let context = if error.kind() == io::ErrorKind::AlreadyExists {
            "Key file already exists — another process created it concurrently"
        } else {
            "Failed to atomically publish key file"
        };
        return Err(pending.abort(anyhow::Error::new(error).context(context)));
    }
    // Return closes the exclusive handle immediately after successful rename.
    Ok(())
}

pub(super) fn open_existing_with_retry(
    mut open: impl FnMut() -> io::Result<File>,
) -> io::Result<File> {
    // A complete key briefly has its final name while the publisher still owns
    // its exclusive handle. Retry only that sharing error, never access denial.
    const ATTEMPTS: usize = 50;
    let mut result = open();
    for _ in 1..ATTEMPTS {
        match &result {
            Err(error) if error.raw_os_error() == Some(ERROR_SHARING_VIOLATION.0 as i32) => {
                std::thread::sleep(Duration::from_millis(10));
                result = open();
            }
            _ => break,
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::Foundation::{
        ERROR_ACCESS_DENIED, ERROR_SUCCESS, GetHandleInformation, HANDLE_FLAG_INHERIT, HLOCAL,
        LocalFree,
    };
    use windows::Win32::Security::Authorization::{
        GetSecurityInfo, SE_FILE_OBJECT, SetNamedSecurityInfoW,
    };
    use windows::Win32::Security::{
        AddAccessAllowedAceEx, CONTAINER_INHERIT_ACE, CreateWellKnownSid,
        DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetSecurityDescriptorControl,
        OBJECT_INHERIT_ACE, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
        SE_DACL_PRESENT, SECURITY_MAX_SID_SIZE, WinWorldSid,
    };
    use windows::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_READONLY, FILE_BASIC_INFO,
        FILE_WRITE_ATTRIBUTES, FileBasicInfo, GetFileInformationByHandle, READ_CONTROL,
    };
    use windows::Win32::System::SystemServices::ACCESS_ALLOWED_ACE_TYPE;

    struct LocalDescriptor(PSECURITY_DESCRIPTOR);

    impl Drop for LocalDescriptor {
        fn drop(&mut self) {
            // SAFETY: GetSecurityInfo allocated this descriptor with LocalAlloc.
            unsafe { LocalFree(Some(HLOCAL(self.0.0))) };
        }
    }

    fn assert_restrictive_descriptor(file: &File) {
        let mut owner = PSID::default();
        let mut dacl = std::ptr::null_mut();
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // SAFETY: the file is live and each output location is writable.
        let status = unsafe {
            GetSecurityInfo(
                HANDLE(file.as_raw_handle()),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                Some(&mut owner),
                None,
                Some(&mut dacl),
                None,
                Some(&mut descriptor),
            )
        };
        assert_eq!(status, ERROR_SUCCESS);
        let descriptor = LocalDescriptor(descriptor);
        let mut user = CurrentUser::load().unwrap();
        let mut control = 0;
        let mut revision = 0;
        // SAFETY: all pointers below refer to this allocated descriptor or the
        // owned TokenUser buffer, and remain alive until the assertions finish.
        unsafe {
            assert!(!owner.0.is_null());
            EqualSid(owner, user.sid()).unwrap();
            GetSecurityDescriptorControl(descriptor.0, &mut control, &mut revision).unwrap();
            assert_ne!(control & SE_DACL_PRESENT.0, 0);
            assert_ne!(control & SE_DACL_PROTECTED.0, 0);
            assert!(!dacl.is_null());
            assert_eq!((*dacl).AceCount, 1, "no inherited or extra ACE is allowed");
            let mut ace = std::ptr::null_mut();
            GetAce(dacl, 0, &mut ace).unwrap();
            let ace = ace.cast::<ACCESS_ALLOWED_ACE>();
            assert_eq!((*ace).Header.AceType, ACCESS_ALLOWED_ACE_TYPE as u8);
            assert_eq!((*ace).Header.AceFlags, 0, "the ACE must not inherit");
            assert_eq!((*ace).Mask, FILE_ALL_ACCESS.0);
            EqualSid(
                PSID(std::ptr::addr_of_mut!((*ace).SidStart).cast()),
                user.sid(),
            )
            .unwrap();
        }
    }

    fn make_inheritable_permissive_parent(path: &Path) {
        let mut world = AlignedBuffer::new(SECURITY_MAX_SID_SIZE as usize);
        let mut sid_bytes = SECURITY_MAX_SID_SIZE;
        let sid = PSID(world.as_mut_ptr());
        let mut acl = AlignedBuffer::new(
            size_of::<ACL>()
                + offset_of!(ACCESS_ALLOWED_ACE, SidStart)
                + SECURITY_MAX_SID_SIZE as usize,
        );
        let acl_bytes = (acl.0.len() * size_of::<usize>()) as u32;
        let wide = wide_path(path).unwrap();
        // SAFETY: both aligned buffers cover their maximum sizes; all borrowed
        // pointers remain alive until SetNamedSecurityInfoW copies the ACL.
        unsafe {
            CreateWellKnownSid(WinWorldSid, None, Some(sid), &mut sid_bytes).unwrap();
            InitializeAcl(acl.as_mut_ptr(), acl_bytes, ACL_REVISION).unwrap();
            AddAccessAllowedAceEx(
                acl.as_mut_ptr(),
                ACL_REVISION,
                OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE,
                FILE_ALL_ACCESS.0,
                sid,
            )
            .unwrap();
            let status = SetNamedSecurityInfoW(
                PCWSTR(wide.as_ptr()),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                None,
                None,
                Some(acl.as_mut_ptr()),
                None,
            );
            assert_eq!(status, ERROR_SUCCESS);
        }
    }

    fn file_identity(file: &File) -> (u32, u32, u32) {
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        // SAFETY: this live handle and fixed-size output are valid.
        unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }.unwrap();
        (
            info.dwVolumeSerialNumber,
            info.nFileIndexHigh,
            info.nFileIndexLow,
        )
    }

    #[test]
    fn protected_at_birth_under_permissive_parent_and_same_identity_after_publish() {
        let dir = tempfile::tempdir().unwrap();
        make_inheritable_permissive_parent(dir.path());
        let key = [17; 32];
        // Consecutive UTF-16 lengths exercise every usize-alignment phase.
        for suffix in ["", "a", "ab", "abc"] {
            let path = dir.path().join(format!(".secret_key{suffix}"));
            let mut identity = None;
            super::super::write_key_file_atomic_publish_with(&path, &key, |file, bytes| {
                assert_eq!(
                    file.metadata()?.len(),
                    0,
                    "inspect before any key bytes are written"
                );
                assert_restrictive_descriptor(file);
                let mut handle_flags = 0;
                // SAFETY: this handle is live and the output is writable.
                win32(unsafe {
                    GetHandleInformation(HANDLE(file.as_raw_handle()), &mut handle_flags)
                })?;
                assert_eq!(handle_flags & HANDLE_FLAG_INHERIT.0, 0);
                identity = Some(file_identity(file));
                file.write_all(bytes)?;
                file.flush()?;
                file.sync_all()
            })
            .unwrap();
            let published = super::super::open_no_follow(&path).unwrap();
            assert_eq!(Some(file_identity(&published)), identity);
            assert_restrictive_descriptor(&published);
            drop(published);
            assert_eq!(super::super::load_or_create_key(&path).unwrap(), key);
        }
        assert!(super::super::tests::no_temp_residue(dir.path()));
    }

    #[test]
    fn filesystem_without_persistent_acls_is_rejected() {
        for flags in [0, !FILE_PERSISTENT_ACLS] {
            let error = require_persistent_acls(flags).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        }
        require_persistent_acls(FILE_PERSISTENT_ACLS).unwrap();
    }

    #[test]
    fn exclusive_handle_blocks_read_write_delete_and_rename() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key.tmp");
        let pending = PendingKeyFile::create(&path).unwrap();
        let errors = [
            File::open(&path).unwrap_err(),
            std::fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .unwrap_err(),
            std::fs::remove_file(&path).unwrap_err(),
            std::fs::rename(&path, dir.path().join("moved")).unwrap_err(),
        ];
        for error in errors {
            assert_eq!(error.raw_os_error(), Some(ERROR_SHARING_VIOLATION.0 as i32));
        }
        drop(pending);
        assert!(!path.exists());
    }

    #[test]
    fn rejected_security_descriptor_never_creates_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key.tmp");
        let mut security = RestrictedSecurity::current_user().unwrap();
        // A malformed ACL exercises a real CreateFileW descriptor rejection,
        // without manufacturing an invalid pointer or invoking the writer.
        unsafe { (*security.acl.as_mut_ptr::<ACL>()).AclRevision = 0 };
        assert!(PendingKeyFile::create_with_security(&path, &mut security).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn publish_collision_preserves_winner_and_cleans_loser() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".secret_key");
        let winner = [19; 32];
        super::super::write_key_file(&path, &winner).unwrap();
        let error = super::super::write_key_file(&path, &[23; 32]).unwrap_err();
        assert!(super::super::is_already_exists_error(&error));
        assert_eq!(super::super::load_or_create_key(&path).unwrap(), winner);
        assert!(super::super::tests::no_temp_residue(dir.path()));
    }

    fn set_readonly(file: &File, readonly: bool) {
        let info = FILE_BASIC_INFO {
            FileAttributes: if readonly {
                FILE_ATTRIBUTE_READONLY.0
            } else {
                FILE_ATTRIBUTE_NORMAL.0
            },
            ..Default::default()
        };
        // SAFETY: the file has attribute-write access and info covers its struct.
        win32(unsafe {
            SetFileInformationByHandle(
                HANDLE(file.as_raw_handle()),
                FileBasicInfo,
                (&info as *const FILE_BASIC_INFO).cast(),
                size_of::<FILE_BASIC_INFO>() as u32,
            )
        })
        .unwrap();
    }

    #[test]
    fn cleanup_failure_is_reported_without_collision_recovery_and_residue_stays_private() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key.tmp");
        let mut pending = PendingKeyFile::create(&path).unwrap();
        pending.file.write_all(b"partial key").unwrap();
        set_readonly(&pending.file, true); // Windows refuses disposition of read-only files.
        let collision = anyhow::Error::new(io::Error::from(io::ErrorKind::AlreadyExists));
        let error = pending.abort(collision);
        assert!(
            error
                .to_string()
                .contains("Failed to delete unpublished key file")
        );
        assert!(!super::super::is_already_exists_error(&error));
        assert_restrictive_descriptor(&pending.file);
        drop(pending); // The panic-path retry also fails on the read-only object.
        assert!(path.exists());
        // Attribute-only access can clear read-only without requesting data writes.
        let residue = std::fs::OpenOptions::new()
            .access_mode(FILE_WRITE_ATTRIBUTES.0 | READ_CONTROL.0)
            .open(&path)
            .unwrap();
        assert_restrictive_descriptor(&residue);
        set_readonly(&residue, false);
        drop(residue);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn read_retries_published_but_still_exclusive_handle() {
        let dir = tempfile::tempdir().unwrap();
        let temp = dir.path().join("key.tmp");
        let path = dir.path().join(".secret_key");
        let key = [29; 32];
        let mut pending = PendingKeyFile::create(&temp).unwrap();
        pending
            .file
            .write_all(super::super::hex_encode(&key).as_bytes())
            .unwrap();
        pending.file.sync_all().unwrap();
        pending.publish(&path).unwrap();
        assert_eq!(
            File::open(&path).unwrap_err().raw_os_error(),
            Some(ERROR_SHARING_VIOLATION.0 as i32)
        );
        let (observed_tx, observed_rx) = std::sync::mpsc::channel();
        let reader_path = path.clone();
        let reader = std::thread::spawn(move || {
            let mut first_attempt = true;
            let opened = open_existing_with_retry(|| {
                let result = File::open(&reader_path);
                if first_attempt {
                    assert_eq!(
                        result.as_ref().unwrap_err().raw_os_error(),
                        Some(ERROR_SHARING_VIOLATION.0 as i32)
                    );
                    observed_tx.send(()).unwrap();
                    first_attempt = false;
                }
                result
            })
            .unwrap();
            drop(opened);
            super::super::load_or_create_key(&reader_path).unwrap()
        });
        observed_rx.recv().unwrap();
        drop(pending);
        assert_eq!(reader.join().unwrap(), key);
    }

    #[test]
    fn read_retry_is_bounded_and_never_retries_access_denial() {
        for code in [ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION] {
            let mut calls = 0;
            let error = open_existing_with_retry(|| {
                calls += 1;
                Err(io::Error::from_raw_os_error(code.0 as i32))
            })
            .unwrap_err();
            assert_eq!(error.raw_os_error(), Some(code.0 as i32));
            assert_eq!(
                calls,
                if code == ERROR_SHARING_VIOLATION {
                    50
                } else {
                    1
                }
            );
        }
    }
}
