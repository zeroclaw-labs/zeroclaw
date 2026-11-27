//! macOS backend for the `computer_use` tool.
//!
//! The module is compiled on macOS and in test builds on every platform.
//! On macOS it carries the CoreGraphics input synthesis and the
//! `screencapture` capture path; everywhere else it carries only the
//! platform-independent helpers. The keycode table, the capture argument
//! builder, the display ordering, and the typing segmentation therefore stay
//! under unit test on every CI runner, including Linux hosts that cannot link
//! an Apple framework.
//!
//! The helpers live at the top level of the module rather than inside a
//! nested `mod`, so a test build compiles them without pulling in the
//! framework bindings below.

use std::ffi::OsString;
use std::path::Path;

use super::backend::{DisplayInfo, KeyCode, LogicalPoint, Modifiers};

#[cfg(target_os = "macos")]
use super::backend::{BackendError, ComputerUseBackend, KeyChord, MouseButton};
#[cfg(target_os = "macos")]
use core::ffi::c_void;
#[cfg(target_os = "macos")]
use std::ffi::c_ulong;
#[cfg(target_os = "macos")]
use std::time::Duration;

#[cfg(target_os = "macos")]
use async_trait::async_trait;

/// `kCGEventFlagMaskShift`.
const KCG_EVENT_FLAG_MASK_SHIFT: u64 = 0x0002_0000;
/// `kCGEventFlagMaskControl`.
const KCG_EVENT_FLAG_MASK_CONTROL: u64 = 0x0004_0000;
/// `kCGEventFlagMaskAlternate`.
const KCG_EVENT_FLAG_MASK_ALTERNATE: u64 = 0x0008_0000;
/// `kCGEventFlagMaskCommand`.
const KCG_EVENT_FLAG_MASK_COMMAND: u64 = 0x0010_0000;

/// Longest run of non-ASCII text posted in one keyboard event, in UTF-16 code units.
///
/// CoreGraphics drops text past a moderate length, so non-ASCII runs are posted
/// in bounded chunks while ASCII characters use real ANSI virtual keycodes.
const MAX_UNICODE_CHUNK: usize = 20;

/// The virtual keycode of Return.
const KCG_VK_RETURN: u16 = 0x24;

/// The virtual keycode of Tab.
const KCG_VK_TAB: u16 = 0x30;

/// The virtual keycode of Space.
const KCG_VK_SPACE: u16 = 0x31;

/// The macOS virtual keycode and whether the key needs Shift held.
///
/// `None` means the key has no US ANSI virtual keycode, which happens for
/// characters outside the ASCII set that the tool layer never produces.
pub(crate) fn key_code_to_macos_vk(key: KeyCode) -> Option<(u16, bool)> {
    match key {
        KeyCode::Char(c) => char_to_macos_vk(c),
        KeyCode::Return => Some((0x24, false)),
        KeyCode::Tab => Some((0x30, false)),
        KeyCode::Space => Some((0x31, false)),
        KeyCode::BackSpace => Some((0x33, false)),
        KeyCode::Escape => Some((0x35, false)),
        KeyCode::Delete => Some((0x75, false)),
        KeyCode::Home => Some((0x73, false)),
        KeyCode::PageUp => Some((0x74, false)),
        KeyCode::End => Some((0x77, false)),
        KeyCode::PageDown => Some((0x79, false)),
        KeyCode::Left => Some((0x7B, false)),
        KeyCode::Right => Some((0x7C, false)),
        KeyCode::Down => Some((0x7D, false)),
        KeyCode::Up => Some((0x7E, false)),
        KeyCode::F1 => Some((0x7A, false)),
        KeyCode::F2 => Some((0x78, false)),
        KeyCode::F3 => Some((0x63, false)),
        KeyCode::F4 => Some((0x76, false)),
        KeyCode::F5 => Some((0x60, false)),
        KeyCode::F6 => Some((0x61, false)),
        KeyCode::F7 => Some((0x62, false)),
        KeyCode::F8 => Some((0x64, false)),
        KeyCode::F9 => Some((0x65, false)),
        KeyCode::F10 => Some((0x6D, false)),
        KeyCode::F11 => Some((0x67, false)),
        KeyCode::F12 => Some((0x6F, false)),
    }
}

/// The macOS virtual keycode of one character, and whether it needs Shift.
///
/// Uppercase letters resolve to the lowercase keycode with Shift implied, so
/// a caller that builds a `KeyCode::Char` directly still types the letter
/// rather than the lowercase one.
fn char_to_macos_vk(c: char) -> Option<(u16, bool)> {
    if c.is_ascii_uppercase() {
        return char_to_macos_vk(c.to_ascii_lowercase()).map(|(vk, _)| (vk, true));
    }
    match c {
        // Shifted punctuation shares the keycode of the digit or the
        // punctuation mark below it and needs the Shift modifier held.
        '!' => Some((0x12, true)),
        '@' => Some((0x13, true)),
        '#' => Some((0x14, true)),
        '$' => Some((0x15, true)),
        '%' => Some((0x17, true)),
        '^' => Some((0x16, true)),
        '&' => Some((0x1A, true)),
        '*' => Some((0x1C, true)),
        '(' => Some((0x19, true)),
        ')' => Some((0x1D, true)),
        '_' => Some((0x1B, true)),
        '+' => Some((0x18, true)),
        '{' => Some((0x21, true)),
        '}' => Some((0x1E, true)),
        '|' => Some((0x2A, true)),
        ':' => Some((0x29, true)),
        '"' => Some((0x27, true)),
        '<' => Some((0x2B, true)),
        '>' => Some((0x2F, true)),
        '?' => Some((0x2C, true)),
        '~' => Some((0x32, true)),
        _ => char_to_macos_vk_unshifted(c),
    }
}

/// The macOS virtual keycode of one unshifted character.
fn char_to_macos_vk_unshifted(c: char) -> Option<(u16, bool)> {
    let vk = match c {
        'a' => 0x00,
        's' => 0x01,
        'd' => 0x02,
        'f' => 0x03,
        'h' => 0x04,
        'g' => 0x05,
        'z' => 0x06,
        'x' => 0x07,
        'c' => 0x08,
        'v' => 0x09,
        'b' => 0x0B,
        'q' => 0x0C,
        'w' => 0x0D,
        'e' => 0x0E,
        'r' => 0x0F,
        'y' => 0x10,
        't' => 0x11,
        'o' => 0x1F,
        'u' => 0x20,
        'i' => 0x22,
        'p' => 0x23,
        'l' => 0x25,
        'j' => 0x26,
        'k' => 0x28,
        'n' => 0x2D,
        'm' => 0x2E,
        '1' => 0x12,
        '2' => 0x13,
        '3' => 0x14,
        '4' => 0x15,
        '6' => 0x16,
        '5' => 0x17,
        '9' => 0x19,
        '7' => 0x1A,
        '8' => 0x1C,
        '0' => 0x1D,
        '=' => 0x18,
        '-' => 0x1B,
        ']' => 0x1E,
        '[' => 0x21,
        '\'' => 0x27,
        ';' => 0x29,
        '\\' => 0x2A,
        ',' => 0x2B,
        '/' => 0x2C,
        '.' => 0x2F,
        '`' => 0x32,
        _ => return None,
    };
    Some((vk, false))
}

/// The CoreGraphics event flags for a set of modifiers.
///
/// `needs_shift` is the implicit Shift of a shifted character such as `!`,
/// which is held in addition to the modifiers the caller asked for.
pub(crate) fn modifiers_to_cg_flags(modifiers: Modifiers, needs_shift: bool) -> u64 {
    let mut flags = 0u64;
    if modifiers.command {
        flags |= KCG_EVENT_FLAG_MASK_COMMAND;
    }
    if modifiers.shift || needs_shift {
        flags |= KCG_EVENT_FLAG_MASK_SHIFT;
    }
    if modifiers.alt {
        flags |= KCG_EVENT_FLAG_MASK_ALTERNATE;
    }
    if modifiers.control {
        flags |= KCG_EVENT_FLAG_MASK_CONTROL;
    }
    flags
}

/// The `screencapture` arguments that capture one display to `output_path`.
///
/// The rectangle is expressed in the global logical-point space of the
/// virtual desktop, so a secondary display placed left of or above the
/// primary one carries a negative origin.
pub(crate) fn screencapture_args(display: &DisplayInfo, output_path: &Path) -> Vec<OsString> {
    vec![
        OsString::from("-x"),
        OsString::from("-t"),
        OsString::from("png"),
        // `screencapture` is only invoked with a display that survived
        // `order_displays` (positive finite dimensions), and clamping to at
        // least 1 point prevents a sub-0.5pt display from emitting a 0-size
        // capture rectangle.
        OsString::from(format!(
            "-R{},{},{},{}",
            display.origin_x.round() as i64,
            display.origin_y.round() as i64,
            (display.logical_w.round() as i64).max(1),
            (display.logical_h.round() as i64).max(1),
        )),
        output_path.as_os_str().to_owned(),
    ]
}

/// Order raw displays into the tool's display list.
///
/// The main display is pinned to index `0` and marked primary, and the
/// remaining active displays keep their order behind it with sequential
/// indices. A display whose size is not a positive finite number is dropped
/// rather than handed to the tool, which cannot scale it. When the main
/// display itself is unusable, the first surviving display takes index `0`,
/// because index `0` is the primary display for every caller.
pub(crate) fn order_displays(
    raw: Vec<(u32, f64, f64, f64, f64)>,
    main_id: u32,
) -> Vec<DisplayInfo> {
    let mut active: Vec<(u32, f64, f64, f64, f64)> = raw
        .into_iter()
        .filter(|(_, origin_x, origin_y, logical_w, logical_h)| {
            origin_x.is_finite()
                && origin_y.is_finite()
                && logical_w.is_finite()
                && logical_h.is_finite()
                && *logical_w > 0.0
                && *logical_h > 0.0
        })
        .collect();

    if let Some(position) = active.iter().position(|display| display.0 == main_id) {
        let main = active.remove(position);
        active.insert(0, main);
    }

    active
        .into_iter()
        .enumerate()
        .map(
            |(index, (native_id, origin_x, origin_y, logical_w, logical_h))| DisplayInfo {
                index: u32::try_from(index).unwrap_or(u32::MAX),
                native_id,
                origin_x,
                origin_y,
                logical_w,
                logical_h,
                is_primary: index == 0,
            },
        )
        .collect()
}

/// One piece of a typed text run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TypeSegment {
    /// A hardware ANSI virtual keycode press with optional Shift flag.
    ///
    /// Used for all printable ASCII characters, space, Return, and Tab so
    /// non-AppKit/canvas apps (Skia, games, Electron/custom widgets) that read
    /// hardware virtual keycodes instead of synthetic Unicode strings receive
    /// genuine key events.
    KeyStroke { virtual_key: u16, shift: bool },
    /// A run of non-ASCII UTF-16 code units to post via
    /// `CGEventKeyboardSetUnicodeString` on key-down.
    UnicodeChunk(Vec<u16>),
}

/// Split typed text into the events that reproduce it.
///
/// Line endings are normalized so `\r\n` produces one Return press rather than
/// two. Every character with a macOS ANSI virtual keycode (letters, digits,
/// punctuation, space, Return, Tab) becomes a [`TypeSegment::KeyStroke`]. Any
/// remaining non-ASCII character is grouped into [`TypeSegment::UnicodeChunk`]
/// runs of at most [`MAX_UNICODE_CHUNK`] UTF-16 code units, cut only between
/// characters so a surrogate pair is never split across two events.
pub(crate) fn segment_text_for_typing(text: &str) -> Vec<TypeSegment> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut segments = Vec::new();
    let mut chunk: Vec<u16> = Vec::new();
    let mut buffer = [0u16; 2];

    for ch in normalized.chars() {
        let hardware_key = match ch {
            '\n' => Some((KCG_VK_RETURN, false)),
            '\t' => Some((KCG_VK_TAB, false)),
            ' ' => Some((KCG_VK_SPACE, false)),
            other => char_to_macos_vk(other),
        };
        if let Some((virtual_key, shift)) = hardware_key {
            flush_chunk(&mut chunk, &mut segments);
            segments.push(TypeSegment::KeyStroke { virtual_key, shift });
        } else {
            let encoded = ch.encode_utf16(&mut buffer);
            if chunk.len() + encoded.len() > MAX_UNICODE_CHUNK {
                flush_chunk(&mut chunk, &mut segments);
            }
            chunk.extend_from_slice(encoded);
        }
    }
    flush_chunk(&mut chunk, &mut segments);
    segments
}

/// Move the pending text run into its own segment.
fn flush_chunk(chunk: &mut Vec<u16>, segments: &mut Vec<TypeSegment>) {
    if !chunk.is_empty() {
        segments.push(TypeSegment::UnicodeChunk(std::mem::take(chunk)));
    }
}

/// Interpolate the intermediate drag points from `from` to `to` across `steps`
/// increments (inclusive of `to`, exclusive of `from` since mouse-down already
/// occurred at `from`). `steps` is clamped to at least 1.
pub(crate) fn interpolate_drag_points(
    from: LogicalPoint,
    to: LogicalPoint,
    steps: u32,
) -> Vec<LogicalPoint> {
    let count = steps.max(1);
    let mut points = Vec::with_capacity(count as usize);
    for step in 1..=count {
        if step == count {
            points.push(to);
        } else {
            let t = f64::from(step) / f64::from(count);
            points.push(LogicalPoint {
                x: from.x + (to.x - from.x) * t,
                y: from.y + (to.y - from.y) * t,
            });
        }
    }
    points
}

// ── macOS platform backend ─────────────────────────────────────────────────

/// The macOS capture and input backend.
#[cfg(target_os = "macos")]
pub struct MacOsBackend {
    /// Deadline for one `screencapture` invocation, in seconds.
    action_timeout_secs: u64,
}

#[cfg(target_os = "macos")]
impl MacOsBackend {
    /// A backend whose screenshot captures are bounded by `action_timeout_secs`.
    pub fn new(action_timeout_secs: u64) -> Self {
        Self {
            action_timeout_secs,
        }
    }
}

/// The system frameworks the backend talks to.
#[cfg(target_os = "macos")]
mod ffi {
    use core::ffi::{c_double, c_void};
    use std::ffi::c_ulong;

    /// `CGPoint`, the CoreGraphics point type.
    #[repr(C)]
    #[derive(Debug, Clone, Copy, Default)]
    pub struct CGPoint {
        pub x: c_double,
        pub y: c_double,
    }

    /// `CGSize`, the CoreGraphics size type.
    #[repr(C)]
    #[derive(Debug, Clone, Copy, Default)]
    pub struct CGSize {
        pub width: c_double,
        pub height: c_double,
    }

    /// `CGRect`, the CoreGraphics rectangle type.
    #[repr(C)]
    #[derive(Debug, Clone, Copy, Default)]
    pub struct CGRect {
        pub origin: CGPoint,
        pub size: CGSize,
    }

    // SAFETY: the declarations below match the CoreGraphics event and display
    // API in Apple's SDK. The framework is linked by name on macOS, the only
    // target that compiles this module.
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        pub(super) fn CGMainDisplayID() -> u32;
        pub(super) fn CGGetActiveDisplayList(
            max_displays: u32,
            active_displays: *mut u32,
            display_count: *mut u32,
        ) -> i32;
        pub(super) fn CGDisplayBounds(display: u32) -> CGRect;
        pub(super) fn CGPreflightScreenCaptureAccess() -> bool;
        pub(super) fn CGEventCreateMouseEvent(
            source: *const c_void,
            mouse_type: u32,
            mouse_cursor_position: CGPoint,
            mouse_button: u32,
        ) -> *mut c_void;
        pub(super) fn CGEventCreateScrollWheelEvent(
            source: *const c_void,
            units: u32,
            wheel_count: u32,
            wheel1: i32,
            ...
        ) -> *mut c_void;
        pub(super) fn CGEventCreateKeyboardEvent(
            source: *const c_void,
            virtual_key: u16,
            key_down: bool,
        ) -> *mut c_void;
        pub(super) fn CGEventKeyboardSetUnicodeString(
            event: *mut c_void,
            string_length: c_ulong,
            unicode_string: *const u16,
        );
        pub(super) fn CGEventSetIntegerValueField(event: *mut c_void, field: u32, value: i64);
        pub(super) fn CGEventSetFlags(event: *mut c_void, flags: u64);
        pub(super) fn CGEventPost(tap: u32, event: *mut c_void);
    }

    // SAFETY: `CFRelease` releases a CoreFoundation reference and takes the
    // generic `CFTypeRef` pointer.
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        pub(super) fn CFRelease(cf: *const c_void);
    }

    // SAFETY: `AXIsProcessTrusted` is a parameter-free query in the
    // ApplicationServices accessibility API.
    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        pub(super) fn AXIsProcessTrusted() -> bool;
    }
}

/// `kCGHIDEventTap`, the event tap that posts into the focused application.
#[cfg(target_os = "macos")]
const KCG_HID_EVENT_TAP: u32 = 0;

/// `kCGEventLeftMouseDown`.
#[cfg(target_os = "macos")]
const KCG_EVENT_LEFT_MOUSE_DOWN: u32 = 1;

/// `kCGEventLeftMouseUp`.
#[cfg(target_os = "macos")]
const KCG_EVENT_LEFT_MOUSE_UP: u32 = 2;

/// `kCGEventRightMouseDown`.
#[cfg(target_os = "macos")]
const KCG_EVENT_RIGHT_MOUSE_DOWN: u32 = 3;

/// `kCGEventRightMouseUp`.
#[cfg(target_os = "macos")]
const KCG_EVENT_RIGHT_MOUSE_UP: u32 = 4;

/// `kCGEventMouseMoved`.
#[cfg(target_os = "macos")]
const KCG_EVENT_MOUSE_MOVED: u32 = 5;

/// `kCGEventLeftMouseDragged`.
#[cfg(target_os = "macos")]
const KCG_EVENT_LEFT_MOUSE_DRAGGED: u32 = 6;

/// `kCGEventRightMouseDragged`.
#[cfg(target_os = "macos")]
const KCG_EVENT_RIGHT_MOUSE_DRAGGED: u32 = 7;

/// `kCGEventOtherMouseDown`.
#[cfg(target_os = "macos")]
const KCG_EVENT_OTHER_MOUSE_DOWN: u32 = 25;

/// `kCGEventOtherMouseUp`.
#[cfg(target_os = "macos")]
const KCG_EVENT_OTHER_MOUSE_UP: u32 = 26;

/// `kCGEventOtherMouseDragged`.
#[cfg(target_os = "macos")]
const KCG_EVENT_OTHER_MOUSE_DRAGGED: u32 = 27;

/// `kCGMouseButtonLeft`.
#[cfg(target_os = "macos")]
const KCG_MOUSE_BUTTON_LEFT: u32 = 0;

/// `kCGMouseButtonRight`.
#[cfg(target_os = "macos")]
const KCG_MOUSE_BUTTON_RIGHT: u32 = 1;

/// `kCGMouseButtonCenter`.
#[cfg(target_os = "macos")]
const KCG_MOUSE_BUTTON_CENTER: u32 = 2;

/// `kCGMouseEventClickState`, the integer field that carries the click count.
#[cfg(target_os = "macos")]
const KCG_MOUSE_EVENT_CLICK_STATE: u32 = 1;

/// `kCGScrollEventUnitLine`, the wheel unit that counts lines.
#[cfg(target_os = "macos")]
const KCG_SCROLL_EVENT_UNIT_LINE: u32 = 1;

/// The largest number of displays a single query asks CoreGraphics for.
#[cfg(target_os = "macos")]
const MAX_ACTIVE_DISPLAYS: usize = 16;

/// The path of the macOS capture tool.
#[cfg(target_os = "macos")]
const SCREENCAPTURE_PATH: &str = "/usr/sbin/screencapture";

/// The pause between the two halves of a double click.
#[cfg(target_os = "macos")]
const MULTI_CLICK_INTERVAL: Duration = Duration::from_millis(25);

/// The pause between interpolated steps of a mouse drag.
#[cfg(target_os = "macos")]
const DRAG_STEP_INTERVAL: Duration = Duration::from_millis(10);

/// The pause between the key-down and key-up of one key press.
#[cfg(target_os = "macos")]
const KEY_PRESS_INTERVAL: Duration = Duration::from_millis(10);

/// The pause between typed keystrokes or Unicode chunks.
#[cfg(target_os = "macos")]
const TYPE_CHUNK_INTERVAL: Duration = Duration::from_millis(2);

/// An owned CoreGraphics event that is released when it is dropped.
#[cfg(target_os = "macos")]
struct CgEventGuard(*mut c_void);

#[cfg(target_os = "macos")]
impl CgEventGuard {
    /// Take ownership of an event, or report that CoreGraphics refused it.
    fn new(event: *mut c_void, what: &str) -> Result<Self, BackendError> {
        if event.is_null() {
            Err(BackendError::InputFailed(format!(
                "CoreGraphics refused to create {what}"
            )))
        } else {
            Ok(Self(event))
        }
    }
}

#[cfg(target_os = "macos")]
impl Drop for CgEventGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { ffi::CFRelease(self.0.cast_const()) };
        }
    }
}

#[cfg(target_os = "macos")]
impl MacOsBackend {
    /// Fail unless the process may control the accessibility API.
    fn ensure_accessibility_trusted() -> Result<(), BackendError> {
        if unsafe { ffi::AXIsProcessTrusted() } {
            Ok(())
        } else {
            Err(BackendError::PermissionDenied {
                permission: "Accessibility",
                instructions:
                    "Grant Accessibility permission to ZeroClaw in System Settings -> Privacy & Security -> Accessibility"
                        .into(),
            })
        }
    }

    /// Fail unless the process may read the screen.
    fn ensure_screen_recording_allowed() -> Result<(), BackendError> {
        if unsafe { ffi::CGPreflightScreenCaptureAccess() } {
            Ok(())
        } else {
            Err(BackendError::PermissionDenied {
                permission: "Screen Recording",
                instructions:
                    "Grant Screen Recording permission to ZeroClaw in System Settings -> Privacy & Security -> Screen Recording"
                        .into(),
            })
        }
    }

    /// Post one event into the focused application.
    fn post_event(event: &CgEventGuard) {
        unsafe { ffi::CGEventPost(KCG_HID_EVENT_TAP, event.0) };
    }

    /// Move the cursor to a global logical point.
    fn move_cursor_to(point: LogicalPoint) -> Result<(), BackendError> {
        let cursor = ffi::CGPoint {
            x: point.x,
            y: point.y,
        };
        let event = CgEventGuard::new(
            unsafe {
                ffi::CGEventCreateMouseEvent(
                    std::ptr::null(),
                    KCG_EVENT_MOUSE_MOVED,
                    cursor,
                    KCG_MOUSE_BUTTON_LEFT,
                )
            },
            "a mouse-moved event",
        )?;
        Self::post_event(&event);
        Ok(())
    }

    /// Press one mouse button once at a global logical point.
    fn press_mouse_button(
        point: LogicalPoint,
        button: MouseButton,
        click_state: u8,
    ) -> Result<(), BackendError> {
        let cursor = ffi::CGPoint {
            x: point.x,
            y: point.y,
        };
        let (down_type, up_type, cg_button) = match button {
            MouseButton::Left => (
                KCG_EVENT_LEFT_MOUSE_DOWN,
                KCG_EVENT_LEFT_MOUSE_UP,
                KCG_MOUSE_BUTTON_LEFT,
            ),
            MouseButton::Right => (
                KCG_EVENT_RIGHT_MOUSE_DOWN,
                KCG_EVENT_RIGHT_MOUSE_UP,
                KCG_MOUSE_BUTTON_RIGHT,
            ),
            MouseButton::Middle => (
                KCG_EVENT_OTHER_MOUSE_DOWN,
                KCG_EVENT_OTHER_MOUSE_UP,
                KCG_MOUSE_BUTTON_CENTER,
            ),
        };

        let down = CgEventGuard::new(
            unsafe { ffi::CGEventCreateMouseEvent(std::ptr::null(), down_type, cursor, cg_button) },
            "a mouse-down event",
        )?;
        unsafe {
            ffi::CGEventSetIntegerValueField(
                down.0,
                KCG_MOUSE_EVENT_CLICK_STATE,
                i64::from(click_state),
            )
        };
        Self::post_event(&down);

        let up = CgEventGuard::new(
            unsafe { ffi::CGEventCreateMouseEvent(std::ptr::null(), up_type, cursor, cg_button) },
            "a mouse-up event",
        )?;
        unsafe {
            ffi::CGEventSetIntegerValueField(
                up.0,
                KCG_MOUSE_EVENT_CLICK_STATE,
                i64::from(click_state),
            )
        };
        Self::post_event(&up);
        Ok(())
    }

    /// Click a mouse button one or more times at a global logical point.
    fn click_at(point: LogicalPoint, button: MouseButton, clicks: u8) -> Result<(), BackendError> {
        Self::move_cursor_to(point)?;
        let total = clicks.max(1);
        for click_state in 1..=total {
            Self::press_mouse_button(point, button, click_state)?;
            if click_state < total {
                std::thread::sleep(MULTI_CLICK_INTERVAL);
            }
        }
        Ok(())
    }

    /// Drag from `from` to `to` while holding `button`, moving across `steps`
    /// interpolated drag events so apps that track drag deltas (vector editors,
    /// sliders, window frames) receive smooth motion.
    fn drag_between(
        from: LogicalPoint,
        to: LogicalPoint,
        button: MouseButton,
        steps: u32,
    ) -> Result<(), BackendError> {
        let (down_type, drag_type, up_type, cg_button) = match button {
            MouseButton::Left => (
                KCG_EVENT_LEFT_MOUSE_DOWN,
                KCG_EVENT_LEFT_MOUSE_DRAGGED,
                KCG_EVENT_LEFT_MOUSE_UP,
                KCG_MOUSE_BUTTON_LEFT,
            ),
            MouseButton::Right => (
                KCG_EVENT_RIGHT_MOUSE_DOWN,
                KCG_EVENT_RIGHT_MOUSE_DRAGGED,
                KCG_EVENT_RIGHT_MOUSE_UP,
                KCG_MOUSE_BUTTON_RIGHT,
            ),
            MouseButton::Middle => (
                KCG_EVENT_OTHER_MOUSE_DOWN,
                KCG_EVENT_OTHER_MOUSE_DRAGGED,
                KCG_EVENT_OTHER_MOUSE_UP,
                KCG_MOUSE_BUTTON_CENTER,
            ),
        };

        Self::move_cursor_to(from)?;

        let start = ffi::CGPoint {
            x: from.x,
            y: from.y,
        };
        let down = CgEventGuard::new(
            unsafe { ffi::CGEventCreateMouseEvent(std::ptr::null(), down_type, start, cg_button) },
            "a drag mouse-down event",
        )?;
        Self::post_event(&down);

        for point in interpolate_drag_points(from, to, steps) {
            std::thread::sleep(DRAG_STEP_INTERVAL);
            let step_pt = ffi::CGPoint {
                x: point.x,
                y: point.y,
            };
            let dragged = CgEventGuard::new(
                unsafe {
                    ffi::CGEventCreateMouseEvent(std::ptr::null(), drag_type, step_pt, cg_button)
                },
                "a mouse-dragged event",
            )?;
            Self::post_event(&dragged);
        }

        std::thread::sleep(DRAG_STEP_INTERVAL);
        let end = ffi::CGPoint { x: to.x, y: to.y };
        let up = CgEventGuard::new(
            unsafe { ffi::CGEventCreateMouseEvent(std::ptr::null(), up_type, end, cg_button) },
            "a drag mouse-up event",
        )?;
        Self::post_event(&up);
        Ok(())
    }

    /// Scroll the wheel by a line delta at a global logical point.
    fn scroll_at(point: LogicalPoint, delta_x: i32, delta_y: i32) -> Result<(), BackendError> {
        Self::move_cursor_to(point)?;
        let event = CgEventGuard::new(
            unsafe {
                ffi::CGEventCreateScrollWheelEvent(
                    std::ptr::null(),
                    KCG_SCROLL_EVENT_UNIT_LINE,
                    2,
                    delta_y.saturating_neg(),
                    delta_x.saturating_neg(),
                )
            },
            "a scroll event",
        )?;
        Self::post_event(&event);
        Ok(())
    }

    /// Post one key press and its release.
    fn post_key_stroke(virtual_key: u16, flags: u64) -> Result<(), BackendError> {
        let down = CgEventGuard::new(
            unsafe { ffi::CGEventCreateKeyboardEvent(std::ptr::null(), virtual_key, true) },
            "a key-down event",
        )?;
        unsafe { ffi::CGEventSetFlags(down.0, flags) };
        Self::post_event(&down);

        let up = CgEventGuard::new(
            unsafe { ffi::CGEventCreateKeyboardEvent(std::ptr::null(), virtual_key, false) },
            "a key-up event",
        )?;
        unsafe { ffi::CGEventSetFlags(up.0, flags) };
        Self::post_event(&up);
        Ok(())
    }

    /// Post one run of non-ASCII text as a keyboard event.
    ///
    /// The Unicode payload is attached ONLY to the key-down event; attaching it
    /// to key-up as well causes terminals such as Ghostty to insert the string
    /// twice.
    fn post_unicode_chunk(units: &[u16]) -> Result<(), BackendError> {
        let length = c_ulong::try_from(units.len())
            .map_err(|_| BackendError::InputFailed("typed text is too long to post".into()))?;

        let down = CgEventGuard::new(
            unsafe { ffi::CGEventCreateKeyboardEvent(std::ptr::null(), 0, true) },
            "a text event",
        )?;
        unsafe { ffi::CGEventKeyboardSetUnicodeString(down.0, length, units.as_ptr()) };
        Self::post_event(&down);

        let up = CgEventGuard::new(
            unsafe { ffi::CGEventCreateKeyboardEvent(std::ptr::null(), 0, false) },
            "a text event",
        )?;
        Self::post_event(&up);
        Ok(())
    }

    /// Type text into the focused application.
    fn type_text_into(text: &str) -> Result<(), BackendError> {
        for segment in segment_text_for_typing(text) {
            match segment {
                TypeSegment::KeyStroke { virtual_key, shift } => {
                    let flags = if shift { KCG_EVENT_FLAG_MASK_SHIFT } else { 0 };
                    Self::post_key_stroke(virtual_key, flags)?;
                    std::thread::sleep(TYPE_CHUNK_INTERVAL);
                }
                TypeSegment::UnicodeChunk(units) => {
                    Self::post_unicode_chunk(&units)?;
                    std::thread::sleep(TYPE_CHUNK_INTERVAL);
                }
            }
        }
        Ok(())
    }

    /// Press one key with its modifiers held.
    fn press_chord(chord: &KeyChord) -> Result<(), BackendError> {
        let Some((virtual_key, needs_shift)) = key_code_to_macos_vk(chord.key) else {
            return Err(BackendError::InputFailed(format!(
                "no macOS virtual keycode for {:?}",
                chord.key
            )));
        };
        let flags = modifiers_to_cg_flags(chord.modifiers, needs_shift);

        let down = CgEventGuard::new(
            unsafe { ffi::CGEventCreateKeyboardEvent(std::ptr::null(), virtual_key, true) },
            "a key-down event",
        )?;
        unsafe { ffi::CGEventSetFlags(down.0, flags) };
        Self::post_event(&down);

        std::thread::sleep(KEY_PRESS_INTERVAL);

        let up = CgEventGuard::new(
            unsafe { ffi::CGEventCreateKeyboardEvent(std::ptr::null(), virtual_key, false) },
            "a key-up event",
        )?;
        unsafe { ffi::CGEventSetFlags(up.0, flags) };
        Self::post_event(&up);
        Ok(())
    }
}

/// Run one blocking CoreGraphics action off the async runtime.
#[cfg(target_os = "macos")]
async fn run_input<F>(work: F) -> Result<(), BackendError>
where
    F: FnOnce() -> Result<(), BackendError> + Send + 'static,
{
    tokio::task::spawn_blocking(work).await.map_err(|error| {
        BackendError::InputFailed(format!("the input synthesis task did not finish: {error}"))
    })?
}

#[cfg(target_os = "macos")]
#[async_trait]
impl ComputerUseBackend for MacOsBackend {
    async fn list_displays(&self) -> Result<Vec<DisplayInfo>, BackendError> {
        let mut ids = [0u32; MAX_ACTIVE_DISPLAYS];
        let mut count = 0u32;
        let status = unsafe {
            ffi::CGGetActiveDisplayList(MAX_ACTIVE_DISPLAYS as u32, ids.as_mut_ptr(), &mut count)
        };
        if status != 0 {
            // The backend contract has no dedicated display-probe failure: a
            // host whose display list cannot be read cannot perform any
            // action, which is what the unsupported-platform variant means.
            return Err(BackendError::UnsupportedPlatform(format!(
                "CoreGraphics could not enumerate the active displays (error {status})"
            )));
        }

        let count = count.min(MAX_ACTIVE_DISPLAYS as u32) as usize;
        let main_id = unsafe { ffi::CGMainDisplayID() };
        let mut raw = Vec::with_capacity(count);
        for id in &ids[..count] {
            let bounds = unsafe { ffi::CGDisplayBounds(*id) };
            raw.push((
                *id,
                bounds.origin.x,
                bounds.origin.y,
                bounds.size.width,
                bounds.size.height,
            ));
        }
        Ok(order_displays(raw, main_id))
    }

    async fn capture_display(&self, display: &DisplayInfo) -> Result<Vec<u8>, BackendError> {
        MacOsBackend::ensure_screen_recording_allowed()?;

        let temp_file = tempfile::Builder::new()
            .prefix("zeroclaw-cu-")
            .suffix(".png")
            .tempfile()
            .map_err(|error| {
                BackendError::CaptureFailed(format!(
                    "could not create a temporary capture file: {error}"
                ))
            })?;
        // The temporary path keeps the file removable once the capture is
        // read, while the closed handle lets `screencapture` write the image.
        let temp_path = temp_file.into_temp_path();
        let args = screencapture_args(display, temp_path.as_ref());

        let output = tokio::time::timeout(
            Duration::from_secs(self.action_timeout_secs),
            tokio::process::Command::new(SCREENCAPTURE_PATH)
                .args(args)
                .kill_on_drop(true)
                .output(),
        )
        .await
        .map_err(|_| {
            BackendError::Timeout(format!(
                "screencapture did not finish within {} seconds",
                self.action_timeout_secs
            ))
        })?
        .map_err(|error| {
            BackendError::CaptureFailed(format!("could not run screencapture: {error}"))
        })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(BackendError::CaptureFailed(if stderr.is_empty() {
                format!("screencapture exited with {}", output.status)
            } else {
                format!("screencapture exited with {}: {stderr}", output.status)
            }));
        }

        tokio::fs::read(&temp_path).await.map_err(|error| {
            BackendError::CaptureFailed(format!("could not read the captured image: {error}"))
        })
    }

    async fn move_cursor(&self, point: LogicalPoint) -> Result<(), BackendError> {
        MacOsBackend::ensure_accessibility_trusted()?;
        run_input(move || MacOsBackend::move_cursor_to(point)).await
    }

    async fn click(
        &self,
        point: LogicalPoint,
        button: MouseButton,
        clicks: u8,
    ) -> Result<(), BackendError> {
        MacOsBackend::ensure_accessibility_trusted()?;
        run_input(move || MacOsBackend::click_at(point, button, clicks)).await
    }

    async fn drag(
        &self,
        from: LogicalPoint,
        to: LogicalPoint,
        button: MouseButton,
        steps: u32,
    ) -> Result<(), BackendError> {
        MacOsBackend::ensure_accessibility_trusted()?;
        run_input(move || MacOsBackend::drag_between(from, to, button, steps)).await
    }

    async fn scroll(
        &self,
        point: LogicalPoint,
        delta_x: i32,
        delta_y: i32,
    ) -> Result<(), BackendError> {
        MacOsBackend::ensure_accessibility_trusted()?;
        run_input(move || MacOsBackend::scroll_at(point, delta_x, delta_y)).await
    }

    async fn type_text(&self, text: &str) -> Result<(), BackendError> {
        MacOsBackend::ensure_accessibility_trusted()?;
        let owned = text.to_string();
        run_input(move || MacOsBackend::type_text_into(&owned)).await
    }

    async fn press_key(&self, chord: &KeyChord) -> Result<(), BackendError> {
        MacOsBackend::ensure_accessibility_trusted()?;
        let owned = *chord;
        run_input(move || MacOsBackend::press_chord(&owned)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::super::keys::parse_key_chord;

    /// A display with the given geometry.
    fn display(
        index: u32,
        native_id: u32,
        origin_x: f64,
        origin_y: f64,
        logical_w: f64,
        logical_h: f64,
    ) -> DisplayInfo {
        DisplayInfo {
            index,
            native_id,
            origin_x,
            origin_y,
            logical_w,
            logical_h,
            is_primary: index == 0,
        }
    }

    #[test]
    fn macos_keycode_table_covers_all_named_and_ascii_keys() {
        // Every named key the chord parser can produce.
        let named = [
            KeyCode::Return,
            KeyCode::Escape,
            KeyCode::Tab,
            KeyCode::Space,
            KeyCode::BackSpace,
            KeyCode::Delete,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::PageUp,
            KeyCode::PageDown,
            KeyCode::F1,
            KeyCode::F2,
            KeyCode::F3,
            KeyCode::F4,
            KeyCode::F5,
            KeyCode::F6,
            KeyCode::F7,
            KeyCode::F8,
            KeyCode::F9,
            KeyCode::F10,
            KeyCode::F11,
            KeyCode::F12,
        ];
        for key in named {
            assert!(
                key_code_to_macos_vk(key).is_some(),
                "{key:?} must have a macOS virtual keycode"
            );
        }

        // Every ASCII letter, in both cases.
        for letter in 'a'..='z' {
            let (vk, needs_shift) = key_code_to_macos_vk(KeyCode::Char(letter))
                .expect("a lowercase letter has a keycode");
            assert!(!needs_shift, "{letter} must not need Shift");
            let (upper_vk, upper_needs_shift) =
                key_code_to_macos_vk(KeyCode::Char(letter.to_ascii_uppercase()))
                    .expect("an uppercase letter has a keycode");
            assert_eq!(upper_vk, vk, "{letter} and its uppercase share a keycode");
            assert!(upper_needs_shift, "{letter} uppercase must need Shift");
        }

        // Every digit.
        for digit in '0'..='9' {
            let (vk, needs_shift) =
                key_code_to_macos_vk(KeyCode::Char(digit)).expect("a digit has a keycode");
            assert!(!needs_shift, "{digit} must not need Shift");
            assert!(vk <= 0x1D, "{digit} must use a digit keycode");
        }

        // Every punctuation character the chord parser accepts.
        let punctuation = [
            '`', '-', '=', '[', ']', '\\', ';', '\'', ',', '.', '/', '~', '!', '@', '#', '$', '%',
            '^', '&', '*', '(', ')', '_', '+', '{', '}', '|', ':', '"', '<', '>', '?',
        ];
        for symbol in punctuation {
            let chord = parse_key_chord(&symbol.to_string())
                .unwrap_or_else(|error| panic!("`{symbol}` must parse: {error}"));
            assert_eq!(chord.key, KeyCode::Char(symbol));
            let (vk, needs_shift) = key_code_to_macos_vk(chord.key)
                .unwrap_or_else(|| panic!("`{symbol}` must have a keycode"));
            assert!(vk > 0, "`{symbol}` must use a real keycode");
            let shifted = matches!(
                symbol,
                '~' | '!'
                    | '@'
                    | '#'
                    | '$'
                    | '%'
                    | '^'
                    | '&'
                    | '*'
                    | '('
                    | ')'
                    | '_'
                    | '+'
                    | '{'
                    | '}'
                    | '|'
                    | ':'
                    | '"'
                    | '<'
                    | '>'
                    | '?'
            );
            assert_eq!(
                needs_shift, shifted,
                "`{symbol}` must report the right Shift requirement"
            );
        }

        // The shifted characters share the keycode of the key they sit on.
        assert_eq!(key_code_to_macos_vk(KeyCode::Char('!')), Some((0x12, true)));
        assert_eq!(
            key_code_to_macos_vk(KeyCode::Char('a')),
            Some((0x00, false))
        );

        // A character outside the US ANSI layout has no virtual keycode.
        assert_eq!(key_code_to_macos_vk(KeyCode::Char('é')), None);
    }

    #[test]
    fn modifiers_to_cg_flags_combines_explicit_and_implicit_shift() {
        let none = Modifiers::default();
        assert_eq!(modifiers_to_cg_flags(none, false), 0);
        assert_eq!(modifiers_to_cg_flags(none, true), KCG_EVENT_FLAG_MASK_SHIFT);

        let explicit = Modifiers {
            command: true,
            control: true,
            alt: true,
            shift: true,
        };
        assert_eq!(
            modifiers_to_cg_flags(explicit, false),
            KCG_EVENT_FLAG_MASK_COMMAND
                | KCG_EVENT_FLAG_MASK_CONTROL
                | KCG_EVENT_FLAG_MASK_ALTERNATE
                | KCG_EVENT_FLAG_MASK_SHIFT
        );

        // An explicit Shift and an implicit one set the same bit.
        let implicit = Modifiers {
            command: true,
            ..Modifiers::default()
        };
        assert_eq!(
            modifiers_to_cg_flags(implicit, true),
            KCG_EVENT_FLAG_MASK_COMMAND | KCG_EVENT_FLAG_MASK_SHIFT
        );
    }

    #[test]
    fn screencapture_args_handles_positive_and_negative_secondary_origins() {
        let secondary = display(1, 7, -1920.0, -120.0, 1920.0, 1080.0);
        let args = screencapture_args(&secondary, Path::new("/tmp/capture.png"));
        assert_eq!(
            args,
            vec![
                OsString::from("-x"),
                OsString::from("-t"),
                OsString::from("png"),
                OsString::from("-R-1920,-120,1920,1080"),
                OsString::from("/tmp/capture.png"),
            ]
        );

        let primary = display(0, 1, 0.0, 0.0, 1512.0, 982.0);
        let args = screencapture_args(&primary, Path::new("/tmp/primary.png"));
        assert_eq!(
            args,
            vec![
                OsString::from("-x"),
                OsString::from("-t"),
                OsString::from("png"),
                OsString::from("-R0,0,1512,982"),
                OsString::from("/tmp/primary.png"),
            ]
        );

        // Fractional geometry is rounded to whole points.
        let fractional = display(1, 9, -1920.4, 0.4, 2560.0, 1440.0);
        let args = screencapture_args(&fractional, Path::new("/tmp/fractional.png"));
        assert_eq!(args[3], OsString::from("-R-1920,0,2560,1440"));
    }

    #[test]
    fn order_displays_pins_main_display_to_index_zero() {
        let raw = vec![
            (3, 0.0, 0.0, 1920.0, 1080.0),
            (1, -1920.0, 0.0, 1920.0, 1080.0),
            (2, 0.0, -1080.0, 1920.0, 1080.0),
        ];
        let ordered = order_displays(raw, 2);
        assert_eq!(ordered.len(), 3);
        assert_eq!(ordered[0].native_id, 2);
        assert_eq!(ordered[0].index, 0);
        assert!(ordered[0].is_primary);
        assert_eq!(ordered[0].origin_x, 0.0);
        assert_eq!(ordered[0].origin_y, -1080.0);
        assert_eq!(ordered[1].native_id, 3);
        assert_eq!(ordered[1].index, 1);
        assert!(!ordered[1].is_primary);
        assert_eq!(ordered[2].native_id, 1);
        assert_eq!(ordered[2].index, 2);
        assert!(!ordered[2].is_primary);
        assert_eq!(ordered[2].origin_x, -1920.0);
    }

    #[test]
    fn order_displays_drops_unusable_displays() {
        let raw = vec![
            (1, 0.0, 0.0, 1920.0, 1080.0),
            (2, 0.0, 0.0, 0.0, 1080.0),
            (3, 0.0, 0.0, 1920.0, f64::NAN),
            (4, 0.0, 0.0, -1920.0, -1080.0),
        ];
        let ordered = order_displays(raw, 1);
        assert_eq!(ordered.len(), 1);
        assert_eq!(ordered[0].native_id, 1);
        assert!(ordered[0].is_primary);
    }

    #[test]
    fn segment_text_for_typing_uses_hardware_keycodes_for_ascii_and_chunks_unicode() {
        // Printable ASCII, uppercase, shifted punctuation, space, Return, and
        // Tab all compile to real ANSI virtual keystrokes so non-AppKit apps
        // (Skia/canvas text inputs) accept them.
        assert_eq!(
            segment_text_for_typing("170"),
            vec![
                TypeSegment::KeyStroke {
                    virtual_key: 0x12,
                    shift: false,
                },
                TypeSegment::KeyStroke {
                    virtual_key: 0x1A,
                    shift: false,
                },
                TypeSegment::KeyStroke {
                    virtual_key: 0x1D,
                    shift: false,
                },
            ]
        );

        // Uppercase letters and shifted symbols carry `shift = true`.
        assert_eq!(
            segment_text_for_typing("A! "),
            vec![
                TypeSegment::KeyStroke {
                    virtual_key: 0x00,
                    shift: true,
                },
                TypeSegment::KeyStroke {
                    virtual_key: 0x12,
                    shift: true,
                },
                TypeSegment::KeyStroke {
                    virtual_key: KCG_VK_SPACE,
                    shift: false,
                },
            ]
        );

        // A carriage-return/line-feed pair is one Return, not two.
        assert_eq!(
            segment_text_for_typing("a\r\nb"),
            vec![
                TypeSegment::KeyStroke {
                    virtual_key: 0x00,
                    shift: false,
                },
                TypeSegment::KeyStroke {
                    virtual_key: KCG_VK_RETURN,
                    shift: false,
                },
                TypeSegment::KeyStroke {
                    virtual_key: 0x0B,
                    shift: false,
                },
            ]
        );

        // A standalone carriage return is a Return as well, and Tab uses vk 0x30.
        assert_eq!(
            segment_text_for_typing("a\rb\tc"),
            vec![
                TypeSegment::KeyStroke {
                    virtual_key: 0x00,
                    shift: false,
                },
                TypeSegment::KeyStroke {
                    virtual_key: KCG_VK_RETURN,
                    shift: false,
                },
                TypeSegment::KeyStroke {
                    virtual_key: 0x0B,
                    shift: false,
                },
                TypeSegment::KeyStroke {
                    virtual_key: KCG_VK_TAB,
                    shift: false,
                },
                TypeSegment::KeyStroke {
                    virtual_key: 0x08,
                    shift: false,
                },
            ]
        );

        // Non-ASCII runs fall back to bounded UTF-16 Unicode chunks without
        // ever splitting a surrogate pair across chunks.
        let long_unicode = "é".repeat(45);
        let segments = segment_text_for_typing(&long_unicode);
        assert_eq!(segments.len(), 3);
        for segment in &segments {
            match segment {
                TypeSegment::UnicodeChunk(units) => {
                    assert!(units.len() <= MAX_UNICODE_CHUNK, "chunk stays bounded");
                }
                other => panic!("expected a Unicode chunk, got {other:?}"),
            }
        }
        let rebuilt: String = segments
            .iter()
            .filter_map(|segment| match segment {
                TypeSegment::UnicodeChunk(units) => Some(String::from_utf16_lossy(units)),
                _ => None,
            })
            .collect();
        assert_eq!(rebuilt, long_unicode);

        // An emoji (2 UTF-16 code units) after 19 BMP non-ASCII code units
        // starts a new chunk instead of splitting its surrogate pair.
        let emoji = "\u{1F980}";
        let segments = segment_text_for_typing(&format!("{}{emoji}", "é".repeat(19)));
        assert_eq!(segments.len(), 2);
        match &segments[0] {
            TypeSegment::UnicodeChunk(units) => assert_eq!(units.len(), 19),
            other => panic!("expected a Unicode chunk, got {other:?}"),
        }
        match &segments[1] {
            TypeSegment::UnicodeChunk(units) => {
                assert_eq!(units.len(), 2, "the emoji keeps both code units");
                assert_eq!(String::from_utf16_lossy(units), emoji);
            }
            other => panic!("expected a Unicode chunk, got {other:?}"),
        }

        // Mixed ASCII and non-ASCII flushes cleanly in exact order.
        let mixed = format!("a{emoji}b");
        assert_eq!(
            segment_text_for_typing(&mixed),
            vec![
                TypeSegment::KeyStroke {
                    virtual_key: 0x00,
                    shift: false,
                },
                TypeSegment::UnicodeChunk(emoji.encode_utf16().collect()),
                TypeSegment::KeyStroke {
                    virtual_key: 0x0B,
                    shift: false,
                },
            ]
        );
    }

    #[test]
    fn interpolate_drag_points_reaches_target_in_requested_steps() {
        let from = LogicalPoint { x: 100.0, y: 200.0 };
        let to = LogicalPoint { x: 300.0, y: 100.0 };
        let points = interpolate_drag_points(from, to, 4);
        assert_eq!(points.len(), 4);
        assert_eq!(points[0], LogicalPoint { x: 150.0, y: 175.0 });
        assert_eq!(points[1], LogicalPoint { x: 200.0, y: 150.0 });
        assert_eq!(points[2], LogicalPoint { x: 250.0, y: 125.0 });
        assert_eq!(points[3], to);

        // Zero steps clamps to 1 direct drag step ending at `to`.
        let single = interpolate_drag_points(from, to, 0);
        assert_eq!(single, vec![to]);
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    #[ignore = "requires macOS desktop with Screen Recording permission"]
    async fn macos_live_screenshot_smoke() {
        let backend = MacOsBackend::new(15);
        let displays = backend.list_displays().await.expect("displays list");
        assert!(
            !displays.is_empty(),
            "a desktop reports at least one display"
        );
        let display = displays.first().expect("the primary display");
        let bytes = backend
            .capture_display(display)
            .await
            .expect("screenshot capture");
        assert!(bytes.starts_with(b"\x89PNG"), "the capture is a PNG");
    }
}
