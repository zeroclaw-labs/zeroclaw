use inkjet::constants::HIGHLIGHT_NAMES;
use inkjet::tree_sitter_highlight::HighlightEvent;
use inkjet::{Highlighter, Language};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use similar::{ChangeTag, TextDiff};

use crate::theme;

// Diff background palette. Foregrounds come from the active theme via
// `theme::syntax_colors`; only the add/delete row tints are diff-specific.
const ADD_BG: Color = Color::Rgb(0, 40, 0);
const DEL_BG: Color = Color::Rgb(55, 0, 0);
const SEP_FG: Color = Color::Rgb(70, 70, 70);

const DIFF_CONTEXT: usize = 3;
const MAX_WRITE_LINES: usize = 60;

/// Decimal digit count of the largest line number in a diff, used to size
/// the gutter so the `|` separator aligns on every row.
fn gutter_width(max_lineno: usize) -> usize {
    max_lineno.max(1).to_string().len()
}

/// Format the gutter for a line that has a number: right-aligned to `width`
/// followed by ` | `.
fn gutter(lineno: usize, width: usize) -> String {
    format!("{lineno:>width$} | ")
}

/// Format the gutter for a line with no number (a side absent on one half of
/// the diff): blank columns to `width` followed by ` | `.
fn gutter_blank(width: usize) -> String {
    format!("{:>width$} | ", "")
}

// ── Syntax highlighting ──────────────────────────────────────────

/// Diff-row foreground for plain (unhighlighted) text, derived from the active
/// theme so it tracks the palette like every other colour.
fn add_fg() -> Color {
    theme::SyntaxScope::DiffPlus.color()
}

fn del_fg() -> Color {
    theme::SyntaxScope::DiffMinus.color()
}

fn ctx_fg() -> Color {
    theme::SyntaxScope::Comment.color()
}

/// Build the color lookup table indexed by `Highlight.0`. Colours come from the
/// active theme, rebuilt per diff so a live theme swap is reflected.
fn hl_colors() -> Vec<Color> {
    theme::syntax_colors(HIGHLIGHT_NAMES)
}

/// Map a file extension to an inkjet `Language`. Returns `None` for
/// unrecognised extensions, which triggers a plain-text fallback.
fn ext_to_language(ext: &str) -> Option<Language> {
    Some(match ext.to_ascii_lowercase().as_str() {
        "rs" => Language::Rust,
        "py" | "pyi" => Language::Python,
        "js" | "mjs" | "cjs" => Language::Javascript,
        "ts" | "mts" | "cts" => Language::Typescript,
        "tsx" => Language::Tsx,
        "jsx" => Language::Jsx,
        "json" | "jsonc" => Language::Json,
        "toml" => Language::Toml,
        "yaml" | "yml" => Language::Yaml,
        "go" => Language::Go,
        "c" | "h" => Language::C,
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" | "hh" => Language::Cpp,
        "html" | "htm" => Language::Html,
        "css" => Language::Css,
        "scss" => Language::Scss,
        "sh" | "bash" | "zsh" => Language::Bash,
        "sql" => Language::Sql,
        "rb" => Language::Ruby,
        "java" => Language::Java,
        "kt" | "kts" => Language::Kotlin,
        "swift" => Language::Swift,
        "lua" => Language::Lua,
        "dockerfile" | "docker" => Language::Dockerfile,
        "diff" | "patch" => Language::Diff,
        "ex" | "exs" => Language::Elixir,
        "hcl" | "tf" => Language::Hcl,
        "zig" => Language::Zig,
        "scala" | "sc" => Language::Scala,
        "php" => Language::Php,
        "dart" => Language::Dart,
        _ => return None,
    })
}

/// Thread-local highlighter — `Highlighter` is `Send + Sync` but
/// `highlight_raw` borrows `&mut self`, so a thread-local avoids
/// contention in async runtimes.
fn with_highlighter<R>(f: impl FnOnce(&mut Highlighter) -> R) -> R {
    thread_local! {
        static HL: std::cell::RefCell<Highlighter> = std::cell::RefCell::new(Highlighter::new());
    }
    HL.with(|cell| f(&mut cell.borrow_mut()))
}

/// Pre-highlight all lines of `text` for the given inkjet `Language`.
/// Returns one `Vec<Span<'static>>` per line, with `bg` forced as the
/// background on every span. Falls back to a single plain span per line
/// when highlighting fails.
fn highlight_all(
    text: &str,
    lang: Language,
    bg: Color,
    plain_fg: Color,
) -> Vec<Vec<Span<'static>>> {
    let colors = hl_colors();

    let events: Vec<HighlightEvent> = match with_highlighter(|hl| {
        hl.highlight_raw(lang, &text)
            .map(|iter| iter.collect::<Result<Vec<_>, _>>())
    }) {
        Ok(Ok(v)) => v,
        _ => return plain_line_spans(text, bg, plain_fg),
    };

    // Walk the event stream, tracking the current highlight scope.
    let mut result: Vec<Vec<Span<'static>>> = Vec::new();
    let mut current_line: Vec<Span<'static>> = Vec::new();
    let mut style_stack: Vec<Color> = Vec::new();

    for event in events {
        match event {
            HighlightEvent::HighlightStart(h) => {
                let fg = colors.get(h.0).copied().unwrap_or(plain_fg);
                style_stack.push(fg);
            }
            HighlightEvent::HighlightEnd => {
                style_stack.pop();
            }
            HighlightEvent::Source { start, end } => {
                let fg = style_stack.last().copied().unwrap_or(plain_fg);
                let style = Style::default().fg(fg).bg(bg);
                let slice = &text[start..end];

                // Split on newlines so each output line is independent.
                for (i, segment) in slice.split('\n').enumerate() {
                    if i > 0 {
                        // Newline boundary — flush current_line.
                        if current_line.is_empty() {
                            current_line.push(Span::styled(String::new(), Style::default().bg(bg)));
                        }
                        result.push(std::mem::take(&mut current_line));
                    }
                    if !segment.is_empty() {
                        current_line.push(Span::styled(segment.to_string(), style));
                    }
                }
            }
        }
    }
    // Flush any remaining content.
    if !current_line.is_empty() {
        result.push(current_line);
    }
    if result.is_empty() {
        return plain_line_spans(text, bg, plain_fg);
    }
    result
}

fn plain_line_spans(text: &str, bg: Color, fg: Color) -> Vec<Vec<Span<'static>>> {
    text.lines()
        .map(|l| vec![Span::styled(l.to_string(), Style::default().bg(bg).fg(fg))])
        .collect()
}

/// Syntax-highlight `code` for a markdown fence language token (e.g. `rust`,
/// `python`, `sh`), returning one span vector per line. Colours come from the
/// active theme via [`hl_colors`]; `plain_fg` styles tokens with no scope and is
/// also the whole-line fallback when the token is unknown or highlighting
/// fails. No background is forced, so the caller's code-block backdrop shows
/// through. Returns `None` only when the token does not resolve to a built-in
/// language, letting the caller render its existing plain code styling.
pub fn highlight_code(
    code: &str,
    fence_token: &str,
    plain_fg: Color,
) -> Option<Vec<Line<'static>>> {
    let language = Language::from_token(fence_token.to_ascii_lowercase())?;
    let lines = highlight_all_no_bg(code, language, plain_fg)
        .into_iter()
        .map(Line::from)
        .collect();
    Some(lines)
}

/// Like [`highlight_all`] but without forcing a background colour on spans, for
/// content rendered over the caller's own backdrop (markdown code fences).
fn highlight_all_no_bg(text: &str, lang: Language, plain_fg: Color) -> Vec<Vec<Span<'static>>> {
    let colors = hl_colors();

    let events: Vec<HighlightEvent> = match with_highlighter(|hl| {
        hl.highlight_raw(lang, &text)
            .map(|iter| iter.collect::<Result<Vec<_>, _>>())
    }) {
        Ok(Ok(v)) => v,
        _ => return plain_line_spans_no_bg(text, plain_fg),
    };

    let mut result: Vec<Vec<Span<'static>>> = Vec::new();
    let mut current_line: Vec<Span<'static>> = Vec::new();
    let mut style_stack: Vec<Color> = Vec::new();

    for event in events {
        match event {
            HighlightEvent::HighlightStart(h) => {
                let fg = colors.get(h.0).copied().unwrap_or(plain_fg);
                style_stack.push(fg);
            }
            HighlightEvent::HighlightEnd => {
                style_stack.pop();
            }
            HighlightEvent::Source { start, end } => {
                let fg = style_stack.last().copied().unwrap_or(plain_fg);
                let style = Style::default().fg(fg);
                let slice = &text[start..end];
                for (i, segment) in slice.split('\n').enumerate() {
                    if i > 0 {
                        result.push(std::mem::take(&mut current_line));
                    }
                    if !segment.is_empty() {
                        current_line.push(Span::styled(segment.to_string(), style));
                    }
                }
            }
        }
    }
    if !current_line.is_empty() {
        result.push(current_line);
    }
    if result.is_empty() {
        return plain_line_spans_no_bg(text, plain_fg);
    }
    result
}

fn plain_line_spans_no_bg(text: &str, fg: Color) -> Vec<Vec<Span<'static>>> {
    text.lines()
        .map(|l| vec![Span::styled(l.to_string(), Style::default().fg(fg))])
        .collect()
}

// ── Public diff API ──────────────────────────────────────────────

/// Build ratatui `Line`s for a unified diff of `old` vs `new`.
///
/// `lang` is an optional file extension (e.g. `"rs"`, `"py"`) used for
/// syntax highlighting. Pass `None` to get plain colored diffs.
///
/// `start_line` is the 1-based line number where `old` begins in the
/// underlying file. The gutter is offset so the displayed numbers match
/// the file on disk. Callers without a known file location (or write
/// diffs that always begin at line 1) should pass `1`.
pub fn diff_lines(
    old: &str,
    new: &str,
    lang: Option<&str>,
    start_line: usize,
) -> Vec<Line<'static>> {
    let start_line = start_line.max(1);
    let diff = TextDiff::from_lines(old, new);
    let mut out: Vec<Line<'static>> = Vec::new();

    let max_lineno = start_line.saturating_add(
        old.lines()
            .count()
            .max(new.lines().count())
            .saturating_sub(1),
    );
    let width = gutter_width(max_lineno);

    // Pre-highlight both sides in full so multi-line token state is correct.
    let (del_fg, add_fg) = (del_fg(), add_fg());
    let (del_hl, add_hl) = match lang.and_then(ext_to_language) {
        Some(language) => (
            Some(highlight_all(old, language, DEL_BG, del_fg)),
            Some(highlight_all(new, language, ADD_BG, add_fg)),
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
                                vec![Span::styled(text, Style::default().bg(DEL_BG).fg(del_fg))]
                            });
                        let lineno = change
                            .old_index()
                            .map(|n| gutter(n + start_line, width))
                            .unwrap_or_else(|| gutter_blank(width));
                        let mut spans = vec![Span::styled(
                            lineno + "- ",
                            Style::default()
                                .bg(DEL_BG)
                                .fg(del_fg)
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
                                vec![Span::styled(text, Style::default().bg(ADD_BG).fg(add_fg))]
                            });
                        let lineno = change
                            .new_index()
                            .map(|n| gutter(n + start_line, width))
                            .unwrap_or_else(|| gutter_blank(width));
                        let mut spans = vec![Span::styled(
                            lineno + "+ ",
                            Style::default()
                                .bg(ADD_BG)
                                .fg(add_fg)
                                .add_modifier(Modifier::BOLD),
                        )];
                        spans.extend(content);
                        Line::from(spans).style(Style::default().bg(ADD_BG))
                    }
                    ChangeTag::Equal => {
                        let lineno = change
                            .old_index()
                            .map(|n| gutter(n + start_line, width))
                            .unwrap_or_else(|| gutter_blank(width));
                        Line::from(Span::styled(
                            format!("{lineno}  {text}"),
                            Style::default().fg(ctx_fg()),
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
    let width = gutter_width(show);

    let add_fg = add_fg();
    let hl = lang
        .and_then(ext_to_language)
        .map(|language| highlight_all(content, language, ADD_BG, add_fg));
    let mut out: Vec<Line<'static>> = Vec::with_capacity(show + 1);

    for (i, item) in all.iter().enumerate().take(show) {
        let content_spans = hl
            .as_ref()
            .and_then(|v| v.get(i))
            .cloned()
            .unwrap_or_else(|| {
                vec![Span::styled(
                    item.to_string(),
                    Style::default().bg(ADD_BG).fg(add_fg),
                )]
            });
        let mut spans = vec![Span::styled(
            gutter(i + 1, width) + "+ ",
            Style::default()
                .bg(ADD_BG)
                .fg(add_fg)
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
        let lines = diff_lines("foo\nbar\n", "foo\nbaz\n", None, 1);
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
        let lines = diff_lines("same\n", "same\n", None, 1);
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
    fn diff_gutter_reflects_start_line_offset() {
        let lines = diff_lines("foo\n", "bar\n", None, 437);
        let del_line = lines
            .iter()
            .find(|l| {
                l.spans
                    .first()
                    .map(|s| s.content.as_ref().ends_with("- "))
                    .unwrap_or(false)
            })
            .expect("should have a delete line");
        let gutter_text = del_line.spans.first().unwrap().content.as_ref();
        assert!(
            gutter_text.contains("437"),
            "gutter should show file-absolute line 437, got: {gutter_text:?}"
        );
    }

    #[test]
    fn diff_delete_line_has_red_bg() {
        let lines = diff_lines("old line\n", "new line\n", None, 1);
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
        let lines = diff_lines("old line\n", "new line\n", None, 1);
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
        let lines = diff_lines(old, new, Some("rs"), 1);
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
        let lines = diff_lines(old, new, None, 1);
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

    #[test]
    fn gutter_width_uniform_across_digit_boundary() {
        // A diff whose line numbers cross 9->10 must pad single-digit
        // numbers so every `|` lands in the same column.
        let old: String = (1..=12).map(|n| format!("line {n}\n")).collect();
        let mut new = old.clone();
        new.push_str("line 13 added\n");
        let lines = diff_lines(&old, &new, None, 1);

        let bar_cols: Vec<usize> = lines
            .iter()
            .filter_map(|l| {
                let s: String = l.spans.iter().map(|sp| sp.content.as_ref()).collect();
                s.find('|')
            })
            .collect();
        assert!(bar_cols.len() > 1, "expected several gutter rows");
        assert!(
            bar_cols.windows(2).all(|w| w[0] == w[1]),
            "`|` separator not column-aligned: {bar_cols:?}"
        );
    }

    #[test]
    fn ext_to_language_maps_common_extensions() {
        assert!(ext_to_language("rs").is_some());
        assert!(ext_to_language("py").is_some());
        assert!(ext_to_language("js").is_some());
        assert!(ext_to_language("ts").is_some());
        assert!(ext_to_language("json").is_some());
        assert!(ext_to_language("toml").is_some());
        assert!(ext_to_language("yaml").is_some());
        assert!(ext_to_language("go").is_some());
        assert!(ext_to_language("unknown_ext_xyz").is_none());
    }

    #[test]
    fn write_lines_with_syntax_highlighting() {
        let content = "fn main() {\n    println!(\"hello\");\n}\n";
        let lines = write_lines(content, Some("rs"));
        // With highlighting, the first content line should have multiple spans
        // (line-number prefix + highlighted tokens).
        assert!(
            lines[0].spans.len() > 2,
            "expected highlighted spans, got {}",
            lines[0].spans.len()
        );
    }

    #[test]
    fn highlight_all_falls_back_on_unknown_language() {
        // Plaintext fallback — should produce one span per line.
        let result = plain_line_spans("hello\nworld\n", ADD_BG, add_fg());
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].len(), 1);
    }

    #[test]
    fn hl_colors_table_covers_all_highlight_names() {
        let colors = hl_colors();
        assert_eq!(
            colors.len(),
            HIGHLIGHT_NAMES.len(),
            "color table must have one entry per highlight name"
        );
    }
}
