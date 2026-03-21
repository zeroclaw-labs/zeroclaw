use crate::channels::traits::{Channel, ChannelMessage, SendMessage};
use crate::config::{TranscriptionConfig, TtsConfig};
use async_trait::async_trait;
use matrix_sdk::{
    authentication::matrix::MatrixSession,
    config::SyncSettings,
    ruma::{
        api::client::receipt::create_receipt,
        events::reaction::ReactionEventContent,
        events::receipt::ReceiptThread,
        events::relation::{Annotation, Thread},
        events::room::message::{
            MessageType, OriginalSyncRoomMessageEvent, Relation, RoomMessageEventContent,
        },
        events::room::MediaSource,
        OwnedEventId, OwnedRoomId, OwnedUserId,
    },
    Client as MatrixSdkClient, LoopCtrl, Room, RoomState, SessionMeta, SessionTokens,
};
use reqwest::Client;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, OnceCell, RwLock};

/// Matrix channel for Matrix Client-Server API.
/// Uses matrix-sdk for reliable sync and encrypted-room decryption.
#[derive(Clone)]
pub struct MatrixChannel {
    homeserver: String,
    access_token: String,
    room_id: String,
    allowed_users: Vec<String>,
    session_owner_hint: Option<String>,
    session_device_id_hint: Option<String>,
    zeroclaw_dir: Option<PathBuf>,
    resolved_room_id_cache: Arc<RwLock<Option<String>>>,
    sdk_client: Arc<OnceCell<MatrixSdkClient>>,
    http_client: Client,
    reaction_events: Arc<RwLock<HashMap<String, String>>>,
    voice_mode: Arc<AtomicBool>,
    transcription_config: Option<TranscriptionConfig>,
    tts_config: Option<TtsConfig>,
    tts_api_url: Option<String>,
    last_draft_edit: Arc<Mutex<HashMap<String, std::time::Instant>>>,
    /// Tracks the current visible event ID for delete-and-resend draft updates.
    draft_current_event: Arc<Mutex<Option<String>>>,
    /// Additional rooms to listen on (from channel_workspaces config).
    /// The configured `room_id` is always included implicitly.
    allowed_rooms: HashSet<String>,
}

impl std::fmt::Debug for MatrixChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MatrixChannel")
            .field("homeserver", &self.homeserver)
            .field("room_id", &self.room_id)
            .field("allowed_users", &self.allowed_users)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Deserialize)]
struct SyncResponse {
    next_batch: String,
    #[serde(default)]
    rooms: Rooms,
}

#[derive(Debug, Deserialize, Default)]
struct Rooms {
    #[serde(default)]
    join: std::collections::HashMap<String, JoinedRoom>,
}

#[derive(Debug, Deserialize)]
struct JoinedRoom {
    #[serde(default)]
    timeline: Timeline,
}

#[derive(Debug, Deserialize, Default)]
struct Timeline {
    #[serde(default)]
    events: Vec<TimelineEvent>,
}

#[derive(Debug, Deserialize)]
struct TimelineEvent {
    #[serde(rename = "type")]
    event_type: String,
    sender: String,
    #[serde(default)]
    event_id: Option<String>,
    #[serde(default)]
    content: EventContent,
}

#[derive(Debug, Deserialize, Default)]
struct EventContent {
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    msgtype: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WhoAmIResponse {
    user_id: String,
    #[serde(default)]
    device_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RoomAliasResponse {
    room_id: String,
}

impl MatrixChannel {
    fn normalize_optional_field(value: Option<String>) -> Option<String> {
        value
            .map(|entry| entry.trim().to_string())
            .filter(|entry| !entry.is_empty())
    }

    pub fn new(
        homeserver: String,
        access_token: String,
        room_id: String,
        allowed_users: Vec<String>,
    ) -> Self {
        Self::new_with_session_hint(homeserver, access_token, room_id, allowed_users, None, None)
    }

    pub fn new_with_session_hint(
        homeserver: String,
        access_token: String,
        room_id: String,
        allowed_users: Vec<String>,
        owner_hint: Option<String>,
        device_id_hint: Option<String>,
    ) -> Self {
        Self::new_with_session_hint_and_zeroclaw_dir(
            homeserver,
            access_token,
            room_id,
            allowed_users,
            owner_hint,
            device_id_hint,
            None,
        )
    }

    pub fn new_with_session_hint_and_zeroclaw_dir(
        homeserver: String,
        access_token: String,
        room_id: String,
        allowed_users: Vec<String>,
        owner_hint: Option<String>,
        device_id_hint: Option<String>,
        zeroclaw_dir: Option<PathBuf>,
    ) -> Self {
        let homeserver = homeserver.trim_end_matches('/').to_string();
        let access_token = access_token.trim().to_string();
        let room_id = room_id.trim().to_string();
        let allowed_users = allowed_users
            .into_iter()
            .map(|user| user.trim().to_string())
            .filter(|user| !user.is_empty())
            .collect();

        Self {
            homeserver,
            access_token,
            room_id,
            allowed_users,
            session_owner_hint: Self::normalize_optional_field(owner_hint),
            session_device_id_hint: Self::normalize_optional_field(device_id_hint),
            zeroclaw_dir,
            resolved_room_id_cache: Arc::new(RwLock::new(None)),
            sdk_client: Arc::new(OnceCell::new()),
            http_client: Client::new(),
            reaction_events: Arc::new(RwLock::new(HashMap::new())),
            voice_mode: Arc::new(AtomicBool::new(false)),
            transcription_config: None,
            tts_config: None,
            tts_api_url: None,
            last_draft_edit: Arc::new(Mutex::new(HashMap::new())),
            draft_current_event: Arc::new(Mutex::new(None)),
            allowed_rooms: HashSet::new(),
        }
    }

    /// Add extra rooms to listen on (e.g. from channel_workspaces keys).
    /// The configured `room_id` is always accepted regardless.
    pub fn with_allowed_rooms(mut self, rooms: impl IntoIterator<Item = String>) -> Self {
        self.allowed_rooms = rooms.into_iter().collect();
        self
    }

    pub fn with_transcription(mut self, config: Option<TranscriptionConfig>) -> Self {
        self.transcription_config = config;
        self
    }

    pub fn with_tts(mut self, config: Option<TtsConfig>) -> Self {
        if let Some(ref cfg) = config {
            if cfg.enabled {
                self.tts_api_url = cfg
                    .openai
                    .as_ref()
                    .map(|o| format!("{}/v1/audio/speech", o.base_url.trim_end_matches('/')));
            }
        }
        self.tts_config = config;
        self
    }

    /// Prepare text for TTS synthesis: strip markdown formatting, normalize
    /// whitespace, and truncate to `max_chars` at the nearest sentence boundary.
    fn prepare_tts_text(raw: &str, max_chars: usize) -> String {
        let mut text = raw.to_string();

        // Strip markdown block-level syntax
        text = text
            .lines()
            .filter(|line| {
                let trimmed = line.trim();
                // Drop code fences, horizontal rules, HTML tags
                !trimmed.starts_with("```")
                    && !trimmed.starts_with("---")
                    && !trimmed.starts_with('<')
            })
            .collect::<Vec<_>>()
            .join(" ");

        // Strip markdown inline formatting
        text = text
            .replace("**", "")
            .replace("__", "")
            .replace(['*', '`', '~'], "")
            .replace("[Voice message]:", "")
            .replace("# ", "")
            .replace("## ", "")
            .replace("### ", "");

        // Normalize dashes and whitespace
        text = text.replace("—", ", ").replace('–', ", ");
        text = text.replace(['\n', '\r'], " ");

        // Collapse multiple spaces
        while text.contains("  ") {
            text = text.replace("  ", " ");
        }
        text = text.trim().to_string();

        // Truncate at a sentence boundary using char indices (UTF-8 safe)
        if text.chars().count() > max_chars {
            let byte_limit = text
                .char_indices()
                .nth(max_chars)
                .map(|(i, _)| i)
                .unwrap_or(text.len());

            let truncation_point = text[..byte_limit]
                .rfind(". ")
                .or_else(|| text[..byte_limit].rfind("! "))
                .or_else(|| text[..byte_limit].rfind("? "))
                .map(|i| i + 1)
                .unwrap_or(byte_limit);

            text.truncate(truncation_point);
            text = text.trim().to_string();
        }

        text
    }

    fn encode_path_segment(value: &str) -> String {
        fn should_encode(byte: u8) -> bool {
            !matches!(
                byte,
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~'
            )
        }

        let mut encoded = String::with_capacity(value.len());
        for byte in value.bytes() {
            if should_encode(byte) {
                use std::fmt::Write;
                let _ = write!(&mut encoded, "%{byte:02X}");
            } else {
                encoded.push(byte as char);
            }
        }

        encoded
    }

    fn auth_header_value(&self) -> String {
        format!("Bearer {}", self.access_token)
    }

    fn matrix_store_dir(&self) -> Option<PathBuf> {
        self.zeroclaw_dir
            .as_ref()
            .map(|dir| dir.join("state").join("matrix"))
    }

    fn is_user_allowed(&self, sender: &str) -> bool {
        Self::is_sender_allowed(&self.allowed_users, sender)
    }

    fn is_sender_allowed(allowed_users: &[String], sender: &str) -> bool {
        if allowed_users.iter().any(|u| u == "*") {
            return true;
        }

        allowed_users.iter().any(|u| u.eq_ignore_ascii_case(sender))
    }

    fn is_supported_message_type(msgtype: &str) -> bool {
        matches!(msgtype, "m.text" | "m.notice")
    }

    fn has_non_empty_body(body: &str) -> bool {
        !body.trim().is_empty()
    }

    fn is_help_command(body: &str) -> bool {
        let t = body.trim().to_lowercase();
        t == "help"
            || t == "!help"
            || t == "/help"
            || t == "commands"
            || t == "!commands"
            || t == "command"
    }

    fn handle_help_command() -> String {
        [
            "**Zero-token commands** _(no LLM, instant)_",
            "",
            "**usage** — Claude Code quota bars with reset dates",
            "**restart** — restart the zeroclaw daemon (zero-token)",
            "**cron** — list cron jobs for this room (checks tmux for pending questions)",
            "**cron all** — list cron jobs for all rooms",
            "**peek** `[N]` — last N lines of this room's tmux pane (default 20)",
            "**history** `[N]` — last N Matrix messages in this room (default 10)",
            "**ticket** `<subcommand>` — ticket management (`ticket help` for details)",
            "**tmux** `<text>` — send text directly to the tmux pane for this room",
            "**help** / **commands** — this help",
            "",
            "Prefix any other message to route to the agent.",
        ]
        .join("\n")
    }

    fn is_history_command(body: &str) -> bool {
        let t = body.trim().to_lowercase();
        t == "history" || t == "!history" || t.starts_with("history ") || t.starts_with("!history ")
    }

    async fn fetch_room_messages(
        http_client: &reqwest::Client,
        homeserver: &str,
        access_token: &str,
        room_id: &str,
        limit: u64,
    ) -> String {
        let encoded = urlencoding::encode(room_id).into_owned();
        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/messages?dir=b&limit={}",
            homeserver.trim_end_matches('/'),
            encoded,
            limit
        );
        let resp = match http_client
            .get(&url)
            .header("Authorization", format!("Bearer {}", access_token))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => return format!("Failed to fetch history: {e}"),
        };
        let json: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => return format!("Failed to parse history: {e}"),
        };
        let chunks = match json.get("chunk").and_then(|c| c.as_array()) {
            Some(v) => v,
            None => return "No messages found.".to_string(),
        };
        let mut lines = Vec::new();
        for ev in chunks.iter().rev() {
            if ev.get("type").and_then(|t| t.as_str()) != Some("m.room.message") {
                continue;
            }
            let sender = ev.get("sender").and_then(|s| s.as_str()).unwrap_or("?");
            let sender_short = sender
                .trim_start_matches('@')
                .split(':')
                .next()
                .unwrap_or(sender);
            let body = ev
                .get("content")
                .and_then(|c| c.get("body"))
                .and_then(|b| b.as_str())
                .unwrap_or("")
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(120)
                .collect::<String>();
            lines.push(format!("**{}**: {}", sender_short, body));
        }
        if lines.is_empty() {
            "No recent messages.".to_string()
        } else {
            format!(
                "**Room history** (last {}):\n\n{}",
                lines.len(),
                lines.join("\n")
            )
        }
    }

    fn is_usage_command(body: &str) -> bool {
        let trimmed = body.trim().to_lowercase();
        trimmed == "usage"
            || trimmed == "!usage"
            || trimmed == "#usage"
            || trimmed == "/usage"
            || trimmed.starts_with("usage ")
            || trimmed.starts_with("!usage ")
            || trimmed.starts_with("#usage ")
            || trimmed.starts_with("/usage ")
    }

    async fn handle_usage_command() -> String {
        // Fetch live quota from Anthropic's OAuth usage endpoint.
        // Token is stored in macOS Keychain under "Claude Code-credentials".
        if let Some(result) = Self::fetch_oauth_usage().await {
            return result;
        }
        // Fallback: aggregate historical token counts from session JSONL files.
        tokio::task::spawn_blocking(Self::aggregate_claude_code_usage)
            .await
            .unwrap_or_else(|_| "Usage data unavailable.".to_string())
    }

    async fn fetch_oauth_usage() -> Option<String> {
        // Retrieve the OAuth access token from macOS Keychain.
        let keychain_output = tokio::process::Command::new("security")
            .args([
                "find-generic-password",
                "-s",
                "Claude Code-credentials",
                "-w",
            ])
            .output()
            .await
            .ok()?;

        if !keychain_output.status.success() {
            return None;
        }

        let json_str = String::from_utf8_lossy(&keychain_output.stdout);
        let creds: serde_json::Value = serde_json::from_str(json_str.trim()).ok()?;
        let token = creds
            .get("claudeAiOauth")?
            .get("accessToken")?
            .as_str()?
            .to_string();

        // Call the usage endpoint.
        let resp = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            reqwest::Client::new()
                .get("https://api.anthropic.com/api/oauth/usage")
                .header("Authorization", format!("Bearer {token}"))
                .header("anthropic-beta", "oauth-2025-04-20")
                .header("User-Agent", "claude-code/2.0.32")
                .send(),
        )
        .await
        .ok()?
        .ok()?;

        if !resp.status().is_success() {
            return None;
        }

        let data: serde_json::Value = resp.json().await.ok()?;

        let fmt_bar = |pct: f64| -> String {
            let filled = ((pct / 100.0) * 20.0).round() as usize;
            let filled = filled.min(20);
            format!(
                "{}{} {:.0}%",
                "█".repeat(filled),
                "░".repeat(20 - filled),
                pct
            )
        };

        let fmt_reset = |resets_at: Option<&str>| -> String {
            resets_at
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| {
                    let local = dt.with_timezone(&chrono::Local);
                    local.format("Resets %b %-d at %-I:%M%P").to_string()
                })
                .unwrap_or_else(|| "No reset info".to_string())
        };

        let mut out = String::from("**Claude Code Usage**\n\n");

        if let Some(fh) = data.get("five_hour") {
            let pct = fh
                .get("utilization")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let reset = fh.get("resets_at").and_then(|v| v.as_str());
            out.push_str(&format!(
                "**Session (5h)**\n`{}`\n_{}_\n\n",
                fmt_bar(pct),
                fmt_reset(reset)
            ));
        }

        if let Some(sd) = data.get("seven_day") {
            let pct = sd
                .get("utilization")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let reset = sd.get("resets_at").and_then(|v| v.as_str());
            out.push_str(&format!(
                "**Week (all models)**\n`{}`\n_{}_\n\n",
                fmt_bar(pct),
                fmt_reset(reset)
            ));
        }

        for (key, label) in &[
            ("seven_day_sonnet", "Week (Sonnet)"),
            ("seven_day_opus", "Week (Opus)"),
        ] {
            if let Some(bucket) = data.get(key) {
                if let Some(pct) = bucket.get("utilization").and_then(|v| v.as_f64()) {
                    let reset = bucket.get("resets_at").and_then(|v| v.as_str());
                    out.push_str(&format!(
                        "**{}**\n`{}`\n_{}_\n\n",
                        label,
                        fmt_bar(pct),
                        fmt_reset(reset)
                    ));
                }
            }
        }

        if let Some(extra) = data.get("extra_usage") {
            let enabled = extra
                .get("is_enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if enabled {
                let pct = extra
                    .get("utilization")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                out.push_str(&format!("**Extra usage**\n`{}`\n\n", fmt_bar(pct)));
            } else {
                out.push_str("**Extra usage:** not enabled\n");
            }
        }

        Some(out.trim_end().to_string())
    }

    /// Estimate cost in USD from token counts based on model name.
    fn estimate_cost(
        model: &str,
        input: u64,
        output: u64,
        cache_read: u64,
        cache_creation: u64,
    ) -> f64 {
        // Prices per million tokens (as of mid-2025)
        let (inp_pm, out_pm, cr_pm, cc_pm) = if model.contains("opus") {
            (15.0, 75.0, 1.50, 18.75)
        } else if model.contains("haiku") {
            (0.80, 4.0, 0.08, 1.0)
        } else {
            // sonnet or unknown — use sonnet pricing
            (3.0, 15.0, 0.30, 3.75)
        };
        (input as f64 * inp_pm
            + output as f64 * out_pm
            + cache_read as f64 * cr_pm
            + cache_creation as f64 * cc_pm)
            / 1_000_000.0
    }

    fn aggregate_claude_code_usage() -> String {
        let home = match std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
            Ok(h) => std::path::PathBuf::from(h),
            Err(_) => return "Usage data unavailable: HOME not set.".to_string(),
        };

        let projects_dir = home.join(".claude").join("projects");
        if !projects_dir.exists() {
            return "Usage data unavailable: `~/.claude/projects` not found.".to_string();
        }

        #[derive(Default)]
        struct Bucket {
            input: u64,
            output: u64,
            cache_read: u64,
            cache_creation: u64,
            cost_usd: f64,
            messages: u64,
        }

        let now_str = chrono::Utc::now().format("%Y-%m-%d").to_string();
        // week = last 7 days prefix strings
        let week_starts: Vec<String> = (0u64..7)
            .map(|d| {
                (chrono::Utc::now() - chrono::Duration::days(d as i64))
                    .format("%Y-%m-%d")
                    .to_string()
            })
            .collect();

        let mut today = Bucket::default();
        let mut week = Bucket::default();
        let mut all_time = Bucket::default();
        let mut model_map: std::collections::HashMap<String, Bucket> =
            std::collections::HashMap::new();

        let project_dirs = match std::fs::read_dir(&projects_dir) {
            Ok(d) => d,
            Err(_) => {
                return "Usage data unavailable: cannot read `~/.claude/projects`.".to_string()
            }
        };

        // Deduplicate by message ID: streaming JSONL writes multiple partial records
        // per turn. Keep only the entry with the highest output_tokens per message ID.
        struct MsgRecord {
            ts: String,
            model: String,
            inp: u64,
            out: u64,
            cr: u64,
            cc: u64,
        }
        let mut best: std::collections::HashMap<String, MsgRecord> =
            std::collections::HashMap::new();

        for project_entry in project_dirs.flatten() {
            let path = project_entry.path();
            if !path.is_dir() {
                continue;
            }
            let files = match std::fs::read_dir(&path) {
                Ok(f) => f,
                Err(_) => continue,
            };
            for file_entry in files.flatten() {
                let fpath = file_entry.path();
                if fpath.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                let contents = match std::fs::read_to_string(&fpath) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                for line in contents.lines() {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else {
                        continue;
                    };
                    let msg = match entry.get("message") {
                        Some(m) => m,
                        None => continue,
                    };
                    if msg.get("role").and_then(|r| r.as_str()) != Some("assistant") {
                        continue;
                    }
                    let usage = match msg.get("usage") {
                        Some(u) => u,
                        None => continue,
                    };
                    let mid = msg
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let out = usage
                        .get("output_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);

                    // Skip partial streaming records (no output yet)
                    if out == 0 && mid.is_empty() {
                        continue;
                    }

                    let existing_out = best.get(&mid).map(|r| r.out).unwrap_or(0);
                    if out >= existing_out {
                        best.insert(
                            mid,
                            MsgRecord {
                                ts: entry
                                    .get("timestamp")
                                    .and_then(|t| t.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                                model: msg
                                    .get("model")
                                    .and_then(|m| m.as_str())
                                    .unwrap_or("unknown")
                                    .to_string(),
                                inp: usage
                                    .get("input_tokens")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(0),
                                out,
                                cr: usage
                                    .get("cache_read_input_tokens")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(0),
                                cc: usage
                                    .get("cache_creation_input_tokens")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(0),
                            },
                        );
                    }
                }
            }
        }

        for rec in best.values() {
            let cost = Self::estimate_cost(&rec.model, rec.inp, rec.out, rec.cr, rec.cc);

            all_time.input += rec.inp;
            all_time.output += rec.out;
            all_time.cache_read += rec.cr;
            all_time.cache_creation += rec.cc;
            all_time.cost_usd += cost;
            all_time.messages += 1;

            if rec.ts.starts_with(&now_str) {
                today.input += rec.inp;
                today.output += rec.out;
                today.cache_read += rec.cr;
                today.cache_creation += rec.cc;
                today.cost_usd += cost;
                today.messages += 1;
            }

            if week_starts.iter().any(|d| rec.ts.starts_with(d.as_str())) {
                week.input += rec.inp;
                week.output += rec.out;
                week.cache_read += rec.cr;
                week.cache_creation += rec.cc;
                week.cost_usd += cost;
                week.messages += 1;
            }

            let entry = model_map
                .entry(rec.model.clone())
                .or_insert_with(Bucket::default);
            entry.input += rec.inp;
            entry.output += rec.out;
            entry.cache_read += rec.cr;
            entry.cache_creation += rec.cc;
            entry.cost_usd += cost;
            entry.messages += 1;
        }

        if all_time.messages == 0 {
            return "No Claude Code usage data found.".to_string();
        }

        let fmt_tok = |n: u64| -> String {
            if n >= 1_000_000 {
                format!("{:.2}M", n as f64 / 1_000_000.0)
            } else if n >= 1_000 {
                format!("{:.1}k", n as f64 / 1_000.0)
            } else {
                n.to_string()
            }
        };

        let fmt_cost = |c: f64| -> String { format!("${:.4}", c) };

        let row = |b: &Bucket| -> String {
            format!(
                "in {} · out {} · cache r{}/w{} · {} · {} msgs",
                fmt_tok(b.input),
                fmt_tok(b.output),
                fmt_tok(b.cache_read),
                fmt_tok(b.cache_creation),
                fmt_cost(b.cost_usd),
                b.messages,
            )
        };

        let mut out = String::from("**Claude Code Usage** _(API-rate estimates)_\n\n");
        out.push_str(&format!("**Today:** {}\n", row(&today)));
        out.push_str(&format!("**7 days:** {}\n", row(&week)));
        out.push_str(&format!("**All time:** {}\n", row(&all_time)));

        if !model_map.is_empty() {
            let mut models: Vec<_> = model_map.iter().collect();
            models.sort_by(|a, b| {
                b.1.cost_usd
                    .partial_cmp(&a.1.cost_usd)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            out.push_str("\n**By model:**\n");
            for (model, b) in models.iter().take(5) {
                out.push_str(&format!(
                    "- `{}` — {} msgs · {}\n",
                    model,
                    b.messages,
                    fmt_cost(b.cost_usd)
                ));
            }
        }

        out
    }

    /// After initial sync, scan each accepted room for unanswered messages sent during downtime.
    /// For each room, queues at most the single most-recent unanswered message. Rooms are
    /// processed with a 500 ms inter-room delay to avoid response floods. Messages older than
    /// 24 hours are ignored.
    async fn check_unanswered_on_startup(
        &self,
        accepted_rooms: &HashSet<OwnedRoomId>,
        my_user_id: &str,
        dedupe: &Arc<
            Mutex<(
                std::collections::VecDeque<String>,
                std::collections::HashSet<String>,
            )>,
        >,
        tx: &mpsc::Sender<ChannelMessage>,
    ) {
        let cutoff_ms = chrono::Utc::now().timestamp_millis() - 86_400_000;

        let mut rooms: Vec<&OwnedRoomId> = accepted_rooms.iter().collect();
        rooms.sort_by_key(|r| r.as_str());

        for (i, room_id) in rooms.iter().enumerate() {
            if i > 0 {
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            }

            let encoded = Self::encode_path_segment(room_id.as_str());
            let url = format!(
                "{}/_matrix/client/v3/rooms/{}/messages?dir=b&limit=50",
                self.homeserver, encoded
            );

            let resp = match self
                .http_client
                .get(&url)
                .header("Authorization", self.auth_header_value())
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => r,
                Ok(r) => {
                    tracing::warn!(
                        "Startup check: messages API returned {} for {}",
                        r.status(),
                        room_id
                    );
                    continue;
                }
                Err(e) => {
                    tracing::warn!(
                        "Startup check: failed to fetch messages for {}: {}",
                        room_id,
                        e
                    );
                    continue;
                }
            };

            let data: serde_json::Value = match resp.json().await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(
                        "Startup check: failed to parse messages for {}: {}",
                        room_id,
                        e
                    );
                    continue;
                }
            };

            let chunk = match data.get("chunk").and_then(|c| c.as_array()) {
                Some(c) => c,
                None => continue,
            };

            // chunk is newest-first (dir=b). Walk until we find the most recent
            // allowed-user message. If a bot message appears first, the room is answered.
            let mut candidate: Option<(String, String, String, i64)> = None;

            for event in chunk {
                let event_type = event.get("type").and_then(|t| t.as_str()).unwrap_or("");
                if event_type != "m.room.message" {
                    continue;
                }

                let ts = event
                    .get("origin_server_ts")
                    .and_then(|t| t.as_i64())
                    .unwrap_or(0);
                if ts < cutoff_ms {
                    break;
                }

                let sender = event.get("sender").and_then(|s| s.as_str()).unwrap_or("");

                if sender == my_user_id {
                    break; // bot already replied — room is answered
                }

                if !Self::is_sender_allowed(&self.allowed_users, sender) {
                    continue;
                }

                let body = event
                    .get("content")
                    .and_then(|c| c.get("body"))
                    .and_then(|b| b.as_str())
                    .unwrap_or("")
                    .to_string();

                if !Self::has_non_empty_body(&body) {
                    continue;
                }

                let event_id = event
                    .get("event_id")
                    .and_then(|e| e.as_str())
                    .unwrap_or("")
                    .to_string();

                candidate = Some((event_id, sender.to_string(), body, ts));
                break;
            }

            if let Some((event_id, sender, body, ts)) = candidate {
                {
                    let mut guard = dedupe.lock().await;
                    let (recent_order, recent_lookup) = &mut *guard;
                    if Self::cache_event_id(&event_id, recent_order, recent_lookup) {
                        continue;
                    }
                }

                tracing::info!(
                    "Matrix startup: queuing unanswered message in {} from {}",
                    room_id,
                    sender
                );

                let msg = ChannelMessage {
                    id: event_id,
                    sender: sender.clone(),
                    reply_target: format!("{}||{}", sender, room_id),
                    content: body,
                    channel: "matrix".to_string(),
                    timestamp: (ts / 1000) as u64,
                    thread_ts: None,
                };

                let _ = tx.send(msg).await;
            }
        }
    }

    fn cache_event_id(
        event_id: &str,
        recent_order: &mut std::collections::VecDeque<String>,
        recent_lookup: &mut std::collections::HashSet<String>,
    ) -> bool {
        const MAX_RECENT_EVENT_IDS: usize = 2048;

        if recent_lookup.contains(event_id) {
            return true;
        }

        let event_id_owned = event_id.to_string();
        recent_lookup.insert(event_id_owned.clone());
        recent_order.push_back(event_id_owned);

        if recent_order.len() > MAX_RECENT_EVENT_IDS {
            if let Some(evicted) = recent_order.pop_front() {
                recent_lookup.remove(&evicted);
            }
        }

        false
    }

    async fn target_room_id(&self) -> anyhow::Result<String> {
        if self.room_id.starts_with('!') {
            return Ok(self.room_id.clone());
        }

        if let Some(cached) = self.resolved_room_id_cache.read().await.clone() {
            return Ok(cached);
        }

        let resolved = self.resolve_room_id().await?;
        *self.resolved_room_id_cache.write().await = Some(resolved.clone());
        Ok(resolved)
    }

    async fn get_my_identity(&self) -> anyhow::Result<WhoAmIResponse> {
        let url = format!("{}/_matrix/client/v3/account/whoami", self.homeserver);
        let resp = self
            .http_client
            .get(&url)
            .header("Authorization", self.auth_header_value())
            .send()
            .await?;

        if !resp.status().is_success() {
            let err = resp.text().await?;
            anyhow::bail!("Matrix whoami failed: {err}");
        }

        Ok(resp.json().await?)
    }

    async fn get_my_user_id(&self) -> anyhow::Result<String> {
        Ok(self.get_my_identity().await?.user_id)
    }

    async fn matrix_client(&self) -> anyhow::Result<MatrixSdkClient> {
        let client = self
            .sdk_client
            .get_or_try_init(|| async {
                let identity = self.get_my_identity().await;
                let whoami = match identity {
                    Ok(whoami) => Some(whoami),
                    Err(error) => {
                        if self.session_owner_hint.is_some() && self.session_device_id_hint.is_some()
                        {
                            tracing::warn!(
                                "Matrix whoami failed; falling back to configured session hints for E2EE session restore: {error}"
                            );
                            None
                        } else {
                            return Err(error);
                        }
                    }
                };

                let resolved_user_id = if let Some(whoami) = whoami.as_ref() {
                    if let Some(hinted) = self.session_owner_hint.as_ref() {
                        if hinted != &whoami.user_id {
                            tracing::warn!(
                                "Matrix configured user_id '{}' does not match whoami '{}'; using whoami.",
                                crate::security::redact(hinted),
                                crate::security::redact(&whoami.user_id)
                            );
                        }
                    }
                    whoami.user_id.clone()
                } else {
                    self.session_owner_hint.clone().ok_or_else(|| {
                        anyhow::anyhow!(
                            "Matrix session restore requires user_id when whoami is unavailable"
                        )
                    })?
                };

                let resolved_device_id = match (whoami.as_ref(), self.session_device_id_hint.as_ref()) {
                    (Some(whoami), Some(hinted)) => {
                        if let Some(whoami_device_id) = whoami.device_id.as_ref() {
                            if whoami_device_id != hinted {
                                tracing::warn!(
                                    "Matrix configured device_id '{}' does not match whoami '{}'; using whoami.",
                                    crate::security::redact(hinted),
                                    crate::security::redact(whoami_device_id)
                                );
                            }
                            whoami_device_id.clone()
                        } else {
                            hinted.clone()
                        }
                    }
                    (Some(whoami), None) => whoami.device_id.clone().ok_or_else(|| {
                        anyhow::anyhow!(
                            "Matrix whoami response did not include device_id. Set channels.matrix.device_id to enable E2EE session restore."
                        )
                    })?,
                    (None, Some(hinted)) => hinted.clone(),
                    (None, None) => {
                        return Err(anyhow::anyhow!(
                            "Matrix E2EE session restore requires device_id when whoami is unavailable"
                        ));
                    }
                };

                let mut client_builder = MatrixSdkClient::builder().homeserver_url(&self.homeserver);

                if let Some(store_dir) = self.matrix_store_dir() {
                    tokio::fs::create_dir_all(&store_dir).await.map_err(|error| {
                        anyhow::anyhow!(
                            "Matrix failed to initialize persistent store directory at '{}': {error}",
                            store_dir.display()
                        )
                    })?;
                    client_builder = client_builder.sqlite_store(&store_dir, None);
                }

                let client = client_builder.build().await?;

                let user_id: OwnedUserId = resolved_user_id.parse()?;
                let session = MatrixSession {
                    meta: SessionMeta {
                        user_id,
                        device_id: resolved_device_id.into(),
                    },
                    tokens: SessionTokens {
                        access_token: self.access_token.clone(),
                        refresh_token: None,
                    },
                };

                client.restore_session(session).await?;

                Ok::<MatrixSdkClient, anyhow::Error>(client)
            })
            .await?;

        Ok(client.clone())
    }

    async fn resolve_room_id(&self) -> anyhow::Result<String> {
        let configured = self.room_id.trim();

        if configured.starts_with('!') {
            return Ok(configured.to_string());
        }

        if configured.starts_with('#') {
            let encoded_alias = Self::encode_path_segment(configured);
            let url = format!(
                "{}/_matrix/client/v3/directory/room/{}",
                self.homeserver, encoded_alias
            );

            let resp = self
                .http_client
                .get(&url)
                .header("Authorization", self.auth_header_value())
                .send()
                .await?;

            if !resp.status().is_success() {
                let err = resp.text().await.unwrap_or_default();
                anyhow::bail!("Matrix room alias resolution failed for '{configured}': {err}");
            }

            let resolved: RoomAliasResponse = resp.json().await?;
            return Ok(resolved.room_id);
        }

        anyhow::bail!(
            "Matrix room reference must start with '!' (room ID) or '#' (room alias), got: {configured}"
        )
    }

    async fn ensure_room_accessible(&self, room_id: &str) -> anyhow::Result<()> {
        let encoded_room = Self::encode_path_segment(room_id);
        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/joined_members",
            self.homeserver, encoded_room
        );

        let resp = self
            .http_client
            .get(&url)
            .header("Authorization", self.auth_header_value())
            .send()
            .await?;

        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("Matrix room access check failed for '{room_id}': {err}");
        }

        Ok(())
    }

    async fn room_is_encrypted(&self, room_id: &str) -> anyhow::Result<bool> {
        let encoded_room = Self::encode_path_segment(room_id);
        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/state/m.room.encryption",
            self.homeserver, encoded_room
        );

        let resp = self
            .http_client
            .get(&url)
            .header("Authorization", self.auth_header_value())
            .send()
            .await?;

        if resp.status().is_success() {
            return Ok(true);
        }

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(false);
        }

        let err = resp.text().await.unwrap_or_default();
        anyhow::bail!("Matrix room encryption check failed for '{room_id}': {err}");
    }

    async fn ensure_room_supported(&self, room_id: &str) -> anyhow::Result<()> {
        self.ensure_room_accessible(room_id).await?;

        if self.room_is_encrypted(room_id).await? {
            tracing::info!(
                "Matrix room {} is encrypted; E2EE decryption is enabled via matrix-sdk.",
                room_id
            );
        }

        Ok(())
    }

    fn sync_filter_for_room(room_id: &str, timeline_limit: usize) -> String {
        let timeline_limit = timeline_limit.max(1);
        serde_json::json!({
            "room": {
                "rooms": [room_id],
                "timeline": {
                    "limit": timeline_limit
                }
            }
        })
        .to_string()
    }

    async fn log_e2ee_diagnostics(&self, client: &MatrixSdkClient) {
        match client.encryption().get_own_device().await {
            Ok(Some(device)) => {
                if device.is_verified() {
                    tracing::info!(
                        "Matrix device '{}' is verified for E2EE.",
                        device.device_id()
                    );
                } else {
                    tracing::warn!(
                        "Matrix device '{}' is not verified. Some clients may label bot messages as unverified until you sign/verify this device from a trusted session.",
                        device.device_id()
                    );
                }
            }
            Ok(None) => {
                tracing::warn!(
                    "Matrix own-device metadata is unavailable; verify/signing status cannot be determined."
                );
            }
            Err(error) => {
                tracing::warn!("Matrix own-device verification check failed: {error}");
            }
        }

        if client.encryption().backups().are_enabled().await {
            tracing::info!("Matrix room-key backup is enabled for this device.");
        } else {
            tracing::warn!(
                "Matrix room-key backup is not enabled for this device; `matrix_sdk_crypto::backups` warnings about missing backup keys may appear until recovery is configured."
            );
        }
    }
}

#[async_trait]
impl Channel for MatrixChannel {
    fn name(&self) -> &str {
        "matrix"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let client = self.matrix_client().await?;
        let target_room_id = if message.recipient.contains("||") {
            message.recipient.split_once("||").unwrap().1.to_string()
        } else {
            self.target_room_id().await?
        };
        let target_room: OwnedRoomId = target_room_id.parse()?;

        let mut room = client.get_room(&target_room);
        if room.is_none() {
            let _ = client.sync_once(SyncSettings::new()).await;
            room = client.get_room(&target_room);
        }

        let Some(room) = room else {
            anyhow::bail!("Matrix room '{}' not found in joined rooms", target_room_id);
        };

        if room.state() != RoomState::Joined {
            anyhow::bail!("Matrix room '{}' is not in joined state", target_room_id);
        }

        // Stop typing notification before sending the response
        if let Err(error) = room.typing_notice(false).await {
            tracing::warn!("Matrix failed to stop typing notification: {error}");
        }

        let mut content = RoomMessageEventContent::text_markdown(&message.content);

        if let Some(ref thread_ts) = message.thread_ts {
            if let Ok(thread_root) = thread_ts.parse::<OwnedEventId>() {
                content.relates_to = Some(Relation::Thread(Thread::plain(
                    thread_root.clone(),
                    thread_root,
                )));
            }
        }

        room.send(content).await?;

        // Voice reply: synthesize TTS and send as m.audio
        if self.voice_mode.load(Ordering::Relaxed) {
            self.voice_mode.store(false, Ordering::Relaxed);
            tracing::info!("Voice mode active, generating TTS reply");

            let max_chars = self
                .tts_config
                .as_ref()
                .map(|c| c.max_text_length)
                .unwrap_or(800);
            let tts_text = Self::prepare_tts_text(&message.content, max_chars);
            tracing::info!("TTS synthesizing {} chars", tts_text.len());

            // Synthesize audio: try Kokoro API first, then macOS say, then edge-tts
            let audio_result: Option<(Vec<u8>, &str, &str)> = {
                let mut result = None;

                // Try Kokoro/OpenAI-compatible TTS API
                if let Some(ref tts_url) = self.tts_api_url {
                    let voice = self
                        .tts_config
                        .as_ref()
                        .map(|c| c.default_voice.as_str())
                        .unwrap_or("echo");
                    let speed = self.tts_config.as_ref().map(|c| c.speed).unwrap_or(1.0);
                    let tts_body = serde_json::json!({
                        "model": "tts-1",
                        "input": &tts_text,
                        "voice": voice,
                        "response_format": "wav",
                        "speed": speed
                    });
                    match self
                        .http_client
                        .post(tts_url)
                        .json(&tts_body)
                        .timeout(std::time::Duration::from_secs(120))
                        .send()
                        .await
                    {
                        Ok(resp) if resp.status().is_success() => {
                            if let Ok(bytes) = resp.bytes().await {
                                result = Some((bytes.to_vec(), "audio/wav", "voice-reply.wav"));
                            }
                        }
                        Ok(resp) => {
                            tracing::warn!("TTS API error: {}", resp.status());
                        }
                        Err(e) => {
                            tracing::warn!("TTS API unavailable: {}", e);
                        }
                    }
                }

                // Fallback: macOS say
                if result.is_none() {
                    tracing::info!("Falling back to macOS say");
                    let voice_dir = std::path::PathBuf::from("/tmp/zeroclaw-voice");
                    let _ = tokio::fs::create_dir_all(&voice_dir).await;
                    let aiff_path = voice_dir.join("reply.aiff");
                    let wav_path = voice_dir.join("reply.wav");

                    let say_ok = tokio::process::Command::new("say")
                        .args(["-v", "Reed (English (US))", "-o"])
                        .arg(&aiff_path)
                        .arg(&tts_text)
                        .output()
                        .await
                        .map(|o| o.status.success())
                        .unwrap_or(false);

                    if say_ok {
                        let convert_ok = tokio::process::Command::new("ffmpeg")
                            .args([
                                "-hide_banner",
                                "-loglevel",
                                "error",
                                "-y",
                                "-i",
                                aiff_path.to_str().unwrap_or_default(),
                                "-ar",
                                "24000",
                                "-ac",
                                "1",
                                wav_path.to_str().unwrap_or_default(),
                            ])
                            .output()
                            .await
                            .map(|o| o.status.success())
                            .unwrap_or(false);
                        if convert_ok {
                            result = tokio::fs::read(&wav_path)
                                .await
                                .ok()
                                .map(|d| (d, "audio/wav", "voice-reply.wav"));
                        }
                    }
                }

                // Last resort: edge-tts (cloud)
                if result.is_none() {
                    tracing::info!("Falling back to edge-tts");
                    let mp3_path = std::path::PathBuf::from("/tmp/zeroclaw-voice/reply.mp3");
                    let edge_ok = tokio::process::Command::new("edge-tts")
                        .args(["--text", &tts_text, "--write-media"])
                        .arg(&mp3_path)
                        .output()
                        .await
                        .map(|o| o.status.success())
                        .unwrap_or(false);
                    if edge_ok {
                        result = tokio::fs::read(&mp3_path)
                            .await
                            .ok()
                            .map(|d| (d, "audio/mpeg", "voice-reply.mp3"));
                    }
                }

                result
            };

            // Upload audio to Matrix and send as voice message
            if let Some((audio_data, mime, filename)) = audio_result {
                let upload_url = format!(
                    "{}/_matrix/media/v3/upload?filename={}",
                    self.homeserver, filename
                );
                let audio_len = audio_data.len();
                match self
                    .http_client
                    .post(&upload_url)
                    .header("Authorization", self.auth_header_value())
                    .header("Content-Type", mime)
                    .body(audio_data)
                    .send()
                    .await
                {
                    Ok(upload_resp) if upload_resp.status().is_success() => {
                        match upload_resp.json::<serde_json::Value>().await {
                            Ok(body) => {
                                if let Some(content_uri) = body["content_uri"].as_str() {
                                    tracing::info!(
                                        "Audio uploaded: {} ({} bytes) -> {}",
                                        filename,
                                        audio_len,
                                        content_uri
                                    );
                                    let encoded_room = Self::encode_path_segment(&target_room_id);
                                    let txn_id = format!(
                                        "voice_{}",
                                        std::time::SystemTime::now()
                                            .duration_since(std::time::UNIX_EPOCH)
                                            .unwrap_or_default()
                                            .as_millis()
                                    );
                                    let audio_msg = serde_json::json!({
                                        "msgtype": "m.audio",
                                        "body": "Voice reply",
                                        "url": content_uri,
                                        "info": {
                                            "mimetype": mime,
                                            "duration": 0
                                        },
                                        "org.matrix.msc3245.voice": {}
                                    });
                                    let send_url = format!(
                                        "{}/_matrix/client/v3/rooms/{}/send/m.room.message/{}",
                                        self.homeserver, encoded_room, txn_id
                                    );
                                    match self
                                        .http_client
                                        .put(&send_url)
                                        .header("Authorization", self.auth_header_value())
                                        .json(&audio_msg)
                                        .send()
                                        .await
                                    {
                                        Ok(put_resp) => {
                                            let status = put_resp.status();
                                            let resp_body =
                                                put_resp.text().await.unwrap_or_default();
                                            if status.is_success() {
                                                tracing::info!("Voice reply sent to Matrix");
                                            } else {
                                                tracing::warn!(
                                                    "Voice PUT failed ({}): {}",
                                                    status,
                                                    resp_body
                                                );
                                            }
                                        }
                                        Err(e) => {
                                            tracing::warn!("Voice PUT request error: {}", e);
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::warn!("Failed to parse upload response: {}", e);
                            }
                        }
                    }
                    Ok(resp) => {
                        tracing::warn!("Audio upload failed: {}", resp.status());
                    }
                    Err(e) => {
                        tracing::warn!("Audio upload request error: {}", e);
                    }
                }
            }
        }

        Ok(())
    }

    fn supports_draft_updates(&self) -> bool {
        true
    }

    async fn send_draft(&self, message: &SendMessage) -> anyhow::Result<Option<String>> {
        let room_id = if message.recipient.contains("||") {
            message.recipient.split_once("||").unwrap().1.to_string()
        } else {
            self.target_room_id().await?
        };
        let encoded_room = Self::encode_path_segment(&room_id);

        let initial_text = if message.content.is_empty() {
            "\u{2026}" // ellipsis
        } else {
            &message.content
        };

        let txn_id = format!(
            "draft_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        );

        let body = serde_json::json!({
            "msgtype": "m.text",
            "body": initial_text,
        });

        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/send/m.room.message/{}",
            self.homeserver, encoded_room, txn_id
        );

        let resp = self
            .http_client
            .put(&url)
            .header("Authorization", self.auth_header_value())
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("Matrix send_draft failed: {err}");
        }

        let resp_json: serde_json::Value = resp.json().await?;
        let event_id = resp_json["event_id"].as_str().map(|s| s.to_string());

        if let Some(ref id) = event_id {
            self.last_draft_edit
                .lock()
                .await
                .insert(id.clone(), std::time::Instant::now());
        }

        Ok(event_id)
    }

    async fn update_draft(
        &self,
        recipient: &str,
        message_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        // Rate-limit: at most one edit every 2 seconds
        {
            let edits = self.last_draft_edit.lock().await;
            if let Some(last_time) = edits.get(message_id) {
                if last_time.elapsed().as_millis() < 2000 {
                    return Ok(());
                }
            }
        }

        let room_id = if recipient.contains("||") {
            recipient.split_once("||").unwrap().1.to_string()
        } else {
            self.target_room_id().await?
        };
        let encoded_room = Self::encode_path_segment(&room_id);

        // Use m.replace to silently edit the original draft message
        let txn_id = format!(
            "edit_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        );
        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/send/m.room.message/{}",
            self.homeserver, encoded_room, txn_id
        );

        let _ = self
            .http_client
            .put(&url)
            .header("Authorization", self.auth_header_value())
            .json(&serde_json::json!({
                "msgtype": "m.text",
                "body": format!("* {text}"),
                "m.new_content": {
                    "msgtype": "m.text",
                    "body": text,
                },
                "m.relates_to": {
                    "rel_type": "m.replace",
                    "event_id": message_id,
                }
            }))
            .send()
            .await;

        self.last_draft_edit
            .lock()
            .await
            .insert(message_id.to_string(), std::time::Instant::now());

        Ok(())
    }

    async fn finalize_draft(
        &self,
        recipient: &str,
        message_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        self.last_draft_edit.lock().await.remove(message_id);

        let room_id = if recipient.contains("||") {
            recipient.split_once("||").unwrap().1.to_string()
        } else {
            self.target_room_id().await?
        };
        let encoded_room = Self::encode_path_segment(&room_id);

        let current_id = self
            .draft_current_event
            .lock()
            .await
            .take()
            .unwrap_or_else(|| message_id.to_string());

        // Edit the draft message in-place via m.replace
        let txn_id = format!(
            "final_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        );
        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/send/m.room.message/{}",
            self.homeserver, encoded_room, txn_id
        );

        match self
            .http_client
            .put(&url)
            .header("Authorization", self.auth_header_value())
            .json(&serde_json::json!({
                "msgtype": "m.text",
                "body": format!("* {text}"),
                "m.new_content": {
                    "msgtype": "m.text",
                    "body": text,
                },
                "m.relates_to": {
                    "rel_type": "m.replace",
                    "event_id": current_id,
                }
            }))
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => {
                tracing::info!("Matrix draft finalized (edit-in-place)");
            }
            Ok(r) => {
                let status = r.status();
                let err = r.text().await.unwrap_or_default();
                tracing::warn!("Matrix finalize_draft failed ({}): {}", status, err);
                anyhow::bail!("finalize_draft send failed: {status}");
            }
            Err(e) => {
                tracing::warn!("Matrix finalize_draft request error: {}", e);
                anyhow::bail!("finalize_draft request error: {e}");
            }
        }

        Ok(())
    }

    async fn cancel_draft(&self, _recipient: &str, message_id: &str) -> anyhow::Result<()> {
        self.last_draft_edit.lock().await.remove(message_id);
        *self.draft_current_event.lock().await = None;
        Ok(())
    }

    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        let target_room_id = self.target_room_id().await?;
        self.ensure_room_supported(&target_room_id).await?;

        let target_room: OwnedRoomId = target_room_id.parse()?;
        let my_user_id: OwnedUserId = match self.get_my_user_id().await {
            Ok(user_id) => user_id.parse()?,
            Err(error) => {
                if let Some(hinted) = self.session_owner_hint.as_ref() {
                    tracing::warn!(
                        "Matrix whoami failed while resolving listener user_id; using configured user_id hint: {error}"
                    );
                    hinted.parse()?
                } else {
                    return Err(error);
                }
            }
        };
        let client = self.matrix_client().await?;

        self.log_e2ee_diagnostics(&client).await;

        let _ = client.sync_once(SyncSettings::new()).await;

        // Build the set of rooms we accept messages from.
        // The configured room_id always passes; additional rooms come from channel_workspaces.
        let mut accepted_rooms: HashSet<OwnedRoomId> = HashSet::new();
        accepted_rooms.insert(target_room.clone());
        for room_str in &self.allowed_rooms {
            match room_str.parse::<OwnedRoomId>() {
                Ok(parsed) => {
                    accepted_rooms.insert(parsed);
                }
                Err(e) => {
                    tracing::warn!("Skipping unparseable workspace room '{}': {}", room_str, e);
                }
            }
        }

        tracing::info!(
            "Matrix channel listening on {} room(s) (primary: {})",
            accepted_rooms.len(),
            self.room_id
        );

        let recent_event_cache = Arc::new(Mutex::new((
            std::collections::VecDeque::new(),
            std::collections::HashSet::new(),
        )));

        self.check_unanswered_on_startup(
            &accepted_rooms,
            my_user_id.as_str(),
            &recent_event_cache,
            &tx,
        )
        .await;

        let tx_handler = tx.clone();
        let accepted_rooms_for_handler = Arc::new(accepted_rooms);
        let my_user_id_for_handler = my_user_id.clone();
        let allowed_users_for_handler = self.allowed_users.clone();
        let dedupe_for_handler = Arc::clone(&recent_event_cache);
        let homeserver_for_handler = self.homeserver.clone();
        let access_token_for_handler = self.access_token.clone();
        let http_client_for_handler = self.http_client.clone();
        let voice_mode_for_handler = Arc::clone(&self.voice_mode);
        let transcription_config_for_handler = self.transcription_config.clone();

        client.add_event_handler(move |event: OriginalSyncRoomMessageEvent, room: Room| {
            let tx = tx_handler.clone();
            let accepted_rooms = Arc::clone(&accepted_rooms_for_handler);
            let my_user_id = my_user_id_for_handler.clone();
            let allowed_users = allowed_users_for_handler.clone();
            let dedupe = Arc::clone(&dedupe_for_handler);
            let homeserver = homeserver_for_handler.clone();
            let access_token = access_token_for_handler.clone();
            let http_client = http_client_for_handler.clone();
            let voice_mode = Arc::clone(&voice_mode_for_handler);
            let transcription_config = transcription_config_for_handler.clone();

            async move {
                if !accepted_rooms.contains(room.room_id()) {
                    return;
                }

                if event.sender == my_user_id {
                    return;
                }

                let sender = event.sender.to_string();
                if !MatrixChannel::is_sender_allowed(&allowed_users, &sender) {
                    return;
                }

                // Helper: extract mxc:// download URL and filename for media types
                let media_info = |source: &MediaSource, name: &str| -> Option<(String, String)> {
                    match source {
                        MediaSource::Plain(mxc) => {
                            let rest = mxc.as_str().strip_prefix("mxc://")?;
                            let url =
                                format!("{}/_matrix/client/v1/media/download/{}", homeserver, rest);
                            Some((url, name.to_string()))
                        }
                        MediaSource::Encrypted(_) => None,
                    }
                };

                let (body, media_download) = match &event.content.msgtype {
                    MessageType::Text(content) => (content.body.clone(), None),
                    MessageType::Notice(content) => (content.body.clone(), None),
                    MessageType::Image(content) => {
                        let dl = media_info(&content.source, &content.body);
                        (format!("[IMAGE:{}]", content.body), dl)
                    }
                    MessageType::File(content) => {
                        let dl = media_info(&content.source, &content.body);
                        (format!("[file: {}]", content.body), dl)
                    }
                    MessageType::Audio(content) => {
                        let dl = media_info(&content.source, &content.body);
                        (format!("[audio: {}]", content.body), dl)
                    }
                    MessageType::Video(content) => {
                        let dl = media_info(&content.source, &content.body);
                        (format!("[video: {}]", content.body), dl)
                    }
                    _ => return,
                };

                // Download media to workspace if present
                let body = if let Some((url, filename)) = media_download {
                    let workspace = std::path::PathBuf::from(
                        shellexpand::tilde(
                            &std::env::var("ZEROCLAW_WORKSPACE")
                                .unwrap_or_else(|_| "/tmp/zeroclaw-uploads".to_string()),
                        )
                        .as_ref(),
                    );
                    let _ = tokio::fs::create_dir_all(&workspace).await;
                    let dest = workspace.join(&filename);
                    let client = reqwest::Client::new();
                    match client
                        .get(&url)
                        .header("Authorization", format!("Bearer {}", access_token))
                        .send()
                        .await
                    {
                        Ok(resp) if resp.status().is_success() => match resp.bytes().await {
                            Ok(bytes) => match tokio::fs::write(&dest, &bytes).await {
                                Ok(()) => format!("{} — saved to {}", body, dest.display()),
                                Err(_) => format!("{} — failed to write to disk", body),
                            },
                            Err(_) => format!("{} — download failed", body),
                        },
                        _ => format!("{} — download failed (auth error?)", body),
                    }
                } else {
                    body
                };

                // Voice transcription: if this was an audio message, transcribe via API
                let body = if body.starts_with("[audio:") {
                    if let (Some(config), Some(path_start)) =
                        (&transcription_config, body.find("saved to "))
                    {
                        if config.enabled {
                            let audio_path = body[path_start + 9..].to_string();
                            let file_name = std::path::Path::new(&audio_path)
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_else(|| "audio.ogg".to_string());
                            match tokio::fs::read(&audio_path).await {
                                Ok(audio_data) => match super::transcription::transcribe_audio(
                                    audio_data, &file_name, config,
                                )
                                .await
                                {
                                    Ok(text) if !text.trim().is_empty() => {
                                        voice_mode.store(true, Ordering::Relaxed);
                                        tracing::info!(
                                            "Matrix voice transcription: {:?}",
                                            text.trim()
                                        );
                                        format!("[Voice message]: {}", text.trim())
                                    }
                                    Ok(_) => {
                                        tracing::warn!(
                                            "Transcription returned empty text for {}",
                                            file_name
                                        );
                                        body
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            "Transcription failed for {}: {}",
                                            file_name,
                                            e
                                        );
                                        body
                                    }
                                },
                                Err(e) => {
                                    tracing::warn!(
                                        "Failed to read audio file {}: {}",
                                        audio_path,
                                        e
                                    );
                                    body
                                }
                            }
                        } else {
                            body
                        }
                    } else {
                        body
                    }
                } else {
                    body
                };

                if !MatrixChannel::has_non_empty_body(&body) {
                    return;
                }

                let event_id = event.event_id.to_string();
                {
                    let mut guard = dedupe.lock().await;
                    let (recent_order, recent_lookup) = &mut *guard;
                    if MatrixChannel::cache_event_id(&event_id, recent_order, recent_lookup) {
                        return;
                    }
                }

                // Send a read receipt for the incoming event
                if let Err(error) = room
                    .send_single_receipt(
                        create_receipt::v3::ReceiptType::Read,
                        ReceiptThread::Unthreaded,
                        event.event_id.clone(),
                    )
                    .await
                {
                    tracing::warn!("Matrix failed to send read receipt: {error}");
                }

                // Helper: send a zero-token reply and return early.
                macro_rules! send_zero_token {
                    ($text:expr, $label:expr) => {{
                        let mut content = RoomMessageEventContent::text_markdown(&$text);
                        if let Some(ref thread_ts) = match &event.content.relates_to {
                            Some(Relation::Thread(thread)) => Some(thread.event_id.to_string()),
                            _ => None,
                        } {
                            if let Ok(thread_root) = thread_ts.parse::<OwnedEventId>() {
                                content.relates_to = Some(Relation::Thread(Thread::plain(
                                    thread_root.clone(),
                                    thread_root,
                                )));
                            }
                        }
                        if let Err(e) = room.send(content).await {
                            tracing::warn!("Matrix failed to send {} result: {}", $label, e);
                        } else {
                            tracing::info!("Matrix {} command executed", $label);
                        }
                        if let Err(error) = room.typing_notice(false).await {
                            tracing::warn!("Matrix failed to stop typing notification: {error}");
                        }
                        return;
                    }};
                }

                // Check for usage command (zero-token operation)
                if MatrixChannel::is_usage_command(&body) {
                    let result = MatrixChannel::handle_usage_command().await;
                    send_zero_token!(result, "usage");
                }

                // Check for help/commands command (zero-token operation)
                if MatrixChannel::is_help_command(&body) {
                    let result = MatrixChannel::handle_help_command();
                    send_zero_token!(result, "help");
                }

                // Check for history command (zero-token operation)
                if MatrixChannel::is_history_command(&body) {
                    let limit: u64 = body
                        .trim()
                        .to_lowercase()
                        .trim_start_matches('!')
                        .trim_start_matches("history")
                        .trim()
                        .parse()
                        .unwrap_or(10)
                        .min(50);
                    let room_id_str = room.room_id().as_str();
                    let result = MatrixChannel::fetch_room_messages(
                        &http_client,
                        &homeserver,
                        &access_token,
                        room_id_str,
                        limit,
                    )
                    .await;
                    send_zero_token!(result, "history");
                }

                // Start typing notification while processing begins
                if let Err(error) = room.typing_notice(true).await {
                    tracing::warn!("Matrix failed to start typing notification: {error}");
                }

                let thread_ts = match &event.content.relates_to {
                    Some(Relation::Thread(thread)) => Some(thread.event_id.to_string()),
                    _ => None,
                };
                let msg = ChannelMessage {
                    id: event_id,
                    sender: sender.clone(),
                    reply_target: format!("{}||{}", sender, room.room_id()),
                    content: body,
                    channel: "matrix".to_string(),
                    timestamp: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                    thread_ts,
                };

                let _ = tx.send(msg).await;
            }
        });

        let sync_settings = SyncSettings::new().timeout(std::time::Duration::from_secs(30));
        client
            .sync_with_result_callback(sync_settings, |sync_result| {
                let tx = tx.clone();
                async move {
                    if tx.is_closed() {
                        return Ok::<LoopCtrl, matrix_sdk::Error>(LoopCtrl::Break);
                    }

                    if let Err(error) = sync_result {
                        tracing::warn!("Matrix sync error: {error}, retrying...");
                        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                    }

                    Ok::<LoopCtrl, matrix_sdk::Error>(LoopCtrl::Continue)
                }
            })
            .await?;

        Ok(())
    }

    async fn health_check(&self) -> bool {
        let Ok(room_id) = self.target_room_id().await else {
            return false;
        };

        if self.ensure_room_supported(&room_id).await.is_err() {
            return false;
        }

        self.matrix_client().await.is_ok()
    }

    async fn add_reaction(
        &self,
        _channel_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> anyhow::Result<()> {
        let client = self.matrix_client().await?;
        let target_room_id = self.target_room_id().await?;
        let target_room: OwnedRoomId = target_room_id.parse()?;

        let room = client
            .get_room(&target_room)
            .ok_or_else(|| anyhow::anyhow!("Matrix room not found for reaction"))?;

        let event_id: OwnedEventId = message_id
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid event ID for reaction: {}", message_id))?;

        let reaction = ReactionEventContent::new(Annotation::new(event_id, emoji.to_string()));
        let response = room.send(reaction).await?;

        let key = format!("{}:{}", message_id, emoji);
        self.reaction_events
            .write()
            .await
            .insert(key, response.event_id.to_string());

        Ok(())
    }

    async fn remove_reaction(
        &self,
        _channel_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> anyhow::Result<()> {
        let key = format!("{}:{}", message_id, emoji);
        let reaction_event_id = self.reaction_events.write().await.remove(&key);

        if let Some(reaction_event_id) = reaction_event_id {
            let client = self.matrix_client().await?;
            let target_room_id = self.target_room_id().await?;
            let target_room: OwnedRoomId = target_room_id.parse()?;

            let room = client
                .get_room(&target_room)
                .ok_or_else(|| anyhow::anyhow!("Matrix room not found for reaction removal"))?;

            let event_id: OwnedEventId = reaction_event_id
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid reaction event ID: {}", reaction_event_id))?;

            room.redact(&event_id, None, None).await?;
        }

        Ok(())
    }

    async fn pin_message(&self, _channel_id: &str, message_id: &str) -> anyhow::Result<()> {
        let room_id = self.target_room_id().await?;
        let encoded_room = Self::encode_path_segment(&room_id);

        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/state/m.room.pinned_events",
            self.homeserver, encoded_room
        );
        let resp = self
            .http_client
            .get(&url)
            .header("Authorization", self.auth_header_value())
            .send()
            .await?;

        let mut pinned: Vec<String> = if resp.status().is_success() {
            let body: serde_json::Value = resp.json().await?;
            body.get("pinned")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        let msg_id = message_id.to_string();
        if pinned.contains(&msg_id) {
            return Ok(());
        }
        pinned.push(msg_id);

        let put_url = format!(
            "{}/_matrix/client/v3/rooms/{}/state/m.room.pinned_events",
            self.homeserver, encoded_room
        );
        let body = serde_json::json!({ "pinned": pinned });
        let resp = self
            .http_client
            .put(&put_url)
            .header("Authorization", self.auth_header_value())
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("Matrix pin_message failed: {err}");
        }

        Ok(())
    }

    async fn unpin_message(&self, _channel_id: &str, message_id: &str) -> anyhow::Result<()> {
        let room_id = self.target_room_id().await?;
        let encoded_room = Self::encode_path_segment(&room_id);

        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/state/m.room.pinned_events",
            self.homeserver, encoded_room
        );
        let resp = self
            .http_client
            .get(&url)
            .header("Authorization", self.auth_header_value())
            .send()
            .await?;

        if !resp.status().is_success() {
            return Ok(());
        }

        let body: serde_json::Value = resp.json().await?;
        let mut pinned: Vec<String> = body
            .get("pinned")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let msg_id = message_id.to_string();
        let original_len = pinned.len();
        pinned.retain(|id| id != &msg_id);

        if pinned.len() == original_len {
            return Ok(());
        }

        let put_url = format!(
            "{}/_matrix/client/v3/rooms/{}/state/m.room.pinned_events",
            self.homeserver, encoded_room
        );
        let body = serde_json::json!({ "pinned": pinned });
        let resp = self
            .http_client
            .put(&put_url)
            .header("Authorization", self.auth_header_value())
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("Matrix unpin_message failed: {err}");
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_channel() -> MatrixChannel {
        MatrixChannel::new(
            "https://matrix.org".to_string(),
            "syt_test_token".to_string(),
            "!room:matrix.org".to_string(),
            vec!["@user:matrix.org".to_string()],
        )
    }

    #[test]
    fn creates_with_correct_fields() {
        let ch = make_channel();
        assert_eq!(ch.homeserver, "https://matrix.org");
        assert_eq!(ch.access_token, "syt_test_token");
        assert_eq!(ch.room_id, "!room:matrix.org");
        assert_eq!(ch.allowed_users.len(), 1);
    }

    #[test]
    fn strips_trailing_slash() {
        let ch = MatrixChannel::new(
            "https://matrix.org/".to_string(),
            "tok".to_string(),
            "!r:m".to_string(),
            vec![],
        );
        assert_eq!(ch.homeserver, "https://matrix.org");
    }

    #[test]
    fn no_trailing_slash_unchanged() {
        let ch = MatrixChannel::new(
            "https://matrix.org".to_string(),
            "tok".to_string(),
            "!r:m".to_string(),
            vec![],
        );
        assert_eq!(ch.homeserver, "https://matrix.org");
    }

    #[test]
    fn multiple_trailing_slashes_strip_all() {
        let ch = MatrixChannel::new(
            "https://matrix.org//".to_string(),
            "tok".to_string(),
            "!r:m".to_string(),
            vec![],
        );
        assert_eq!(ch.homeserver, "https://matrix.org");
    }

    #[test]
    fn trims_access_token() {
        let ch = MatrixChannel::new(
            "https://matrix.org".to_string(),
            "  syt_test_token  ".to_string(),
            "!r:m".to_string(),
            vec![],
        );
        assert_eq!(ch.access_token, "syt_test_token");
    }

    #[test]
    fn session_hints_are_normalized() {
        let ch = MatrixChannel::new_with_session_hint(
            "https://matrix.org".to_string(),
            "tok".to_string(),
            "!r:m".to_string(),
            vec![],
            Some("  @bot:matrix.org ".to_string()),
            Some("  DEVICE123  ".to_string()),
        );

        assert_eq!(ch.session_owner_hint.as_deref(), Some("@bot:matrix.org"));
        assert_eq!(ch.session_device_id_hint.as_deref(), Some("DEVICE123"));
    }

    #[test]
    fn empty_session_hints_are_ignored() {
        let ch = MatrixChannel::new_with_session_hint(
            "https://matrix.org".to_string(),
            "tok".to_string(),
            "!r:m".to_string(),
            vec![],
            Some("   ".to_string()),
            Some(String::new()),
        );

        assert!(ch.session_owner_hint.is_none());
        assert!(ch.session_device_id_hint.is_none());
    }

    #[test]
    fn matrix_store_dir_is_derived_from_zeroclaw_dir() {
        let ch = MatrixChannel::new_with_session_hint_and_zeroclaw_dir(
            "https://matrix.org".to_string(),
            "tok".to_string(),
            "!r:m".to_string(),
            vec![],
            None,
            None,
            Some(PathBuf::from("/tmp/zeroclaw")),
        );

        assert_eq!(
            ch.matrix_store_dir(),
            Some(PathBuf::from("/tmp/zeroclaw/state/matrix"))
        );
    }

    #[test]
    fn matrix_store_dir_absent_without_zeroclaw_dir() {
        let ch = MatrixChannel::new_with_session_hint(
            "https://matrix.org".to_string(),
            "tok".to_string(),
            "!r:m".to_string(),
            vec![],
            None,
            None,
        );

        assert!(ch.matrix_store_dir().is_none());
    }

    #[test]
    fn encode_path_segment_encodes_room_refs() {
        assert_eq!(
            MatrixChannel::encode_path_segment("#ops:matrix.example.com"),
            "%23ops%3Amatrix.example.com"
        );
        assert_eq!(
            MatrixChannel::encode_path_segment("!room:matrix.example.com"),
            "%21room%3Amatrix.example.com"
        );
    }

    #[test]
    fn supported_message_type_detection() {
        assert!(MatrixChannel::is_supported_message_type("m.text"));
        assert!(MatrixChannel::is_supported_message_type("m.notice"));
        assert!(!MatrixChannel::is_supported_message_type("m.image"));
        assert!(!MatrixChannel::is_supported_message_type("m.file"));
    }

    #[test]
    fn body_presence_detection() {
        assert!(MatrixChannel::has_non_empty_body("hello"));
        assert!(MatrixChannel::has_non_empty_body("  hello  "));
        assert!(!MatrixChannel::has_non_empty_body(""));
        assert!(!MatrixChannel::has_non_empty_body("   \n\t  "));
    }

    #[test]
    fn send_content_uses_markdown_formatting() {
        let content = RoomMessageEventContent::text_markdown("**hello**");
        let value = serde_json::to_value(content).unwrap();

        assert_eq!(value["msgtype"], "m.text");
        assert_eq!(value["body"], "**hello**");
        assert_eq!(value["format"], "org.matrix.custom.html");
        assert!(value["formatted_body"]
            .as_str()
            .unwrap_or_default()
            .contains("<strong>hello</strong>"));
    }

    #[test]
    fn sync_filter_for_room_targets_requested_room() {
        let filter = MatrixChannel::sync_filter_for_room("!room:matrix.org", 0);
        let value: serde_json::Value = serde_json::from_str(&filter).unwrap();

        assert_eq!(value["room"]["rooms"][0], "!room:matrix.org");
        assert_eq!(value["room"]["timeline"]["limit"], 1);
    }

    #[test]
    fn event_id_cache_deduplicates_and_evicts_old_entries() {
        let mut recent_order = std::collections::VecDeque::new();
        let mut recent_lookup = std::collections::HashSet::new();

        assert!(!MatrixChannel::cache_event_id(
            "$first:event",
            &mut recent_order,
            &mut recent_lookup
        ));
        assert!(MatrixChannel::cache_event_id(
            "$first:event",
            &mut recent_order,
            &mut recent_lookup
        ));

        for i in 0..2050 {
            let event_id = format!("$event-{i}:matrix");
            MatrixChannel::cache_event_id(&event_id, &mut recent_order, &mut recent_lookup);
        }

        assert!(!MatrixChannel::cache_event_id(
            "$first:event",
            &mut recent_order,
            &mut recent_lookup
        ));
    }

    #[test]
    fn trims_room_id_and_allowed_users() {
        let ch = MatrixChannel::new(
            "https://matrix.org".to_string(),
            "tok".to_string(),
            "  !room:matrix.org  ".to_string(),
            vec![
                "  @user:matrix.org  ".to_string(),
                "   ".to_string(),
                "@other:matrix.org".to_string(),
            ],
        );

        assert_eq!(ch.room_id, "!room:matrix.org");
        assert_eq!(ch.allowed_users.len(), 2);
        assert!(ch.allowed_users.contains(&"@user:matrix.org".to_string()));
        assert!(ch.allowed_users.contains(&"@other:matrix.org".to_string()));
    }

    #[test]
    fn wildcard_allows_anyone() {
        let ch = MatrixChannel::new(
            "https://m.org".to_string(),
            "tok".to_string(),
            "!r:m".to_string(),
            vec!["*".to_string()],
        );
        assert!(ch.is_user_allowed("@anyone:matrix.org"));
        assert!(ch.is_user_allowed("@hacker:evil.org"));
    }

    #[test]
    fn specific_user_allowed() {
        let ch = make_channel();
        assert!(ch.is_user_allowed("@user:matrix.org"));
    }

    #[test]
    fn unknown_user_denied() {
        let ch = make_channel();
        assert!(!ch.is_user_allowed("@stranger:matrix.org"));
        assert!(!ch.is_user_allowed("@evil:hacker.org"));
    }

    #[test]
    fn user_case_insensitive() {
        let ch = MatrixChannel::new(
            "https://m.org".to_string(),
            "tok".to_string(),
            "!r:m".to_string(),
            vec!["@User:Matrix.org".to_string()],
        );
        assert!(ch.is_user_allowed("@user:matrix.org"));
        assert!(ch.is_user_allowed("@USER:MATRIX.ORG"));
    }

    #[test]
    fn empty_allowlist_denies_all() {
        let ch = MatrixChannel::new(
            "https://m.org".to_string(),
            "tok".to_string(),
            "!r:m".to_string(),
            vec![],
        );
        assert!(!ch.is_user_allowed("@anyone:matrix.org"));
    }

    #[test]
    fn name_returns_matrix() {
        let ch = make_channel();
        assert_eq!(ch.name(), "matrix");
    }

    #[test]
    fn sync_response_deserializes_empty() {
        let json = r#"{"next_batch":"s123","rooms":{"join":{}}}"#;
        let resp: SyncResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.next_batch, "s123");
        assert!(resp.rooms.join.is_empty());
    }

    #[test]
    fn sync_response_deserializes_with_events() {
        let json = r#"{
            "next_batch": "s456",
            "rooms": {
                "join": {
                    "!room:matrix.org": {
                        "timeline": {
                            "events": [
                                {
                                    "type": "m.room.message",
                                    "event_id": "$event:matrix.org",
                                    "sender": "@user:matrix.org",
                                    "content": {
                                        "msgtype": "m.text",
                                        "body": "Hello!"
                                    }
                                }
                            ]
                        }
                    }
                }
            }
        }"#;
        let resp: SyncResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.next_batch, "s456");
        let room = resp.rooms.join.get("!room:matrix.org").unwrap();
        assert_eq!(room.timeline.events.len(), 1);
        assert_eq!(room.timeline.events[0].sender, "@user:matrix.org");
        assert_eq!(
            room.timeline.events[0].event_id.as_deref(),
            Some("$event:matrix.org")
        );
        assert_eq!(
            room.timeline.events[0].content.body.as_deref(),
            Some("Hello!")
        );
        assert_eq!(
            room.timeline.events[0].content.msgtype.as_deref(),
            Some("m.text")
        );
    }

    #[test]
    fn sync_response_ignores_non_text_events() {
        let json = r#"{
            "next_batch": "s789",
            "rooms": {
                "join": {
                    "!room:m": {
                        "timeline": {
                            "events": [
                                {
                                    "type": "m.room.member",
                                    "sender": "@user:m",
                                    "content": {}
                                }
                            ]
                        }
                    }
                }
            }
        }"#;
        let resp: SyncResponse = serde_json::from_str(json).unwrap();
        let room = resp.rooms.join.get("!room:m").unwrap();
        assert_eq!(room.timeline.events[0].event_type, "m.room.member");
        assert!(room.timeline.events[0].content.body.is_none());
    }

    #[test]
    fn whoami_response_deserializes() {
        let json = r#"{"user_id":"@bot:matrix.org"}"#;
        let resp: WhoAmIResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.user_id, "@bot:matrix.org");
    }

    #[test]
    fn event_content_defaults() {
        let json = r#"{"type":"m.room.message","sender":"@u:m","content":{}}"#;
        let event: TimelineEvent = serde_json::from_str(json).unwrap();
        assert!(event.content.body.is_none());
        assert!(event.content.msgtype.is_none());
    }

    #[test]
    fn event_content_supports_notice_msgtype() {
        let json = r#"{
            "type":"m.room.message",
            "sender":"@u:m",
            "event_id":"$notice:m",
            "content":{"msgtype":"m.notice","body":"Heads up"}
        }"#;
        let event: TimelineEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.content.msgtype.as_deref(), Some("m.notice"));
        assert_eq!(event.content.body.as_deref(), Some("Heads up"));
        assert_eq!(event.event_id.as_deref(), Some("$notice:m"));
    }

    #[tokio::test]
    async fn invalid_room_reference_fails_fast() {
        let ch = MatrixChannel::new(
            "https://matrix.org".to_string(),
            "tok".to_string(),
            "room_without_prefix".to_string(),
            vec![],
        );

        let err = ch.resolve_room_id().await.unwrap_err();
        assert!(err
            .to_string()
            .contains("must start with '!' (room ID) or '#' (room alias)"));
    }

    #[tokio::test]
    async fn target_room_id_keeps_canonical_room_id_without_lookup() {
        let ch = MatrixChannel::new(
            "https://matrix.org".to_string(),
            "tok".to_string(),
            "!canonical:matrix.org".to_string(),
            vec![],
        );

        let room_id = ch.target_room_id().await.unwrap();
        assert_eq!(room_id, "!canonical:matrix.org");
    }

    #[tokio::test]
    async fn target_room_id_uses_cached_alias_resolution() {
        let ch = MatrixChannel::new(
            "https://matrix.org".to_string(),
            "tok".to_string(),
            "#ops:matrix.org".to_string(),
            vec![],
        );

        *ch.resolved_room_id_cache.write().await = Some("!cached:matrix.org".to_string());
        let room_id = ch.target_room_id().await.unwrap();
        assert_eq!(room_id, "!cached:matrix.org");
    }

    #[test]
    fn sync_response_missing_rooms_defaults() {
        let json = r#"{"next_batch":"s0"}"#;
        let resp: SyncResponse = serde_json::from_str(json).unwrap();
        assert!(resp.rooms.join.is_empty());
    }
}
