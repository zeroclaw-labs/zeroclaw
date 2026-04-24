//! Source-path introspection for legal ingestion.
//!
//! The user's corpus layout is:
//!
//! ```text
//! <root>/현행법령/<YYYYMMDD>/<법령명>.md    ← currently in-force version
//! <root>/연혁법령/<YYYYMMDD>/<법령명>.md    ← historical (pre-amendment) version
//! ```
//!
//! This module extracts:
//!   - [`SourceCategory`] — `Current` (현행법령) / `Historical` (연혁법령) /
//!     `Unknown` (neither marker in the path)
//!   - **publish date** as `YYYYMMDD`, pulled from the parent folder
//!     name. Accepts common separator variants (`20250131`,
//!     `2025-01-31`, `2025.01.31`) by stripping non-digits.
//!
//! The JSON `meta.ancYd` inside each statute file is authoritative
//! for the 공포일, but the path signal is cheap, independent, and
//! used as a fallback when the JSON is malformed. When both are
//! present and disagree, the caller decides which wins — the default
//! policy (see `ingest.rs`) is to prefer JSON.

use regex::Regex;
use std::path::Path;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceCategory {
    /// Path contained `현행법령` — this is the currently in-force version.
    Current,
    /// Path contained `연혁법령` — this is a pre-amendment historical version.
    Historical,
    /// Neither marker present. Caller falls back to treating the doc as Current
    /// so backward compat with the pre-versioning ingest API is preserved.
    Unknown,
}

impl SourceCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Historical => "historical",
            Self::Unknown => "unknown",
        }
    }
}

/// Parsed path metadata used by the ingester.
#[derive(Debug, Clone)]
pub struct SourcePathMeta {
    pub category: SourceCategory,
    /// `YYYYMMDD` string when a date folder was recognised; `None` otherwise.
    pub publish_date: Option<String>,
}

/// Inspect a source path string and return the category + publish date.
/// Windows-style backslashes and Unix slashes are both accepted.
pub fn parse(path: &str) -> SourcePathMeta {
    SourcePathMeta {
        category: detect_category(path),
        publish_date: detect_publish_date(path),
    }
}

/// Overload taking a `Path` for ergonomic callers.
pub fn parse_path(path: &Path) -> SourcePathMeta {
    parse(&path.to_string_lossy())
}

fn detect_category(path: &str) -> SourceCategory {
    if path.contains("현행법령") {
        SourceCategory::Current
    } else if path.contains("연혁법령") {
        SourceCategory::Historical
    } else {
        SourceCategory::Unknown
    }
}

/// Walk the path's ancestors, trying each component against a
/// date-folder regex. The deepest (closest to file) match wins, so
/// layouts like `연혁법령/민법/20200526/민법.md` work as well as the
/// primary `연혁법령/20200526/민법.md`.
fn detect_publish_date(path: &str) -> Option<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    // Strict form per user-confirmed layout: exactly 8 digits, no
    // separators, one date folder per corpus.
    let re = RE.get_or_init(|| Regex::new(r"^(\d{4})(\d{2})(\d{2})$").unwrap());

    // Split on both separators so we work on Windows + Unix.
    let components: Vec<&str> = path.split(['/', '\\']).collect();
    for comp in components.iter().rev() {
        if let Some(caps) = re.captures(comp) {
            let y = caps.get(1)?.as_str();
            let m = caps.get(2)?.as_str();
            let d = caps.get(3)?.as_str();
            // Sanity check: month 1-12, day 1-31 (cheap validation).
            if (1..=12).contains(&m.parse::<u32>().ok()?)
                && (1..=31).contains(&d.parse::<u32>().ok()?)
            {
                return Some(format!("{y}{m}{d}"));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_current_category() {
        let meta = parse(r"D:\국가법령정보api\현행법령\20260127\근로기준법.md");
        assert_eq!(meta.category, SourceCategory::Current);
        assert_eq!(meta.publish_date.as_deref(), Some("20260127"));
    }

    #[test]
    fn detects_historical_category() {
        let meta = parse("/corpus/연혁법령/20200526/민법.md");
        assert_eq!(meta.category, SourceCategory::Historical);
        assert_eq!(meta.publish_date.as_deref(), Some("20200526"));
    }

    #[test]
    fn unknown_when_no_marker() {
        let meta = parse("/tmp/random/20250131/민법.md");
        assert_eq!(meta.category, SourceCategory::Unknown);
        assert_eq!(meta.publish_date.as_deref(), Some("20250131"));
    }

    #[test]
    fn accepts_legacy_promulgation_dates() {
        // The earliest 연혁법령 entries date from the late 1940s — no
        // arbitrary floor on year so the deep archive is ingestible.
        let meta =
            parse(r"D:\국가법령정보api(법령과 판례)\연혁법령\19490506\공보처직제_10947.md");
        assert_eq!(meta.category, SourceCategory::Historical);
        assert_eq!(meta.publish_date.as_deref(), Some("19490506"));
    }

    #[test]
    fn rejects_invalid_month_or_day_in_folder_name() {
        // `20251345` has month=13; must NOT match.
        let meta = parse("/연혁법령/20251345/민법.md");
        assert!(meta.publish_date.is_none());
    }

    #[test]
    fn date_absent_when_no_date_folder() {
        let meta = parse("/현행법령/민법.md");
        assert_eq!(meta.category, SourceCategory::Current);
        assert!(meta.publish_date.is_none());
    }
}
