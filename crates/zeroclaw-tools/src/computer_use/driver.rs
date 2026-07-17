//! Platform driver for in-process computer use.
//!
//! The backend trait is intentionally private. Callers submit the versioned
//! protocol request through [`execute`], which validates the request before a
//! platform implementation can observe it.

use async_trait::async_trait;
use std::time::Duration;

use super::protocol::{Action, ErrorCode, ProtocolError, Request, Response, ResponseData};
use crate::screenshot::ScreenshotReservation;

#[async_trait]
trait Backend: Send + Sync {
    async fn execute(
        &self,
        action: &Action,
        policy: &super::protocol::Policy,
        screenshot: Option<&ScreenshotReservation>,
        deadline: tokio::time::Instant,
    ) -> Result<ResponseData, ProtocolError>;
}

/// Validate and execute one in-process computer-use request.
pub(crate) async fn execute(
    request: Request,
    screenshot: Option<&ScreenshotReservation>,
    timeout: Duration,
) -> Response {
    let request_id = request.request_id;
    if let Err(error) = request.validate() {
        return Response::failure(request_id, error);
    }

    let Some(deadline) = tokio::time::Instant::now().checked_add(timeout) else {
        return Response::failure(
            request_id,
            ProtocolError::new(
                ErrorCode::InvalidRequest,
                "computer-use timeout is outside the supported range",
                false,
            ),
        );
    };
    if timeout.is_zero() {
        return Response::failure(
            request_id,
            ProtocolError::new(
                ErrorCode::InvalidRequest,
                "computer-use timeout must be greater than zero",
                false,
            ),
        );
    }
    match (&request.action, screenshot) {
        (Action::Screenshot { path, .. }, Some(reservation)) if path == reservation.path() => {}
        (Action::Screenshot { .. }, _) => {
            return Response::failure(
                request_id,
                ProtocolError::new(
                    ErrorCode::InvalidPath,
                    "screenshot request does not match its held destination",
                    false,
                ),
            );
        }
        (_, Some(_)) => {
            return Response::failure(
                request_id,
                ProtocolError::new(
                    ErrorCode::InvalidRequest,
                    "non-screenshot request included a screenshot destination",
                    false,
                ),
            );
        }
        (_, None) => {}
    }

    let result = platform::PlatformBackend
        .execute(&request.action, &request.policy, screenshot, deadline)
        .await;
    let response = match result {
        Ok(data) => Response::success(request_id, data),
        Err(error) => Response::failure(request_id, error),
    };

    match response.validate_for(&request) {
        Ok(()) => response,
        Err(error) => Response::failure(request_id, error),
    }
}

#[cfg(target_os = "linux")]
#[path = "platform_linux.rs"]
mod platform;

#[cfg(target_os = "windows")]
#[path = "platform_windows.rs"]
mod platform;

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
mod platform {
    use super::*;

    pub(super) struct PlatformBackend;

    #[async_trait]
    impl Backend for PlatformBackend {
        async fn execute(
            &self,
            _action: &Action,
            _policy: &super::super::protocol::Policy,
            _screenshot: Option<&ScreenshotReservation>,
            _deadline: tokio::time::Instant,
        ) -> Result<ResponseData, ProtocolError> {
            Err(ProtocolError::new(
                ErrorCode::UnsupportedPlatform,
                "computer use is not implemented on this platform",
                false,
            ))
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::fs::File;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    use std::path::Path;
    use std::process::Stdio;
    use std::time::Duration;

    use core_foundation::base::CFType;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::number::CFNumber;
    use core_foundation::string::CFString;
    use core_graphics::access::ScreenCaptureAccess;
    use core_graphics::display::CGDisplay;
    use core_graphics::event::{
        CGEvent, CGEventFlags, CGEventType, CGKeyCode, CGMouseButton, KeyCode, ScrollEventUnit,
    };
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
    use core_graphics::geometry::{CGPoint, CGRect};
    use core_graphics::window::{
        create_description_from_array, create_window_list, kCGNullWindowID,
        kCGWindowListExcludeDesktopElements, kCGWindowListOptionOnScreenOnly,
    };
    use serde::Deserialize;
    use serde::de::DeserializeOwned;
    use serde_json::json;
    use tokio::io::{AsyncRead, AsyncReadExt};
    use tokio::process::Command;

    use super::*;
    use crate::computer_use::protocol::{
        AccessibilitySnapshot, ActionKind, ApplicationIdentity, ApplicationSelectorKind,
        DEFAULT_AX_DEPTH, DEFAULT_AX_NODES, ElementSummary, Key, KeyModifier, MAX_AX_DEPTH,
        MAX_AX_NODES, MAX_AX_STRING_CHARS, MAX_PROCESS_STDERR_BYTES, MAX_RESPONSE_BYTES,
        MAX_RUNNING_APPLICATIONS, MAX_SCREENSHOT_BYTES, MouseButton, PermissionState, Permissions,
        Platform, Point, Policy, Rect, application_selector_kind,
    };
    use zeroclaw_config::schema::ComputerUseApplicationAccess;

    const JXA_TIMEOUT: Duration = Duration::from_secs(15);
    const FOCUS_TIMEOUT: Duration = Duration::from_secs(5);
    const FOCUS_POLL_INTERVAL: Duration = Duration::from_millis(100);
    const MAX_JXA_OUTPUT_BYTES: usize = MAX_RESPONSE_BYTES;
    const MAX_JXA_OUTPUT_CHARS: usize = MAX_JXA_OUTPUT_BYTES / 4;

    /// One immutable script handles every accessibility query. Request values
    /// are supplied only as argv JSON; none are interpolated into source code.
    const MACOS_JXA: &str = r#"
'use strict';

function limitedString(readValue, limit) {
    try {
        const value = readValue();
        if (value === null || value === undefined) return null;
        if (typeof value === 'object') return null;
        return Array.from(String(value)).slice(0, limit).join('').replace(/[\u0000-\u001f\u007f]/g, ' ');
    } catch (_) {
        return null;
    }
}

function exactString(readValue, limit, label) {
    let value;
    try { value = readValue(); } catch (_) {
        throw codedError('application_not_found', label + ' is unavailable', true);
    }
    if (value === null || value === undefined || typeof value === 'object') {
        throw codedError('application_not_found', label + ' is unavailable', true);
    }
    const text = String(value);
    if (Array.from(text).length > limit || /[\u0000-\u001f\u007f]/.test(text)) {
        throw codedError('protocol_violation', label + ' exceeds the protocol limit', false);
    }
    return text;
}

function readBoolean(readValue) {
    try {
        const value = readValue();
        return typeof value === 'boolean' ? value : null;
    } catch (_) {
        return null;
    }
}

function failure(code, message, retryable) {
    return {
        ok: false,
        error: {
            code: code,
            message: Array.from(String(message)).slice(0, 512).join('').replace(/[\u0000-\u001f\u007f]/g, ' '),
            retryable: Boolean(retryable)
        }
    };
}

function codedError(code, message, retryable) {
    const error = new Error(message);
    error.protocolCode = code;
    error.retryable = Boolean(retryable);
    return error;
}

function frontmostProcess(systemEvents) {
    const processes = systemEvents.applicationProcesses.whose({frontmost: true})();
    if (!processes || processes.length !== 1) {
        throw codedError('application_not_found', 'macOS did not report exactly one frontmost application', true);
    }
    return processes[0];
}

function processIdentity(process, stringLimit) {
    const name = exactString(function () { return process.name(); }, stringLimit, 'application name');
    let bundleId = null;
    try {
        const candidate = process.bundleIdentifier();
        if (candidate !== null && candidate !== undefined && String(candidate).length > 0) {
            bundleId = exactString(function () { return candidate; }, stringLimit, 'bundle identifier');
        }
    } catch (_) {}
    let pid = null;
    try { pid = Number(process.unixId()); } catch (_) {}
    if (!name || !Number.isSafeInteger(pid) || pid <= 0) {
        throw codedError('application_not_found', 'frontmost application identity is incomplete', true);
    }
    return {name: name, bundle_id: bundleId, pid: pid};
}

function identityMatches(identity, selector) {
    if (!selector || typeof selector.value !== 'string') return false;
    if (selector.kind === 'bundle_identifier') return identity.bundle_id === selector.value;
    if (selector.kind === 'display_name') return identity.name === selector.value;
    return false;
}

function identityAllowed(identity, request) {
    return request.desktop_wide === true || request.allowed_applications.some(function (selector) {
        return identityMatches(identity, selector);
    });
}

function authorizeFrontmost(systemEvents, request) {
    const process = frontmostProcess(systemEvents);
    const identity = processIdentity(process, request.string_limit);
    if (!identityAllowed(identity, request)) {
        throw codedError('application_not_allowed', 'frontmost application is not allowed by this request', false);
    }
    if (request.expected_application && !identityMatches(identity, request.expected_application)) {
        throw codedError('application_mismatch', 'frontmost application does not match expected_application', true);
    }
    return {process: process, identity: identity};
}

function listApplications(systemEvents, request) {
    const processes = systemEvents.applicationProcesses();
    const applications = [];
    const seenPids = Object.create(null);
    let truncated = false;
    for (let index = 0; index < processes.length; index += 1) {
        const process = processes[index];
        if (readBoolean(function () { return process.backgroundOnly(); }) === true) continue;
        let identity;
        try {
            identity = processIdentity(process, request.string_limit);
        } catch (_) {
            continue;
        }
        if (!identityAllowed(identity, request) || seenPids[String(identity.pid)]) continue;
        if (applications.length >= request.max_applications) {
            truncated = true;
            break;
        }
        seenPids[String(identity.pid)] = true;
        applications.push(identity);
    }
    applications.sort(function (left, right) {
        const byName = left.name.localeCompare(right.name);
        return byName !== 0 ? byName : left.pid - right.pid;
    });
    return {ok: true, data: {applications: applications, truncated: truncated}};
}

function elementRole(element, limit) {
    return limitedString(function () { return element.role(); }, limit) || 'AXUnknown';
}

function elementTitle(element, limit) {
    return limitedString(function () { return element.title(); }, limit);
}

function exactElementRole(element, limit) {
    try {
        const value = element.role();
        if (value === null || value === undefined) return null;
        const text = String(value);
        return Array.from(text).length <= limit ? text : null;
    } catch (_) {
        return null;
    }
}

function exactElementTitle(element, limit) {
    try {
        const value = element.title();
        if (value === null || value === undefined) return null;
        const text = String(value);
        return Array.from(text).length <= limit ? text : null;
    } catch (_) {
        return null;
    }
}

function elementBounds(element) {
    try {
        const position = element.position();
        const size = element.size();
        const values = [Number(position[0]), Number(position[1]), Number(size[0]), Number(size[1])];
        if (!values.every(Number.isFinite) || values[2] < 0 || values[3] < 0) return null;
        return {x: values[0], y: values[1], width: values[2], height: values[3]};
    } catch (_) {
        return null;
    }
}

function elementActions(element, limit) {
    let actions;
    try { actions = element.actions(); } catch (_) { return []; }
    const names = [];
    for (let index = 0; index < actions.length && names.length < 32; index += 1) {
        const name = limitedString(function () { return actions[index].name(); }, limit);
        if (name) names.push(name);
    }
    return names;
}

function elementChildren(element) {
    try { return element.uiElements(); } catch (_) { return []; }
}

function snapshotNode(item, id, stringLimit) {
    const role = elementRole(item.element, stringLimit);
    const subrole = limitedString(function () { return item.element.subrole(); }, stringLimit);
    const secure = role.toUpperCase().indexOf('SECURE') >= 0
        || role.toUpperCase().indexOf('PASSWORD') >= 0
        || (subrole && (subrole.toUpperCase().indexOf('SECURE') >= 0
            || subrole.toUpperCase().indexOf('PASSWORD') >= 0));
    return {
        id: id,
        parent_id: item.parentId,
        depth: item.depth,
        role: role,
        title: elementTitle(item.element, stringLimit),
        value: secure ? null : limitedString(function () { return item.element.value(); }, stringLimit),
        description: limitedString(function () { return item.element.description(); }, stringLimit),
        enabled: readBoolean(function () { return item.element.enabled(); }),
        focused: readBoolean(function () { return item.element.focused(); }),
        bounds: elementBounds(item.element),
        actions: elementActions(item.element, stringLimit)
    };
}

function inspect(systemEvents, request) {
    const target = authorizeFrontmost(systemEvents, request);
    const nodes = [];
    const queue = [{element: target.process, parentId: null, depth: 0}];
    let truncated = false;
    let cursor = 0;

    while (cursor < queue.length && nodes.length < request.max_nodes) {
        const item = queue[cursor];
        cursor += 1;
        const id = nodes.length + 1;
        nodes.push(snapshotNode(item, id, request.string_limit));

        if (item.depth >= request.max_depth) {
            if (elementChildren(item.element).length > 0) truncated = true;
            continue;
        }
        const children = elementChildren(item.element);
        for (let index = 0; index < children.length; index += 1) {
            if (queue.length >= request.max_nodes) {
                truncated = true;
                break;
            }
            queue.push({element: children[index], parentId: id, depth: item.depth + 1});
        }
    }
    if (cursor < queue.length) truncated = true;

    const snapshot = {
        application: target.identity,
        nodes: nodes,
        truncated: truncated,
        max_nodes: request.max_nodes,
        max_depth: request.max_depth
    };
    let envelope = {ok: true, data: snapshot};
    let serialized = JSON.stringify(envelope);
    while (serialized.length > request.output_chars && snapshot.nodes.length > 1) {
        snapshot.nodes.pop();
        snapshot.truncated = true;
        serialized = JSON.stringify(envelope);
    }
    if (serialized.length > request.output_chars) {
        return failure('output_too_large', 'accessibility snapshot exceeds the output limit', false);
    }
    return envelope;
}

function pressElement(systemEvents, request) {
    const initial = authorizeFrontmost(systemEvents, request);
    const queue = [{element: initial.process, depth: 0}];
    const matches = [];
    let cursor = 0;
    let visited = 0;
    let truncated = false;

    while (cursor < queue.length && visited < request.max_nodes && matches.length < 2) {
        const item = queue[cursor];
        cursor += 1;
        visited += 1;
        const role = exactElementRole(item.element, request.string_limit);
        const title = exactElementTitle(item.element, request.string_limit);
        if (title === request.title && (!request.role || role === request.role)) {
            matches.push({element: item.element, role: role, title: title, bounds: elementBounds(item.element)});
        }

        if (item.depth >= request.max_depth) {
            if (elementChildren(item.element).length > 0) truncated = true;
            continue;
        }
        const children = elementChildren(item.element);
        for (let index = 0; index < children.length; index += 1) {
            if (queue.length >= request.max_nodes) {
                truncated = true;
                break;
            }
            queue.push({element: children[index], depth: item.depth + 1});
        }
    }
    if (cursor < queue.length) truncated = true;
    if (matches.length === 0) {
        throw codedError('element_not_found', truncated ? 'no matching element found within bounded search' : 'no matching element found', true);
    }
    if (matches.length !== 1 || truncated) {
        throw codedError('ambiguous_element', truncated ? 'bounded search cannot establish a unique match' : 'more than one element matched', false);
    }

    // Resolve policy and identity again immediately before the AXPress side effect.
    const current = authorizeFrontmost(systemEvents, request);
    if (current.identity.pid !== initial.identity.pid) {
        throw codedError('application_mismatch', 'frontmost application changed during element search', true);
    }
    const currentRole = exactElementRole(matches[0].element, request.string_limit);
    const currentTitle = exactElementTitle(matches[0].element, request.string_limit);
    const currentEnabled = readBoolean(function () { return matches[0].element.enabled(); });
    if (currentRole !== matches[0].role || currentTitle !== matches[0].title) {
        throw codedError('application_mismatch', 'matched element changed before AXPress', false);
    }
    if (currentEnabled === false) {
        throw codedError('element_not_found', 'matched element is disabled', false);
    }
    const actions = elementActions(matches[0].element, request.string_limit);
    if (actions.indexOf('AXPress') < 0) {
        throw codedError('element_not_found', 'matched element does not support AXPress', false);
    }

    let actionObjects;
    try { actionObjects = matches[0].element.actions(); } catch (_) {
        throw codedError('accessibility_unavailable', 'could not resolve element actions', true);
    }
    let pressAction = null;
    for (let index = 0; index < actionObjects.length; index += 1) {
        const name = limitedString(function () { return actionObjects[index].name(); }, request.string_limit);
        if (name === 'AXPress') {
            if (pressAction !== null) {
                throw codedError('ambiguous_element', 'matched element exposes multiple AXPress actions', false);
            }
            pressAction = actionObjects[index];
        }
    }
    if (pressAction === null) {
        throw codedError('element_not_found', 'matched element does not expose AXPress', false);
    }
    try {
        pressAction.perform();
    } catch (_) {
        throw codedError('command_failed', 'AXPress outcome may be unknown', false);
    }
    return {
        ok: true,
        data: {
            application: current.identity,
            element: {role: matches[0].role, title: matches[0].title, bounds: matches[0].bounds}
        }
    };
}

function run(argv) {
    try {
        if (!argv || argv.length !== 2) {
            return JSON.stringify(failure('invalid_request', 'JXA expects an operation and one JSON payload', false));
        }
        const operation = argv[0];
        const request = JSON.parse(argv[1]);
        const systemEvents = Application('System Events');
        systemEvents.includeStandardAdditions = false;

        let result;
        if (operation === 'frontmost') {
            const target = authorizeFrontmost(systemEvents, request);
            result = {ok: true, data: target.identity};
        } else if (operation === 'list_apps') {
            result = listApplications(systemEvents, request);
        } else if (operation === 'inspect') {
            result = inspect(systemEvents, request);
        } else if (operation === 'press_element') {
            result = pressElement(systemEvents, request);
        } else {
            result = failure('invalid_request', 'unknown JXA operation', false);
        }
        return JSON.stringify(result);
    } catch (error) {
        const code = error && error.protocolCode ? error.protocolCode : 'accessibility_unavailable';
        const retryable = error && error.retryable !== undefined ? error.retryable : true;
        const message = error && error.message ? error.message : String(error);
        return JSON.stringify(failure(code, message, retryable));
    }
}
"#;

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
                Action::Capabilities {} => Ok(capabilities()),
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
                    let application = activate_application(policy, application, deadline).await?;
                    Ok(ResponseData::Focused { application })
                }
                Action::MouseMove {
                    x,
                    y,
                    expected_application,
                } => {
                    require_post_event_access()?;
                    let application = mouse_move(
                        policy,
                        expected_application,
                        Point { x: *x, y: *y },
                        deadline,
                    )
                    .await?;
                    Ok(ResponseData::Input { application })
                }
                Action::Click {
                    x,
                    y,
                    button,
                    expected_application,
                } => {
                    require_post_event_access()?;
                    let application = click(
                        policy,
                        expected_application,
                        Point { x: *x, y: *y },
                        *button,
                        deadline,
                    )
                    .await?;
                    Ok(ResponseData::Input { application })
                }
                Action::Scroll {
                    delta_x,
                    delta_y,
                    expected_application,
                } => {
                    require_post_event_access()?;
                    let application =
                        scroll(policy, expected_application, *delta_x, *delta_y, deadline).await?;
                    Ok(ResponseData::Input { application })
                }
                Action::TypeText {
                    text,
                    expected_application,
                } => {
                    require_post_event_access()?;
                    let application =
                        type_text(policy, expected_application, text, deadline).await?;
                    Ok(ResponseData::Input { application })
                }
                Action::KeyPress {
                    key,
                    modifiers,
                    expected_application,
                } => {
                    require_post_event_access()?;
                    let application =
                        key_press(policy, expected_application, *key, modifiers, deadline).await?;
                    Ok(ResponseData::Input { application })
                }
                Action::PressElement {
                    application,
                    role,
                    title,
                } => {
                    activate_application(policy, application, deadline).await?;
                    press_element(policy, application, role.as_deref(), title, deadline).await
                }
            }
        }
    }

    fn capabilities() -> ResponseData {
        let accessibility = if objc2_core_graphics::CGPreflightPostEventAccess() {
            PermissionState::Granted
        } else {
            PermissionState::Denied
        };
        let screen_recording = if ScreenCaptureAccess.preflight() {
            PermissionState::Granted
        } else {
            PermissionState::Denied
        };
        ResponseData::Capabilities {
            platform: Platform::Macos,
            actions: ActionKind::ALL.to_vec(),
            permissions: Permissions {
                accessibility,
                screen_recording,
            },
        }
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ApplicationListResult {
        applications: Vec<ApplicationIdentity>,
        truncated: bool,
    }

    async fn list_applications(
        policy: &Policy,
        deadline: tokio::time::Instant,
    ) -> Result<ResponseData, ProtocolError> {
        require_post_event_access()?;
        let payload = merge_json(
            frontmost_payload(policy, None),
            json!({"max_applications": MAX_RUNNING_APPLICATIONS}),
        )?;
        let listed: ApplicationListResult =
            run_jxa("list_apps", payload, deadline, OperationKind::Read).await?;
        if listed.applications.len() > MAX_RUNNING_APPLICATIONS {
            return Err(ProtocolError::new(
                ErrorCode::ProtocolViolation,
                "macOS returned too many running applications",
                false,
            ));
        }
        let mut seen_pids = std::collections::HashSet::new();
        for application in &listed.applications {
            validate_application(application, policy, None)?;
            if !seen_pids.insert(application.pid) {
                return Err(ProtocolError::new(
                    ErrorCode::ProtocolViolation,
                    "macOS returned duplicate running application identities",
                    false,
                ));
            }
        }
        Ok(ResponseData::Applications {
            applications: listed.applications,
            truncated: listed.truncated,
        })
    }

    async fn inspect(
        policy: &Policy,
        expected_application: &str,
        max_nodes: u32,
        max_depth: u32,
        deadline: tokio::time::Instant,
    ) -> Result<ResponseData, ProtocolError> {
        require_post_event_access()?;
        let payload = frontmost_payload(policy, Some(expected_application));
        let payload = merge_json(
            payload,
            json!({
                "max_nodes": max_nodes,
                "max_depth": max_depth,
                "output_chars": MAX_JXA_OUTPUT_CHARS,
            }),
        )?;
        let snapshot: AccessibilitySnapshot =
            run_jxa("inspect", payload, deadline, OperationKind::Read).await?;
        validate_snapshot(
            &snapshot,
            policy,
            Some(expected_application),
            max_nodes,
            max_depth,
        )?;
        Ok(ResponseData::Inspect { snapshot })
    }

    async fn press_element(
        policy: &Policy,
        application: &str,
        role: Option<&str>,
        title: &str,
        deadline: tokio::time::Instant,
    ) -> Result<ResponseData, ProtocolError> {
        require_post_event_access()?;
        let payload = frontmost_payload(policy, Some(application));
        let payload = merge_json(
            payload,
            json!({
                "max_nodes": MAX_AX_NODES,
                "max_depth": MAX_AX_DEPTH,
                "role": role,
                "title": title,
            }),
        )?;
        let pressed: PressResult =
            run_jxa("press_element", payload, deadline, OperationKind::Mutation).await?;
        validate_application(&pressed.application, policy, Some(application))
            .map_err(ProtocolError::with_unknown_outcome)?;
        validate_element_summary(&pressed.element).map_err(ProtocolError::with_unknown_outcome)?;
        Ok(ResponseData::ElementPressed {
            application: pressed.application,
            element: pressed.element,
        })
    }

    async fn resolve_frontmost(
        policy: &Policy,
        expected_application: Option<&str>,
        deadline: tokio::time::Instant,
    ) -> Result<ApplicationIdentity, ProtocolError> {
        let identity: ApplicationIdentity = run_jxa(
            "frontmost",
            frontmost_payload(policy, expected_application),
            deadline,
            OperationKind::Read,
        )
        .await?;
        validate_application(&identity, policy, expected_application)?;
        Ok(identity)
    }

    fn frontmost_payload(policy: &Policy, expected_application: Option<&str>) -> serde_json::Value {
        json!({
            "desktop_wide": policy.application_access == ComputerUseApplicationAccess::Desktop,
            "allowed_applications": policy.allowed_applications.iter().map(|selector| selector_payload(selector)).collect::<Vec<_>>(),
            "expected_application": expected_application.map(selector_payload),
            "string_limit": MAX_AX_STRING_CHARS,
        })
    }

    fn selector_payload(selector: &str) -> serde_json::Value {
        let kind = match application_selector_kind(selector) {
            ApplicationSelectorKind::BundleIdentifier => "bundle_identifier",
            ApplicationSelectorKind::DisplayName => "display_name",
        };
        json!({"kind": kind, "value": selector})
    }

    async fn activate_application(
        policy: &Policy,
        selector: &str,
        deadline: tokio::time::Instant,
    ) -> Result<ApplicationIdentity, ProtocolError> {
        let selector_kind = application_selector_kind(selector);
        let mut command = Command::new("/usr/bin/open");
        command
            .arg(
                if selector_kind == ApplicationSelectorKind::BundleIdentifier {
                    "-b"
                } else {
                    "-a"
                },
            )
            .arg(selector)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .env_clear();
        copy_ui_environment(&mut command);

        let output = run_status_capped(
            command,
            remaining(deadline, FOCUS_TIMEOUT)?,
            "open application",
            OperationKind::Mutation,
        )
        .await?;
        if !output.status.success() {
            return Err(ProtocolError::new(
                ErrorCode::ApplicationNotFound,
                format!(
                    "could not focus application: {}",
                    sanitized_external_bytes(&output.stderr)
                ),
                false,
            )
            .with_unknown_outcome());
        }

        loop {
            match resolve_frontmost(policy, Some(selector), deadline).await {
                Ok(application) => return Ok(application),
                Err(error)
                    if matches!(
                        error.code,
                        ErrorCode::ApplicationMismatch | ErrorCode::ApplicationNotFound
                    ) && tokio::time::Instant::now() < deadline =>
                {
                    let delay = remaining(deadline, FOCUS_POLL_INTERVAL)
                        .map_err(ProtocolError::with_unknown_outcome)?;
                    tokio::time::sleep(delay).await;
                }
                Err(error) => return Err(error.with_unknown_outcome()),
            }
        }
    }

    async fn screenshot(
        policy: &Policy,
        application: &str,
        reservation: &ScreenshotReservation,
        deadline: tokio::time::Instant,
    ) -> Result<ResponseData, ProtocolError> {
        if !ScreenCaptureAccess.preflight() {
            return Err(ProtocolError::new(
                ErrorCode::PermissionDenied,
                "screen-recording permission is not granted",
                false,
            ));
        }

        reservation.verify_path_identity().map_err(|error| {
            ProtocolError::new(
                ErrorCode::InvalidPath,
                sanitized_external_owned(format!(
                    "held screenshot destination is invalid: {error:#}"
                )),
                false,
            )
        })?;
        let display = display_metadata()?;
        let temporary_dir = tempfile::Builder::new()
            .prefix(".zeroclaw-computer-use-")
            .tempdir()
            .map_err(io_error)?;
        let directory_mode = temporary_dir
            .path()
            .metadata()
            .map_err(io_error)?
            .permissions()
            .mode();
        if directory_mode & 0o077 != 0 {
            return Err(ProtocolError::new(
                ErrorCode::InvalidPath,
                "private screenshot staging directory has unsafe permissions",
                false,
            ));
        }
        let capture_path = temporary_dir.path().join("capture.png");
        activate_application(policy, application, deadline).await?;
        resolve_frontmost(policy, Some(application), deadline).await?;
        crate::screenshot::capture_macos_main_display_to_path(
            &capture_path,
            remaining(deadline, JXA_TIMEOUT)?,
        )
        .await
        .map_err(|error| {
            ProtocolError::new(
                ErrorCode::ScreenCaptureUnavailable,
                sanitized_external_owned(format!(
                    "main-display screenshot outcome may be unknown: {error:#}"
                )),
                false,
            )
            .with_unknown_outcome()
        })?;
        let mut capture = open_captured_file(&capture_path)?;
        await_before_deadline(
            deadline,
            "copy screenshot to destination",
            OperationKind::Mutation,
            async {
                reservation
                    .replace_from_bounded_reader(
                        &mut capture.file,
                        capture.size,
                        MAX_SCREENSHOT_BYTES,
                    )
                    .await
                    .map(|_| ())
                    .map_err(|error| {
                        ProtocolError::new(
                            ErrorCode::InvalidPath,
                            sanitized_external_owned(format!(
                                "could not write held screenshot destination: {error:#}"
                            )),
                            false,
                        )
                        .with_unknown_outcome()
                    })
            },
        )
        .await?;

        Ok(ResponseData::Screenshot {
            path: reservation.path().to_path_buf(),
            display_bounds: display.display_bounds,
            pixel_width: display.pixel_width,
            pixel_height: display.pixel_height,
        })
    }

    struct DisplayMetadata {
        display_bounds: Rect,
        pixel_width: u64,
        pixel_height: u64,
    }

    fn display_metadata() -> Result<DisplayMetadata, ProtocolError> {
        let display = CGDisplay::main();
        let bounds = display.bounds();
        let display_bounds = Rect {
            x: bounds.origin.x,
            y: bounds.origin.y,
            width: bounds.size.width,
            height: bounds.size.height,
        };
        let pixel_width = display.pixels_wide();
        let pixel_height = display.pixels_high();
        if !display_bounds.x.is_finite()
            || !display_bounds.y.is_finite()
            || !display_bounds.width.is_finite()
            || !display_bounds.height.is_finite()
            || display_bounds.width <= 0.0
            || display_bounds.height <= 0.0
            || pixel_width == 0
            || pixel_height == 0
        {
            return Err(ProtocolError::new(
                ErrorCode::ScreenCaptureUnavailable,
                "main-display geometry is invalid",
                true,
            ));
        }
        let horizontal_scale = pixel_width as f64 / display_bounds.width;
        let vertical_scale = pixel_height as f64 / display_bounds.height;
        if !horizontal_scale.is_finite()
            || !vertical_scale.is_finite()
            || horizontal_scale <= 0.0
            || vertical_scale <= 0.0
            || (horizontal_scale - vertical_scale).abs() > 0.01
        {
            return Err(ProtocolError::new(
                ErrorCode::ScreenCaptureUnavailable,
                "main-display pixel scale is inconsistent",
                true,
            ));
        }
        Ok(DisplayMetadata {
            display_bounds,
            pixel_width,
            pixel_height,
        })
    }

    struct CapturedFile {
        file: tokio::fs::File,
        size: u64,
    }

    fn open_captured_file(path: &Path) -> Result<CapturedFile, ProtocolError> {
        let file = File::options()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(io_error)?;
        let metadata = file.metadata().map_err(io_error)?;
        let path_metadata = std::fs::metadata(path).map_err(io_error)?;
        if !metadata.file_type().is_file()
            || metadata.nlink() != 1
            || metadata.len() == 0
            || metadata.len() > MAX_SCREENSHOT_BYTES
            || path_metadata.dev() != metadata.dev()
            || path_metadata.ino() != metadata.ino()
            || path_metadata.nlink() != 1
        {
            return Err(ProtocolError::new(
                ErrorCode::ScreenCaptureUnavailable,
                "captured screenshot did not resolve to one bounded regular-file inode",
                false,
            ));
        }
        Ok(CapturedFile {
            file: tokio::fs::File::from_std(file),
            size: metadata.len(),
        })
    }

    fn require_post_event_access() -> Result<(), ProtocolError> {
        if objc2_core_graphics::CGPreflightPostEventAccess() {
            Ok(())
        } else {
            Err(ProtocolError::new(
                ErrorCode::PermissionDenied,
                "accessibility permission for input events is not granted",
                false,
            ))
        }
    }

    fn event_source() -> Result<CGEventSource, ProtocolError> {
        CGEventSource::new(CGEventSourceStateID::CombinedSessionState).map_err(|()| {
            ProtocolError::new(
                ErrorCode::EventCreationFailed,
                "CoreGraphics could not create an event source",
                true,
            )
        })
    }

    fn post_mouse(
        pid: u32,
        point: Point,
        event_type: CGEventType,
        button: MouseButton,
    ) -> Result<(), ProtocolError> {
        require_post_event_access()?;
        require_active_display(point)?;
        let source = event_source()?;
        let event = CGEvent::new_mouse_event(
            source,
            event_type,
            CGPoint::new(point.x, point.y),
            cg_button(button),
        )
        .map_err(|()| event_creation_error("mouse"))?;
        event.post_to_pid(pid_i32(pid)?);
        Ok(())
    }

    async fn mouse_move(
        policy: &Policy,
        expected: &str,
        point: Point,
        deadline: tokio::time::Instant,
    ) -> Result<ApplicationIdentity, ProtocolError> {
        activate_application(policy, expected, deadline).await?;
        let application = resolve_frontmost(policy, Some(expected), deadline).await?;
        require_window_owner(point, &application)?;
        post_mouse(
            application.pid,
            point,
            CGEventType::MouseMoved,
            MouseButton::Left,
        )?;
        let settle = remaining(deadline, Duration::from_millis(25))
            .map_err(ProtocolError::with_unknown_outcome)?;
        tokio::time::sleep(settle).await;
        let current = resolve_frontmost(policy, Some(expected), deadline)
            .await
            .map_err(ProtocolError::with_unknown_outcome)?;
        require_same_process(&application, &current)
            .map_err(ProtocolError::with_unknown_outcome)?;
        let actual = pointer_location().map_err(ProtocolError::with_unknown_outcome)?;
        if !points_are_close(point, actual) {
            return Err(ProtocolError::new(
                ErrorCode::CommandFailed,
                "application-targeted mouse move did not update the global pointer",
                false,
            )
            .with_unknown_outcome());
        }
        Ok(current)
    }

    async fn click(
        policy: &Policy,
        expected: &str,
        point: Point,
        button: MouseButton,
        deadline: tokio::time::Instant,
    ) -> Result<ApplicationIdentity, ProtocolError> {
        activate_application(policy, expected, deadline).await?;
        let (down, up) = mouse_event_types(button);
        let application = resolve_frontmost(policy, Some(expected), deadline).await?;
        require_window_owner(point, &application)?;
        post_mouse(application.pid, point, down, button)?;
        let mut release = MouseReleaseGuard::new(application.pid, point, button, up);

        let current = resolve_frontmost(policy, Some(expected), deadline)
            .await
            .map_err(ProtocolError::with_unknown_outcome)?;
        require_same_process(&application, &current)
            .map_err(ProtocolError::with_unknown_outcome)?;
        require_window_owner(point, &current).map_err(ProtocolError::with_unknown_outcome)?;
        release
            .release()
            .map_err(ProtocolError::with_unknown_outcome)?;
        Ok(current)
    }

    struct MouseReleaseGuard {
        pid: u32,
        point: Point,
        button: MouseButton,
        event_type: CGEventType,
        armed: bool,
    }

    impl MouseReleaseGuard {
        fn new(pid: u32, point: Point, button: MouseButton, event_type: CGEventType) -> Self {
            Self {
                pid,
                point,
                button,
                event_type,
                armed: true,
            }
        }

        fn release(&mut self) -> Result<(), ProtocolError> {
            post_mouse(self.pid, self.point, self.event_type, self.button)?;
            self.armed = false;
            Ok(())
        }
    }

    impl Drop for MouseReleaseGuard {
        fn drop(&mut self) {
            if self.armed {
                let _ = post_mouse(self.pid, self.point, self.event_type, self.button);
            }
        }
    }

    async fn scroll(
        policy: &Policy,
        expected: &str,
        delta_x: i32,
        delta_y: i32,
        deadline: tokio::time::Instant,
    ) -> Result<ApplicationIdentity, ProtocolError> {
        activate_application(policy, expected, deadline).await?;
        let application = resolve_frontmost(policy, Some(expected), deadline).await?;
        let point = pointer_location()?;
        policy.validate_coordinate(point)?;
        require_window_owner(point, &application)?;
        require_post_event_access()?;
        let source = event_source()?;
        let event =
            CGEvent::new_scroll_event(source, ScrollEventUnit::PIXEL, 2, delta_y, delta_x, 0)
                .map_err(|()| event_creation_error("scroll"))?;
        event.post_to_pid(pid_i32(application.pid)?);
        Ok(application)
    }

    async fn type_text(
        policy: &Policy,
        expected: &str,
        text: &str,
        deadline: tokio::time::Instant,
    ) -> Result<ApplicationIdentity, ProtocolError> {
        activate_application(policy, expected, deadline).await?;
        let application = resolve_frontmost(policy, Some(expected), deadline).await?;
        post_key_event(
            application.pid,
            0,
            CGEventFlags::CGEventFlagNull,
            true,
            Some(text),
        )?;
        let mut release = KeyReleaseGuard::new(application.pid, 0, CGEventFlags::CGEventFlagNull);

        let current = resolve_frontmost(policy, Some(expected), deadline)
            .await
            .map_err(ProtocolError::with_unknown_outcome)?;
        require_same_process(&application, &current)
            .map_err(ProtocolError::with_unknown_outcome)?;
        release
            .release()
            .map_err(ProtocolError::with_unknown_outcome)?;
        Ok(current)
    }

    async fn key_press(
        policy: &Policy,
        expected: &str,
        key: Key,
        modifiers: &[KeyModifier],
        deadline: tokio::time::Instant,
    ) -> Result<ApplicationIdentity, ProtocolError> {
        activate_application(policy, expected, deadline).await?;
        let application = resolve_frontmost(policy, Some(expected), deadline).await?;
        let flags = modifier_flags(modifiers);
        let code = key_code(key);
        post_key_event(application.pid, code, flags, true, None)?;
        let mut release = KeyReleaseGuard::new(application.pid, code, flags);

        let current = resolve_frontmost(policy, Some(expected), deadline)
            .await
            .map_err(ProtocolError::with_unknown_outcome)?;
        require_same_process(&application, &current)
            .map_err(ProtocolError::with_unknown_outcome)?;
        release
            .release()
            .map_err(ProtocolError::with_unknown_outcome)?;
        Ok(current)
    }

    struct KeyReleaseGuard {
        pid: u32,
        key_code: CGKeyCode,
        flags: CGEventFlags,
        armed: bool,
    }

    impl KeyReleaseGuard {
        fn new(pid: u32, key_code: CGKeyCode, flags: CGEventFlags) -> Self {
            Self {
                pid,
                key_code,
                flags,
                armed: true,
            }
        }

        fn release(&mut self) -> Result<(), ProtocolError> {
            post_key_event(self.pid, self.key_code, self.flags, false, None)?;
            self.armed = false;
            Ok(())
        }
    }

    impl Drop for KeyReleaseGuard {
        fn drop(&mut self) {
            if self.armed {
                let _ = post_key_event(self.pid, self.key_code, self.flags, false, None);
            }
        }
    }

    fn post_key_event(
        pid: u32,
        key_code: CGKeyCode,
        flags: CGEventFlags,
        key_down: bool,
        text: Option<&str>,
    ) -> Result<(), ProtocolError> {
        require_post_event_access()?;
        let pid = pid_i32(pid)?;
        let event = CGEvent::new_keyboard_event(event_source()?, key_code, key_down)
            .map_err(|()| event_creation_error("keyboard"))?;
        event.set_flags(flags);
        if key_down && let Some(text) = text {
            event.set_string(text);
        }
        event.post_to_pid(pid);
        Ok(())
    }

    fn pid_i32(pid: u32) -> Result<i32, ProtocolError> {
        i32::try_from(pid).map_err(|_| {
            ProtocolError::new(
                ErrorCode::ProtocolViolation,
                "application PID is outside the supported range",
                false,
            )
        })
    }

    fn pointer_location() -> Result<Point, ProtocolError> {
        let event = CGEvent::new(event_source()?).map_err(|()| event_creation_error("pointer"))?;
        let point = event.location();
        let point = Point {
            x: point.x,
            y: point.y,
        };
        require_active_display(point)?;
        Ok(point)
    }

    fn require_active_display(point: Point) -> Result<(), ProtocolError> {
        let count = CGDisplay::display_count_with_point(CGPoint::new(point.x, point.y)).map_err(
            |error| {
                ProtocolError::new(
                    ErrorCode::InvalidCoordinate,
                    format!("could not resolve display at coordinate: {error}"),
                    true,
                )
            },
        )?;
        if count == 0 {
            return Err(ProtocolError::new(
                ErrorCode::InvalidCoordinate,
                "coordinate is not on an active display",
                false,
            ));
        }
        Ok(())
    }

    fn require_window_owner(
        point: Point,
        application: &ApplicationIdentity,
    ) -> Result<(), ProtocolError> {
        require_active_display(point)?;
        let options = kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements;
        let windows = create_window_list(options, kCGNullWindowID).ok_or_else(|| {
            ProtocolError::new(
                ErrorCode::ApplicationMismatch,
                "macOS did not return an on-screen window list",
                true,
            )
        })?;
        let descriptions = create_description_from_array(windows).ok_or_else(|| {
            ProtocolError::new(
                ErrorCode::ApplicationMismatch,
                "macOS did not return on-screen window descriptions",
                true,
            )
        })?;

        for window in &descriptions {
            let Some(bounds) = window_bounds(&window) else {
                return Err(ProtocolError::new(
                    ErrorCode::ApplicationMismatch,
                    "an on-screen window has unknown bounds; refusing global input",
                    false,
                ));
            };
            if !rect_contains(bounds, point) {
                continue;
            }
            let alpha = dictionary_f64(&window, "kCGWindowAlpha").ok_or_else(|| {
                ProtocolError::new(
                    ErrorCode::ApplicationMismatch,
                    "topmost window alpha is unavailable; refusing global input",
                    false,
                )
            })?;
            if !alpha.is_finite() || alpha < 1.0 {
                return Err(ProtocolError::new(
                    ErrorCode::ApplicationMismatch,
                    "topmost window is transparent or translucent; refusing global input",
                    false,
                ));
            }
            let owner = dictionary_i64(&window, "kCGWindowOwnerPID")
                .and_then(|pid| u32::try_from(pid).ok())
                .ok_or_else(|| {
                    ProtocolError::new(
                        ErrorCode::ApplicationMismatch,
                        "topmost window owner is unavailable; refusing global input",
                        false,
                    )
                })?;
            if owner != application.pid {
                return Err(ProtocolError::new(
                    ErrorCode::ApplicationMismatch,
                    "topmost window at the input coordinate is not owned by the expected application",
                    true,
                ));
            }
            return Ok(());
        }

        Err(ProtocolError::new(
            ErrorCode::ApplicationMismatch,
            "no on-screen application window owns the input coordinate",
            false,
        ))
    }

    fn window_bounds(window: &CFDictionary<CFString, CFType>) -> Option<Rect> {
        let key = CFString::new("kCGWindowBounds");
        let bounds = window.find(key)?.downcast::<CFDictionary>()?;
        let bounds = CGRect::from_dict_representation(&bounds)?;
        let rect = Rect {
            x: bounds.origin.x,
            y: bounds.origin.y,
            width: bounds.size.width,
            height: bounds.size.height,
        };
        if rect.x.is_finite()
            && rect.y.is_finite()
            && rect.width.is_finite()
            && rect.height.is_finite()
            && rect.width >= 0.0
            && rect.height >= 0.0
        {
            Some(rect)
        } else {
            None
        }
    }

    fn dictionary_i64(dictionary: &CFDictionary<CFString, CFType>, key: &str) -> Option<i64> {
        dictionary
            .find(CFString::new(key))?
            .downcast::<CFNumber>()?
            .to_i64()
    }

    fn dictionary_f64(dictionary: &CFDictionary<CFString, CFType>, key: &str) -> Option<f64> {
        dictionary
            .find(CFString::new(key))?
            .downcast::<CFNumber>()?
            .to_f64()
    }

    fn rect_contains(rect: Rect, point: Point) -> bool {
        point.x >= rect.x
            && point.y >= rect.y
            && point.x < rect.x + rect.width
            && point.y < rect.y + rect.height
    }

    fn points_are_close(expected: Point, actual: Point) -> bool {
        (expected.x - actual.x).abs() <= 1.0 && (expected.y - actual.y).abs() <= 1.0
    }

    fn require_same_process(
        expected: &ApplicationIdentity,
        current: &ApplicationIdentity,
    ) -> Result<(), ProtocolError> {
        if expected.pid == current.pid
            && expected.name == current.name
            && expected.bundle_id == current.bundle_id
        {
            Ok(())
        } else {
            Err(ProtocolError::new(
                ErrorCode::ApplicationMismatch,
                "target application process changed during input",
                true,
            ))
        }
    }

    fn key_code(key: Key) -> CGKeyCode {
        match key {
            Key::A => KeyCode::ANSI_A,
            Key::B => KeyCode::ANSI_B,
            Key::C => KeyCode::ANSI_C,
            Key::D => KeyCode::ANSI_D,
            Key::E => KeyCode::ANSI_E,
            Key::F => KeyCode::ANSI_F,
            Key::G => KeyCode::ANSI_G,
            Key::H => KeyCode::ANSI_H,
            Key::I => KeyCode::ANSI_I,
            Key::J => KeyCode::ANSI_J,
            Key::K => KeyCode::ANSI_K,
            Key::L => KeyCode::ANSI_L,
            Key::M => KeyCode::ANSI_M,
            Key::N => KeyCode::ANSI_N,
            Key::O => KeyCode::ANSI_O,
            Key::P => KeyCode::ANSI_P,
            Key::Q => KeyCode::ANSI_Q,
            Key::R => KeyCode::ANSI_R,
            Key::S => KeyCode::ANSI_S,
            Key::T => KeyCode::ANSI_T,
            Key::U => KeyCode::ANSI_U,
            Key::V => KeyCode::ANSI_V,
            Key::W => KeyCode::ANSI_W,
            Key::X => KeyCode::ANSI_X,
            Key::Y => KeyCode::ANSI_Y,
            Key::Z => KeyCode::ANSI_Z,
            Key::Digit0 => KeyCode::ANSI_0,
            Key::Digit1 => KeyCode::ANSI_1,
            Key::Digit2 => KeyCode::ANSI_2,
            Key::Digit3 => KeyCode::ANSI_3,
            Key::Digit4 => KeyCode::ANSI_4,
            Key::Digit5 => KeyCode::ANSI_5,
            Key::Digit6 => KeyCode::ANSI_6,
            Key::Digit7 => KeyCode::ANSI_7,
            Key::Digit8 => KeyCode::ANSI_8,
            Key::Digit9 => KeyCode::ANSI_9,
            Key::Enter => KeyCode::RETURN,
            Key::Tab => KeyCode::TAB,
            Key::Space => KeyCode::SPACE,
            Key::Backspace => KeyCode::DELETE,
            Key::Delete => KeyCode::FORWARD_DELETE,
            Key::Escape => KeyCode::ESCAPE,
            Key::Home => KeyCode::HOME,
            Key::End => KeyCode::END,
            Key::PageUp => KeyCode::PAGE_UP,
            Key::PageDown => KeyCode::PAGE_DOWN,
            Key::LeftArrow => KeyCode::LEFT_ARROW,
            Key::RightArrow => KeyCode::RIGHT_ARROW,
            Key::UpArrow => KeyCode::UP_ARROW,
            Key::DownArrow => KeyCode::DOWN_ARROW,
            Key::F1 => KeyCode::F1,
            Key::F2 => KeyCode::F2,
            Key::F3 => KeyCode::F3,
            Key::F4 => KeyCode::F4,
            Key::F5 => KeyCode::F5,
            Key::F6 => KeyCode::F6,
            Key::F7 => KeyCode::F7,
            Key::F8 => KeyCode::F8,
            Key::F9 => KeyCode::F9,
            Key::F10 => KeyCode::F10,
            Key::F11 => KeyCode::F11,
            Key::F12 => KeyCode::F12,
        }
    }

    fn modifier_flags(modifiers: &[KeyModifier]) -> CGEventFlags {
        modifiers
            .iter()
            .fold(CGEventFlags::CGEventFlagNull, |flags, modifier| {
                flags
                    | match modifier {
                        KeyModifier::Command => CGEventFlags::CGEventFlagCommand,
                        KeyModifier::Control => CGEventFlags::CGEventFlagControl,
                        KeyModifier::Option => CGEventFlags::CGEventFlagAlternate,
                        KeyModifier::Shift => CGEventFlags::CGEventFlagShift,
                        KeyModifier::Function => CGEventFlags::CGEventFlagSecondaryFn,
                    }
            })
    }

    fn cg_button(button: MouseButton) -> CGMouseButton {
        match button {
            MouseButton::Left => CGMouseButton::Left,
            MouseButton::Right => CGMouseButton::Right,
            MouseButton::Middle => CGMouseButton::Center,
        }
    }

    fn mouse_event_types(button: MouseButton) -> (CGEventType, CGEventType) {
        match button {
            MouseButton::Left => (CGEventType::LeftMouseDown, CGEventType::LeftMouseUp),
            MouseButton::Right => (CGEventType::RightMouseDown, CGEventType::RightMouseUp),
            MouseButton::Middle => (CGEventType::OtherMouseDown, CGEventType::OtherMouseUp),
        }
    }

    fn event_creation_error(kind: &str) -> ProtocolError {
        ProtocolError::new(
            ErrorCode::EventCreationFailed,
            format!("CoreGraphics could not create a {kind} event"),
            true,
        )
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct JxaEnvelope<T> {
        ok: bool,
        data: Option<T>,
        error: Option<ProtocolError>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct PressResult {
        application: ApplicationIdentity,
        element: ElementSummary,
    }

    struct ProcessOutput {
        status: std::process::ExitStatus,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum OperationKind {
        Read,
        Mutation,
    }

    async fn run_jxa<T: DeserializeOwned>(
        operation: &str,
        payload: serde_json::Value,
        deadline: tokio::time::Instant,
        kind: OperationKind,
    ) -> Result<T, ProtocolError> {
        let payload = serde_json::to_string(&payload).map_err(|error| {
            ProtocolError::new(
                ErrorCode::ProtocolViolation,
                format!("could not encode JXA arguments: {error}"),
                false,
            )
        })?;
        let mut command = Command::new("/usr/bin/osascript");
        command
            .args(["-l", "JavaScript", "-e", MACOS_JXA, "--", operation])
            .arg(payload)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .env_clear();
        copy_ui_environment(&mut command);

        let output = run_command_capped(
            command,
            remaining(deadline, JXA_TIMEOUT)?,
            "JXA accessibility query",
            kind,
        )
        .await?;
        if !output.status.success() {
            return Err(mark_unknown_if_mutation(
                ProtocolError::new(
                    ErrorCode::CommandFailed,
                    format!(
                        "JXA failed with {}: {}",
                        output.status,
                        sanitized_external_bytes(&output.stderr)
                    ),
                    true,
                ),
                kind,
            ));
        }
        let envelope: JxaEnvelope<T> =
            serde_json::from_slice(trim_ascii(&output.stdout)).map_err(|error| {
                mark_unknown_if_mutation(
                    ProtocolError::new(
                        ErrorCode::ProtocolViolation,
                        format!("JXA returned invalid JSON: {error}"),
                        false,
                    ),
                    kind,
                )
            })?;
        match (envelope.ok, envelope.data, envelope.error) {
            (true, Some(data), None) => Ok(data),
            (false, None, Some(mut error)) => {
                sanitize_external_string(&mut error.message);
                if kind == OperationKind::Mutation && error.code == ErrorCode::CommandFailed {
                    Err(error.with_unknown_outcome())
                } else {
                    Err(error)
                }
            }
            _ => Err(mark_unknown_if_mutation(
                ProtocolError::new(
                    ErrorCode::ProtocolViolation,
                    "JXA returned an inconsistent success/error envelope",
                    false,
                ),
                kind,
            )),
        }
    }

    async fn run_status_capped(
        mut command: Command,
        timeout: Duration,
        label: &str,
        kind: OperationKind,
    ) -> Result<ProcessOutput, ProtocolError> {
        command.stdout(Stdio::null());
        let output = run_command_capped(command, timeout, label, kind).await?;
        Ok(output)
    }

    async fn run_command_capped(
        mut command: Command,
        timeout: Duration,
        label: &str,
        kind: OperationKind,
    ) -> Result<ProcessOutput, ProtocolError> {
        let mut child = command.spawn().map_err(|error| {
            ProtocolError::new(
                ErrorCode::CommandFailed,
                format!("could not start {label}: {error}"),
                true,
            )
        })?;
        let stdout = child.stdout.take();
        let Some(stderr) = child.stderr.take() else {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(mark_unknown_if_mutation(
                ProtocolError::new(
                    ErrorCode::CommandFailed,
                    format!("{label} stderr was not piped"),
                    false,
                ),
                kind,
            ));
        };

        let exchange = async {
            let read_stdout = async {
                match stdout {
                    Some(stdout) => read_capped(stdout, MAX_JXA_OUTPUT_BYTES).await,
                    None => Ok(CappedOutput::default()),
                }
            };
            tokio::try_join!(
                read_stdout,
                read_capped(stderr, MAX_PROCESS_STDERR_BYTES),
                child.wait()
            )
        };

        let result = tokio::time::timeout(timeout, exchange).await;
        let (stdout, stderr, status) = match result {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(mark_unknown_if_mutation(
                    ProtocolError::new(
                        ErrorCode::Io,
                        format!("I/O failure while running {label}: {error}"),
                        true,
                    ),
                    kind,
                ));
            }
            Err(_) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(mark_unknown_if_mutation(
                    ProtocolError::new(
                        ErrorCode::Timeout,
                        format!("{label} exceeded its timeout"),
                        true,
                    ),
                    kind,
                ));
            }
        };
        if stdout.exceeded || stderr.exceeded {
            return Err(mark_unknown_if_mutation(
                ProtocolError::new(
                    ErrorCode::OutputTooLarge,
                    format!("{label} exceeded its output limit"),
                    false,
                ),
                kind,
            ));
        }
        Ok(ProcessOutput {
            status,
            stdout: stdout.bytes,
            stderr: stderr.bytes,
        })
    }

    fn mark_unknown_if_mutation(error: ProtocolError, kind: OperationKind) -> ProtocolError {
        if kind == OperationKind::Mutation {
            error.with_unknown_outcome()
        } else {
            error
        }
    }

    fn remaining(
        deadline: tokio::time::Instant,
        maximum: Duration,
    ) -> Result<Duration, ProtocolError> {
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

    async fn await_before_deadline<T, F>(
        deadline: tokio::time::Instant,
        label: &str,
        kind: OperationKind,
        future: F,
    ) -> Result<T, ProtocolError>
    where
        F: std::future::Future<Output = Result<T, ProtocolError>>,
    {
        let timeout = remaining(deadline, Duration::MAX)?;
        match tokio::time::timeout(timeout, future).await {
            Ok(result) => result.map_err(|error| mark_unknown_if_mutation(error, kind)),
            Err(_) => Err(mark_unknown_if_mutation(
                ProtocolError::new(
                    ErrorCode::Timeout,
                    format!("{label} exceeded the computer-use deadline"),
                    false,
                ),
                kind,
            )),
        }
    }

    #[derive(Default)]
    struct CappedOutput {
        bytes: Vec<u8>,
        exceeded: bool,
    }

    async fn read_capped<R>(reader: R, limit: usize) -> std::io::Result<CappedOutput>
    where
        R: AsyncRead + Unpin,
    {
        let mut reader = reader.take(limit.saturating_add(1) as u64);
        let mut bytes = Vec::with_capacity(limit.min(8 * 1024));
        reader.read_to_end(&mut bytes).await?;
        let exceeded = bytes.len() > limit;
        if exceeded {
            bytes.truncate(limit);
        }
        Ok(CappedOutput { bytes, exceeded })
    }

    fn copy_ui_environment(command: &mut Command) {
        for name in [
            "HOME", "USER", "LOGNAME", "TMPDIR", "LANG", "LC_ALL", "LC_CTYPE",
        ] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
    }

    fn validate_application(
        application: &ApplicationIdentity,
        policy: &Policy,
        expected: Option<&str>,
    ) -> Result<(), ProtocolError> {
        if application.pid == 0
            || application.name.trim() != application.name
            || application.name.is_empty()
            || application.name.chars().count() > MAX_AX_STRING_CHARS
            || has_unsafe_external_characters(&application.name)
            || application.bundle_id.as_ref().is_some_and(|bundle_id| {
                bundle_id.trim() != bundle_id
                    || bundle_id.is_empty()
                    || bundle_id.chars().count() > MAX_AX_STRING_CHARS
                    || has_unsafe_external_characters(bundle_id)
            })
        {
            return Err(ProtocolError::new(
                ErrorCode::ProtocolViolation,
                "macOS returned an invalid application identity",
                false,
            ));
        }
        if !policy.allows(application) {
            return Err(ProtocolError::new(
                ErrorCode::ApplicationNotAllowed,
                "frontmost application is not admitted by the resolved policy",
                false,
            ));
        }
        if expected.is_some_and(|selector| !application.matches(selector)) {
            return Err(ProtocolError::new(
                ErrorCode::ApplicationMismatch,
                "frontmost application does not match expected_application",
                true,
            ));
        }
        Ok(())
    }

    fn validate_snapshot(
        snapshot: &AccessibilitySnapshot,
        policy: &Policy,
        expected: Option<&str>,
        max_nodes: u32,
        max_depth: u32,
    ) -> Result<(), ProtocolError> {
        validate_application(&snapshot.application, policy, expected)?;
        if snapshot.max_nodes != max_nodes
            || snapshot.max_depth != max_depth
            || snapshot.nodes.is_empty()
            || snapshot.nodes.len() > max_nodes as usize
        {
            return Err(ProtocolError::new(
                ErrorCode::ProtocolViolation,
                "accessibility snapshot limits do not match the request",
                false,
            ));
        }
        for (index, node) in snapshot.nodes.iter().enumerate() {
            let expected_id = u32::try_from(index)
                .ok()
                .and_then(|value| value.checked_add(1))
                .ok_or_else(|| {
                    ProtocolError::new(
                        ErrorCode::ProtocolViolation,
                        "accessibility node identifier overflow",
                        false,
                    )
                })?;
            let parent_is_valid = if index == 0 {
                node.parent_id.is_none() && node.depth == 0
            } else {
                node.parent_id
                    .and_then(|parent| parent.checked_sub(1))
                    .and_then(|parent| snapshot.nodes.get(parent as usize))
                    .is_some_and(|parent| parent.depth.checked_add(1) == Some(node.depth))
            };
            if node.id != expected_id
                || !parent_is_valid
                || node.depth > max_depth
                || node.parent_id.is_some_and(|parent| parent >= node.id)
                || node.role.is_empty()
                || node.role.chars().count() > MAX_AX_STRING_CHARS
                || has_unsafe_external_characters(&node.role)
                || node.title.as_ref().is_some_and(|value| {
                    value.chars().count() > MAX_AX_STRING_CHARS
                        || has_unsafe_external_characters(value)
                })
                || node.value.as_ref().is_some_and(|value| {
                    value.chars().count() > MAX_AX_STRING_CHARS
                        || has_unsafe_external_characters(value)
                })
                || node.description.as_ref().is_some_and(|value| {
                    value.chars().count() > MAX_AX_STRING_CHARS
                        || has_unsafe_external_characters(value)
                })
                || node.actions.len() > 32
                || node.actions.iter().any(|action| {
                    action.chars().count() > MAX_AX_STRING_CHARS
                        || has_unsafe_external_characters(action)
                })
            {
                return Err(ProtocolError::new(
                    ErrorCode::ProtocolViolation,
                    "accessibility snapshot contains an invalid node",
                    false,
                ));
            }
            validate_optional_rect(node.bounds)?;
        }
        Ok(())
    }

    fn validate_element_summary(element: &ElementSummary) -> Result<(), ProtocolError> {
        if element.role.is_empty()
            || element.role.chars().count() > MAX_AX_STRING_CHARS
            || has_unsafe_external_characters(&element.role)
            || element.title.as_ref().is_some_and(|title| {
                title.chars().count() > MAX_AX_STRING_CHARS || has_unsafe_external_characters(title)
            })
        {
            return Err(ProtocolError::new(
                ErrorCode::ProtocolViolation,
                "pressed element summary is invalid",
                false,
            ));
        }
        validate_optional_rect(element.bounds)
    }

    fn validate_optional_rect(rect: Option<Rect>) -> Result<(), ProtocolError> {
        if rect.is_some_and(|rect| {
            !rect.x.is_finite()
                || !rect.y.is_finite()
                || !rect.width.is_finite()
                || !rect.height.is_finite()
                || rect.width < 0.0
                || rect.height < 0.0
        }) {
            return Err(ProtocolError::new(
                ErrorCode::ProtocolViolation,
                "accessibility element has invalid bounds",
                false,
            ));
        }
        Ok(())
    }

    fn merge_json(
        mut base: serde_json::Value,
        extra: serde_json::Value,
    ) -> Result<serde_json::Value, ProtocolError> {
        let Some(base) = base.as_object_mut() else {
            return Err(ProtocolError::new(
                ErrorCode::ProtocolViolation,
                "internal JXA payload is not an object",
                false,
            ));
        };
        let Some(extra) = extra.as_object() else {
            return Err(ProtocolError::new(
                ErrorCode::ProtocolViolation,
                "internal JXA extension is not an object",
                false,
            ));
        };
        base.extend(extra.clone());
        Ok(serde_json::Value::Object(base.clone()))
    }

    fn trim_ascii(bytes: &[u8]) -> &[u8] {
        let start = bytes
            .iter()
            .position(|byte| !byte.is_ascii_whitespace())
            .unwrap_or(bytes.len());
        let end = bytes
            .iter()
            .rposition(|byte| !byte.is_ascii_whitespace())
            .map_or(start, |index| index + 1);
        &bytes[start..end]
    }

    fn io_error(error: std::io::Error) -> ProtocolError {
        ProtocolError::new(ErrorCode::Io, format!("filesystem error: {error}"), true)
    }

    fn has_unsafe_external_characters(value: &str) -> bool {
        value
            .chars()
            .any(zeroclaw_api::tool::is_unsafe_confirmation_character)
    }

    fn sanitize_external_string(value: &mut String) {
        if has_unsafe_external_characters(value) {
            *value = value
                .chars()
                .map(|character| {
                    if zeroclaw_api::tool::is_unsafe_confirmation_character(character) {
                        '\u{fffd}'
                    } else {
                        character
                    }
                })
                .collect();
        }
    }

    fn sanitized_external_owned(mut value: String) -> String {
        sanitize_external_string(&mut value);
        value
    }

    fn sanitized_external_bytes(value: &[u8]) -> String {
        sanitized_external_owned(String::from_utf8_lossy(value).trim().to_owned())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn key_mapping_is_exhaustive_for_protocol_table() {
            let keycodes: Vec<_> = Key::ALL.iter().copied().map(key_code).collect();
            assert_eq!(keycodes.len(), Key::ALL.len());
            assert_eq!(key_code(Key::Enter), KeyCode::RETURN);
            assert_eq!(key_code(Key::Delete), KeyCode::FORWARD_DELETE);
        }

        #[test]
        fn pointer_postcondition_allows_only_rounding_tolerance() {
            assert!(points_are_close(
                Point { x: 10.25, y: 20.75 },
                Point { x: 11.0, y: 20.0 }
            ));
            assert!(!points_are_close(
                Point { x: 10.0, y: 20.0 },
                Point { x: 11.01, y: 20.0 }
            ));
        }

        #[test]
        fn untrusted_values_are_not_part_of_static_jxa_source() {
            assert!(!MACOS_JXA.contains("com.example.Untrusted"));
            assert!(MACOS_JXA.contains("argv[1]"));
            assert!(MACOS_JXA.contains("listApplications"));
            assert!(MACOS_JXA.contains("pressAction.perform()"));
        }

        #[test]
        fn desktop_policy_is_explicit_in_jxa_payload_without_a_wildcard() {
            let policy = Policy {
                application_access: ComputerUseApplicationAccess::Desktop,
                allowed_applications: Vec::new(),
                min_coordinate_x: None,
                min_coordinate_y: None,
                max_coordinate_x: None,
                max_coordinate_y: None,
                max_text_chars: 1,
            };
            let payload = frontmost_payload(&policy, Some("com.example.Editor"));
            assert_eq!(payload["desktop_wide"], true);
            assert_eq!(payload["allowed_applications"], json!([]));
            assert_eq!(
                payload["expected_application"],
                json!({"kind": "bundle_identifier", "value": "com.example.Editor"})
            );
        }

        #[test]
        fn jxa_envelope_shape_is_strict() {
            let decoded: JxaEnvelope<ApplicationIdentity> = serde_json::from_str(
                r#"{"ok":true,"data":{"name":"Editor","bundle_id":"com.example.Editor","pid":42}}"#,
            )
            .expect("test envelope decodes");
            assert!(decoded.ok);
            assert!(decoded.error.is_none());
            assert_eq!(decoded.data.expect("test data exists").pid, 42);
        }
    }
}

#[cfg(all(
    test,
    not(any(target_os = "macos", target_os = "linux", target_os = "windows"))
))]
mod tests {
    use super::*;
    use crate::computer_use::protocol::{Action, PROTOCOL_VERSION, Policy};
    use uuid::Uuid;

    #[tokio::test]
    async fn unsupported_platform_fails_closed() {
        let request = Request {
            version: PROTOCOL_VERSION,
            request_id: Uuid::new_v4(),
            action: Action::Capabilities {},
            policy: Policy {
                application_access:
                    zeroclaw_config::schema::ComputerUseApplicationAccess::Allowlist,
                allowed_applications: Vec::new(),
                min_coordinate_x: None,
                min_coordinate_y: None,
                max_coordinate_x: None,
                max_coordinate_y: None,
                max_text_chars: 1,
            },
        };
        let response = execute(request, None, Duration::from_secs(1)).await;
        assert!(!response.ok);
        assert_eq!(
            response.error.map(|error| error.code),
            Some(ErrorCode::UnsupportedPlatform)
        );
    }
}
