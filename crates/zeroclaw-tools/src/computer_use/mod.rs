//! Platform-independent pieces of the `computer_use` desktop automation tool.
//!
//! - [`backend`] defines the GUI types a model speaks in and the
//!   [`ComputerUseBackend`] trait a platform implements. Capture and input
//!   synthesis live behind that trait, so policy and receipts stay in one place
//!   and a platform backend cannot reach around them.
//! - [`scale`] converts between the downscaled screenshot a model sees and the
//!   logical-point coordinates the OS accepts, and performs the matching PNG
//!   resize.
//! - [`keys`] parses the `cmd+shift+s` chord syntax into a [`KeyChord`].
//! - [`ComputerUseTool`] is the model-facing tool: it owns the autonomy and
//!   display policy, the coordinate mapping, and the screenshot receipts, and
//!   delegates every OS-visible step to a backend.
//!
//! A target with no capture backend compiles `stub`, whose `UnsupportedBackend`
//! reports an actionable [`BackendError::UnsupportedPlatform`] for every call,
//! and `default_backend` hands that stub out. The reachable surface is
//! therefore the same everywhere: a caller never has to branch on the host.

pub mod backend;
pub mod keys;
#[cfg(any(target_os = "macos", test))]
mod macos;
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

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use zeroclaw_api::media::{MarkerKind, RenderedMarker};
use zeroclaw_api::tool::{Tool, ToolExecutionContext, ToolOutput, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::schema::ComputerUseToolConfig;

use crate::i18n::{get_required_tool_string, get_required_tool_string_with_args};

/// The name the model calls this tool by.
const TOOL_NAME: &str = "computer_use";

/// Fluent key carrying the tool description.
const TOOL_DESCRIPTION_KEY: &str = "tool-computer-use";

/// Longest `text` payload the `type` action accepts, in characters.
const MAX_TEXT_CHARS: usize = 4096;

/// Default number of interpolated drag steps.
const DEFAULT_DRAG_STEPS: u32 = 10;

/// Maximum number of interpolated drag steps accepted by `drag`.
const MAX_DRAG_STEPS: u32 = 100;

/// Subdirectory of the agent workspace that receives screenshot files.
const SCREENSHOT_SUBDIR: &str = "computer_use";

/// Filename prefix for saved screenshots.
const SCREENSHOT_PREFIX: &str = "screenshot";

/// Every action the tool accepts, in schema order.
const ACTIONS: [&str; 8] = [
    "screenshot",
    "click",
    "drag",
    "move",
    "scroll",
    "type",
    "key",
    "wait",
];

/// How an action relates to the autonomy gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActionClass {
    /// Observational: a read-only policy still permits it.
    Observe,
    /// State-changing: needs an acting autonomy level.
    Mutate,
}

/// Classify `action`, or `None` when the tool does not support it.
///
/// One table decides both dispatch validity and the autonomy gate, so a new
/// action cannot be added to the schema while silently escaping the gate.
fn classify_action(action: &str) -> Option<ActionClass> {
    match action {
        "screenshot" | "wait" => Some(ActionClass::Observe),
        "click" | "drag" | "move" | "scroll" | "type" | "key" => Some(ActionClass::Mutate),
        _ => None,
    }
}

/// The backend this build and host can actually run.
///
/// `action_timeout_secs` bounds how long the real capture path may run: on
/// macOS it is the deadline `screencapture` gets before the capture fails as a
/// timeout. Input synthesis posts events directly and takes no timeout of its
/// own. The unsupported backend never uses the value.
#[cfg(not(target_os = "macos"))]
pub fn default_backend(action_timeout_secs: u64) -> Arc<dyn ComputerUseBackend> {
    let _ = action_timeout_secs;
    Arc::new(UnsupportedBackend::new())
}

/// The backend this build and host can actually run.
///
/// `action_timeout_secs` bounds one screen capture: the subprocess
/// `screencapture` is killed and reported as a timeout when it runs past the
/// deadline. Input synthesis posts CoreGraphics events synchronously and takes
/// no timeout of its own.
#[cfg(target_os = "macos")]
pub fn default_backend(action_timeout_secs: u64) -> Arc<dyn ComputerUseBackend> {
    Arc::new(macos::MacOsBackend::new(action_timeout_secs))
}

/// The model-facing `computer_use` tool.
///
/// Policy is decided here rather than in a backend: the autonomy gate, the
/// secondary-display gate, parameter validation, and coordinate mapping all run
/// before a backend sees the call, so an implementation cannot bypass them.
pub struct ComputerUseTool {
    security: Arc<SecurityPolicy>,
    config: ComputerUseToolConfig,
    /// Canonical per-agent workspace. Screenshots land under
    /// `<agent_workspace_dir>/computer_use/` regardless of the process working
    /// directory or the session's `security.workspace_dir`.
    agent_workspace_dir: PathBuf,
    backend: Arc<dyn ComputerUseBackend>,
    /// Monotonic sequence so two screenshots in the same second never collide.
    screenshot_seq: AtomicU64,
    description: String,
}

impl ComputerUseTool {
    pub fn new(
        security: Arc<SecurityPolicy>,
        config: ComputerUseToolConfig,
        agent_workspace_dir: PathBuf,
        backend: Arc<dyn ComputerUseBackend>,
    ) -> Self {
        Self {
            security,
            config,
            agent_workspace_dir,
            backend,
            screenshot_seq: AtomicU64::new(0),
            description: get_required_tool_string(TOOL_DESCRIPTION_KEY),
        }
    }

    /// Resolve the target display for an action.
    ///
    /// The secondary-display gate runs before `list_displays` so a blocked
    /// display costs no capture probe and no backend call at all.
    async fn resolve_display(&self, args: &Value) -> Result<(DisplayInfo, usize), ToolResult> {
        let display_index = match args.get("display") {
            None | Some(Value::Null) => 0,
            Some(value) => match value.as_u64() {
                Some(raw) => u32::try_from(raw)
                    .map_err(|_| tool_error("tool-computer-use-error-display-u32-overflow"))?,
                None => {
                    return Err(tool_error("tool-computer-use-error-display-must-be-u32"));
                }
            },
        };

        if display_index > 0 && !self.config.allow_secondary_displays {
            let display = display_index.to_string();
            return Err(tool_error_with_args(
                "tool-computer-use-error-secondary-display-blocked",
                &[("display", &display)],
            ));
        }

        let displays = match self.backend.list_displays().await {
            Ok(displays) => displays,
            Err(error) => return Err(backend_error(&error)),
        };
        if displays.is_empty() {
            return Err(tool_error("tool-computer-use-error-no-displays"));
        }
        let count = displays.len();

        match displays.iter().find(|d| d.index == display_index) {
            Some(display) => Ok((display.clone(), count)),
            None => {
                let display = display_index.to_string();
                let count = count.to_string();
                Err(tool_error_with_args(
                    "tool-computer-use-error-display-out-of-range",
                    &[("display", &display), ("count", &count)],
                ))
            }
        }
    }

    /// Capture, downscale, and persist one screenshot, returning its canonical path.
    async fn capture_screenshot(
        &self,
        display: &DisplayInfo,
    ) -> Result<SavedScreenshot, ToolResult> {
        let (target_w, target_h) = match scale::scaled_image_size(
            display.logical_w,
            display.logical_h,
            self.config.max_dimension,
        ) {
            Ok(size) => size,
            Err(error) => return Err(scale_error(&error)),
        };

        let raw_png = match self.backend.capture_display(display).await {
            Ok(bytes) => bytes,
            Err(error) => return Err(backend_error(&error)),
        };

        let png = match scale::resize_png_to(&raw_png, target_w, target_h) {
            Ok(png) => png,
            Err(error) => return Err(scale_error(&error)),
        };

        let path = match self.save_screenshot(&png).await {
            Ok(path) => path,
            Err(error) => {
                return Err(tool_error_with_args(
                    "tool-computer-use-error-save-screenshot",
                    &[("err", &error)],
                ));
            }
        };

        Ok(SavedScreenshot {
            path,
            width: target_w,
            height: target_h,
        })
    }

    /// Write `png` into the agent workspace under a collision-free name.
    ///
    /// The bytes go to a sibling temporary file first and are renamed into
    /// place, so a reader never observes a half-written PNG. Any failure
    /// removes the temporary file rather than leaving debris behind.
    async fn save_screenshot(&self, png: &[u8]) -> Result<PathBuf, String> {
        let dir = self.agent_workspace_dir.join(SCREENSHOT_SUBDIR);
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;

        let seq = self.screenshot_seq.fetch_add(1, Ordering::Relaxed) + 1;
        let stamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
        let file_name = format!("{SCREENSHOT_PREFIX}_{stamp}_{seq:04}.png");
        let final_path = dir.join(&file_name);
        let temp_path = dir.join(format!(
            "{file_name}.{}.{}.tmp",
            std::process::id(),
            uuid::Uuid::new_v4().simple(),
        ));

        if let Err(error) = write_new_file(&temp_path, png).await {
            remove_if_present(&temp_path).await;
            return Err(format!("cannot write {}: {error}", temp_path.display()));
        }
        if let Err(error) = tokio::fs::rename(&temp_path, &final_path).await {
            remove_if_present(&temp_path).await;
            return Err(format!(
                "cannot move {} into place: {error}",
                temp_path.display()
            ));
        }

        Ok(tokio::fs::canonicalize(&final_path)
            .await
            .unwrap_or(final_path))
    }

    /// Map a validated screenshot pixel onto the display's global logical point.
    fn to_logical(
        &self,
        display: &DisplayInfo,
        x: f64,
        y: f64,
    ) -> Result<LogicalPoint, ToolResult> {
        scale::image_to_logical(display, x, y, self.config.max_dimension)
            .map_err(|error| scale_error(&error))
    }

    async fn screenshot_action(&self, args: &Value) -> ToolResult {
        let (display, displays_count) = match self.resolve_display(args).await {
            Ok(resolved) => resolved,
            Err(result) => return result,
        };
        let saved = match self.capture_screenshot(&display).await {
            Ok(saved) => saved,
            Err(result) => return result,
        };

        let path = saved.path.to_string_lossy().to_string();
        let output = ToolOutput::json(json!({
            "action": "screenshot",
            "display": display.index,
            "width": saved.width,
            "height": saved.height,
            "logical_width": display.logical_w,
            "logical_height": display.logical_h,
            "displays_count": displays_count,
            "path": path,
        }));
        ToolResult::ok(output).with_attachment(RenderedMarker {
            target: path,
            kind: MarkerKind::Image,
        })
    }

    async fn click_action(&self, args: &Value) -> ToolResult {
        let (x, y) = match self.required_point(args) {
            Ok(point) => point,
            Err(result) => return result,
        };
        let button = match optional_button(args) {
            Ok(button) => button,
            Err(result) => return result,
        };
        let clicks = match optional_clicks(args) {
            Ok(clicks) => clicks,
            Err(result) => return result,
        };
        let (display, _) = match self.resolve_display(args).await {
            Ok(resolved) => resolved,
            Err(result) => return result,
        };
        let point = match self.to_logical(&display, x, y) {
            Ok(point) => point,
            Err(result) => return result,
        };

        if let Err(error) = self.backend.click(point, button, clicks).await {
            return backend_error(&error);
        }

        ToolResult::ok(ToolOutput::json(json!({
            "action": "click",
            "display": display.index,
            "x": x,
            "y": y,
            "button": button_label(button),
            "clicks": clicks,
        })))
    }

    async fn drag_action(&self, args: &Value) -> ToolResult {
        let from_x = match required_coordinate(args, "from_x") {
            Ok(v) => v,
            Err(result) => return result,
        };
        let from_y = match required_coordinate(args, "from_y") {
            Ok(v) => v,
            Err(result) => return result,
        };
        let to_x = match required_coordinate(args, "to_x") {
            Ok(v) => v,
            Err(result) => return result,
        };
        let to_y = match required_coordinate(args, "to_y") {
            Ok(v) => v,
            Err(result) => return result,
        };
        let button = match optional_button(args) {
            Ok(button) => button,
            Err(result) => return result,
        };
        let steps = match optional_drag_steps(args) {
            Ok(steps) => steps,
            Err(result) => return result,
        };
        let (display, _) = match self.resolve_display(args).await {
            Ok(resolved) => resolved,
            Err(result) => return result,
        };
        let from = match self.to_logical(&display, from_x, from_y) {
            Ok(point) => point,
            Err(result) => return result,
        };
        let to = match self.to_logical(&display, to_x, to_y) {
            Ok(point) => point,
            Err(result) => return result,
        };

        if let Err(error) = self.backend.drag(from, to, button, steps).await {
            return backend_error(&error);
        }

        ToolResult::ok(ToolOutput::json(json!({
            "action": "drag",
            "display": display.index,
            "from_x": from_x,
            "from_y": from_y,
            "to_x": to_x,
            "to_y": to_y,
            "button": button_label(button),
            "steps": steps,
        })))
    }

    async fn move_action(&self, args: &Value) -> ToolResult {
        let (x, y) = match self.required_point(args) {
            Ok(point) => point,
            Err(result) => return result,
        };
        let (display, _) = match self.resolve_display(args).await {
            Ok(resolved) => resolved,
            Err(result) => return result,
        };
        let point = match self.to_logical(&display, x, y) {
            Ok(point) => point,
            Err(result) => return result,
        };

        if let Err(error) = self.backend.move_cursor(point).await {
            return backend_error(&error);
        }

        ToolResult::ok(ToolOutput::json(json!({
            "action": "move",
            "display": display.index,
            "x": x,
            "y": y,
        })))
    }

    async fn scroll_action(&self, args: &Value) -> ToolResult {
        let (x, y) = match self.required_point(args) {
            Ok(point) => point,
            Err(result) => return result,
        };
        let delta_x = match optional_i32(args, "delta_x") {
            Ok(delta) => delta,
            Err(result) => return result,
        };
        let delta_y = match optional_i32(args, "delta_y") {
            Ok(delta) => delta,
            Err(result) => return result,
        };
        if delta_x == 0 && delta_y == 0 {
            return tool_error("tool-computer-use-error-scroll-zero");
        }

        let (display, _) = match self.resolve_display(args).await {
            Ok(resolved) => resolved,
            Err(result) => return result,
        };
        let point = match self.to_logical(&display, x, y) {
            Ok(point) => point,
            Err(result) => return result,
        };

        if let Err(error) = self.backend.scroll(point, delta_x, delta_y).await {
            return backend_error(&error);
        }

        ToolResult::ok(ToolOutput::json(json!({
            "action": "scroll",
            "display": display.index,
            "x": x,
            "y": y,
            "delta_x": delta_x,
            "delta_y": delta_y,
        })))
    }

    async fn type_action(&self, args: &Value) -> ToolResult {
        let text = match required_text(args) {
            Ok(text) => text,
            Err(result) => return result,
        };
        let chars = text.chars().count();

        if let Err(error) = self.backend.type_text(text).await {
            return backend_error(&error);
        }

        // Deliberately report only the length: typed text is frequently a
        // password or other secret, and echoing it back would place it in the
        // transcript and the provider request.
        ToolResult::ok(ToolOutput::json(json!({
            "action": "type",
            "chars": chars,
        })))
    }

    async fn key_action(&self, args: &Value) -> ToolResult {
        let raw = match required_key(args) {
            Ok(raw) => raw,
            Err(result) => return result,
        };
        let chord = match keys::parse_key_chord(raw) {
            Ok(chord) => chord,
            Err(error) => return key_parse_error(&error),
        };

        if let Err(error) = self.backend.press_key(&chord).await {
            return backend_error(&error);
        }

        ToolResult::ok(ToolOutput::json(json!({
            "action": "key",
            "key": raw,
        })))
    }

    async fn wait_action(&self, args: &Value, context: &ToolExecutionContext) -> ToolResult {
        let seconds = match required_seconds(args) {
            Ok(seconds) => seconds,
            Err(result) => return result,
        };
        let max_wait_secs = self.config.max_wait_secs;
        if !seconds.is_finite() || seconds < 0.0 || seconds > max_wait_secs as f64 {
            let max = max_wait_secs.to_string();
            return tool_error_with_args("tool-computer-use-error-wait-range", &[("max", &max)]);
        }

        let sleep = tokio::time::sleep(Duration::from_secs_f64(seconds));
        match context.cancellation_token() {
            Some(token) => {
                tokio::select! {
                    biased;
                    () = token.cancelled() => {
                        return tool_error("tool-computer-use-error-cancelled");
                    }
                    () = sleep => {}
                }
            }
            None => sleep.await,
        }

        ToolResult::ok(ToolOutput::json(json!({
            "action": "wait",
            "seconds": seconds,
        })))
    }

    /// Validate the required `x`/`y` pair of a pointer action.
    fn required_point(&self, args: &Value) -> Result<(f64, f64), ToolResult> {
        Ok((
            required_coordinate(args, "x")?,
            required_coordinate(args, "y")?,
        ))
    }
}

/// A screenshot written to disk, with the size the model sees.
struct SavedScreenshot {
    path: PathBuf,
    width: u32,
    height: u32,
}

#[async_trait]
impl Tool for ComputerUseTool {
    fn name(&self) -> &str {
        TOOL_NAME
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ACTIONS,
                    "description": "Action to perform. 'screenshot' captures the display, attaches the downscaled PNG image directly to the tool result for vision inspection (do not call file_read on the saved path), and returns image dimensions; coordinates for click, drag, move, and scroll are pixels in that image."
                },
                "display": {
                    "type": "integer",
                    "minimum": 0,
                    "default": 0,
                    "description": "Display index; 0 is the primary display. Secondary displays require [risk_profiles.<alias>.computer_use].allow_secondary_displays = true."
                },
                "x": {
                    "type": "number",
                    "description": "Screenshot pixel X for click, move, and scroll."
                },
                "y": {
                    "type": "number",
                    "description": "Screenshot pixel Y for click, move, and scroll."
                },
                "from_x": {
                    "type": "number",
                    "description": "Start screenshot pixel X for drag."
                },
                "from_y": {
                    "type": "number",
                    "description": "Start screenshot pixel Y for drag."
                },
                "to_x": {
                    "type": "number",
                    "description": "End screenshot pixel X for drag."
                },
                "to_y": {
                    "type": "number",
                    "description": "End screenshot pixel Y for drag."
                },
                "button": {
                    "type": "string",
                    "enum": ["left", "right", "middle"],
                    "default": "left",
                    "description": "Mouse button for click and drag."
                },
                "clicks": {
                    "type": "integer",
                    "enum": [1, 2],
                    "default": 1,
                    "description": "Click count for click: 1 for a single click, 2 for a double click."
                },
                "steps": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_DRAG_STEPS,
                    "default": DEFAULT_DRAG_STEPS,
                    "description": "Number of interpolated drag steps for drag (default: 10)."
                },
                "delta_x": {
                    "type": "integer",
                    "default": 0,
                    "description": "Horizontal scroll amount; positive scrolls right, negative scrolls left."
                },
                "delta_y": {
                    "type": "integer",
                    "default": 0,
                    "description": "Vertical scroll amount; positive scrolls down, negative scrolls up."
                },
                "text": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": MAX_TEXT_CHARS,
                    "description": "Text to type into the focused application."
                },
                "key": {
                    "type": "string",
                    "description": "Key chord to press, e.g. 'Return', 'cmd+c', or 'cmd+shift+s'."
                },
                "seconds": {
                    "type": "number",
                    "minimum": 0,
                    "description": "Seconds to wait; bounded by [risk_profiles.<alias>.computer_use].max_wait_secs."
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        self.execute_with_context(args, &ToolExecutionContext::current().unwrap_or_default())
            .await
    }

    async fn execute_with_context(
        &self,
        args: Value,
        context: &ToolExecutionContext,
    ) -> anyhow::Result<ToolResult> {
        if !args.is_object() {
            return Ok(tool_error("tool-computer-use-error-missing-action"));
        }
        let action = match args.get("action").and_then(Value::as_str) {
            Some(action) => action.trim(),
            None => return Ok(tool_error("tool-computer-use-error-missing-action")),
        };

        // Autonomy is decided before the action is dispatched, and only for
        // actions that actually change desktop state: observing the screen and
        // waiting stay available to a read-only agent.
        if classify_action(action) == Some(ActionClass::Mutate) && !self.security.can_act() {
            return Ok(tool_error_with_args(
                "tool-computer-use-error-read-only",
                &[("action", action)],
            ));
        }

        let result = match action {
            "screenshot" => self.screenshot_action(&args).await,
            "click" => self.click_action(&args).await,
            "drag" => self.drag_action(&args).await,
            "move" => self.move_action(&args).await,
            "scroll" => self.scroll_action(&args).await,
            "type" => self.type_action(&args).await,
            "key" => self.key_action(&args).await,
            "wait" => self.wait_action(&args, context).await,
            other => tool_error_with_args(
                "tool-computer-use-error-unknown-action",
                &[("action", other)],
            ),
        };
        Ok(result)
    }
}

/// Build a localized failure result from a key with no arguments.
fn tool_error(key: &str) -> ToolResult {
    ToolResult::err(get_required_tool_string(key))
}

/// Build a localized failure result from a key and its Fluent arguments.
fn tool_error_with_args(key: &str, args: &[(&str, &str)]) -> ToolResult {
    ToolResult::err(get_required_tool_string_with_args(key, args))
}

/// Surface a [`KeyParseError`] through its localized Fluent template.
fn key_parse_error(error: &KeyParseError) -> ToolResult {
    match error {
        KeyParseError::Empty => tool_error("tool-computer-use-error-key-empty"),
        KeyParseError::MissingKey => tool_error("tool-computer-use-error-key-missing-key"),
        KeyParseError::UnknownKey(token) => {
            tool_error_with_args("tool-computer-use-error-key-unknown", &[("key", token)])
        }
        KeyParseError::MultipleKeys => tool_error("tool-computer-use-error-key-multiple"),
    }
}

/// Surface a [`ScaleError`] through its localized Fluent template.
fn scale_error(error: &ScaleError) -> ToolResult {
    match error {
        ScaleError::InvalidDisplayDimensions => {
            tool_error("tool-computer-use-error-scale-invalid-display")
        }
        ScaleError::NonFiniteCoordinate => tool_error("tool-computer-use-error-scale-non-finite"),
        ScaleError::OutOfBounds {
            x,
            y,
            width,
            height,
        } => {
            let x = x.to_string();
            let y = y.to_string();
            let width = width.to_string();
            let height = height.to_string();
            tool_error_with_args(
                "tool-computer-use-error-scale-out-of-bounds",
                &[("x", &x), ("y", &y), ("width", &width), ("height", &height)],
            )
        }
        ScaleError::InvalidPng(err) => {
            tool_error_with_args("tool-computer-use-error-scale-invalid-png", &[("err", err)])
        }
    }
}

/// Surface a [`BackendError`] through its localized Fluent template.
fn backend_error(error: &BackendError) -> ToolResult {
    match error {
        BackendError::PermissionDenied {
            permission,
            instructions,
        } => tool_error_with_args(
            "tool-computer-use-error-backend-permission-denied",
            &[("permission", permission), ("instructions", instructions)],
        ),
        BackendError::UnsupportedPlatform(detail) => tool_error_with_args(
            "tool-computer-use-error-backend-unsupported",
            &[("detail", detail)],
        ),
        BackendError::CaptureFailed(detail) => tool_error_with_args(
            "tool-computer-use-error-backend-capture-failed",
            &[("detail", detail)],
        ),
        BackendError::InputFailed(detail) => tool_error_with_args(
            "tool-computer-use-error-backend-input-failed",
            &[("detail", detail)],
        ),
        BackendError::Timeout(detail) => tool_error_with_args(
            "tool-computer-use-error-backend-timeout",
            &[("detail", detail)],
        ),
    }
}

/// The `button` label a click summary reports.
fn button_label(button: MouseButton) -> &'static str {
    match button {
        MouseButton::Left => "left",
        MouseButton::Right => "right",
        MouseButton::Middle => "middle",
    }
}

/// Read a required finite numeric coordinate.
fn required_coordinate(args: &Value, param: &str) -> Result<f64, ToolResult> {
    match args.get(param) {
        None | Some(Value::Null) => Err(tool_error_with_args(
            "tool-computer-use-error-param-required",
            &[("param", param)],
        )),
        Some(value) => match value.as_f64() {
            Some(number) if number.is_finite() => Ok(number),
            Some(_) => Err(tool_error_with_args(
                "tool-computer-use-error-param-must-be-finite-number",
                &[("param", param)],
            )),
            None => Err(tool_error_with_args(
                "tool-computer-use-error-param-must-be-number",
                &[("param", param)],
            )),
        },
    }
}

/// Read the optional `button`, defaulting to the left button.
fn optional_button(args: &Value) -> Result<MouseButton, ToolResult> {
    match args.get("button") {
        None | Some(Value::Null) => Ok(MouseButton::Left),
        Some(Value::String(raw)) => match raw.trim().to_ascii_lowercase().as_str() {
            "left" => Ok(MouseButton::Left),
            "right" => Ok(MouseButton::Right),
            "middle" => Ok(MouseButton::Middle),
            _ => Err(tool_error("tool-computer-use-error-button-invalid")),
        },
        Some(_) => Err(tool_error_with_args(
            "tool-computer-use-error-param-must-be-string",
            &[("param", "button")],
        )),
    }
}

/// Read the optional `clicks`, which is exactly 1 or 2.
fn optional_clicks(args: &Value) -> Result<u8, ToolResult> {
    match args.get("clicks") {
        None | Some(Value::Null) => Ok(1),
        Some(value) => match value.as_u64() {
            Some(1) => Ok(1),
            Some(2) => Ok(2),
            Some(_) | None => Err(tool_error("tool-computer-use-error-clicks-invalid")),
        },
    }
}

/// Read the optional `steps` for a drag action, defaulting to [`DEFAULT_DRAG_STEPS`].
fn optional_drag_steps(args: &Value) -> Result<u32, ToolResult> {
    match args.get("steps") {
        None | Some(Value::Null) => Ok(DEFAULT_DRAG_STEPS),
        Some(value) => match value.as_u64().and_then(|raw| u32::try_from(raw).ok()) {
            Some(steps) if (1..=MAX_DRAG_STEPS).contains(&steps) => Ok(steps),
            _ => {
                let max = MAX_DRAG_STEPS.to_string();
                Err(tool_error_with_args(
                    "tool-computer-use-error-steps-invalid",
                    &[("max", &max)],
                ))
            }
        },
    }
}

/// Read an optional signed 32-bit integer, defaulting to 0.
fn optional_i32(args: &Value, param: &str) -> Result<i32, ToolResult> {
    match args.get(param) {
        None | Some(Value::Null) => Ok(0),
        Some(value) => match value.as_i64() {
            Some(raw) => i32::try_from(raw).map_err(|_| {
                tool_error_with_args(
                    "tool-computer-use-error-param-i32-overflow",
                    &[("param", param)],
                )
            }),
            None => Err(tool_error_with_args(
                "tool-computer-use-error-param-must-be-integer",
                &[("param", param)],
            )),
        },
    }
}

/// Read the required `text` of a `type` action, enforcing the length bounds.
fn required_text(args: &Value) -> Result<&str, ToolResult> {
    match args.get("text") {
        Some(Value::String(text)) => {
            if text.is_empty() {
                return Err(tool_error("tool-computer-use-error-text-empty"));
            }
            if text.chars().count() > MAX_TEXT_CHARS {
                let max = MAX_TEXT_CHARS.to_string();
                return Err(tool_error_with_args(
                    "tool-computer-use-error-text-too-long",
                    &[("max", &max)],
                ));
            }
            Ok(text.as_str())
        }
        Some(_) => Err(tool_error_with_args(
            "tool-computer-use-error-param-must-be-string",
            &[("param", "text")],
        )),
        None => Err(tool_error_with_args(
            "tool-computer-use-error-param-required",
            &[("param", "text")],
        )),
    }
}

/// Read the required `key` chord of a `key` action.
fn required_key(args: &Value) -> Result<&str, ToolResult> {
    match args.get("key") {
        Some(Value::String(raw)) if !raw.trim().is_empty() => Ok(raw.as_str()),
        Some(Value::String(_)) => Err(tool_error("tool-computer-use-error-key-empty")),
        Some(_) => Err(tool_error_with_args(
            "tool-computer-use-error-param-must-be-string",
            &[("param", "key")],
        )),
        None => Err(tool_error_with_args(
            "tool-computer-use-error-param-required",
            &[("param", "key")],
        )),
    }
}

/// Read the required `seconds` of a `wait` action.
fn required_seconds(args: &Value) -> Result<f64, ToolResult> {
    match args.get("seconds") {
        Some(value) => match value.as_f64() {
            Some(number) => Ok(number),
            None => Err(tool_error_with_args(
                "tool-computer-use-error-param-must-be-number",
                &[("param", "seconds")],
            )),
        },
        None => Err(tool_error_with_args(
            "tool-computer-use-error-param-required",
            &[("param", "seconds")],
        )),
    }
}

/// Create `path` exclusively and flush its bytes to disk.
async fn write_new_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .await?;
    file.write_all(bytes).await?;
    file.sync_all().await
}

/// Remove `path` if it exists, ignoring the outcome.
async fn remove_if_present(path: &Path) {
    let _ = tokio::fs::remove_file(path).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::Cursor;
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;
    use std::time::Instant;

    use image::{ImageFormat, Rgba, RgbaImage};
    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;
    use zeroclaw_api::attribution::{Attributable, Role, ToolKind};
    use zeroclaw_config::autonomy::AutonomyLevel;

    // ── Mock backend ─────────────────────────────────────────────────────────

    /// A backend that records every call and replays canned captures.
    struct MockBackend {
        displays: Vec<DisplayInfo>,
        png: Vec<u8>,
        /// When set, every call fails with this permission denial.
        deny: Option<(&'static str, &'static str)>,
        list_displays_calls: AtomicUsize,
        capture_calls: AtomicUsize,
        clicks: Mutex<Vec<(LogicalPoint, MouseButton, u8)>>,
        drags: Mutex<Vec<(LogicalPoint, LogicalPoint, MouseButton, u32)>>,
        moves: Mutex<Vec<LogicalPoint>>,
        scrolls: Mutex<Vec<(LogicalPoint, i32, i32)>>,
        typed: Mutex<Vec<String>>,
        keys: Mutex<Vec<KeyChord>>,
    }

    impl MockBackend {
        fn new(displays: Vec<DisplayInfo>, png: Vec<u8>) -> Self {
            Self {
                displays,
                png,
                deny: None,
                list_displays_calls: AtomicUsize::new(0),
                capture_calls: AtomicUsize::new(0),
                clicks: Mutex::new(Vec::new()),
                drags: Mutex::new(Vec::new()),
                moves: Mutex::new(Vec::new()),
                scrolls: Mutex::new(Vec::new()),
                typed: Mutex::new(Vec::new()),
                keys: Mutex::new(Vec::new()),
            }
        }

        fn denying(
            displays: Vec<DisplayInfo>,
            png: Vec<u8>,
            permission: &'static str,
            instructions: &'static str,
        ) -> Self {
            let mut backend = Self::new(displays, png);
            backend.deny = Some((permission, instructions));
            backend
        }

        fn denied(&self) -> Option<BackendError> {
            self.deny.map(
                |(permission, instructions)| BackendError::PermissionDenied {
                    permission,
                    instructions: instructions.to_string(),
                },
            )
        }

        fn list_displays_calls(&self) -> usize {
            self.list_displays_calls.load(Ordering::SeqCst)
        }

        fn capture_calls(&self) -> usize {
            self.capture_calls.load(Ordering::SeqCst)
        }

        fn clicks(&self) -> Vec<(LogicalPoint, MouseButton, u8)> {
            self.clicks.lock().map(|v| v.clone()).unwrap_or_default()
        }

        fn drags(&self) -> Vec<(LogicalPoint, LogicalPoint, MouseButton, u32)> {
            self.drags.lock().map(|v| v.clone()).unwrap_or_default()
        }

        fn moves(&self) -> Vec<LogicalPoint> {
            self.moves.lock().map(|v| v.clone()).unwrap_or_default()
        }

        fn scrolls(&self) -> Vec<(LogicalPoint, i32, i32)> {
            self.scrolls.lock().map(|v| v.clone()).unwrap_or_default()
        }

        fn typed(&self) -> Vec<String> {
            self.typed.lock().map(|v| v.clone()).unwrap_or_default()
        }

        fn keys(&self) -> Vec<KeyChord> {
            self.keys.lock().map(|v| v.clone()).unwrap_or_default()
        }
    }

    #[async_trait]
    impl ComputerUseBackend for MockBackend {
        async fn list_displays(&self) -> Result<Vec<DisplayInfo>, BackendError> {
            self.list_displays_calls.fetch_add(1, Ordering::SeqCst);
            if let Some(error) = self.denied() {
                return Err(error);
            }
            Ok(self.displays.clone())
        }

        async fn capture_display(&self, _display: &DisplayInfo) -> Result<Vec<u8>, BackendError> {
            self.capture_calls.fetch_add(1, Ordering::SeqCst);
            if let Some(error) = self.denied() {
                return Err(error);
            }
            Ok(self.png.clone())
        }

        async fn move_cursor(&self, point: LogicalPoint) -> Result<(), BackendError> {
            if let Some(error) = self.denied() {
                return Err(error);
            }
            if let Ok(mut moves) = self.moves.lock() {
                moves.push(point);
            }
            Ok(())
        }

        async fn click(
            &self,
            point: LogicalPoint,
            button: MouseButton,
            clicks: u8,
        ) -> Result<(), BackendError> {
            if let Some(error) = self.denied() {
                return Err(error);
            }
            if let Ok(mut recorded) = self.clicks.lock() {
                recorded.push((point, button, clicks));
            }
            Ok(())
        }

        async fn drag(
            &self,
            from: LogicalPoint,
            to: LogicalPoint,
            button: MouseButton,
            steps: u32,
        ) -> Result<(), BackendError> {
            if let Some(error) = self.denied() {
                return Err(error);
            }
            if let Ok(mut recorded) = self.drags.lock() {
                recorded.push((from, to, button, steps));
            }
            Ok(())
        }

        async fn scroll(
            &self,
            point: LogicalPoint,
            delta_x: i32,
            delta_y: i32,
        ) -> Result<(), BackendError> {
            if let Some(error) = self.denied() {
                return Err(error);
            }
            if let Ok(mut recorded) = self.scrolls.lock() {
                recorded.push((point, delta_x, delta_y));
            }
            Ok(())
        }

        async fn type_text(&self, text: &str) -> Result<(), BackendError> {
            if let Some(error) = self.denied() {
                return Err(error);
            }
            if let Ok(mut recorded) = self.typed.lock() {
                recorded.push(text.to_string());
            }
            Ok(())
        }

        async fn press_key(&self, chord: &KeyChord) -> Result<(), BackendError> {
            if let Some(error) = self.denied() {
                return Err(error);
            }
            if let Ok(mut recorded) = self.keys.lock() {
                recorded.push(*chord);
            }
            Ok(())
        }
    }

    // ── Fixtures ─────────────────────────────────────────────────────────────

    /// A display of the given size whose origin is `(origin_x, origin_y)`.
    fn display(
        index: u32,
        origin_x: f64,
        origin_y: f64,
        logical_w: f64,
        logical_h: f64,
    ) -> DisplayInfo {
        DisplayInfo {
            index,
            native_id: index + 1,
            origin_x,
            origin_y,
            logical_w,
            logical_h,
            is_primary: index == 0,
        }
    }

    /// The primary display every fixture starts from: 400x300 points.
    fn primary() -> DisplayInfo {
        display(0, 0.0, 0.0, 400.0, 300.0)
    }

    /// Encode a `width` x `height` RGBA gradient as PNG bytes.
    fn synthetic_png(width: u32, height: u32) -> Vec<u8> {
        let mut buffer = Cursor::new(Vec::new());
        let image = RgbaImage::from_fn(width, height, |x, y| {
            Rgba([(x % 256) as u8, (y % 256) as u8, 128, 255])
        });
        image
            .write_to(&mut buffer, ImageFormat::Png)
            .expect("synthetic PNG encodes");
        buffer.into_inner()
    }

    /// The pixel dimensions of `png`.
    fn png_dimensions(png: &[u8]) -> (u32, u32) {
        let image =
            image::load_from_memory_with_format(png, ImageFormat::Png).expect("PNG decodes");
        (image.width(), image.height())
    }

    fn config() -> ComputerUseToolConfig {
        ComputerUseToolConfig {
            enabled: true,
            max_dimension: 200,
            max_wait_secs: 30,
            ..ComputerUseToolConfig::default()
        }
    }

    /// The production default dimension, so a test can assert real pixel math.
    fn config_at_default_dimension() -> ComputerUseToolConfig {
        ComputerUseToolConfig {
            enabled: true,
            ..ComputerUseToolConfig::default()
        }
    }

    fn full_security(workspace: &Path) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: workspace.to_path_buf(),
            workspace_only: false,
            forbidden_paths: Vec::new(),
            ..SecurityPolicy::default()
        })
    }

    /// A tool over a mock backend, with an isolated agent workspace.
    fn tool_with(
        security: Arc<SecurityPolicy>,
        config: ComputerUseToolConfig,
        agent_workspace: &Path,
        backend: Arc<MockBackend>,
    ) -> ComputerUseTool {
        ComputerUseTool::new(
            security,
            config,
            agent_workspace.to_path_buf(),
            backend as Arc<dyn ComputerUseBackend>,
        )
    }

    /// A full-autonomy tool over the 400x300 primary display.
    fn happy_tool(
        security_workspace: &Path,
        agent_workspace: &Path,
    ) -> (ComputerUseTool, Arc<MockBackend>) {
        let backend = Arc::new(MockBackend::new(vec![primary()], synthetic_png(400, 300)));
        let tool = tool_with(
            full_security(security_workspace),
            config(),
            agent_workspace,
            backend.clone(),
        );
        (tool, backend)
    }

    // ── Tests ────────────────────────────────────────────────────────────────

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

    #[tokio::test]
    async fn readonly_allows_screenshot_and_wait_but_blocks_all_input_actions() {
        let agent_workspace = TempDir::new().expect("tempdir");
        let security_workspace = TempDir::new().expect("tempdir");
        let readonly = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            workspace_dir: security_workspace.path().to_path_buf(),
            ..SecurityPolicy::default()
        });
        let backend = Arc::new(MockBackend::new(vec![primary()], synthetic_png(400, 300)));
        let tool = tool_with(readonly, config(), agent_workspace.path(), backend.clone());

        // Observation is permitted.
        let screenshot = tool
            .execute(json!({"action": "screenshot"}))
            .await
            .expect("screenshot runs");
        assert!(
            screenshot.success,
            "read-only must still allow screenshot: {:?}",
            screenshot.error
        );
        let wait = tool
            .execute(json!({"action": "wait", "seconds": 0}))
            .await
            .expect("wait runs");
        assert!(wait.success, "read-only must still allow wait");

        // Every state-changing action is refused before touching the backend.
        for args in [
            json!({"action": "click", "x": 10, "y": 10}),
            json!({"action": "drag", "from_x": 10, "from_y": 10, "to_x": 50, "to_y": 50}),
            json!({"action": "move", "x": 10, "y": 10}),
            json!({"action": "scroll", "x": 10, "y": 10, "delta_y": 1}),
            json!({"action": "type", "text": "hello"}),
            json!({"action": "key", "key": "Return"}),
        ] {
            let result = tool.execute(args.clone()).await.expect("call returns");
            assert!(!result.success, "{args} must be blocked in read-only mode");
            let error = result.error.unwrap_or_default();
            assert!(
                error.contains("read-only"),
                "{args} error must explain read-only autonomy, got: {error}"
            );
            assert!(
                error.contains(args["action"].as_str().unwrap_or_default()),
                "{args} error must name the blocked action, got: {error}"
            );
        }

        assert!(
            backend.clicks().is_empty(),
            "no click may reach the backend"
        );
        assert!(backend.drags().is_empty(), "no drag may reach the backend");
        assert!(backend.moves().is_empty(), "no move may reach the backend");
        assert!(
            backend.scrolls().is_empty(),
            "no scroll may reach the backend"
        );
        assert!(backend.typed().is_empty(), "no text may reach the backend");
        assert!(backend.keys().is_empty(), "no key may reach the backend");
    }

    #[tokio::test]
    async fn secondary_display_rejected_by_default_and_allowed_when_enabled() {
        let agent_workspace = TempDir::new().expect("tempdir");
        let security_workspace = TempDir::new().expect("tempdir");
        let secondary = display(1, -1920.0, 0.0, 1920.0, 1080.0);
        let backend = Arc::new(MockBackend::new(
            vec![primary(), secondary],
            synthetic_png(1920, 1080),
        ));

        // Closed by default: the gate fires before any display query. The
        // default 1280 long edge is used so the coordinate math below is the
        // real production mapping.
        let closed = tool_with(
            full_security(security_workspace.path()),
            config_at_default_dimension(),
            agent_workspace.path(),
            backend.clone(),
        );
        let blocked = closed
            .execute(json!({"action": "click", "display": 1, "x": 640, "y": 360}))
            .await
            .expect("call returns");
        assert!(!blocked.success, "a secondary display is closed by default");
        let error = blocked.error.unwrap_or_default();
        assert!(
            error.contains("blocked by policy"),
            "the gate must say policy blocked it, got: {error}"
        );
        assert!(
            error.contains("allow_secondary_displays"),
            "the gate must name the setting to change, got: {error}"
        );
        assert_eq!(
            backend.list_displays_calls(),
            0,
            "a policy-blocked display must not query the backend"
        );
        assert!(backend.clicks().is_empty());

        // Opened by config: the negative-origin display maps exactly.
        let open_config = ComputerUseToolConfig {
            allow_secondary_displays: true,
            ..config_at_default_dimension()
        };
        let open = tool_with(
            full_security(security_workspace.path()),
            open_config,
            agent_workspace.path(),
            backend.clone(),
        );
        let allowed = open
            .execute(json!({"action": "click", "display": 1, "x": 640, "y": 360}))
            .await
            .expect("call returns");
        assert!(
            allowed.success,
            "an enabled secondary display must work: {:?}",
            allowed.error
        );

        let clicks = backend.clicks();
        assert_eq!(clicks.len(), 1, "exactly one click reaches the backend");
        let (point, button, count) = clicks[0];
        // 640 of 1280 delivered pixels is the midpoint of 1920 points, offset
        // by the display's negative origin; 360 of 720 is the vertical midpoint.
        assert!(
            (point.x - (-960.0)).abs() < 0.5,
            "x was {}, expected -960",
            point.x
        );
        assert!(
            (point.y - 540.0).abs() < 0.5,
            "y was {}, expected 540",
            point.y
        );
        assert_eq!(button, MouseButton::Left);
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn screenshot_writes_to_agent_workspace_not_session_cwd_and_declares_image_attachment() {
        let agent_workspace = TempDir::new().expect("agent workspace");
        let security_workspace = TempDir::new().expect("security workspace");
        let backend = Arc::new(MockBackend::new(vec![primary()], synthetic_png(400, 300)));
        let tool = tool_with(
            full_security(security_workspace.path()),
            config(),
            agent_workspace.path(),
            backend.clone(),
        );

        let result = tool
            .execute(json!({"action": "screenshot"}))
            .await
            .expect("screenshot runs");
        assert!(result.success, "screenshot failed: {:?}", result.error);
        assert_eq!(backend.capture_calls(), 1, "one capture per screenshot");

        // The session workspace stays untouched: screenshots belong to the agent.
        assert_eq!(
            std::fs::read_dir(security_workspace.path())
                .expect("security workspace readable")
                .count(),
            0,
            "no file may land in security.workspace_dir"
        );

        let screenshot_dir = agent_workspace.path().join(SCREENSHOT_SUBDIR);
        let entries: Vec<_> = std::fs::read_dir(&screenshot_dir)
            .expect("screenshot dir exists")
            .map(|entry| entry.expect("entry readable").path())
            .collect();
        assert_eq!(entries.len(), 1, "exactly one screenshot file");
        let saved = &entries[0];
        assert_eq!(
            saved.extension().and_then(|e| e.to_str()),
            Some("png"),
            "screenshot must be a PNG"
        );

        // 400x300 at max_dimension 200 scales to exactly 200x150.
        let bytes = std::fs::read(saved).expect("screenshot readable");
        assert_eq!(png_dimensions(&bytes), (200, 150));

        let canonical = std::fs::canonicalize(saved).expect("canonical path");
        let data = result.output.data().expect("structured output");
        assert_eq!(data["action"], "screenshot");
        assert_eq!(data["display"], 0);
        assert_eq!(data["width"], 200);
        assert_eq!(data["height"], 150);
        assert_eq!(data["logical_width"], 400.0);
        assert_eq!(data["logical_height"], 300.0);
        assert_eq!(data["displays_count"], 1);
        assert_eq!(data["path"], canonical.to_string_lossy().to_string());

        let attachments = result.output.attachments();
        assert_eq!(attachments.len(), 1, "one declared attachment");
        assert_eq!(attachments[0].kind, MarkerKind::Image);
        assert_eq!(
            attachments[0].target,
            canonical.to_string_lossy().to_string()
        );
    }

    #[tokio::test]
    async fn input_validation_rejects_bad_coords_zero_scroll_empty_text_bad_key_and_excessive_wait()
    {
        let agent_workspace = TempDir::new().expect("tempdir");
        let security_workspace = TempDir::new().expect("tempdir");
        let backend = Arc::new(MockBackend::new(vec![primary()], synthetic_png(400, 300)));
        let tool = tool_with(
            full_security(security_workspace.path()),
            config(),
            agent_workspace.path(),
            backend.clone(),
        );

        let too_long = "a".repeat(MAX_TEXT_CHARS + 1);
        let cases: Vec<(&str, Value)> = vec![
            // Missing and mistyped coordinates.
            ("missing x", json!({"action": "click", "y": 10})),
            ("missing y", json!({"action": "move", "x": 10})),
            ("string x", json!({"action": "click", "x": "10", "y": 10})),
            ("bool y", json!({"action": "click", "x": 10, "y": true})),
            // Out-of-bounds and negative coordinates in screenshot space.
            ("negative x", json!({"action": "click", "x": -1, "y": 10})),
            (
                "past right edge",
                json!({"action": "move", "x": 200, "y": 10}),
            ),
            (
                "past bottom edge",
                json!({"action": "move", "x": 10, "y": 150}),
            ),
            (
                "far out of bounds",
                json!({"action": "click", "x": 1e9, "y": 10}),
            ),
            // A NaN cannot survive JSON, so it arrives as null and is rejected.
            ("null x", json!({"action": "click", "x": null, "y": 10})),
            // Click shape.
            (
                "clicks 3",
                json!({"action": "click", "x": 1, "y": 1, "clicks": 3}),
            ),
            (
                "clicks float",
                json!({"action": "click", "x": 1, "y": 1, "clicks": 1.5}),
            ),
            (
                "bad button",
                json!({"action": "click", "x": 1, "y": 1, "button": "sideways"}),
            ),
            (
                "button wrong type",
                json!({"action": "click", "x": 1, "y": 1, "button": 2}),
            ),
            // Drag shape.
            (
                "drag missing to_x",
                json!({"action": "drag", "from_x": 1, "from_y": 1, "to_y": 10}),
            ),
            (
                "drag out of bounds to_x",
                json!({"action": "drag", "from_x": 1, "from_y": 1, "to_x": 200, "to_y": 10}),
            ),
            (
                "drag steps 0",
                json!({"action": "drag", "from_x": 1, "from_y": 1, "to_x": 10, "to_y": 10, "steps": 0}),
            ),
            (
                "drag steps too high",
                json!({"action": "drag", "from_x": 1, "from_y": 1, "to_x": 10, "to_y": 10, "steps": 101}),
            ),
            (
                "drag steps float",
                json!({"action": "drag", "from_x": 1, "from_y": 1, "to_x": 10, "to_y": 10, "steps": 2.5}),
            ),
            // Scroll needs a non-zero delta.
            (
                "zero scroll",
                json!({"action": "scroll", "x": 1, "y": 1, "delta_x": 0, "delta_y": 0}),
            ),
            (
                "string delta",
                json!({"action": "scroll", "x": 1, "y": 1, "delta_y": "3"}),
            ),
            (
                "delta overflows i32",
                json!({"action": "scroll", "x": 1, "y": 1, "delta_y": 5_000_000_000i64}),
            ),
            // Type bounds.
            ("empty text", json!({"action": "type", "text": ""})),
            ("text not a string", json!({"action": "type", "text": 42})),
            ("text too long", json!({"action": "type", "text": too_long})),
            // Key parsing.
            ("empty key", json!({"action": "key", "key": "  "})),
            (
                "unknown key",
                json!({"action": "key", "key": "cmd+notakey"}),
            ),
            ("two keys", json!({"action": "key", "key": "cmd+a+b"})),
            // Wait bounds.
            ("wait too long", json!({"action": "wait", "seconds": 31})),
            ("wait negative", json!({"action": "wait", "seconds": -1})),
            ("wait missing", json!({"action": "wait"})),
            // Display typing.
            (
                "display string",
                json!({"action": "click", "display": "0", "x": 1, "y": 1}),
            ),
            (
                "display negative",
                json!({"action": "click", "display": -1, "x": 1, "y": 1}),
            ),
            (
                "display float",
                json!({"action": "click", "display": 1.5, "x": 1, "y": 1}),
            ),
            (
                "display out of range",
                json!({"action": "click", "display": 7, "x": 1, "y": 1}),
            ),
            // Action dispatch.
            ("unknown action", json!({"action": "teleport"})),
            ("missing action", json!({"x": 1, "y": 1})),
            ("action not a string", json!({"action": 7})),
            ("not an object", json!(["screenshot"])),
        ];

        for (label, args) in cases {
            let result = tool
                .execute(args.clone())
                .await
                .unwrap_or_else(|error| panic!("{label} must return a result, got {error}"));
            assert!(
                !result.success,
                "{label} must be rejected, got success for {args}"
            );
            let error = result.error.unwrap_or_default();
            assert!(
                !error.is_empty(),
                "{label} must explain why it was rejected"
            );
        }

        // Nothing invalid reached the backend.
        assert!(backend.clicks().is_empty(), "no invalid click dispatched");
        assert!(backend.drags().is_empty(), "no invalid drag dispatched");
        assert!(backend.moves().is_empty(), "no invalid move dispatched");
        assert!(backend.scrolls().is_empty(), "no invalid scroll dispatched");
        assert!(backend.typed().is_empty(), "no invalid text dispatched");
        assert!(backend.keys().is_empty(), "no invalid key dispatched");
    }

    #[tokio::test]
    async fn wait_honors_cancellation_token() {
        let agent_workspace = TempDir::new().expect("tempdir");
        let security_workspace = TempDir::new().expect("tempdir");
        let backend = Arc::new(MockBackend::new(vec![primary()], synthetic_png(400, 300)));
        let tool = tool_with(
            full_security(security_workspace.path()),
            config(),
            agent_workspace.path(),
            backend,
        );

        let token = CancellationToken::new();
        let context = ToolExecutionContext::new(Some(token.clone()));
        let canceller = token.clone();
        let cancel_after = async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            canceller.cancel();
        };

        let started = Instant::now();
        let (result, ()) = tokio::join!(
            tool.execute_with_context(json!({"action": "wait", "seconds": 10}), &context),
            cancel_after
        );
        let elapsed = started.elapsed();

        let result = result.expect("wait returns a result");
        assert!(!result.success, "a cancelled wait must fail");
        let error = result.error.unwrap_or_default();
        assert!(
            error.contains("cancelled"),
            "the cancellation must be reported, got: {error}"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "cancellation must interrupt the sleep, took {elapsed:?}"
        );
        assert!(
            elapsed >= Duration::from_millis(40),
            "the wait must have started before it was cancelled, took {elapsed:?}"
        );

        // A pre-cancelled token short-circuits without sleeping at all.
        token.cancel();
        let started = Instant::now();
        let result = tool
            .execute_with_context(json!({"action": "wait", "seconds": 10}), &context)
            .await
            .expect("wait returns a result");
        assert!(!result.success);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn backend_permission_denied_surfaces_actionable_error() {
        let agent_workspace = TempDir::new().expect("tempdir");
        let security_workspace = TempDir::new().expect("tempdir");
        let backend = Arc::new(MockBackend::denying(
            vec![primary()],
            synthetic_png(400, 300),
            "Screen Recording",
            "grant it in System Settings > Privacy & Security > Screen Recording",
        ));
        let tool = tool_with(
            full_security(security_workspace.path()),
            config(),
            agent_workspace.path(),
            backend,
        );

        let result = tool
            .execute(json!({"action": "screenshot"}))
            .await
            .expect("call returns");
        assert!(!result.success, "a denied capture must fail");
        let error = result.error.unwrap_or_default();
        assert!(
            error.contains("Screen Recording permission denied"),
            "the error must name the permission, got: {error}"
        );
        assert!(
            error.contains("System Settings"),
            "the error must say how to grant it, got: {error}"
        );

        // The same actionable wording reaches input actions.
        let result = tool
            .execute(json!({"action": "click", "x": 1, "y": 1}))
            .await
            .expect("call returns");
        assert!(!result.success);
        assert!(
            result
                .error
                .unwrap_or_default()
                .contains("Screen Recording permission denied")
        );

        // A failed capture leaves no screenshot behind.
        let screenshot_dir = agent_workspace.path().join(SCREENSHOT_SUBDIR);
        let leftovers = std::fs::read_dir(&screenshot_dir)
            .map(|entries| entries.count())
            .unwrap_or(0);
        assert_eq!(
            leftovers, 0,
            "a denied capture must not create a screenshot file"
        );
    }

    #[tokio::test]
    async fn type_and_key_and_scroll_and_move_dispatch_to_backend() {
        let agent_workspace = TempDir::new().expect("agent workspace");
        let security_workspace = TempDir::new().expect("security workspace");
        let (tool, backend) = happy_tool(security_workspace.path(), agent_workspace.path());

        // screenshot
        let result = tool
            .execute(json!({"action": "screenshot"}))
            .await
            .expect("screenshot runs");
        assert!(result.success, "screenshot failed: {:?}", result.error);

        // click with every option set
        let result = tool
            .execute(json!({
                "action": "click",
                "x": 100,
                "y": 75,
                "button": "right",
                "clicks": 2
            }))
            .await
            .expect("click runs");
        assert!(result.success, "click failed: {:?}", result.error);
        let clicks = backend.clicks();
        assert_eq!(clicks.len(), 1);
        let (point, button, count) = clicks[0];
        // 100 of 200 delivered pixels is 200 of 400 points; 75 of 150 is 150 of 300.
        assert!((point.x - 200.0).abs() < 0.5, "x was {}", point.x);
        assert!((point.y - 150.0).abs() < 0.5, "y was {}", point.y);
        assert_eq!(button, MouseButton::Right);
        assert_eq!(count, 2);

        // drag
        let result = tool
            .execute(json!({
                "action": "drag",
                "from_x": 20,
                "from_y": 30,
                "to_x": 100,
                "to_y": 75,
                "steps": 12
            }))
            .await
            .expect("drag runs");
        assert!(result.success, "drag failed: {:?}", result.error);
        let drags = backend.drags();
        assert_eq!(drags.len(), 1);
        let (from, to, drag_button, steps) = drags[0];
        assert!((from.x - 40.0).abs() < 0.5, "from.x was {}", from.x);
        assert!((from.y - 60.0).abs() < 0.5, "from.y was {}", from.y);
        assert!((to.x - 200.0).abs() < 0.5, "to.x was {}", to.x);
        assert!((to.y - 150.0).abs() < 0.5, "to.y was {}", to.y);
        assert_eq!(drag_button, MouseButton::Left);
        assert_eq!(steps, 12);

        // move
        let result = tool
            .execute(json!({"action": "move", "x": 0, "y": 0}))
            .await
            .expect("move runs");
        assert!(result.success, "move failed: {:?}", result.error);
        assert_eq!(
            backend.moves(),
            vec![LogicalPoint { x: 0.0, y: 0.0 }],
            "the top-left screenshot pixel maps to the display origin"
        );

        // scroll
        let result = tool
            .execute(json!({
                "action": "scroll",
                "x": 10,
                "y": 10,
                "delta_x": -3,
                "delta_y": 5
            }))
            .await
            .expect("scroll runs");
        assert!(result.success, "scroll failed: {:?}", result.error);
        let scrolls = backend.scrolls();
        assert_eq!(scrolls.len(), 1);
        assert_eq!(scrolls[0].1, -3);
        assert_eq!(scrolls[0].2, 5);

        // type — the response must not echo the typed secret
        let secret = "hunter2-correct-horse";
        let result = tool
            .execute(json!({"action": "type", "text": secret}))
            .await
            .expect("type runs");
        assert!(result.success, "type failed: {:?}", result.error);
        assert_eq!(backend.typed(), vec![secret.to_string()]);
        let data = result.output.data().expect("structured output");
        assert_eq!(data["action"], "type");
        assert_eq!(data["chars"], secret.chars().count());
        assert!(
            !result.output.as_str().contains(secret),
            "the response must not echo typed text: {}",
            result.output.as_str()
        );

        // key
        let result = tool
            .execute(json!({"action": "key", "key": "cmd+shift+s"}))
            .await
            .expect("key runs");
        assert!(result.success, "key failed: {:?}", result.error);
        let keys = backend.keys();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, KeyCode::Char('s'));
        assert!(keys[0].modifiers.command);
        assert!(keys[0].modifiers.shift);
        assert_eq!(
            result.output.data().expect("structured")["key"],
            "cmd+shift+s"
        );

        // wait
        let result = tool
            .execute(json!({"action": "wait", "seconds": 0}))
            .await
            .expect("wait runs");
        assert!(result.success, "wait failed: {:?}", result.error);
        assert_eq!(result.output.data().expect("structured")["action"], "wait");

        // Two screenshots in the same second must not collide.
        let result = tool
            .execute(json!({"action": "screenshot"}))
            .await
            .expect("second screenshot runs");
        assert!(result.success);
        assert_eq!(backend.capture_calls(), 2);
        let entries = std::fs::read_dir(agent_workspace.path().join(SCREENSHOT_SUBDIR))
            .expect("screenshot dir exists")
            .count();
        assert_eq!(entries, 2, "each screenshot gets its own file");
    }

    #[tokio::test]
    async fn computer_use_tool_attribution_and_schema() {
        let agent_workspace = TempDir::new().expect("agent workspace");
        let security_workspace = TempDir::new().expect("security workspace");
        let (tool, _backend) = happy_tool(security_workspace.path(), agent_workspace.path());

        // Attribution: a native plugin tool under its own name.
        assert_eq!(tool.role(), Role::Tool(ToolKind::Plugin));
        assert_eq!(tool.alias(), "computer_use");
        assert_eq!(tool.name(), "computer_use");
        assert!(
            !tool.alias().contains('.'),
            "alias must not break the composite"
        );
        assert_eq!(
            tool.description(),
            crate::i18n::get_required_tool_string("tool-computer-use")
        );
        assert!(tool.description().contains("screenshot"));

        // Schema shape the model plans against.
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"], json!(["action"]));
        let actions: Vec<&str> = schema["properties"]["action"]["enum"]
            .as_array()
            .expect("action enum")
            .iter()
            .map(|value| value.as_str().expect("enum entries are strings"))
            .collect();
        assert_eq!(actions, ACTIONS.to_vec());
        for property in [
            "action", "display", "x", "y", "from_x", "from_y", "to_x", "to_y", "button", "clicks",
            "steps", "delta_x", "delta_y", "text", "key", "seconds",
        ] {
            assert!(
                schema["properties"][property].is_object(),
                "{property} must be documented in the schema"
            );
        }
        assert_eq!(
            schema["properties"]["button"]["enum"],
            json!(["left", "right", "middle"])
        );
        assert_eq!(schema["properties"]["clicks"]["enum"], json!([1, 2]));
        assert_eq!(schema["properties"]["display"]["minimum"], 0);
        assert_eq!(schema["properties"]["text"]["maxLength"], MAX_TEXT_CHARS);
        assert_eq!(
            schema["properties"]["text"]["maxLength"],
            json!(4096),
            "the schema bound must match the enforced bound"
        );

        // The read-only and unknown-action messages stay in the catalogue.
        assert!(
            crate::i18n::get_required_tool_string("tool-computer-use-error-read-only")
                .contains("read-only")
        );
        let unknown = crate::i18n::get_required_tool_string_with_args(
            "tool-computer-use-error-unknown-action",
            &[("action", "teleport")],
        );
        assert!(unknown.contains("teleport"), "message was {unknown}");
    }
}
