//! Versioned wire types shared by computer-use clients and platform drivers.
//!
//! The protocol is deliberately strict: unknown fields are rejected, every
//! request carries its resolved per-call policy, and all payloads have hard
//! size limits. Platform backends must call [`Request::validate`] before
//! touching an operating-system API.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Component, Path, PathBuf};
use uuid::Uuid;
use zeroclaw_config::schema::ComputerUseApplicationAccess;

/// Current computer-use stdio protocol version.
pub const PROTOCOL_VERSION: u16 = 1;
/// Maximum encoded request size accepted by a protocol endpoint.
pub const MAX_REQUEST_BYTES: usize = 128 * 1024;
/// Maximum encoded response size emitted by a protocol endpoint.
pub const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
/// Maximum stderr captured from a platform process.
pub const MAX_PROCESS_STDERR_BYTES: usize = 16 * 1024;
/// Maximum PNG size accepted after a screen capture.
pub const MAX_SCREENSHOT_BYTES: u64 = 64 * 1024 * 1024;
/// Absolute maximum number of nodes in one accessibility snapshot.
pub const MAX_AX_NODES: u32 = 1_024;
/// Default number of nodes requested from an accessibility snapshot.
pub const DEFAULT_AX_NODES: u32 = 512;
/// Absolute maximum accessibility-tree depth.
pub const MAX_AX_DEPTH: u32 = 32;
/// Default accessibility-tree depth.
pub const DEFAULT_AX_DEPTH: u32 = 12;
/// Maximum UTF-8 bytes in text typed by one request.
pub const MAX_TEXT_BYTES: usize = 64 * 1024;
/// Maximum Unicode scalar values in text typed by one request.
pub const MAX_TEXT_CHARS: usize = zeroclaw_config::schema::COMPUTER_USE_MAX_TEXT_CHARS;
/// Maximum Unicode scalar values in an application identifier.
pub const MAX_APPLICATION_CHARS: usize =
    zeroclaw_config::schema::COMPUTER_USE_MAX_APPLICATION_CHARS;
/// Maximum number of application identifiers in a resolved policy.
pub const MAX_ALLOWED_APPLICATIONS: usize =
    zeroclaw_config::schema::COMPUTER_USE_MAX_ALLOWED_APPLICATIONS;
/// Maximum running application identities returned by `list_apps`.
pub const MAX_RUNNING_APPLICATIONS: usize = 256;
/// Maximum Unicode scalar values in a screenshot destination path.
pub const MAX_PATH_CHARS: usize = 4_096;
/// Maximum UTF-8 bytes in a screenshot destination path.
pub const MAX_PATH_BYTES: usize = 16 * 1024;
/// Maximum Unicode scalar values in an accessibility selector or node field.
pub const MAX_AX_STRING_CHARS: usize = 512;
/// Maximum absolute scroll delta accepted in one action.
pub const MAX_SCROLL_DELTA: i32 = 10_000;
/// Absolute protocol safety cap for global logical display coordinates.
pub const MAX_ABS_COORDINATE: f64 = 1_000_000.0;

macro_rules! define_actions {
    ($(
        $(#[$variant_meta:meta])*
        $variant:ident => $wire:literal, confirmation = $confirmation:literal, model_required = [$($model_required:literal),* $(,)?] {
            $($(#[$field_meta:meta])* $field:ident: $field_ty:ty),* $(,)?
        }
    ),+ $(,)?) => {
        /// A computer-use action carried by a protocol request.
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        #[serde(tag = "type", deny_unknown_fields)]
        pub enum Action {
            $(
                $(#[$variant_meta])*
                #[serde(rename = $wire)]
                $variant {
                    $($(#[$field_meta])* $field: $field_ty),*
                },
            )+
        }

        /// Payload-independent identity of a computer-use action.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum ActionKind {
            $(
                #[serde(rename = $wire)]
                $variant,
            )+
        }

        impl ActionKind {
            /// Every action kind, generated from the canonical action table.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            /// Stable wire name for this action kind.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $wire),+
                }
            }

            /// Whether this action must receive fresh operator confirmation.
            #[must_use]
            pub const fn requires_fresh_confirmation(self) -> bool {
                match self {
                    $(Self::$variant => $confirmation),+
                }
            }

            /// Agent-supplied fields required for this action. Generated from
            /// the canonical action table; driver-injected fields are omitted.
            #[must_use]
            pub const fn model_required_fields(self) -> &'static [&'static str] {
                match self {
                    $(Self::$variant => &[$($model_required),*]),+
                }
            }
        }

        impl Action {
            /// Return the payload-independent action kind.
            #[must_use]
            pub const fn kind(&self) -> ActionKind {
                match self {
                    $(Self::$variant { .. } => ActionKind::$variant),+
                }
            }

            /// Whether this action must receive fresh operator confirmation.
            #[must_use]
            pub const fn requires_fresh_confirmation(&self) -> bool {
                self.kind().requires_fresh_confirmation()
            }
        }
    };
}

define_actions! {
    /// Report supported actions and current permission state.
    Capabilities => "capabilities", confirmation = false, model_required = [] {},
    /// List running applications admitted by the resolved application policy.
    ListApplications => "list_apps", confirmation = false, model_required = [] {},
    /// Inspect one exact frontmost application's accessibility tree.
    Inspect => "inspect", confirmation = false, model_required = ["expected_application"] {
        expected_application: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_nodes: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_depth: Option<u32>,
    },
    /// Capture the main display to a caller-selected PNG path.
    Screenshot => "screenshot", confirmation = true, model_required = ["application"] {
        application: String,
        path: PathBuf,
    },
    /// Bring an allowed application to the foreground.
    Focus => "focus", confirmation = true, model_required = ["application"] {
        application: String,
    },
    /// Move the pointer at allowed global coordinates through the expected app.
    MouseMove => "mouse_move", confirmation = true, model_required = ["x", "y", "expected_application"] {
        x: f64,
        y: f64,
        expected_application: String,
    },
    /// Click allowed global coordinates through the expected app's event stream.
    Click => "click", confirmation = true, model_required = ["x", "y", "button", "expected_application"] {
        x: f64,
        y: f64,
        button: MouseButton,
        expected_application: String,
    },
    /// Scroll the expected app at the policy-bounded current pointer location.
    Scroll => "scroll", confirmation = true, model_required = ["delta_x", "delta_y", "expected_application"] {
        delta_x: i32,
        delta_y: i32,
        expected_application: String,
    },
    /// Type Unicode text into the frontmost allowed application.
    TypeText => "type_text", confirmation = true, model_required = ["text", "expected_application"] {
        text: String,
        expected_application: String,
    },
    /// Press a key, optionally with modifiers, in the frontmost application.
    KeyPress => "key_press", confirmation = true, model_required = ["key", "expected_application"] {
        key: Key,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        modifiers: Vec<KeyModifier>,
        expected_application: String,
    },
    /// Find exactly one accessibility element and invoke its native action.
    PressElement => "press_element", confirmation = true, model_required = ["application", "title"] {
        application: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        role: Option<String>,
        title: String,
    },
}

/// A versioned computer-use request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// Wire protocol version.
    pub version: u16,
    /// Correlation identifier generated by the caller.
    pub request_id: Uuid,
    /// Requested computer-use operation.
    pub action: Action,
    /// Per-call policy resolved from canonical runtime configuration.
    pub policy: Policy,
}

impl Request {
    /// Create a request for the current protocol version.
    #[must_use]
    pub fn new(request_id: Uuid, action: Action, policy: Policy) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            request_id,
            action,
            policy,
        }
    }

    /// Validate version, encoded size, policy, and action fields.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.version != PROTOCOL_VERSION {
            return Err(ProtocolError::new(
                ErrorCode::UnsupportedVersion,
                format!(
                    "unsupported computer-use protocol version {}; expected {PROTOCOL_VERSION}",
                    self.version
                ),
                false,
            ));
        }
        if self.request_id.is_nil() {
            return Err(invalid_request("request_id must not be nil"));
        }
        encoded_size(self, MAX_REQUEST_BYTES, "request")?;
        self.action.validate(&self.policy)
    }
}

/// Resolved policy applied to exactly one computer-use request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    /// Whether application access is exact-allowlist or desktop-wide.
    pub application_access: ComputerUseApplicationAccess,
    /// Exact application names or stable identifiers admitted for this call.
    pub allowed_applications: Vec<String>,
    /// Inclusive minimum global x coordinate.
    pub min_coordinate_x: Option<i64>,
    /// Inclusive minimum global y coordinate.
    pub min_coordinate_y: Option<i64>,
    /// Inclusive maximum global x coordinate.
    pub max_coordinate_x: Option<i64>,
    /// Inclusive maximum global y coordinate.
    pub max_coordinate_y: Option<i64>,
    /// Maximum Unicode scalar values accepted by `type_text`.
    pub max_text_chars: usize,
}

impl Policy {
    /// Validate internally consistent, globally bounded policy fields.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.application_access == ComputerUseApplicationAccess::Desktop
            && !self.allowed_applications.is_empty()
        {
            return Err(invalid_policy(
                "allowed_applications must be empty for desktop application access",
            ));
        }
        if self.allowed_applications.len() > MAX_ALLOWED_APPLICATIONS {
            return Err(invalid_policy(format!(
                "allowed_applications exceeds the limit of {MAX_ALLOWED_APPLICATIONS}"
            )));
        }
        for application in &self.allowed_applications {
            if application == "*" {
                return Err(invalid_policy(
                    "allowed_applications does not accept wildcard selectors",
                ));
            }
            validate_application_identifier(application)?;
        }
        validate_coordinate_bounds("x", self.min_coordinate_x, self.max_coordinate_x)?;
        validate_coordinate_bounds("y", self.min_coordinate_y, self.max_coordinate_y)?;
        if self.max_text_chars == 0 || self.max_text_chars > MAX_TEXT_CHARS {
            return Err(invalid_policy(format!(
                "max_text_chars must be between 1 and {MAX_TEXT_CHARS}"
            )));
        }
        Ok(())
    }

    /// Whether an identity supplied by the platform backend matches exactly.
    #[must_use]
    pub fn allows(&self, application: &ApplicationIdentity) -> bool {
        self.application_access == ComputerUseApplicationAccess::Desktop
            || self
                .allowed_applications
                .iter()
                .any(|allowed| application.matches(allowed))
    }

    fn allows_selector(&self, selector: &str) -> bool {
        self.application_access == ComputerUseApplicationAccess::Desktop
            || self
                .allowed_applications
                .iter()
                .any(|allowed| allowed == selector)
    }

    pub(crate) fn validate_coordinate(&self, point: Point) -> Result<(), ProtocolError> {
        if !point.x.is_finite()
            || !point.y.is_finite()
            || point.x.abs() > MAX_ABS_COORDINATE
            || point.y.abs() > MAX_ABS_COORDINATE
        {
            return Err(ProtocolError::new(
                ErrorCode::InvalidCoordinate,
                format!(
                    "coordinates must be finite and within +/-{MAX_ABS_COORDINATE} logical points"
                ),
                false,
            ));
        }
        let x_outside = self
            .min_coordinate_x
            .is_some_and(|minimum| point.x < minimum as f64)
            || self
                .max_coordinate_x
                .is_some_and(|maximum| point.x > maximum as f64);
        let y_outside = self
            .min_coordinate_y
            .is_some_and(|minimum| point.y < minimum as f64)
            || self
                .max_coordinate_y
                .is_some_and(|maximum| point.y > maximum as f64);
        if x_outside || y_outside {
            return Err(ProtocolError::new(
                ErrorCode::InvalidCoordinate,
                format!(
                    "coordinate ({}, {}) is outside the allowed x={:?}..={:?}, y={:?}..={:?} bounds",
                    point.x,
                    point.y,
                    self.min_coordinate_x,
                    self.max_coordinate_x,
                    self.min_coordinate_y,
                    self.max_coordinate_y
                ),
                false,
            ));
        }
        Ok(())
    }
}

impl Action {
    /// Validate this action against a resolved per-call policy.
    pub fn validate(&self, policy: &Policy) -> Result<(), ProtocolError> {
        policy.validate()?;
        match self {
            Self::Capabilities {} => Ok(()),
            Self::ListApplications {} => require_application_access(policy),
            Self::Inspect {
                expected_application,
                max_nodes,
                max_depth,
            } => {
                require_application_access(policy)?;
                validate_expected_application(policy, Some(expected_application.as_str()))?;
                if max_nodes.is_some_and(|value| value == 0 || value > MAX_AX_NODES) {
                    return Err(invalid_request(format!(
                        "max_nodes must be between 1 and {MAX_AX_NODES}"
                    )));
                }
                if max_depth.is_some_and(|value| value == 0 || value > MAX_AX_DEPTH) {
                    return Err(invalid_request(format!(
                        "max_depth must be between 1 and {MAX_AX_DEPTH}"
                    )));
                }
                Ok(())
            }
            Self::Screenshot { application, path } => {
                require_application_access(policy)?;
                validate_application_identifier(application)?;
                if !policy.allows_selector(application) {
                    return Err(application_not_allowed(application));
                }
                validate_screenshot_path(path)
            }
            Self::Focus { application } | Self::PressElement { application, .. } => {
                require_application_access(policy)?;
                validate_application_identifier(application)?;
                if !policy.allows_selector(application) {
                    return Err(application_not_allowed(application));
                }
                if let Self::PressElement { role, title, .. } = self {
                    if let Some(role) = role {
                        validate_ax_selector("role", role)?;
                    }
                    validate_ax_selector("title", title)?;
                }
                Ok(())
            }
            Self::MouseMove {
                x,
                y,
                expected_application,
            }
            | Self::Click {
                x,
                y,
                expected_application,
                ..
            } => {
                require_application_access(policy)?;
                validate_expected_application(policy, Some(expected_application.as_str()))?;
                policy.validate_coordinate(Point { x: *x, y: *y })
            }
            Self::Scroll {
                delta_x,
                delta_y,
                expected_application,
            } => {
                require_application_access(policy)?;
                validate_expected_application(policy, Some(expected_application.as_str()))?;
                if delta_x.unsigned_abs() > MAX_SCROLL_DELTA as u32
                    || delta_y.unsigned_abs() > MAX_SCROLL_DELTA as u32
                    || (*delta_x == 0 && *delta_y == 0)
                {
                    return Err(invalid_request(format!(
                        "scroll deltas must be non-zero and within +/-{MAX_SCROLL_DELTA}"
                    )));
                }
                Ok(())
            }
            Self::TypeText {
                text,
                expected_application,
            } => {
                require_application_access(policy)?;
                validate_expected_application(policy, Some(expected_application.as_str()))?;
                let chars = text.chars().count();
                if chars == 0
                    || chars > policy.max_text_chars
                    || text.len() > MAX_TEXT_BYTES
                    || text
                        .chars()
                        .any(zeroclaw_api::tool::is_unsafe_confirmation_character)
                {
                    return Err(invalid_request(format!(
                        "text must contain 1..={} non-control characters and at most {MAX_TEXT_BYTES} UTF-8 bytes",
                        policy.max_text_chars
                    )));
                }
                Ok(())
            }
            Self::KeyPress {
                modifiers,
                expected_application,
                ..
            } => {
                require_application_access(policy)?;
                validate_expected_application(policy, Some(expected_application.as_str()))?;
                if has_duplicate_modifiers(modifiers) {
                    return Err(invalid_request("modifiers must not contain duplicates"));
                }
                Ok(())
            }
        }
    }
}

/// A point in global logical display coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Point {
    /// Horizontal coordinate.
    pub x: f64,
    /// Vertical coordinate.
    pub y: f64,
}

/// A rectangle in global logical display coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rect {
    /// Left edge.
    pub x: f64,
    /// Top edge.
    pub y: f64,
    /// Width.
    pub width: f64,
    /// Height.
    pub height: f64,
}

macro_rules! define_wire_enum {
    (
        $(#[$enum_meta:meta])*
        pub enum $name:ident {
            $($(#[$variant_meta:meta])* $variant:ident => $wire:literal),+ $(,)?
        }
    ) => {
        $(#[$enum_meta])*
        pub enum $name {
            $(
                $(#[$variant_meta])*
                #[serde(rename = $wire)]
                $variant,
            )+
        }

        impl $name {
            /// Every wire value, generated from this enum's canonical table.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            /// Stable serialized value.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $wire),+
                }
            }
        }
    };
}

define_wire_enum! {
    /// Mouse button used by click and drag actions.
    #[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub enum MouseButton {
        /// Primary mouse button.
        #[default]
        Left => "left",
        /// Secondary mouse button.
        Right => "right",
        /// Middle mouse button.
        Middle => "middle",
    }
}

define_wire_enum! {
    /// Modifier applied to a key press.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub enum KeyModifier {
        /// Command key.
        Command => "command",
        /// Control key.
        Control => "control",
        /// Option/Alt key.
        Option => "option",
        /// Shift key.
        Shift => "shift",
        /// Function key.
        Function => "function",
    }
}

macro_rules! define_keys {
    ($($variant:ident => $wire:literal),+ $(,)?) => {
        /// Layout-independent key accepted by a computer-use request.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum Key {
            $(
                #[serde(rename = $wire)]
                $variant,
            )+
        }

        impl Key {
            /// Every accepted key, generated from the canonical key table.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            /// Stable wire name for this key.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $wire),+
                }
            }
        }
    };
}

define_keys! {
    A => "a",
    B => "b",
    C => "c",
    D => "d",
    E => "e",
    F => "f",
    G => "g",
    H => "h",
    I => "i",
    J => "j",
    K => "k",
    L => "l",
    M => "m",
    N => "n",
    O => "o",
    P => "p",
    Q => "q",
    R => "r",
    S => "s",
    T => "t",
    U => "u",
    V => "v",
    W => "w",
    X => "x",
    Y => "y",
    Z => "z",
    Digit0 => "0",
    Digit1 => "1",
    Digit2 => "2",
    Digit3 => "3",
    Digit4 => "4",
    Digit5 => "5",
    Digit6 => "6",
    Digit7 => "7",
    Digit8 => "8",
    Digit9 => "9",
    Enter => "enter",
    Tab => "tab",
    Space => "space",
    Backspace => "backspace",
    Delete => "delete",
    Escape => "escape",
    Home => "home",
    End => "end",
    PageUp => "page_up",
    PageDown => "page_down",
    LeftArrow => "left_arrow",
    RightArrow => "right_arrow",
    UpArrow => "up_arrow",
    DownArrow => "down_arrow",
    F1 => "f1",
    F2 => "f2",
    F3 => "f3",
    F4 => "f4",
    F5 => "f5",
    F6 => "f6",
    F7 => "f7",
    F8 => "f8",
    F9 => "f9",
    F10 => "f10",
    F11 => "f11",
    F12 => "f12",
}

/// Identity resolved directly from the operating system.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationIdentity {
    /// Process display name.
    pub name: String,
    /// Stable bundle/application identifier when the platform reports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle_id: Option<String>,
    /// Operating-system process identifier.
    pub pid: u32,
}

impl ApplicationIdentity {
    /// Whether this identity matches an exact name or bundle-id selector.
    #[must_use]
    pub fn matches(&self, selector: &str) -> bool {
        match application_selector_kind(selector) {
            ApplicationSelectorKind::BundleIdentifier => {
                self.bundle_id.as_deref() == Some(selector)
            }
            ApplicationSelectorKind::DisplayName => self.name == selector,
        }
    }
}

/// Identity namespace selected by an application allowlist entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationSelectorKind {
    /// Reverse-DNS-style application identifier; only `bundle_id` may match.
    BundleIdentifier,
    /// Human-visible process/application name; only `name` may match.
    DisplayName,
}

/// Classify one exact application selector without cross-matching identity
/// namespaces. Dotted reverse-DNS-shaped values are application identifiers.
#[must_use]
pub fn application_selector_kind(selector: &str) -> ApplicationSelectorKind {
    let mut segments = selector.split('.');
    let first = segments.next().unwrap_or_default();
    let mut segment_count = 1_usize;
    let rest_are_valid = segments.all(|segment| {
        segment_count += 1;
        !segment.is_empty()
            && segment
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '-')
    });
    let first_is_valid = !first.is_empty()
        && first
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-');
    if segment_count >= 2 && first_is_valid && rest_are_valid {
        ApplicationSelectorKind::BundleIdentifier
    } else {
        ApplicationSelectorKind::DisplayName
    }
}

/// One flattened node in a bounded accessibility-tree snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessibilityNode {
    /// Monotonic node identifier within this snapshot.
    pub id: u32,
    /// Parent node identifier, absent for the application root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<u32>,
    /// Depth relative to the application root.
    pub depth: u32,
    /// Accessibility role.
    pub role: String,
    /// Human-visible title, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Human-visible value, when representable as text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Accessibility description, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Whether the element is enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Whether the element has keyboard focus.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused: Option<bool>,
    /// Element bounds in global logical display coordinates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounds: Option<Rect>,
    /// Accessibility actions supported by the element.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<String>,
}

/// Result of a bounded accessibility-tree inspection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessibilitySnapshot {
    /// Frontmost application inspected by the backend.
    pub application: ApplicationIdentity,
    /// Flattened accessibility nodes in breadth-first order.
    pub nodes: Vec<AccessibilityNode>,
    /// Whether a node, depth, or encoded-output limit truncated the tree.
    pub truncated: bool,
    /// Effective node limit.
    pub max_nodes: u32,
    /// Effective depth limit.
    pub max_depth: u32,
}

/// Summary of an element selected for a native press/invoke action.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ElementSummary {
    /// Accessibility role.
    pub role: String,
    /// Element title, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Element bounds, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounds: Option<Rect>,
}

/// Platform implemented by a computer-use backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    /// Apple macOS.
    Macos,
    /// Linux desktop.
    Linux,
    /// Microsoft Windows.
    Windows,
}

/// Current state of an operating-system permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionState {
    /// Permission is available.
    Granted,
    /// Permission is known to be unavailable.
    Denied,
    /// The platform cannot query this permission without prompting.
    Unknown,
}

/// Permissions relevant to computer use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Permissions {
    /// Accessibility permission used for inspection and input.
    pub accessibility: PermissionState,
    /// Screen-recording permission used for screenshots.
    pub screen_recording: PermissionState,
}

/// Successful response payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ResponseData {
    /// Backend action inventory and permission state.
    Capabilities {
        /// Current platform.
        platform: Platform,
        /// Actions supported by this backend.
        actions: Vec<ActionKind>,
        /// Current permission state.
        permissions: Permissions,
    },
    /// Running applications visible to the resolved application policy.
    Applications {
        /// Bounded application identities resolved by the operating system.
        applications: Vec<ApplicationIdentity>,
        /// Whether additional admitted applications were omitted.
        truncated: bool,
    },
    /// Accessibility inspection result.
    Inspect {
        /// Bounded frontmost-application snapshot.
        snapshot: AccessibilitySnapshot,
    },
    /// Main-display screenshot metadata.
    Screenshot {
        /// Caller-supplied destination path.
        path: PathBuf,
        /// Main-display bounds in global logical coordinates.
        display_bounds: Rect,
        /// Captured physical pixel width.
        pixel_width: u64,
        /// Captured physical pixel height.
        pixel_height: u64,
    },
    /// Result shared by coordinate, text, scroll, and key input.
    Input {
        /// Frontmost allowed application checked immediately before input.
        application: ApplicationIdentity,
    },
    /// Application focused by the backend.
    Focused {
        /// Identity re-resolved after focus.
        application: ApplicationIdentity,
    },
    /// Element selected and pressed through the accessibility API.
    ElementPressed {
        /// Frontmost application containing the element.
        application: ApplicationIdentity,
        /// Unique matched element.
        element: ElementSummary,
    },
}

/// Versioned response emitted for one request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    /// Wire protocol version.
    pub version: u16,
    /// Request correlation identifier.
    pub request_id: Uuid,
    /// Whether the request completed successfully.
    pub ok: bool,
    /// Successful response payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<ResponseData>,
    /// Stable structured error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ProtocolError>,
}

impl Response {
    /// Construct a successful response.
    #[must_use]
    pub fn success(request_id: Uuid, data: ResponseData) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            request_id,
            ok: true,
            data: Some(data),
            error: None,
        }
    }

    /// Construct a failed response.
    #[must_use]
    pub fn failure(request_id: Uuid, error: ProtocolError) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            request_id,
            ok: false,
            data: None,
            error: Some(error),
        }
    }

    /// Validate correlation, success/error shape, payload pairing, and size.
    pub fn validate_for(&self, request: &Request) -> Result<(), ProtocolError> {
        if self.version != PROTOCOL_VERSION || self.version != request.version {
            return Err(protocol_violation(
                "response version does not match request",
            ));
        }
        if self.request_id != request.request_id {
            return Err(protocol_violation(
                "response request_id does not match request",
            ));
        }
        match (self.ok, &self.data, &self.error) {
            (true, Some(data), None) => validate_response_data(data, &request.action)?,
            (false, None, Some(error)) => {
                if error.outcome_unknown && error.retryable {
                    return Err(protocol_violation(
                        "an unknown action outcome cannot be marked retryable",
                    ));
                }
            }
            _ => {
                return Err(protocol_violation(
                    "response must contain exactly one of data or error consistent with ok",
                ));
            }
        }
        encoded_size(self, MAX_RESPONSE_BYTES, "response")
    }
}

macro_rules! define_error_codes {
    ($($variant:ident => $wire:literal),+ $(,)?) => {
        /// Stable machine-readable computer-use error code.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum ErrorCode {
            $(
                #[serde(rename = $wire)]
                $variant,
            )+
        }

        impl ErrorCode {
            /// Stable wire string for this error code.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $wire),+
                }
            }
        }
    };
}

define_error_codes! {
    InvalidRequest => "invalid_request",
    UnsupportedVersion => "unsupported_version",
    UnsupportedPlatform => "unsupported_platform",
    InvalidPolicy => "invalid_policy",
    InvalidCoordinate => "invalid_coordinate",
    InvalidPath => "invalid_path",
    PermissionDenied => "permission_denied",
    ApplicationNotAllowed => "application_not_allowed",
    ApplicationMismatch => "application_mismatch",
    ApplicationNotFound => "application_not_found",
    AccessibilityUnavailable => "accessibility_unavailable",
    ElementNotFound => "element_not_found",
    AmbiguousElement => "ambiguous_element",
    ScreenCaptureUnavailable => "screen_capture_unavailable",
    EventCreationFailed => "event_creation_failed",
    OutputTooLarge => "output_too_large",
    Timeout => "timeout",
    CommandFailed => "command_failed",
    Io => "io",
    ProtocolViolation => "protocol_violation",
}

/// Structured error returned across the protocol boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtocolError {
    /// Stable machine-readable code.
    pub code: ErrorCode,
    /// Bounded human-readable detail.
    pub message: String,
    /// Whether retrying after an external-state change may succeed.
    pub retryable: bool,
    /// Whether the platform may have completed the requested side effect even
    /// though it could not return a trustworthy success response.
    #[serde(default, skip_serializing_if = "is_false")]
    pub outcome_unknown: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl ProtocolError {
    /// Construct a structured protocol error.
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>, retryable: bool) -> Self {
        let mut message = message.into();
        truncate_chars(&mut message, MAX_AX_STRING_CHARS);
        Self {
            code,
            message,
            retryable,
            outcome_unknown: false,
        }
    }

    /// Mark an error that happened after the side-effect boundary may have
    /// been crossed. Callers must inspect current state instead of retrying.
    #[must_use]
    pub fn with_unknown_outcome(mut self) -> Self {
        self.retryable = false;
        self.outcome_unknown = true;
        self
    }
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for ProtocolError {}

fn validate_response_data(data: &ResponseData, action: &Action) -> Result<(), ProtocolError> {
    let matches = matches!(
        (data, action),
        (ResponseData::Capabilities { .. }, Action::Capabilities {})
            | (
                ResponseData::Applications { .. },
                Action::ListApplications {}
            )
            | (ResponseData::Inspect { .. }, Action::Inspect { .. })
            | (ResponseData::Screenshot { .. }, Action::Screenshot { .. })
            | (ResponseData::Focused { .. }, Action::Focus { .. })
            | (ResponseData::Input { .. }, Action::MouseMove { .. })
            | (ResponseData::Input { .. }, Action::Click { .. })
            | (ResponseData::Input { .. }, Action::Scroll { .. })
            | (ResponseData::Input { .. }, Action::TypeText { .. })
            | (ResponseData::Input { .. }, Action::KeyPress { .. })
            | (
                ResponseData::ElementPressed { .. },
                Action::PressElement { .. }
            )
    );
    if !matches {
        return Err(protocol_violation(
            "response data type does not match request action",
        ));
    }
    if let (
        ResponseData::Screenshot { path: actual, .. },
        Action::Screenshot { path: expected, .. },
    ) = (data, action)
        && actual != expected
    {
        return Err(protocol_violation(
            "screenshot response path does not match request path",
        ));
    }
    Ok(())
}

fn encoded_size<T: Serialize>(value: &T, limit: usize, label: &str) -> Result<(), ProtocolError> {
    let encoded = serde_json::to_vec(value).map_err(|error| {
        ProtocolError::new(
            ErrorCode::ProtocolViolation,
            format!("failed to encode {label}: {error}"),
            false,
        )
    })?;
    if encoded.len() > limit {
        return Err(ProtocolError::new(
            ErrorCode::OutputTooLarge,
            format!("encoded {label} exceeds the limit of {limit} bytes"),
            false,
        ));
    }
    Ok(())
}

fn validate_expected_application(
    policy: &Policy,
    expected: Option<&str>,
) -> Result<(), ProtocolError> {
    if let Some(expected) = expected {
        validate_application_identifier(expected)?;
        if !policy.allows_selector(expected) {
            return Err(application_not_allowed(expected));
        }
    }
    Ok(())
}

fn validate_application_identifier(application: &str) -> Result<(), ProtocolError> {
    let chars = application.chars().count();
    if application == "*"
        || application.trim() != application
        || chars == 0
        || chars > MAX_APPLICATION_CHARS
        || application
            .chars()
            .any(zeroclaw_api::tool::is_unsafe_confirmation_character)
    {
        return Err(invalid_policy(format!(
            "application identifiers must be trimmed, single-line, and contain 1..={MAX_APPLICATION_CHARS} characters"
        )));
    }
    Ok(())
}

fn validate_ax_selector(name: &str, value: &str) -> Result<(), ProtocolError> {
    let chars = value.chars().count();
    if value.trim() != value
        || chars == 0
        || chars > MAX_AX_STRING_CHARS
        || value
            .chars()
            .any(zeroclaw_api::tool::is_unsafe_confirmation_character)
    {
        return Err(invalid_request(format!(
            "{name} must be trimmed and contain 1..={MAX_AX_STRING_CHARS} characters"
        )));
    }
    Ok(())
}

fn validate_screenshot_path(path: &Path) -> Result<(), ProtocolError> {
    let Some(value) = path.to_str() else {
        return Err(invalid_path("screenshot path must be valid UTF-8"));
    };
    if !path.is_absolute()
        || value.chars().count() > MAX_PATH_CHARS
        || value.len() > MAX_PATH_BYTES
        || value.contains('\0')
    {
        return Err(invalid_path(format!(
            "screenshot path must be absolute and at most {MAX_PATH_CHARS} characters or {MAX_PATH_BYTES} UTF-8 bytes"
        )));
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return Err(invalid_path(
            "screenshot path must not contain '.' or '..' components",
        ));
    }
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_none_or(|extension| !extension.eq_ignore_ascii_case("png"))
    {
        return Err(invalid_path("screenshot path must have a .png extension"));
    }
    Ok(())
}

fn require_application_access(policy: &Policy) -> Result<(), ProtocolError> {
    if policy.application_access == ComputerUseApplicationAccess::Allowlist
        && policy.allowed_applications.is_empty()
    {
        return Err(invalid_policy(
            "allowed_applications must not be empty for allowlist application access",
        ));
    }
    Ok(())
}

fn validate_coordinate_bounds(
    axis: &str,
    minimum: Option<i64>,
    maximum: Option<i64>,
) -> Result<(), ProtocolError> {
    // Values beyond JavaScript's exact-integer range cannot be compared
    // consistently by the JXA backend and Rust caller.
    const MAX_EXACT_INTEGER: i64 = (1_i64 << 53) - 1;
    for (name, value) in [("minimum", minimum), ("maximum", maximum)] {
        if value.is_some_and(|value| value.unsigned_abs() > MAX_EXACT_INTEGER as u64) {
            return Err(invalid_policy(format!(
                "{name} {axis} coordinate exceeds the exact integer range"
            )));
        }
    }
    if minimum.zip(maximum).is_some_and(|(min, max)| min > max) {
        return Err(invalid_policy(format!(
            "minimum {axis} coordinate must not exceed maximum"
        )));
    }
    Ok(())
}

fn has_duplicate_modifiers(modifiers: &[KeyModifier]) -> bool {
    modifiers
        .iter()
        .enumerate()
        .any(|(index, modifier)| modifiers[..index].contains(modifier))
}

fn truncate_chars(value: &mut String, limit: usize) {
    if let Some((byte_index, _)) = value.char_indices().nth(limit) {
        value.truncate(byte_index);
    }
}

fn invalid_request(message: impl Into<String>) -> ProtocolError {
    ProtocolError::new(ErrorCode::InvalidRequest, message, false)
}

fn invalid_policy(message: impl Into<String>) -> ProtocolError {
    ProtocolError::new(ErrorCode::InvalidPolicy, message, false)
}

fn invalid_path(message: impl Into<String>) -> ProtocolError {
    ProtocolError::new(ErrorCode::InvalidPath, message, false)
}

fn application_not_allowed(application: &str) -> ProtocolError {
    ProtocolError::new(
        ErrorCode::ApplicationNotAllowed,
        format!("application {application:?} is not admitted by this request"),
        false,
    )
}

fn protocol_violation(message: impl Into<String>) -> ProtocolError {
    ProtocolError::new(ErrorCode::ProtocolViolation, message, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> Policy {
        Policy {
            application_access: ComputerUseApplicationAccess::Allowlist,
            allowed_applications: vec!["com.example.Editor".to_owned()],
            min_coordinate_x: Some(-1_920),
            min_coordinate_y: Some(0),
            max_coordinate_x: Some(3_840),
            max_coordinate_y: Some(2_160),
            max_text_chars: MAX_TEXT_CHARS,
        }
    }

    #[test]
    fn action_table_is_canonical_and_unique() {
        let names: Vec<_> = ActionKind::ALL
            .iter()
            .map(|action| action.as_str())
            .collect();
        assert_eq!(names.len(), 11);
        for (index, name) in names.iter().enumerate() {
            assert!(!names[..index].contains(name));
        }
    }

    #[test]
    fn tagged_action_round_trip_rejects_unknown_fields() {
        let action = Action::Click {
            x: 12.5,
            y: 24.0,
            button: MouseButton::Right,
            expected_application: "com.example.Editor".to_owned(),
        };
        let encoded = serde_json::to_string(&action).expect("test action serializes");
        assert!(encoded.contains("\"type\":\"click\""));
        let decoded: Action = serde_json::from_str(&encoded).expect("test action deserializes");
        assert_eq!(decoded, action);

        let unknown = r#"{"type":"click","x":1,"y":2,"unknown":true}"#;
        assert!(serde_json::from_str::<Action>(unknown).is_err());
        let missing_target = r#"{"type":"click","x":1,"y":2}"#;
        assert!(serde_json::from_str::<Action>(missing_target).is_err());
    }

    #[test]
    fn policy_accepts_negative_minimum_and_rejects_out_of_bounds_points() {
        let action = Action::MouseMove {
            x: -1_919.0,
            y: 20.0,
            expected_application: "com.example.Editor".to_owned(),
        };
        assert!(action.validate(&policy()).is_ok());

        let action = Action::MouseMove {
            x: -1_921.0,
            y: 20.0,
            expected_application: "com.example.Editor".to_owned(),
        };
        let error = action
            .validate(&policy())
            .expect_err("point is out of bounds");
        assert_eq!(error.code, ErrorCode::InvalidCoordinate);
    }

    #[test]
    fn expected_application_must_come_from_resolved_policy() {
        let action = Action::KeyPress {
            key: Key::Enter,
            modifiers: Vec::new(),
            expected_application: "com.example.Other".to_owned(),
        };
        let error = action
            .validate(&policy())
            .expect_err("unlisted expected application must fail");
        assert_eq!(error.code, ErrorCode::ApplicationNotAllowed);
    }

    #[test]
    fn desktop_access_accepts_any_exact_target_without_a_wildcard() {
        let mut desktop = policy();
        desktop.application_access = ComputerUseApplicationAccess::Desktop;
        desktop.allowed_applications.clear();
        let action = Action::KeyPress {
            key: Key::Enter,
            modifiers: Vec::new(),
            expected_application: "com.example.Other".to_owned(),
        };
        assert!(action.validate(&desktop).is_ok());
        assert!(desktop.allows(&ApplicationIdentity {
            name: "Other".to_owned(),
            bundle_id: Some("com.example.Other".to_owned()),
            pid: 44,
        }));
    }

    #[test]
    fn desktop_access_rejects_an_ambiguous_allowlist_and_wildcards() {
        let mut desktop = policy();
        desktop.application_access = ComputerUseApplicationAccess::Desktop;
        assert_eq!(
            desktop
                .validate()
                .expect_err("desktop mode must not carry an allowlist")
                .code,
            ErrorCode::InvalidPolicy
        );

        let mut wildcard = policy();
        wildcard.allowed_applications = vec!["*".to_owned()];
        assert_eq!(
            wildcard
                .validate()
                .expect_err("wildcards are not application identities")
                .code,
            ErrorCode::InvalidPolicy
        );
    }

    #[test]
    fn bundle_selectors_cannot_match_spoofed_display_names() {
        let spoof = ApplicationIdentity {
            name: "com.example.Editor".to_owned(),
            bundle_id: Some("com.attacker.Spoof".to_owned()),
            pid: 42,
        };
        assert!(!spoof.matches("com.example.Editor"));

        let expected = ApplicationIdentity {
            name: "Editor".to_owned(),
            bundle_id: Some("com.example.Editor".to_owned()),
            pid: 43,
        };
        assert!(expected.matches("com.example.Editor"));
        assert!(expected.matches("Editor"));
    }

    #[test]
    fn text_is_bounded_by_policy_characters_and_protocol_bytes() {
        let mut restricted = policy();
        restricted.max_text_chars = 2;
        let action = Action::TypeText {
            text: "abc".to_owned(),
            expected_application: "com.example.Editor".to_owned(),
        };
        assert_eq!(
            action
                .validate(&restricted)
                .expect_err("text exceeds character policy")
                .code,
            ErrorCode::InvalidRequest
        );

        let action = Action::TypeText {
            text: "safe\nenter".to_owned(),
            expected_application: "com.example.Editor".to_owned(),
        };
        assert_eq!(
            action
                .validate(&policy())
                .expect_err("control characters require explicit key actions")
                .code,
            ErrorCode::InvalidRequest
        );
    }

    #[test]
    fn response_must_match_action_and_screenshot_path() {
        let request = Request::new(
            Uuid::new_v4(),
            Action::Screenshot {
                application: "com.example.Editor".to_owned(),
                path: PathBuf::from("/tmp/zeroclaw-test.png"),
            },
            policy(),
        );
        let response = Response::success(
            request.request_id,
            ResponseData::Screenshot {
                path: PathBuf::from("/tmp/different.png"),
                display_bounds: Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 10.0,
                    height: 10.0,
                },
                pixel_width: 20,
                pixel_height: 20,
            },
        );
        assert_eq!(
            response
                .validate_for(&request)
                .expect_err("mismatched path must fail")
                .code,
            ErrorCode::ProtocolViolation
        );
    }

    #[test]
    fn fresh_confirmation_is_derived_from_action_table() {
        assert!(!ActionKind::ListApplications.requires_fresh_confirmation());
        assert!(!ActionKind::Inspect.requires_fresh_confirmation());
        assert!(ActionKind::Screenshot.requires_fresh_confirmation());
        assert!(ActionKind::PressElement.requires_fresh_confirmation());
        assert!(ActionKind::MouseMove.requires_fresh_confirmation());
        assert!(
            ActionKind::Click
                .model_required_fields()
                .contains(&"expected_application")
        );
        assert!(
            !ActionKind::Focus
                .model_required_fields()
                .contains(&"expected_application")
        );
    }
}
