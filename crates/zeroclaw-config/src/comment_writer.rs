//! Shared TOML comment-writing helpers used by both the gateway HTTP CRUD
//! handlers and the CLI `zeroclaw config set --comment` / `zeroclaw config patch`
//! flow. Walks a `toml_edit::DocumentMut` to a leaf key by dotted path and
//! decorates its leading whitespace with `# {comment}\n`. Empty comment string
//! strips comment lines from the existing prefix.
//!
//! Single source of truth — neither the gateway nor the CLI should re-implement
//! this logic. Issue #6175.

use std::path::Path;

/// Apply a batch of `(dotted_path, comment)` annotations to the on-disk TOML
/// file at `config_path`. Comments are written to the leaf key's leading decor
/// (the line above `key = value`), preserving blank lines and stripping any
/// prior `#`-prefixed lines.
///
/// Best-effort: silently skips paths that don't resolve to a leaf key. Fails
/// only on I/O errors.
pub async fn apply_comments(
    config_path: &Path,
    annotations: &[(String, String)],
) -> Result<(), std::io::Error> {
    if annotations.is_empty() {
        return Ok(());
    }
    let raw = tokio::fs::read_to_string(config_path).await?;
    let mut doc: toml_edit::DocumentMut = match raw.parse() {
        Ok(d) => d,
        Err(_) => return Ok(()), // unparseable; bail without touching file
    };
    for (path, comment) in annotations {
        decorate_key(doc.as_table_mut(), path, comment);
    }
    tokio::fs::write(config_path, doc.to_string()).await
}

/// Walk to the leaf key for `dotted` and decorate it with `# {comment}\n`,
/// preserving any non-comment whitespace already in the prefix. Empty comment
/// strips comment lines from the existing prefix while leaving blank lines.
pub fn decorate_key(root: &mut toml_edit::Table, dotted: &str, comment: &str) {
    let segments: Vec<&str> = dotted.split('.').collect();
    let (last, rest) = match segments.split_last() {
        Some(s) => s,
        None => return,
    };
    fn walk<'a>(
        table: &'a mut toml_edit::Table,
        segs: &[&str],
    ) -> Option<&'a mut toml_edit::Table> {
        let mut cursor = table;
        for seg in segs {
            cursor = cursor.get_mut(seg)?.as_table_mut()?;
        }
        Some(cursor)
    }
    let table = match walk(root, rest) {
        Some(t) => t,
        None => return,
    };
    if let Some(mut key) = table.key_mut(last) {
        let decor = key.leaf_decor_mut();
        let new_prefix = build_comment_prefix(decor.prefix(), comment);
        decor.set_prefix(new_prefix);
    }
}

/// Build the new leading decor for a leaf, applying `# {comment}\n` while
/// preserving any non-comment whitespace already in the prefix. Empty `comment`
/// strips `#`-prefixed lines from the existing prefix.
pub fn build_comment_prefix(existing: Option<&toml_edit::RawString>, comment: &str) -> String {
    let prev = existing.and_then(|r| r.as_str()).unwrap_or("");
    let mut kept = String::new();
    for line in prev.split_inclusive('\n') {
        if !line.trim_start().starts_with('#') {
            kept.push_str(line);
        }
    }
    if !comment.is_empty() {
        for line in comment.lines() {
            kept.push_str("# ");
            kept.push_str(line);
            kept.push('\n');
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_comment_prefix_appends_to_blank() {
        assert_eq!(build_comment_prefix(None, "why"), "# why\n");
    }

    #[test]
    fn build_comment_prefix_replaces_existing_comment() {
        let raw = toml_edit::RawString::from("\n# old\n");
        let out = build_comment_prefix(Some(&raw), "new");
        assert!(out.contains("# new\n"));
        assert!(!out.contains("old"));
        assert!(out.starts_with('\n')); // blank line preserved
    }

    #[test]
    fn build_comment_prefix_empty_strips() {
        let raw = toml_edit::RawString::from("\n# stale\n");
        let out = build_comment_prefix(Some(&raw), "");
        assert!(!out.contains('#'));
        assert_eq!(out, "\n");
    }

    #[test]
    fn build_comment_prefix_preserves_multi_blank_lines() {
        let raw = toml_edit::RawString::from("\n\n# inline\n");
        let out = build_comment_prefix(Some(&raw), "fresh");
        assert!(out.starts_with("\n\n"));
        assert!(out.contains("# fresh\n"));
        assert!(!out.contains("inline"));
    }

    #[test]
    fn build_comment_prefix_handles_multiline_comment() {
        let out = build_comment_prefix(None, "first\nsecond\nthird");
        assert_eq!(out, "# first\n# second\n# third\n");
    }
}
