use std::collections::BTreeSet;
use std::sync::OnceLock;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProtectedLiteral {
    pub text: String,
    pub reason: &'static str,
}

const DOC_LOCAL_TERMS: &[&str] = &[
    "ZeroClaw",
    "ZeroClaw Maturity Framework",
    "zerocode",
    "ACP",
    "MCP",
    "TOML",
    "YAML",
    "JSON",
    "HTTP",
    "HTTPS",
    "WSS",
    "OAuth",
];

const COMMAND_PREFIXES: &[&str] = &[
    "bash",
    "cd",
    "zeroclaw",
    "zerocode",
    "cargo",
    "git",
    "gh",
    "curl",
    "docker",
    "sh",
    "source",
    "systemctl",
    "journalctl",
    "mdbook",
    "msgfmt",
    "msgmerge",
    "msgcat",
    "msgattrib",
    "msginit",
];

const GENERIC_REGISTRY_TERMS: &[&str] = &[
    "Command",
    "Compatible",
    "Cron",
    "Custom",
    "Email",
    "Manifest",
    "Voice Call",
    "Webhook",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FenceLanguage {
    Toml,
    Yaml,
    Json,
    Generic,
}

const FENCE_LANGUAGE_TAGS: &[(&str, FenceLanguage)] = &[
    ("toml", FenceLanguage::Toml),
    ("yaml", FenceLanguage::Yaml),
    ("yml", FenceLanguage::Yaml),
    ("json", FenceLanguage::Json),
    ("jsonc", FenceLanguage::Json),
];

impl FenceLanguage {
    fn from_tag(tag: &str) -> Self {
        FENCE_LANGUAGE_TAGS
            .iter()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(tag))
            .map(|(_, language)| *language)
            .unwrap_or(Self::Generic)
    }
}

pub fn protected_literals(text: &str) -> Vec<ProtectedLiteral> {
    let mut literals = Vec::new();
    collect_protected_terms(text, &mut literals);
    collect_inline_code_literals(text, &mut literals);
    collect_fenced_code_literals(text, &mut literals);
    sort_dedup_literals(&mut literals);
    literals
}

pub fn missing_protected_literal(source: &str, translation: &str) -> Option<ProtectedLiteral> {
    if translation.trim().is_empty() {
        return None;
    }

    protected_literals(source)
        .into_iter()
        .find(|literal| !translation.contains(&literal.text))
}

pub fn preservation_prompt(source: &str) -> Option<String> {
    let literals = protected_literals(source);
    if literals.is_empty() {
        return None;
    }

    let mut out = String::from("Preserve these exact substrings unchanged:");
    for literal in literals {
        out.push_str("\n- ");
        out.push_str(&literal.text);
    }
    Some(out)
}

pub fn contains_generated_toml_block(source: &str, translation: &str) -> bool {
    if has_toml_fence(translation) && !has_toml_fence(source) {
        return true;
    }
    toml_like_block_score(translation) >= 3 && toml_like_block_score(source) < 3
}

fn collect_protected_terms(text: &str, literals: &mut Vec<ProtectedLiteral>) {
    for term in protected_terms() {
        if contains_protected_term(text, term) {
            push_literal(literals, term, "protected product/protocol name changed");
        }
    }
}

fn contains_protected_term(text: &str, term: &str) -> bool {
    text.match_indices(term)
        .any(|(idx, _)| has_term_boundary(text, idx, term.len()))
}

fn has_term_boundary(text: &str, start: usize, len: usize) -> bool {
    let before = text[..start].chars().next_back();
    let after = text[start + len..].chars().next();
    !before.is_some_and(is_identifier_char) && !after.is_some_and(is_identifier_char)
}

fn is_identifier_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn protected_terms() -> &'static [String] {
    static TERMS: OnceLock<Vec<String>> = OnceLock::new();
    TERMS.get_or_init(|| {
        let mut terms: BTreeSet<String> = DOC_LOCAL_TERMS
            .iter()
            .map(|term| (*term).to_string())
            .collect();

        for provider in zeroclaw_providers::list_model_providers() {
            if is_protected_registry_term(provider.display_name) {
                terms.insert(provider.display_name.to_string());
            }
        }

        for channel in zeroclaw_config::schema::Config::default()
            .channels
            .channels()
        {
            if is_protected_registry_term(channel.name) {
                terms.insert(channel.name.to_string());
            }
        }

        terms.into_iter().collect()
    })
}

fn is_protected_registry_term(term: &str) -> bool {
    !GENERIC_REGISTRY_TERMS.contains(&term)
}

fn collect_inline_code_literals(text: &str, literals: &mut Vec<ProtectedLiteral>) {
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
        collect_machine_literal(literal, literals);
    }
}

fn collect_fenced_code_literals(text: &str, literals: &mut Vec<ProtectedLiteral>) {
    let mut fence_language: Option<FenceLanguage> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            if fence_language.is_some() {
                fence_language = None;
            } else {
                let tag = trimmed
                    .trim_start_matches('`')
                    .split_whitespace()
                    .next()
                    .unwrap_or_default();
                fence_language = Some(FenceLanguage::from_tag(tag));
            }
            continue;
        }

        let Some(language) = fence_language else {
            continue;
        };
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        match language {
            FenceLanguage::Toml => collect_toml_literals(trimmed, literals),
            FenceLanguage::Yaml => collect_yaml_literals(trimmed, literals),
            FenceLanguage::Json => collect_json_literals(trimmed, literals),
            FenceLanguage::Generic => collect_generic_code_literal(trimmed, literals),
        }
    }
}

fn collect_machine_literal(text: &str, literals: &mut Vec<ProtectedLiteral>) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }

    collect_command_literals(trimmed, literals);
    for flag in trimmed.split_whitespace().filter(|part| is_cli_flag(part)) {
        push_literal(literals, flag, "machine-facing code literal changed");
    }

    if is_toml_section(trimmed)
        || is_env_var(trimmed)
        || is_path_like(trimmed)
        || is_url_like(trimmed)
        || is_symbol_like(trimmed)
        || is_label_like(trimmed)
        || is_structured_inline_literal(trimmed)
    {
        push_literal(literals, trimmed, "machine-facing code literal changed");
    }
}

fn collect_generic_code_literal(text: &str, literals: &mut Vec<ProtectedLiteral>) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }

    collect_command_literals(trimmed, literals);
    for flag in trimmed.split_whitespace().filter(|part| is_cli_flag(part)) {
        push_literal(literals, flag, "machine-facing code literal changed");
    }

    if is_env_var(trimmed)
        || is_path_like(trimmed)
        || is_url_like(trimmed)
        || is_symbol_like(trimmed)
        || is_label_like(trimmed)
    {
        push_literal(literals, trimmed, "machine-facing code literal changed");
    }
}

fn collect_toml_literals(line: &str, literals: &mut Vec<ProtectedLiteral>) {
    if is_toml_section(line) {
        push_literal(literals, line.trim(), "machine-facing code literal changed");
    } else if let Some((key, _)) = line.split_once('=') {
        let key = key.trim();
        if is_config_key(key) {
            push_literal(literals, key, "machine-facing code literal changed");
        }
    }
}

fn collect_yaml_literals(line: &str, literals: &mut Vec<ProtectedLiteral>) {
    let Some((key, _)) = line.split_once(':') else {
        return;
    };
    let key = key.trim().trim_matches('"').trim_matches('\'');
    if is_config_key(key) {
        push_literal(literals, key, "machine-facing code literal changed");
    }
}

fn collect_json_literals(line: &str, literals: &mut Vec<ProtectedLiteral>) {
    let trimmed = line
        .trim_start()
        .trim_start_matches('{')
        .trim_start_matches(',');
    let Some(rest) = trimmed.strip_prefix('"') else {
        return;
    };
    let Some((key, after)) = rest.split_once('"') else {
        return;
    };
    if after.trim_start().starts_with(':') && is_config_key(key) {
        push_literal(literals, key, "machine-facing code literal changed");
    }
}

fn collect_command_literals(text: &str, literals: &mut Vec<ProtectedLiteral>) {
    for segment in shell_segments(text) {
        if let Some(command) = command_literal(segment) {
            push_literal(literals, &command, "machine-facing code literal changed");
        }
    }
}

fn shell_segments(text: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let bytes = text.as_bytes();
    let mut start = 0;
    let mut idx = 0;

    while idx < bytes.len() {
        let separator_len = match bytes[idx] {
            b'&' if bytes.get(idx + 1) == Some(&b'&') => 2,
            b'|' if bytes.get(idx + 1) == Some(&b'|') => 2,
            b'|' | b';' => 1,
            _ => 0,
        };

        if separator_len == 0 {
            idx += 1;
            continue;
        }

        segments.push(&text[start..idx]);
        idx += separator_len;
        start = idx;
    }

    segments.push(&text[start..]);
    segments
}

fn command_literal(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let command = trimmed.split_whitespace().next()?;
    if !COMMAND_PREFIXES.contains(&command) {
        return None;
    }
    let has_placeholder = trimmed.split_whitespace().any(is_placeholder);
    if !has_placeholder && trimmed.chars().all(is_command_literal_char) {
        return Some(trimmed.to_string());
    }

    let mut keep = vec![command];
    for part in trimmed.split_whitespace().skip(1) {
        if is_placeholder(part) {
            break;
        }
        if part.chars().all(is_command_literal_char) {
            keep.push(part);
        } else {
            break;
        }
    }
    Some(keep.join(" "))
}

fn is_cli_flag(text: &str) -> bool {
    let text = text.trim_matches(|c: char| c == ',' || c == ';' || c == '.');
    if let Some(flag) = text.strip_prefix("--") {
        return !flag.is_empty()
            && flag
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '=');
    }
    if let Some(flag) = text.strip_prefix('-') {
        return !flag.is_empty()
            && !flag.starts_with('-')
            && flag
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '=');
    }
    false
}

fn is_placeholder(text: &str) -> bool {
    let text = text.trim_matches(|c: char| c == ',' || c == ';' || c == '.');
    (text.starts_with('<') && text.ends_with('>')) || (text.starts_with('[') && text.ends_with(']'))
}

fn is_command_literal_char(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(
            c,
            ' ' | '\t'
                | '-'
                | '_'
                | '.'
                | '/'
                | ':'
                | '='
                | ','
                | '['
                | ']'
                | '<'
                | '>'
                | '"'
                | '\''
                | '$'
                | '~'
                | '*'
                | '?'
                | '+'
                | '@'
                | '#'
                | '%'
        )
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

fn is_env_var(text: &str) -> bool {
    let text = text.trim_matches(|c: char| c == ',' || c == ';' || c == '.');
    text.len() >= 3
        && text
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        && text.chars().any(|c| c == '_')
}

fn is_path_like(text: &str) -> bool {
    let text = text.trim_matches(|c: char| c == ',' || c == ';' || c == '.');
    if text.contains(char::is_whitespace) || text.starts_with("//") {
        return false;
    }
    (text.starts_with('/') && text.chars().any(|c| c.is_ascii_alphanumeric()))
        || text.starts_with("./")
        || text.starts_with("../")
        || text.starts_with("~/")
        || text.contains(".md")
        || text.contains(".toml")
        || text.contains(".json")
        || text.contains(".jsonl")
        || text.contains(".db")
}

fn is_url_like(text: &str) -> bool {
    text.starts_with("http://") || text.starts_with("https://") || text.starts_with("wss://")
}

fn is_symbol_like(text: &str) -> bool {
    let text = text.trim();
    if text.len() < 3 || text.contains(' ') {
        return false;
    }
    text.contains("::")
        || text.contains("()")
        || text.contains('_')
        || text.contains("->")
        || text.ends_with(')')
        || text.ends_with("[]")
}

fn is_label_like(text: &str) -> bool {
    let text = text.trim();
    let Some((prefix, value)) = text.split_once(':') else {
        return false;
    };
    matches!(
        prefix,
        "priority" | "provider" | "r" | "risk" | "size" | "status" | "type"
    ) && !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn is_structured_inline_literal(text: &str) -> bool {
    let text = text.trim();
    (text.starts_with('[') && text.ends_with(']') && is_toml_section(text))
        || (text.contains('=')
            && text
                .split_once('=')
                .is_some_and(|(key, _)| is_config_key(key)))
}

fn has_toml_fence(text: &str) -> bool {
    text.lines().any(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("```")
            && trimmed
                .trim_start_matches('`')
                .trim_start()
                .starts_with("toml")
    })
}

fn toml_like_block_score(text: &str) -> usize {
    let mut best = 0;
    let mut current = 0;
    for line in text.lines() {
        let weight = toml_like_line_weight(line);
        if weight > 0 {
            current += weight;
            best = best.max(current);
        } else if !line.trim().is_empty() {
            current = 0;
        }
    }
    best
}

fn toml_like_line_weight(line: &str) -> usize {
    let trimmed = line.trim();
    if trimmed.starts_with('#') || trimmed.is_empty() {
        return 0;
    }
    if trimmed.starts_with('[') && trimmed.ends_with(']') {
        return 2;
    }
    if trimmed
        .split_once('=')
        .is_some_and(|(key, value)| is_toml_like_key(key.trim()) && !value.trim().is_empty())
    {
        return 1;
    }
    0
}

fn is_toml_like_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

fn push_literal(literals: &mut Vec<ProtectedLiteral>, text: &str, reason: &'static str) {
    literals.push(ProtectedLiteral {
        text: text.to_string(),
        reason,
    });
}

fn sort_dedup_literals(literals: &mut Vec<ProtectedLiteral>) {
    literals.sort_by(|a, b| a.text.cmp(&b.text).then(a.reason.cmp(b.reason)));
    literals.dedup_by(|a, b| a.text == b.text);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(source: &str) -> Vec<String> {
        protected_literals(source)
            .into_iter()
            .map(|literal| literal.text)
            .collect()
    }

    #[test]
    fn extracts_registry_provider_terms() {
        assert!(texts("Configure OpenRouter or Anthropic.").contains(&"OpenRouter".to_string()));
    }

    #[test]
    fn extracts_registry_channel_terms() {
        assert!(
            texts("Send the message to Discord and WhatsApp.").contains(&"Discord".to_string())
        );
    }

    #[test]
    fn ignores_generic_registry_terms() {
        assert!(!texts("Command").contains(&"Command".to_string()));
    }

    #[test]
    fn does_not_match_protected_terms_inside_words() {
        assert!(!texts("Coherent local validation").contains(&"Cohere".to_string()));
    }

    #[test]
    fn extracts_cli_command_and_placeholders() {
        let literals = texts("Run `zeroclaw [OPTIONS] <COMMAND>`.");
        assert!(literals.contains(&"zeroclaw".to_string()));
        assert!(!literals.contains(&"[OPTIONS]".to_string()));
        assert!(!literals.contains(&"<COMMAND>".to_string()));
    }

    #[test]
    fn does_not_synthesize_non_contiguous_command_fragments() {
        let literals = texts("Run `gh pr view <pr> --json body`.");
        assert!(literals.contains(&"gh pr view".to_string()));
        assert!(literals.contains(&"--json".to_string()));
        assert!(!literals.contains(&"gh --json".to_string()));
    }

    #[test]
    fn does_not_synthesize_shell_operator_commands() {
        let literals =
            texts("Run `systemctl --user daemon-reload && systemctl --user restart zeroclaw`.");
        assert!(!literals.contains(&"systemctl --user --user".to_string()));
        assert!(literals.contains(&"systemctl --user daemon-reload".to_string()));
        assert!(literals.contains(&"systemctl --user restart zeroclaw".to_string()));
        assert!(literals.contains(&"--user".to_string()));
    }

    #[test]
    fn extracts_compound_shell_command_segments() {
        let literals = texts(
            "Run `git clone https://github.com/zeroclaw-labs/zeroclaw && cd zeroclaw && source ~/.cargo/env`.",
        );
        assert!(
            literals.contains(&"git clone https://github.com/zeroclaw-labs/zeroclaw".to_string())
        );
        assert!(literals.contains(&"cd zeroclaw".to_string()));
        assert!(literals.contains(&"source ~/.cargo/env".to_string()));
    }

    #[test]
    fn extracts_short_flags_in_compound_commands() {
        let literals = texts("Run `curl -sSf https://example.test/install.sh | sh -s -- -y`.");
        assert!(literals.contains(&"-sSf".to_string()));
        assert!(literals.contains(&"-s".to_string()));
        assert!(literals.contains(&"-y".to_string()));
    }

    #[test]
    fn does_not_treat_comments_as_paths() {
        assert!(texts("Use `///` for doc comments.").is_empty());
        assert!(texts("`// args: { \"run_id\": \"<run-id>\" }`").is_empty());
    }

    #[test]
    fn extracts_toml_fence_keys() {
        let source = "```toml\n[observability]\nruntime_trace_mode = \"rolling\"\n```";
        let literals = texts(source);
        assert!(literals.contains(&"[observability]".to_string()));
        assert!(literals.contains(&"runtime_trace_mode".to_string()));
    }

    #[test]
    fn allows_translated_toml_comments() {
        let source = "```toml\n[providers.models.nearai.tee]\nmodel   = \"...\"       # pick a modelId from https://cloud-api.near.ai/v1/model/list\napi_key = \"...\"\n```";
        let translation = "```toml\n[providers.models.nearai.tee]\nmodel   = \"...\"       # elige un modelId de https://cloud-api.near.ai/v1/model/list\napi_key = \"...\"\n```";
        assert_eq!(missing_protected_literal(source, translation), None);
    }

    #[test]
    fn extracts_yaml_and_json_keys() {
        let source =
            "```yaml\napi_key: value\n```\n```json\n{\"runtime_profile\": \"default\"}\n```";
        let literals = texts(source);
        assert!(literals.contains(&"api_key".to_string()));
        assert!(literals.contains(&"runtime_profile".to_string()));
    }

    #[test]
    fn treats_jsonc_fences_as_json_not_shell() {
        let literals = texts(
            "```jsonc\n// args: { \"output\": \"Bumped bzip2; source hash verified.\" }\n```",
        );
        assert!(!literals.contains(&"source hash verified.\"".to_string()));
    }

    #[test]
    fn parses_fence_language_tags_from_table() {
        assert_eq!(FenceLanguage::from_tag("toml"), FenceLanguage::Toml);
        assert_eq!(FenceLanguage::from_tag("YAML"), FenceLanguage::Yaml);
        assert_eq!(FenceLanguage::from_tag("yml"), FenceLanguage::Yaml);
        assert_eq!(FenceLanguage::from_tag("jsonc"), FenceLanguage::Json);
        assert_eq!(FenceLanguage::from_tag("shell"), FenceLanguage::Generic);
    }

    #[test]
    fn reports_missing_label_literals() {
        let issue = missing_protected_literal("Use `status:blocked`.", "Utilisez `status:bloqué`.");
        assert_eq!(
            issue.as_ref().map(|literal| literal.text.as_str()),
            Some("status:blocked")
        );
    }

    #[test]
    fn preservation_prompt_lists_literals() {
        let prompt = preservation_prompt("Run `zeroclaw daemon` with OpenAI.").unwrap();
        assert!(prompt.contains("- zeroclaw daemon"));
        assert!(prompt.contains("- OpenAI"));
    }

    #[test]
    fn detects_generated_toml_fence_in_translation() {
        assert!(contains_generated_toml_block(
            "Configure the gateway before exposing it.",
            "在公开之前配置网关。\n\n```toml\n[gateway]\nhost = \"0.0.0.0\"\nport = 42617\n```",
        ));
    }

    #[test]
    fn detects_generated_toml_like_block_in_translation() {
        assert!(contains_generated_toml_block(
            "Set the model provider in config.",
            "[providers.models.openai.default]\napi_key = \"...\"\nmodel = \"gpt-5\"",
        ));
    }

    #[test]
    fn detects_generated_two_line_toml_like_block() {
        assert!(contains_generated_toml_block(
            "Configure the gateway before exposing it.",
            "[gateway]\nport = 42617",
        ));
    }

    #[test]
    fn allows_source_toml_blocks() {
        assert!(!contains_generated_toml_block(
            "```toml\n[gateway]\nhost = \"127.0.0.1\"\nport = 42617\n```",
            "```toml\n[gateway]\nhost = \"127.0.0.1\"\nport = 42617\n```",
        ));
    }
}
