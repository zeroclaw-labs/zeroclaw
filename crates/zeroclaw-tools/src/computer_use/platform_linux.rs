//! Linux computer-use backend.
//!
//! X11 requests use EWMH for window identity, XTEST for input, and a root
//! `GetImage` for capture.  Accessibility queries use AT-SPI and correlate the
//! application's accessibility-bus owner PID with the operating-system PID.
//! No desktop, identity, policy, or accessibility state is cached across calls.

use std::collections::{HashSet, VecDeque};
use std::future::Future;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::Duration;

use super::*;
use crate::computer_use::protocol::{
    AccessibilityNode, AccessibilitySnapshot, ActionKind, ApplicationIdentity,
    ApplicationSelectorKind, DEFAULT_AX_DEPTH, DEFAULT_AX_NODES, ElementSummary, Key, KeyModifier,
    MAX_AX_DEPTH, MAX_AX_NODES, MAX_AX_STRING_CHARS, MAX_RESPONSE_BYTES, MAX_RUNNING_APPLICATIONS,
    MAX_SCREENSHOT_BYTES, MouseButton, PermissionState, Permissions, Platform, Point, Policy, Rect,
    application_selector_kind,
};
use async_trait::async_trait;
use atspi::proxy::accessible::ObjectRefExt;
use atspi::proxy::proxy_ext::ProxyExt;
use atspi::{AccessibilityConnection, CoordType, ObjectRefOwned, Role, State};
use x11rb::CURRENT_TIME;
use x11rb::connection::Connection;
use x11rb::protocol::res::{ClientIdMask, ClientIdSpec, ConnectionExt as ResConnectionExt};
use x11rb::protocol::xproto::{
    Atom, AtomEnum, BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, ButtonIndex, ClientMessageEvent,
    ConnectionExt as XprotoConnectionExt, EventMask, GetPropertyReply, ImageFormat, ImageOrder,
    KEY_PRESS_EVENT, KEY_RELEASE_EVENT, Keycode, Keysym, MOTION_NOTIFY_EVENT, Window,
};
use x11rb::protocol::xtest::ConnectionExt as XtestConnectionExt;
use x11rb::rust_connection::RustConnection;

const AX_CALL_TIMEOUT: Duration = Duration::from_secs(5);
const FOCUS_POLL_INTERVAL: Duration = Duration::from_millis(50);
const MAX_X11_PROPERTY_ITEMS: u32 = 4_096;
const MAX_X11_SCROLL_STEPS: u32 = 100;
const MAX_NODE_ACTIONS: usize = 32;
const MAX_ACTIVE_PROBE_CHILDREN: usize = 64;
const MAX_X11_WINDOW_DEPTH: usize = 64;
const RESPONSE_HEADROOM: usize = 64 * 1024;

static LINUX_DESKTOP_WORKER_ACTIVE: AtomicBool = AtomicBool::new(false);

struct LinuxDesktopWorkerLease;

impl LinuxDesktopWorkerLease {
    fn acquire() -> Result<Self, ProtocolError> {
        LINUX_DESKTOP_WORKER_ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                ProtocolError::new(
                    ErrorCode::CommandFailed,
                    "another Linux desktop worker is still active",
                    true,
                )
            })?;
        Ok(Self)
    }
}

impl Drop for LinuxDesktopWorkerLease {
    fn drop(&mut self) {
        LINUX_DESKTOP_WORKER_ACTIVE.store(false, Ordering::Release);
    }
}

pub(super) struct PlatformBackend;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DesktopSession {
    X11,
    Wayland,
    Unavailable,
}

#[derive(Clone)]
struct BlockingCancellation {
    state: Arc<AtomicU8>,
}

const EVENT_IDLE: u8 = 0;
const EVENT_IN_PROGRESS: u8 = 1;
const EVENT_CANCELLED: u8 = 2;
const EVENT_CANCELLED_IN_PROGRESS: u8 = 3;

struct InputEventPermit {
    state: Arc<AtomicU8>,
}

impl Drop for InputEventPermit {
    fn drop(&mut self) {
        let _ = self.state.compare_exchange(
            EVENT_IN_PROGRESS,
            EVENT_IDLE,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        let _ = self.state.compare_exchange(
            EVENT_CANCELLED_IN_PROGRESS,
            EVENT_CANCELLED,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
}

impl BlockingCancellation {
    fn new() -> Self {
        Self {
            state: Arc::new(AtomicU8::new(EVENT_IDLE)),
        }
    }

    fn cancel(&self) {
        loop {
            let current = self.state.load(Ordering::Acquire);
            let next = match current {
                EVENT_IDLE => EVENT_CANCELLED,
                EVENT_IN_PROGRESS => EVENT_CANCELLED_IN_PROGRESS,
                EVENT_CANCELLED | EVENT_CANCELLED_IN_PROGRESS => return,
                _ => EVENT_CANCELLED,
            };
            if self
                .state
                .compare_exchange(current, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return;
            }
        }
    }

    fn check(&self) -> Result<(), ProtocolError> {
        if matches!(
            self.state.load(Ordering::Acquire),
            EVENT_CANCELLED | EVENT_CANCELLED_IN_PROGRESS
        ) {
            Err(ProtocolError::new(
                ErrorCode::Timeout,
                "computer-use request deadline expired before the Linux desktop event boundary",
                false,
            ))
        } else {
            Ok(())
        }
    }

    fn begin_event(&self) -> Result<InputEventPermit, ProtocolError> {
        self.state
            .compare_exchange(
                EVENT_IDLE,
                EVENT_IN_PROGRESS,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|_| {
                ProtocolError::new(
                    ErrorCode::Timeout,
                    "computer-use request was cancelled before the Linux desktop event boundary",
                    false,
                )
            })?;
        Ok(InputEventPermit {
            state: Arc::clone(&self.state),
        })
    }
}

#[derive(Clone, Copy)]
enum BlockingOperation {
    Read,
    Mutation,
}

async fn run_x11_blocking<T, F>(
    deadline: tokio::time::Instant,
    operation: BlockingOperation,
    task: F,
) -> Result<T, ProtocolError>
where
    T: Send + 'static,
    F: FnOnce(BlockingCancellation) -> Result<T, ProtocolError> + Send + 'static,
{
    check_deadline(deadline)?;
    let lease = LinuxDesktopWorkerLease::acquire()?;
    let cancellation = BlockingCancellation::new();
    let task_cancellation = cancellation.clone();
    let handle = tokio::task::spawn_blocking(move || {
        let _lease = lease;
        task(task_cancellation)
    });
    match tokio::time::timeout_at(deadline, handle).await {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => {
            let error = ProtocolError::new(
                ErrorCode::CommandFailed,
                format!(
                    "Linux X11 worker failed: {}",
                    sanitized_external(&error.to_string())
                ),
                false,
            );
            Err(mark_blocking_operation(error, operation))
        }
        Err(_) => {
            cancellation.cancel();
            let error = ProtocolError::new(
                ErrorCode::Timeout,
                "Linux X11 operation exceeded the computer-use deadline",
                false,
            );
            Err(mark_blocking_operation(error, operation))
        }
    }
}

fn mark_blocking_operation(error: ProtocolError, operation: BlockingOperation) -> ProtocolError {
    match operation {
        BlockingOperation::Read => error,
        BlockingOperation::Mutation => error.with_unknown_outcome(),
    }
}

#[async_trait]
impl Backend for PlatformBackend {
    async fn execute(
        &self,
        action: &Action,
        policy: &Policy,
        screenshot_reservation: Option<&ScreenshotReservation>,
        deadline: tokio::time::Instant,
    ) -> Result<ResponseData, ProtocolError> {
        if LINUX_DESKTOP_WORKER_ACTIVE.load(Ordering::Acquire) {
            return Err(ProtocolError::new(
                ErrorCode::CommandFailed,
                "a prior Linux desktop worker is still active",
                true,
            ));
        }
        if matches!(action, Action::Capabilities {}) {
            return capabilities(deadline).await;
        }

        match action {
            Action::Capabilities {} => capabilities(deadline).await,
            Action::ListApplications {} => list_applications(policy, deadline).await,
            Action::Inspect {
                expected_application,
                max_nodes,
                max_depth,
            } => {
                inspect(
                    policy,
                    expected_application,
                    max_nodes.unwrap_or(DEFAULT_AX_NODES),
                    max_depth.unwrap_or(DEFAULT_AX_DEPTH),
                    deadline,
                )
                .await
            }
            Action::Screenshot { application, .. } => {
                let reservation = screenshot_reservation.ok_or_else(|| {
                    ProtocolError::new(
                        ErrorCode::InvalidPath,
                        "Linux screenshot request has no held destination",
                        false,
                    )
                })?;
                screenshot(policy, application, reservation, deadline).await
            }
            Action::Focus { application } => focus(policy, application, deadline).await,
            Action::MouseMove {
                x,
                y,
                expected_application,
            } => {
                mouse_move(
                    policy,
                    expected_application,
                    Point { x: *x, y: *y },
                    deadline,
                )
                .await
            }
            Action::Click {
                x,
                y,
                button,
                expected_application,
            } => {
                click(
                    policy,
                    expected_application,
                    Point { x: *x, y: *y },
                    *button,
                    deadline,
                )
                .await
            }
            Action::Scroll {
                delta_x,
                delta_y,
                expected_application,
            } => scroll(policy, expected_application, *delta_x, *delta_y, deadline).await,
            Action::TypeText {
                text,
                expected_application,
            } => type_text(policy, expected_application, text, deadline).await,
            Action::KeyPress {
                key,
                modifiers,
                expected_application,
            } => key_press(policy, expected_application, *key, modifiers, deadline).await,
            Action::PressElement {
                application,
                role,
                title,
            } => press_element(policy, application, role.as_deref(), title, deadline).await,
        }
    }
}

fn desktop_session() -> DesktopSession {
    desktop_session_from(
        std::env::var_os("XDG_SESSION_TYPE").as_deref(),
        std::env::var_os("DISPLAY").as_deref(),
        std::env::var_os("WAYLAND_DISPLAY").as_deref(),
    )
}

fn desktop_session_from(
    session_type: Option<&std::ffi::OsStr>,
    display: Option<&std::ffi::OsStr>,
    wayland_display: Option<&std::ffi::OsStr>,
) -> DesktopSession {
    let nonempty = |value: Option<&std::ffi::OsStr>| value.is_some_and(|value| !value.is_empty());
    let display = nonempty(display);
    let wayland_display = nonempty(wayland_display);
    match session_type.and_then(std::ffi::OsStr::to_str) {
        Some(value) if value.eq_ignore_ascii_case("x11") => {
            if display {
                DesktopSession::X11
            } else {
                DesktopSession::Unavailable
            }
        }
        Some(value) if value.eq_ignore_ascii_case("wayland") => {
            if wayland_display {
                DesktopSession::Wayland
            } else {
                DesktopSession::Unavailable
            }
        }
        Some(_) => DesktopSession::Unavailable,
        None if display && !wayland_display => DesktopSession::X11,
        None if wayland_display && !display => DesktopSession::Wayland,
        None => DesktopSession::Unavailable,
    }
}

fn require_x11(cancellation: BlockingCancellation) -> Result<X11Context, ProtocolError> {
    match desktop_session() {
        DesktopSession::X11 => X11Context::connect(cancellation),
        DesktopSession::Wayland => Err(ProtocolError::new(
            ErrorCode::UnsupportedPlatform,
            "this Linux action requires an X11 session; Wayland does not expose authoritative global window ownership",
            false,
        )),
        DesktopSession::Unavailable => Err(ProtocolError::new(
            ErrorCode::UnsupportedPlatform,
            "no unambiguous graphical Linux desktop session is available",
            true,
        )),
    }
}

async fn capabilities(deadline: tokio::time::Instant) -> Result<ResponseData, ProtocolError> {
    let accessibility = match await_ax(
        deadline,
        "query Linux accessibility status",
        atspi::connection::read_session_accessibility(),
    )
    .await
    {
        Ok(true) => PermissionState::Granted,
        Ok(false) => PermissionState::Denied,
        Err(_) => PermissionState::Unknown,
    };

    let mut actions = vec![ActionKind::Capabilities];
    let screen_recording = match desktop_session() {
        DesktopSession::X11 => {
            match run_x11_blocking(deadline, BlockingOperation::Read, |cancellation| {
                X11Context::connect(cancellation).map(|context| context.xtest_available)
            })
            .await
            {
                Ok(xtest_available) => {
                    actions.extend([
                        ActionKind::ListApplications,
                        ActionKind::Screenshot,
                        ActionKind::Focus,
                        ActionKind::MouseMove,
                        ActionKind::Click,
                        ActionKind::Scroll,
                        ActionKind::TypeText,
                        ActionKind::KeyPress,
                    ]);
                    if xtest_available && accessibility == PermissionState::Granted {
                        actions.extend([ActionKind::Inspect, ActionKind::PressElement]);
                    } else if accessibility == PermissionState::Granted {
                        actions.extend([ActionKind::Inspect, ActionKind::PressElement]);
                        actions.retain(|action| {
                            !matches!(
                                action,
                                ActionKind::MouseMove
                                    | ActionKind::Click
                                    | ActionKind::Scroll
                                    | ActionKind::TypeText
                                    | ActionKind::KeyPress
                            )
                        });
                    } else if !xtest_available {
                        actions.retain(|action| {
                            !matches!(
                                action,
                                ActionKind::MouseMove
                                    | ActionKind::Click
                                    | ActionKind::Scroll
                                    | ActionKind::TypeText
                                    | ActionKind::KeyPress
                            )
                        });
                    }
                    // X11 has no permission preflight that does not itself capture
                    // pixels. A live connection proves availability, not capture
                    // authorization in a nested compositor or policy layer.
                    PermissionState::Unknown
                }
                Err(_) => PermissionState::Denied,
            }
        }
        DesktopSession::Wayland => {
            if accessibility == PermissionState::Granted {
                actions.extend([
                    ActionKind::ListApplications,
                    ActionKind::Inspect,
                    ActionKind::PressElement,
                ]);
            }
            PermissionState::Denied
        }
        DesktopSession::Unavailable => PermissionState::Denied,
    };
    actions.sort_by_key(|action| action.as_str());
    actions.dedup();

    Ok(ResponseData::Capabilities {
        platform: Platform::Linux,
        actions,
        permissions: Permissions {
            accessibility,
            screen_recording,
        },
    })
}

async fn list_applications(
    policy: &Policy,
    deadline: tokio::time::Instant,
) -> Result<ResponseData, ProtocolError> {
    match desktop_session() {
        DesktopSession::X11 => {
            let policy = policy.clone();
            let (applications, truncated) =
                run_x11_blocking(deadline, BlockingOperation::Read, move |cancellation| {
                    let context = require_x11(cancellation)?;
                    context.list_applications(&policy, deadline)
                })
                .await?;
            Ok(ResponseData::Applications {
                applications,
                truncated,
            })
        }
        DesktopSession::Wayland => {
            let context = AxContext::connect(deadline).await?;
            let (applications, truncated) = context.list_applications(policy, deadline).await?;
            Ok(ResponseData::Applications {
                applications,
                truncated,
            })
        }
        DesktopSession::Unavailable => Err(ProtocolError::new(
            ErrorCode::UnsupportedPlatform,
            "no unambiguous graphical Linux desktop session is available",
            true,
        )),
    }
}

async fn inspect(
    policy: &Policy,
    expected_application: &str,
    max_nodes: u32,
    max_depth: u32,
    deadline: tokio::time::Instant,
) -> Result<ResponseData, ProtocolError> {
    let ax = AxContext::connect(deadline).await?;
    let application =
        resolve_active_application(policy, expected_application, &ax, deadline).await?;
    let application_reference = application.reference.clone();
    let snapshot = ax
        .snapshot(application, max_nodes, max_depth, deadline)
        .await?;
    let current = resolve_active_application(policy, expected_application, &ax, deadline).await?;
    if current.identity != snapshot.application || current.reference != application_reference {
        return Err(ProtocolError::new(
            ErrorCode::ApplicationMismatch,
            "active Linux application changed during accessibility inspection",
            true,
        ));
    }
    ax.verify_application_pid(&current, deadline).await?;
    Ok(ResponseData::Inspect { snapshot })
}

async fn press_element(
    policy: &Policy,
    selector: &str,
    role: Option<&str>,
    title: &str,
    deadline: tokio::time::Instant,
) -> Result<ResponseData, ProtocolError> {
    let ax = AxContext::connect(deadline).await?;
    let first_application = resolve_active_application(policy, selector, &ax, deadline).await?;
    let first_match = ax
        .unique_element(&first_application, role, title, deadline)
        .await?;

    // Re-resolve both the application and the element immediately before the
    // AT-SPI action. Accessible object paths below the application root are
    // explicitly ephemeral in AT-SPI.
    let current_application = resolve_active_application(policy, selector, &ax, deadline).await?;
    if current_application.identity != first_application.identity
        || current_application.reference != first_application.reference
    {
        return Err(ProtocolError::new(
            ErrorCode::ApplicationMismatch,
            "active Linux application changed before the accessibility action",
            true,
        ));
    }
    let current_match = ax
        .unique_element(&current_application, role, title, deadline)
        .await?;
    if current_match.summary != first_match.summary
        || current_match.reference != first_match.reference
    {
        return Err(ProtocolError::new(
            ErrorCode::ApplicationMismatch,
            "Linux accessibility element changed before the action",
            true,
        ));
    }
    ax.verify_application_pid(&current_application, deadline)
        .await?;
    {
        let proxy = await_ax(
            deadline,
            "resolve Linux accessibility element",
            current_match
                .reference
                .as_accessible_proxy(ax.connection.connection()),
        )
        .await?;
        let state = await_ax(
            deadline,
            "revalidate Linux accessibility element state",
            proxy.get_state(),
        )
        .await?;
        if !state.contains(State::Enabled) || !state.contains(State::Sensitive) {
            return Err(ProtocolError::new(
                ErrorCode::ElementNotFound,
                "Linux accessibility element is not enabled and sensitive",
                true,
            ));
        }
        let live_role = await_ax(
            deadline,
            "revalidate Linux accessibility element role",
            proxy.get_role(),
        )
        .await?;
        let live_title = await_ax(
            deadline,
            "revalidate Linux accessibility element name",
            proxy.name(),
        )
        .await?;
        if live_role == Role::PasswordText
            || live_role.name() != current_match.summary.role
            || exact_ax_selector_value(live_title.as_str()) != Some(title)
            || role.is_some_and(|expected| expected != live_role.name())
        {
            return Err(ProtocolError::new(
                ErrorCode::ApplicationMismatch,
                "Linux accessibility element identity changed before the action",
                true,
            ));
        }
        let proxies = await_ax(
            deadline,
            "revalidate Linux accessibility interfaces",
            proxy.proxies(),
        )
        .await?;
        let action = await_ax(
            deadline,
            "resolve Linux accessibility action",
            proxies.action(),
        )
        .await?;
        let actions = await_ax(
            deadline,
            "revalidate Linux accessibility actions",
            action.get_actions(),
        )
        .await?;
        if actions.is_empty() {
            return Err(ProtocolError::new(
                ErrorCode::ElementNotFound,
                "Linux accessibility element exposes no action",
                true,
            ));
        }
    }

    let final_application = resolve_active_application(policy, selector, &ax, deadline).await?;
    if final_application.identity != current_application.identity
        || final_application.reference != current_application.reference
    {
        return Err(ProtocolError::new(
            ErrorCode::ApplicationMismatch,
            "active Linux application changed immediately before the accessibility action",
            true,
        ));
    }
    ax.verify_application_pid(&final_application, deadline)
        .await?;

    let element_reference = current_match.reference.clone();
    let expected_role = current_match.summary.role.clone();
    let expected_title = title.to_owned();
    let connection = ax.connection;
    let worker_application = current_application.clone();
    run_ax_action_until_resolved(
        connection,
        element_reference,
        expected_role,
        expected_title,
        worker_application,
        policy.clone(),
        selector.to_owned(),
        deadline,
    )
    .await?;

    Ok(ResponseData::ElementPressed {
        application: current_application.identity,
        element: current_match.summary,
    })
}

async fn run_ax_action_until_resolved(
    connection: AccessibilityConnection,
    reference: ObjectRefOwned,
    expected_role: String,
    expected_title: String,
    expected_application: AxApplication,
    policy: Policy,
    selector: String,
    deadline: tokio::time::Instant,
) -> Result<(), ProtocolError> {
    check_deadline(deadline)?;
    let lease = LinuxDesktopWorkerLease::acquire()?;
    let cancellation = BlockingCancellation::new();
    let task_cancellation = cancellation.clone();
    let handle =
        zeroclaw_spawn::spawn!(async move {
            let _lease = lease;
            let ax = AxContext { connection };
            let proxy = reference
                .as_accessible_proxy(ax.connection.connection())
                .await
                .map_err(|error| ax_worker_error("resolve Linux accessibility element", error))?;
            let state = proxy.get_state().await.map_err(|error| {
                ax_worker_error("revalidate Linux accessibility element state", error)
            })?;
            if !state.contains(State::Enabled) || !state.contains(State::Sensitive) {
                return Err(ProtocolError::new(
                    ErrorCode::ElementNotFound,
                    "Linux accessibility element is not enabled and sensitive",
                    true,
                ));
            }
            let role = proxy.get_role().await.map_err(|error| {
                ax_worker_error("revalidate Linux accessibility element role", error)
            })?;
            let title = proxy.name().await.map_err(|error| {
                ax_worker_error("revalidate Linux accessibility element name", error)
            })?;
            if role == Role::PasswordText
                || role.name() != expected_role
                || exact_ax_selector_value(title.as_str()) != Some(expected_title.as_str())
            {
                return Err(ProtocolError::new(
                    ErrorCode::ApplicationMismatch,
                    "Linux accessibility element identity changed at the action boundary",
                    true,
                ));
            }
            let proxies = proxy.proxies().await.map_err(|error| {
                ax_worker_error("revalidate Linux accessibility interfaces", error)
            })?;
            let action = proxies
                .action()
                .await
                .map_err(|error| ax_worker_error("resolve Linux accessibility action", error))?;
            let actions = action.get_actions().await.map_err(|error| {
                ax_worker_error("revalidate Linux accessibility actions", error)
            })?;
            if actions.is_empty() {
                return Err(ProtocolError::new(
                    ErrorCode::ElementNotFound,
                    "Linux accessibility element exposes no action",
                    true,
                ));
            }

            revalidate_action_application(
                &ax,
                &expected_application,
                &policy,
                &selector,
                deadline,
                task_cancellation.clone(),
            )
            .await?;

            // AT-SPI specifies action zero as the default when an object exposes
            // one or more actions. This future is intentionally not cancelled on
            // the caller deadline: its occupancy lease survives until D-Bus
            // reports the actual mutation outcome.
            check_deadline(deadline)?;
            let _event_permit = task_cancellation.begin_event()?;
            let invoked = action.do_action(0).await.map_err(|error| {
                ax_worker_error("invoke Linux accessibility action", error).with_unknown_outcome()
            })?;
            if !invoked {
                return Err(ProtocolError::new(
                    ErrorCode::EventCreationFailed,
                    "Linux accessibility action reported failure",
                    false,
                )
                .with_unknown_outcome());
            }
            Ok(())
        });

    match tokio::time::timeout_at(deadline, handle).await {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => Err(ProtocolError::new(
            ErrorCode::CommandFailed,
            format!(
                "Linux accessibility action worker failed: {}",
                sanitized_external(&error.to_string())
            ),
            false,
        )
        .with_unknown_outcome()),
        Err(_) => {
            cancellation.cancel();
            Err(ProtocolError::new(
                ErrorCode::Timeout,
                "Linux accessibility action exceeded the deadline and remains isolated until its outcome resolves",
                false,
            )
            .with_unknown_outcome())
        }
    }
}

fn ax_worker_error(label: &str, error: impl std::fmt::Display) -> ProtocolError {
    ProtocolError::new(
        ErrorCode::AccessibilityUnavailable,
        format!("{label} failed: {}", sanitized_external(&error.to_string())),
        true,
    )
}

async fn revalidate_action_application(
    ax: &AxContext,
    expected: &AxApplication,
    policy: &Policy,
    selector: &str,
    deadline: tokio::time::Instant,
    cancellation: BlockingCancellation,
) -> Result<(), ProtocolError> {
    match desktop_session() {
        DesktopSession::X11 => {
            ax.verify_application_pid(expected, deadline).await?;
            let policy = policy.clone();
            let selector = selector.to_owned();
            let handle = tokio::task::spawn_blocking(move || {
                let x11 = require_x11(cancellation)?;
                x11.active_identity(&policy, Some(&selector), deadline)
            });
            let identity = handle.await.map_err(|error| {
                ProtocolError::new(
                    ErrorCode::CommandFailed,
                    format!(
                        "Linux X11 identity worker failed: {}",
                        sanitized_external(&error.to_string())
                    ),
                    false,
                )
            })??;
            if identity != expected.identity {
                return Err(ProtocolError::new(
                    ErrorCode::ApplicationMismatch,
                    "active X11 application changed at the accessibility action boundary",
                    true,
                ));
            }
        }
        DesktopSession::Wayland => {
            let current = ax.active_application(deadline).await?;
            validate_application(&current.identity, policy, Some(selector))?;
            if current.identity != expected.identity || current.reference != expected.reference {
                return Err(ProtocolError::new(
                    ErrorCode::ApplicationMismatch,
                    "active Wayland application changed at the accessibility action boundary",
                    true,
                ));
            }
            ax.verify_application_pid(&current, deadline).await?;
            if ax
                .reference_is_active(&expected.reference, deadline)
                .await?
                != Some(true)
            {
                return Err(ProtocolError::new(
                    ErrorCode::ApplicationMismatch,
                    "expected Wayland application is no longer authoritatively active",
                    true,
                ));
            }
        }
        DesktopSession::Unavailable => {
            return Err(ProtocolError::new(
                ErrorCode::UnsupportedPlatform,
                "Linux desktop session changed before the accessibility action",
                true,
            ));
        }
    }
    check_deadline(deadline)
}

async fn resolve_active_application(
    policy: &Policy,
    expected: &str,
    ax: &AxContext,
    deadline: tokio::time::Instant,
) -> Result<AxApplication, ProtocolError> {
    match desktop_session() {
        DesktopSession::X11 => {
            let policy = policy.clone();
            let expected = expected.to_owned();
            let identity =
                run_x11_blocking(deadline, BlockingOperation::Read, move |cancellation| {
                    let x11 = require_x11(cancellation)?;
                    x11.active_identity(&policy, Some(&expected), deadline)
                })
                .await?;
            let application = ax.application_for_pid(identity.pid, deadline).await?;
            ax.verify_application_pid(&application, deadline).await?;
            Ok(AxApplication {
                reference: application.reference,
                identity,
            })
        }
        DesktopSession::Wayland => {
            let application = ax.active_application(deadline).await?;
            validate_application(&application.identity, policy, Some(expected))?;
            ax.verify_application_pid(&application, deadline).await?;
            Ok(application)
        }
        DesktopSession::Unavailable => Err(ProtocolError::new(
            ErrorCode::UnsupportedPlatform,
            "no unambiguous graphical Linux desktop session is available",
            true,
        )),
    }
}

struct AxContext {
    connection: AccessibilityConnection,
}

#[derive(Clone)]
struct AxApplication {
    reference: ObjectRefOwned,
    identity: ApplicationIdentity,
}

struct AxElementMatch {
    reference: ObjectRefOwned,
    summary: ElementSummary,
}

impl AxContext {
    async fn connect(deadline: tokio::time::Instant) -> Result<Self, ProtocolError> {
        let connection = await_ax(
            deadline,
            "connect to the Linux accessibility bus",
            AccessibilityConnection::new(),
        )
        .await?;
        Ok(Self { connection })
    }

    async fn root_children(
        &self,
        maximum: usize,
        deadline: tokio::time::Instant,
    ) -> Result<(Vec<ObjectRefOwned>, bool), ProtocolError> {
        let root = await_ax(
            deadline,
            "resolve Linux accessibility registry",
            self.connection.root_accessible_on_registry(),
        )
        .await?;
        let child_count = await_ax(
            deadline,
            "count Linux accessibility applications",
            root.child_count(),
        )
        .await?;
        if child_count < 0 {
            return Err(protocol_violation(
                "Linux accessibility registry returned a negative child count",
            ));
        }
        let total = usize::try_from(child_count).map_err(|_| {
            protocol_violation("Linux accessibility registry child count overflowed")
        })?;
        let count = total.min(maximum);
        let mut children = Vec::with_capacity(count);
        for index in 0..count {
            let index = i32::try_from(index).map_err(|_| {
                protocol_violation("Linux accessibility application index overflowed")
            })?;
            let child = await_ax(
                deadline,
                "read Linux accessibility application",
                root.get_child_at_index(index),
            )
            .await?;
            if !child.is_null() {
                children.push(child);
            }
        }
        Ok((children, total > count))
    }

    async fn identity_for_reference(
        &self,
        reference: &ObjectRefOwned,
        deadline: tokio::time::Instant,
    ) -> Result<ApplicationIdentity, ProtocolError> {
        let pid = self.pid_for_reference(reference, deadline).await?;
        let proxy = await_ax(
            deadline,
            "resolve Linux accessibility application",
            reference.as_accessible_proxy(self.connection.connection()),
        )
        .await?;
        let process_name = process_name(pid)?;
        let attributes = optional_ax(
            deadline,
            "read Linux accessibility application attributes",
            proxy.get_attributes(),
        )
        .await?;
        let reported_bundle_id = attributes.as_ref().and_then(application_id_from_attributes);
        let identity = linux_process_identity(pid, process_name, reported_bundle_id)?;
        validate_identity_shape(&identity)?;
        Ok(identity)
    }

    async fn pid_for_reference(
        &self,
        reference: &ObjectRefOwned,
        deadline: tokio::time::Instant,
    ) -> Result<u32, ProtocolError> {
        let name = reference.name().cloned().ok_or_else(|| {
            protocol_violation("Linux accessibility object has no unique bus owner")
        })?;
        let dbus = await_ax(
            deadline,
            "connect to Linux accessibility DBus",
            atspi::zbus::fdo::DBusProxy::new(self.connection.connection()),
        )
        .await?;
        let pid = await_ax(
            deadline,
            "resolve Linux accessibility application PID",
            dbus.get_connection_unix_process_id(name.into()),
        )
        .await?;
        if pid == 0 || !std::path::Path::new("/proc").join(pid.to_string()).is_dir() {
            return Err(ProtocolError::new(
                ErrorCode::ApplicationNotFound,
                "Linux accessibility application PID is no longer live",
                true,
            ));
        }
        Ok(pid)
    }

    async fn verify_application_pid(
        &self,
        application: &AxApplication,
        deadline: tokio::time::Instant,
    ) -> Result<(), ProtocolError> {
        let current = self
            .pid_for_reference(&application.reference, deadline)
            .await?;
        if current != application.identity.pid {
            return Err(ProtocolError::new(
                ErrorCode::ApplicationMismatch,
                "Linux accessibility bus owner changed before the action",
                true,
            ));
        }
        Ok(())
    }

    async fn application_for_pid(
        &self,
        expected_pid: u32,
        deadline: tokio::time::Instant,
    ) -> Result<AxApplication, ProtocolError> {
        let (children, truncated) = self
            .root_children(MAX_RUNNING_APPLICATIONS, deadline)
            .await?;
        let mut match_ref = None;
        for child in children {
            if self.pid_for_reference(&child, deadline).await? == expected_pid {
                if match_ref.is_some() {
                    return Err(protocol_violation(
                        "multiple Linux accessibility applications share one PID",
                    ));
                }
                match_ref = Some(child);
            }
        }
        if truncated {
            return Err(ProtocolError::new(
                ErrorCode::OutputTooLarge,
                "Linux accessibility application list was truncated before unique PID correlation",
                false,
            ));
        }
        let reference = match_ref.ok_or_else(|| {
            ProtocolError::new(
                ErrorCode::AccessibilityUnavailable,
                "frontmost Linux application is not exposed on the accessibility bus",
                true,
            )
        })?;
        let identity = self.identity_for_reference(&reference, deadline).await?;
        Ok(AxApplication {
            reference,
            identity,
        })
    }

    async fn list_applications(
        &self,
        policy: &Policy,
        deadline: tokio::time::Instant,
    ) -> Result<(Vec<ApplicationIdentity>, bool), ProtocolError> {
        let (children, mut truncated) = self
            .root_children(MAX_RUNNING_APPLICATIONS, deadline)
            .await?;
        let mut applications = Vec::new();
        let mut seen = HashSet::new();
        for child in children {
            let identity = match self.identity_for_reference(&child, deadline).await {
                Ok(identity) => identity,
                Err(error) if error.code == ErrorCode::ApplicationNotFound => continue,
                Err(error) => return Err(error),
            };
            if policy.allows(&identity) && seen.insert(identity.pid) {
                if applications.len() == MAX_RUNNING_APPLICATIONS {
                    truncated = true;
                    break;
                }
                applications.push(identity);
            }
        }
        applications.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then_with(|| left.pid.cmp(&right.pid))
        });
        Ok((applications, truncated))
    }

    async fn active_application(
        &self,
        deadline: tokio::time::Instant,
    ) -> Result<AxApplication, ProtocolError> {
        let (children, truncated) = self
            .root_children(MAX_RUNNING_APPLICATIONS, deadline)
            .await?;
        let mut active = None;
        for child in children {
            let Some(is_active) = self.reference_is_active(&child, deadline).await? else {
                return Err(ProtocolError::new(
                    ErrorCode::OutputTooLarge,
                    "Linux accessibility active-window probe was truncated",
                    false,
                ));
            };
            if is_active {
                if active.is_some() {
                    return Err(ProtocolError::new(
                        ErrorCode::ApplicationMismatch,
                        "Linux accessibility bus reports multiple active applications",
                        true,
                    ));
                }
                let identity = self.identity_for_reference(&child, deadline).await?;
                active = Some(AxApplication {
                    reference: child,
                    identity,
                });
            }
        }
        if truncated {
            return Err(ProtocolError::new(
                ErrorCode::OutputTooLarge,
                "Linux accessibility application list was truncated before unique active-app resolution",
                false,
            ));
        }
        active.ok_or_else(|| {
            ProtocolError::new(
                ErrorCode::ApplicationNotFound,
                "Linux accessibility bus did not report exactly one active application",
                true,
            )
        })
    }

    async fn reference_is_active(
        &self,
        reference: &ObjectRefOwned,
        deadline: tokio::time::Instant,
    ) -> Result<Option<bool>, ProtocolError> {
        let proxy = await_ax(
            deadline,
            "resolve Linux accessibility application",
            reference.as_accessible_proxy(self.connection.connection()),
        )
        .await?;
        let states = await_ax(
            deadline,
            "read Linux accessibility application state",
            proxy.get_state(),
        )
        .await?;
        if states.contains(State::Active) {
            return Ok(Some(true));
        }
        let count = await_ax(
            deadline,
            "count Linux accessibility top-level windows",
            proxy.child_count(),
        )
        .await?;
        if count < 0 {
            return Err(protocol_violation(
                "Linux accessibility application returned a negative child count",
            ));
        }
        let total = usize::try_from(count).map_err(|_| {
            protocol_violation("Linux accessibility top-level window count overflowed")
        })?;
        let take = total.min(MAX_ACTIVE_PROBE_CHILDREN);
        for index in 0..take {
            let index = i32::try_from(index).map_err(|_| {
                protocol_violation("Linux accessibility top-level window index overflowed")
            })?;
            let child = await_ax(
                deadline,
                "read Linux accessibility top-level window",
                proxy.get_child_at_index(index),
            )
            .await?;
            if child.is_null() {
                continue;
            }
            let child_proxy = await_ax(
                deadline,
                "resolve Linux accessibility top-level window",
                child.as_accessible_proxy(self.connection.connection()),
            )
            .await?;
            let states = await_ax(
                deadline,
                "read Linux accessibility top-level window state",
                child_proxy.get_state(),
            )
            .await?;
            if states.contains(State::Active) {
                return Ok(Some(true));
            }
        }
        if take < total {
            Ok(None)
        } else {
            Ok(Some(false))
        }
    }

    async fn snapshot(
        &self,
        application: AxApplication,
        max_nodes: u32,
        max_depth: u32,
        deadline: tokio::time::Instant,
    ) -> Result<AccessibilitySnapshot, ProtocolError> {
        let mut queue = VecDeque::new();
        queue.push_back((application.reference.clone(), None, 0_u32));
        let mut nodes = Vec::new();
        let mut truncated = false;
        let mut encoded_budget = 0_usize;

        while let Some((reference, parent_id, depth)) = queue.pop_front() {
            check_deadline(deadline)?;
            if nodes.len() >= max_nodes as usize {
                truncated = true;
                break;
            }
            let proxy = await_ax(
                deadline,
                "resolve Linux accessibility node",
                reference.as_accessible_proxy(self.connection.connection()),
            )
            .await?;
            let node_id = u32::try_from(nodes.len())
                .ok()
                .and_then(|value| value.checked_add(1))
                .ok_or_else(|| protocol_violation("Linux accessibility node id overflowed"))?;
            let node = self
                .read_node(&proxy, node_id, parent_id, depth, deadline)
                .await?;
            let node_size = serde_json::to_vec(&node)
                .map_err(|error| {
                    protocol_violation(format!("could not size Linux accessibility node: {error}"))
                })?
                .len();
            if encoded_budget.saturating_add(node_size)
                > MAX_RESPONSE_BYTES.saturating_sub(RESPONSE_HEADROOM)
            {
                truncated = true;
                break;
            }
            encoded_budget = encoded_budget.saturating_add(node_size);
            nodes.push(node);

            let child_count = await_ax(
                deadline,
                "count Linux accessibility children",
                proxy.child_count(),
            )
            .await?;
            if child_count < 0 {
                return Err(protocol_violation(
                    "Linux accessibility node returned a negative child count",
                ));
            }
            if depth >= max_depth {
                if child_count > 0 {
                    truncated = true;
                }
                continue;
            }
            let remaining = (max_nodes as usize)
                .saturating_sub(nodes.len())
                .saturating_sub(queue.len());
            let total = usize::try_from(child_count)
                .map_err(|_| protocol_violation("Linux accessibility child count overflowed"))?;
            let take = total.min(remaining);
            if take < total {
                truncated = true;
            }
            for index in 0..take {
                let index = i32::try_from(index).map_err(|_| {
                    protocol_violation("Linux accessibility child index overflowed")
                })?;
                let child = await_ax(
                    deadline,
                    "read Linux accessibility child",
                    proxy.get_child_at_index(index),
                )
                .await?;
                if !child.is_null() {
                    queue.push_back((child, Some(node_id), depth.saturating_add(1)));
                }
            }
        }
        if nodes.is_empty() {
            return Err(ProtocolError::new(
                ErrorCode::AccessibilityUnavailable,
                "Linux accessibility application exposed no readable root node",
                true,
            ));
        }
        Ok(AccessibilitySnapshot {
            application: application.identity,
            nodes,
            truncated,
            max_nodes,
            max_depth,
        })
    }

    async fn read_node(
        &self,
        proxy: &atspi::proxy::accessible::AccessibleProxy<'_>,
        id: u32,
        parent_id: Option<u32>,
        depth: u32,
        deadline: tokio::time::Instant,
    ) -> Result<AccessibilityNode, ProtocolError> {
        let role = await_ax(deadline, "read Linux accessibility role", proxy.get_role()).await?;
        let secure = role == Role::PasswordText;
        let state = optional_ax(
            deadline,
            "read Linux accessibility state",
            proxy.get_state(),
        )
        .await?;
        let title = if secure {
            None
        } else {
            optional_ax(deadline, "read Linux accessibility name", proxy.name())
                .await?
                .map(sanitize_ax_string)
                .filter(|value| !value.is_empty())
        };
        let description = if secure {
            None
        } else {
            optional_ax(
                deadline,
                "read Linux accessibility description",
                proxy.description(),
            )
            .await?
            .map(sanitize_ax_string)
            .filter(|value| !value.is_empty())
        };
        let proxies = optional_ax(
            deadline,
            "read Linux accessibility interfaces",
            proxy.proxies(),
        )
        .await?;
        let mut actions = Vec::new();
        let mut value = None;
        let mut bounds = None;
        if let Some(proxies) = &proxies {
            if let Some(action) = optional_ax(
                deadline,
                "resolve Linux accessibility action interface",
                proxies.action(),
            )
            .await?
                && let Some(reported) = optional_ax(
                    deadline,
                    "read Linux accessibility actions",
                    action.get_actions(),
                )
                .await?
            {
                for item in reported.into_iter().take(MAX_NODE_ACTIONS) {
                    let name = sanitize_ax_string(item.name);
                    if !name.is_empty() {
                        actions.push(name);
                    }
                }
            }
            if !secure
                && let Some(text) = optional_ax(
                    deadline,
                    "resolve Linux accessibility text interface",
                    proxies.text(),
                )
                .await?
                && let Some(count) = optional_ax(
                    deadline,
                    "read Linux accessibility text length",
                    text.character_count(),
                )
                .await?
                && count > 0
            {
                let bounded = count.min(MAX_AX_STRING_CHARS as i32);
                value = optional_ax(
                    deadline,
                    "read Linux accessibility text",
                    text.get_text(0, bounded),
                )
                .await?
                .map(sanitize_ax_string)
                .filter(|value| !value.is_empty());
            }
            if let Some(component) = optional_ax(
                deadline,
                "resolve Linux accessibility component interface",
                proxies.component(),
            )
            .await?
                && let Some((x, y, width, height)) = optional_ax(
                    deadline,
                    "read Linux accessibility bounds",
                    component.get_extents(CoordType::Screen),
                )
                .await?
                && width >= 0
                && height >= 0
            {
                bounds = Some(Rect {
                    x: f64::from(x),
                    y: f64::from(y),
                    width: f64::from(width),
                    height: f64::from(height),
                });
            }
        }
        Ok(AccessibilityNode {
            id,
            parent_id,
            depth,
            role: role.name().to_owned(),
            title,
            value,
            description,
            enabled: state.map(|state| state.contains(State::Enabled)),
            focused: state.map(|state| state.contains(State::Focused)),
            bounds,
            actions,
        })
    }

    async fn unique_element(
        &self,
        application: &AxApplication,
        expected_role: Option<&str>,
        expected_title: &str,
        deadline: tokio::time::Instant,
    ) -> Result<AxElementMatch, ProtocolError> {
        let mut queue = VecDeque::new();
        queue.push_back((application.reference.clone(), 0_u32));
        let mut visited = 0_u32;
        let mut matched = None;
        let mut truncated = false;
        while let Some((reference, depth)) = queue.pop_front() {
            check_deadline(deadline)?;
            if visited >= MAX_AX_NODES {
                truncated = true;
                break;
            }
            visited = visited.saturating_add(1);
            let proxy = await_ax(
                deadline,
                "resolve Linux accessibility element",
                reference.as_accessible_proxy(self.connection.connection()),
            )
            .await?;
            let role = await_ax(
                deadline,
                "read Linux accessibility element role",
                proxy.get_role(),
            )
            .await?;
            let secure = role == Role::PasswordText;
            let title = if secure {
                None
            } else {
                optional_ax(
                    deadline,
                    "read Linux accessibility element name",
                    proxy.name(),
                )
                .await?
                .and_then(|value| exact_ax_selector_value(&value).map(str::to_owned))
            };
            let role_name = role.name().to_owned();
            if !secure
                && title.as_deref() == Some(expected_title)
                && expected_role.is_none_or(|expected| expected == role_name)
            {
                let summary = ElementSummary {
                    role: role_name.clone(),
                    title: title.clone(),
                    bounds: element_bounds(&proxy, deadline).await?,
                };
                if matched.is_some() {
                    return Err(ProtocolError::new(
                        ErrorCode::AmbiguousElement,
                        "multiple Linux accessibility elements match the selector",
                        false,
                    ));
                }
                matched = Some(AxElementMatch {
                    reference: reference.clone(),
                    summary,
                });
            }
            let count = await_ax(
                deadline,
                "count Linux accessibility element children",
                proxy.child_count(),
            )
            .await?;
            if count < 0 {
                return Err(protocol_violation(
                    "Linux accessibility element returned a negative child count",
                ));
            }
            if depth >= MAX_AX_DEPTH {
                if count > 0 {
                    truncated = true;
                }
                continue;
            }
            let remaining = (MAX_AX_NODES as usize)
                .saturating_sub(visited as usize)
                .saturating_sub(queue.len());
            let count = usize::try_from(count).map_err(|_| {
                protocol_violation("Linux accessibility element child count overflowed")
            })?;
            let take = count.min(remaining);
            if take < count {
                truncated = true;
            }
            for index in 0..take {
                let index = i32::try_from(index).map_err(|_| {
                    protocol_violation("Linux accessibility element child index overflowed")
                })?;
                let child = await_ax(
                    deadline,
                    "read Linux accessibility element child",
                    proxy.get_child_at_index(index),
                )
                .await?;
                if !child.is_null() {
                    queue.push_back((child, depth.saturating_add(1)));
                }
            }
        }
        if truncated {
            return Err(ProtocolError::new(
                ErrorCode::OutputTooLarge,
                "Linux accessibility search was truncated before uniqueness could be proven",
                false,
            ));
        }
        matched.ok_or_else(|| {
            ProtocolError::new(
                ErrorCode::ElementNotFound,
                "no Linux accessibility element matches the selector",
                false,
            )
        })
    }
}

async fn element_bounds(
    proxy: &atspi::proxy::accessible::AccessibleProxy<'_>,
    deadline: tokio::time::Instant,
) -> Result<Option<Rect>, ProtocolError> {
    let Some(proxies) = optional_ax(
        deadline,
        "read Linux accessibility element interfaces",
        proxy.proxies(),
    )
    .await?
    else {
        return Ok(None);
    };
    let Some(component) = optional_ax(
        deadline,
        "resolve Linux accessibility element component",
        proxies.component(),
    )
    .await?
    else {
        return Ok(None);
    };
    let Some((x, y, width, height)) = optional_ax(
        deadline,
        "read Linux accessibility element bounds",
        component.get_extents(CoordType::Screen),
    )
    .await?
    else {
        return Ok(None);
    };
    if width < 0 || height < 0 {
        return Ok(None);
    }
    Ok(Some(Rect {
        x: f64::from(x),
        y: f64::from(y),
        width: f64::from(width),
        height: f64::from(height),
    }))
}

async fn await_ax<T, E, F>(
    deadline: tokio::time::Instant,
    label: &str,
    future: F,
) -> Result<T, ProtocolError>
where
    F: Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    let timeout = remaining(deadline, AX_CALL_TIMEOUT)?;
    match tokio::time::timeout(timeout, future).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(ProtocolError::new(
            ErrorCode::AccessibilityUnavailable,
            format!("{label} failed: {}", sanitized_external(&error.to_string())),
            true,
        )),
        Err(_) => Err(ProtocolError::new(
            ErrorCode::Timeout,
            format!("{label} exceeded the computer-use deadline"),
            false,
        )),
    }
}

async fn optional_ax<T, E, F>(
    deadline: tokio::time::Instant,
    label: &str,
    future: F,
) -> Result<Option<T>, ProtocolError>
where
    F: Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    match await_ax(deadline, label, future).await {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.code == ErrorCode::AccessibilityUnavailable => Ok(None),
        Err(error) => Err(error),
    }
}

fn application_id_from_attributes(
    attributes: &std::collections::HashMap<String, String>,
) -> Option<String> {
    ["application-id", "app-id", "desktop-file-id"]
        .iter()
        .filter_map(|key| attributes.get(*key))
        .find(|value| {
            application_selector_kind(value) == ApplicationSelectorKind::BundleIdentifier
                && valid_external_string(value)
        })
        .cloned()
}

struct X11Context {
    connection: RustConnection,
    screen_number: usize,
    atoms: X11Atoms,
    xtest_available: bool,
    cancellation: BlockingCancellation,
}

struct X11Atoms {
    active_window: Atom,
    client_list_stacking: Atom,
    wm_pid: Atom,
    utf8_string: Atom,
    gtk_application_id: Atom,
    wm_desktop_file: Atom,
}

impl X11Context {
    fn connect(cancellation: BlockingCancellation) -> Result<Self, ProtocolError> {
        cancellation.check()?;
        let (connection, screen_number) =
            x11rb::connect(None).map_err(|error| x11_error("connect to X11", error))?;
        if connection.setup().roots.get(screen_number).is_none() {
            return Err(protocol_violation("X11 selected an invalid screen"));
        }
        let res_version = connection
            .res_query_version(1, 2)
            .map_err(|error| x11_error("query X-Resource extension", error))?
            .reply()
            .map_err(|error| x11_error("read X-Resource extension version", error))?;
        if !xres_supports_local_pid(res_version.server_major, res_version.server_minor) {
            return Err(ProtocolError::new(
                ErrorCode::PermissionDenied,
                "X11 server cannot prove local window-owner process IDs",
                false,
            ));
        }
        let atoms = X11Atoms::new(&connection)?;
        let xtest_available = connection
            .xtest_get_version(2, 2)
            .is_ok_and(|cookie| cookie.reply().is_ok());
        Ok(Self {
            connection,
            screen_number,
            atoms,
            xtest_available,
            cancellation,
        })
    }

    fn screen(&self) -> Result<&x11rb::protocol::xproto::Screen, ProtocolError> {
        self.connection
            .setup()
            .roots
            .get(self.screen_number)
            .ok_or_else(|| protocol_violation("X11 selected screen disappeared"))
    }

    fn require_xtest(&self) -> Result<(), ProtocolError> {
        if self.xtest_available {
            Ok(())
        } else {
            Err(ProtocolError::new(
                ErrorCode::EventCreationFailed,
                "X11 XTEST extension is unavailable",
                false,
            ))
        }
    }

    fn active_window(&self) -> Result<Window, ProtocolError> {
        let root = self.screen()?.root;
        let values =
            self.property_u32(root, self.atoms.active_window, AtomEnum::WINDOW.into(), 1)?;
        match values.as_slice() {
            [window] if *window != x11rb::NONE => Ok(*window),
            _ => Err(ProtocolError::new(
                ErrorCode::ApplicationNotFound,
                "X11 did not report exactly one active window",
                true,
            )),
        }
    }

    fn active_identity(
        &self,
        policy: &Policy,
        expected: Option<&str>,
        deadline: tokio::time::Instant,
    ) -> Result<ApplicationIdentity, ProtocolError> {
        check_deadline(deadline)?;
        let window = self.active_window()?;
        let identity = self.window_identity(window)?;
        validate_application(&identity, policy, expected)?;
        check_deadline(deadline)?;
        if self.active_window()? != window {
            return Err(ProtocolError::new(
                ErrorCode::ApplicationMismatch,
                "active X11 window changed while its identity was being resolved",
                true,
            ));
        }
        check_deadline(deadline)?;
        Ok(identity)
    }

    fn client_windows(&self) -> Result<Vec<Window>, ProtocolError> {
        let root = self.screen()?.root;
        let windows = self.property_u32(
            root,
            self.atoms.client_list_stacking,
            AtomEnum::WINDOW.into(),
            MAX_X11_PROPERTY_ITEMS,
        )?;
        if windows.is_empty() {
            return Err(ProtocolError::new(
                ErrorCode::ApplicationNotFound,
                "X11 window manager did not report any client windows",
                true,
            ));
        }
        Ok(windows)
    }

    fn window_identity(&self, window: Window) -> Result<ApplicationIdentity, ProtocolError> {
        let pids = self.property_u32(window, self.atoms.wm_pid, AtomEnum::CARDINAL.into(), 1)?;
        let pid = match pids.as_slice() {
            [pid] if *pid != 0 => *pid,
            _ => {
                return Err(ProtocolError::new(
                    ErrorCode::ApplicationNotFound,
                    "X11 window has no unique operating-system PID",
                    true,
                ));
            }
        };
        let server_pid = self.window_server_pid(window)?;
        if server_pid != pid {
            return Err(ProtocolError::new(
                ErrorCode::ApplicationMismatch,
                "X11 window PID property does not match its server-proven local owner",
                true,
            ));
        }
        if !std::path::Path::new("/proc").join(pid.to_string()).is_dir() {
            return Err(ProtocolError::new(
                ErrorCode::ApplicationNotFound,
                "X11 window process is no longer live",
                true,
            ));
        }
        let process_name = process_name(pid)?;
        let gtk_application_id = self
            .property_string(
                window,
                self.atoms.gtk_application_id,
                self.atoms.utf8_string,
            )?
            .filter(|value| {
                application_selector_kind(value) == ApplicationSelectorKind::BundleIdentifier
            });
        let desktop_file_id = self
            .property_string(window, self.atoms.wm_desktop_file, self.atoms.utf8_string)?
            .filter(|value| {
                application_selector_kind(value) == ApplicationSelectorKind::BundleIdentifier
            });
        let reported_bundle_id = gtk_application_id.or(desktop_file_id);
        linux_process_identity(pid, process_name, reported_bundle_id)
    }

    fn window_server_pid(&self, window: Window) -> Result<u32, ProtocolError> {
        let spec = ClientIdSpec {
            client: window,
            mask: ClientIdMask::LOCAL_CLIENT_PID,
        };
        let reply = self
            .connection
            .res_query_client_ids(&[spec])
            .map_err(|error| x11_error("query X11 window owner PID", error))?
            .reply()
            .map_err(|error| x11_error("read X11 window owner PID", error))?;
        match reply.ids.as_slice() {
            [identity]
                if identity.spec.client == window
                    && u32::from(identity.spec.mask)
                        == u32::from(ClientIdMask::LOCAL_CLIENT_PID) =>
            {
                match identity.value.as_slice() {
                    [pid] if *pid != 0 => Ok(*pid),
                    _ => Err(protocol_violation(
                        "X-Resource returned an invalid local window-owner PID",
                    )),
                }
            }
            _ => Err(ProtocolError::new(
                ErrorCode::ApplicationNotFound,
                "X11 server could not prove exactly one local owner for the window",
                true,
            )),
        }
    }

    fn list_applications(
        &self,
        policy: &Policy,
        deadline: tokio::time::Instant,
    ) -> Result<(Vec<ApplicationIdentity>, bool), ProtocolError> {
        let windows = self.client_windows()?;
        let mut applications = Vec::new();
        let mut seen = HashSet::new();
        let mut truncated = false;
        for window in windows.into_iter().rev() {
            check_deadline(deadline)?;
            let Ok(identity) = self.window_identity(window) else {
                continue;
            };
            if policy.allows(&identity) && seen.insert(identity.pid) {
                if applications.len() == MAX_RUNNING_APPLICATIONS {
                    truncated = true;
                    break;
                }
                applications.push(identity);
            }
        }
        applications.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then_with(|| left.pid.cmp(&right.pid))
        });
        Ok((applications, truncated))
    }

    fn property_u32(
        &self,
        window: Window,
        property: Atom,
        expected_type: Atom,
        maximum_items: u32,
    ) -> Result<Vec<u32>, ProtocolError> {
        if property == x11rb::NONE || expected_type == x11rb::NONE {
            return Err(ProtocolError::new(
                ErrorCode::ApplicationNotFound,
                "required X11/EWMH property atom is unavailable",
                true,
            ));
        }
        let reply = self
            .connection
            .get_property(false, window, property, expected_type, 0, maximum_items)
            .map_err(|error| x11_error("request X11 property", error))?
            .reply()
            .map_err(|error| x11_error("read X11 property", error))?;
        if reply.type_ != expected_type || reply.format != 32 || reply.bytes_after != 0 {
            return Err(protocol_violation(
                "X11 property has an unexpected type, format, or unbounded remainder",
            ));
        }
        reply
            .value32()
            .map(Iterator::collect)
            .ok_or_else(|| protocol_violation("X11 property is not a 32-bit value"))
    }

    fn property_string(
        &self,
        window: Window,
        property: Atom,
        expected_type: Atom,
    ) -> Result<Option<String>, ProtocolError> {
        if property == x11rb::NONE || expected_type == x11rb::NONE {
            return Ok(None);
        }
        let reply = self
            .connection
            .get_property(
                false,
                window,
                property,
                expected_type,
                0,
                (MAX_AX_STRING_CHARS as u32).saturating_add(1),
            )
            .map_err(|error| x11_error("request X11 string property", error))?
            .reply()
            .map_err(|error| x11_error("read X11 string property", error))?;
        if reply.type_ == u32::from(AtomEnum::NONE) {
            return Ok(None);
        }
        if reply.type_ != expected_type || reply.format != 8 || reply.bytes_after != 0 {
            return Err(protocol_violation(
                "X11 string property has an unexpected type, format, or size",
            ));
        }
        bounded_property_string(&reply).map(Some)
    }

    fn focus_window(
        &self,
        window: Window,
        deadline: tokio::time::Instant,
    ) -> Result<(), ProtocolError> {
        check_deadline(deadline)?;
        if self.atoms.active_window == x11rb::NONE {
            return Err(ProtocolError::new(
                ErrorCode::ApplicationNotFound,
                "X11 window manager does not expose the active-window protocol",
                true,
            ));
        }
        let root = self.screen()?.root;
        let event = ClientMessageEvent::new(
            32,
            window,
            self.atoms.active_window,
            [1_u32, CURRENT_TIME, 0, 0, 0],
        );
        let _event_permit = self.cancellation.begin_event()?;
        self.connection
            .send_event(
                false,
                root,
                EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                event,
            )
            .map_err(|error| x11_error("send X11 focus request", error).with_unknown_outcome())?
            .check()
            .map_err(|error| x11_error("check X11 focus request", error).with_unknown_outcome())?;
        self.connection
            .flush()
            .map_err(|error| x11_error("flush X11 focus request", error).with_unknown_outcome())?;
        Ok(())
    }

    fn window_owner_pid_at_point(&self, point: Point) -> Result<u32, ProtocolError> {
        let x = logical_coordinate(point.x, "x")?;
        let y = logical_coordinate(point.y, "y")?;
        let root = self.screen()?.root;
        let mut window = root;
        for _ in 0..MAX_X11_WINDOW_DEPTH {
            let translated = self
                .connection
                .translate_coordinates(root, window, x, y)
                .map_err(|error| x11_error("query X11 window at coordinate", error))?
                .reply()
                .map_err(|error| x11_error("read X11 window at coordinate", error))?;
            if !translated.same_screen {
                return Err(ProtocolError::new(
                    ErrorCode::ApplicationMismatch,
                    "X11 coordinate is not on the selected screen",
                    true,
                ));
            }
            if translated.child == x11rb::NONE {
                if window == root {
                    return Err(ProtocolError::new(
                        ErrorCode::ApplicationMismatch,
                        "X11 coordinate resolves to the desktop background",
                        true,
                    ));
                }
                return self.window_server_pid(window);
            }
            window = translated.child;
        }
        Err(ProtocolError::new(
            ErrorCode::OutputTooLarge,
            "X11 window hierarchy at the coordinate exceeds the safety depth",
            false,
        ))
    }

    fn verify_coordinate_owner(
        &self,
        policy: &Policy,
        expected: &str,
        point: Point,
        deadline: tokio::time::Instant,
    ) -> Result<ApplicationIdentity, ProtocolError> {
        let initial_active = self.active_identity(policy, Some(expected), deadline)?;
        let owner_pid = self.window_owner_pid_at_point(point)?;
        let final_active = self.active_identity(policy, Some(expected), deadline)?;
        if initial_active != final_active || owner_pid != final_active.pid {
            return Err(ProtocolError::new(
                ErrorCode::ApplicationMismatch,
                "X11 coordinate owner is not the stable active expected application",
                true,
            ));
        }
        check_deadline(deadline)?;
        Ok(final_active)
    }

    fn send_input(&self, event_type: u8, detail: u8, x: i16, y: i16) -> Result<(), ProtocolError> {
        self.require_xtest()?;
        // Releases are cleanup and must remain possible after cancellation so
        // a deadline cannot leave a synthetic key or button held down.
        let root = self.screen()?.root;
        let _event_permit = if matches!(event_type, KEY_RELEASE_EVENT | BUTTON_RELEASE_EVENT) {
            None
        } else {
            Some(self.cancellation.begin_event()?)
        };
        self.connection
            .xtest_fake_input(event_type, detail, CURRENT_TIME, root, x, y, 0)
            .map_err(|error| x11_error("create X11 input event", error).with_unknown_outcome())?
            .check()
            .map_err(|error| x11_error("check X11 input event", error).with_unknown_outcome())?;
        self.connection
            .flush()
            .map_err(|error| x11_error("flush X11 input event", error).with_unknown_outcome())
    }

    fn pointer(&self) -> Result<Point, ProtocolError> {
        self.cancellation.check()?;
        let reply = self
            .connection
            .query_pointer(self.screen()?.root)
            .map_err(|error| x11_error("query X11 pointer", error))?
            .reply()
            .map_err(|error| x11_error("read X11 pointer", error))?;
        if !reply.same_screen {
            return Err(ProtocolError::new(
                ErrorCode::ApplicationMismatch,
                "X11 pointer is not on the selected screen",
                true,
            ));
        }
        Ok(Point {
            x: f64::from(reply.root_x),
            y: f64::from(reply.root_y),
        })
    }

    fn verify_pointer_owner(
        &self,
        policy: &Policy,
        expected_application: &str,
        expected_point: Point,
        deadline: tokio::time::Instant,
    ) -> Result<ApplicationIdentity, ProtocolError> {
        let actual = self.pointer()?;
        if actual != expected_point {
            return Err(ProtocolError::new(
                ErrorCode::ApplicationMismatch,
                "X11 pointer moved away from the authorized coordinate",
                true,
            ));
        }
        self.verify_coordinate_owner(policy, expected_application, actual, deadline)
    }

    fn keyboard_map(&self) -> Result<KeyboardMap, ProtocolError> {
        let setup = self.connection.setup();
        let count = setup
            .max_keycode
            .checked_sub(setup.min_keycode)
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| protocol_violation("X11 keyboard keycode range is invalid"))?;
        let reply = self
            .connection
            .get_keyboard_mapping(setup.min_keycode, count)
            .map_err(|error| x11_error("request X11 keyboard map", error))?
            .reply()
            .map_err(|error| x11_error("read X11 keyboard map", error))?;
        KeyboardMap::new(setup.min_keycode, count, reply)
    }

    fn capture_png(&self) -> Result<(Vec<u8>, Rect, u64, u64), ProtocolError> {
        self.cancellation.check()?;
        let screen = self.screen()?;
        let width = screen.width_in_pixels;
        let height = screen.height_in_pixels;
        if width == 0 || height == 0 {
            return Err(ProtocolError::new(
                ErrorCode::ScreenCaptureUnavailable,
                "X11 selected screen has invalid dimensions",
                true,
            ));
        }
        let format = self
            .connection
            .setup()
            .pixmap_formats
            .iter()
            .find(|format| format.depth == screen.root_depth)
            .copied()
            .ok_or_else(|| {
                ProtocolError::new(
                    ErrorCode::ScreenCaptureUnavailable,
                    "X11 root-screen pixel format is unavailable",
                    false,
                )
            })?;
        // Bound the server reply from request geometry before GetImage can
        // cause x11rb to allocate its reply payload.
        let layout = x11_image_layout(width, height, format.bits_per_pixel, format.scanline_pad)?;
        let reply = self
            .connection
            .get_image(
                ImageFormat::Z_PIXMAP,
                screen.root,
                0,
                0,
                width,
                height,
                u32::MAX,
            )
            .map_err(|error| x11_error("request X11 screen image", error))?
            .reply()
            .map_err(|error| x11_error("read X11 screen image", error))?;
        self.cancellation.check()?;
        if reply.depth != screen.root_depth || reply.data.len() != layout.total_bytes {
            return Err(protocol_violation(
                "X11 screenshot reply does not match the preflighted root-screen format",
            ));
        }
        if reply.visual != screen.root_visual {
            return Err(protocol_violation(
                "X11 screenshot reply visual does not match the root window",
            ));
        }
        let visual = screen
            .allowed_depths
            .iter()
            .flat_map(|depth| depth.visuals.iter())
            .find(|visual| visual.visual_id == screen.root_visual)
            .copied()
            .ok_or_else(|| {
                ProtocolError::new(
                    ErrorCode::ScreenCaptureUnavailable,
                    "X11 screen visual is unavailable",
                    false,
                )
            })?;
        let png = encode_x11_png(
            &reply.data,
            width,
            height,
            layout,
            self.connection.setup().image_byte_order,
            visual.red_mask,
            visual.green_mask,
            visual.blue_mask,
            &self.cancellation,
        )?;
        Ok((
            png,
            Rect {
                x: 0.0,
                y: 0.0,
                width: f64::from(width),
                height: f64::from(height),
            },
            u64::from(width),
            u64::from(height),
        ))
    }
}

impl X11Atoms {
    fn new(connection: &RustConnection) -> Result<Self, ProtocolError> {
        Ok(Self {
            active_window: intern_atom(connection, b"_NET_ACTIVE_WINDOW")?,
            client_list_stacking: intern_atom(connection, b"_NET_CLIENT_LIST_STACKING")?,
            wm_pid: intern_atom(connection, b"_NET_WM_PID")?,
            utf8_string: intern_atom(connection, b"UTF8_STRING")?,
            gtk_application_id: intern_atom(connection, b"_GTK_APPLICATION_ID")?,
            wm_desktop_file: intern_atom(connection, b"_NET_WM_DESKTOP_FILE")?,
        })
    }
}

fn intern_atom(connection: &RustConnection, name: &[u8]) -> Result<Atom, ProtocolError> {
    connection
        .intern_atom(true, name)
        .map_err(|error| x11_error("request X11 atom", error))?
        .reply()
        .map(|reply| reply.atom)
        .map_err(|error| x11_error("read X11 atom", error))
}

fn xres_supports_local_pid(major: u16, minor: u16) -> bool {
    major > 1 || (major == 1 && minor >= 2)
}

async fn focus(
    policy: &Policy,
    selector: &str,
    deadline: tokio::time::Instant,
) -> Result<ResponseData, ProtocolError> {
    let policy = policy.clone();
    let selector = selector.to_owned();
    run_x11_blocking(deadline, BlockingOperation::Mutation, move |cancellation| {
        focus_blocking(&policy, &selector, deadline, cancellation)
    })
    .await
}

fn focus_blocking(
    policy: &Policy,
    selector: &str,
    deadline: tokio::time::Instant,
    cancellation: BlockingCancellation,
) -> Result<ResponseData, ProtocolError> {
    let context = require_x11(cancellation)?;
    let windows = context.client_windows()?;
    let mut target = None;
    let mut matched_pid = None;
    for window in windows.into_iter().rev() {
        check_deadline(deadline)?;
        if let Ok(identity) = context.window_identity(window)
            && identity.matches(selector)
            && policy.allows(&identity)
        {
            if matched_pid.is_some_and(|pid| pid != identity.pid) {
                return Err(ProtocolError::new(
                    ErrorCode::ApplicationMismatch,
                    "multiple X11 processes match the requested application selector",
                    true,
                ));
            }
            matched_pid = Some(identity.pid);
            if target.is_none() {
                target = Some((window, identity));
            }
        }
    }
    let (window, selected_identity) = target.ok_or_else(|| {
        ProtocolError::new(
            ErrorCode::ApplicationNotFound,
            "no X11 application window matches the requested selector",
            true,
        )
    })?;
    let current_identity = context.window_identity(window)?;
    validate_application(&current_identity, policy, Some(selector))?;
    if current_identity != selected_identity {
        return Err(ProtocolError::new(
            ErrorCode::ApplicationMismatch,
            "X11 target application identity changed before focus",
            true,
        ));
    }
    context.focus_window(window, deadline)?;
    loop {
        match context.active_identity(policy, Some(selector), deadline) {
            Ok(application) => return Ok(ResponseData::Focused { application }),
            Err(error)
                if matches!(
                    error.code,
                    ErrorCode::ApplicationMismatch | ErrorCode::ApplicationNotFound
                ) && tokio::time::Instant::now() < deadline =>
            {
                std::thread::sleep(
                    remaining(deadline, FOCUS_POLL_INTERVAL)
                        .map_err(ProtocolError::with_unknown_outcome)?,
                );
            }
            Err(error) => return Err(error.with_unknown_outcome()),
        }
    }
}

async fn screenshot(
    policy: &Policy,
    selector: &str,
    reservation: &ScreenshotReservation,
    deadline: tokio::time::Instant,
) -> Result<ResponseData, ProtocolError> {
    reservation.verify_path_identity().map_err(|error| {
        ProtocolError::new(
            ErrorCode::InvalidPath,
            format!(
                "held Linux screenshot destination is invalid: {}",
                sanitized_external(&format!("{error:#}"))
            ),
            false,
        )
    })?;
    let policy = policy.clone();
    let selector = selector.to_owned();
    let captured = run_x11_blocking(deadline, BlockingOperation::Mutation, move |cancellation| {
        capture_x11_blocking(&policy, &selector, deadline, cancellation)
    })
    .await?;
    check_deadline(deadline).map_err(ProtocolError::with_unknown_outcome)?;
    let expected_size = captured.png.len() as u64;
    let mut source = captured.png.as_slice();
    match tokio::time::timeout_at(
        deadline,
        reservation.replace_from_bounded_reader(&mut source, expected_size, MAX_SCREENSHOT_BYTES),
    )
    .await
    {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            return Err(ProtocolError::new(
                ErrorCode::InvalidPath,
                format!(
                    "could not write held Linux screenshot destination: {}",
                    sanitized_external(&format!("{error:#}"))
                ),
                false,
            )
            .with_unknown_outcome());
        }
        Err(_) => {
            return Err(ProtocolError::new(
                ErrorCode::Timeout,
                "writing the Linux screenshot exceeded the computer-use deadline",
                false,
            )
            .with_unknown_outcome());
        }
    }
    check_deadline(deadline).map_err(ProtocolError::with_unknown_outcome)?;
    Ok(ResponseData::Screenshot {
        path: reservation.path().to_path_buf(),
        display_bounds: captured.display_bounds,
        pixel_width: captured.pixel_width,
        pixel_height: captured.pixel_height,
    })
}

struct CapturedX11Screenshot {
    png: Vec<u8>,
    display_bounds: Rect,
    pixel_width: u64,
    pixel_height: u64,
}

fn capture_x11_blocking(
    policy: &Policy,
    selector: &str,
    deadline: tokio::time::Instant,
    cancellation: BlockingCancellation,
) -> Result<CapturedX11Screenshot, ProtocolError> {
    let focused = match focus_blocking(policy, selector, deadline, cancellation.clone())? {
        ResponseData::Focused { application } => application,
        _ => {
            return Err(protocol_violation(
                "Linux focus returned an invalid payload",
            ));
        }
    };
    let context = require_x11(cancellation)?;
    context
        .active_identity(policy, Some(selector), deadline)
        .map_err(ProtocolError::with_unknown_outcome)?;
    let (png, display_bounds, pixel_width, pixel_height) = context
        .capture_png()
        .map_err(ProtocolError::with_unknown_outcome)?;
    let current = context
        .active_identity(policy, Some(selector), deadline)
        .map_err(ProtocolError::with_unknown_outcome)?;
    if current != focused {
        return Err(ProtocolError::new(
            ErrorCode::ApplicationMismatch,
            "active X11 application changed during screen capture",
            false,
        )
        .with_unknown_outcome());
    }
    Ok(CapturedX11Screenshot {
        png,
        display_bounds,
        pixel_width,
        pixel_height,
    })
}

async fn mouse_move(
    policy: &Policy,
    expected: &str,
    point: Point,
    deadline: tokio::time::Instant,
) -> Result<ResponseData, ProtocolError> {
    let policy = policy.clone();
    let expected = expected.to_owned();
    run_x11_blocking(deadline, BlockingOperation::Mutation, move |cancellation| {
        mouse_move_blocking(&policy, &expected, point, deadline, cancellation)
    })
    .await
}

fn mouse_move_blocking(
    policy: &Policy,
    expected: &str,
    point: Point,
    deadline: tokio::time::Instant,
    cancellation: BlockingCancellation,
) -> Result<ResponseData, ProtocolError> {
    let context = require_x11(cancellation)?;
    context.require_xtest()?;
    let application = context.verify_coordinate_owner(policy, expected, point, deadline)?;
    let x = logical_coordinate(point.x, "x")?;
    let y = logical_coordinate(point.y, "y")?;
    context.send_input(MOTION_NOTIFY_EVENT, 0, x, y)?;
    let actual = context
        .pointer()
        .map_err(ProtocolError::with_unknown_outcome)?;
    if actual.x != f64::from(x) || actual.y != f64::from(y) {
        return Err(ProtocolError::new(
            ErrorCode::ApplicationMismatch,
            "X11 pointer did not reach the authorized coordinate",
            false,
        )
        .with_unknown_outcome());
    }
    let current = context
        .verify_coordinate_owner(policy, expected, actual, deadline)
        .map_err(ProtocolError::with_unknown_outcome)?;
    if current != application {
        return Err(ProtocolError::new(
            ErrorCode::ApplicationMismatch,
            "X11 coordinate owner or active application changed after mouse movement",
            false,
        )
        .with_unknown_outcome());
    }
    Ok(ResponseData::Input {
        application: current,
    })
}

async fn click(
    policy: &Policy,
    expected: &str,
    point: Point,
    button: MouseButton,
    deadline: tokio::time::Instant,
) -> Result<ResponseData, ProtocolError> {
    let policy = policy.clone();
    let expected = expected.to_owned();
    run_x11_blocking(deadline, BlockingOperation::Mutation, move |cancellation| {
        click_blocking(&policy, &expected, point, button, deadline, cancellation)
    })
    .await
}

fn click_blocking(
    policy: &Policy,
    expected: &str,
    point: Point,
    button: MouseButton,
    deadline: tokio::time::Instant,
    cancellation: BlockingCancellation,
) -> Result<ResponseData, ProtocolError> {
    let context = require_x11(cancellation)?;
    context.require_xtest()?;
    let application = context.verify_coordinate_owner(policy, expected, point, deadline)?;
    let x = logical_coordinate(point.x, "x")?;
    let y = logical_coordinate(point.y, "y")?;
    let event_point = Point {
        x: f64::from(x),
        y: f64::from(y),
    };
    context.send_input(MOTION_NOTIFY_EVENT, 0, x, y)?;
    context
        .verify_pointer_owner(policy, expected, event_point, deadline)
        .map_err(ProtocolError::with_unknown_outcome)?;
    let detail = mouse_button_detail(button);
    if let Err(error) = context.send_input(BUTTON_PRESS_EVENT, detail, 0, 0) {
        if error.outcome_unknown {
            let _ = context.send_input(BUTTON_RELEASE_EVENT, detail, 0, 0);
        }
        return Err(error.with_unknown_outcome());
    }
    let before_release = context
        .verify_pointer_owner(policy, expected, event_point, deadline)
        .map(|_| ());
    if let Err(error) = before_release {
        let _ = context.send_input(BUTTON_RELEASE_EVENT, detail, 0, 0);
        return Err(error.with_unknown_outcome());
    }
    if let Err(error) = context.send_input(BUTTON_RELEASE_EVENT, detail, 0, 0) {
        let _ = context.send_input(BUTTON_RELEASE_EVENT, detail, 0, 0);
        return Err(error.with_unknown_outcome());
    }
    let current = context
        .active_identity(policy, Some(expected), deadline)
        .map_err(ProtocolError::with_unknown_outcome)?;
    if current != application {
        return Err(ProtocolError::new(
            ErrorCode::ApplicationMismatch,
            "active X11 application changed during click",
            false,
        )
        .with_unknown_outcome());
    }
    Ok(ResponseData::Input {
        application: current,
    })
}

async fn scroll(
    policy: &Policy,
    expected: &str,
    delta_x: i32,
    delta_y: i32,
    deadline: tokio::time::Instant,
) -> Result<ResponseData, ProtocolError> {
    let policy = policy.clone();
    let expected = expected.to_owned();
    run_x11_blocking(deadline, BlockingOperation::Mutation, move |cancellation| {
        scroll_blocking(&policy, &expected, delta_x, delta_y, deadline, cancellation)
    })
    .await
}

fn scroll_blocking(
    policy: &Policy,
    expected: &str,
    delta_x: i32,
    delta_y: i32,
    deadline: tokio::time::Instant,
    cancellation: BlockingCancellation,
) -> Result<ResponseData, ProtocolError> {
    let context = require_x11(cancellation)?;
    context.require_xtest()?;
    let point = context.pointer()?;
    let application = context.verify_coordinate_owner(policy, expected, point, deadline)?;
    let horizontal = scroll_steps(delta_x, 7, 6)?;
    let vertical = scroll_steps(delta_y, 4, 5)?;
    let mut emitted = false;
    for (detail, count) in [horizontal, vertical].into_iter().flatten() {
        for _ in 0..count {
            context
                .verify_pointer_owner(policy, expected, point, deadline)
                .map_err(|error| {
                    if emitted {
                        error.with_unknown_outcome()
                    } else {
                        error
                    }
                })?;
            if let Err(error) = context.send_input(BUTTON_PRESS_EVENT, detail, 0, 0) {
                if error.outcome_unknown {
                    let _ = context.send_input(BUTTON_RELEASE_EVENT, detail, 0, 0);
                }
                return Err(if emitted {
                    error.with_unknown_outcome()
                } else {
                    error
                });
            }
            emitted = true;
            let before_release = context
                .verify_pointer_owner(policy, expected, point, deadline)
                .map(|_| ());
            if let Err(error) = before_release {
                let _ = context.send_input(BUTTON_RELEASE_EVENT, detail, 0, 0);
                return Err(error.with_unknown_outcome());
            }
            if let Err(error) = context.send_input(BUTTON_RELEASE_EVENT, detail, 0, 0) {
                let _ = context.send_input(BUTTON_RELEASE_EVENT, detail, 0, 0);
                return Err(error.with_unknown_outcome());
            }
        }
    }
    let current = context
        .active_identity(policy, Some(expected), deadline)
        .map_err(|error| {
            if emitted {
                error.with_unknown_outcome()
            } else {
                error
            }
        })?;
    if current != application {
        return Err(ProtocolError::new(
            ErrorCode::ApplicationMismatch,
            "active X11 application changed during scroll",
            false,
        )
        .with_unknown_outcome());
    }
    Ok(ResponseData::Input {
        application: current,
    })
}

async fn type_text(
    policy: &Policy,
    expected: &str,
    text: &str,
    deadline: tokio::time::Instant,
) -> Result<ResponseData, ProtocolError> {
    let policy = policy.clone();
    let expected = expected.to_owned();
    let text = text.to_owned();
    run_x11_blocking(deadline, BlockingOperation::Mutation, move |cancellation| {
        type_text_blocking(&policy, &expected, &text, deadline, cancellation)
    })
    .await
}

fn type_text_blocking(
    policy: &Policy,
    expected: &str,
    text: &str,
    deadline: tokio::time::Instant,
    cancellation: BlockingCancellation,
) -> Result<ResponseData, ProtocolError> {
    let context = require_x11(cancellation)?;
    context.require_xtest()?;
    let keyboard = context.keyboard_map()?;
    let mut keys = Vec::with_capacity(text.chars().count());
    for character in text.chars() {
        keys.push(keyboard.for_character(character)?);
    }
    let shift_keycode = if keys.iter().any(|key| key.shift) {
        Some(keyboard.for_keysym(XK_SHIFT_L)?.keycode)
    } else {
        None
    };
    let application = context.active_identity(policy, Some(expected), deadline)?;
    let mut emitted = false;
    for mapped in keys {
        send_mapped_key(
            &context,
            policy,
            expected,
            mapped,
            shift_keycode,
            deadline,
            &mut emitted,
        )?;
    }
    let current = context
        .active_identity(policy, Some(expected), deadline)
        .map_err(|error| {
            if emitted {
                error.with_unknown_outcome()
            } else {
                error
            }
        })?;
    if current != application {
        return Err(ProtocolError::new(
            ErrorCode::ApplicationMismatch,
            "active X11 application changed during text input",
            false,
        )
        .with_unknown_outcome());
    }
    Ok(ResponseData::Input {
        application: current,
    })
}

async fn key_press(
    policy: &Policy,
    expected: &str,
    key: Key,
    modifiers: &[KeyModifier],
    deadline: tokio::time::Instant,
) -> Result<ResponseData, ProtocolError> {
    let policy = policy.clone();
    let expected = expected.to_owned();
    let modifiers = modifiers.to_vec();
    run_x11_blocking(deadline, BlockingOperation::Mutation, move |cancellation| {
        key_press_blocking(&policy, &expected, key, &modifiers, deadline, cancellation)
    })
    .await
}

fn key_press_blocking(
    policy: &Policy,
    expected: &str,
    key: Key,
    modifiers: &[KeyModifier],
    deadline: tokio::time::Instant,
    cancellation: BlockingCancellation,
) -> Result<ResponseData, ProtocolError> {
    let context = require_x11(cancellation)?;
    context.require_xtest()?;
    let keyboard = context.keyboard_map()?;
    let main = keyboard.for_keysym(protocol_key_keysym(key))?;
    let mut modifier_keys = Vec::with_capacity(modifiers.len());
    for modifier in modifiers {
        let keysym = modifier_keysym(*modifier).ok_or_else(|| {
            ProtocolError::new(
                ErrorCode::EventCreationFailed,
                "X11 does not expose a portable Function modifier",
                false,
            )
        })?;
        modifier_keys.push(keyboard.for_keysym(keysym)?.keycode);
    }
    if main.shift {
        let shift = keyboard.for_keysym(XK_SHIFT_L)?.keycode;
        if !modifier_keys.contains(&shift) {
            modifier_keys.push(shift);
        }
    }
    let application = context.active_identity(policy, Some(expected), deadline)?;
    let mut held = Vec::new();
    let mut emitted = false;
    for keycode in modifier_keys {
        if let Err(error) = revalidate_and_send_key(
            &context,
            policy,
            expected,
            KEY_PRESS_EVENT,
            keycode,
            deadline,
        ) {
            if error.outcome_unknown {
                let _ = context.send_input(KEY_RELEASE_EVENT, keycode, 0, 0);
            }
            release_held_keys(&context, &held);
            return Err(if emitted {
                error.with_unknown_outcome()
            } else {
                error
            });
        }
        emitted = true;
        held.push(keycode);
    }
    if let Err(error) = revalidate_and_send_key(
        &context,
        policy,
        expected,
        KEY_PRESS_EVENT,
        main.keycode,
        deadline,
    ) {
        if error.outcome_unknown {
            let _ = context.send_input(KEY_RELEASE_EVENT, main.keycode, 0, 0);
        }
        release_held_keys(&context, &held);
        return Err(if emitted {
            error.with_unknown_outcome()
        } else {
            error
        });
    }
    emitted = true;
    held.push(main.keycode);
    let mut failure = None;
    for keycode in held.iter().rev().copied() {
        if failure.is_none()
            && let Err(error) = revalidate_and_send_key(
                &context,
                policy,
                expected,
                KEY_RELEASE_EVENT,
                keycode,
                deadline,
            )
        {
            let _ = context.send_input(KEY_RELEASE_EVENT, keycode, 0, 0);
            failure = Some(error);
        } else if failure.is_some() {
            let _ = context.send_input(KEY_RELEASE_EVENT, keycode, 0, 0);
        }
    }
    if let Some(error) = failure {
        return Err(error.with_unknown_outcome());
    }
    let current = context
        .active_identity(policy, Some(expected), deadline)
        .map_err(ProtocolError::with_unknown_outcome)?;
    if current != application || !emitted {
        return Err(ProtocolError::new(
            ErrorCode::ApplicationMismatch,
            "active X11 application changed during key input",
            false,
        )
        .with_unknown_outcome());
    }
    Ok(ResponseData::Input {
        application: current,
    })
}

fn send_mapped_key(
    context: &X11Context,
    policy: &Policy,
    expected: &str,
    mapped: MappedKey,
    shift_keycode: Option<Keycode>,
    deadline: tokio::time::Instant,
    emitted: &mut bool,
) -> Result<(), ProtocolError> {
    let shift = if mapped.shift {
        Some(shift_keycode.ok_or_else(|| {
            protocol_violation("preflighted X11 shifted key has no Shift keycode")
        })?)
    } else {
        None
    };
    if let Some(shift) = shift {
        if let Err(error) =
            revalidate_and_send_key(context, policy, expected, KEY_PRESS_EVENT, shift, deadline)
        {
            if error.outcome_unknown {
                let _ = context.send_input(KEY_RELEASE_EVENT, shift, 0, 0);
            }
            return Err(if *emitted {
                error.with_unknown_outcome()
            } else {
                error
            });
        }
        *emitted = true;
    }
    if let Err(error) = revalidate_and_send_key(
        context,
        policy,
        expected,
        KEY_PRESS_EVENT,
        mapped.keycode,
        deadline,
    ) {
        if error.outcome_unknown {
            let _ = context.send_input(KEY_RELEASE_EVENT, mapped.keycode, 0, 0);
        }
        if let Some(shift) = shift {
            let _ = context.send_input(KEY_RELEASE_EVENT, shift, 0, 0);
        }
        return Err(if *emitted {
            error.with_unknown_outcome()
        } else {
            error
        });
    }
    *emitted = true;
    if let Err(error) = revalidate_and_send_key(
        context,
        policy,
        expected,
        KEY_RELEASE_EVENT,
        mapped.keycode,
        deadline,
    ) {
        let _ = context.send_input(KEY_RELEASE_EVENT, mapped.keycode, 0, 0);
        if let Some(shift) = shift {
            let _ = context.send_input(KEY_RELEASE_EVENT, shift, 0, 0);
        }
        return Err(error.with_unknown_outcome());
    }
    if let Some(shift) = shift
        && let Err(error) = revalidate_and_send_key(
            context,
            policy,
            expected,
            KEY_RELEASE_EVENT,
            shift,
            deadline,
        )
    {
        let _ = context.send_input(KEY_RELEASE_EVENT, shift, 0, 0);
        return Err(error.with_unknown_outcome());
    }
    Ok(())
}

fn revalidate_and_send_key(
    context: &X11Context,
    policy: &Policy,
    expected: &str,
    event_type: u8,
    keycode: Keycode,
    deadline: tokio::time::Instant,
) -> Result<(), ProtocolError> {
    context.active_identity(policy, Some(expected), deadline)?;
    context.send_input(event_type, keycode, 0, 0)
}

fn release_held_keys(context: &X11Context, held: &[Keycode]) {
    for keycode in held.iter().rev().copied() {
        let _ = context.send_input(KEY_RELEASE_EVENT, keycode, 0, 0);
    }
}

#[derive(Clone, Copy)]
struct MappedKey {
    keycode: Keycode,
    shift: bool,
}

struct KeyboardMap {
    first_keycode: Keycode,
    keycode_count: u8,
    keysyms_per_keycode: usize,
    keysyms: Vec<Keysym>,
}

impl KeyboardMap {
    fn new(
        first_keycode: Keycode,
        keycode_count: u8,
        reply: x11rb::protocol::xproto::GetKeyboardMappingReply,
    ) -> Result<Self, ProtocolError> {
        let per_keycode = usize::from(reply.keysyms_per_keycode);
        let expected = usize::from(keycode_count)
            .checked_mul(per_keycode)
            .ok_or_else(|| protocol_violation("X11 keyboard map size overflowed"))?;
        if per_keycode == 0 || reply.keysyms.len() != expected {
            return Err(protocol_violation("X11 keyboard map has an invalid shape"));
        }
        Ok(Self {
            first_keycode,
            keycode_count,
            keysyms_per_keycode: per_keycode,
            keysyms: reply.keysyms,
        })
    }

    fn for_character(&self, character: char) -> Result<MappedKey, ProtocolError> {
        let value = u32::from(character);
        let keysym = if value <= 0xff {
            value
        } else {
            0x0100_0000 | value
        };
        self.for_keysym(keysym)
    }

    fn for_keysym(&self, keysym: Keysym) -> Result<MappedKey, ProtocolError> {
        for offset in 0..usize::from(self.keycode_count) {
            let start = offset
                .checked_mul(self.keysyms_per_keycode)
                .ok_or_else(|| protocol_violation("X11 keyboard map offset overflowed"))?;
            for level in 0..self.keysyms_per_keycode.min(2) {
                if self.keysyms.get(start + level).copied() == Some(keysym) {
                    let offset = u8::try_from(offset).map_err(|_| {
                        protocol_violation("X11 keyboard keycode offset overflowed")
                    })?;
                    let keycode = self
                        .first_keycode
                        .checked_add(offset)
                        .ok_or_else(|| protocol_violation("X11 keyboard keycode overflowed"))?;
                    return Ok(MappedKey {
                        keycode,
                        shift: level == 1,
                    });
                }
            }
        }
        Err(ProtocolError::new(
            ErrorCode::EventCreationFailed,
            "requested character or key is not present in the current X11 keyboard map",
            false,
        ))
    }
}

#[derive(Clone, Copy, Debug)]
struct X11ImageLayout {
    bytes_per_pixel: usize,
    stride: usize,
    total_bytes: usize,
    rgb_row_bytes: usize,
}

fn x11_image_layout(
    width: u16,
    height: u16,
    bits_per_pixel: u8,
    scanline_pad: u8,
) -> Result<X11ImageLayout, ProtocolError> {
    let bytes_per_pixel = match bits_per_pixel {
        16 => 2_usize,
        24 => 3_usize,
        32 => 4_usize,
        _ => {
            return Err(ProtocolError::new(
                ErrorCode::ScreenCaptureUnavailable,
                "X11 screen uses an unsupported bits-per-pixel format",
                false,
            ));
        }
    };
    if !matches!(scanline_pad, 8 | 16 | 32) {
        return Err(ProtocolError::new(
            ErrorCode::ScreenCaptureUnavailable,
            "X11 screen uses an unsupported scanline format",
            false,
        ));
    }
    let row_bits = usize::from(width)
        .checked_mul(usize::from(bits_per_pixel))
        .ok_or_else(|| protocol_violation("X11 screenshot row size overflowed"))?;
    let pad = usize::from(scanline_pad);
    let stride_bits = row_bits
        .checked_add(pad.saturating_sub(1))
        .map(|value| value / pad * pad)
        .ok_or_else(|| protocol_violation("X11 screenshot stride overflowed"))?;
    let stride = stride_bits / 8;
    let total_bytes = stride
        .checked_mul(usize::from(height))
        .ok_or_else(|| protocol_violation("X11 screenshot size overflowed"))?;
    let rgb_row_bytes = usize::from(width)
        .checked_mul(3)
        .ok_or_else(|| protocol_violation("X11 RGB row size overflowed"))?;
    let rgb_total = rgb_row_bytes
        .checked_mul(usize::from(height))
        .ok_or_else(|| protocol_violation("X11 RGB screenshot size overflowed"))?;
    if total_bytes == 0
        || total_bytes as u64 > MAX_SCREENSHOT_BYTES
        || rgb_total as u64 > MAX_SCREENSHOT_BYTES
    {
        return Err(ProtocolError::new(
            ErrorCode::OutputTooLarge,
            "preflighted X11 screenshot exceeds the protocol limit",
            false,
        ));
    }
    Ok(X11ImageLayout {
        bytes_per_pixel,
        stride,
        total_bytes,
        rgb_row_bytes,
    })
}

fn encode_x11_png(
    data: &[u8],
    width: u16,
    height: u16,
    layout: X11ImageLayout,
    byte_order: ImageOrder,
    red_mask: u32,
    green_mask: u32,
    blue_mask: u32,
    cancellation: &BlockingCancellation,
) -> Result<Vec<u8>, ProtocolError> {
    if red_mask == 0 || green_mask == 0 || blue_mask == 0 {
        return Err(ProtocolError::new(
            ErrorCode::ScreenCaptureUnavailable,
            "X11 screen uses an unsupported color-mask format",
            false,
        ));
    }
    if data.len() != layout.total_bytes {
        return Err(protocol_violation(
            "X11 screenshot payload does not match its declared geometry",
        ));
    }
    let mut output = BoundedBytes::new(MAX_SCREENSHOT_BYTES as usize);
    {
        let mut encoder = png::Encoder::new(&mut output, u32::from(width), u32::from(height));
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(png_error)?;
        {
            let mut stream = writer.stream_writer().map_err(png_error)?;
            for row in data.chunks_exact(layout.stride) {
                cancellation.check()?;
                let rgb = decode_x11_row(
                    row, width, layout, byte_order, red_mask, green_mask, blue_mask,
                )?;
                stream.write_all(&rgb).map_err(|error| {
                    ProtocolError::new(
                        ErrorCode::OutputTooLarge,
                        format!("could not stream bounded X11 screenshot: {error}"),
                        false,
                    )
                })?;
            }
            stream.finish().map_err(png_error)?;
        }
        writer.finish().map_err(png_error)?;
    }
    output.into_bytes()
}

fn decode_x11_row(
    row: &[u8],
    width: u16,
    layout: X11ImageLayout,
    byte_order: ImageOrder,
    red_mask: u32,
    green_mask: u32,
    blue_mask: u32,
) -> Result<Vec<u8>, ProtocolError> {
    if row.len() != layout.stride {
        return Err(protocol_violation("X11 screenshot row is truncated"));
    }
    let pixel_bytes = usize::from(width)
        .checked_mul(layout.bytes_per_pixel)
        .ok_or_else(|| protocol_violation("X11 screenshot pixel row overflowed"))?;
    let row = row
        .get(..pixel_bytes)
        .ok_or_else(|| protocol_violation("X11 screenshot row is truncated"))?;
    let mut rgb = Vec::with_capacity(layout.rgb_row_bytes);
    for bytes in row.chunks_exact(layout.bytes_per_pixel) {
        let pixel = match (byte_order, layout.bytes_per_pixel) {
            (order, 2) if order == ImageOrder::LSB_FIRST => {
                u32::from(u16::from_le_bytes([bytes[0], bytes[1]]))
            }
            (order, 2) if order == ImageOrder::MSB_FIRST => {
                u32::from(u16::from_be_bytes([bytes[0], bytes[1]]))
            }
            (order, 3) if order == ImageOrder::LSB_FIRST => {
                u32::from(bytes[0]) | (u32::from(bytes[1]) << 8) | (u32::from(bytes[2]) << 16)
            }
            (order, 3) if order == ImageOrder::MSB_FIRST => {
                (u32::from(bytes[0]) << 16) | (u32::from(bytes[1]) << 8) | u32::from(bytes[2])
            }
            (order, 4) if order == ImageOrder::LSB_FIRST => {
                u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
            }
            (order, 4) if order == ImageOrder::MSB_FIRST => {
                u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
            }
            _ => {
                return Err(ProtocolError::new(
                    ErrorCode::ScreenCaptureUnavailable,
                    "X11 screen reports an unknown image byte order",
                    false,
                ));
            }
        };
        rgb.extend_from_slice(&[
            scale_masked_channel(pixel, red_mask)?,
            scale_masked_channel(pixel, green_mask)?,
            scale_masked_channel(pixel, blue_mask)?,
        ]);
    }
    Ok(rgb)
}

struct BoundedBytes {
    bytes: Vec<u8>,
    limit: usize,
}

impl BoundedBytes {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
        }
    }

    fn into_bytes(self) -> Result<Vec<u8>, ProtocolError> {
        if self.bytes.is_empty() {
            Err(ProtocolError::new(
                ErrorCode::ScreenCaptureUnavailable,
                "X11 PNG encoder produced an empty image",
                false,
            ))
        } else {
            Ok(self.bytes)
        }
    }
}

impl Write for BoundedBytes {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let length = self
            .bytes
            .len()
            .checked_add(buffer.len())
            .ok_or_else(|| std::io::Error::other("PNG size overflow"))?;
        if length > self.limit {
            return Err(std::io::Error::other("PNG exceeds protocol size limit"));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn png_error(error: png::EncodingError) -> ProtocolError {
    ProtocolError::new(
        ErrorCode::ScreenCaptureUnavailable,
        format!("could not encode bounded X11 screenshot: {error}"),
        false,
    )
}

fn scale_masked_channel(pixel: u32, mask: u32) -> Result<u8, ProtocolError> {
    let shift = mask.trailing_zeros();
    let maximum = mask >> shift;
    if maximum == 0 {
        return Err(protocol_violation("X11 color channel mask is empty"));
    }
    if maximum != u32::MAX && maximum & (maximum + 1) != 0 {
        return Err(protocol_violation(
            "X11 color channel mask is not contiguous",
        ));
    }
    let value = (pixel & mask) >> shift;
    let scaled = value
        .checked_mul(255)
        .and_then(|value| value.checked_add(maximum / 2))
        .map(|value| value / maximum)
        .ok_or_else(|| protocol_violation("X11 color channel scaling overflowed"))?;
    u8::try_from(scaled).map_err(|_| protocol_violation("X11 color channel is invalid"))
}

fn bounded_property_string(reply: &GetPropertyReply) -> Result<String, ProtocolError> {
    let value = reply.value.strip_suffix(&[0]).unwrap_or(&reply.value);
    let value = std::str::from_utf8(value)
        .map_err(|_| protocol_violation("X11 string property is not valid UTF-8"))?;
    if !valid_external_string(value) {
        return Err(protocol_violation(
            "X11 string property is invalid or too long",
        ));
    }
    Ok(value.to_owned())
}

fn process_name(pid: u32) -> Result<String, ProtocolError> {
    let path = std::path::Path::new("/proc")
        .join(pid.to_string())
        .join("comm");
    let value = std::fs::read_to_string(path).map_err(|error| {
        ProtocolError::new(
            ErrorCode::ApplicationNotFound,
            format!(
                "could not read live Linux process identity: {}",
                sanitized_external(&error.to_string())
            ),
            true,
        )
    })?;
    let value = value.strip_suffix('\n').unwrap_or(&value);
    let value = value.strip_suffix('\r').unwrap_or(value);
    if !valid_external_string(value) {
        return Err(protocol_violation(
            "Linux process returned an invalid exact display identity",
        ));
    }
    Ok(value.to_owned())
}

fn linux_process_identity(
    pid: u32,
    process_name: String,
    reported_bundle_id: Option<String>,
) -> Result<ApplicationIdentity, ProtocolError> {
    if application_selector_kind(&process_name) == ApplicationSelectorKind::BundleIdentifier
        && reported_bundle_id.is_none()
    {
        return Err(ProtocolError::new(
            ErrorCode::ApplicationNotFound,
            "dotted Linux process identity has no authoritative application identifier",
            false,
        ));
    }
    let identity = ApplicationIdentity {
        name: process_name,
        bundle_id: reported_bundle_id,
        pid,
    };
    validate_identity_shape(&identity)?;
    Ok(identity)
}

fn validate_application(
    application: &ApplicationIdentity,
    policy: &Policy,
    expected: Option<&str>,
) -> Result<(), ProtocolError> {
    validate_identity_shape(application)?;
    if !policy.allows(application) {
        return Err(ProtocolError::new(
            ErrorCode::ApplicationNotAllowed,
            "Linux application is not admitted by the resolved policy",
            false,
        ));
    }
    if expected.is_some_and(|selector| !application.matches(selector)) {
        return Err(ProtocolError::new(
            ErrorCode::ApplicationMismatch,
            "active Linux application does not match expected_application",
            true,
        ));
    }
    Ok(())
}

fn validate_identity_shape(application: &ApplicationIdentity) -> Result<(), ProtocolError> {
    if application.pid == 0
        || !valid_external_string(&application.name)
        || application
            .bundle_id
            .as_ref()
            .is_some_and(|value| !valid_external_string(value))
    {
        return Err(protocol_violation(
            "Linux returned an invalid application identity",
        ));
    }
    Ok(())
}

fn valid_external_string(value: &str) -> bool {
    !value.is_empty()
        && value.trim() == value
        && value.chars().count() <= MAX_AX_STRING_CHARS
        && !value
            .chars()
            .any(zeroclaw_api::tool::is_unsafe_confirmation_character)
}

fn exact_ax_selector_value(value: &str) -> Option<&str> {
    valid_external_string(value).then_some(value)
}

fn sanitize_ax_string(mut value: String) -> String {
    value = value
        .chars()
        .map(|character| {
            if zeroclaw_api::tool::is_unsafe_confirmation_character(character) {
                '\u{fffd}'
            } else {
                character
            }
        })
        .take(MAX_AX_STRING_CHARS)
        .collect();
    value.trim().to_owned()
}

fn sanitized_external(value: &str) -> String {
    sanitize_ax_string(value.to_owned())
}

fn logical_coordinate(value: f64, axis: &str) -> Result<i16, ProtocolError> {
    let rounded = value.round();
    if !value.is_finite()
        || (value - rounded).abs() > 0.000_001
        || rounded < f64::from(i16::MIN)
        || rounded > f64::from(i16::MAX)
    {
        return Err(ProtocolError::new(
            ErrorCode::InvalidCoordinate,
            format!("X11 {axis} coordinate must be one exact signed 16-bit pixel"),
            false,
        ));
    }
    Ok(rounded as i16)
}

fn scroll_steps(
    delta: i32,
    positive_detail: u8,
    negative_detail: u8,
) -> Result<Option<(u8, u32)>, ProtocolError> {
    if delta == 0 {
        return Ok(None);
    }
    let steps = delta.unsigned_abs().div_ceil(120).max(1);
    if steps > MAX_X11_SCROLL_STEPS {
        return Err(ProtocolError::new(
            ErrorCode::InvalidRequest,
            format!(
                "X11 scroll request expands to more than {MAX_X11_SCROLL_STEPS} discrete events"
            ),
            false,
        ));
    }
    Ok(Some((
        if delta > 0 {
            positive_detail
        } else {
            negative_detail
        },
        steps,
    )))
}

fn mouse_button_detail(button: MouseButton) -> u8 {
    match button {
        MouseButton::Left => u8::from(ButtonIndex::M1),
        MouseButton::Middle => u8::from(ButtonIndex::M2),
        MouseButton::Right => u8::from(ButtonIndex::M3),
    }
}

fn protocol_key_keysym(key: Key) -> Keysym {
    match key {
        Key::A => u32::from(b'a'),
        Key::B => u32::from(b'b'),
        Key::C => u32::from(b'c'),
        Key::D => u32::from(b'd'),
        Key::E => u32::from(b'e'),
        Key::F => u32::from(b'f'),
        Key::G => u32::from(b'g'),
        Key::H => u32::from(b'h'),
        Key::I => u32::from(b'i'),
        Key::J => u32::from(b'j'),
        Key::K => u32::from(b'k'),
        Key::L => u32::from(b'l'),
        Key::M => u32::from(b'm'),
        Key::N => u32::from(b'n'),
        Key::O => u32::from(b'o'),
        Key::P => u32::from(b'p'),
        Key::Q => u32::from(b'q'),
        Key::R => u32::from(b'r'),
        Key::S => u32::from(b's'),
        Key::T => u32::from(b't'),
        Key::U => u32::from(b'u'),
        Key::V => u32::from(b'v'),
        Key::W => u32::from(b'w'),
        Key::X => u32::from(b'x'),
        Key::Y => u32::from(b'y'),
        Key::Z => u32::from(b'z'),
        Key::Digit0 => u32::from(b'0'),
        Key::Digit1 => u32::from(b'1'),
        Key::Digit2 => u32::from(b'2'),
        Key::Digit3 => u32::from(b'3'),
        Key::Digit4 => u32::from(b'4'),
        Key::Digit5 => u32::from(b'5'),
        Key::Digit6 => u32::from(b'6'),
        Key::Digit7 => u32::from(b'7'),
        Key::Digit8 => u32::from(b'8'),
        Key::Digit9 => u32::from(b'9'),
        Key::Enter => 0xff0d,
        Key::Tab => 0xff09,
        Key::Space => 0x20,
        Key::Backspace => 0xff08,
        Key::Delete => 0xffff,
        Key::Escape => 0xff1b,
        Key::Home => 0xff50,
        Key::End => 0xff57,
        Key::PageUp => 0xff55,
        Key::PageDown => 0xff56,
        Key::LeftArrow => 0xff51,
        Key::RightArrow => 0xff53,
        Key::UpArrow => 0xff52,
        Key::DownArrow => 0xff54,
        Key::F1 => 0xffbe,
        Key::F2 => 0xffbf,
        Key::F3 => 0xffc0,
        Key::F4 => 0xffc1,
        Key::F5 => 0xffc2,
        Key::F6 => 0xffc3,
        Key::F7 => 0xffc4,
        Key::F8 => 0xffc5,
        Key::F9 => 0xffc6,
        Key::F10 => 0xffc7,
        Key::F11 => 0xffc8,
        Key::F12 => 0xffc9,
    }
}

const XK_SHIFT_L: Keysym = 0xffe1;

fn modifier_keysym(modifier: KeyModifier) -> Option<Keysym> {
    match modifier {
        KeyModifier::Command => Some(0xffeb),
        KeyModifier::Control => Some(0xffe3),
        KeyModifier::Option => Some(0xffe9),
        KeyModifier::Shift => Some(XK_SHIFT_L),
        KeyModifier::Function => None,
    }
}

fn remaining(deadline: tokio::time::Instant, maximum: Duration) -> Result<Duration, ProtocolError> {
    let now = tokio::time::Instant::now();
    if now >= deadline {
        return Err(ProtocolError::new(
            ErrorCode::Timeout,
            "computer-use request deadline expired",
            false,
        ));
    }
    Ok(deadline.duration_since(now).min(maximum))
}

fn check_deadline(deadline: tokio::time::Instant) -> Result<(), ProtocolError> {
    remaining(deadline, Duration::MAX).map(|_| ())
}

fn x11_error(label: &str, error: impl std::fmt::Display) -> ProtocolError {
    ProtocolError::new(
        ErrorCode::CommandFailed,
        format!("{label} failed: {}", sanitized_external(&error.to_string())),
        true,
    )
}

fn protocol_violation(message: impl Into<String>) -> ProtocolError {
    ProtocolError::new(ErrorCode::ProtocolViolation, message, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_detection_never_falls_back_from_explicit_wayland() {
        use std::ffi::OsStr;
        assert_eq!(
            desktop_session_from(
                Some(OsStr::new("wayland")),
                Some(OsStr::new(":0")),
                Some(OsStr::new("wayland-0")),
            ),
            DesktopSession::Wayland
        );
        assert_eq!(
            desktop_session_from(Some(OsStr::new("wayland")), Some(OsStr::new(":0")), None,),
            DesktopSession::Unavailable
        );
    }

    #[test]
    fn ambiguous_implicit_session_is_unavailable() {
        use std::ffi::OsStr;
        assert_eq!(
            desktop_session_from(None, Some(OsStr::new(":0")), Some(OsStr::new("wayland-0")),),
            DesktopSession::Unavailable
        );
    }

    #[test]
    fn desktop_worker_lease_remains_exclusive_until_owner_drops() {
        let lease = LinuxDesktopWorkerLease::acquire().unwrap();
        assert!(LinuxDesktopWorkerLease::acquire().is_err());
        drop(lease);
        assert!(LinuxDesktopWorkerLease::acquire().is_ok());
    }

    #[test]
    fn cancellation_closes_the_next_mutation_boundary() {
        let cancellation = BlockingCancellation::new();
        let event = cancellation.begin_event().unwrap();
        cancellation.cancel();
        assert!(cancellation.begin_event().is_err());
        drop(event);
        assert!(cancellation.check().is_err());
    }

    #[test]
    fn xresource_version_must_support_local_client_pid() {
        assert!(!xres_supports_local_pid(1, 1));
        assert!(xres_supports_local_pid(1, 2));
        assert!(xres_supports_local_pid(2, 0));
    }

    #[test]
    fn pixel_decoder_handles_common_little_endian_bgrx() {
        let layout = x11_image_layout(1, 1, 32, 32).unwrap();
        let rgb = decode_x11_row(
            &[0x33, 0x22, 0x11, 0],
            1,
            layout,
            ImageOrder::LSB_FIRST,
            0x00ff_0000,
            0x0000_ff00,
            0x0000_00ff,
        )
        .unwrap();
        assert_eq!(rgb, [0x11, 0x22, 0x33]);
    }

    #[test]
    fn secure_strings_are_bounded_and_control_free() {
        let value = format!("  hello\n{}  ", "x".repeat(MAX_AX_STRING_CHARS + 20));
        let sanitized = sanitize_ax_string(value);
        assert!(sanitized.chars().count() <= MAX_AX_STRING_CHARS);
        assert!(!sanitized.contains('\n'));
    }

    #[test]
    fn keyboard_map_preflights_shifted_and_missing_keys() {
        let map = KeyboardMap {
            first_keycode: 8,
            keycode_count: 1,
            keysyms_per_keycode: 2,
            keysyms: vec![u32::from(b'a'), u32::from(b'A')],
        };
        let lower = map.for_character('a').unwrap();
        let upper = map.for_character('A').unwrap();
        assert_eq!(lower.keycode, 8);
        assert!(!lower.shift);
        assert!(upper.shift);
        assert!(map.for_character('z').is_err());
    }

    #[test]
    fn dotted_selector_never_matches_display_name_namespace() {
        let identity = ApplicationIdentity {
            name: "org.example.Editor".to_owned(),
            bundle_id: None,
            pid: 42,
        };
        assert!(!identity.matches("org.example.Editor"));
    }

    #[test]
    fn dotted_process_name_requires_authoritative_application_id() {
        assert!(linux_process_identity(42, "org.example.Editor".to_owned(), None).is_err());
        let identity = linux_process_identity(
            42,
            "org.example.Editor".to_owned(),
            Some("org.freedesktop.StrongerId".to_owned()),
        )
        .unwrap();
        assert_eq!(identity.name, "org.example.Editor");
        assert_eq!(
            identity.bundle_id.as_deref(),
            Some("org.freedesktop.StrongerId")
        );
        assert!(!identity.matches("org.example.Editor"));
        assert!(identity.matches("org.freedesktop.StrongerId"));
    }

    #[test]
    fn scroll_button_details_are_axis_specific() {
        assert_eq!(scroll_steps(120, 7, 6).unwrap(), Some((7, 1)));
        assert_eq!(scroll_steps(-120, 7, 6).unwrap(), Some((6, 1)));
        assert_eq!(scroll_steps(120, 4, 5).unwrap(), Some((4, 1)));
        assert_eq!(scroll_steps(-120, 4, 5).unwrap(), Some((5, 1)));
    }

    #[test]
    fn capture_layout_rejects_oversized_reply_before_capture() {
        let error = x11_image_layout(u16::MAX, u16::MAX, 32, 32).unwrap_err();
        assert_eq!(error.code, ErrorCode::OutputTooLarge);
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "requires a logged-in Linux graphical session"]
    async fn live_linux_capabilities() {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        let result = capabilities(deadline).await.unwrap();
        let ResponseData::Capabilities { platform, .. } = result else {
            panic!("capabilities returned the wrong payload");
        };
        assert_eq!(platform, Platform::Linux);
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "requires an active logged-in Linux app selected by ZEROCLAW_TEST_COMPUTER_USE_APPLICATION"]
    async fn live_linux_list_and_inspect_exact_application() {
        let selector = std::env::var("ZEROCLAW_TEST_COMPUTER_USE_APPLICATION")
            .expect("set ZEROCLAW_TEST_COMPUTER_USE_APPLICATION to the active exact app identity");
        let policy = Policy {
            application_access: zeroclaw_config::schema::ComputerUseApplicationAccess::Allowlist,
            allowed_applications: vec![selector.clone()],
            min_coordinate_x: None,
            min_coordinate_y: None,
            max_coordinate_x: None,
            max_coordinate_y: None,
            max_text_chars: zeroclaw_config::schema::COMPUTER_USE_MAX_TEXT_CHARS,
        };
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        let listed = list_applications(&policy, deadline).await.unwrap();
        let ResponseData::Applications { applications, .. } = listed else {
            panic!("list_applications returned the wrong payload");
        };
        assert!(
            applications
                .iter()
                .any(|application| application.matches(&selector))
        );

        let inspect_deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        let inspected = inspect(
            &policy,
            &selector,
            DEFAULT_AX_NODES,
            DEFAULT_AX_DEPTH,
            inspect_deadline,
        )
        .await
        .unwrap();
        let ResponseData::Inspect { snapshot } = inspected else {
            panic!("inspect returned the wrong payload");
        };
        assert!(snapshot.application.matches(&selector));
        assert!(!snapshot.nodes.is_empty());
    }
}
