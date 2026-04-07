//! Common channel routing framework for messaging platforms.
//!
//! Routes messages from external channels (KakaoTalk, Telegram, Slack, etc.)
//! through the Railway relay to the user's specific MoA device.
//!
//! All user-facing interactions use **buttons** (not slash commands) so that
//! non-technical users can navigate with simple taps.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use uuid::Uuid;

use super::remote::{DeviceRouter, RoutedMessage, REMOTE_RESPONSE_CHANNELS};
use crate::auth::store::{AuthStore, ChannelLink};

// ── Structured Reply ────────────────────────────────────────────────

/// A button that can be rendered in any channel's native format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplyButton {
    /// Display label shown on the button.
    pub label: String,
    /// Action performed when clicked.
    pub action: ButtonAction,
}

/// What happens when a button is tapped.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ButtonAction {
    /// Send this text back as a message (KakaoTalk quickReply / Telegram callback).
    /// The `callback_data` is a compact token that `handle_channel_command` recognises.
    PostBack(String),
    /// Open a URL in the browser.
    WebLink(String),
}

/// A structured reply that channels render in their native button format.
#[derive(Debug, Clone)]
pub struct ChannelReply {
    /// Main text body.
    pub text: String,
    /// Optional buttons displayed below the text.
    pub buttons: Vec<ReplyButton>,
}

impl ChannelReply {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            buttons: vec![],
        }
    }

    pub fn with_buttons(text: impl Into<String>, buttons: Vec<ReplyButton>) -> Self {
        Self {
            text: text.into(),
            buttons,
        }
    }

    /// Convenience: plain-text fallback (for channels that don't support buttons).
    pub fn as_plain_text(&self) -> String {
        if self.buttons.is_empty() {
            return self.text.clone();
        }
        let mut out = self.text.clone();
        out.push_str("\n\n");
        for btn in &self.buttons {
            match &btn.action {
                ButtonAction::PostBack(data) => {
                    out.push_str(&format!("• {} (입력: {})\n", btn.label, data));
                }
                ButtonAction::WebLink(url) => {
                    out.push_str(&format!("• {} → {}\n", btn.label, url));
                }
            }
        }
        out
    }
}

fn postback(label: impl Into<String>, data: impl Into<String>) -> ReplyButton {
    ReplyButton {
        label: label.into(),
        action: ButtonAction::PostBack(data.into()),
    }
}

fn weblink(label: impl Into<String>, url: impl Into<String>) -> ReplyButton {
    ReplyButton {
        label: label.into(),
        action: ButtonAction::WebLink(url.into()),
    }
}

// ── Callback data prefixes ──────────────────────────────────────────
// Compact tokens that fit in Telegram's 64-byte callback_data limit.

pub const CB_DEVICE_SELECT: &str = "moa:dev:"; // moa:dev:1, moa:dev:2
pub const CB_MODE_FULL: &str = "moa:mode:full";
pub const CB_MODE_READONLY: &str = "moa:mode:ro";
pub const CB_UNLINK: &str = "moa:unlink";
pub const CB_HELP: &str = "moa:help";
pub const CB_DEVICE_LIST: &str = "moa:devlist";
pub const CB_SETTINGS: &str = "moa:settings";

// ── Route Result ────────────────────────────────────────────────────

/// Result of routing a channel message to a device.
#[derive(Debug)]
pub enum RouteResult {
    /// Message successfully delivered; response will arrive asynchronously.
    Delivered {
        msg_id: String,
        response_rx: mpsc::Receiver<RoutedMessage>,
    },
    /// User not linked — needs onboarding.
    NotLinked,
    /// User linked but no device selected yet.
    NoDeviceSelected { link: ChannelLink },
    /// Device is offline.
    DeviceOffline {
        device_id: String,
        device_name: Option<String>,
    },
}

/// Collects chunked responses from device.
pub struct ResponseCollector {
    pub rx: mpsc::Receiver<RoutedMessage>,
    pub msg_id: String,
}

impl ResponseCollector {
    pub async fn collect(mut self, timeout: Duration) -> ChannelReply {
        let mut full_response = String::new();

        let result = tokio::time::timeout(timeout, async {
            while let Some(msg) = self.rx.recv().await {
                match msg.msg_type.as_str() {
                    "done" | "remote_response" => {
                        if !msg.content.is_empty() {
                            full_response = msg.content;
                        }
                        break;
                    }
                    "chunk" | "remote_chunk" => {
                        full_response.push_str(&msg.content);
                    }
                    "error" | "remote_error" => {
                        full_response = msg.content;
                        break;
                    }
                    _ => {
                        full_response.push_str(&msg.content);
                    }
                }
            }
        })
        .await;

        REMOTE_RESPONSE_CHANNELS.lock().remove(&self.msg_id);

        if result.is_err() {
            if full_response.is_empty() {
                return ChannelReply::text(
                    "MoA 디바이스가 응답하는 데 시간이 오래 걸리고 있습니다. 잠시 후 다시 시도해 주세요.",
                );
            }
            full_response.push_str("\n\n(응답이 길어 일부만 전달되었습니다)");
        }

        // Append a settings button to every AI response so users can always
        // access mode/device controls without knowing slash commands.
        ChannelReply::with_buttons(full_response, vec![postback("⚙️ 설정", CB_SETTINGS)])
    }
}

// ── Core Routing ────────────────────────────────────────────────────

/// Route a channel message to the user's MoA device.
pub async fn route_channel_message(
    auth_store: &AuthStore,
    device_router: &DeviceRouter,
    channel: &str,
    platform_uid: &str,
    content: &str,
) -> RouteResult {
    let link = match auth_store.find_channel_link_full(channel, platform_uid) {
        Ok(Some(link)) => link,
        Ok(None) => return RouteResult::NotLinked,
        Err(e) => {
            tracing::error!(channel, platform_uid, "Channel link lookup: {e}");
            return RouteResult::NotLinked;
        }
    };

    let device_id = match &link.device_id {
        Some(id) if !id.is_empty() => id.clone(),
        _ => return RouteResult::NoDeviceSelected { link },
    };

    if !device_router.is_device_online(&device_id) {
        let device_name = auth_store.list_devices(&link.user_id).ok().and_then(|ds| {
            ds.into_iter()
                .find(|d| d.device_id == device_id)
                .map(|d| d.device_name)
        });
        return RouteResult::DeviceOffline {
            device_id,
            device_name,
        };
    }

    let msg_id = Uuid::new_v4().to_string();

    // Use "channel_relay" type which device_link already handles.
    // The device processes it via local gateway → agent loop → memory store.
    // Autonomy mode is embedded in the payload so the local gateway can enforce it.
    let routed = RoutedMessage {
        id: msg_id.clone(),
        direction: "to_device".into(),
        content: serde_json::json!({
            "content": content,
            "channel": channel,
            "autonomy_mode": &link.autonomy_mode,
        })
        .to_string(),
        msg_type: "channel_relay".into(),
    };

    let (resp_tx, resp_rx) = mpsc::channel::<RoutedMessage>(64);
    REMOTE_RESPONSE_CHANNELS
        .lock()
        .insert(msg_id.clone(), resp_tx);

    if let Err(err) = device_router.send_to_device(&device_id, routed).await {
        tracing::warn!(device_id, "Channel → device send failed: {err}");
        REMOTE_RESPONSE_CHANNELS.lock().remove(&msg_id);
        return RouteResult::DeviceOffline {
            device_id,
            device_name: None,
        };
    }

    RouteResult::Delivered {
        msg_id,
        response_rx: resp_rx,
    }
}

// ── Button-Based Command Handling ───────────────────────────────────

/// Handle button callbacks and text commands.
///
/// Returns `Some(ChannelReply)` if the input was a command/callback,
/// `None` if it's a regular message that should go to the AI.
pub fn handle_channel_command(
    auth_store: &AuthStore,
    device_router: &DeviceRouter,
    channel: &str,
    platform_uid: &str,
    message: &str,
) -> Option<ChannelReply> {
    let trimmed = message.trim();

    // ── Settings menu ──
    if trimmed == CB_SETTINGS || trimmed == "/설정" || trimmed.eq_ignore_ascii_case("/settings") {
        return Some(settings_menu(auth_store, channel, platform_uid));
    }

    // ── Device list ──
    if trimmed == CB_DEVICE_LIST
        || trimmed == "/디바이스"
        || trimmed.eq_ignore_ascii_case("/device")
    {
        return Some(device_list_reply(
            auth_store,
            device_router,
            channel,
            platform_uid,
        ));
    }

    // ── Device selection by callback: moa:dev:1 ──
    if let Some(num_str) = trimmed.strip_prefix(CB_DEVICE_SELECT) {
        return Some(select_device(auth_store, channel, platform_uid, num_str));
    }
    // Legacy slash: /디바이스 1
    if let Some(num_str) = trimmed.strip_prefix("/디바이스 ") {
        return Some(select_device(auth_store, channel, platform_uid, num_str));
    }

    // ── Mode switches ──
    if trimmed == CB_MODE_FULL || trimmed == "/모드 전체" {
        let _ = auth_store.set_channel_autonomy_mode(channel, platform_uid, "full");
        return Some(ChannelReply::with_buttons(
            "🔓 전체 모드로 전환되었습니다.\n\nMoA가 파일 작성, 명령 실행 등 모든 기능을 사용할 수 있습니다.\n대화 내용은 계속 기억에 저장됩니다.",
            vec![
                postback("🔒 안전 모드로 되돌리기", CB_MODE_READONLY),
                postback("⚙️ 설정", CB_SETTINGS),
            ],
        ));
    }
    if trimmed == CB_MODE_READONLY || trimmed == "/모드 읽기전용" || trimmed == "/모드 안전"
    {
        let _ = auth_store.set_channel_autonomy_mode(channel, platform_uid, "read_only");
        return Some(ChannelReply::with_buttons(
            "🔒 안전 모드로 전환되었습니다.\n\n대화 내용은 계속 기억에 저장됩니다.\n검색, 기억 조회가 가능하며, 파일 수정과 명령 실행은 제한됩니다.",
            vec![
                postback("🔓 전체 모드로 전환", CB_MODE_FULL),
                postback("⚙️ 설정", CB_SETTINGS),
            ],
        ));
    }

    // ── Unlink ──
    if trimmed == CB_UNLINK || trimmed == "/연결해제" {
        let _ = auth_store.unlink_channel(channel, platform_uid);
        return Some(ChannelReply::text(
            "연결이 해제되었습니다.\n다시 연결하려면 아무 메시지를 보내주세요.",
        ));
    }

    // ── Help ──
    if trimmed == CB_HELP || trimmed == "/도움말" || trimmed.eq_ignore_ascii_case("/help") {
        return Some(ChannelReply::with_buttons(
            "📋 MoA 도움말\n\n\
             아래 버튼으로 설정을 변경할 수 있습니다.\n\
             일반 메시지를 보내면 AI가 답변합니다.",
            vec![
                postback("⚙️ 설정", CB_SETTINGS),
                postback("📱 디바이스 변경", CB_DEVICE_LIST),
            ],
        ));
    }

    None
}

/// Build the settings menu with buttons.
fn settings_menu(auth_store: &AuthStore, channel: &str, platform_uid: &str) -> ChannelReply {
    let current_mode = auth_store
        .find_channel_link_full(channel, platform_uid)
        .ok()
        .flatten()
        .map(|l| l.autonomy_mode)
        .unwrap_or_else(|| "read_only".into());

    let mode_label = if current_mode == "full" {
        "🔓 전체 모드"
    } else {
        "🔒 안전 모드"
    };

    let mode_toggle = if current_mode == "full" {
        postback("🔒 안전 모드로 전환", CB_MODE_READONLY)
    } else {
        postback("🔓 전체 모드로 전환", CB_MODE_FULL)
    };

    ChannelReply::with_buttons(
        format!(
            "⚙️ MoA 설정\n\n\
             현재 모드: {mode_label}\n\
             • 안전 모드: 대화, 검색, 기억 저장/조회 (파일 수정·명령 실행 제한)\n\
             • 전체 모드: 파일 작성, 명령 실행 등 모든 기능"
        ),
        vec![
            mode_toggle,
            postback("📱 디바이스 변경", CB_DEVICE_LIST),
            postback("🔗 연결 해제", CB_UNLINK),
            postback("❓ 도움말", CB_HELP),
        ],
    )
}

/// Build device list reply with selection buttons.
fn device_list_reply(
    auth_store: &AuthStore,
    device_router: &DeviceRouter,
    channel: &str,
    platform_uid: &str,
) -> ChannelReply {
    let link =
        match auth_store
            .find_channel_link_full(channel, platform_uid)
            .ok()
            .flatten()
        {
            Some(l) => l,
            None => return ChannelReply::text(
                "MoA 계정이 연결되어 있지 않습니다.\n아무 메시지를 보내면 연결 안내가 표시됩니다.",
            ),
        };
    let devices = match auth_store.list_devices(&link.user_id).ok() {
        Some(d) => d,
        None => return ChannelReply::text("디바이스 목록을 확인할 수 없습니다."),
    };
    if devices.is_empty() {
        return ChannelReply::text("등록된 디바이스가 없습니다.\nMoA 앱을 설치해 주세요.");
    }
    if devices.len() == 1 {
        return ChannelReply::with_buttons(
            format!(
                "현재 연결된 디바이스: {}\n디바이스가 1대뿐이므로 변경할 수 없습니다.",
                devices[0].device_name
            ),
            vec![postback("⚙️ 설정으로 돌아가기", CB_SETTINGS)],
        );
    }

    let mut text = "📱 디바이스를 선택하세요\n".to_string();
    let mut buttons = Vec::new();
    for (i, d) in devices.iter().enumerate() {
        let online = if device_router.is_device_online(&d.device_id) {
            "🟢"
        } else {
            "⚪"
        };
        let current = if link.device_id.as_deref() == Some(&d.device_id) {
            " ✓"
        } else {
            ""
        };
        text.push_str(&format!(
            "\n{} {} {}{}",
            i + 1,
            online,
            d.device_name,
            current
        ));
        buttons.push(postback(
            format!("{} {}", online, d.device_name),
            format!("{}{}", CB_DEVICE_SELECT, i + 1),
        ));
    }
    ChannelReply::with_buttons(text, buttons)
}

/// Handle device selection (by number).
fn select_device(
    auth_store: &AuthStore,
    channel: &str,
    platform_uid: &str,
    num_str: &str,
) -> ChannelReply {
    let num: usize = match num_str.trim().parse() {
        Ok(n) => n,
        Err(_) => return ChannelReply::text("올바른 번호를 입력해 주세요."),
    };
    let link = match auth_store
        .find_channel_link_full(channel, platform_uid)
        .ok()
        .flatten()
    {
        Some(l) => l,
        None => return ChannelReply::text("연결 정보를 찾을 수 없습니다."),
    };
    let devices = match auth_store.list_devices(&link.user_id).ok() {
        Some(d) => d,
        None => return ChannelReply::text("디바이스 목록을 확인할 수 없습니다."),
    };
    if num == 0 || num > devices.len() {
        return ChannelReply::text("올바른 번호를 입력해 주세요.");
    }
    let target = &devices[num - 1];
    let _ = auth_store.update_channel_device(channel, platform_uid, &target.device_id);
    ChannelReply::with_buttons(
        format!(
            "✅ '{}'(으)로 연결되었습니다.\n이제 메시지를 보내면 이 디바이스의 MoA가 답변합니다.",
            target.device_name
        ),
        vec![postback("⚙️ 설정", CB_SETTINGS)],
    )
}

// ── Onboarding Messages ─────────────────────────────────────────────

/// Generate the onboarding auth URL.
pub fn build_onboarding_url(gateway_url: &str, channel: &str, platform_uid: &str) -> String {
    let encoded_uid = urlencoding::encode(platform_uid);
    format!("{gateway_url}/auth?channel_link={channel}&platform_uid={encoded_uid}")
}

/// Reply for first-time users who need to link.
pub fn onboarding_reply(auth_url: &str) -> ChannelReply {
    ChannelReply::with_buttons(
        "MoA에 오신 것을 환영합니다! 🎉\n\n\
         MoA 계정과 연결하면 바로 사용할 수 있습니다.\n\
         아래 버튼을 눌러 로그인해 주세요.",
        vec![weblink("🔗 MoA 계정 연결하기", auth_url)],
    )
}

/// Reply for multiple devices — present selection buttons.
pub fn device_selection_reply(
    devices: &[crate::auth::store::Device],
    device_router: &DeviceRouter,
) -> ChannelReply {
    let mut text =
        "MoA 앱이 여러 디바이스에 설치되어 있습니다.\n어떤 디바이스와 대화할까요?\n".to_string();
    let mut buttons = Vec::new();
    for (i, d) in devices.iter().enumerate() {
        let online = if device_router.is_device_online(&d.device_id) {
            "🟢"
        } else {
            "⚪"
        };
        text.push_str(&format!("\n{} {} — {}", i + 1, d.device_name, online));
        buttons.push(postback(
            format!("{} {}", online, d.device_name),
            format!("{}{}", CB_DEVICE_SELECT, i + 1),
        ));
    }
    ChannelReply::with_buttons(text, buttons)
}

/// Friendly message when device is offline.
pub fn device_offline_reply(device_name: Option<&str>) -> ChannelReply {
    let text = match device_name {
        Some(name) => format!(
            "'{name}' 디바이스에 연결할 수 없습니다.\n\n\
             디바이스가 꺼져 있거나 인터넷에 연결되어 있지 않을 수 있습니다.\n\
             MoA 앱이 실행 중인지 확인해 주세요."
        ),
        None => "디바이스에 연결할 수 없습니다.\n\n\
                 디바이스가 꺼져 있거나 인터넷에 연결되어 있지 않을 수 있습니다.\n\
                 MoA 앱이 실행 중인지 확인해 주세요."
            .into(),
    };
    ChannelReply::with_buttons(
        text,
        vec![
            postback("📱 다른 디바이스 선택", CB_DEVICE_LIST),
            postback("⚙️ 설정", CB_SETTINGS),
        ],
    )
}
