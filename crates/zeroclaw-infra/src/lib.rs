//! Channel infrastructure: session backends, debouncing, and stall watchdog.
//! These are cross-cutting utilities used by multiple channel implementations.

pub mod conversation_owners;
pub mod cost_ledger;
pub mod debounce;
pub mod net_guard;
pub mod session_events;
pub mod session_queue;
pub mod session_repository;
pub mod stall_watchdog;

/// Explicit bridge for pre-event-store protocol test harnesses. Shipped
/// entrances must use the admitted execution port for transcript mutations.
#[macro_export]
macro_rules! legacy_fixture_call {
    ($repository:expr, $method:ident($($argument:expr),* $(,)?)) => {
        $crate::legacy_fixture_call!($repository, $method($($argument),*) => ())
    };
    ($repository:expr, $method:ident($($argument:expr),* $(,)?) => $result:ty) => {{
        #[cfg(any(test, feature = "test-util"))]
        { $repository.$method($($argument),*) }
        #[cfg(not(any(test, feature = "test-util")))]
        {
            let _ = (&$repository, $(&$argument),*);
            Err::<$result, anyhow::Error>(anyhow::Error::msg("legacy transcript operations require the session execution port"))
        }
    }};
}

use std::net::SocketAddr;
use std::path::Path;

pub fn effective_gateway_bind_socket_addr(host: &str, port: u16) -> SocketAddr {
    parse_gateway_bind_socket_addr(host, port)
        .unwrap_or_else(|_| fallback_gateway_bind_socket_addr(port))
}

pub fn parse_gateway_bind_socket_addr(
    host: &str,
    port: u16,
) -> Result<SocketAddr, std::net::AddrParseError> {
    format!("{host}:{port}").parse()
}

pub fn fallback_gateway_bind_socket_addr(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}

/// Data directories whose startup recovery this process has already claimed,
/// by resolved location, so two spellings of one directory are one claim.
static STARTUP_RECOVERY_CLAIMED: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashSet<std::path::PathBuf>>,
> = std::sync::LazyLock::new(Default::default);

/// Settle the session turns a previous process left marked "running" under
/// `data_dir`, and return how many there were.
///
/// The caller must hold process ownership of `data_dir` and must not have
/// admitted a turn yet. The first time a process owns a data directory is the
/// one moment it knows no turn is running there: whoever marked them is gone.
/// Each such turn is recorded as "error" with its history kept, and nothing
/// is restarted.
///
/// It acts once per process for a directory. Every later call returns `Ok(0)`
/// without looking, because by then a running turn may be this process's own.
/// That holds after a failed attempt too: the error is returned once, and the
/// turns stay as they are until the next process start.
///
/// Run state lives only in the SQLite store, so that store is settled whenever
/// it exists, whichever backend is configured now: a later reload may switch
/// to it. It is neither created nor migrated for this.
pub fn recover_abandoned_session_turns(data_dir: &Path) -> std::io::Result<usize> {
    let identity = std::fs::canonicalize(data_dir).unwrap_or_else(|_| data_dir.to_path_buf());
    if !STARTUP_RECOVERY_CLAIMED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(identity)
    {
        return Ok(0);
    }
    let recovered = session_events::upgrade::readers::recover_abandoned_turns_at(data_dir)?;
    if recovered > 0 {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({ "sessions": recovered })),
            "marked session turns left running by a previous process as failed"
        );
    }
    Ok(recovered)
}

#[cfg(test)]
mod tests {
    #[test]
    fn recovery_creates_no_store() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert_eq!(
            super::recover_abandoned_session_turns(tmp.path()).unwrap(),
            0
        );
        assert!(!tmp.path().join("sessions").exists());
    }
}
