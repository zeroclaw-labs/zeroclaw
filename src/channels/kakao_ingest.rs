//! Parsers for KakaoTalk-forwarded content arriving in the bot's 1:1 chat.
//!
//! Two real-world shapes show up when a user forwards group-chat content
//! into the MoA bot via KakaoTalk's native share sheet:
//!
//! 1. **Multi-select share**: the user long-presses several messages,
//!    selects them, taps Share, and picks the MoA channel. Kakao
//!    concatenates those messages into one utterance with a per-line
//!    sender tag prefix. Detection: ≥ 2 lines that look like
//!    `[sender] [HH:MM] message` or `[sender] [오전/오후 H:MM] message`.
//!
//! 2. **Chat export (대화 내보내기)**: KakaoTalk PC/Android exports a
//!    full chat history as plaintext with a header line like
//!    `KakaoTalk Chats with <room>` and rows like
//!    `[sender] [YYYY. M. D. 오후 H:MM] message`. The user pastes
//!    that text (or a portion) into the 1:1 chat. Detection: contains
//!    a date-and-time row matching the export format, or starts with
//!    the export header.
//!
//! Both shapes are parsed into a [`KakaoIngest`] envelope so the
//! gateway can rewrite the raw utterance into a structured prompt
//! before handing it to the AI loop. When neither shape matches, the
//! parser returns `KakaoIngest::PlainText` and the original utterance
//! is forwarded unchanged.

use std::sync::OnceLock;

use regex::Regex;

/// Parsed inbound utterance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KakaoIngest {
    /// Original utterance — no Kakao-shaped content detected.
    PlainText(String),
    /// Multiple messages forwarded together via multi-select share.
    /// Each entry preserves its sender + timestamp string + body.
    ForwardedBatch {
        messages: Vec<ForwardedMessage>,
        /// Original utterance, kept verbatim for raw inspection.
        raw: String,
    },
    /// Chat export (`대화 내보내기`) text dump pasted by the user.
    ChatExport {
        room_label: Option<String>,
        messages: Vec<ForwardedMessage>,
        raw: String,
    },
}

/// One parsed forwarded message line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardedMessage {
    pub sender: String,
    /// Timestamp string preserved verbatim from the source ("오후 3:24",
    /// "2024. 4. 1. 오전 9:15", etc.). Not parsed into a typed value
    /// because formats differ across Android/iOS/PC and ZeroClaw
    /// downstream does not need a typed value in v1.
    pub timestamp_raw: String,
    pub body: String,
}

/// Conservative threshold for treating a multi-line utterance as a
/// forwarded batch rather than a single typed message.
const FORWARD_MIN_MESSAGES: usize = 2;

/// Hard cap on parsed messages per ingest. Beyond this the rest is
/// dropped — Kakao Skill payloads are bounded and the AI prompt would
/// not benefit from arbitrarily deep batches.
pub const MAX_INGEST_MESSAGES: usize = 200;

/// Public entry point. Inspects the utterance and returns the strongest
/// match. Order: chat export → forwarded batch → plain text.
pub fn parse_ingest(utterance: &str) -> KakaoIngest {
    let trimmed = utterance.trim();
    if trimmed.is_empty() {
        return KakaoIngest::PlainText(String::new());
    }

    if let Some(export) = try_parse_chat_export(trimmed) {
        return export;
    }

    if let Some(batch) = try_parse_forwarded_batch(trimmed) {
        return batch;
    }

    KakaoIngest::PlainText(utterance.to_string())
}

/// Format an ingest as a structured prompt prefix for the AI loop.
/// Returns the original utterance verbatim for `PlainText`.
pub fn render_for_prompt(ingest: &KakaoIngest) -> String {
    match ingest {
        KakaoIngest::PlainText(s) => s.clone(),
        KakaoIngest::ForwardedBatch { messages, .. } => {
            let mut out = String::new();
            out.push_str("[전달된 메시지 ");
            out.push_str(&messages.len().to_string());
            out.push_str("개]\n\n");
            for m in messages {
                out.push_str(&format_message_line(m));
                out.push('\n');
            }
            out
        }
        KakaoIngest::ChatExport {
            room_label,
            messages,
            ..
        } => {
            let mut out = String::new();
            out.push_str("[대화 내보내기");
            if let Some(label) = room_label {
                out.push_str(": ");
                out.push_str(label);
            }
            out.push_str(" — ");
            out.push_str(&messages.len().to_string());
            out.push_str("개 메시지]\n\n");
            for m in messages {
                out.push_str(&format_message_line(m));
                out.push('\n');
            }
            out
        }
    }
}

fn format_message_line(m: &ForwardedMessage) -> String {
    if m.timestamp_raw.is_empty() {
        format!("- {sender}: {body}", sender = m.sender, body = m.body)
    } else {
        format!(
            "- [{ts}] {sender}: {body}",
            ts = m.timestamp_raw,
            sender = m.sender,
            body = m.body
        )
    }
}

// ── Chat-export parsing ─────────────────────────────────────────────

/// Match the export header: "KakaoTalk Chats with <room>" or
/// the Korean variant "<room> 님과 카카오톡 대화".
fn export_header_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?im)^\s*(?:KakaoTalk Chats with\s+(?P<room_en>.+)|(?P<room_ko>.+?)\s*님과 카카오톡 대화)\s*$",
        )
        .expect("static export header regex must compile")
    })
}

/// Match a row like:
///   `[name] [2024. 4. 1. 오후 3:24] message body`
///   `[name] [2024. 12. 31. 오전 9:15] body with [brackets] inside`
fn export_row_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?m)^\[(?P<sender>[^\]\n]+)\]\s+\[(?P<ts>\d{4}\.\s*\d{1,2}\.\s*\d{1,2}\.\s*(?:오전|오후)\s+\d{1,2}:\d{2})\]\s+(?P<body>.+?)\s*$",
        )
        .expect("static export row regex must compile")
    })
}

fn try_parse_chat_export(utterance: &str) -> Option<KakaoIngest> {
    let row_re = export_row_regex();
    let mut messages = Vec::new();
    for cap in row_re.captures_iter(utterance) {
        if messages.len() >= MAX_INGEST_MESSAGES {
            break;
        }
        messages.push(ForwardedMessage {
            sender: cap["sender"].trim().to_string(),
            timestamp_raw: cap["ts"].to_string(),
            body: cap["body"].trim().to_string(),
        });
    }

    if messages.is_empty() {
        return None;
    }

    // Need either the explicit header OR multiple export rows to be
    // confident this is a chat-export paste rather than a single line
    // someone happened to format with a date-bracket prefix.
    let header_cap = export_header_regex().captures(utterance);
    if header_cap.is_none() && messages.len() < 2 {
        return None;
    }

    let room_label = header_cap.and_then(|c| {
        c.name("room_en")
            .or_else(|| c.name("room_ko"))
            .map(|m| m.as_str().trim().to_string())
            .filter(|s| !s.is_empty())
    });

    Some(KakaoIngest::ChatExport {
        room_label,
        messages,
        raw: utterance.to_string(),
    })
}

// ── Forwarded-batch parsing ─────────────────────────────────────────

/// Match a multi-select share line like:
///   `[name] [오후 3:24] body`
///   `[name] [3:24 PM] body`
///   `[name] [13:24] body`
fn forward_row_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?m)^\[(?P<sender>[^\]\n]+)\]\s+\[(?P<ts>(?:오전|오후)\s+\d{1,2}:\d{2}|\d{1,2}:\d{2}(?:\s*[AP]M)?)\]\s+(?P<body>.+?)\s*$",
        )
        .expect("static forward row regex must compile")
    })
}

fn try_parse_forwarded_batch(utterance: &str) -> Option<KakaoIngest> {
    let row_re = forward_row_regex();
    let mut messages = Vec::new();
    for cap in row_re.captures_iter(utterance) {
        if messages.len() >= MAX_INGEST_MESSAGES {
            break;
        }
        messages.push(ForwardedMessage {
            sender: cap["sender"].trim().to_string(),
            timestamp_raw: cap["ts"].to_string(),
            body: cap["body"].trim().to_string(),
        });
    }

    if messages.len() < FORWARD_MIN_MESSAGES {
        return None;
    }

    Some(KakaoIngest::ForwardedBatch {
        messages,
        raw: utterance.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_when_no_kakao_shape() {
        let utt = "안녕하세요, 모아. 어제 의뢰인이 뭐라 했지?";
        assert_eq!(parse_ingest(utt), KakaoIngest::PlainText(utt.to_string()));
    }

    #[test]
    fn empty_utterance_yields_empty_plaintext() {
        assert_eq!(parse_ingest("   "), KakaoIngest::PlainText(String::new()));
    }

    #[test]
    fn single_forwarded_line_is_not_a_batch() {
        // One line with a Kakao prefix is just a typed message that
        // happens to look formatted — don't promote to batch.
        let utt = "[김의뢰인] [오후 3:24] 내일 일정 어떻게 되나요?";
        let parsed = parse_ingest(utt);
        assert!(matches!(parsed, KakaoIngest::PlainText(_)), "{:?}", parsed);
    }

    #[test]
    fn forwarded_batch_two_rows_korean_am_pm() {
        let utt = "\
[김의뢰인] [오후 3:24] 내일 9시 가능합니다.\n\
[변호사] [오후 3:25] 네, 그럼 그 시각으로 잡겠습니다.";
        let parsed = parse_ingest(utt);
        match parsed {
            KakaoIngest::ForwardedBatch { messages, .. } => {
                assert_eq!(messages.len(), 2);
                assert_eq!(messages[0].sender, "김의뢰인");
                assert_eq!(messages[0].timestamp_raw, "오후 3:24");
                assert_eq!(messages[0].body, "내일 9시 가능합니다.");
                assert_eq!(messages[1].sender, "변호사");
            }
            other => panic!("expected ForwardedBatch, got {:?}", other),
        }
    }

    #[test]
    fn forwarded_batch_24h_format() {
        let utt = "\
[Alice] [13:24] hello\n\
[Bob] [13:25] hi back";
        let parsed = parse_ingest(utt);
        assert!(
            matches!(parsed, KakaoIngest::ForwardedBatch { .. }),
            "{:?}",
            parsed
        );
    }

    #[test]
    fn forwarded_batch_english_am_pm() {
        let utt = "\
[Alice] [3:24 PM] hello\n\
[Bob] [3:25 PM] hi back";
        let parsed = parse_ingest(utt);
        match parsed {
            KakaoIngest::ForwardedBatch { messages, .. } => {
                assert_eq!(messages.len(), 2);
                assert_eq!(messages[0].timestamp_raw, "3:24 PM");
            }
            other => panic!("expected ForwardedBatch, got {:?}", other),
        }
    }

    #[test]
    fn chat_export_with_korean_header() {
        let utt = "\
김의뢰인 님과 카카오톡 대화\n\
저장한 날짜 : 2024-04-01 18:00:00\n\n\
[김의뢰인] [2024. 4. 1. 오후 3:24] 내일 9시 가능합니다.\n\
[변호사] [2024. 4. 1. 오후 3:25] 네, 그럼 그 시각으로 잡겠습니다.\n\
[김의뢰인] [2024. 4. 1. 오후 3:30] 감사합니다.";
        let parsed = parse_ingest(utt);
        match parsed {
            KakaoIngest::ChatExport {
                room_label,
                messages,
                ..
            } => {
                assert_eq!(room_label.as_deref(), Some("김의뢰인"));
                assert_eq!(messages.len(), 3);
                assert_eq!(messages[2].body, "감사합니다.");
            }
            other => panic!("expected ChatExport, got {:?}", other),
        }
    }

    #[test]
    fn chat_export_with_english_header() {
        let utt = "\
KakaoTalk Chats with Project Team\n\
[Alice] [2024. 4. 1. 오후 3:24] morning standup notes\n\
[Bob] [2024. 4. 1. 오후 3:25] thanks";
        let parsed = parse_ingest(utt);
        match parsed {
            KakaoIngest::ChatExport {
                room_label,
                messages,
                ..
            } => {
                assert_eq!(room_label.as_deref(), Some("Project Team"));
                assert_eq!(messages.len(), 2);
            }
            other => panic!("expected ChatExport, got {:?}", other),
        }
    }

    #[test]
    fn chat_export_without_header_requires_two_rows() {
        // Two export-format rows but no header — accept as ChatExport
        // because the date stamp pattern is unambiguous.
        let utt = "\
[Alice] [2024. 4. 1. 오후 3:24] one\n\
[Bob] [2024. 4. 1. 오후 3:25] two";
        let parsed = parse_ingest(utt);
        assert!(
            matches!(parsed, KakaoIngest::ChatExport { .. }),
            "{:?}",
            parsed
        );
    }

    #[test]
    fn single_export_row_without_header_is_plaintext() {
        // One isolated export row could be a coincidence; don't promote.
        let utt = "[Alice] [2024. 4. 1. 오후 3:24] hello";
        let parsed = parse_ingest(utt);
        assert!(matches!(parsed, KakaoIngest::PlainText(_)), "{:?}", parsed);
    }

    #[test]
    fn batch_caps_at_max_ingest_messages() {
        use std::fmt::Write as _;
        let mut utt = String::new();
        for i in 0..(MAX_INGEST_MESSAGES + 50) {
            writeln!(utt, "[u{i}] [13:24] msg{i}").expect("writeln to String cannot fail");
        }
        let parsed = parse_ingest(&utt);
        match parsed {
            KakaoIngest::ForwardedBatch { messages, .. } => {
                assert_eq!(messages.len(), MAX_INGEST_MESSAGES);
            }
            other => panic!("expected ForwardedBatch, got {:?}", other),
        }
    }

    #[test]
    fn render_plain_text_returns_original() {
        let utt = "hello there";
        let parsed = parse_ingest(utt);
        assert_eq!(render_for_prompt(&parsed), utt);
    }

    #[test]
    fn render_forwarded_batch_includes_count_header_and_rows() {
        let utt = "\
[Alice] [13:24] one\n\
[Bob] [13:25] two";
        let parsed = parse_ingest(utt);
        let rendered = render_for_prompt(&parsed);
        assert!(rendered.contains("[전달된 메시지 2개]"));
        assert!(rendered.contains("[13:24] Alice: one"));
        assert!(rendered.contains("[13:25] Bob: two"));
    }

    #[test]
    fn render_chat_export_includes_room_and_count() {
        let utt = "\
김의뢰인 님과 카카오톡 대화\n\
[김의뢰인] [2024. 4. 1. 오후 3:24] one\n\
[변호사] [2024. 4. 1. 오후 3:25] two";
        let parsed = parse_ingest(utt);
        let rendered = render_for_prompt(&parsed);
        assert!(rendered.contains("[대화 내보내기: 김의뢰인 — 2개 메시지]"));
        assert!(rendered.contains("김의뢰인: one"));
    }

    #[test]
    fn body_with_brackets_is_preserved() {
        let utt = "\
[Alice] [13:24] file [report.pdf] please\n\
[Bob] [13:25] got it";
        let parsed = parse_ingest(utt);
        match parsed {
            KakaoIngest::ForwardedBatch { messages, .. } => {
                assert_eq!(messages[0].body, "file [report.pdf] please");
            }
            other => panic!("expected ForwardedBatch, got {:?}", other),
        }
    }

    #[test]
    fn raw_utterance_preserved_in_envelope() {
        let utt = "\
[Alice] [13:24] one\n\
[Bob] [13:25] two";
        let parsed = parse_ingest(utt);
        match parsed {
            KakaoIngest::ForwardedBatch { raw, .. } => assert_eq!(raw, utt),
            other => panic!("expected ForwardedBatch, got {:?}", other),
        }
    }
}
