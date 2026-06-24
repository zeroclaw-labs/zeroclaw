//! Process-global access to the daemon's [`ControlPlaneHandle`].
//!
//! The control-plane is a daemon-wide singleton (one durable store + `boot_id` +
//! re-entry drain per daemon run), so it is reached through a global `OnceLock` —
//! the same pattern the health registry uses (`crate::health`) — rather than threaded
//! through every tool-construction signature.
//!
//! It is **optional by construction**: until [`init_control_plane`] runs at daemon
//! boot, [`control_plane`] returns `None` and every producer call site is a no-op
//! (tests, one-shot CLI, and any non-daemon context keep today's behavior). This is
//! what lets the supervision plane be additive and fail-safe.

use std::sync::OnceLock;

use super::boot::ControlPlaneHandle;

static CONTROL_PLANE: OnceLock<ControlPlaneHandle> = OnceLock::new();

/// Install the daemon's control-plane handle. Called ONCE at boot
/// (`daemon::run`). Subsequent calls are ignored (returns `false`), so a reload
/// iteration cannot swap the live store out from under in-flight producers.
pub fn init_control_plane(handle: ControlPlaneHandle) -> bool {
    CONTROL_PLANE.set(handle).is_ok()
}

/// The live control-plane, or `None` when not running under a booted daemon. Producers
/// MUST treat `None` as "supervision disabled" and proceed exactly as today.
pub fn control_plane() -> Option<&'static ControlPlaneHandle> {
    CONTROL_PLANE.get()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uninitialized_is_none() {
        // In the unit-test process the daemon never boots, so the plane is absent and
        // producers no-op. (We do not call init here — that would leak into other tests
        // via the process-global; init is exercised by the daemon integration path.)
        assert!(control_plane().is_none());
    }
}
