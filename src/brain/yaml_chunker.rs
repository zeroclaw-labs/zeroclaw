//! YAML-aware chunking — splits YAML files by top-level keys.
//!
//! Each top-level key becomes a chunk. If a chunk exceeds `max_chars`,
//! it's split at second-level keys. The `_meta` key (if present) is
//! prepended to all chunks as context.

/// A YAML chunk with key context.
#[derive(Debug, Clone)]
pub struct YamlChunk {
    pub index: usize,
    pub key: String,
    pub content: String,
}

/// Split a YAML file into chunks by top-level keys.
///
/// - Each top-level mapping key becomes a separate chunk
/// - `_meta` is prepended to every chunk as context
/// - Keys producing content larger than `max_chars` are split at 2nd-level keys
/// - Token estimate: ~4 chars per token
pub fn chunk_yaml(text: &str, max_chars: usize) -> Vec<YamlChunk> {
    if text.trim().is_empty() {
        return Vec::new();
    }

    let parsed: serde_yaml::Value = match serde_yaml::from_str(text) {
        Ok(v) => v,
        Err(_) => {
            // If YAML is invalid, treat entire file as one chunk
            return vec![YamlChunk {
                index: 0,
                key: "raw".to_string(),
                content: text.to_string(),
            }];
        }
    };

    let mapping = match parsed.as_mapping() {
        Some(m) => m,
        None => {
            return vec![YamlChunk {
                index: 0,
                key: "root".to_string(),
                content: text.to_string(),
            }];
        }
    };

    // Extract _meta block for prepending to all chunks
    let meta_key = serde_yaml::Value::String("_meta".to_string());
    let meta_text = mapping
        .get(&meta_key)
        .and_then(|v| serde_yaml::to_string(v).ok())
        .map(|s| format!("_meta:\n{}", indent_yaml(&s)));

    let mut chunks = Vec::new();

    for (key, value) in mapping {
        let key_str = key_to_string(key);
        if key_str == "_meta" {
            continue;
        }

        let value_yaml = match serde_yaml::to_string(value) {
            Ok(s) => s,
            Err(_) => continue,
        };

        let chunk_content = format!("{key_str}:\n{}", indent_yaml(&value_yaml));
        let full_content = prepend_meta(meta_text.as_ref(), &chunk_content);

        if full_content.len() <= max_chars {
            chunks.push(YamlChunk {
                index: chunks.len(),
                key: key_str,
                content: full_content,
            });
        } else {
            // Split at 2nd-level keys
            split_second_level(&key_str, value, meta_text.as_ref(), max_chars, &mut chunks);
        }
    }

    // Re-index
    for (i, chunk) in chunks.iter_mut().enumerate() {
        chunk.index = i;
    }

    chunks
}

/// Split a value at its second-level keys when the top-level chunk is too large.
fn split_second_level(
    parent_key: &str,
    value: &serde_yaml::Value,
    meta_text: Option<&String>,
    max_chars: usize,
    chunks: &mut Vec<YamlChunk>,
) {
    if let Some(mapping) = value.as_mapping() {
        for (k, v) in mapping {
            let sub_key = key_to_string(k);
            let sub_yaml = match serde_yaml::to_string(v) {
                Ok(s) => s,
                Err(_) => continue,
            };

            let content = format!("{parent_key}.{sub_key}:\n{}", indent_yaml(&sub_yaml));
            let full = prepend_meta(meta_text, &content);

            // If still too large, just truncate — 3rd level splitting isn't worth the complexity
            let final_content = if full.len() > max_chars {
                full[..max_chars.min(full.len())].to_string()
            } else {
                full
            };

            chunks.push(YamlChunk {
                index: chunks.len(),
                key: format!("{parent_key}.{sub_key}"),
                content: final_content,
            });
        }
    } else {
        // Not a mapping — emit as single chunk, truncated if needed
        let value_yaml = serde_yaml::to_string(value).unwrap_or_default();
        let content = format!("{parent_key}:\n{}", indent_yaml(&value_yaml));
        let full = prepend_meta(meta_text, &content);
        let final_content = if full.len() > max_chars {
            full[..max_chars.min(full.len())].to_string()
        } else {
            full
        };

        chunks.push(YamlChunk {
            index: chunks.len(),
            key: parent_key.to_string(),
            content: final_content,
        });
    }
}

fn prepend_meta(meta: Option<&String>, content: &str) -> String {
    match meta {
        Some(m) => format!("{m}\n\n{content}"),
        None => content.to_string(),
    }
}

fn key_to_string(key: &serde_yaml::Value) -> String {
    match key {
        serde_yaml::Value::String(s) => s.clone(),
        other => format!("{other:?}"),
    }
}

/// Indent YAML output by 2 spaces (serde_yaml outputs without leading indent).
fn indent_yaml(s: &str) -> String {
    s.lines()
        .map(|line| {
            if line.trim().is_empty() {
                String::new()
            } else {
                format!("  {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_text() {
        assert!(chunk_yaml("", 2000).is_empty());
        assert!(chunk_yaml("   ", 2000).is_empty());
    }

    #[test]
    fn simple_yaml() {
        let yaml = "name: Joel\nrole: founder\n";
        let chunks = chunk_yaml(yaml, 2000);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].key, "name");
        assert_eq!(chunks[1].key, "role");
    }

    #[test]
    fn meta_prepended() {
        let yaml = "_meta:\n  version: '2.0'\nidentity:\n  name: Joel\n";
        let chunks = chunk_yaml(yaml, 2000);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].key, "identity");
        assert!(chunks[0].content.contains("_meta:"));
        assert!(chunks[0].content.contains("identity:"));
    }

    #[test]
    fn large_value_splits_at_second_level() {
        let mut yaml = String::from("_meta:\n  version: '2.0'\nsession:\n");
        for i in 0..50 {
            use std::fmt::Write;
            writeln!(yaml, "  key_{i}: {}", "x".repeat(100)).unwrap();
        }
        let chunks = chunk_yaml(&yaml, 500);
        assert!(chunks.len() > 1, "Expected split, got {}", chunks.len());
        // All chunks should have the parent key prefix
        for chunk in &chunks {
            assert!(
                chunk.key.starts_with("session."),
                "Expected session.* key, got {}",
                chunk.key
            );
        }
    }

    #[test]
    fn invalid_yaml_returns_single_chunk() {
        let yaml = "this: is: not: valid:\nyaml";
        let chunks = chunk_yaml(yaml, 2000);
        assert!(!chunks.is_empty());
    }

    #[test]
    fn indexes_are_sequential() {
        let yaml = "a: 1\nb: 2\nc: 3\n";
        let chunks = chunk_yaml(yaml, 2000);
        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.index, i);
        }
    }
}
