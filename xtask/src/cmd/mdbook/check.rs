use crate::util::*;
use std::collections::HashMap;
use std::process::Command;

#[derive(Debug)]
struct PoEntry {
    reference: String,
    msgstr_line: usize,
    msgid: String,
    msgstr: String,
}

pub fn run() -> anyhow::Result<()> {
    let root = repo_root();
    let po_dir = po_dir(&root);
    require_tool("msgfmt", "apt install gettext / brew install gettext")?;

    let mut entries: Vec<_> = std::fs::read_dir(&po_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "po"))
        .collect();
    entries.sort_by_key(|e| e.path());

    let mut failed = false;
    for entry in entries {
        let path = entry.path();
        let locale = path.file_stem().unwrap_or_default().to_string_lossy();
        let status = Command::new("msgfmt")
            .args(["--check-format", "-o", "/dev/null"])
            .arg(&path)
            .status()?;
        if !status.success() {
            eprintln!("FAIL: {locale}");
            failed = true;
        }
        let raw = std::fs::read_to_string(&path)?;
        let po_entries = parse_po_entries(&raw);
        for (entry, reason) in audit_generated_responses(&po_entries) {
            eprintln!(
                "FAIL: {locale}:{}: generated-response translation ({reason}) at {}",
                entry.msgstr_line, entry.reference
            );
            failed = true;
        }
        for (entry, reason) in audit_protected_literals(&po_entries) {
            eprintln!(
                "FAIL: {locale}:{}: protected-literal translation ({reason}) at {}",
                entry.msgstr_line, entry.reference
            );
            failed = true;
        }
    }

    if failed {
        anyhow::bail!(
            "one or more .po files have format, generated-response, or protected-literal errors"
        );
    }
    println!("All .po files OK.");
    Ok(())
}

fn audit_generated_responses(entries: &[PoEntry]) -> Vec<(&PoEntry, &'static str)> {
    entries
        .iter()
        .filter_map(|entry| generated_response_reason(entry).map(|reason| (entry, reason)))
        .collect()
}

fn audit_protected_literals(entries: &[PoEntry]) -> Vec<(&PoEntry, &'static str)> {
    entries
        .iter()
        .filter_map(|entry| protected_literal_reason(entry).map(|reason| (entry, reason)))
        .collect()
}

fn parse_po_entries(raw: &str) -> Vec<PoEntry> {
    let mut entries = Vec::new();
    let mut references = Vec::new();
    let mut msgid_lines = Vec::new();
    let mut msgstr_lines = Vec::new();
    let mut msgid_line = 0usize;
    let mut msgstr_line = 0usize;
    let mut in_msgid = false;
    let mut in_msgstr = false;

    for (idx, line) in raw.lines().enumerate() {
        let line_number = idx + 1;
        if let Some(reference) = line.strip_prefix("#: ") {
            references.push(reference.to_string());
            continue;
        }

        if let Some(rest) = line.strip_prefix("msgid ") {
            commit_po_entry(
                &mut entries,
                &references,
                msgid_line,
                msgstr_line,
                &msgid_lines,
                &msgstr_lines,
            );
            msgid_lines.clear();
            msgstr_lines.clear();
            msgstr_line = 0;
            msgid_line = line_number;
            in_msgid = true;
            in_msgstr = false;
            msgid_lines.push(rest.to_string());
            continue;
        }

        if let Some(rest) = msgstr_value(line) {
            in_msgid = false;
            in_msgstr = true;
            if msgstr_line == 0 {
                msgstr_line = line_number;
            }
            msgstr_lines.push(rest.to_string());
            continue;
        }

        if line.trim_start().starts_with('"') {
            if in_msgid {
                msgid_lines.push(line.trim().to_string());
            } else if in_msgstr {
                msgstr_lines.push(line.trim().to_string());
            }
            continue;
        }

        if line.trim().is_empty() {
            commit_po_entry(
                &mut entries,
                &references,
                msgid_line,
                msgstr_line,
                &msgid_lines,
                &msgstr_lines,
            );
            references.clear();
            msgid_lines.clear();
            msgstr_lines.clear();
            msgid_line = 0;
            msgstr_line = 0;
            in_msgid = false;
            in_msgstr = false;
        }
    }

    commit_po_entry(
        &mut entries,
        &references,
        msgid_line,
        msgstr_line,
        &msgid_lines,
        &msgstr_lines,
    );
    entries
}

fn msgstr_value(line: &str) -> Option<&str> {
    if let Some(rest) = line.strip_prefix("msgstr ") {
        return Some(rest);
    }
    let rest = line.strip_prefix("msgstr[")?;
    let (_, value) = rest.split_once("] ")?;
    Some(value)
}

fn commit_po_entry(
    entries: &mut Vec<PoEntry>,
    references: &[String],
    msgid_line: usize,
    msgstr_line: usize,
    msgid_lines: &[String],
    msgstr_lines: &[String],
) {
    if msgid_lines.is_empty() {
        return;
    }
    let msgid = decode_po_string(msgid_lines);
    if msgid.is_empty() {
        return;
    }
    entries.push(PoEntry {
        reference: references.join(" "),
        msgstr_line: msgstr_line.max(msgid_line),
        msgid,
        msgstr: decode_po_string(msgstr_lines),
    });
}

fn decode_po_string(lines: &[String]) -> String {
    let mut out = String::new();
    for line in lines {
        let inner = line.trim();
        if !(inner.starts_with('"') && inner.ends_with('"') && inner.len() >= 2) {
            continue;
        }
        let mut chars = inner[1..inner.len() - 1].chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        }
    }
    out
}

fn generated_response_reason(entry: &PoEntry) -> Option<&'static str> {
    if entry.msgstr.trim().is_empty() {
        return None;
    }

    let source_len = entry.msgid.chars().count().max(1);
    let translation_len = entry.msgstr.chars().count();
    let ratio = translation_len as f64 / source_len as f64;

    if translation_len > (source_len * 3).max(80)
        && contains_assistant_response_phrase(&entry.msgstr)
    {
        return Some("assistant-response phrase");
    }
    if translation_len > (source_len * 4).max(300)
        && has_markdown_heading_outside_code(&entry.msgstr)
        && !has_markdown_heading_outside_code(&entry.msgid)
    {
        return Some("generated document headings");
    }
    if translation_len > (source_len * 4).max(250) && contains_generated_doc_metadata(&entry.msgstr)
    {
        return Some("generated metadata block");
    }
    if translation_len > (source_len * 3).max(250) && has_repeated_generated_sentence(&entry.msgstr)
    {
        return Some("repeated generated prose");
    }
    if source_len <= 24 && translation_len > 220 && ratio > 8.0 {
        return Some("short source expanded excessively");
    }

    None
}

fn protected_literal_reason(entry: &PoEntry) -> Option<&'static str> {
    if entry.msgstr.trim().is_empty() {
        return None;
    }

    for phrase in PROTECTED_PHRASES {
        if entry.msgid.contains(phrase) && !entry.msgstr.contains(phrase) {
            return Some("protected product/protocol name changed");
        }
    }

    for literal in protected_code_literals(&entry.msgid) {
        if !entry.msgstr.contains(&literal) {
            return Some("machine-facing code literal changed");
        }
    }

    None
}

const PROTECTED_PHRASES: &[&str] = &["ZeroClaw Maturity Framework"];

fn protected_code_literals(text: &str) -> Vec<String> {
    let mut literals = Vec::new();
    collect_inline_code_literals(text, &mut literals);
    collect_fenced_code_literals(text, &mut literals);
    literals.sort();
    literals.dedup();
    literals
}

fn collect_inline_code_literals(text: &str, literals: &mut Vec<String>) {
    let mut rest = text;
    while let Some(start) = rest.find('`') {
        rest = &rest[start + 1..];
        if rest.starts_with("``") {
            continue;
        }
        let Some(end) = rest.find('`') else {
            break;
        };
        let literal = &rest[..end];
        rest = &rest[end + 1..];
        if is_protected_command_literal(literal) {
            literals.push(literal.to_string());
        }
    }
}

fn collect_fenced_code_literals(text: &str, literals: &mut Vec<String>) {
    let mut fence_language: Option<String> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            if fence_language.is_some() {
                fence_language = None;
            } else {
                fence_language = Some(
                    trimmed
                        .trim_start_matches('`')
                        .split_whitespace()
                        .next()
                        .unwrap_or_default()
                        .to_ascii_lowercase(),
                );
            }
            continue;
        }
        let Some(language) = fence_language.as_deref() else {
            continue;
        };
        if trimmed.is_empty() {
            continue;
        }

        if is_protected_command_literal(trimmed) {
            literals.push(trimmed.to_string());
        }
        if language != "toml" {
            continue;
        }

        if is_toml_section(trimmed) {
            literals.push(trimmed.to_string());
        } else if let Some((key, _)) = trimmed.split_once('=') {
            let key = key.trim();
            if is_config_key(key) {
                literals.push(key.to_string());
            }
        }
    }
}

fn is_protected_command_literal(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed == "zeroclaw daemon" || trimmed.starts_with("zeroclaw daemon ")
}

fn is_config_key(text: &str) -> bool {
    is_toml_key_path(text)
}

fn is_toml_section(text: &str) -> bool {
    let text = text.trim();
    let section = if text.starts_with("[[") && text.ends_with("]]") {
        &text[2..text.len() - 2]
    } else if text.starts_with('[') && text.ends_with(']') {
        &text[1..text.len() - 1]
    } else {
        return false;
    };
    is_toml_key_path(section.trim())
}

fn is_toml_key_path(text: &str) -> bool {
    let text = text.trim();
    if text.is_empty() {
        return false;
    }

    let mut start = 0;
    let mut quote = None;
    for (idx, c) in text.char_indices() {
        if let Some(active_quote) = quote {
            if c == active_quote {
                quote = None;
            }
            continue;
        }

        match c {
            '"' | '\'' => quote = Some(c),
            '.' => {
                if !is_toml_key_segment(&text[start..idx]) {
                    return false;
                }
                start = idx + c.len_utf8();
            }
            _ => {}
        }
    }

    quote.is_none() && is_toml_key_segment(&text[start..])
}

fn is_toml_key_segment(text: &str) -> bool {
    let text = text.trim();
    is_bare_toml_key(text) || is_quoted_toml_key(text)
}

fn is_bare_toml_key(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn is_quoted_toml_key(text: &str) -> bool {
    text.len() >= 2
        && ((text.starts_with('"') && text.ends_with('"'))
            || (text.starts_with('\'') && text.ends_with('\'')))
}

fn contains_assistant_response_phrase(text: &str) -> bool {
    const NEEDLES: &[&str] = &[
        "please provide",
        "provide the text",
        "provide more context",
        "more context",
        "i can translate",
        "i will translate",
        "here is",
        "pourriez-vous me communiquer",
        "chaîne semble incomplète",
        "文脈",
        "提供できます",
        "翻訳を提供",
        "特定の文脈",
        "以下の手順",
        "正式に通知",
        "última actualización",
        "发布日期",
        "最后更新",
        "最後更新",
        "作者",
        "发布日",
        "バージョン",
        "ライセンス",
    ];
    let lower = text.to_lowercase();
    NEEDLES.iter().any(|needle| lower.contains(needle))
}

fn contains_generated_doc_metadata(text: &str) -> bool {
    [
        "**Última actualización:**",
        "**Autor:**",
        "**Estado:**",
        "**版本**",
        "**发布日期**",
        "**最后更新**",
        "**最後更新**",
        "**Version:**",
        "**Status:**",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

fn has_markdown_heading_outside_code(text: &str) -> bool {
    let mut in_code = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            continue;
        }
        let hashes = trimmed.chars().take_while(|&c| c == '#').count();
        if (1..=6).contains(&hashes) && trimmed.chars().nth(hashes) == Some(' ') {
            return true;
        }
    }
    false
}

fn has_repeated_generated_sentence(text: &str) -> bool {
    let mut seen: HashMap<&str, usize> = HashMap::new();
    for sentence in text
        .split(['.', '!', '?', '。', '！', '？'])
        .map(str::trim)
        .filter(|sentence| sentence.chars().count() >= 24)
    {
        let count = seen
            .entry(sentence)
            .and_modify(|count| *count += 1)
            .or_insert(1);
        if *count >= 3 {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(msgid: &str, msgstr: &str) -> PoEntry {
        PoEntry {
            reference: "src/example.md".to_string(),
            msgstr_line: 2,
            msgid: msgid.to_string(),
            msgstr: msgstr.to_string(),
        }
    }

    #[test]
    fn flags_generated_document_response() {
        let msgstr = format!(
            "{}{}",
            "**版本**：1.0\n**发布日期**：2023-10-01\n\n# 插件注册表治理文档\n\n## 1. 简介\n\n",
            "这是一个完整的新文档，而不是源字符串的翻译。它继续描述治理委员会、审核流程、撤销流程、透明度要求和版本历史。".repeat(12)
        );
        let issue = entry(
            "Write the Plugin Registry governance document (who controls the registry, how plugins are reviewed, how compromised plugins are revoked)",
            &msgstr,
        );
        assert_eq!(
            generated_response_reason(&issue),
            Some("assistant-response phrase")
        );
    }

    #[test]
    fn flags_assistant_request_for_more_context() {
        let issue = entry(
            "When",
            "もし特定の文脈や技術文書での使用例があれば、より正確な翻訳を提供できます。もし特定の文脈や技術文書での使用例があれば、より正確な翻訳を提供できます。もし特定の文脈や技術文書での使用例があれば、より正確な翻訳を提供できます。",
        );
        assert_eq!(
            generated_response_reason(&issue),
            Some("assistant-response phrase")
        );
    }

    #[test]
    fn flags_repeated_generated_prose() {
        let msgstr = format!(
            "{}{}",
            "このフローは、ストリーミング、ツール呼び出し、セキュリティゲートを注釈付きで示しています。".repeat(12),
            "「ユーザーがメッセージを送信」してから「エージェントが返信する」までの間の処理フローを、ストリーミング、ツール呼び出し、セキュリティゲートを注釈付きで示します。"
        );
        let issue = entry(
            "What happens between \"user sends a message\" and \"agent replies\" — the full path, with streaming, tool calls, and security gates annotated.",
            &msgstr,
        );
        assert_eq!(
            generated_response_reason(&issue),
            Some("repeated generated prose")
        );
    }

    #[test]
    fn does_not_flag_normal_translation_with_code_fence_comment() {
        let clean = entry(
            "```toml\n# Correct\nweb_dist_dir = \"/absolute/path\"\n```",
            "```toml\n# Correct\nweb_dist_dir = \"/absolute/path\"\n```",
        );
        assert_eq!(generated_response_reason(&clean), None);
    }

    #[test]
    fn flags_translated_command_literal_for_6407() {
        let issue = entry(
            "[`zeroclaw daemon`↴](#zeroclaw-daemon)",
            "[`zeroclaw 守护进程`↴](#zeroclaw-daemon)",
        );
        assert_eq!(generated_response_reason(&issue), None);
        assert_eq!(
            protected_literal_reason(&issue),
            Some("machine-facing code literal changed")
        );
    }

    #[test]
    fn flags_translated_toml_keys_for_6407() {
        let issue = entry(
            "```toml\n[observability]\nruntime_trace_mode = \"rolling\"\nruntime_trace_path = \"state/runtime-trace.jsonl\"\n```",
            "```toml\n[可观测性]\n运行时跟踪模式 = \"rolling\"\n运行时跟踪路径 = \"state/runtime-trace.jsonl\"\n```",
        );
        assert_eq!(
            protected_literal_reason(&issue),
            Some("machine-facing code literal changed")
        );
    }

    #[test]
    fn flags_single_word_toml_keys_for_6407() {
        let issue = entry(
            "```toml\n[agent]\nenabled = true\nmodel = \"openai.default\"\n```",
            "```toml\n[agent]\n启用 = true\n模型 = \"openai.default\"\n```",
        );
        assert_eq!(
            protected_literal_reason(&issue),
            Some("machine-facing code literal changed")
        );
    }

    #[test]
    fn flags_quoted_dotted_toml_sections_for_6407() {
        let issue = entry(
            "```toml\n[cost.rates.providers.models.anthropic.\"claude.opus\"]\ninput = 15.0\n```",
            "```toml\n[cost.rates.providers.models.anthropic.\"claude-opus\"]\ninput = 15.0\n```",
        );
        assert_eq!(
            protected_literal_reason(&issue),
            Some("machine-facing code literal changed")
        );
    }

    #[test]
    fn ignores_non_toml_fenced_config_like_text() {
        let clean = entry(
            "```text\n[placeholder]\nexample_value = 1\n```",
            "```text\n[marcador]\nvalor_de_ejemplo = 1\n```",
        );
        assert_eq!(protected_literal_reason(&clean), None);
    }

    #[test]
    fn flags_translated_product_name_for_6407() {
        let issue = entry("The ZeroClaw Maturity Framework", "零爪成熟度框架");
        assert_eq!(
            protected_literal_reason(&issue),
            Some("protected product/protocol name changed")
        );
    }

    #[test]
    fn allows_translated_prose_around_preserved_literals() {
        let clean = entry(
            "Run `zeroclaw daemon` after setting `[observability]`.",
            "设置 `[observability]` 后运行 `zeroclaw daemon`。",
        );
        assert_eq!(protected_literal_reason(&clean), None);
    }

    #[test]
    fn allows_translated_cli_placeholders() {
        let clean = entry(
            "**Usage:** `zeroclaw [OPTIONS] <COMMAND>`",
            "**Uso:** `zeroclaw [OPCIONES] <COMANDO>`",
        );
        assert_eq!(protected_literal_reason(&clean), None);
    }

    #[test]
    fn parses_plural_msgstr_variants_for_audit() {
        // This fixture intentionally contains generated-response contamination so
        // grep-based follow-up audits do not mistake it for leaked catalog content.
        let raw = r#"#: src/example.md
msgid "item"
msgid_plural "items"
msgstr[0] ""
msgstr[1] "Please provide more context so I can translate this correctly. Please provide more context so I can translate this correctly. Please provide more context so I can translate this correctly."
"#;

        let entries = parse_po_entries(raw);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].msgstr_line, 4);
        assert_eq!(
            generated_response_reason(&entries[0]),
            Some("assistant-response phrase")
        );
    }
}
