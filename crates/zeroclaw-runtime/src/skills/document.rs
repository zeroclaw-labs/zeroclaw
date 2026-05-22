//! Parse and serialize canonical `SKILL.md` files.
//!
//! A [`SkillDocument`] is the on-disk pair of frontmatter and body. The
//! splitter [`split_frontmatter`] is shared with the legacy `parse_skill_markdown`
//! path in `super` so both readers see the same delimiter rules.

use std::fmt::Write as _;

use super::frontmatter::SkillFrontmatter;

#[derive(Debug, Clone, PartialEq, Eq)]
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

    let flush = |fm: &mut SkillFrontmatter, key: &str, parts: &[String]| {
        let val = parts.join(" ");
        let val = val.trim();
        if val.is_empty() {
            return;
        }
        assign(fm, key, val);
    };

    for line in src.lines() {
        if let Some((ref key, ref mut parts)) = multiline {
            if line.starts_with(' ') || line.starts_with('\t') {
                parts.push(line.trim().to_string());
                continue;
            }
            let (key_owned, parts_owned) = (key.clone(), std::mem::take(parts));
            flush(&mut fm, &key_owned, &parts_owned);
            multiline = None;
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
        assign(&mut fm, key, value);
    }
    if let Some((key, parts)) = multiline {
        flush(&mut fm, &key, &parts);
    }

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
            },
            body: "# Code Review\n\nReviews diffs.\n".into(),
        };
        let parsed = SkillDocument::parse(&original.serialize()).unwrap();
        assert_eq!(parsed.frontmatter, original.frontmatter);
    }
}
