// @Ref: SUMMARY §3 Step 5 — insert [[]] and [[rep|alias]] into markdown.
//
// Insertion strategy:
// - For every occurrence of a keyword in the body, wrap in `[[]]`.
// - For synonyms listed in the gatekeeper's synonym_pairs, render as
//   `[[representative|alias]]` — preserves original wording while
//   pointing backlinks to the representative.
// - Skip text inside existing `[[…]]` links, code fences, and inline code
//   to avoid double-wrapping or mangling code.

use super::WikilinkOutput;
use super::LinkRecord;
use std::collections::HashMap;

pub struct InsertResult {
    pub content: String,
    pub links: Vec<LinkRecord>,
}

/// Insert wikilinks into `markdown` for every occurrence of `keywords`
/// and every alias in the synonym map.
///
/// Deterministic: ordering is fixed by `keywords` input. Callers should
/// sort keywords by descending length so e.g. "민법 제750조" matches
/// before "750조" and avoids partial-match collisions.
pub fn insert_wikilinks(
    markdown: &str,
    keywords: &[String],
    synonyms: &HashMap<String, String>,
) -> InsertResult {
    // Build a (surface_form, target_canonical) list, longest first.
    let mut surface_to_target: Vec<(String, String)> = Vec::new();
    for k in keywords {
        surface_to_target.push((k.clone(), k.clone()));
    }
    for (alias, rep) in synonyms {
        // Only add aliases whose representative is in the keyword set.
        if keywords.iter().any(|k| k == rep) {
            surface_to_target.push((alias.clone(), rep.clone()));
        }
    }
    surface_to_target.sort_by(|a, b| b.0.chars().count().cmp(&a.0.chars().count()));
    surface_to_target.dedup_by(|a, b| a.0 == b.0);

    let mut out = String::with_capacity(markdown.len() + 64);
    let mut links: Vec<LinkRecord> = Vec::new();

    let mut line_no: u32 = 0;
    for line in markdown.split_inclusive('\n') {
        line_no += 1;
        out.push_str(&process_line(line, &surface_to_target, line_no, &mut links));
    }

    InsertResult { content: out, links }
}

fn process_line(
    line: &str,
    surface_to_target: &[(String, String)],
    line_no: u32,
    links: &mut Vec<LinkRecord>,
) -> String {
    // Skip inside existing [[…]] or `code`. Single-pass scan.
    let mut out = String::with_capacity(line.len() + 16);
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0usize;

    while i < chars.len() {
        // Existing wikilink → pass through.
        if chars[i] == '[' && chars.get(i + 1) == Some(&'[') {
            if let Some(end) = find_close(&chars, i + 2) {
                for c in &chars[i..=end + 1] {
                    out.push(*c);
                }
                i = end + 2;
                continue;
            }
        }
        // Inline code → pass through.
        if chars[i] == '`' {
            let mut j = i + 1;
            while j < chars.len() && chars[j] != '`' {
                j += 1;
            }
            for c in &chars[i..=j.min(chars.len() - 1)] {
                out.push(*c);
            }
            i = j + 1;
            continue;
        }

        // Try to match a keyword starting at i.
        let tail: String = chars[i..].iter().collect();
        if let Some((surface, target)) = surface_to_target
            .iter()
            .find(|(s, _)| tail.starts_with(s.as_str()))
        {
            let link_text = if surface == target {
                format!("[[{surface}]]")
            } else {
                format!("[[{target}|{surface}]]")
            };
            out.push_str(&link_text);
            links.push(LinkRecord {
                target_raw: target.clone(),
                display_text: surface.clone(),
                link_type: if surface == target { "wikilink".into() } else { "alias".into() },
                context: context_slice(line, i),
                line_number: line_no,
            });
            i += surface.chars().count();
            continue;
        }

        out.push(chars[i]);
        i += 1;
    }

    out
}

fn find_close(chars: &[char], start: usize) -> Option<usize> {
    let mut j = start;
    while j + 1 < chars.len() {
        if chars[j] == ']' && chars[j + 1] == ']' {
            return Some(j);
        }
        j += 1;
    }
    None
}

fn context_slice(line: &str, char_offset: usize) -> String {
    const WINDOW: usize = 40;
    let chars: Vec<char> = line.chars().collect();
    let start = char_offset.saturating_sub(WINDOW);
    let end = (char_offset + WINDOW).min(chars.len());
    chars[start..end].iter().collect::<String>().replace('\n', " ")
}

/// Rebuild annotated content from a raw markdown + pipeline verdict.
/// Convenience wrapper used by WikilinkPipeline::run — callers normally
/// use `insert_wikilinks` directly.
#[allow(dead_code)]
pub fn apply(output: &mut WikilinkOutput, markdown: &str) {
    let r = insert_wikilinks(markdown, &output.keywords, &output.synonyms);
    output.annotated_content = r.content;
    output.links = r.links;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_exact_matches() {
        let md = "이 사건은 민법 제750조에 근거합니다.";
        let result = insert_wikilinks(md, &["민법 제750조".into()], &HashMap::new());
        assert!(result.content.contains("[[민법 제750조]]"));
        assert_eq!(result.links.len(), 1);
    }

    #[test]
    fn alias_link_preserves_original_surface() {
        let md = "750조의 요건을 보자.";
        let mut synonyms = HashMap::new();
        synonyms.insert("750조".into(), "민법 제750조".into());
        let result = insert_wikilinks(md, &["민법 제750조".into()], &synonyms);
        assert!(result.content.contains("[[민법 제750조|750조]]"));
    }

    #[test]
    fn existing_wikilinks_untouched() {
        let md = "이미 [[민법 제750조]]는 연결됨";
        let result = insert_wikilinks(md, &["민법 제750조".into()], &HashMap::new());
        // Should not have [[[[…]]]]
        assert!(!result.content.contains("[[[["));
    }

    #[test]
    fn inline_code_skipped() {
        let md = "`민법 제750조` is a statute";
        let result = insert_wikilinks(md, &["민법 제750조".into()], &HashMap::new());
        assert!(!result.content.contains("[[민법 제750조]]"));
    }

    #[test]
    fn multiple_occurrences_all_linked() {
        let md = "민법 제750조와 민법 제750조";
        let result = insert_wikilinks(md, &["민법 제750조".into()], &HashMap::new());
        assert_eq!(result.links.len(), 2);
    }

    #[test]
    fn longest_match_first() {
        // "민법 제750조" should match before "750조" so we don't split inside it.
        let md = "민법 제750조가 핵심";
        let mut syn = HashMap::new();
        syn.insert("750조".into(), "민법 제750조".into());
        let result =
            insert_wikilinks(md, &["민법 제750조".into()], &syn);
        // Expect full form wrapped, not a partial "제[[민법 제750조|750조]]".
        assert!(result.content.contains("[[민법 제750조]]"));
        assert!(!result.content.contains("[[민법 제750조|750조]]"));
    }
}
