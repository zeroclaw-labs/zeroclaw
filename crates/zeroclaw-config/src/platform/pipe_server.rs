//! Who serves a Windows named pipe, as the kernel reports it.
//!
//! A pipe's name is predictable and a pipe carries no peer credential, so a
//! client about to send a secret over one first asks the kernel which process
//! created the server end and which account that process runs under. The
//! answer type is portable, so a caller's decision can be tested anywhere; the
//! queries themselves exist on Windows only.

/// The account a process runs under, relative to this process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessAccount {
    /// The user this process runs as.
    ThisUser,
    /// The local system account.
    System,
    /// Any other account.
    Other,
}

#[cfg(windows)]
pub use win32::{process_account, server_process_id};

#[cfg(windows)]
mod win32 {
    use super::ProcessAccount;
    use std::io;
    use std::mem::size_of;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, HANDLE};
    use windows::Win32::Security::{
        GetLengthSid, GetTokenInformation, IsValidSid, IsWellKnownSid, PSID, TOKEN_QUERY,
        TOKEN_USER, TokenUser, WinLocalSystemSid,
    };
    use windows::Win32::System::Pipes::GetNamedPipeServerProcessId;
    use windows::Win32::System::Threading::{
        GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    /// The id of the process that created the server end of `pipe`, an open
    /// client handle.
    pub fn server_process_id(pipe: &impl AsRawHandle) -> io::Result<u32> {
        let mut pid = 0u32;
        // SAFETY: `pipe` keeps its handle open for the call, and `pid` is a
        // valid out-pointer.
        unsafe { GetNamedPipeServerProcessId(HANDLE(pipe.as_raw_handle()), &mut pid) }
            .map_err(|error| io::Error::other(format!("GetNamedPipeServerProcessId: {error}")))?;
        Ok(pid)
    }

    /// The account process `pid` runs under, read from its token's user and
    /// compared with this process's. The token user, not the token owner: an
    /// elevated token's owner defaults to the Administrators group while its
    /// user stays the account.
    pub fn process_account(pid: u32) -> io::Result<ProcessAccount> {
        // SAFETY: OpenProcess reads no memory of this process.
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
            .map_err(|error| io::Error::other(format!("OpenProcess({pid}): {error}")))?;
        // SAFETY: OpenProcess returned an owned, valid handle.
        let process = unsafe { OwnedHandle::from_raw_handle(process.0) };
        let server = TokenUserSid::read(HANDLE(process.as_raw_handle()))?;
        // SAFETY: the current-process pseudo handle is always valid and is
        // never closed.
        let own = TokenUserSid::read(unsafe { GetCurrentProcess() })?;
        Ok(if server.bytes() == own.bytes() {
            ProcessAccount::ThisUser
        } else if server.is_local_system() {
            ProcessAccount::System
        } else {
            ProcessAccount::Other
        })
    }

    /// A process token's `TOKEN_USER`, in an allocation aligned for it:
    /// `Vec<u8>` promises no alignment, and the structure's is no larger than
    /// `usize` on either supported Windows ABI.
    struct TokenUserSid(Vec<usize>);

    impl TokenUserSid {
        fn read(process: HANDLE) -> io::Result<Self> {
            let mut token = HANDLE::default();
            // SAFETY: `process` is open for the call and `token` is writable.
            unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) }
                .map_err(|error| io::Error::other(format!("OpenProcessToken: {error}")))?;
            // SAFETY: OpenProcessToken returned an owned, valid handle.
            let token = unsafe { OwnedHandle::from_raw_handle(token.0) };
            let token_handle = HANDLE(token.as_raw_handle());
            let mut bytes = 0u32;
            // SAFETY: the documented size query, without an output buffer.
            let probe =
                unsafe { GetTokenInformation(token_handle, TokenUser, None, 0, &mut bytes) };
            if let Err(error) = probe
                && error.code() != ERROR_INSUFFICIENT_BUFFER.to_hresult()
            {
                return Err(io::Error::other(format!("GetTokenInformation: {error}")));
            }
            if (bytes as usize) < size_of::<TOKEN_USER>() {
                return Err(io::Error::other(
                    "the process token reported an invalid user buffer size",
                ));
            }
            let capacity = bytes;
            let mut buffer = vec![0usize; (capacity as usize).div_ceil(size_of::<usize>())];
            // SAFETY: the aligned allocation covers `capacity` bytes and stays
            // alive for every later use of the SID pointing into it.
            unsafe {
                GetTokenInformation(
                    token_handle,
                    TokenUser,
                    Some(buffer.as_mut_ptr().cast()),
                    capacity,
                    &mut bytes,
                )
            }
            .map_err(|error| io::Error::other(format!("GetTokenInformation: {error}")))?;
            if bytes > capacity || (bytes as usize) < size_of::<TOKEN_USER>() {
                return Err(io::Error::other(
                    "the process token returned an invalid user buffer",
                ));
            }
            let user = Self(buffer);
            let sid = user.sid();
            // SAFETY: GetTokenInformation initialized TOKEN_USER and its SID.
            if sid.0.is_null() || !unsafe { IsValidSid(sid) }.as_bool() {
                return Err(io::Error::other(
                    "the process token holds an invalid user SID",
                ));
            }
            Ok(user)
        }

        fn sid(&self) -> PSID {
            // SAFETY: `read` verified a full TOKEN_USER in this aligned
            // allocation.
            unsafe { (*self.0.as_ptr().cast::<TOKEN_USER>()).User.Sid }
        }

        /// The SID's bytes; two accounts are the same when these are equal.
        fn bytes(&self) -> &[u8] {
            let sid = self.sid();
            // SAFETY: `read` verified the SID, whose length its header gives,
            // and the SID lives in this allocation for as long as `self`.
            unsafe {
                let length = GetLengthSid(sid) as usize;
                std::slice::from_raw_parts(sid.0.cast::<u8>().cast_const(), length)
            }
        }

        fn is_local_system(&self) -> bool {
            // SAFETY: `read` verified the SID.
            unsafe { IsWellKnownSid(self.sid(), WinLocalSystemSid) }.as_bool()
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use tokio::net::windows::named_pipe::{ClientOptions, ServerOptions};

    #[tokio::test]
    async fn the_server_of_a_pipe_this_process_serves_is_this_process() {
        let name = format!(r"\\.\pipe\zeroclaw-config-{}", std::process::id());
        let _server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&name)
            .expect("create the pipe");

        let client = ClientOptions::new().open(&name).expect("open the pipe");
        assert_eq!(
            server_process_id(&client).expect("the kernel names the server"),
            std::process::id()
        );
    }

    #[test]
    fn this_process_runs_as_this_user() {
        assert_eq!(
            process_account(std::process::id()).expect("this process's token is readable"),
            ProcessAccount::ThisUser
        );
    }

    #[test]
    fn a_child_process_runs_as_this_user() {
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "ping -n 30 127.0.0.1 >NUL"])
            .spawn()
            .expect("spawn a long-lived child");
        let account = process_account(child.id());
        child.kill().expect("terminate the child");
        child.wait().expect("reap the child");
        assert_eq!(
            account.expect("the child's token is readable"),
            ProcessAccount::ThisUser
        );
    }
}
