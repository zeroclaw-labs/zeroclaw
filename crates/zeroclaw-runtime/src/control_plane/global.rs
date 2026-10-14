//! Process-global access to daemon and non-daemon control-plane owners.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::boot::{ControlPlaneHandle, ControlPlaneRecoveryOwner};

static CONTROL_PLANE: OnceLock<ControlPlaneRecoveryOwner> = OnceLock::new();
type RecoveryOwnerCell = tokio::sync::OnceCell<ControlPlaneRecoveryOwner>;
static NON_DAEMON_CONTROL_PLANES: OnceLock<Mutex<HashMap<PathBuf, Arc<RecoveryOwnerCell>>>> =
    OnceLock::new();

/// Retain one recovery owner and reaper for each configured non-daemon store.
/// Delegate and peer producers share this owner instead of reconciling the same
/// store independently.
pub(crate) async fn non_daemon_control_plane(
    data_dir: &Path,
) -> anyhow::Result<ControlPlaneHandle> {
    let cell = NON_DAEMON_CONTROL_PLANES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .entry(data_dir.to_path_buf())
        .or_insert_with(|| Arc::new(RecoveryOwnerCell::new()))
        .clone();
    cell.get_or_try_init(|| async {
        let owner = ControlPlaneRecoveryOwner::start(data_dir).await?;
        std::mem::drop(owner.spawn_reaper(
            super::reaper::DEFAULT_MAX_RUNTIME_SECS,
            CancellationToken::new(),
        ));
        Ok::<_, anyhow::Error>(owner)
    })
    .await
    .map(|owner| owner.handle().clone())
}

/// Install the daemon's control-plane handle. Called ONCE at boot
/// (`daemon::run`). Subsequent calls are ignored (returns `false`), so a reload
/// iteration cannot swap the live store out from under in-flight producers.
pub(crate) fn init_control_plane(owner: ControlPlaneRecoveryOwner) -> bool {
    CONTROL_PLANE.set(owner).is_ok()
}

/// The daemon-owned control plane, or `None` when this process has not booted a
/// daemon. Producers that require durable lifecycle state must either attach to
/// the configured store explicitly or reject the operation.
pub fn control_plane() -> Option<&'static ControlPlaneHandle> {
    CONTROL_PLANE.get().map(ControlPlaneRecoveryOwner::handle)
}

/// Spawn the daemon-owned reaper without exposing recovery capability through
/// producer/observer handles.
pub(crate) fn spawn_control_plane_reaper(
    max_runtime_secs: i64,
    cancel: CancellationToken,
) -> Option<JoinHandle<()>> {
    CONTROL_PLANE
        .get()
        .map(|owner| owner.spawn_reaper(max_runtime_secs, cancel))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uninitialized_is_none() {
        // In the unit-test process the daemon never boots, so the plane is absent and
        // producers choose their explicit fallback. (We do not call init here — that would leak into other tests
        // via the process-global; init is exercised by the daemon integration path.)
        assert!(control_plane().is_none());
    }
}
