//! Observer — detect user edits and record them as correction observations.

use serde::{Deserialize, Serialize};

/// A single observed correction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorrectionObservation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
    pub uuid: String,
    pub original_text: String,
    pub corrected_text: String,
    pub context_before: Option<String>,
    pub context_after: Option<String>,
    pub document_type: Option<String>,
    pub category: Option<String>,
    pub source: String,
    pub grammar_valid: bool,
    pub observed_at: i64,
    pub session_id: Option<String>,
}

/// How much context to capture around each edit.
const CONTEXT_CHARS: usize = 50;
/// Minimum significant edit length.
const MIN_EDIT_LEN: usize = 1;

/// Observe edits between two versions of a document.
///
/// Uses a simple word-level diff to detect replacements. Insertions
/// (pure adds) and deletions (pure removes) are ignored — we only
/// care about replacements (where the user changed something).
pub fn observe_edit(
    before: &str,
    after: &str,
    doc_type: Option<&str>,
    category: Option<&str>,
    session_id: Option<&str>,
) -> Vec<CorrectionObservation> {
    let diffs = word_diff(before, after);
    let now = chrono::Utc::now().timestamp();

    diffs
        .into_iter()
        .filter_map(|d| {
            // Only interested in replacements, and skip trivial whitespace changes
            if matches!(d.kind, DiffKind::Replace)
                && d.removed.trim() != d.added.trim()
                && d.removed.trim().len() >= MIN_EDIT_LEN
                && d.added.trim().len() >= MIN_EDIT_LEN
            {
                let context_before = Some(extract_context(before, d.start, CONTEXT_CHARS, false));
                let context_after =
                    Some(extract_context(before, d.end_before, CONTEXT_CHARS, true));

                Some(CorrectionObservation {
                    id: None,
                    uuid: uuid::Uuid::new_v4().to_string(),
                    original_text: d.removed,
                    corrected_text: d.added,
                    context_before,
                    context_after,
                    document_type: doc_type.map(str::to_string),
                    category: category.map(str::to_string),
                    source: "user_edit".to_string(),
                    grammar_valid: true, // validated later
                    observed_at: now,
                    session_id: session_id.map(str::to_string),
                })
            } else {
                None
            }
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DiffKind {
    Replace,
    Insert,
    Delete,
    Equal,
}

#[derive(Debug, Clone)]
struct WordDiff {
    kind: DiffKind,
    removed: String,
    added: String,
    /// Byte offset into `before` where the diff starts.
    start: usize,
    /// Byte offset into `before` where the diff ends.
    end_before: usize,
}

/// Simple word-level diff between two strings.
///
/// Uses LCS-style diff at the word boundary level and groups adjacent
/// changes into Replace ops. Good enough for observing edits — not
/// meant to match the semantics of a proper diff tool.
fn word_diff(before: &str, after: &str) -> Vec<WordDiff> {
    // Split on whitespace while keeping positions
    let before_tokens = tokenize(before);
    let after_tokens = tokenize(after);

    let lcs = lcs_indices(
        &before_tokens.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(),
        &after_tokens.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(),
    );

    let mut diffs = Vec::new();
    let mut i_before = 0;
    let mut i_after = 0;

    for (lcs_b, lcs_a) in &lcs {
        // Everything before these indices is a diff
        if i_before < *lcs_b || i_after < *lcs_a {
            let removed = before_tokens[i_before..*lcs_b]
                .iter()
                .map(|t| t.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            let added = after_tokens[i_after..*lcs_a]
                .iter()
                .map(|t| t.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");

            let kind = if removed.is_empty() {
                DiffKind::Insert
            } else if added.is_empty() {
                DiffKind::Delete
            } else {
                DiffKind::Replace
            };

            let start = before_tokens.get(i_before).map(|t| t.start).unwrap_or(before.len());
            let end_before = if *lcs_b > i_before {
                before_tokens[*lcs_b - 1].end
            } else {
                start
            };

            diffs.push(WordDiff {
                kind,
                removed,
                added,
                start,
                end_before,
            });
        }
        i_before = *lcs_b + 1;
        i_after = *lcs_a + 1;
    }

    // Trailing diff
    if i_before < before_tokens.len() || i_after < after_tokens.len() {
        let removed = before_tokens.get(i_before..)
            .map(|s| s.iter().map(|t| t.text.as_str()).collect::<Vec<_>>().join(" "))
            .unwrap_or_default();
        let added = after_tokens.get(i_after..)
            .map(|s| s.iter().map(|t| t.text.as_str()).collect::<Vec<_>>().join(" "))
            .unwrap_or_default();

        let kind = if removed.is_empty() {
            DiffKind::Insert
        } else if added.is_empty() {
            DiffKind::Delete
        } else {
            DiffKind::Replace
        };

        let start = before_tokens.get(i_before).map(|t| t.start).unwrap_or(before.len());
        let end_before = before_tokens.last().map(|t| t.end).unwrap_or(before.len());

        diffs.push(WordDiff {
            kind,
            removed,
            added,
            start,
            end_before,
        });
    }

    diffs
}

#[derive(Debug, Clone)]
struct Token {
    text: String,
    start: usize,
    end: usize,
}

/// Tokenize on whitespace while preserving byte offsets.
fn tokenize(s: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut start = 0usize;
    let mut in_token = false;

    for (i, ch) in s.char_indices() {
        if ch.is_whitespace() {
            if in_token {
                tokens.push(Token {
                    text: s[start..i].to_string(),
                    start,
                    end: i,
                });
                in_token = false;
            }
        } else if !in_token {
            start = i;
            in_token = true;
        }
    }

    if in_token {
        tokens.push(Token {
            text: s[start..].to_string(),
            start,
            end: s.len(),
        });
    }

    tokens
}

/// Compute LCS indices between two token sequences.
/// Returns (before_index, after_index) pairs for matching tokens.
fn lcs_indices(a: &[&str], b: &[&str]) -> Vec<(usize, usize)> {
    let n = a.len();
    let m = b.len();
    if n == 0 || m == 0 {
        return Vec::new();
    }

    // DP table
    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    for i in 0..n {
        for j in 0..m {
            dp[i + 1][j + 1] = if a[i] == b[j] {
                dp[i][j] + 1
            } else {
                dp[i][j + 1].max(dp[i + 1][j])
            };
        }
    }

    // Backtrack
    let mut i = n;
    let mut j = m;
    let mut out = Vec::with_capacity(dp[n][m]);
    while i > 0 && j > 0 {
        if a[i - 1] == b[j - 1] {
            out.push((i - 1, j - 1));
            i -= 1;
            j -= 1;
        } else if dp[i - 1][j] >= dp[i][j - 1] {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    out.reverse();
    out
}

/// Extract context around a byte position.
fn extract_context(s: &str, pos: usize, max_chars: usize, after: bool) -> String {
    if after {
        let end = (pos + max_chars).min(s.len());
        // Ensure we land on a char boundary
        let mut end_adj = end;
        while end_adj > pos && !s.is_char_boundary(end_adj) {
            end_adj -= 1;
        }
        s[pos.min(s.len())..end_adj].to_string()
    } else {
        let start = pos.saturating_sub(max_chars);
        let mut start_adj = start;
        while start_adj < pos && !s.is_char_boundary(start_adj) {
            start_adj += 1;
        }
        s[start_adj..pos.min(s.len())].to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observe_detects_replacement() {
        let before = "원고는 피고에게 청구하였다";
        let after = "원고는 피고에게 청구합니다";
        let obs = observe_edit(before, after, Some("legal_brief"), Some("document"), None);
        assert_eq!(obs.len(), 1);
        assert_eq!(obs[0].original_text, "청구하였다");
        assert_eq!(obs[0].corrected_text, "청구합니다");
        assert_eq!(obs[0].document_type.as_deref(), Some("legal_brief"));
        assert_eq!(obs[0].source, "user_edit");
    }

    #[test]
    fn observe_ignores_no_changes() {
        let before = "no change";
        let after = "no change";
        let obs = observe_edit(before, after, None, None, None);
        assert!(obs.is_empty());
    }

    #[test]
    fn observe_ignores_pure_insertion() {
        let before = "hello";
        let after = "hello world";
        let obs = observe_edit(before, after, None, None, None);
        // Pure insertion — not a replacement, should be empty
        assert!(obs.is_empty());
    }

    #[test]
    fn word_diff_finds_single_word_change() {
        let diffs = word_diff("foo bar baz", "foo qux baz");
        let replacements: Vec<_> = diffs.iter().filter(|d| d.kind == DiffKind::Replace).collect();
        assert_eq!(replacements.len(), 1);
        assert_eq!(replacements[0].removed, "bar");
        assert_eq!(replacements[0].added, "qux");
    }

    #[test]
    fn tokenize_handles_multibyte() {
        let tokens = tokenize("한글 테스트");
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[0].text, "한글");
        assert_eq!(tokens[1].text, "테스트");
    }
}
