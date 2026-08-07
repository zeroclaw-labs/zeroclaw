//! Normalize placeholder-shaped tags before mdBook interprets them as HTML.

use pulldown_cmark::{Event, Parser};
use serde_json::Value;
use std::collections::HashSet;
use std::io::Read;

/// `supports <renderer>`: every renderer is supported (we only touch content).
pub fn supports() -> ! {
    std::process::exit(0);
}

pub fn run() -> anyhow::Result<()> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let pair: Value = serde_json::from_str(&input)?;
    let mut book = pair
        .get(1)
        .cloned()
        .ok_or_else(|| anyhow::Error::msg("preprocessor input missing book element"))?;

    if let Some(items) = book.get_mut("items").and_then(Value::as_array_mut) {
        for item in items {
            normalize_section(item);
        }
    }

    println!("{}", serde_json::to_string(&book)?);
    Ok(())
}

fn normalize_section(section: &mut Value) {
    let Some(chapter) = section.get_mut("Chapter") else {
        return;
    };

    if let Some(content) = chapter.get("content").and_then(Value::as_str) {
        chapter["content"] = Value::String(normalize(content));
    }
    if let Some(sub_items) = chapter.get_mut("sub_items").and_then(Value::as_array_mut) {
        for item in sub_items {
            normalize_section(item);
        }
    }
}

/// Wrap placeholder-shaped tags in code spans while preserving Markdown code,
/// autolinks, and real HTML.
#[must_use]
pub fn normalize(markdown: &str) -> String {
    let mut output = String::with_capacity(markdown.len());
    let mut copied_through = 0;
    let paired_html = paired_html_openings(markdown);

    for (event, range) in Parser::new(markdown).into_offset_iter() {
        if !matches!(event, Event::Html(_) | Event::InlineHtml(_)) {
            continue;
        }

        let source = &markdown[range.clone()];
        let Some((trimmed, name)) = bare_opening_tag(source) else {
            continue;
        };
        if paired_html.contains(&range.start) || is_bare_html_tag(name) {
            continue;
        }

        let leading_len = source.len() - source.trim_start().len();
        let trailing_start = leading_len + trimmed.len();
        output.push_str(&markdown[copied_through..range.start]);
        output.push_str(&source[..leading_len]);
        output.push('`');
        output.push_str(trimmed);
        output.push('`');
        output.push_str(&source[trailing_start..]);
        copied_through = range.end;
    }

    output.push_str(&markdown[copied_through..]);
    output
}

fn paired_html_openings(markdown: &str) -> HashSet<usize> {
    let mut stack: Vec<(String, usize)> = Vec::new();
    let mut paired = HashSet::new();

    for (event, range) in Parser::new(markdown).into_offset_iter() {
        if !matches!(event, Event::Html(_) | Event::InlineHtml(_)) {
            continue;
        }

        let source = &markdown[range.clone()];
        if let Some((_, name)) = bare_opening_tag(source) {
            if !is_bare_html_tag(name) {
                stack.push((name.to_ascii_lowercase(), range.start));
            }
        } else if let Some(name) = bare_closing_tag(source)
            && stack
                .last()
                .is_some_and(|(opening, _)| opening.eq_ignore_ascii_case(name))
        {
            let (_, opening_start) = stack.pop().expect("checked non-empty stack");
            paired.insert(opening_start);
        }
    }

    paired
}

fn bare_opening_tag(source: &str) -> Option<(&str, &str)> {
    let trimmed = source.trim();
    let name = trimmed.strip_prefix('<')?.strip_suffix('>')?;
    is_placeholder_name(name).then_some((trimmed, name))
}

fn bare_closing_tag(source: &str) -> Option<&str> {
    let trimmed = source.trim();
    let name = trimmed.strip_prefix("</")?.strip_suffix('>')?;
    is_placeholder_name(name).then_some(name)
}

fn is_placeholder_name(name: &str) -> bool {
    let Some(first) = name.chars().next() else {
        return false;
    };
    first.is_ascii_alphabetic()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

fn is_bare_html_tag(name: &str) -> bool {
    matches!(name.to_ascii_lowercase().as_str(), "br" | "hr" | "wbr")
}

#[cfg(test)]
mod tests {
    use super::{normalize, normalize_section};
    use serde_json::json;

    #[test]
    fn wraps_plain_placeholders_but_preserves_html_and_autolinks() {
        let input = "Use <ALIAS>, <config-dir>, and <section>. <div>Body</div> <br> <https://example.com> <a@b.test>";
        let expected = "Use `<ALIAS>`, `<config-dir>`, and `<section>`. <div>Body</div> <br> <https://example.com> <a@b.test>";

        assert_eq!(normalize(input), expected);
    }

    #[test]
    fn preserves_inline_code_and_fenced_blocks() {
        let input = "Already `<ALIAS>` and `` `<CONFIG>` ``.\n\n```toml\nvalue = \"<RAW>\"\n```\nAfter <TARGET>.\n";
        let expected = "Already `<ALIAS>` and `` `<CONFIG>` ``.\n\n```toml\nvalue = \"<RAW>\"\n```\nAfter `<TARGET>`.\n";

        assert_eq!(normalize(input), expected);
        assert_eq!(normalize(expected), expected);
    }

    #[test]
    fn normalizes_table_cells_and_translated_paragraphs() {
        let input = "| 参数 | 值 |\n| --- | --- |\n| 目标 | <target> |\n\n将 <recipient-id> 替换为收件人标识。";
        let expected = "| 参数 | 值 |\n| --- | --- |\n| 目标 | `<target>` |\n\n将 `<recipient-id>` 替换为收件人标识。";

        assert_eq!(normalize(input), expected);
    }

    #[test]
    fn respects_tilde_fences_and_multiline_code_spans() {
        let input = "~~~text\n<RAW>\n~~~~\n`before\n<INLINE>\nafter` and <VISIBLE>";
        let expected = "~~~text\n<RAW>\n~~~~\n`before\n<INLINE>\nafter` and `<VISIBLE>`";

        assert_eq!(normalize(input), expected);
    }

    #[test]
    fn unmatched_backticks_do_not_hide_later_placeholders() {
        let input = "Default value: ``\n\nUse [channels.email.<alias>] next.";
        let expected = "Default value: ``\n\nUse [channels.email.`<alias>`] next.";

        assert_eq!(normalize(input), expected);
    }

    #[test]
    fn normalizes_void_html_names_used_as_placeholders() {
        let input = "Translate <INPUT>, <SOURCE>, and <LINK>, but preserve <br>.";
        let expected = "Translate `<INPUT>`, `<SOURCE>`, and `<LINK>`, but preserve <br>.";

        assert_eq!(normalize(input), expected);
    }

    #[test]
    fn pairs_real_html_events_without_matching_code_or_unrelated_tags() {
        let input = "Use <section> here. `</section>`\n\n<my-widget>Body</my-widget>";
        let expected = "Use `<section>` here. `</section>`\n\n<my-widget>Body</my-widget>";

        assert_eq!(normalize(input), expected);
    }

    #[test]
    fn normalizes_top_level_and_nested_chapters_idempotently() {
        let mut section = json!({
            "Chapter": {
                "content": "Use <TOP>.",
                "sub_items": [{
                    "Chapter": {
                        "content": "将 <NESTED> 替换为值。",
                        "sub_items": []
                    }
                }]
            }
        });

        normalize_section(&mut section);
        normalize_section(&mut section);

        assert_eq!(section["Chapter"]["content"], "Use `<TOP>`.");
        assert_eq!(
            section["Chapter"]["sub_items"][0]["Chapter"]["content"],
            "将 `<NESTED>` 替换为值。"
        );
    }
}
