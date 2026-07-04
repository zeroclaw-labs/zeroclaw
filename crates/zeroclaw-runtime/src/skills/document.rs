//! Parse and serialize canonical `SKILL.md` files.
//!
//! A [`SkillDocument`] is the on-disk pair of frontmatter and body. The
//! splitter [`split_frontmatter`] is shared with the legacy `parse_skill_markdown`
//! path in `super` so both readers see the same delimiter rules.

use std::fmt::Write as _;

use super::frontmatter::SkillFrontmatter;
use super::{SkillSlashChoice, SkillSlashOption};

// `Eq` is intentionally NOT derived: the frontmatter's `slash_options` carry
// `f64` bounds (no total ordering). `PartialEq` covers the round-trip tests.
#[derive(Debug, Clone, PartialEq)]
pub struct SkillDocument {
    pub frontmatter: SkillFrontmatter,
    pub body: String,
}

#[derive(Debug, thiserror::Error)]
pub enum DocumentParseError {
    #[error("SKILL.md is missing the leading `---` frontmatter delimiter")]
    MissingFrontmatter,

    #[error("SKILL.md frontmatter is missing required field `{0}`")]
    MissingRequiredField(&'static str),
}

impl SkillDocument {
    pub fn parse(content: &str) -> Result<Self, DocumentParseError> {
        let (frontmatter_src, body) =
            split_frontmatter(content).ok_or(DocumentParseError::MissingFrontmatter)?;
        let frontmatter = parse_frontmatter(&frontmatter_src)?;
        // Strip the conventional blank line that follows the closing `---`;
        // callers see the body content directly.
        let body = body.strip_prefix('\n').map(String::from).unwrap_or(body);
        Ok(Self { frontmatter, body })
    }

    pub fn serialize(&self) -> String {
        let mut out = String::with_capacity(self.body.len() + 256);
        out.push_str("---\n");
        write_field(&mut out, "name", &self.frontmatter.name);
        write_block_scalar(&mut out, "description", &self.frontmatter.description);
        write_optional(&mut out, "license", self.frontmatter.license.as_deref());
        write_optional(&mut out, "author", self.frontmatter.author.as_deref());
        write_optional(&mut out, "version", self.frontmatter.version.as_deref());
        write_optional(&mut out, "category", self.frontmatter.category.as_deref());
        write_tags(&mut out, &self.frontmatter.tags);
        write_slash_options(&mut out, &self.frontmatter.slash_options);
        out.push_str("---\n");
        if !self.body.is_empty() {
            if !self.body.starts_with('\n') {
                out.push('\n');
            }
            out.push_str(&self.body);
            if !self.body.ends_with('\n') {
                out.push('\n');
            }
        }
        out
    }
}

/// Splits `---\n...\n---\n` from the body. Mirrors `super::split_skill_frontmatter`
/// — extracted here so future readers don't drift on delimiter handling.
pub fn split_frontmatter(content: &str) -> Option<(String, String)> {
    let normalized = content.replace("\r\n", "\n");
    let rest = normalized.strip_prefix("---\n")?;
    if let Some(idx) = rest.find("\n---\n") {
        return Some((rest[..idx].to_string(), rest[idx + 5..].to_string()));
    }
    if let Some(frontmatter) = rest.strip_suffix("\n---") {
        return Some((frontmatter.to_string(), String::new()));
    }
    None
}

/// Flat `key: value` parser tightly typed to [`SkillFrontmatter`]. Handles
/// inline strings and YAML block scalars (`>-`, `>`, `|`, `|-`) for
/// `description`. Does not attempt nested mappings; the schema is flat by
/// design.
fn parse_frontmatter(src: &str) -> Result<SkillFrontmatter, DocumentParseError> {
    let mut fm = SkillFrontmatter::default();
    let mut multiline: Option<(String, Vec<String>)> = None;
    let mut collecting_tags = false;

    // Carve the nested `slash_options:` block out of the flat scan: its indented
    // lines must not be (mis)read as flat keys — e.g. an option `description:`
    // would otherwise hijack the skill's block-scalar collector.
    let block_range = locate_slash_options_block(src);

    let flush = |fm: &mut SkillFrontmatter, key: &str, parts: &[String]| {
        let val = parts.join(" ");
        let val = val.trim();
        if val.is_empty() {
            return;
        }
        assign(fm, key, val);
    };

    for (idx, line) in src.lines().enumerate() {
        if let Some((start, end)) = block_range
            && idx >= start
            && idx < end
        {
            continue;
        }
        if let Some((ref key, ref mut parts)) = multiline {
            if line.starts_with(' ') || line.starts_with('\t') {
                parts.push(line.trim().to_string());
                continue;
            }
            let (key_owned, parts_owned) = (key.clone(), std::mem::take(parts));
            flush(&mut fm, &key_owned, &parts_owned);
            multiline = None;
        }
        // YAML block list under `tags:` — consume `- item` continuation lines
        // until a non-list line. Mirrors the loader's parser so both readers
        // agree on tag shape (zeroclaw-labs/zeroclaw#7490 reads the same tags).
        if collecting_tags {
            let trimmed = line.trim();
            if let Some(item) = trimmed.strip_prefix("- ") {
                let tag = item.trim().trim_matches('"').trim_matches('\'');
                if !tag.is_empty() {
                    fm.tags.push(tag.to_string());
                }
                continue;
            }
            collecting_tags = false;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim().trim_matches('"').trim_matches('\'');
        if matches!(value, ">-" | ">" | "|" | "|-") {
            multiline = Some((key.to_string(), Vec::new()));
            continue;
        }
        if key == "tags" {
            if value.is_empty() {
                // Block list (`tags:` then `  - item` lines) follows.
                collecting_tags = true;
            } else {
                // Inline flow list: `[a, b, c]` (or a bare comma list).
                let inner = value.trim_start_matches('[').trim_end_matches(']');
                fm.tags = inner
                    .split(',')
                    .map(|t| t.trim().trim_matches('"').trim_matches('\'').to_string())
                    .filter(|t| !t.is_empty())
                    .collect();
            }
            continue;
        }
        assign(&mut fm, key, value);
    }
    if let Some((key, parts)) = multiline {
        flush(&mut fm, &key, &parts);
    }

    // The one nested field. The flat loop above skips the `slash_options:`
    // block (its indented lines have no top-level `key: value` it recognizes);
    // the shared helper lifts it out and parses it.
    fm.slash_options = parse_slash_options(src);

    if fm.name.is_empty() {
        return Err(DocumentParseError::MissingRequiredField("name"));
    }
    if fm.description.is_empty() {
        return Err(DocumentParseError::MissingRequiredField("description"));
    }
    Ok(fm)
}

fn assign(fm: &mut SkillFrontmatter, key: &str, value: &str) {
    match key {
        "name" => fm.name = value.to_string(),
        "description" => fm.description = value.to_string(),
        "license" => fm.license = Some(value.to_string()),
        "author" => fm.author = Some(value.to_string()),
        "version" => fm.version = Some(value.to_string()),
        "category" => fm.category = Some(value.to_string()),
        _ => {}
    }
}

fn write_field(out: &mut String, key: &str, value: &str) {
    if value.contains('\n') {
        write_block_scalar(out, key, value);
    } else {
        let _ = writeln!(out, "{key}: {value}");
    }
}

fn write_block_scalar(out: &mut String, key: &str, value: &str) {
    if value.contains('\n') || value.len() > 80 {
        let _ = writeln!(out, "{key}: >-");
        for line in value.split('\n') {
            let _ = writeln!(out, "  {}", line.trim());
        }
    } else {
        let _ = writeln!(out, "{key}: {value}");
    }
}

fn write_optional(out: &mut String, key: &str, value: Option<&str>) {
    if let Some(v) = value
        && !v.is_empty()
    {
        write_field(out, key, v);
    }
}

/// Serialize tags as an inline flow list (`tags: [a, b]`) — compact and parsed
/// identically by both this reader and the loader's `parse_simple_frontmatter`.
/// Empty tags are omitted so a tagless skill stays byte-identical.
fn write_tags(out: &mut String, tags: &[String]) {
    if tags.is_empty() {
        return;
    }
    let _ = writeln!(out, "tags: [{}]", tags.join(", "));
}

// ─── Nested `slash_options:` (the one non-flat field) ──────────────────────
//
// Parsed/serialized here and shared with the loader's `parse_simple_frontmatter`
// so both readers agree on the shape. A scoped, dependency-free parser for the
// exact YAML subset the serializer below emits — see `parse_slash_options`.

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// Locate a top-level `slash_options:` block as a `[start, end)` line range
/// (header line included). The block is the header plus every following indented
/// (or blank) line, ending at the next top-level non-blank line or EOF. Only the
/// block form (`slash_options:` with an empty inline value) is recognized; an
/// inline `slash_options: [...]` is intentionally ignored.
fn locate_slash_options_block(src: &str) -> Option<(usize, usize)> {
    let lines: Vec<&str> = src.lines().collect();
    let start = lines.iter().position(|line| {
        if line.starts_with(' ') || line.starts_with('\t') {
            return false;
        }
        match line.split_once(':') {
            Some((key, value)) => key.trim() == "slash_options" && value.trim().is_empty(),
            None => false,
        }
    })?;
    let mut end = start + 1;
    while end < lines.len() {
        let line = lines[end];
        if line.trim().is_empty() || line.starts_with(' ') || line.starts_with('\t') {
            end += 1;
        } else {
            break;
        }
    }
    Some((start, end))
}

/// Parse the nested `slash_options:` block from a SKILL.md frontmatter source.
/// Absent block → empty. Lenient by design (matching the flat parser): an option
/// missing `name` or `type` is dropped rather than failing the whole parse.
///
/// Supported subset (exactly what `write_slash_options` emits — keeps the
/// hand-rolled parser tractable, no YAML dependency):
/// ```text
/// slash_options:
///   - name: format
///     description: Output format.
///     type: string
///     required: true
///     choices: [{name: Email, value: email}, {name: Tweet, value: tweet}]
///   - name: words
///     type: integer
///     min: 10
///     max: 2000
/// ```
/// `choices` accepts an inline flow list (above) or a block list of single-line
/// flow maps (`    - {name: X, value: Y}`). Option descriptions are single-line
/// (no block scalars inside an option).
pub(crate) fn parse_slash_options(src: &str) -> Vec<SkillSlashOption> {
    let Some((start, end)) = locate_slash_options_block(src) else {
        return Vec::new();
    };
    let lines: Vec<&str> = src.lines().collect();
    let block = &lines[start + 1..end];

    let mut options: Vec<SkillSlashOption> = Vec::new();
    let mut item_indent: Option<usize> = None;
    let mut cur: Option<OptionBuilder> = None;

    for &line in block {
        if line.trim().is_empty() {
            continue;
        }
        let indent = indent_of(line);
        let trimmed = line.trim_start();

        let dash_rest = if trimmed == "-" {
            Some("")
        } else {
            trimmed.strip_prefix("- ")
        };

        if let Some(rest) = dash_rest {
            let ii = *item_indent.get_or_insert(indent);
            if indent <= ii {
                // A new option item at the item indent.
                if let Some(b) = cur.take() {
                    options.extend(b.build());
                }
                let mut b = OptionBuilder::default();
                if !rest.trim().is_empty() {
                    apply_option_field(&mut b, rest.trim());
                }
                cur = Some(b);
            } else if let (Some(b), Some(choice)) = (cur.as_mut(), parse_choice(rest.trim())) {
                // A deeper dash → a `choices:` entry for the current option.
                b.choices.push(choice);
            }
            continue;
        }

        if let Some(b) = cur.as_mut() {
            apply_option_field(b, trimmed);
        }
    }
    if let Some(b) = cur.take() {
        options.extend(b.build());
    }
    options
}

#[derive(Default)]
struct OptionBuilder {
    name: String,
    description: String,
    kind: String,
    required: bool,
    choices: Vec<SkillSlashChoice>,
    min: Option<f64>,
    max: Option<f64>,
    min_length: Option<u32>,
    max_length: Option<u32>,
}

impl OptionBuilder {
    fn build(self) -> Option<SkillSlashOption> {
        if self.name.is_empty() || self.kind.is_empty() {
            return None;
        }
        Some(SkillSlashOption {
            name: self.name,
            description: self.description,
            // SKILL.md markdown frontmatter has no localization syntax; locale
            // dictionaries are a `[[skill.slash_options]]` (TOML) feature.
            description_localizations: Default::default(),
            kind: self.kind,
            required: self.required,
            choices: self.choices,
            min: self.min,
            max: self.max,
            min_length: self.min_length,
            max_length: self.max_length,
        })
    }
}

fn apply_option_field(b: &mut OptionBuilder, line: &str) {
    let Some((key, value)) = line.split_once(':') else {
        return;
    };
    let key = key.trim();
    let raw = value.trim();
    if key == "choices" {
        // Inline flow list; an empty value means a block list follows (handled
        // by the deeper-dash arm in `parse_slash_options`).
        if !raw.is_empty() {
            b.choices.extend(parse_inline_choices(raw));
        }
        return;
    }
    let val = raw.trim_matches('"').trim_matches('\'');
    match key {
        "name" => b.name = val.to_string(),
        "description" => b.description = val.to_string(),
        "type" => b.kind = val.to_string(),
        "required" => b.required = val.eq_ignore_ascii_case("true"),
        "min" => b.min = val.parse().ok(),
        "max" => b.max = val.parse().ok(),
        "min_length" => b.min_length = val.parse().ok(),
        "max_length" => b.max_length = val.parse().ok(),
        _ => {}
    }
}

/// Parse an inline flow list `[{name: A, value: a}, {name: B, value: b}]` by
/// extracting each balanced `{...}` group. Brace-depth tracking lets commas and
/// colons inside a choice be ignored by the outer split.
fn parse_inline_choices(s: &str) -> Vec<SkillSlashChoice> {
    let mut choices = Vec::new();
    let mut depth = 0usize;
    let mut buf = String::new();
    for ch in s.chars() {
        match ch {
            '{' => {
                depth += 1;
                buf.push(ch);
            }
            '}' => {
                buf.push(ch);
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    if let Some(choice) = parse_choice(&buf) {
                        choices.push(choice);
                    }
                    buf.clear();
                }
            }
            _ if depth > 0 => buf.push(ch),
            _ => {}
        }
    }
    choices
}

/// Parse a single flow map `{name: X, value: Y}` (braces optional). `name` is
/// required; `value` defaults to the display name when omitted.
fn parse_choice(s: &str) -> Option<SkillSlashChoice> {
    let inner = s.trim();
    let inner = inner.strip_prefix('{').unwrap_or(inner);
    let inner = inner.strip_suffix('}').unwrap_or(inner);
    let mut name: Option<String> = None;
    let mut value: Option<String> = None;
    for part in inner.split(',') {
        let Some((k, v)) = part.split_once(':') else {
            continue;
        };
        let v = v.trim().trim_matches('"').trim_matches('\'');
        match k.trim() {
            "name" => name = Some(v.to_string()),
            "value" => value = Some(v.to_string()),
            _ => {}
        }
    }
    let name = name.filter(|n| !n.is_empty())?;
    let value = value
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| name.clone());
    Some(SkillSlashChoice { name, value })
}

fn write_slash_options(out: &mut String, opts: &[SkillSlashOption]) {
    if opts.is_empty() {
        return;
    }
    out.push_str("slash_options:\n");
    for o in opts {
        let _ = writeln!(out, "  - name: {}", o.name);
        if !o.description.is_empty() {
            let _ = writeln!(out, "    description: {}", o.description);
        }
        let _ = writeln!(out, "    type: {}", o.kind);
        if o.required {
            out.push_str("    required: true\n");
        }
        if let Some(min) = o.min {
            let _ = writeln!(out, "    min: {}", fmt_number(min));
        }
        if let Some(max) = o.max {
            let _ = writeln!(out, "    max: {}", fmt_number(max));
        }
        if let Some(min_len) = o.min_length {
            let _ = writeln!(out, "    min_length: {min_len}");
        }
        if let Some(max_len) = o.max_length {
            let _ = writeln!(out, "    max_length: {max_len}");
        }
        if !o.choices.is_empty() {
            let rendered: Vec<String> = o
                .choices
                .iter()
                .map(|c| format!("{{name: {}, value: {}}}", c.name, c.value))
                .collect();
            let _ = writeln!(out, "    choices: [{}]", rendered.join(", "));
        }
    }
}

/// Render an f64 bound without a trailing `.0` for whole numbers (`min: 10`,
/// not `min: 10.0`); both parse back to the same f64.
fn fmt_number(v: f64) -> String {
    if v.is_finite() && v.fract() == 0.0 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_canonical_frontmatter() {
        let content = "---\nname: code-review\ndescription: Reviews PRs.\n---\n# Body\n";
        let doc = SkillDocument::parse(content).unwrap();
        assert_eq!(doc.frontmatter.name, "code-review");
        assert_eq!(doc.frontmatter.description, "Reviews PRs.");
        assert_eq!(doc.body, "# Body\n");
    }

    #[test]
    fn parses_block_scalar_description() {
        let content = "---\nname: x\ndescription: >-\n  multi-line\n  description text\n---\n";
        let doc = SkillDocument::parse(content).unwrap();
        assert_eq!(doc.frontmatter.description, "multi-line description text");
    }

    #[test]
    fn parses_optional_flat_fields() {
        let content = "---\nname: x\ndescription: y\nlicense: MIT\nauthor: alice\nversion: 0.1.0\ncategory: coding\n---\n";
        let doc = SkillDocument::parse(content).unwrap();
        assert_eq!(doc.frontmatter.license.as_deref(), Some("MIT"));
        assert_eq!(doc.frontmatter.author.as_deref(), Some("alice"));
        assert_eq!(doc.frontmatter.version.as_deref(), Some("0.1.0"));
        assert_eq!(doc.frontmatter.category.as_deref(), Some("coding"));
    }

    #[test]
    fn rejects_missing_required_name() {
        let content = "---\ndescription: y\n---\n";
        let err = SkillDocument::parse(content).unwrap_err();
        assert!(matches!(
            err,
            DocumentParseError::MissingRequiredField("name")
        ));
    }

    #[test]
    fn rejects_missing_required_description() {
        let content = "---\nname: x\n---\n";
        let err = SkillDocument::parse(content).unwrap_err();
        assert!(matches!(
            err,
            DocumentParseError::MissingRequiredField("description")
        ));
    }

    #[test]
    fn rejects_missing_frontmatter_delimiter() {
        let content = "# No frontmatter\n";
        let err = SkillDocument::parse(content).unwrap_err();
        assert!(matches!(err, DocumentParseError::MissingFrontmatter));
    }

    #[test]
    fn round_trips_minimal_document() {
        let original = SkillDocument {
            frontmatter: SkillFrontmatter {
                name: "x".into(),
                description: "y".into(),
                ..Default::default()
            },
            body: "# X\n\nDoes X.\n".into(),
        };
        let serialized = original.serialize();
        let parsed = SkillDocument::parse(&serialized).unwrap();
        assert_eq!(parsed.frontmatter, original.frontmatter);
        assert_eq!(parsed.body.trim_end(), original.body.trim_end());
    }

    #[test]
    fn round_trips_with_optional_fields() {
        let original = SkillDocument {
            frontmatter: SkillFrontmatter {
                name: "code-review".into(),
                description: "Review pull requests for correctness, security, and style.".into(),
                license: Some("MIT".into()),
                author: Some("zeroclaw-labs".into()),
                version: Some("0.2.0".into()),
                category: Some("coding".into()),
                tags: vec!["slash".into(), "ops".into()],
                slash_options: Vec::new(),
            },
            body: "# Code Review\n\nReviews diffs.\n".into(),
        };
        let parsed = SkillDocument::parse(&original.serialize()).unwrap();
        assert_eq!(parsed.frontmatter, original.frontmatter);
    }

    #[test]
    fn parses_inline_and_block_tags() {
        let inline = "---\nname: x\ndescription: y\ntags: [slash, ops]\n---\n";
        assert_eq!(
            SkillDocument::parse(inline).unwrap().frontmatter.tags,
            vec!["slash", "ops"]
        );
        let block = "---\nname: x\ndescription: y\ntags:\n  - slash\n  - ops\n---\n";
        assert_eq!(
            SkillDocument::parse(block).unwrap().frontmatter.tags,
            vec!["slash", "ops"]
        );
    }

    #[test]
    fn editing_preserves_tags() {
        // Regression for the strip-on-save bug: parse -> serialize -> parse
        // keeps the tags instead of dropping them.
        let original = "---\nname: x\ndescription: y\ntags: [slash, open-skills]\n---\n# Body\n";
        let doc = SkillDocument::parse(original).unwrap();
        let reparsed = SkillDocument::parse(&doc.serialize()).unwrap();
        assert_eq!(reparsed.frontmatter.tags, vec!["slash", "open-skills"]);
    }

    #[test]
    fn parses_slash_options_with_inline_choices_and_bounds() {
        let content = "---\nname: draft\ndescription: d\ntags: [slash]\nslash_options:\n  \
            - name: format\n    description: Output format.\n    type: string\n    \
            required: true\n    choices: [{name: Email, value: email}, {name: Tweet, value: tweet}]\n  \
            - name: words\n    type: integer\n    min: 10\n    max: 2000\n---\n# Draft\n";
        let fm = SkillDocument::parse(content).unwrap().frontmatter;
        assert_eq!(fm.slash_options.len(), 2);
        let format = &fm.slash_options[0];
        assert_eq!(format.name, "format");
        assert_eq!(format.description, "Output format.");
        assert_eq!(format.kind, "string");
        assert!(format.required);
        assert_eq!(format.choices.len(), 2);
        assert_eq!(format.choices[0].name, "Email");
        assert_eq!(format.choices[0].value, "email");
        let words = &fm.slash_options[1];
        assert_eq!(words.kind, "integer");
        assert!(!words.required);
        assert_eq!(words.min, Some(10.0));
        assert_eq!(words.max, Some(2000.0));
        // Flat fields are unaffected by the nested block.
        assert_eq!(fm.tags, vec!["slash"]);
        assert_eq!(fm.description, "d");
    }

    #[test]
    fn parses_slash_options_with_block_choices() {
        let content = "---\nname: x\ndescription: d\nslash_options:\n  - name: tone\n    \
            type: string\n    choices:\n      - {name: Formal, value: formal}\n      \
            - {name: Casual, value: casual}\n---\n";
        let fm = SkillDocument::parse(content).unwrap().frontmatter;
        assert_eq!(fm.slash_options.len(), 1);
        assert_eq!(fm.slash_options[0].choices.len(), 2);
        assert_eq!(fm.slash_options[0].choices[1].name, "Casual");
        assert_eq!(fm.slash_options[0].choices[1].value, "casual");
    }

    #[test]
    fn absent_slash_options_is_empty() {
        let fm = SkillDocument::parse("---\nname: x\ndescription: d\n---\n# Body\n")
            .unwrap()
            .frontmatter;
        assert!(fm.slash_options.is_empty());
    }

    #[test]
    fn drops_option_missing_name_or_type() {
        let content = "---\nname: x\ndescription: d\nslash_options:\n  - name: notype\n  \
            - type: string\n  - name: ok\n    type: string\n---\n";
        let fm = SkillDocument::parse(content).unwrap().frontmatter;
        assert_eq!(fm.slash_options.len(), 1);
        assert_eq!(fm.slash_options[0].name, "ok");
    }

    #[test]
    fn slash_options_block_does_not_pollute_skill_description() {
        let content = "---\nname: x\ndescription: real skill description\nslash_options:\n  \
            - name: q\n    description: option description\n    type: string\n---\n";
        let fm = SkillDocument::parse(content).unwrap().frontmatter;
        assert_eq!(fm.description, "real skill description");
        assert_eq!(fm.slash_options[0].description, "option description");
    }

    #[test]
    fn description_block_scalar_survives_a_following_slash_options_block() {
        let content = "---\nname: x\ndescription: >-\n  long multi\n  line desc\nslash_options:\n  \
            - name: q\n    type: string\n---\n";
        let fm = SkillDocument::parse(content).unwrap().frontmatter;
        assert_eq!(fm.description, "long multi line desc");
        assert_eq!(fm.slash_options.len(), 1);
    }

    #[test]
    fn round_trips_slash_options() {
        let original = SkillDocument {
            frontmatter: SkillFrontmatter {
                name: "draft".into(),
                description: "Draft content.".into(),
                tags: vec!["slash".into()],
                slash_options: vec![
                    SkillSlashOption {
                        name: "format".into(),
                        description: "Output format.".into(),
                        description_localizations: Default::default(),
                        kind: "string".into(),
                        required: true,
                        choices: vec![
                            SkillSlashChoice {
                                name: "Email".into(),
                                value: "email".into(),
                            },
                            SkillSlashChoice {
                                name: "Tweet".into(),
                                value: "tweet".into(),
                            },
                        ],
                        min: None,
                        max: None,
                        min_length: None,
                        max_length: None,
                    },
                    SkillSlashOption {
                        name: "words".into(),
                        description: String::new(),
                        description_localizations: Default::default(),
                        kind: "integer".into(),
                        required: false,
                        choices: vec![],
                        min: Some(10.0),
                        max: Some(2000.0),
                        min_length: None,
                        max_length: None,
                    },
                ],
                ..Default::default()
            },
            body: "# Draft\n\nBody.\n".into(),
        };
        let parsed = SkillDocument::parse(&original.serialize()).unwrap();
        assert_eq!(parsed.frontmatter, original.frontmatter);
    }
}
