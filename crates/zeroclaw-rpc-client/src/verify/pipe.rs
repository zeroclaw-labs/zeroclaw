//! Who serves a Windows named pipe, decided from what the kernel reports.
//!
//! Three facts, all read from the kernel and none from the pipe's server:
//!
//! - the process that created the pipe's server end
//!   (`GetNamedPipeServerProcessId`), and the user its token names, which
//!   must be the expected account;
//! - the pipe's owner, which must be that account or the Administrators
//!   group (the owner an elevated process gives the objects it creates). A
//!   process can make only its own user, or a group it holds the owner right
//!   for, the owner of what it creates, so another account that is not an
//!   administrator cannot produce a pipe owned by this one. This also holds
//!   when the process that created the pipe has exited and its ID now names
//!   some other process;
//! - the pipe's access list. Windows checks a new server instance of an
//!   existing pipe against that pipe's access list, and the instance's
//!   creator does not become the owner, so the owner alone does not prove
//!   who made the instance this client reached. Only the expected account,
//!   `SYSTEM`, the Administrators group and `CREATOR OWNER` may hold the
//!   right to add instances or to change the pipe's security; any other
//!   account or group holding either fails the check. A broad group
//!   (everyone, anonymous, or every signed-in user) may not even write to
//!   the pipe. A pipe with no access list lets every account do all of that.
//!
//! Where the line falls:
//!
//! - an administrator is outside the boundary. An elevated process of any
//!   administrator account creates pipes the Administrators group owns, and
//!   those pass the owner check;
//! - another named account or group may be granted plain read and write
//!   access to the pipe. That lets it connect as one more client, not serve
//!   under the pipe's name, so it is the owner's choice to make.
//!
//! The decision is written against [`PipeServer`], so it runs on every
//! platform under test; only the Windows implementation calls the operating
//! system.

use super::EndpointRejection;
use std::fmt;

/// A Windows security identifier, read from its binary form.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Sid {
    authority: u64,
    sub_authorities: Vec<u32>,
}

/// The only SID revision Windows defines.
const SID_REVISION: u8 = 1;
/// `SID_MAX_SUB_AUTHORITIES`.
const SID_MAX_SUB_AUTHORITIES: usize = 15;

impl Sid {
    pub(crate) fn new(authority: u64, sub_authorities: &[u32]) -> Self {
        Self {
            authority,
            sub_authorities: sub_authorities.to_vec(),
        }
    }

    /// Read a SID from the start of `bytes`, with the number of bytes it
    /// takes. `None` when the bytes are not a well-formed SID.
    pub(crate) fn read(bytes: &[u8]) -> Option<(Self, usize)> {
        let (&revision, rest) = bytes.split_first()?;
        let (&count, rest) = rest.split_first()?;
        let count = usize::from(count);
        if revision != SID_REVISION || count > SID_MAX_SUB_AUTHORITIES {
            return None;
        }
        // The identifier authority is six bytes, most significant first.
        let authority = rest
            .get(..6)?
            .iter()
            .fold(0u64, |value, byte| (value << 8) | u64::from(*byte));
        let sub_authorities = rest
            .get(6..6 + 4 * count)?
            .as_chunks::<4>()
            .0
            .iter()
            .map(|word| u32::from_le_bytes(*word))
            .collect();
        Some((
            Self {
                authority,
                sub_authorities,
            },
            8 + 4 * count,
        ))
    }

    /// The binary form [`Self::read`] reads.
    #[cfg(test)]
    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = vec![SID_REVISION, self.sub_authorities.len() as u8];
        bytes.extend_from_slice(&self.authority.to_be_bytes()[2..]);
        for sub in &self.sub_authorities {
            bytes.extend_from_slice(&sub.to_le_bytes());
        }
        bytes
    }
}

impl fmt::Display for Sid {
    /// The documented string form: `S-1-<authority>-<sub>...`, with the
    /// authority in hex when it does not fit in 32 bits.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.authority < 1 << 32 {
            write!(f, "S-{SID_REVISION}-{}", self.authority)?;
        } else {
            write!(f, "S-{SID_REVISION}-0x{:012X}", self.authority)?;
        }
        for sub in &self.sub_authorities {
            write!(f, "-{sub}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Sid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// `BUILTIN\Administrators`: the owner an elevated process gives the objects
/// it creates.
fn administrators() -> Sid {
    Sid::new(5, &[32, 544])
}

/// `NT AUTHORITY\SYSTEM`, the account the operating system runs as.
fn system() -> Sid {
    Sid::new(5, &[18])
}

/// `CREATOR OWNER`, a placeholder Windows replaces with the creator's
/// account when an object inherits the entry.
fn creator_owner() -> Sid {
    Sid::new(3, &[0])
}

/// Groups so broad that access granted to them is access granted to other
/// accounts.
fn broad_groups() -> [Sid; 9] {
    [
        Sid::new(1, &[0]),       // Everyone
        Sid::new(2, &[0]),       // LOCAL
        Sid::new(2, &[1]),       // CONSOLE LOGON
        Sid::new(5, &[2]),       // NETWORK
        Sid::new(5, &[4]),       // INTERACTIVE
        Sid::new(5, &[7]),       // ANONYMOUS LOGON
        Sid::new(5, &[11]),      // Authenticated Users
        Sid::new(5, &[32, 545]), // BUILTIN\Users
        Sid::new(5, &[32, 546]), // BUILTIN\Guests
    ]
}

/// Access that lets its holder create a server instance of its own under the
/// pipe's name, or change who may. `GENERIC_WRITE` is here because on a pipe
/// it maps to `FILE_GENERIC_WRITE`, which includes the instance right.
const CONTROL_ACCESS: u32 = 0x0000_0004 // FILE_CREATE_PIPE_INSTANCE
    | 0x0004_0000 // WRITE_DAC
    | 0x0008_0000 // WRITE_OWNER
    | 0x1000_0000 // GENERIC_ALL
    | 0x4000_0000; // GENERIC_WRITE

/// Access that lets its holder write to the pipe, or any of
/// [`CONTROL_ACCESS`].
const WRITE_ACCESS: u32 = 0x0000_0002 // FILE_WRITE_DATA
    | CONTROL_ACCESS;

const SE_DACL_PRESENT: u16 = 0x0004;
const SE_SELF_RELATIVE: u16 = 0x8000;
const ACCESS_ALLOWED_ACE_TYPE: u8 = 0x0;
const ACCESS_DENIED_ACE_TYPE: u8 = 0x1;
const ACCESS_ALLOWED_CALLBACK_ACE_TYPE: u8 = 0x9;
const ACCESS_DENIED_CALLBACK_ACE_TYPE: u8 = 0xA;
const INHERIT_ONLY_ACE: u8 = 0x08;

/// Access an access list grants one account.
struct Grant {
    sid: Sid,
    access: u32,
}

/// The parts of a pipe's security descriptor this check reads.
struct PipeSecurity {
    owner: Option<Sid>,
    /// `None` when the pipe has no access list at all.
    grants: Option<Vec<Grant>>,
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*bytes.get(at)?, *bytes.get(at + 1)?]))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    let word = bytes.get(at..at + 4)?;
    Some(u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
}

/// Read the owner and the granting entries of a self-relative security
/// descriptor, the form `GetKernelObjectSecurity` returns.
fn parse_security_descriptor(descriptor: &[u8]) -> Result<PipeSecurity, String> {
    let malformed = || "the pipe's security descriptor is malformed".to_string();
    if descriptor.first() != Some(&1) {
        return Err(malformed());
    }
    let control = u16_at(descriptor, 2).ok_or_else(malformed)?;
    if control & SE_SELF_RELATIVE == 0 {
        return Err(malformed());
    }
    let owner_at = u32_at(descriptor, 4).ok_or_else(malformed)? as usize;
    let dacl_at = u32_at(descriptor, 16).ok_or_else(malformed)? as usize;
    let owner = match owner_at {
        0 => None,
        at => Some(
            descriptor
                .get(at..)
                .and_then(Sid::read)
                .ok_or_else(malformed)?
                .0,
        ),
    };
    let grants = if control & SE_DACL_PRESENT == 0 || dacl_at == 0 {
        None
    } else {
        Some(parse_grants(
            descriptor.get(dacl_at..).ok_or_else(malformed)?,
        )?)
    };
    Ok(PipeSecurity { owner, grants })
}

/// The entries of an access list that grant access to the object itself.
/// Denying entries only take access away, and inherit-only entries apply to
/// children a pipe does not have, so neither is returned. An entry of a kind
/// this check cannot read fails the whole check.
fn parse_grants(acl: &[u8]) -> Result<Vec<Grant>, String> {
    let malformed = || "the pipe's access list is malformed".to_string();
    let size = usize::from(u16_at(acl, 2).ok_or_else(malformed)?);
    let count = u16_at(acl, 4).ok_or_else(malformed)?;
    let acl = acl.get(..size).ok_or_else(malformed)?;
    let mut at = 8;
    let mut grants = Vec::new();
    for _ in 0..count {
        let kind = *acl.get(at).ok_or_else(malformed)?;
        let flags = *acl.get(at + 1).ok_or_else(malformed)?;
        let length = usize::from(u16_at(acl, at + 2).ok_or_else(malformed)?);
        if length < 8 {
            return Err(malformed());
        }
        let entry = acl.get(at..at + length).ok_or_else(malformed)?;
        at += length;
        if flags & INHERIT_ONLY_ACE != 0 {
            continue;
        }
        match kind {
            ACCESS_ALLOWED_ACE_TYPE | ACCESS_ALLOWED_CALLBACK_ACE_TYPE => {
                let access = u32_at(entry, 4).ok_or_else(malformed)?;
                let (sid, _) = entry.get(8..).and_then(Sid::read).ok_or_else(malformed)?;
                grants.push(Grant { sid, access });
            }
            ACCESS_DENIED_ACE_TYPE | ACCESS_DENIED_CALLBACK_ACE_TYPE => {}
            other => {
                return Err(format!(
                    "the pipe's access list has an entry of type {other:#x} this check cannot read"
                ));
            }
        }
    }
    Ok(grants)
}

/// The owner and access list of a pipe whose server runs as `expected`.
fn check_pipe_security(descriptor: &[u8], expected: &Sid) -> Result<(), EndpointRejection> {
    let security =
        parse_security_descriptor(descriptor).map_err(EndpointRejection::PipeSecurityUnreadable)?;
    let owner = security.owner.ok_or_else(|| {
        EndpointRejection::PipeSecurityUnreadable("the pipe reports no owner".to_string())
    })?;
    if owner != *expected && owner != administrators() {
        return Err(EndpointRejection::PipeOwner {
            expected: expected.to_string(),
            actual: owner.to_string(),
        });
    }
    let Some(grants) = security.grants else {
        return Err(EndpointRejection::PipeUnprotected);
    };
    // Instance and security control is an allowlist: no list of the accounts
    // that must not hold it can be complete, so name the few that may.
    let may_control = [
        expected.clone(),
        system(),
        administrators(),
        creator_owner(),
    ];
    let broad = broad_groups();
    if let Some(grant) = grants.iter().find(|grant| {
        (grant.access & CONTROL_ACCESS != 0 && !may_control.contains(&grant.sid))
            || (grant.access & WRITE_ACCESS != 0 && broad.contains(&grant.sid))
    }) {
        return Err(EndpointRejection::PipeWritable {
            account: grant.sid.to_string(),
            access: grant.access,
        });
    }
    Ok(())
}

/// What Windows reports about the server end of a pipe this client holds
/// open.
pub(crate) trait PipeServer {
    /// An open process, closed when dropped.
    type Process;
    /// The user this client process runs as.
    fn current_user(&self) -> Result<Sid, String>;
    /// The ID of the process that created the pipe's server end.
    fn server_process_id(&self) -> Result<u32, String>;
    /// Open process `pid` to read its token.
    fn open_process(&self, pid: u32) -> Result<Self::Process, String>;
    /// The user `process`'s token names.
    fn process_user(&self, process: &Self::Process) -> Result<Sid, String>;
    /// The pipe's owner and access list, as a self-relative security
    /// descriptor.
    fn security_descriptor(&self) -> Result<Vec<u8>, String>;
}

/// Prove the pipe `server` describes is served by `expected` (this
/// process's own user when `None`), that no other account owns it or may add
/// instances to it or change its security, and that no broad group may
/// write to it. Any query the kernel refuses fails the check.
pub(crate) fn verify_pipe_server<P: PipeServer>(
    server: &P,
    expected: Option<Sid>,
) -> Result<(), EndpointRejection> {
    let unknown =
        |what: String, error: String| EndpointRejection::PeerUnknown(format!("{what}: {error}"));
    let expected = match expected {
        Some(sid) => sid,
        None => server
            .current_user()
            .map_err(|error| unknown("this process's account".to_string(), error))?,
    };
    let pid = server
        .server_process_id()
        .map_err(|error| unknown("the pipe's server process".to_string(), error))?;
    let process = server
        .open_process(pid)
        .map_err(|error| unknown(format!("server process {pid}"), error))?;
    let actual = server
        .process_user(&process)
        .map_err(|error| unknown(format!("the account of server process {pid}"), error))?;
    drop(process);
    if actual != expected {
        return Err(EndpointRejection::ServerAccount {
            pid,
            expected: expected.to_string(),
            actual: actual.to_string(),
        });
    }
    let descriptor = server
        .security_descriptor()
        .map_err(EndpointRejection::PipeSecurityUnreadable)?;
    check_pipe_security(&descriptor, &expected)
}

#[cfg(windows)]
pub(crate) use win32::Win32Pipe;

#[cfg(windows)]
mod win32 {
    use super::{PipeServer, Sid};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, GetKernelObjectSecurity, GetLengthSid, GetTokenInformation,
        IsValidSid, OWNER_SECURITY_INFORMATION, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    /// The client end of a pipe, borrowed for the length of the check.
    pub(crate) struct Win32Pipe {
        handle: HANDLE,
    }

    impl Win32Pipe {
        pub(crate) fn new(handle: std::os::windows::io::RawHandle) -> Self {
            Self { handle }
        }
    }

    /// A process or token handle the check opened, closed when dropped.
    pub(crate) struct OwnedHandle(HANDLE);

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            // SAFETY: the handle came open from `OpenProcess` or
            // `OpenProcessToken` and is closed exactly once, here.
            unsafe { CloseHandle(self.0) };
        }
    }

    fn last_error(call: &str) -> String {
        format!("{call} failed: {}", std::io::Error::last_os_error())
    }

    /// The user the token of `process` names.
    fn token_user(process: HANDLE) -> Result<Sid, String> {
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: `process` is an open process handle or this process's
        // pseudo-handle, and `token` is a valid place for the result.
        if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
            return Err(last_error("OpenProcessToken"));
        }
        let token = OwnedHandle(token);
        let mut needed = 0u32;
        // SAFETY: a size query: no buffer, length 0, and a valid place for
        // the size Windows needs.
        unsafe { GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &mut needed) };
        if needed == 0 {
            return Err(last_error("GetTokenInformation"));
        }
        // `TOKEN_USER` starts with a pointer, so the buffer is pointer-aligned.
        let mut buffer = vec![0usize; (needed as usize).div_ceil(std::mem::size_of::<usize>())];
        // SAFETY: `buffer` is writable for at least `needed` bytes and
        // aligned for `TOKEN_USER`.
        if unsafe {
            GetTokenInformation(
                token.0,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                needed,
                &mut needed,
            )
        } == 0
        {
            return Err(last_error("GetTokenInformation"));
        }
        // SAFETY: on success the buffer holds a `TOKEN_USER` whose SID points
        // into the same buffer, which outlives every read below.
        let sid = unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid };
        // SAFETY: `sid` points at the SID Windows wrote into `buffer`.
        if unsafe { IsValidSid(sid) } == 0 {
            return Err("the token's user SID is malformed".to_string());
        }
        // SAFETY: a valid SID is exactly `GetLengthSid` bytes long, all of
        // them inside `buffer`.
        let bytes = unsafe {
            std::slice::from_raw_parts(sid.cast::<u8>().cast_const(), GetLengthSid(sid) as usize)
        };
        Sid::read(bytes)
            .map(|(sid, _)| sid)
            .ok_or_else(|| "the token's user SID is malformed".to_string())
    }

    impl PipeServer for Win32Pipe {
        type Process = OwnedHandle;

        fn current_user(&self) -> Result<Sid, String> {
            // SAFETY: `GetCurrentProcess` returns a pseudo-handle that needs
            // no closing.
            token_user(unsafe { GetCurrentProcess() })
        }

        fn server_process_id(&self) -> Result<u32, String> {
            let mut pid = 0u32;
            // SAFETY: `self.handle` is the open client end of a pipe,
            // borrowed for this call, and `pid` is a valid place for the ID.
            if unsafe { GetNamedPipeServerProcessId(self.handle, &mut pid) } == 0 {
                return Err(last_error("GetNamedPipeServerProcessId"));
            }
            Ok(pid)
        }

        fn open_process(&self, pid: u32) -> Result<OwnedHandle, String> {
            // SAFETY: no pointer arguments; a null result is handled below.
            let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
            if handle.is_null() {
                Err(last_error("OpenProcess"))
            } else {
                Ok(OwnedHandle(handle))
            }
        }

        fn process_user(&self, process: &OwnedHandle) -> Result<Sid, String> {
            token_user(process.0)
        }

        fn security_descriptor(&self) -> Result<Vec<u8>, String> {
            let wanted = OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
            let mut needed = 0u32;
            // SAFETY: a size query: no buffer, length 0, and a valid place
            // for the size Windows needs. The client end was opened for
            // reading, which includes the right to read its security.
            unsafe {
                GetKernelObjectSecurity(self.handle, wanted, std::ptr::null_mut(), 0, &mut needed)
            };
            if needed == 0 {
                return Err(last_error("GetKernelObjectSecurity"));
            }
            // A security descriptor is read in 32-bit fields, so the buffer
            // is 32-bit aligned.
            let mut buffer = vec![0u32; (needed as usize).div_ceil(4)];
            // SAFETY: `buffer` is writable for at least `needed` bytes.
            if unsafe {
                GetKernelObjectSecurity(
                    self.handle,
                    wanted,
                    buffer.as_mut_ptr().cast(),
                    needed,
                    &mut needed,
                )
            } == 0
            {
                return Err(last_error("GetKernelObjectSecurity"));
            }
            let mut descriptor: Vec<u8> =
                buffer.iter().flat_map(|word| word.to_ne_bytes()).collect();
            descriptor.truncate(needed as usize);
            Ok(descriptor)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn me() -> Sid {
        Sid::new(5, &[21, 1_004_336_348, 1_177_238_915, 682_003_330, 1001])
    }

    fn someone_else() -> Sid {
        Sid::new(5, &[21, 1_004_336_348, 1_177_238_915, 682_003_330, 1002])
    }

    /// A group of this machine or domain that is none of the broad ones.
    fn custom_group() -> Sid {
        Sid::new(5, &[21, 1_004_336_348, 1_177_238_915, 682_003_330, 2100])
    }

    fn everyone() -> Sid {
        Sid::new(1, &[0])
    }

    const GENERIC_ALL: u32 = 0x1000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const WRITE_DAC: u32 = 0x0004_0000;
    const WRITE_OWNER: u32 = 0x0008_0000;
    const FILE_CREATE_PIPE_INSTANCE: u32 = 0x0000_0004;
    const FILE_GENERIC_READ: u32 = 0x0012_0089;
    /// What an ordinary client needs: read and write the pipe's data and
    /// attributes, without the right to add instances.
    const CLIENT_READ_WRITE: u32 = 0x0012_019b;

    /// One access list entry: its type, flags, access mask and account.
    type Entry = (u8, u8, u32, Sid);

    fn allow(access: u32, sid: Sid) -> Entry {
        (ACCESS_ALLOWED_ACE_TYPE, 0, access, sid)
    }

    /// A self-relative security descriptor with `owner` and, unless `None`,
    /// an access list of `entries`.
    fn descriptor(owner: Option<&Sid>, entries: Option<&[Entry]>) -> Vec<u8> {
        let owner_bytes = owner.map(Sid::to_bytes).unwrap_or_default();
        let acl = entries.map(|entries| {
            let mut body = Vec::new();
            for (kind, flags, access, sid) in entries {
                let sid = sid.to_bytes();
                body.push(*kind);
                body.push(*flags);
                body.extend_from_slice(&((8 + sid.len()) as u16).to_le_bytes());
                body.extend_from_slice(&access.to_le_bytes());
                body.extend_from_slice(&sid);
            }
            let mut acl = vec![2, 0];
            acl.extend_from_slice(&((8 + body.len()) as u16).to_le_bytes());
            acl.extend_from_slice(&(entries.len() as u16).to_le_bytes());
            acl.extend_from_slice(&[0, 0]);
            acl.extend_from_slice(&body);
            acl
        });
        let owner_at = if owner.is_some() { 20 } else { 0 };
        let dacl_at = if acl.is_some() {
            20 + owner_bytes.len()
        } else {
            0
        };
        let control = SE_SELF_RELATIVE | if acl.is_some() { SE_DACL_PRESENT } else { 0 };
        let mut bytes = vec![1, 0];
        bytes.extend_from_slice(&control.to_le_bytes());
        bytes.extend_from_slice(&(owner_at as u32).to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&(dacl_at as u32).to_le_bytes());
        bytes.extend_from_slice(&owner_bytes);
        bytes.extend_from_slice(&acl.unwrap_or_default());
        bytes
    }

    /// The access list a pipe gets by default: full control for its owner,
    /// `SYSTEM` and administrators, and read access for everyone and the
    /// anonymous account.
    fn default_entries(owner: &Sid) -> Vec<Entry> {
        vec![
            allow(GENERIC_ALL, owner.clone()),
            allow(GENERIC_ALL, system()),
            allow(GENERIC_ALL, administrators()),
            allow(FILE_GENERIC_READ, everyone()),
            allow(FILE_GENERIC_READ, Sid::new(5, &[7])),
        ]
    }

    /// A pipe as the kernel would describe it.
    struct FakePipe {
        current: Result<Sid, String>,
        pid: Result<u32, String>,
        open: Result<(), String>,
        server_user: Result<Sid, String>,
        descriptor: Result<Vec<u8>, String>,
    }

    impl FakePipe {
        /// A pipe this account serves, owns and keeps to itself.
        fn mine() -> Self {
            Self {
                current: Ok(me()),
                pid: Ok(4242),
                open: Ok(()),
                server_user: Ok(me()),
                descriptor: Ok(descriptor(Some(&me()), Some(&default_entries(&me())))),
            }
        }
    }

    impl PipeServer for FakePipe {
        type Process = u32;

        fn current_user(&self) -> Result<Sid, String> {
            self.current.clone()
        }

        fn server_process_id(&self) -> Result<u32, String> {
            self.pid.clone()
        }

        fn open_process(&self, pid: u32) -> Result<u32, String> {
            self.open.clone().map(|()| pid)
        }

        fn process_user(&self, _process: &u32) -> Result<Sid, String> {
            self.server_user.clone()
        }

        fn security_descriptor(&self) -> Result<Vec<u8>, String> {
            self.descriptor.clone()
        }
    }

    #[test]
    fn a_sid_reads_back_in_its_string_form() {
        let sid = me();
        let mut bytes = sid.to_bytes();
        bytes.extend_from_slice(b"trailing");
        assert_eq!(Sid::read(&bytes), Some((sid.clone(), 28)));
        assert_eq!(
            sid.to_string(),
            "S-1-5-21-1004336348-1177238915-682003330-1001"
        );
        assert_eq!(everyone().to_string(), "S-1-1-0");
        assert_eq!(Sid::new(1 << 40, &[7]).to_string(), "S-1-0x010000000000-7");
        assert_eq!(
            Sid::read(&Sid::new(1 << 40, &[7]).to_bytes()).map(|(sid, _)| sid),
            Some(Sid::new(1 << 40, &[7]))
        );
    }

    #[test]
    fn malformed_sids_are_not_read() {
        let good = me().to_bytes();
        let mut revision_two = good.clone();
        revision_two[0] = 2;
        let mut too_many = good.clone();
        too_many[1] = 16;
        for bytes in [
            &[][..],
            &good[..1],
            &good[..7],
            &good[..good.len() - 1],
            &revision_two[..],
            &too_many[..],
        ] {
            assert_eq!(Sid::read(bytes), None, "{bytes:?}");
        }
    }

    #[test]
    fn a_pipe_this_account_serves_owns_and_keeps_to_itself_passes() {
        assert_eq!(verify_pipe_server(&FakePipe::mine(), None), Ok(()));
    }

    #[test]
    fn a_pipe_an_elevated_process_of_this_account_created_passes() {
        let pipe = FakePipe {
            descriptor: Ok(descriptor(
                Some(&administrators()),
                Some(&default_entries(&administrators())),
            )),
            ..FakePipe::mine()
        };
        assert_eq!(verify_pipe_server(&pipe, None), Ok(()));
    }

    #[test]
    fn a_pipe_another_account_serves_is_refused_and_both_accounts_are_named() {
        let pipe = FakePipe {
            server_user: Ok(someone_else()),
            ..FakePipe::mine()
        };
        let rejection = verify_pipe_server(&pipe, None).expect_err("another account");
        assert_eq!(
            rejection,
            EndpointRejection::ServerAccount {
                pid: 4242,
                expected: me().to_string(),
                actual: someone_else().to_string(),
            }
        );
        assert_eq!(
            rejection.to_string(),
            "served by process 4242 running as \
             S-1-5-21-1004336348-1177238915-682003330-1002, expected \
             S-1-5-21-1004336348-1177238915-682003330-1001"
        );
    }

    #[test]
    fn an_expected_account_stands_in_for_this_processs_own() {
        let pipe = FakePipe::mine();
        assert_eq!(verify_pipe_server(&pipe, Some(me())), Ok(()));
        assert_eq!(
            verify_pipe_server(&pipe, Some(someone_else())),
            Err(EndpointRejection::ServerAccount {
                pid: 4242,
                expected: someone_else().to_string(),
                actual: me().to_string(),
            })
        );
    }

    #[test]
    fn a_pipe_another_account_owns_is_refused_even_when_this_account_serves_it() {
        // The process the pipe names runs as this account, but the pipe was
        // created by another: its creator exited and the ID was reused, or
        // the creator handed its server end on.
        let pipe = FakePipe {
            descriptor: Ok(descriptor(
                Some(&someone_else()),
                Some(&default_entries(&someone_else())),
            )),
            ..FakePipe::mine()
        };
        assert_eq!(
            verify_pipe_server(&pipe, None),
            Err(EndpointRejection::PipeOwner {
                expected: me().to_string(),
                actual: someone_else().to_string(),
            })
        );
    }

    #[test]
    fn a_broad_group_that_may_write_add_instances_or_change_security_is_refused() {
        for group in broad_groups() {
            for access in [
                0x0000_0002,
                0x0000_0004,
                0x0004_0000,
                0x0008_0000,
                GENERIC_ALL,
                0x4000_0000,
                FILE_GENERIC_READ | 0x0000_0004,
            ] {
                let mut entries = default_entries(&me());
                entries.push(allow(access, group.clone()));
                let pipe = FakePipe {
                    descriptor: Ok(descriptor(Some(&me()), Some(&entries))),
                    ..FakePipe::mine()
                };
                assert_eq!(
                    verify_pipe_server(&pipe, None),
                    Err(EndpointRejection::PipeWritable {
                        account: group.to_string(),
                        access,
                    }),
                    "{group} {access:#x}"
                );
            }
        }
    }

    #[test]
    fn entries_that_grant_nothing_on_the_pipe_itself_are_ignored() {
        let mut entries = default_entries(&me());
        entries.push((ACCESS_DENIED_ACE_TYPE, 0, GENERIC_ALL, everyone()));
        for grantee in [everyone(), someone_else()] {
            entries.push((
                ACCESS_ALLOWED_ACE_TYPE,
                INHERIT_ONLY_ACE,
                GENERIC_ALL,
                grantee,
            ));
        }
        let pipe = FakePipe {
            descriptor: Ok(descriptor(Some(&me()), Some(&entries))),
            ..FakePipe::mine()
        };
        assert_eq!(verify_pipe_server(&pipe, None), Ok(()));
    }

    #[test]
    fn another_account_or_group_that_may_add_instances_or_change_security_is_refused() {
        for grantee in [someone_else(), custom_group()] {
            for access in [
                FILE_CREATE_PIPE_INSTANCE,
                WRITE_DAC,
                WRITE_OWNER,
                GENERIC_ALL,
                GENERIC_WRITE,
                CLIENT_READ_WRITE | FILE_CREATE_PIPE_INSTANCE,
            ] {
                let mut entries = default_entries(&me());
                entries.push(allow(access, grantee.clone()));
                let pipe = FakePipe {
                    descriptor: Ok(descriptor(Some(&me()), Some(&entries))),
                    ..FakePipe::mine()
                };
                assert_eq!(
                    verify_pipe_server(&pipe, None),
                    Err(EndpointRejection::PipeWritable {
                        account: grantee.to_string(),
                        access,
                    }),
                    "{grantee} {access:#x}"
                );
            }
        }
    }

    #[test]
    fn another_account_or_group_may_read_and_write_as_a_client() {
        for grantee in [someone_else(), custom_group()] {
            for access in [FILE_GENERIC_READ, CLIENT_READ_WRITE] {
                let mut entries = default_entries(&me());
                entries.push(allow(access, grantee.clone()));
                let pipe = FakePipe {
                    descriptor: Ok(descriptor(Some(&me()), Some(&entries))),
                    ..FakePipe::mine()
                };
                assert_eq!(
                    verify_pipe_server(&pipe, None),
                    Ok(()),
                    "{grantee} {access:#x}"
                );
            }
        }
    }

    #[test]
    fn this_account_system_administrators_and_creator_owner_may_hold_control() {
        for holder in [me(), system(), administrators(), creator_owner()] {
            for access in [
                FILE_CREATE_PIPE_INSTANCE,
                WRITE_DAC,
                WRITE_OWNER,
                GENERIC_ALL,
            ] {
                let pipe = FakePipe {
                    descriptor: Ok(descriptor(
                        Some(&me()),
                        Some(&[allow(access, holder.clone())]),
                    )),
                    ..FakePipe::mine()
                };
                assert_eq!(
                    verify_pipe_server(&pipe, None),
                    Ok(()),
                    "{holder} {access:#x}"
                );
            }
        }
    }

    /// The account check passes whenever the server process ID names a
    /// process of this account, which also happens when the pipe's creator
    /// exited and its ID was reused. A pipe this account owns, whose access
    /// list let another account add an instance under its name, would then
    /// hand that account the credential: the instance's creator never
    /// becomes the pipe's owner.
    fn reused_process_pipe(grantee: Sid, access: u32) -> FakePipe {
        let entries = [
            allow(GENERIC_ALL, me()),
            allow(access | CLIENT_READ_WRITE, grantee),
        ];
        FakePipe {
            descriptor: Ok(descriptor(Some(&me()), Some(&entries))),
            ..FakePipe::mine()
        }
    }

    #[test]
    fn a_named_account_with_instance_or_security_control_is_refused_after_pid_reuse() {
        let accepted: Vec<u32> = [
            FILE_CREATE_PIPE_INSTANCE,
            WRITE_DAC,
            WRITE_OWNER,
            GENERIC_ALL,
        ]
        .into_iter()
        .filter(|access| {
            verify_pipe_server(&reused_process_pipe(someone_else(), *access), None).is_ok()
        })
        .collect();
        assert!(
            accepted.is_empty(),
            "control granted to another account was accepted: {accepted:#x?}"
        );
    }

    #[test]
    fn a_custom_group_with_full_control_is_refused_after_pid_reuse() {
        assert!(!broad_groups().contains(&custom_group()));
        let outcome = verify_pipe_server(&reused_process_pipe(custom_group(), GENERIC_ALL), None);
        assert!(
            outcome.is_err(),
            "full control for a custom group was accepted: {outcome:?}"
        );
    }

    /// The structural comparison of [`Sid`] agrees with the operating
    /// system's own `EqualSid` on the forms this check meets.
    #[cfg(windows)]
    #[test]
    fn sid_equality_matches_equal_sid() {
        use windows_sys::Win32::Security::{EqualSid, IsValidSid};
        let sids = [
            me(),
            someone_else(),
            custom_group(),
            system(),
            administrators(),
            creator_owner(),
            everyone(),
            Sid::new(5, &[21, 1_004_336_348, 1_177_238_915, 682_003_330]),
            Sid::new(1 << 40, &[7]),
            Sid::new(16, &[8192]),
        ];
        for left in &sids {
            for right in &sids {
                let mut left_bytes = left.to_bytes();
                let mut right_bytes = right.to_bytes();
                // SAFETY: both buffers hold a complete SID in its binary
                // form and outlive the calls.
                let native = unsafe {
                    assert_ne!(IsValidSid(left_bytes.as_mut_ptr().cast()), 0, "{left}");
                    assert_ne!(IsValidSid(right_bytes.as_mut_ptr().cast()), 0, "{right}");
                    EqualSid(
                        left_bytes.as_mut_ptr().cast(),
                        right_bytes.as_mut_ptr().cast(),
                    ) != 0
                };
                let read_left = Sid::read(&left_bytes).map(|(sid, _)| sid);
                let read_right = Sid::read(&right_bytes).map(|(sid, _)| sid);
                assert_eq!(read_left == read_right, native, "{left} and {right}");
            }
        }
    }

    #[test]
    fn a_callback_grant_counts_as_a_grant() {
        let mut entries = default_entries(&me());
        entries.push((ACCESS_ALLOWED_CALLBACK_ACE_TYPE, 0, GENERIC_ALL, everyone()));
        let pipe = FakePipe {
            descriptor: Ok(descriptor(Some(&me()), Some(&entries))),
            ..FakePipe::mine()
        };
        assert_eq!(
            verify_pipe_server(&pipe, None),
            Err(EndpointRejection::PipeWritable {
                account: everyone().to_string(),
                access: GENERIC_ALL,
            })
        );
    }

    #[test]
    fn a_pipe_with_no_access_list_is_refused() {
        let pipe = FakePipe {
            descriptor: Ok(descriptor(Some(&me()), None)),
            ..FakePipe::mine()
        };
        assert_eq!(
            verify_pipe_server(&pipe, None),
            Err(EndpointRejection::PipeUnprotected)
        );
    }

    #[test]
    fn an_access_list_entry_the_check_cannot_read_is_refused() {
        // An object entry (type 5) carries fields before its SID that this
        // check does not parse; it fails rather than guesses.
        let mut entries = default_entries(&me());
        entries.push((0x5, 0, GENERIC_ALL, everyone()));
        let pipe = FakePipe {
            descriptor: Ok(descriptor(Some(&me()), Some(&entries))),
            ..FakePipe::mine()
        };
        match verify_pipe_server(&pipe, None) {
            Err(EndpointRejection::PipeSecurityUnreadable(error)) => {
                assert!(error.contains("type 0x5"), "{error}");
            }
            other => panic!("expected an unreadable access list, got {other:?}"),
        }
    }

    #[test]
    fn a_malformed_or_ownerless_descriptor_is_refused() {
        let good = descriptor(Some(&me()), Some(&default_entries(&me())));
        let mut not_self_relative = good.clone();
        not_self_relative[3] = 0;
        let mut owner_out_of_range = good.clone();
        owner_out_of_range[4..8].copy_from_slice(&9999u32.to_le_bytes());
        let mut acl_overrun = good.clone();
        let acl_at = 20 + me().to_bytes().len();
        acl_overrun[acl_at + 2..acl_at + 4].copy_from_slice(&u16::MAX.to_le_bytes());
        let ownerless = descriptor(None, Some(&default_entries(&me())));
        for (case, bytes) in [
            ("empty", Vec::new()),
            ("truncated header", good[..12].to_vec()),
            ("truncated access list", good[..good.len() - 3].to_vec()),
            ("absolute form", not_self_relative),
            ("owner offset out of range", owner_out_of_range),
            ("access list larger than the descriptor", acl_overrun),
            ("no owner", ownerless),
        ] {
            let pipe = FakePipe {
                descriptor: Ok(bytes),
                ..FakePipe::mine()
            };
            assert!(
                matches!(
                    verify_pipe_server(&pipe, None),
                    Err(EndpointRejection::PipeSecurityUnreadable(_))
                ),
                "{case}"
            );
        }
    }

    #[test]
    fn every_query_the_kernel_refuses_fails_the_check() {
        fn denied<T>() -> Result<T, String> {
            Err("Access is denied. (os error 5)".to_string())
        }
        let cases: [(&str, FakePipe); 4] = [
            (
                "this process's account",
                FakePipe {
                    current: denied(),
                    ..FakePipe::mine()
                },
            ),
            (
                "the pipe's server process",
                FakePipe {
                    pid: denied(),
                    ..FakePipe::mine()
                },
            ),
            (
                "server process 4242",
                FakePipe {
                    open: denied(),
                    ..FakePipe::mine()
                },
            ),
            (
                "the account of server process 4242",
                FakePipe {
                    server_user: denied(),
                    ..FakePipe::mine()
                },
            ),
        ];
        for (what, pipe) in cases {
            assert_eq!(
                verify_pipe_server(&pipe, None),
                Err(EndpointRejection::PeerUnknown(format!(
                    "{what}: Access is denied. (os error 5)"
                ))),
                "{what}"
            );
        }
        let pipe = FakePipe {
            descriptor: denied(),
            ..FakePipe::mine()
        };
        assert_eq!(
            verify_pipe_server(&pipe, None),
            Err(EndpointRejection::PipeSecurityUnreadable(
                "Access is denied. (os error 5)".to_string()
            ))
        );
    }
}
