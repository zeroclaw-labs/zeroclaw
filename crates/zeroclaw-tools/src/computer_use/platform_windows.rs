//! Windows computer-use backend.
//!
//! UI Automation and `SendInput` are accessed only through safe wrapper
//! crates. Every operation gets a fresh COM apartment on a dedicated thread;
//! no configuration, allowlist, UI element, or application identity is cached
//! across calls.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use enigo::{
    Axis, Button as EnigoButton, Coordinate, Direction, Enigo, Key as EnigoKey, Keyboard, Mouse,
    Settings,
};
use sysinfo::{Pid, ProcessesToUpdate, System};
use tokio::sync::oneshot;
use uiautomation::controls::WindowControl;
use uiautomation::patterns::UIInvokePattern;
use uiautomation::screenshots::Screenshot;
#[cfg(test)]
use uiautomation::types::ControlType;
use uiautomation::types::{Point as UiPoint, Rect as UiRect};
use uiautomation::{Error as UiError, UIAutomation, UIElement};

use super::{Backend, ScreenshotReservation};
use crate::computer_use::protocol::{
    AccessibilityNode, AccessibilitySnapshot, Action, ActionKind, ApplicationIdentity,
    DEFAULT_AX_DEPTH, DEFAULT_AX_NODES, ElementSummary, ErrorCode, Key, KeyModifier, MAX_AX_DEPTH,
    MAX_AX_NODES, MAX_AX_STRING_CHARS, MAX_RUNNING_APPLICATIONS, MAX_SCREENSHOT_BYTES, MouseButton,
    PermissionState, Permissions, Platform, Point, Policy, ProtocolError, Rect, ResponseData,
};
#[cfg(test)]
use zeroclaw_config::schema::ComputerUseApplicationAccess;

const FOCUS_POLL_INTERVAL: Duration = Duration::from_millis(50);
const MAX_ROOT_ASCENT: usize = 64;
const MAX_TOP_LEVEL_WINDOWS: usize = 1_024;
const POINTER_TOLERANCE_PIXELS: i32 = 1;
const TREE_WALK_END_HRESULT: i32 = 0x8000_4003_u32 as i32;
const STAGED_SCREENSHOT_NAME: &str = "capture.png";
static WINDOWS_WORKER_ACTIVE: AtomicBool = AtomicBool::new(false);

pub(super) struct PlatformBackend;

#[async_trait]
impl Backend for PlatformBackend {
    async fn execute(
        &self,
        action: &Action,
        policy: &Policy,
        screenshot_reservation: Option<&ScreenshotReservation>,
        deadline: tokio::time::Instant,
    ) -> Result<ResponseData, ProtocolError> {
        match action {
            Action::Capabilities {} => {
                run_windows_thread(deadline, OperationKind::Read, |_| capabilities()).await
            }
            Action::ListApplications {} => {
                let policy = policy.clone();
                run_windows_thread(deadline, OperationKind::Read, move |_| {
                    list_applications(&policy, deadline)
                })
                .await
            }
            Action::Inspect {
                expected_application,
                max_nodes,
                max_depth,
            } => {
                let policy = policy.clone();
                let expected_application = expected_application.clone();
                let max_nodes = max_nodes.unwrap_or(DEFAULT_AX_NODES);
                let max_depth = max_depth.unwrap_or(DEFAULT_AX_DEPTH);
                run_windows_thread(deadline, OperationKind::Read, move |_| {
                    inspect(
                        &policy,
                        &expected_application,
                        max_nodes,
                        max_depth,
                        deadline,
                    )
                })
                .await
            }
            Action::Screenshot { application, path } => {
                let Some(reservation) = screenshot_reservation else {
                    return Err(ProtocolError::new(
                        ErrorCode::InvalidPath,
                        "screenshot destination is unavailable",
                        false,
                    ));
                };
                debug_assert_eq!(reservation.path(), path);
                screenshot(policy, application, reservation, deadline).await
            }
            Action::Focus { application } => {
                let policy = policy.clone();
                let application = application.clone();
                run_windows_thread(deadline, OperationKind::Mutation, move |cancelled| {
                    let automation = new_automation()?;
                    let application =
                        focus_application(&automation, &policy, &application, deadline, cancelled)?;
                    Ok(ResponseData::Focused { application })
                })
                .await
            }
            Action::MouseMove {
                x,
                y,
                expected_application,
            } => {
                let policy = policy.clone();
                let expected_application = expected_application.clone();
                let point = Point { x: *x, y: *y };
                run_windows_thread(deadline, OperationKind::Mutation, move |cancelled| {
                    let application =
                        mouse_move(&policy, &expected_application, point, deadline, cancelled)?;
                    Ok(ResponseData::Input { application })
                })
                .await
            }
            Action::Click {
                x,
                y,
                button,
                expected_application,
            } => {
                let policy = policy.clone();
                let expected_application = expected_application.clone();
                let point = Point { x: *x, y: *y };
                let button = *button;
                run_windows_thread(deadline, OperationKind::Mutation, move |cancelled| {
                    let application = click(
                        &policy,
                        &expected_application,
                        point,
                        button,
                        deadline,
                        cancelled,
                    )?;
                    Ok(ResponseData::Input { application })
                })
                .await
            }
            Action::Scroll {
                delta_x,
                delta_y,
                expected_application,
            } => {
                let policy = policy.clone();
                let expected_application = expected_application.clone();
                let delta_x = *delta_x;
                let delta_y = *delta_y;
                run_windows_thread(deadline, OperationKind::Mutation, move |cancelled| {
                    let application = scroll(
                        &policy,
                        &expected_application,
                        delta_x,
                        delta_y,
                        deadline,
                        cancelled,
                    )?;
                    Ok(ResponseData::Input { application })
                })
                .await
            }
            Action::TypeText { .. } => Err(windows_text_unsupported_error()),
            Action::KeyPress {
                key,
                modifiers,
                expected_application,
            } => {
                let policy = policy.clone();
                let expected_application = expected_application.clone();
                let key = *key;
                let modifiers = modifiers.clone();
                run_windows_thread(deadline, OperationKind::Mutation, move |cancelled| {
                    let application = key_press(
                        &policy,
                        &expected_application,
                        key,
                        &modifiers,
                        deadline,
                        cancelled,
                    )?;
                    Ok(ResponseData::Input { application })
                })
                .await
            }
            Action::PressElement {
                application,
                role,
                title,
            } => {
                let policy = policy.clone();
                let application = application.clone();
                let role = role.clone();
                let title = title.clone();
                run_windows_thread(deadline, OperationKind::Mutation, move |cancelled| {
                    press_element(
                        &policy,
                        &application,
                        role.as_deref(),
                        &title,
                        deadline,
                        cancelled,
                    )
                })
                .await
            }
        }
    }
}

#[derive(Clone, Copy)]
enum OperationKind {
    Read,
    Mutation,
}

impl OperationKind {
    fn timeout_error(self) -> ProtocolError {
        let error = ProtocolError::new(
            ErrorCode::Timeout,
            "Windows computer-use operation exceeded its deadline",
            false,
        );
        match self {
            Self::Read => error,
            Self::Mutation => error.with_unknown_outcome(),
        }
    }

    fn annotate_error(self, error: ProtocolError) -> ProtocolError {
        match self {
            Self::Read => error,
            Self::Mutation => error.with_unknown_outcome(),
        }
    }
}

struct CancellationOnDrop {
    state: Arc<AtomicBool>,
}

impl CancellationOnDrop {
    fn new() -> Self {
        Self {
            state: Arc::new(AtomicBool::new(false)),
        }
    }

    fn worker_state(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.state)
    }

    fn cancel(&self) {
        self.state.store(true, Ordering::Release);
    }
}

impl Drop for CancellationOnDrop {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[derive(Debug)]
struct WindowsWorkerLease;

impl WindowsWorkerLease {
    fn acquire() -> Result<Self, ProtocolError> {
        WINDOWS_WORKER_ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                ProtocolError::new(
                    ErrorCode::CommandFailed,
                    "a previous Windows UI Automation worker is still active",
                    true,
                )
            })?;
        Ok(Self)
    }

    fn is_held(&self) -> bool {
        WINDOWS_WORKER_ACTIVE.load(Ordering::Acquire)
    }
}

impl Drop for WindowsWorkerLease {
    fn drop(&mut self) {
        WINDOWS_WORKER_ACTIVE.store(false, Ordering::Release);
    }
}

async fn run_windows_thread<T, F>(
    deadline: tokio::time::Instant,
    operation: OperationKind,
    task: F,
) -> Result<T, ProtocolError>
where
    T: Send + 'static,
    F: FnOnce(&AtomicBool) -> Result<T, ProtocolError> + Send + 'static,
{
    if tokio::time::Instant::now() >= deadline {
        return Err(operation.timeout_error());
    }

    let worker_lease = WindowsWorkerLease::acquire()?;
    let (sender, receiver) = oneshot::channel();
    let cancellation = CancellationOnDrop::new();
    let worker_cancelled = cancellation.worker_state();
    std::thread::Builder::new()
        .name("zeroclaw-windows-uia".to_owned())
        .spawn(move || {
            let result = if worker_cancelled.load(Ordering::Acquire) {
                Err(operation.timeout_error())
            } else {
                task(&worker_cancelled)
            };
            debug_assert!(worker_lease.is_held());
            let _ = sender.send(result);
        })
        .map_err(|_| {
            ProtocolError::new(
                ErrorCode::CommandFailed,
                "could not start the Windows UI Automation worker",
                true,
            )
        })?;

    match tokio::time::timeout_at(deadline, receiver).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => {
            let error = ProtocolError::new(
                ErrorCode::CommandFailed,
                "Windows UI Automation worker stopped without a response",
                matches!(operation, OperationKind::Read),
            );
            match operation {
                OperationKind::Read => Err(error),
                OperationKind::Mutation => Err(error.with_unknown_outcome()),
            }
        }
        Err(_) => {
            cancellation.cancel();
            Err(operation.timeout_error())
        }
    }
}

fn new_automation() -> Result<UIAutomation, ProtocolError> {
    UIAutomation::new().map_err(|_| accessibility_error())
}

fn new_enigo() -> Result<Enigo, ProtocolError> {
    Enigo::new(&Settings::default()).map_err(|_| input_setup_error())
}

/// Per-call materialized view of the live foreground UIA root.
struct ResolvedTarget {
    identity: ApplicationIdentity,
    root: UIElement,
}

struct WalkEntry {
    element: UIElement,
    parent_id: Option<u32>,
    depth: u32,
}

struct WalkResult {
    entries: Vec<WalkEntry>,
    truncated: bool,
}

struct StagedScreenshot {
    directory: tempfile::TempDir,
    size: u64,
    display_bounds: Rect,
    pixel_width: u64,
    pixel_height: u64,
}

impl StagedScreenshot {
    fn path(&self) -> std::path::PathBuf {
        self.directory.path().join(STAGED_SCREENSHOT_NAME)
    }
}

fn capabilities() -> Result<ResponseData, ProtocolError> {
    let accessibility = UIAutomation::new()
        .and_then(|automation| automation.get_root_element())
        // UIA readiness does not prove access to an arbitrary target: UIPI is
        // evaluated against the target process at action time.
        .map_or(PermissionState::Denied, |_| PermissionState::Unknown);

    Ok(ResponseData::Capabilities {
        platform: Platform::Windows,
        actions: windows_actions(),
        permissions: Permissions {
            accessibility,
            // Windows has no non-capturing equivalent of the macOS TCC
            // preflight. Do not capture pixels merely to answer capabilities.
            screen_recording: PermissionState::Unknown,
        },
    })
}

fn windows_actions() -> Vec<ActionKind> {
    ActionKind::ALL
        .iter()
        .copied()
        .filter(|action| !matches!(action, ActionKind::TypeText))
        .collect()
}

fn list_applications(
    policy: &Policy,
    deadline: tokio::time::Instant,
) -> Result<ResponseData, ProtocolError> {
    let automation = new_automation()?;
    let root = automation
        .get_root_element()
        .map_err(|_| accessibility_error())?;
    let walker = automation
        .get_control_view_walker()
        .map_err(|_| accessibility_error())?;

    let mut applications = Vec::new();
    let mut seen_pids = HashSet::new();
    let mut truncated = false;
    let mut inspected = 0_usize;
    let mut current = optional_tree_element(walker.get_first_child(&root), OperationKind::Read)?;
    while let Some(element) = current {
        check_deadline(deadline, OperationKind::Read)?;
        inspected += 1;
        if inspected > MAX_TOP_LEVEL_WINDOWS {
            truncated = true;
            break;
        }

        let identity = optional_application_identity(identity_from_root(&element))?;
        if let Some(identity) = identity
            && windows_policy_allows(policy, &identity)
            && seen_pids.insert(identity.pid)
        {
            if applications.len() == MAX_RUNNING_APPLICATIONS {
                truncated = true;
                break;
            }
            applications.push(identity);
        }
        current = optional_tree_element(walker.get_next_sibling(&element), OperationKind::Read)?;
    }

    Ok(ResponseData::Applications {
        applications,
        truncated,
    })
}

fn inspect(
    policy: &Policy,
    expected: &str,
    max_nodes: u32,
    max_depth: u32,
    deadline: tokio::time::Instant,
) -> Result<ResponseData, ProtocolError> {
    let automation = new_automation()?;
    let target = resolve_foreground(&automation, policy, Some(expected), deadline)?;
    let walked = walk_control_tree(
        &automation,
        &target,
        max_nodes,
        max_depth,
        deadline,
        OperationKind::Read,
    )?;

    let mut nodes = Vec::with_capacity(walked.entries.len());
    for (index, entry) in walked.entries.iter().enumerate() {
        check_deadline(deadline, OperationKind::Read)?;
        nodes.push(accessibility_node(node_id_for_index(index)?, entry));
    }
    let current = revalidate_foreground(&automation, policy, expected, &target, deadline)?;

    Ok(ResponseData::Inspect {
        snapshot: AccessibilitySnapshot {
            application: current.identity,
            nodes,
            truncated: walked.truncated,
            max_nodes,
            max_depth,
        },
    })
}

fn press_element(
    policy: &Policy,
    application: &str,
    requested_role: Option<&str>,
    requested_title: &str,
    deadline: tokio::time::Instant,
    cancelled: &AtomicBool,
) -> Result<ResponseData, ProtocolError> {
    let automation = new_automation()?;
    let focused = focus_target(&automation, policy, application, deadline, cancelled)?;
    let walked = walk_control_tree(
        &automation,
        &focused,
        MAX_AX_NODES,
        MAX_AX_DEPTH,
        deadline,
        OperationKind::Mutation,
    )?;

    let mut matches = Vec::new();
    for entry in walked.entries {
        check_deadline(deadline, OperationKind::Mutation)?;
        let title = exact_ui_selector(
            entry
                .element
                .get_name()
                .map_err(|_| accessibility_error().with_unknown_outcome())?,
        );
        let role =
            required_role_string(&entry.element).map_err(ProtocolError::with_unknown_outcome)?;
        if title.as_deref() == Some(requested_title)
            && requested_role.is_none_or(|expected| expected == role)
        {
            matches.push((entry.element, role, title));
            if matches.len() > 1 {
                break;
            }
        }
    }

    if matches.len() > 1 || (matches.len() == 1 && walked.truncated) {
        return Err(ProtocolError::new(
            ErrorCode::AmbiguousElement,
            "Windows UI Automation could not prove the element selector is unique",
            false,
        ));
    }
    let Some((element, role, title)) = matches.pop() else {
        return Err(ProtocolError::new(
            ErrorCode::ElementNotFound,
            "Windows UI Automation did not find the requested element",
            true,
        ));
    };

    let runtime_id = element
        .get_runtime_id()
        .map_err(|_| stale_element_error())?;
    if runtime_id.is_empty() || element.get_process_id().ok() != Some(focused.identity.pid) {
        return Err(stale_element_error());
    }
    if element.is_enabled().ok() != Some(true) {
        return Err(ProtocolError::new(
            ErrorCode::ElementNotFound,
            "the requested Windows UI Automation element is not enabled",
            true,
        ));
    }
    let summary = ElementSummary {
        role: role.clone(),
        title: title.clone(),
        bounds: element_bounds(&element),
    };

    let current = revalidate_foreground(&automation, policy, application, &focused, deadline)?;
    if element.get_runtime_id().ok().as_deref() != Some(runtime_id.as_slice())
        || element.get_name().ok().and_then(exact_ui_selector) != title
        || required_role_string(&element).ok().as_deref() != Some(role.as_str())
        || element.is_enabled().ok() != Some(true)
    {
        return Err(stale_element_error().with_unknown_outcome());
    }
    let element_root = application_root(&automation, element.clone())
        .map_err(ProtocolError::with_unknown_outcome)?;
    if !automation
        .compare_elements(&element_root, &current.root)
        .map_err(|_| accessibility_error().with_unknown_outcome())?
    {
        return Err(stale_element_error().with_unknown_outcome());
    }
    let invoke = element.get_pattern::<UIInvokePattern>().map_err(|_| {
        ProtocolError::new(
            ErrorCode::ElementNotFound,
            "the requested Windows UI Automation element cannot be invoked",
            false,
        )
    })?;
    check_effect(deadline, OperationKind::Mutation, cancelled)?;
    invoke.invoke().map_err(|_| input_permission_error())?;

    let after = revalidate_foreground(&automation, policy, application, &current, deadline)
        .map_err(ProtocolError::with_unknown_outcome)?;
    Ok(ResponseData::ElementPressed {
        application: after.identity,
        element: summary,
    })
}

async fn screenshot(
    policy: &Policy,
    application: &str,
    reservation: &ScreenshotReservation,
    deadline: tokio::time::Instant,
) -> Result<ResponseData, ProtocolError> {
    reservation.verify_path_identity().map_err(|_| {
        ProtocolError::new(
            ErrorCode::InvalidPath,
            "held screenshot destination is invalid",
            false,
        )
    })?;
    let policy = policy.clone();
    let application = application.to_owned();

    let staged = run_windows_thread(deadline, OperationKind::Mutation, move |cancelled| {
        stage_screenshot(&policy, &application, deadline, cancelled)
    })
    .await?;

    let mut source = tokio::fs::File::open(staged.path()).await.map_err(|_| {
        ProtocolError::new(
            ErrorCode::ScreenCaptureUnavailable,
            "could not open the staged Windows screenshot",
            false,
        )
        .with_unknown_outcome()
    })?;
    let copy =
        reservation.replace_from_bounded_reader(&mut source, staged.size, MAX_SCREENSHOT_BYTES);
    match tokio::time::timeout_at(deadline, copy).await {
        Ok(Ok(_)) => {}
        Ok(Err(_)) => {
            return Err(ProtocolError::new(
                ErrorCode::InvalidPath,
                "could not write the held screenshot destination",
                false,
            )
            .with_unknown_outcome());
        }
        Err(_) => return Err(OperationKind::Mutation.timeout_error()),
    }

    Ok(ResponseData::Screenshot {
        path: reservation.path().to_path_buf(),
        display_bounds: staged.display_bounds,
        pixel_width: staged.pixel_width,
        pixel_height: staged.pixel_height,
    })
}

fn stage_screenshot(
    policy: &Policy,
    application: &str,
    deadline: tokio::time::Instant,
    cancelled: &AtomicBool,
) -> Result<StagedScreenshot, ProtocolError> {
    let directory = tempfile::Builder::new()
        .prefix(".zeroclaw-computer-use-windows-")
        .tempdir()
        .map_err(|_| io_error("could not create private screenshot staging"))?;
    let staging_path = directory.path().join(STAGED_SCREENSHOT_NAME);
    let automation = new_automation()?;
    let enigo = new_enigo()?;
    let focused = focus_target(&automation, policy, application, deadline, cancelled)?;
    revalidate_foreground(&automation, policy, application, &focused, deadline)?;
    let (width, height) = enigo.main_display().map_err(|_| screen_capture_error())?;
    let (pixel_width, pixel_height) =
        positive_display_size(width, height).ok_or_else(screen_capture_error)?;
    let raw_bytes = pixel_width
        .checked_mul(pixel_height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(screen_capture_error)?;
    if raw_bytes > MAX_SCREENSHOT_BYTES {
        return Err(ProtocolError::new(
            ErrorCode::OutputTooLarge,
            "main-display screenshot exceeds the Windows capture memory limit",
            false,
        )
        .with_unknown_outcome());
    }

    check_effect(deadline, OperationKind::Mutation, cancelled)?;
    let capture = Screenshot::capture_rect(UiRect::new(0, 0, width, height))
        .map_err(|_| screen_capture_error().with_unknown_outcome())?;
    revalidate_foreground(&automation, policy, application, &focused, deadline)
        .map_err(ProtocolError::with_unknown_outcome)?;
    if u64::from(capture.width()) != pixel_width || u64::from(capture.height()) != pixel_height {
        return Err(ProtocolError::new(
            ErrorCode::ScreenCaptureUnavailable,
            "Windows screenshot dimensions changed during capture",
            true,
        )
        .with_unknown_outcome());
    }
    check_effect(deadline, OperationKind::Mutation, cancelled)?;
    capture
        .save_png(&staging_path)
        .map_err(|_| screen_capture_error().with_unknown_outcome())?;
    let metadata = std::fs::symlink_metadata(&staging_path)
        .map_err(|_| screen_capture_error().with_unknown_outcome())?;
    if !metadata.file_type().is_file()
        || metadata.len() == 0
        || metadata.len() > MAX_SCREENSHOT_BYTES
    {
        return Err(ProtocolError::new(
            ErrorCode::ScreenCaptureUnavailable,
            "staged Windows screenshot is not one bounded regular file",
            false,
        )
        .with_unknown_outcome());
    }

    Ok(StagedScreenshot {
        directory,
        size: metadata.len(),
        display_bounds: Rect {
            x: 0.0,
            y: 0.0,
            width: f64::from(width),
            height: f64::from(height),
        },
        pixel_width,
        pixel_height,
    })
}

fn mouse_move(
    policy: &Policy,
    expected: &str,
    point: Point,
    deadline: tokio::time::Instant,
    cancelled: &AtomicBool,
) -> Result<ApplicationIdentity, ProtocolError> {
    let automation = new_automation()?;
    let mut enigo = new_enigo()?;
    let focused = focus_target(&automation, policy, expected, deadline, cancelled)?;
    let screen_point = checked_primary_display_point(&enigo, point)?;
    require_point_owner(&automation, &focused, screen_point, deadline)?;
    let current = revalidate_foreground(&automation, policy, expected, &focused, deadline)?;
    check_effect(deadline, OperationKind::Mutation, cancelled)?;
    enigo
        .move_mouse(screen_point.get_x(), screen_point.get_y(), Coordinate::Abs)
        .map_err(|_| input_permission_error())?;

    let after = revalidate_foreground(&automation, policy, expected, &current, deadline)
        .map_err(ProtocolError::with_unknown_outcome)?;
    let actual_point = require_pointer_location(&enigo, screen_point)?;
    authorize_pointer_point(&enigo, &automation, &after, policy, actual_point, deadline)
        .map_err(ProtocolError::with_unknown_outcome)?;
    Ok(after.identity)
}

fn click(
    policy: &Policy,
    expected: &str,
    point: Point,
    button: MouseButton,
    deadline: tokio::time::Instant,
    cancelled: &AtomicBool,
) -> Result<ApplicationIdentity, ProtocolError> {
    let automation = new_automation()?;
    let mut enigo = new_enigo()?;
    let focused = focus_target(&automation, policy, expected, deadline, cancelled)?;
    let screen_point = checked_primary_display_point(&enigo, point)?;
    require_point_owner(&automation, &focused, screen_point, deadline)?;
    revalidate_foreground(&automation, policy, expected, &focused, deadline)?;

    check_effect(deadline, OperationKind::Mutation, cancelled)?;
    enigo
        .move_mouse(screen_point.get_x(), screen_point.get_y(), Coordinate::Abs)
        .map_err(|_| input_permission_error())?;
    let moved = revalidate_foreground(&automation, policy, expected, &focused, deadline)
        .map_err(ProtocolError::with_unknown_outcome)?;
    let actual_point = require_pointer_location(&enigo, screen_point)?;
    authorize_pointer_point(&enigo, &automation, &moved, policy, actual_point, deadline)
        .map_err(ProtocolError::with_unknown_outcome)?;

    let mut release = MouseReleaseGuard::new(&mut enigo, enigo_button(button));
    check_effect(deadline, OperationKind::Mutation, cancelled)?;
    let actual_point = require_pointer_location(&*release.enigo, screen_point)?;
    authorize_pointer_point(
        &*release.enigo,
        &automation,
        &moved,
        policy,
        actual_point,
        deadline,
    )
    .map_err(ProtocolError::with_unknown_outcome)?;
    check_effect(deadline, OperationKind::Mutation, cancelled)
        .map_err(ProtocolError::with_unknown_outcome)?;
    release.press()?;
    let pressed = revalidate_foreground(&automation, policy, expected, &moved, deadline)
        .map_err(ProtocolError::with_unknown_outcome)?;
    let actual_point = require_pointer_location(&*release.enigo, screen_point)?;
    authorize_pointer_point(
        &*release.enigo,
        &automation,
        &pressed,
        policy,
        actual_point,
        deadline,
    )
    .map_err(ProtocolError::with_unknown_outcome)?;
    check_effect(deadline, OperationKind::Mutation, cancelled)
        .map_err(ProtocolError::with_unknown_outcome)?;
    release.release()?;
    Ok(pressed.identity)
}

fn scroll(
    policy: &Policy,
    expected: &str,
    delta_x: i32,
    delta_y: i32,
    deadline: tokio::time::Instant,
    cancelled: &AtomicBool,
) -> Result<ApplicationIdentity, ProtocolError> {
    let automation = new_automation()?;
    let mut enigo = new_enigo()?;
    let focused = focus_target(&automation, policy, expected, deadline, cancelled)?;
    let mut current = revalidate_foreground(&automation, policy, expected, &focused, deadline)?;
    let mut crossed_boundary = false;

    if delta_x != 0 {
        current_authorized_pointer(&enigo, &automation, &current, policy, deadline)?;
        check_effect(deadline, OperationKind::Mutation, cancelled)?;
        enigo
            .scroll(delta_x, Axis::Horizontal)
            .map_err(|_| input_permission_error())?;
        crossed_boundary = true;
        current = revalidate_foreground(&automation, policy, expected, &current, deadline)
            .map_err(ProtocolError::with_unknown_outcome)?;
    }
    if delta_y != 0 {
        let pointer_result =
            current_authorized_pointer(&enigo, &automation, &current, policy, deadline);
        if crossed_boundary {
            pointer_result.map_err(ProtocolError::with_unknown_outcome)?;
        } else {
            pointer_result?;
        }
        let deadline_result = check_effect(deadline, OperationKind::Mutation, cancelled);
        if crossed_boundary {
            deadline_result.map_err(ProtocolError::with_unknown_outcome)?;
        } else {
            deadline_result?;
        }
        enigo
            .scroll(delta_y, Axis::Vertical)
            .map_err(|_| input_permission_error())?;
        current = revalidate_foreground(&automation, policy, expected, &current, deadline)
            .map_err(ProtocolError::with_unknown_outcome)?;
    }
    Ok(current.identity)
}

fn key_press(
    policy: &Policy,
    expected: &str,
    key: Key,
    modifiers: &[KeyModifier],
    deadline: tokio::time::Instant,
    cancelled: &AtomicBool,
) -> Result<ApplicationIdentity, ProtocolError> {
    let automation = new_automation()?;
    let mut enigo = new_enigo()?;
    let focused = focus_target(&automation, policy, expected, deadline, cancelled)?;
    let mut current = revalidate_foreground(&automation, policy, expected, &focused, deadline)?;
    let key = enigo_key(key);
    let modifier_keys = modifiers
        .iter()
        .copied()
        .map(enigo_modifier)
        .collect::<Result<Vec<_>, _>>()?;
    let mut release = KeyReleaseGuard::new(&mut enigo);

    for modifier in modifier_keys {
        check_effect(deadline, OperationKind::Mutation, cancelled)?;
        release.press(modifier)?;
        current = revalidate_foreground(&automation, policy, expected, &current, deadline)
            .map_err(ProtocolError::with_unknown_outcome)?;
    }
    check_effect(deadline, OperationKind::Mutation, cancelled)
        .map_err(ProtocolError::with_unknown_outcome)?;
    release.press(key)?;
    current = revalidate_foreground(&automation, policy, expected, &current, deadline)
        .map_err(ProtocolError::with_unknown_outcome)?;
    check_effect(deadline, OperationKind::Mutation, cancelled)
        .map_err(ProtocolError::with_unknown_outcome)?;
    release.release_one(key)?;
    current = revalidate_foreground(&automation, policy, expected, &current, deadline)
        .map_err(ProtocolError::with_unknown_outcome)?;
    while release.has_held() {
        check_effect(deadline, OperationKind::Mutation, cancelled)
            .map_err(ProtocolError::with_unknown_outcome)?;
        release.release_last()?;
        current = revalidate_foreground(&automation, policy, expected, &current, deadline)
            .map_err(ProtocolError::with_unknown_outcome)?;
    }
    Ok(current.identity)
}

fn focus_application(
    automation: &UIAutomation,
    policy: &Policy,
    application: &str,
    deadline: tokio::time::Instant,
    cancelled: &AtomicBool,
) -> Result<ApplicationIdentity, ProtocolError> {
    Ok(focus_target(automation, policy, application, deadline, cancelled)?.identity)
}

fn focus_target(
    automation: &UIAutomation,
    policy: &Policy,
    application: &str,
    deadline: tokio::time::Instant,
    cancelled: &AtomicBool,
) -> Result<ResolvedTarget, ProtocolError> {
    let target = find_application(automation, policy, application, deadline)?;
    let window = WindowControl::try_from(target.root.clone()).map_err(|_| {
        ProtocolError::new(
            ErrorCode::AccessibilityUnavailable,
            "Windows UI Automation target is not a top-level window control",
            false,
        )
    })?;
    check_effect(deadline, OperationKind::Mutation, cancelled)?;
    if !window
        .set_foregrand()
        .map_err(|_| input_permission_error())?
    {
        return Err(input_permission_error());
    }

    loop {
        match resolve_foreground(automation, policy, Some(application), deadline) {
            Ok(current) => {
                require_same_target(automation, &target, &current)?;
                return Ok(current);
            }
            Err(error)
                if matches!(
                    error.code,
                    ErrorCode::ApplicationMismatch | ErrorCode::ApplicationNotFound
                ) && tokio::time::Instant::now() < deadline =>
            {
                std::thread::sleep(FOCUS_POLL_INTERVAL);
            }
            Err(error) => return Err(error.with_unknown_outcome()),
        }
    }
}

fn find_application(
    automation: &UIAutomation,
    policy: &Policy,
    application: &str,
    deadline: tokio::time::Instant,
) -> Result<ResolvedTarget, ProtocolError> {
    let desktop = automation
        .get_root_element()
        .map_err(|_| accessibility_error())?;
    let walker = automation
        .get_control_view_walker()
        .map_err(|_| accessibility_error())?;
    let mut current = optional_tree_element(walker.get_first_child(&desktop), OperationKind::Read)?;
    let mut inspected = 0_usize;
    let mut matched: Option<ResolvedTarget> = None;

    while let Some(element) = current {
        check_deadline(deadline, OperationKind::Mutation)?;
        inspected += 1;
        if inspected > MAX_TOP_LEVEL_WINDOWS {
            return Err(ProtocolError::new(
                ErrorCode::ApplicationNotFound,
                "Windows top-level application search exceeded its safety bound",
                true,
            ));
        }
        let identity = optional_application_identity(identity_from_root(&element))?;
        if let Some(identity) = identity
            && windows_policy_allows(policy, &identity)
            && identity.matches(application)
        {
            let candidate = ResolvedTarget {
                identity,
                root: element.clone(),
            };
            if let Some(existing) = &matched {
                if existing.identity.pid != candidate.identity.pid {
                    return Err(ProtocolError::new(
                        ErrorCode::ApplicationMismatch,
                        "more than one Windows process matches the exact application selector",
                        false,
                    ));
                }
            } else {
                matched = Some(candidate);
            }
        }
        current = optional_tree_element(walker.get_next_sibling(&element), OperationKind::Read)?;
    }

    matched.ok_or_else(|| {
        ProtocolError::new(
            ErrorCode::ApplicationNotFound,
            "Windows did not report the requested running application",
            true,
        )
    })
}

fn resolve_foreground(
    automation: &UIAutomation,
    policy: &Policy,
    expected: Option<&str>,
    deadline: tokio::time::Instant,
) -> Result<ResolvedTarget, ProtocolError> {
    check_deadline(deadline, OperationKind::Read)?;
    let focused = automation.get_focused_element().map_err(|_| {
        ProtocolError::new(
            ErrorCode::ApplicationNotFound,
            "Windows did not report a focused UI Automation element",
            true,
        )
    })?;
    let root = application_root(automation, focused)?;
    let identity = identity_from_root(&root)?;
    if !windows_policy_allows(policy, &identity) {
        return Err(ProtocolError::new(
            ErrorCode::ApplicationNotAllowed,
            "the focused Windows application is not allowed by this request",
            false,
        ));
    }
    if expected.is_some_and(|selector| !identity.matches(selector)) {
        return Err(ProtocolError::new(
            ErrorCode::ApplicationMismatch,
            "the focused Windows application does not match expected_application",
            true,
        ));
    }
    Ok(ResolvedTarget { identity, root })
}

fn revalidate_foreground(
    automation: &UIAutomation,
    policy: &Policy,
    expected: &str,
    previous: &ResolvedTarget,
    deadline: tokio::time::Instant,
) -> Result<ResolvedTarget, ProtocolError> {
    let current = resolve_foreground(automation, policy, Some(expected), deadline)?;
    require_same_target(automation, previous, &current)?;
    Ok(current)
}

fn require_same_target(
    automation: &UIAutomation,
    expected: &ResolvedTarget,
    actual: &ResolvedTarget,
) -> Result<(), ProtocolError> {
    let same_element = automation
        .compare_elements(&expected.root, &actual.root)
        .map_err(|_| accessibility_error())?;
    let expected_window: isize = expected
        .root
        .get_native_window_handle()
        .map_err(|_| accessibility_error())?
        .into();
    let actual_window: isize = actual
        .root
        .get_native_window_handle()
        .map_err(|_| accessibility_error())?
        .into();
    if expected.identity.pid != actual.identity.pid
        || expected.identity.name != actual.identity.name
        || expected_window != actual_window
        || !same_element
    {
        return Err(ProtocolError::new(
            ErrorCode::ApplicationMismatch,
            "the focused Windows application changed during the operation",
            true,
        ));
    }
    Ok(())
}

fn application_root(
    automation: &UIAutomation,
    element: UIElement,
) -> Result<UIElement, ProtocolError> {
    let desktop = automation
        .get_root_element()
        .map_err(|_| accessibility_error())?;
    let walker = automation
        .get_control_view_walker()
        .map_err(|_| accessibility_error())?;
    let pid = element
        .get_process_id()
        .map_err(|_| accessibility_error())?;
    if pid == 0 {
        return Err(ProtocolError::new(
            ErrorCode::ApplicationNotFound,
            "focused Windows UI Automation element has no process identity",
            true,
        ));
    }

    let mut current = element;
    for _ in 0..MAX_ROOT_ASCENT {
        let parent = walker.get_parent(&current).map_err(|_| {
            ProtocolError::new(
                ErrorCode::AccessibilityUnavailable,
                "Windows UI Automation could not prove application ancestry",
                true,
            )
        })?;
        if automation
            .compare_elements(&parent, &desktop)
            .map_err(|_| accessibility_error())?
        {
            return Ok(current);
        }
        if parent.get_process_id().map_err(|_| accessibility_error())? != pid {
            return Err(ProtocolError::new(
                ErrorCode::AccessibilityUnavailable,
                "Windows UI Automation application ancestry crossed a process boundary",
                true,
            ));
        }
        current = parent;
    }
    Err(ProtocolError::new(
        ErrorCode::AccessibilityUnavailable,
        "Windows UI Automation application ancestry exceeded its safety bound",
        true,
    ))
}

fn identity_from_root(root: &UIElement) -> Result<ApplicationIdentity, ProtocolError> {
    let pid = root.get_process_id().map_err(|_| accessibility_error())?;
    if pid == 0 {
        return Err(ProtocolError::new(
            ErrorCode::ApplicationNotFound,
            "Windows application has no process identity",
            true,
        ));
    }
    if root
        .get_native_window_handle()
        .map_err(|_| accessibility_error())?
        .is_invalid()
    {
        return Err(ProtocolError::new(
            ErrorCode::ApplicationNotFound,
            "Windows application has no native top-level window identity",
            true,
        ));
    }
    let process_pid = Pid::from_u32(pid);
    let process_pids = [process_pid];
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::Some(&process_pids), true);
    let process = system.process(process_pid).ok_or_else(|| {
        ProtocolError::new(
            ErrorCode::ApplicationNotFound,
            "Windows application process disappeared during identity resolution",
            true,
        )
    })?;
    let image_name = process.name().to_str().ok_or_else(|| {
        ProtocolError::new(
            ErrorCode::ApplicationNotFound,
            "Windows application process name is not valid Unicode",
            true,
        )
    })?;
    let image_name = exact_application_string(image_name.to_owned())?;
    let display_name = image_name
        .rsplit_once('.')
        .filter(|(_, extension)| extension.eq_ignore_ascii_case("exe"))
        .map_or(image_name.as_str(), |(stem, _)| stem);
    let name = exact_application_string(display_name.to_owned())?;
    if root.get_process_id().map_err(|_| accessibility_error())? != pid
        || root
            .get_native_window_handle()
            .map_err(|_| accessibility_error())?
            .is_invalid()
    {
        return Err(ProtocolError::new(
            ErrorCode::ApplicationMismatch,
            "Windows application identity changed while resolving its process name",
            true,
        ));
    }
    Ok(ApplicationIdentity {
        name,
        // The protocol's identifier namespace is the only namespace in which
        // a dotted Windows image name (for example, `notepad.exe`) is usable.
        bundle_id: Some(image_name),
        pid,
    })
}

fn windows_policy_allows(policy: &Policy, identity: &ApplicationIdentity) -> bool {
    policy.allows(identity)
}

fn optional_application_identity(
    result: Result<ApplicationIdentity, ProtocolError>,
) -> Result<Option<ApplicationIdentity>, ProtocolError> {
    match result {
        Ok(identity) => Ok(Some(identity)),
        Err(error) if error.code == ErrorCode::ApplicationNotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn node_id_for_index(index: usize) -> Result<u32, ProtocolError> {
    index
        .checked_add(1)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| {
            ProtocolError::new(
                ErrorCode::ProtocolViolation,
                "Windows accessibility node identifier exceeded protocol bounds",
                false,
            )
        })
}

fn optional_tree_element(
    result: Result<UIElement, UiError>,
    operation: OperationKind,
) -> Result<Option<UIElement>, ProtocolError> {
    match result {
        Ok(element) => Ok(Some(element)),
        Err(error) if error.code() == TREE_WALK_END_HRESULT => Ok(None),
        Err(_) => Err(operation.annotate_error(accessibility_error())),
    }
}

fn walk_control_tree(
    automation: &UIAutomation,
    target: &ResolvedTarget,
    max_nodes: u32,
    max_depth: u32,
    deadline: tokio::time::Instant,
    operation: OperationKind,
) -> Result<WalkResult, ProtocolError> {
    let walker = automation
        .get_control_view_walker()
        .map_err(|_| operation.annotate_error(accessibility_error()))?;
    let mut queue = VecDeque::new();
    let mut entries = Vec::with_capacity(max_nodes as usize);
    let mut truncated = false;
    queue.push_back((target.root.clone(), None, 0_u32));

    while let Some((element, parent_id, depth)) = queue.pop_front() {
        check_deadline(deadline, operation)?;
        if entries.len() == max_nodes as usize {
            truncated = true;
            break;
        }
        if element
            .get_process_id()
            .map_err(|_| operation.annotate_error(accessibility_error()))?
            != target.identity.pid
        {
            truncated = true;
            continue;
        }
        let id = node_id_for_index(entries.len())?;
        entries.push(WalkEntry {
            element: element.clone(),
            parent_id,
            depth,
        });

        if depth >= max_depth {
            if optional_tree_element(walker.get_first_child(&element), operation)?.is_some() {
                truncated = true;
            }
            continue;
        }

        let mut child = optional_tree_element(walker.get_first_child(&element), operation)?;
        while let Some(current) = child {
            check_deadline(deadline, operation)?;
            if entries.len().saturating_add(queue.len()) >= max_nodes as usize {
                truncated = true;
                break;
            }
            if current
                .get_process_id()
                .map_err(|_| operation.annotate_error(accessibility_error()))?
                == target.identity.pid
            {
                queue.push_back((current.clone(), Some(id), depth + 1));
            } else {
                truncated = true;
            }
            child = optional_tree_element(walker.get_next_sibling(&current), operation)?;
        }
    }
    if !queue.is_empty() {
        truncated = true;
    }
    Ok(WalkResult { entries, truncated })
}

fn accessibility_node(id: u32, entry: &WalkEntry) -> AccessibilityNode {
    let secure = entry.element.is_password().unwrap_or(true);
    let role = role_string(&entry.element);
    let title = if secure {
        None
    } else {
        optional_ui_string(entry.element.get_name().ok())
    };
    let description = if secure {
        None
    } else {
        optional_ui_string(entry.element.get_help_text().ok())
    };
    let actions = if entry.element.get_pattern::<UIInvokePattern>().is_ok() {
        vec!["invoke".to_owned()]
    } else {
        Vec::new()
    };
    AccessibilityNode {
        id,
        parent_id: entry.parent_id,
        depth: entry.depth,
        role,
        title,
        value: None,
        description,
        enabled: entry.element.is_enabled().ok(),
        focused: entry.element.has_keyboard_focus().ok(),
        bounds: element_bounds(&entry.element),
        actions,
    }
}

fn role_string(element: &UIElement) -> String {
    element
        .get_control_type()
        .map_or_else(|_| "Unknown".to_owned(), |role| format!("{role:?}"))
}

fn required_role_string(element: &UIElement) -> Result<String, ProtocolError> {
    element
        .get_control_type()
        .map(|role| format!("{role:?}"))
        .map_err(|_| accessibility_error())
}

fn element_bounds(element: &UIElement) -> Option<Rect> {
    element.get_bounding_rectangle().ok().and_then(|bounds| {
        let width = bounds.get_right().checked_sub(bounds.get_left())?;
        let height = bounds.get_bottom().checked_sub(bounds.get_top())?;
        if width <= 0 || height <= 0 {
            return None;
        }
        Some(Rect {
            x: f64::from(bounds.get_left()),
            y: f64::from(bounds.get_top()),
            width: f64::from(width),
            height: f64::from(height),
        })
    })
}

fn require_point_owner(
    automation: &UIAutomation,
    target: &ResolvedTarget,
    point: UiPoint,
    deadline: tokio::time::Instant,
) -> Result<(), ProtocolError> {
    check_deadline(deadline, OperationKind::Mutation)?;
    let element = automation.element_from_point(point).map_err(|_| {
        ProtocolError::new(
            ErrorCode::ApplicationMismatch,
            "Windows could not resolve the application at the requested coordinate",
            true,
        )
    })?;
    if element.get_process_id().ok() != Some(target.identity.pid) {
        return Err(ProtocolError::new(
            ErrorCode::ApplicationMismatch,
            "the topmost Windows UI element at the coordinate belongs to another process",
            false,
        ));
    }
    let owner = application_root(automation, element)?;
    if owner
        .get_native_window_handle()
        .map_err(|_| accessibility_error())?
        .is_invalid()
    {
        return Err(ProtocolError::new(
            ErrorCode::ApplicationMismatch,
            "the Windows UI element at the coordinate has no native window identity",
            true,
        ));
    }
    if !automation
        .compare_elements(&owner, &target.root)
        .map_err(|_| accessibility_error())?
    {
        return Err(ProtocolError::new(
            ErrorCode::ApplicationMismatch,
            "the topmost Windows UI element at the coordinate belongs to another window",
            false,
        ));
    }
    Ok(())
}

fn require_pointer_location(enigo: &Enigo, point: UiPoint) -> Result<UiPoint, ProtocolError> {
    let (actual_x, actual_y) = enigo.location().map_err(|_| input_permission_error())?;
    checked_pointer_location(actual_x, actual_y, point)
}

fn checked_pointer_location(
    actual_x: i32,
    actual_y: i32,
    point: UiPoint,
) -> Result<UiPoint, ProtocolError> {
    if actual_x.abs_diff(point.get_x()) > POINTER_TOLERANCE_PIXELS as u32
        || actual_y.abs_diff(point.get_y()) > POINTER_TOLERANCE_PIXELS as u32
    {
        return Err(ProtocolError::new(
            ErrorCode::CommandFailed,
            "Windows pointer did not reach the requested coordinate",
            false,
        )
        .with_unknown_outcome());
    }
    Ok(UiPoint::new(actual_x, actual_y))
}

fn current_authorized_pointer(
    enigo: &Enigo,
    automation: &UIAutomation,
    target: &ResolvedTarget,
    policy: &Policy,
    deadline: tokio::time::Instant,
) -> Result<UiPoint, ProtocolError> {
    let (x, y) = enigo.location().map_err(|_| input_setup_error())?;
    authorize_pointer_point(
        enigo,
        automation,
        target,
        policy,
        UiPoint::new(x, y),
        deadline,
    )
}

fn authorize_pointer_point(
    enigo: &Enigo,
    automation: &UIAutomation,
    target: &ResolvedTarget,
    policy: &Policy,
    point: UiPoint,
    deadline: tokio::time::Instant,
) -> Result<UiPoint, ProtocolError> {
    let protocol_point = Point {
        x: f64::from(point.get_x()),
        y: f64::from(point.get_y()),
    };
    policy.validate_coordinate(protocol_point)?;
    let checked = checked_primary_display_point(enigo, protocol_point)?;
    if checked.get_x() != point.get_x() || checked.get_y() != point.get_y() {
        return Err(ProtocolError::new(
            ErrorCode::ApplicationMismatch,
            "Windows pointer coordinates changed during validation",
            true,
        ));
    }
    require_point_owner(automation, target, point, deadline)?;
    Ok(point)
}

fn checked_primary_display_point(enigo: &Enigo, point: Point) -> Result<UiPoint, ProtocolError> {
    let (width, height) = enigo.main_display().map_err(|_| input_setup_error())?;
    positive_display_size(width, height).ok_or_else(input_setup_error)?;
    let x = rounded_coordinate(point.x)?;
    let y = rounded_coordinate(point.y)?;
    if x < 0 || y < 0 || x >= width || y >= height {
        return Err(ProtocolError::new(
            ErrorCode::InvalidCoordinate,
            "Windows input coordinates must be inside the primary display",
            false,
        ));
    }
    Ok(UiPoint::new(x, y))
}

fn rounded_coordinate(value: f64) -> Result<i32, ProtocolError> {
    if !value.is_finite() || value < f64::from(i32::MIN) || value > f64::from(i32::MAX) {
        return Err(ProtocolError::new(
            ErrorCode::InvalidCoordinate,
            "Windows input coordinate is outside the supported pixel range",
            false,
        ));
    }
    Ok(value.round() as i32)
}

fn positive_display_size(width: i32, height: i32) -> Option<(u64, u64)> {
    if width <= 0 || height <= 0 {
        return None;
    }
    Some((width as u64, height as u64))
}

fn enigo_button(button: MouseButton) -> EnigoButton {
    match button {
        MouseButton::Left => EnigoButton::Left,
        MouseButton::Right => EnigoButton::Right,
        MouseButton::Middle => EnigoButton::Middle,
    }
}

fn enigo_modifier(modifier: KeyModifier) -> Result<EnigoKey, ProtocolError> {
    match modifier {
        KeyModifier::Command => Ok(EnigoKey::Meta),
        KeyModifier::Control => Ok(EnigoKey::Control),
        KeyModifier::Option => Ok(EnigoKey::Alt),
        KeyModifier::Shift => Ok(EnigoKey::Shift),
        KeyModifier::Function => Err(ProtocolError::new(
            ErrorCode::EventCreationFailed,
            "the Function modifier has no Windows virtual-key representation",
            false,
        )),
    }
}

fn enigo_key(key: Key) -> EnigoKey {
    match key {
        Key::A => EnigoKey::A,
        Key::B => EnigoKey::B,
        Key::C => EnigoKey::C,
        Key::D => EnigoKey::D,
        Key::E => EnigoKey::E,
        Key::F => EnigoKey::F,
        Key::G => EnigoKey::G,
        Key::H => EnigoKey::H,
        Key::I => EnigoKey::I,
        Key::J => EnigoKey::J,
        Key::K => EnigoKey::K,
        Key::L => EnigoKey::L,
        Key::M => EnigoKey::M,
        Key::N => EnigoKey::N,
        Key::O => EnigoKey::O,
        Key::P => EnigoKey::P,
        Key::Q => EnigoKey::Q,
        Key::R => EnigoKey::R,
        Key::S => EnigoKey::S,
        Key::T => EnigoKey::T,
        Key::U => EnigoKey::U,
        Key::V => EnigoKey::V,
        Key::W => EnigoKey::W,
        Key::X => EnigoKey::X,
        Key::Y => EnigoKey::Y,
        Key::Z => EnigoKey::Z,
        Key::Digit0 => EnigoKey::Num0,
        Key::Digit1 => EnigoKey::Num1,
        Key::Digit2 => EnigoKey::Num2,
        Key::Digit3 => EnigoKey::Num3,
        Key::Digit4 => EnigoKey::Num4,
        Key::Digit5 => EnigoKey::Num5,
        Key::Digit6 => EnigoKey::Num6,
        Key::Digit7 => EnigoKey::Num7,
        Key::Digit8 => EnigoKey::Num8,
        Key::Digit9 => EnigoKey::Num9,
        Key::Enter => EnigoKey::Return,
        Key::Tab => EnigoKey::Tab,
        Key::Space => EnigoKey::Space,
        Key::Backspace => EnigoKey::Backspace,
        Key::Delete => EnigoKey::Delete,
        Key::Escape => EnigoKey::Escape,
        Key::Home => EnigoKey::Home,
        Key::End => EnigoKey::End,
        Key::PageUp => EnigoKey::PageUp,
        Key::PageDown => EnigoKey::PageDown,
        Key::LeftArrow => EnigoKey::LeftArrow,
        Key::RightArrow => EnigoKey::RightArrow,
        Key::UpArrow => EnigoKey::UpArrow,
        Key::DownArrow => EnigoKey::DownArrow,
        Key::F1 => EnigoKey::F1,
        Key::F2 => EnigoKey::F2,
        Key::F3 => EnigoKey::F3,
        Key::F4 => EnigoKey::F4,
        Key::F5 => EnigoKey::F5,
        Key::F6 => EnigoKey::F6,
        Key::F7 => EnigoKey::F7,
        Key::F8 => EnigoKey::F8,
        Key::F9 => EnigoKey::F9,
        Key::F10 => EnigoKey::F10,
        Key::F11 => EnigoKey::F11,
        Key::F12 => EnigoKey::F12,
    }
}

struct MouseReleaseGuard<'a> {
    enigo: &'a mut Enigo,
    button: EnigoButton,
    armed: bool,
}

impl<'a> MouseReleaseGuard<'a> {
    fn new(enigo: &'a mut Enigo, button: EnigoButton) -> Self {
        Self {
            enigo,
            button,
            armed: false,
        }
    }

    fn press(&mut self) -> Result<(), ProtocolError> {
        self.enigo
            .button(self.button, Direction::Press)
            .map_err(|_| input_permission_error())?;
        self.armed = true;
        Ok(())
    }

    fn release(&mut self) -> Result<(), ProtocolError> {
        self.enigo
            .button(self.button, Direction::Release)
            .map_err(|_| input_permission_error())?;
        self.armed = false;
        Ok(())
    }
}

impl Drop for MouseReleaseGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.enigo.button(self.button, Direction::Release);
        }
    }
}

struct KeyReleaseGuard<'a> {
    enigo: &'a mut Enigo,
    held: Vec<EnigoKey>,
}

impl<'a> KeyReleaseGuard<'a> {
    fn new(enigo: &'a mut Enigo) -> Self {
        Self {
            enigo,
            held: Vec::new(),
        }
    }

    fn press(&mut self, key: EnigoKey) -> Result<(), ProtocolError> {
        self.enigo
            .key(key, Direction::Press)
            .map_err(|_| input_permission_error())?;
        self.held.push(key);
        Ok(())
    }

    fn release_one(&mut self, key: EnigoKey) -> Result<(), ProtocolError> {
        self.enigo
            .key(key, Direction::Release)
            .map_err(|_| input_permission_error())?;
        if let Some(index) = self.held.iter().rposition(|held| *held == key) {
            self.held.remove(index);
        }
        Ok(())
    }

    fn has_held(&self) -> bool {
        !self.held.is_empty()
    }

    fn release_last(&mut self) -> Result<(), ProtocolError> {
        let Some(key) = self.held.last().copied() else {
            return Ok(());
        };
        self.enigo
            .key(key, Direction::Release)
            .map_err(|_| input_permission_error())?;
        self.held.pop();
        Ok(())
    }
}

impl Drop for KeyReleaseGuard<'_> {
    fn drop(&mut self) {
        while let Some(key) = self.held.pop() {
            let _ = self.enigo.key(key, Direction::Release);
        }
    }
}

fn exact_application_string(value: String) -> Result<String, ProtocolError> {
    if value.is_empty()
        || value.trim() != value
        || value.chars().count() > MAX_AX_STRING_CHARS
        || value
            .chars()
            .any(zeroclaw_api::tool::is_unsafe_confirmation_character)
    {
        return Err(ProtocolError::new(
            ErrorCode::ApplicationNotFound,
            "Windows application identity is empty or outside protocol bounds",
            true,
        ));
    }
    Ok(value)
}

fn exact_ui_selector(value: String) -> Option<String> {
    (!value.is_empty()
        && value.trim() == value
        && value.chars().count() <= MAX_AX_STRING_CHARS
        && !value
            .chars()
            .any(zeroclaw_api::tool::is_unsafe_confirmation_character))
    .then_some(value)
}

fn optional_ui_string(value: Option<String>) -> Option<String> {
    let value = value?;
    let mut output = String::new();
    for character in value.chars().take(MAX_AX_STRING_CHARS) {
        output.push(
            if zeroclaw_api::tool::is_unsafe_confirmation_character(character) {
                ' '
            } else {
                character
            },
        );
    }
    let trimmed = output.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn check_deadline(
    deadline: tokio::time::Instant,
    operation: OperationKind,
) -> Result<(), ProtocolError> {
    if tokio::time::Instant::now() >= deadline {
        Err(operation.timeout_error())
    } else {
        Ok(())
    }
}

fn check_effect(
    deadline: tokio::time::Instant,
    operation: OperationKind,
    cancelled: &AtomicBool,
) -> Result<(), ProtocolError> {
    if cancelled.load(Ordering::Acquire) {
        Err(operation.timeout_error())
    } else {
        check_deadline(deadline, operation)
    }
}

fn accessibility_error() -> ProtocolError {
    ProtocolError::new(
        ErrorCode::AccessibilityUnavailable,
        "Windows UI Automation is unavailable",
        true,
    )
}

fn stale_element_error() -> ProtocolError {
    ProtocolError::new(
        ErrorCode::ApplicationMismatch,
        "Windows UI Automation element changed before invocation",
        true,
    )
}

fn input_setup_error() -> ProtocolError {
    ProtocolError::new(
        ErrorCode::EventCreationFailed,
        "Windows input subsystem is unavailable",
        true,
    )
}

fn windows_text_unsupported_error() -> ProtocolError {
    ProtocolError::new(
        ErrorCode::EventCreationFailed,
        "Windows text input is unavailable because safe Unicode release tracking is unsupported",
        false,
    )
}

fn input_permission_error() -> ProtocolError {
    ProtocolError::new(
        ErrorCode::PermissionDenied,
        "Windows rejected input; UIPI or desktop integrity policy may have blocked it",
        false,
    )
    .with_unknown_outcome()
}

fn screen_capture_error() -> ProtocolError {
    ProtocolError::new(
        ErrorCode::ScreenCaptureUnavailable,
        "Windows main-display capture is unavailable",
        true,
    )
}

fn io_error(message: &'static str) -> ProtocolError {
    ProtocolError::new(ErrorCode::Io, message, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(allowed_applications: &[&str]) -> Policy {
        Policy {
            application_access: ComputerUseApplicationAccess::Allowlist,
            allowed_applications: allowed_applications
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
            min_coordinate_x: None,
            min_coordinate_y: None,
            max_coordinate_x: None,
            max_coordinate_y: None,
            max_text_chars: 1_000,
        }
    }

    #[test]
    fn windows_selectors_preserve_protocol_namespaces() {
        let identity = ApplicationIdentity {
            name: "editor.example".to_owned(),
            bundle_id: None,
            pid: 42,
        };
        assert!(!windows_policy_allows(
            &policy(&["editor.example"]),
            &identity
        ));
        let display_identity = ApplicationIdentity {
            name: "Editor".to_owned(),
            bundle_id: None,
            pid: 42,
        };
        assert!(windows_policy_allows(
            &policy(&["Editor"]),
            &display_identity
        ));
        assert!(!windows_policy_allows(
            &policy(&["editor"]),
            &display_identity
        ));
        let image_identity = ApplicationIdentity {
            name: "editor".to_owned(),
            bundle_id: Some("editor.exe".to_owned()),
            pid: 42,
        };
        assert!(windows_policy_allows(
            &policy(&["editor.exe"]),
            &image_identity
        ));
    }

    #[test]
    fn accessibility_node_ids_are_one_based() {
        assert_eq!(node_id_for_index(0).expect("first node"), 1);
        assert_eq!(node_id_for_index(41).expect("later node"), 42);
    }

    #[test]
    fn application_names_reject_surrounding_whitespace() {
        assert_eq!(
            exact_application_string(" Editor ".to_owned())
                .expect_err("untrimmed identity must fail")
                .code,
            ErrorCode::ApplicationNotFound
        );
    }

    #[test]
    fn ui_strings_are_bounded_and_strip_control_characters() {
        assert_eq!(
            optional_ui_string(Some("  hello\nworld  ".to_owned())).as_deref(),
            Some("hello world")
        );
        assert!(exact_ui_selector("hello\nworld".to_owned()).is_none());
        let long = "x".repeat(MAX_AX_STRING_CHARS + 1);
        assert!(exact_ui_selector(long.clone()).is_none());
        assert_eq!(
            optional_ui_string(Some(long))
                .expect("bounded UI string")
                .chars()
                .count(),
            MAX_AX_STRING_CHARS
        );
    }

    #[test]
    fn coordinate_rounding_rejects_non_finite_values() {
        assert_eq!(rounded_coordinate(12.6).expect("coordinate"), 13);
        assert_eq!(
            rounded_coordinate(f64::NAN)
                .expect_err("NaN must fail")
                .code,
            ErrorCode::InvalidCoordinate
        );
    }

    #[test]
    fn pointer_verification_returns_the_actual_location() {
        let actual =
            checked_pointer_location(101, 99, UiPoint::new(100, 100)).expect("one-pixel tolerance");
        assert_eq!(actual.get_x(), 101);
        assert_eq!(actual.get_y(), 99);
        assert_eq!(
            checked_pointer_location(102, 100, UiPoint::new(100, 100))
                .expect_err("pointer outside tolerance")
                .code,
            ErrorCode::CommandFailed
        );
    }

    #[test]
    fn identity_scan_only_skips_non_application_candidates() {
        let absent = ProtocolError::new(ErrorCode::ApplicationNotFound, "candidate absent", true);
        assert!(
            optional_application_identity(Err(absent))
                .expect("non-application candidate is skippable")
                .is_none()
        );
        let provider_error =
            ProtocolError::new(ErrorCode::AccessibilityUnavailable, "provider failed", true);
        let Err(error) = optional_application_identity(Err(provider_error)) else {
            panic!("provider failures must abort the scan");
        };
        assert_eq!(error.code, ErrorCode::AccessibilityUnavailable);
    }

    #[test]
    fn worker_lease_remains_exclusive_until_drop() {
        let first = WindowsWorkerLease::acquire().expect("first worker lease");
        assert_eq!(
            WindowsWorkerLease::acquire()
                .expect_err("overlapping worker must fail")
                .code,
            ErrorCode::CommandFailed
        );
        drop(first);
        drop(WindowsWorkerLease::acquire().expect("lease released with worker"));
    }

    #[test]
    fn every_protocol_key_has_a_windows_mapping() {
        for key in Key::ALL {
            let _ = enigo_key(*key);
        }
    }

    #[test]
    fn function_modifier_fails_before_input() {
        let error =
            enigo_modifier(KeyModifier::Function).expect_err("Windows has no Function virtual key");
        assert_eq!(error.code, ErrorCode::EventCreationFailed);
        assert!(!error.outcome_unknown);
    }

    #[test]
    fn windows_text_input_fails_closed_before_input() {
        assert!(
            !windows_actions()
                .iter()
                .any(|action| matches!(action, ActionKind::TypeText))
        );
        let error = windows_text_unsupported_error();
        assert_eq!(error.code, ErrorCode::EventCreationFailed);
        assert!(!error.outcome_unknown);
    }

    #[test]
    fn mutation_timeouts_are_unknown_and_not_retryable() {
        let error = OperationKind::Mutation.timeout_error();
        assert_eq!(error.code, ErrorCode::Timeout);
        assert!(error.outcome_unknown);
        assert!(!error.retryable);
    }

    #[test]
    fn control_type_role_is_stable_debug_name() {
        assert_eq!(format!("{:?}", ControlType::Button), "Button");
    }

    #[test]
    #[ignore = "requires a live Windows session"]
    fn live_windows_capabilities_are_read_only() {
        let response = capabilities().expect("capabilities response");
        let ResponseData::Capabilities {
            platform,
            actions,
            permissions: _,
        } = response
        else {
            panic!("unexpected capabilities response");
        };
        assert!(matches!(platform, Platform::Windows));
        assert_eq!(actions.len(), ActionKind::ALL.len() - 1);
        assert!(
            !actions
                .iter()
                .any(|action| matches!(action, ActionKind::TypeText))
        );
    }

    #[test]
    #[ignore = "requires ZEROCLAW_WINDOWS_TEST_APPLICATION focused in a live Windows session"]
    fn live_windows_exact_selector_lists_and_inspects() {
        let selector = std::env::var("ZEROCLAW_WINDOWS_TEST_APPLICATION")
            .expect("set an exact Windows process name such as notepad.exe");
        let policy = policy(&[selector.as_str()]);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let ResponseData::Applications { applications, .. } =
            list_applications(&policy, deadline).expect("list applications")
        else {
            panic!("unexpected list response");
        };
        assert!(
            applications
                .iter()
                .any(|identity| identity.matches(&selector)),
            "exact selector was not listed"
        );

        let ResponseData::Inspect { snapshot } =
            inspect(&policy, &selector, 64, 8, deadline).expect("inspect focused application")
        else {
            panic!("unexpected inspect response");
        };
        assert!(snapshot.application.matches(&selector));
        assert!(!snapshot.nodes.is_empty());
    }
}
