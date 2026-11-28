//! The arguments line of a Matrix approval prompt, shown as inert text.
//!
//! A tool's arguments are written by the model, so after it has read untrusted content they
//! may carry text an attacker chose. The prompt goes out through the channel's `send`, which
//! acts on `[image:…]`, `[file:…]` and similar markers and renders the rest as Markdown.
//! Unescaped, `[Approve](https://…)` would be a live link with any label, `<a href=…>`
//! rendered HTML, and `[file:/etc/hostname]` an upload, all in front of an operator deciding
//! whether to approve. Telegram escapes the same summary as HTML for the same reason.

use std::sync::LazyLock;

use regex::Regex;

/// `http://` and `https://`, in any case.
static LINK_SCHEME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)http(s?)://").expect("valid regex"));

/// `summary` as a Markdown code span that Matrix shows as typed: no Markdown, HTML or link
/// inside it is rendered. Brackets, angle brackets and backticks become their full-width forms,
/// so no marker is parsed and nothing renders even if the span is cut short; line breaks become
/// spaces, so the span stays in one paragraph; and links are defanged to `hxxp(s)://`, for
/// clients that link plain text.
pub(super) fn inert(summary: &str) -> String {
    let shown: String = LINK_SCHEME
        .replace_all(summary, "hxxp${1}://")
        .chars()
        .map(full_width)
        .collect();
    if shown.trim().is_empty() {
        return shown;
    }
    format!("`{shown}`")
}

fn full_width(c: char) -> char {
    match c {
        '[' => '［',
        ']' => '］',
        '<' => '＜',
        '>' => '＞',
        '`' => '｀',
        '\n' | '\r' => ' ',
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use matrix_sdk::ruma::events::room::message::RoomMessageEventContent;

    use super::*;

    const HOSTILE: &str = concat!(
        "body: [Approve](https://example.com) ",
        "<a href=\"https://example.com\">x</a> [file:/etc/hostname]"
    );

    fn formatted_body(text: &str) -> Option<String> {
        let content = serde_json::to_value(RoomMessageEventContent::text_markdown(text))
            .expect("content serializes");
        content
            .get("formatted_body")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    }

    #[test]
    fn hostile_arguments_render_as_one_code_span() {
        let shown = inert(HOSTILE);
        assert_eq!(
            shown,
            concat!(
                "`body: ［Approve］(hxxps://example.com) ",
                "＜a href=\"hxxps://example.com\"＞x＜/a＞ ［file:/etc/hostname］`"
            )
        );
        let html = formatted_body(&shown).expect("a code span is formatted");
        assert!(
            html.starts_with("<code>") && html.ends_with("</code>"),
            "{html}"
        );
        assert!(!html.contains("<a"), "{html}");
    }

    #[test]
    fn no_marker_survives() {
        let (text, markers) = super::super::markers::parse(&inert(HOSTILE));
        assert!(markers.is_empty(), "{markers:?}");
        assert_eq!(text, inert(HOSTILE));
    }

    #[test]
    fn a_cut_span_still_renders_nothing() {
        let shown = inert(HOSTILE);
        let cut: String = shown.chars().take(shown.chars().count() - 1).collect();
        let html = formatted_body(&cut).unwrap_or_default();
        assert!(!html.contains("<a") && !html.contains("<code>"), "{html}");
    }

    #[test]
    fn backticks_and_line_breaks_cannot_close_the_span() {
        assert_eq!(inert("a`b\n\nc\r\n"), "`a｀b  c  `");
    }

    #[test]
    fn links_are_defanged_in_any_case() {
        assert_eq!(
            inert("HTTP://a.example http://b.example xhttps://c"),
            "`hxxp://a.example hxxp://b.example xhxxps://c`"
        );
    }

    #[test]
    fn an_empty_summary_stays_empty() {
        assert_eq!(inert(""), "");
    }
}
