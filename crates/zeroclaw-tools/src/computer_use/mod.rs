//! Platform-independent pieces of the `computer_use` desktop automation tool.
//!
//! The tool itself arrives in a later change; this module holds the layers it
//! builds on, each usable and testable on its own:
//!
//! - [`backend`] defines the GUI types a model speaks in and the
//!   [`ComputerUseBackend`] trait a platform implements. Capture and input
//!   synthesis live behind that trait, so policy and receipts stay in one place
//!   and a platform backend cannot reach around them.
//! - [`scale`] converts between the downscaled screenshot a model sees and the
//!   logical-point coordinates the OS accepts, and performs the matching PNG
//!   resize.
//! - [`keys`] parses the `cmd+shift+s` chord syntax into a [`KeyChord`].
//!
//! A target with no capture backend compiles `stub`, whose `UnsupportedBackend`
//! reports an actionable [`BackendError::UnsupportedPlatform`] for every call,
//! and `default_backend` hands that stub out. The reachable surface is
//! therefore the same everywhere: a caller never has to branch on the host.

pub mod backend;
pub mod keys;
pub mod scale;
#[cfg(not(target_os = "macos"))]
pub mod stub;

pub use backend::{
    BackendError, ComputerUseBackend, DisplayInfo, KeyChord, KeyCode, LogicalPoint, Modifiers,
    MouseButton,
};
pub use keys::{KeyParseError, parse_key_chord};
pub use scale::{ScaleError, image_to_logical, resize_png_to, scaled_image_size};
#[cfg(not(target_os = "macos"))]
pub use stub::UnsupportedBackend;

#[cfg(not(target_os = "macos"))]
use std::sync::Arc;

/// The backend this build and host can actually run.
///
/// `action_timeout_secs` bounds one capture or input action. The unsupported
/// backend never uses it; a platform backend applies it to its own subprocess
/// and IPC deadlines.
///
/// Only the unsupported backend exists so far, so this is compiled on the
/// targets that have one. A later change adds the platform backends here.
#[cfg(not(target_os = "macos"))]
pub fn default_backend(_action_timeout_secs: u64) -> Arc<dyn ComputerUseBackend> {
    Arc::new(UnsupportedBackend::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reexports_are_usable_from_the_module_root() {
        // The tool layer imports these from `computer_use` rather than from each
        // submodule, so the re-exports are part of the module's contract.
        let display = DisplayInfo {
            index: 0,
            native_id: 1,
            origin_x: 0.0,
            origin_y: 0.0,
            logical_w: 1512.0,
            logical_h: 982.0,
            is_primary: true,
        };
        assert_eq!(
            scaled_image_size(display.logical_w, display.logical_h, 1280)
                .expect("valid dimensions"),
            (1280, 831)
        );
        assert_eq!(
            image_to_logical(&display, 0.0, 0.0, 1280).expect("in bounds"),
            LogicalPoint { x: 0.0, y: 0.0 }
        );
        assert_eq!(
            parse_key_chord("cmd+s").expect("chord parses"),
            KeyChord {
                key: KeyCode::Char('s'),
                modifiers: Modifiers {
                    command: true,
                    ..Modifiers::default()
                },
            }
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[tokio::test]
    async fn default_backend_reports_the_platform_is_unsupported() {
        let backend = default_backend(15);
        match backend.list_displays().await {
            Err(BackendError::UnsupportedPlatform(message)) => {
                assert!(message.contains("macOS only"), "message was {message}");
            }
            other => panic!("expected an unsupported-platform error, got {other:?}"),
        }
    }
}
