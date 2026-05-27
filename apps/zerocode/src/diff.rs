use std::sync::OnceLock;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use similar::{ChangeTag, TextDiff};
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Style as SyntectStyle, Theme, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

// Diff background / foreground palette (unchanged from before)
const ADD_BG: Color = Color::Rgb(0, 40, 0);
const ADD_FG: Color = Color::Rgb(140, 240, 140);
const DEL_BG: Color = Color::Rgb(55, 0, 0);
const DEL_FG: Color = Color::Rgb(240, 140, 140);
const CTX_FG: Color = Color::Rgb(130, 130, 130);
const SEP_FG: Color = Color::Rgb(70, 70, 70);

const DIFF_CONTEXT: usize = 3;
const MAX_WRITE_LINES: usize = 60;

// ── Syntax highlighting ──────────────────────────────────────────

fn syntax_set() -> &'static SyntaxSet {
    static SS: OnceLock<SyntaxSet> = OnceLock::new();
    SS.get_or_init(two_face::syntax::extra_newlines)
}

fn highlight_theme() -> &'static Theme {
    static THEME: OnceLock<Theme> = OnceLock::new();
    THEME.get_or_init(|| {
        let ts = ThemeSet::from(two_face::theme::extra());
        ts.themes
            .get("Catppuccin Mocha")
            .cloned()
            .unwrap_or_else(|| ts.themes.into_values().next().unwrap_or_default())
    })
}

fn hl_color(c: syntect::highlighting::Color) -> Color {
    if c.a == 0x01 {
        Color::Reset
    } else if c.a == 0x00 {
        Color::Indexed(c.r)
    } else {
        Color::Rgb(c.r, c.g, c.b)
    }
}

fn hl_style(style: SyntectStyle, bg: Color) -> Style {
    let mut s = Style::default().fg(hl_color(style.foreground)).bg(bg);
    if style.font_style.contains(FontStyle::BOLD) {
        s = s.add_modifier(Modifier::BOLD);
    }
    s
}

/// Pre-highlight all lines of `text` for the given file extension.
/// Returns one `Vec<Span<'static>>` per line (indexed 0-based), with `bg`
/// forced as the background on every span.  Falls back to a single plain
/// span per line when the extension is unknown or highlighting fails.
fn highlight_all(text: &str, ext: &str, bg: Color, plain_fg: Color) -> Vec<Vec<Span<'static>>> {
    let ss = syntax_set();
    let Some(syntax) = ss
        .find_syntax_by_extension(ext)
        .or_else(|| ss.find_syntax_by_token(ext))
    else {
        return plain_line_spans(text, bg, plain_fg);
    };

    let theme = highlight_theme();
    let mut hl = HighlightLines::new(syntax, theme);

    LinesWithEndings::from(text)
        .map(|line| match hl.highlight_line(line, ss) {
            Ok(regions) => {
                let spans: Vec<Span<'static>> = regions
                    .into_iter()
                    .map(|(style, token)| {
                        Span::styled(
                            token.trim_end_matches('\n').to_string(),
                            hl_style(style, bg),
                        )
                    })
                    .filter(|s| !s.content.is_empty())
                    .collect();
                if spans.is_empty() {
                    vec![Span::styled(String::new(), Style::default().bg(bg))]
                } else {
                    spans
                }
            }
            Err(_) => vec![Span::styled(
                line.trim_end_matches('\n').to_string(),
                Style::default().bg(bg).fg(plain_fg),
            )],
        })
        .collect()
}

fn plain_line_spans(text: &str, bg: Color, fg: Color) -> Vec<Vec<Span<'static>>> {
    text.lines()
        .map(|l| vec![Span::styled(l.to_string(), Style::default().bg(bg).fg(fg))])
        .collect()
}

// ── Public diff API ──────────────────────────────────────────────

/// Build ratatui `Line`s for a unified diff of `old` vs `new`.
///
/// `lang` is an optional file extension (e.g. `"rs"`, `"py"`) used for
/// syntax highlighting.  Pass `None` to get plain colored diffs.
pub fn diff_lines(old: &str, new: &str, lang: Option<&str>) -> Vec<Line<'static>> {
    let diff = TextDiff::from_lines(old, new);
    let mut out: Vec<Line<'static>> = Vec::new();

    // Pre-highlight both sides in full so multi-line token state is correct.
    let (del_hl, add_hl) = match lang {
        Some(ext) => (
            Some(highlight_all(old, ext, DEL_BG, DEL_FG)),
            Some(highlight_all(new, ext, ADD_BG, ADD_FG)),
        ),
        None => (None, None),
    };

    for (gi, group) in diff.grouped_ops(DIFF_CONTEXT).iter().enumerate() {
        if gi > 0 {
            out.push(Line::from(Span::styled(
                "  \u{22ef}".to_string(),
                Style::default().fg(SEP_FG),
            )));
        }
        for op in group {
            for change in diff.iter_changes(op) {
                let text = change.value().trim_end_matches('\n').to_string();
                let line = match change.tag() {
                    ChangeTag::Delete => {
                        let content = del_hl
                            .as_ref()
                            .and_then(|v| change.old_index().and_then(|i| v.get(i)))
                            .cloned()
                            .unwrap_or_else(|| {
                                vec![Span::styled(text, Style::default().bg(DEL_BG).fg(DEL_FG))]
                            });
                        let lineno = change
                            .old_index()
                            .map(|n| format!("{} | ", n + 1))
                            .unwrap_or_else(|| "  | ".to_string());
                        let mut spans = vec![Span::styled(
                            lineno + "- ",
                            Style::default()
                                .bg(DEL_BG)
                                .fg(DEL_FG)
                                .add_modifier(Modifier::BOLD),
                        )];
                        spans.extend(content);
                        Line::from(spans).style(Style::default().bg(DEL_BG))
                    }
                    ChangeTag::Insert => {
                        let content = add_hl
                            .as_ref()
                            .and_then(|v| change.new_index().and_then(|i| v.get(i)))
                            .cloned()
                            .unwrap_or_else(|| {
                                vec![Span::styled(text, Style::default().bg(ADD_BG).fg(ADD_FG))]
                            });
                        let lineno = change
                            .new_index()
                            .map(|n| format!("{} | ", n + 1))
                            .unwrap_or_else(|| "  | ".to_string());
                        let mut spans = vec![Span::styled(
                            lineno + "+ ",
                            Style::default()
                                .bg(ADD_BG)
                                .fg(ADD_FG)
                                .add_modifier(Modifier::BOLD),
                        )];
                        spans.extend(content);
                        Line::from(spans).style(Style::default().bg(ADD_BG))
                    }
                    ChangeTag::Equal => {
                        let lineno = change
                            .old_index()
                            .map(|n| format!("{} | ", n + 1))
                            .unwrap_or_else(|| "  | ".to_string());
                        Line::from(Span::styled(
                            format!("{}  {}", lineno, text),
                            Style::default().fg(CTX_FG),
                        ))
                    }
                };
                out.push(line);
            }
        }
    }

    if out.is_empty() {
        out.push(Line::from(Span::styled(
            "  (no changes)".to_string(),
            Style::default().fg(SEP_FG),
        )));
    }

    out
}

/// Build ratatui `Line`s showing `content` as entirely new (file_write).
///
/// `lang` is an optional file extension for syntax highlighting.
/// Capped at `MAX_WRITE_LINES`; a `⋯ N more lines` trailer is appended
/// when the file is larger.
pub fn write_lines(content: &str, lang: Option<&str>) -> Vec<Line<'static>> {
    let all: Vec<&str> = content.lines().collect();
    let show = all.len().min(MAX_WRITE_LINES);

    let hl = lang.map(|ext| highlight_all(content, ext, ADD_BG, ADD_FG));
    let mut out: Vec<Line<'static>> = Vec::with_capacity(show + 1);

    for (i, item) in all.iter().enumerate().take(show) {
        let content_spans = hl
            .as_ref()
            .and_then(|v| v.get(i))
            .cloned()
            .unwrap_or_else(|| {
                vec![Span::styled(
                    item.to_string(),
                    Style::default().bg(ADD_BG).fg(ADD_FG),
                )]
            });
        let mut spans = vec![Span::styled(
            format!("{} | + ", i + 1),
            Style::default()
                .bg(ADD_BG)
                .fg(ADD_FG)
                .add_modifier(Modifier::BOLD),
        )];
        spans.extend(content_spans);
        out.push(Line::from(spans).style(Style::default().bg(ADD_BG)));
    }

    if all.len() > MAX_WRITE_LINES {
        out.push(Line::from(Span::styled(
            format!("  \u{22ef} {} more lines", all.len() - MAX_WRITE_LINES),
            Style::default().fg(SEP_FG),
        )));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_produces_add_and_delete_lines() {
        let lines = diff_lines("foo\nbar\n", "foo\nbaz\n", None);
        let rendered: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert!(
            rendered
                .iter()
                .any(|s| s.contains("- ") && s.contains("bar"))
        );
        assert!(
            rendered
                .iter()
                .any(|s| s.contains("+ ") && s.contains("baz"))
        );
    }

    #[test]
    fn diff_no_changes_returns_placeholder() {
        let lines = diff_lines("same\n", "same\n", None);
        let all: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(all.contains("no changes"));
    }

    #[test]
    fn write_lines_caps_at_max() {
        let content: String = (0..100).map(|i| format!("line {i}\n")).collect();
        let lines = write_lines(&content, None);
        let last: String = lines
            .last()
            .unwrap()
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(last.contains("more lines"), "expected trailer, got: {last}");
        assert_eq!(lines.len(), MAX_WRITE_LINES + 1);
    }

    #[test]
    fn diff_delete_line_has_red_bg() {
        let lines = diff_lines("old line\n", "new line\n", None);
        let del_line = lines
            .iter()
            .find(|l| {
                l.spans
                    .first()
                    .map(|s| s.content.as_ref().ends_with("- "))
                    .unwrap_or(false)
            })
            .expect("should have a delete line");
        assert_eq!(del_line.style.bg, Some(DEL_BG));
    }

    #[test]
    fn diff_insert_line_has_green_bg() {
        let lines = diff_lines("old line\n", "new line\n", None);
        let ins_line = lines
            .iter()
            .find(|l| {
                l.spans
                    .first()
                    .map(|s| s.content.as_ref().ends_with("+ "))
                    .unwrap_or(false)
            })
            .expect("should have an insert line");
        assert_eq!(ins_line.style.bg, Some(ADD_BG));
    }

    #[test]
    fn diff_rust_syntax_highlighting_applies() {
        let old = "fn foo() {}\n";
        let new = "fn bar() {}\n";
        let lines = diff_lines(old, new, Some("rs"));
        // With syntax highlighting, the delete and insert lines should have
        // multiple spans (keyword, space, identifier, …) rather than one.
        let del = lines
            .iter()
            .find(|l| {
                l.spans
                    .first()
                    .map(|s| s.content.as_ref().ends_with("- "))
                    .unwrap_or(false)
            })
            .expect("delete line");
        assert!(
            del.spans.len() > 2,
            "expected multiple spans from syntax highlighting, got {}",
            del.spans.len()
        );
    }

    #[test]
    fn test_diff_lines_shows_left_aligned_line_numbers() {
        let old = "line one\nline two\n";
        let new = "line one\nline three\n";
        let lines = diff_lines(old, new, None);
        // First changed line should start with a number prefix
        let first = lines
            .iter()
            .find(|l| l.spans.iter().any(|s| s.content.contains("three")))
            .unwrap();
        assert!(
            first.spans[0]
                .content
                .starts_with(|c: char| c.is_ascii_digit()),
            "expected left-aligned line number"
        );

        let write_lines = write_lines("first\nsecond\nthird", None);
        assert!(write_lines[0].spans[0].content.starts_with("1 | + "));
    }
}
