//! Backend trait and platform-independent GUI types for `computer_use`.
//!
//! Everything a platform backend needs to speak in is defined here: display
//! metadata in OS logical-point space, a logical cursor position, mouse
//! buttons, modifier state, platform-independent key identifiers, and the
//! structured failure modes an implementation reports instead of a formatted
//! string. The tool layer owns policy, scaling, and receipts; a backend owns
//! only capture and input synthesis.

use async_trait::async_trait;

/// Active display metadata in OS logical-point coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct DisplayInfo {
    /// Zero-based index in the ordered display list; index `0` is always the
    /// primary display.
    pub index: u32,
    /// Platform-native display identifier (e.g. macOS `CGDirectDisplayID`).
    pub native_id: u32,
    /// Global logical-point X origin. Negative when the display sits left of
    /// the primary display.
    pub origin_x: f64,
    /// Global logical-point Y origin. Negative when the display sits above the
    /// primary display.
    pub origin_y: f64,
    /// Display logical width in points (strictly positive).
    pub logical_w: f64,
    /// Display logical height in points (strictly positive).
    pub logical_h: f64,
    /// True for the primary display.
    pub is_primary: bool,
}

/// Global logical-point coordinate on the virtual desktop.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LogicalPoint {
    pub x: f64,
    pub y: f64,
}

/// Mouse button for click actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// Modifier keys held during a key press.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub command: bool,
    pub control: bool,
    pub alt: bool,
    pub shift: bool,
}

/// Platform-independent key identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyCode {
    /// A single ASCII graphic character, normalized to lowercase for letters.
    Char(char),
    Return,
    Escape,
    Tab,
    Space,
    BackSpace,
    Delete,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
}

/// A parsed key press with optional modifiers (for example `cmd+shift+s`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyChord {
    pub key: KeyCode,
    pub modifiers: Modifiers,
}

/// Structured failure modes from a [`ComputerUseBackend`].
///
/// Each variant carries enough detail for the tool layer to surface an
/// actionable message: a denied permission names the permission and how to
/// grant it, and an input failure names the step that failed.
#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    /// A required OS-level permission has not been granted.
    #[error("{permission} permission denied: {instructions}")]
    PermissionDenied {
        /// The permission that is missing, as a human-readable name.
        permission: &'static str,
        /// How the operator can grant the permission.
        instructions: String,
    },
    /// The build or host cannot perform this action at all.
    #[error("{0}")]
    UnsupportedPlatform(String),
    /// Screen capture failed.
    #[error("screenshot capture failed: {0}")]
    CaptureFailed(String),
    /// Input synthesis failed.
    #[error("input synthesis failed: {0}")]
    InputFailed(String),
    /// The action did not finish inside its deadline.
    #[error("action timed out: {0}")]
    Timeout(String),
}

/// Platform-specific desktop automation backend.
///
/// Implementations must be safe to share across tasks and must report every
/// failure as a [`BackendError`] rather than panicking: a capture or input call
/// crosses a process- and OS-owned boundary the tool cannot police.
#[async_trait]
pub trait ComputerUseBackend: Send + Sync {
    /// Every active display, primary first.
    async fn list_displays(&self) -> Result<Vec<DisplayInfo>, BackendError>;

    /// Capture `display` as encoded PNG bytes, at its native pixel size.
    async fn capture_display(&self, display: &DisplayInfo) -> Result<Vec<u8>, BackendError>;

    /// Move the cursor to a global logical point.
    async fn move_cursor(&self, point: LogicalPoint) -> Result<(), BackendError>;

    /// Click `button` `clicks` times at a global logical point.
    async fn click(
        &self,
        point: LogicalPoint,
        button: MouseButton,
        clicks: u8,
    ) -> Result<(), BackendError>;

    /// Scroll by wheel deltas at a global logical point.
    async fn scroll(
        &self,
        point: LogicalPoint,
        delta_x: i32,
        delta_y: i32,
    ) -> Result<(), BackendError>;

    /// Type `text` into the focused application.
    async fn type_text(&self, text: &str) -> Result<(), BackendError>;

    /// Press one key with the chord's modifiers held.
    async fn press_key(&self, chord: &KeyChord) -> Result<(), BackendError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifiers_default_is_all_released() {
        let modifiers = Modifiers::default();
        assert!(!modifiers.command);
        assert!(!modifiers.control);
        assert!(!modifiers.alt);
        assert!(!modifiers.shift);
    }

    #[test]
    fn backend_errors_render_the_expected_messages() {
        let denied = BackendError::PermissionDenied {
            permission: "Accessibility",
            instructions: "enable it in System Settings".to_string(),
        };
        assert_eq!(
            denied.to_string(),
            "Accessibility permission denied: enable it in System Settings"
        );
        assert_eq!(
            BackendError::UnsupportedPlatform("no backend".to_string()).to_string(),
            "no backend"
        );
        assert_eq!(
            BackendError::CaptureFailed("no display".to_string()).to_string(),
            "screenshot capture failed: no display"
        );
        assert_eq!(
            BackendError::InputFailed("bad key".to_string()).to_string(),
            "input synthesis failed: bad key"
        );
        assert_eq!(
            BackendError::Timeout("slow".to_string()).to_string(),
            "action timed out: slow"
        );
    }
}
