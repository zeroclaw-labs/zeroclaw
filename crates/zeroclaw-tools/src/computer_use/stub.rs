//! Backend for platforms that cannot capture or inject input.
//!
//! Every method reports the same [`BackendError::UnsupportedPlatform`], so a
//! build on such a target still registers a backend object and fails with an
//! actionable message instead of failing to compile or panicking at the call
//! site.

use async_trait::async_trait;

use super::backend::{
    BackendError, ComputerUseBackend, DisplayInfo, KeyChord, LogicalPoint, MouseButton,
};

/// The message every stub call reports.
const UNSUPPORTED: &str =
    "computer_use is currently supported on macOS only; Linux/X11 support is planned";

/// A backend that refuses every action because the platform cannot perform it.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnsupportedBackend;

impl UnsupportedBackend {
    /// A new stub backend.
    pub fn new() -> Self {
        Self
    }

    /// The failure every method reports.
    fn unsupported() -> BackendError {
        BackendError::UnsupportedPlatform(UNSUPPORTED.to_string())
    }
}

#[async_trait]
impl ComputerUseBackend for UnsupportedBackend {
    async fn list_displays(&self) -> Result<Vec<DisplayInfo>, BackendError> {
        Err(Self::unsupported())
    }

    async fn capture_display(&self, _display: &DisplayInfo) -> Result<Vec<u8>, BackendError> {
        Err(Self::unsupported())
    }

    async fn move_cursor(&self, _point: LogicalPoint) -> Result<(), BackendError> {
        Err(Self::unsupported())
    }

    async fn click(
        &self,
        _point: LogicalPoint,
        _button: MouseButton,
        _clicks: u8,
    ) -> Result<(), BackendError> {
        Err(Self::unsupported())
    }

    async fn scroll(
        &self,
        _point: LogicalPoint,
        _delta_x: i32,
        _delta_y: i32,
    ) -> Result<(), BackendError> {
        Err(Self::unsupported())
    }

    async fn type_text(&self, _text: &str) -> Result<(), BackendError> {
        Err(Self::unsupported())
    }

    async fn press_key(&self, _chord: &KeyChord) -> Result<(), BackendError> {
        Err(Self::unsupported())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::computer_use::backend::{KeyCode, Modifiers};

    /// Assert `result` is the unsupported-platform failure.
    fn assert_unsupported<T: std::fmt::Debug>(result: Result<T, BackendError>) {
        match result {
            Err(BackendError::UnsupportedPlatform(message)) => assert_eq!(message, UNSUPPORTED),
            other => panic!("expected an unsupported-platform error, got {other:?}"),
        }
    }

    fn display() -> DisplayInfo {
        DisplayInfo {
            index: 0,
            native_id: 1,
            origin_x: 0.0,
            origin_y: 0.0,
            logical_w: 1512.0,
            logical_h: 982.0,
            is_primary: true,
        }
    }

    #[tokio::test]
    async fn every_operation_reports_the_unsupported_platform() {
        let backend = UnsupportedBackend::new();
        assert_unsupported(backend.list_displays().await);
        assert_unsupported(backend.capture_display(&display()).await);
        assert_unsupported(backend.move_cursor(LogicalPoint { x: 1.0, y: 2.0 }).await);
        assert_unsupported(
            backend
                .click(LogicalPoint { x: 1.0, y: 2.0 }, MouseButton::Left, 1)
                .await,
        );
        assert_unsupported(backend.scroll(LogicalPoint { x: 1.0, y: 2.0 }, 0, 1).await);
        assert_unsupported(backend.type_text("hello").await);
        assert_unsupported(
            backend
                .press_key(&KeyChord {
                    key: KeyCode::Return,
                    modifiers: Modifiers::default(),
                })
                .await,
        );
    }
}
