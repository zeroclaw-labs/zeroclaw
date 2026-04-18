//! One2X hooks for the channels crate.
//!
//! Contains session hygiene, tool pairing repair, and agent hooks adapted
//! for the workspace crate architecture.

use std::sync::{Arc, OnceLock};

use zeroclaw_api::channel::{Channel, ChannelMessage};
use zeroclaw_config::schema::Config;

/// Root-crate injected channel descriptor.
///
/// The channels crate cannot depend on the root crate, so One2X-specific
/// channels (such as the Web channel that lives in `src/one2x/web_channel.rs`)
/// cross this boundary through a small IoC hook.
pub struct InjectedChannel {
    pub display_name: &'static str,
    pub channel: Arc<dyn Channel>,
}

type ExtraChannelsFn = Box<dyn Fn(&Config) -> Vec<InjectedChannel> + Send + Sync + 'static>;
type MessageBusReadyFn =
    Box<dyn Fn(&Config, tokio::sync::mpsc::Sender<ChannelMessage>) + Send + Sync + 'static>;

/// Root-crate hooks for extending the channel orchestrator.
pub struct ChannelHooks {
    pub extra_channels: ExtraChannelsFn,
    pub on_message_bus_ready: MessageBusReadyFn,
}

static CHANNEL_HOOKS: OnceLock<ChannelHooks> = OnceLock::new();

/// Register root-crate channel hooks exactly once at process startup.
pub fn register_channel_hooks(hooks: ChannelHooks) {
    if CHANNEL_HOOKS.set(hooks).is_err() {
        tracing::warn!(
            "one2x::register_channel_hooks called more than once; only the first registration is active"
        );
    }
}

/// Return extra channels injected by the root crate.
pub fn extra_channels(config: &Config) -> Vec<InjectedChannel> {
    CHANNEL_HOOKS
        .get()
        .map(|hooks| (hooks.extra_channels)(config))
        .unwrap_or_default()
}

/// Notify the root crate that the channel message bus is ready.
pub fn notify_message_bus_ready(config: &Config, tx: tokio::sync::mpsc::Sender<ChannelMessage>) {
    if let Some(hooks) = CHANNEL_HOOKS.get() {
        (hooks.on_message_bus_ready)(config, tx);
    }
}

/// Whether One2X root-crate hooks have been registered.
pub fn hooks_registered() -> bool {
    CHANNEL_HOOKS.get().is_some()
}

pub mod session_hygiene {
    use std::fs;
    use std::io::{BufRead, BufReader, Write};
    use std::path::Path;
    use zeroclaw_api::provider::ChatMessage;

    const MAX_TOOL_RESULT_SESSION_CHARS: usize = 2_000;
    const MAX_SESSION_FILE_BYTES: u64 = 500 * 1024;

    pub fn trim_tool_result_for_session(msg: &ChatMessage) -> ChatMessage {
        if msg.role != "tool" || msg.content.len() <= MAX_TOOL_RESULT_SESSION_CHARS {
            return msg.clone();
        }
        let keep_head = MAX_TOOL_RESULT_SESSION_CHARS * 2 / 3;
        let keep_tail = MAX_TOOL_RESULT_SESSION_CHARS / 3;
        let omitted = msg.content.len() - keep_head - keep_tail;
        let mut eh = keep_head;
        while eh > 0 && !msg.content.is_char_boundary(eh) {
            eh -= 1;
        }
        let mut st = msg.content.len() - keep_tail;
        while st < msg.content.len() && !msg.content.is_char_boundary(st) {
            st += 1;
        }
        tracing::debug!(
            original = msg.content.len(),
            "Trimmed large tool result for session persistence"
        );
        ChatMessage {
            role: msg.role.clone(),
            content: format!(
                "{}... [{} chars omitted] ...{}",
                &msg.content[..eh],
                omitted,
                &msg.content[st..]
            ),
        }
    }

    pub fn repair_session_messages(msgs: &mut Vec<ChatMessage>) {
        let before = msgs.len();
        msgs.retain(|m| !m.content.trim().is_empty());
        while !msgs.is_empty() && msgs[0].role == "tool" {
            msgs.remove(0);
        }
        let mut i = 0;
        while i < msgs.len() {
            if msgs[i].content.contains("[CONTEXT SUMMARY") {
                while i + 1 < msgs.len() && msgs[i + 1].role == "tool" {
                    msgs.remove(i + 1);
                }
            }
            i += 1;
        }
        let removed = before - msgs.len();
        if removed > 0 {
            tracing::info!(removed, "Repaired session: removed broken messages");
        }
    }

    pub fn truncate_session_file(session_path: &Path, keep_last_n: usize) -> std::io::Result<bool> {
        if !session_path.exists() {
            return Ok(false);
        }
        let metadata = fs::metadata(session_path)?;
        if metadata.len() < MAX_SESSION_FILE_BYTES {
            return Ok(false);
        }
        let file = fs::File::open(session_path)?;
        let reader = BufReader::new(file);
        let lines: Vec<String> = reader.lines().collect::<Result<Vec<_>, _>>()?;
        if lines.len() <= keep_last_n {
            return Ok(false);
        }
        let keep_from = lines.len() - keep_last_n;
        let kept_lines = &lines[keep_from..];
        let tmp_path = session_path.with_extension("jsonl.tmp");
        {
            let mut tmp = fs::File::create(&tmp_path)?;
            writeln!(
                tmp,
                r#"{{"_compacted":true,"dropped":{},"kept":{},"timestamp":"{}"}}"#,
                keep_from,
                keep_last_n,
                chrono::Utc::now().to_rfc3339()
            )?;
            for line in kept_lines {
                writeln!(tmp, "{}", line)?;
            }
            tmp.flush()?;
        }
        fs::rename(&tmp_path, session_path)?;
        tracing::info!(
            path = %session_path.display(),
            dropped = keep_from,
            kept = keep_last_n,
            "Session file truncated after compaction"
        );
        Ok(true)
    }
}

pub mod tool_pairing {
    use zeroclaw_api::provider::ChatMessage;

    const SYNTHETIC_TOOL_RESULT: &str = "[Tool result missing — internal error]";

    fn extract_tool_use_ids(content: &str) -> Vec<String> {
        let trimmed = content.trim();
        let mut ids = Vec::new();
        if trimmed.starts_with('[') {
            if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(trimmed) {
                for v in &arr {
                    if v.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                        if let Some(id) = v.get("id").and_then(|i| i.as_str()) {
                            ids.push(id.to_string());
                        }
                    }
                }
            }
        } else if trimmed.starts_with('{') {
            if let Ok(obj) = serde_json::from_str::<serde_json::Value>(trimmed) {
                if let Some(calls) = obj.get("tool_calls").and_then(|c| c.as_array()) {
                    for call in calls {
                        if let Some(id) = call.get("id").and_then(|i| i.as_str()) {
                            ids.push(id.to_string());
                        }
                    }
                }
            }
        }
        ids
    }

    fn extract_tool_result_id(content: &str) -> Option<String> {
        let trimmed = content.trim();
        if trimmed.starts_with('{') {
            if let Ok(obj) = serde_json::from_str::<serde_json::Value>(trimmed) {
                return obj
                    .get("tool_call_id")
                    .and_then(|i| i.as_str())
                    .map(|s| s.to_string());
            }
        }
        None
    }

    pub fn repair_tool_pairing(history: &mut Vec<ChatMessage>) {
        if history.len() < 2 {
            return;
        }
        let mut repaired = Vec::with_capacity(history.len() + 4);
        let mut i = 0;
        let mut total_injected = 0usize;
        let mut total_removed = 0usize;

        while i < history.len() {
            let msg = &history[i];
            if msg.role != "assistant" {
                if msg.role == "tool"
                    && (repaired.is_empty()
                        || repaired
                            .last()
                            .map_or(true, |m: &ChatMessage| m.role != "assistant"))
                {
                    total_removed += 1;
                    i += 1;
                    continue;
                }
                repaired.push(msg.clone());
                i += 1;
                continue;
            }
            let tool_use_ids = extract_tool_use_ids(&msg.content);
            if tool_use_ids.is_empty() {
                repaired.push(msg.clone());
                i += 1;
                continue;
            }
            repaired.push(msg.clone());
            i += 1;
            let mut matched_ids = std::collections::HashSet::new();
            let mut seen_result_ids = std::collections::HashSet::new();
            let tool_use_id_set: std::collections::HashSet<_> =
                tool_use_ids.iter().cloned().collect();
            while i < history.len() && history[i].role == "tool" {
                if let Some(result_id) = extract_tool_result_id(&history[i].content) {
                    if tool_use_id_set.contains(&result_id) && !seen_result_ids.contains(&result_id)
                    {
                        matched_ids.insert(result_id.clone());
                        seen_result_ids.insert(result_id);
                        repaired.push(history[i].clone());
                    } else {
                        total_removed += 1;
                    }
                } else {
                    repaired.push(history[i].clone());
                }
                i += 1;
            }
            for use_id in &tool_use_ids {
                if !matched_ids.contains(use_id) {
                    let synthetic = serde_json::json!({
                        "tool_call_id": use_id,
                        "content": SYNTHETIC_TOOL_RESULT,
                    });
                    repaired.push(ChatMessage::tool(synthetic.to_string()));
                    total_injected += 1;
                }
            }
        }

        if total_injected > 0 || total_removed > 0 {
            tracing::info!(
                injected = total_injected,
                removed = total_removed,
                "repair_tool_pairing: fixed tool_use/tool_result mismatches"
            );
            *history = repaired;
        }
    }
}

pub mod agent_hooks {
    pub fn detect_fast_approval(user_message: &str) -> Option<&'static str> {
        let trimmed = user_message.trim().to_lowercase();
        let approvals = [
            "ok",
            "好",
            "好的",
            "可以",
            "行",
            "执行",
            "做吧",
            "开始",
            "确认",
            "go",
            "yes",
            "do it",
            "proceed",
            "continue",
            "sure",
            "yep",
            "yeah",
            "confirmed",
            "approve",
            "agreed",
            "lgtm",
        ];
        if approvals.iter().any(|a| trimmed == *a) {
            Some(
                "The user approved. Execute the previously discussed plan immediately. \
                 Do not re-explain the plan — take the first action now.",
            )
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::CliChannel;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static MESSAGE_BUS_READY_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[test]
    fn registered_channel_hooks_expose_channels_and_message_bus_callback() {
        MESSAGE_BUS_READY_CALLS.store(0, Ordering::Relaxed);

        register_channel_hooks(ChannelHooks {
            extra_channels: Box::new(|_| {
                vec![InjectedChannel {
                    display_name: "CLI",
                    channel: Arc::new(CliChannel::new()),
                }]
            }),
            on_message_bus_ready: Box::new(|_, _| {
                MESSAGE_BUS_READY_CALLS.fetch_add(1, Ordering::Relaxed);
            }),
        });

        let extras = extra_channels(&Config::default());
        assert_eq!(extras.len(), 1);
        assert_eq!(extras[0].display_name, "CLI");
        assert!(hooks_registered());

        let (tx, _rx) = tokio::sync::mpsc::channel::<ChannelMessage>(1);
        notify_message_bus_ready(&Config::default(), tx);
        assert_eq!(MESSAGE_BUS_READY_CALLS.load(Ordering::Relaxed), 1);
    }
}
