//! Canonical Korean law-name normalization + common short-form aliases.
//!
//! Purpose
//! ───────
//! Korean legal writing uses many short forms for the same law:
//!     민법 ≡ 민  ≡ 民法
//!     형법 ≡ 형  ≡ 刑法
//!     근로기준법 ≡ 근기법
//!     근로자퇴직급여 보장법 ≡ 근퇴법
//!     민사소송법 ≡ 민소법 ≡ 민소
//!     형사소송법 ≡ 형소법 ≡ 형소
//!
//! All of these must resolve to the **same statute slug**, otherwise the
//! same law ends up as multiple disconnected subgraphs and citations
//! silently fail to wire up. We solve this with a conservative, hand-
//! curated mapping of **official name ↔ short form(s)**.
//!
//! Contract
//! ────────
//! - [`canonical_name`] — normalize any recognized form to the official
//!   long name (the one that goes into the slug and
//!   `vault_frontmatter.law_name`).
//! - [`short_forms`] — every known short form for a given official name;
//!   used by `slug::statute_aliases` to generate
//!   `vault_aliases` rows so `legal_graph_find` can match "근기법 43조의2".
//!
//! **Design constraint**: this table is deliberately small and
//! high-confidence. Only laws whose short forms are unambiguous among
//! Korean practitioners are listed. An unknown law name passes through
//! unchanged (we never invent a slug from a name we don't recognize).
//! Adding a new entry requires only a single line; see the test block
//! for the invariants the table must maintain.

use std::sync::OnceLock;

/// Canonical → list of widely-used short forms.
///
/// Rules:
///   - First entry is always the official long name (e.g. `민법`).
///   - Short forms that are ambiguous prefixes of another law are
///     commented out (e.g. `상` alone collides with 상법 / 상가건물
///     임대차보호법; only `상법` is listed).
///   - Japanese/hanja variants are included when they're commonly seen
///     in scanned historical judgments.
const LAW_ALIAS_TABLE: &[(&str, &[&str])] = &[
    // ── Core civil / criminal / commercial codes ──
    ("민법", &["민", "民法"]),
    ("형법", &["형", "刑法"]),
    ("상법", &["商法"]),
    // ── Procedure codes ──
    ("민사소송법", &["민소법", "민소", "민사소송"]),
    ("형사소송법", &["형소법", "형소", "형사소송"]),
    ("행정소송법", &["행소법", "행소"]),
    ("가사소송법", &["가소법"]),
    ("민사집행법", &["민집법"]),
    // ── Labor / employment ──
    ("근로기준법", &["근기법"]),
    ("근로자퇴직급여 보장법", &["근로자퇴직급여보장법", "근퇴법"]),
    ("남녀고용평등과 일ㆍ가정 양립 지원에 관한 법률", &["남녀고용평등법"]),
    ("노동조합 및 노동관계조정법", &["노조법", "노동조합법"]),
    ("산업안전보건법", &["산안법"]),
    ("산업재해보상보험법", &["산재법", "산재보상법"]),
    // ── Tax ──
    ("국세기본법", &["국기법"]),
    ("소득세법", &["소법"]),
    ("법인세법", &["법법"]),
    ("부가가치세법", &["부가법"]),
    ("상속세 및 증여세법", &["상증법"]),
    // ── Constitutional / administrative ──
    ("헌법", &["憲法"]),
    ("헌법재판소법", &["헌재법"]),
    ("행정기본법", &[]),
    ("행정절차법", &["행절법"]),
    // ── Family / minors / consumer ──
    ("가정폭력범죄의 처벌 등에 관한 특례법", &["가폭법"]),
    ("성폭력범죄의 처벌 등에 관한 특례법", &["성폭법"]),
    ("아동복지법", &[]),
    ("아동학대범죄의 처벌 등에 관한 특례법", &["아학법"]),
    ("소비자기본법", &[]),
    ("약관의 규제에 관한 법률", &["약관법"]),
    // ── Real estate / housing ──
    ("주택임대차보호법", &["주임법"]),
    ("상가건물 임대차보호법", &["상임법"]),
    ("부동산등기법", &["부등법"]),
    // ── IP / competition ──
    ("저작권법", &[]),
    ("특허법", &[]),
    ("상표법", &[]),
    ("디자인보호법", &[]),
    ("독점규제 및 공정거래에 관한 법률", &["공정거래법"]),
    ("부정경쟁방지 및 영업비밀보호에 관한 법률", &["부경법"]),
    // ── Banking / credit ──
    ("은행법", &[]),
    ("신용정보의 이용 및 보호에 관한 법률", &["신정법"]),
    // ── Judicial officers / courts ──
    ("법원조직법", &[]),
    ("검찰청법", &[]),
    ("변호사법", &[]),
    ("노동위원회법", &[]),
    // ── Misc. procedural ──
    ("동산ㆍ채권 등의 담보에 관한 법률", &["동담법"]),
    ("전자문서 및 전자거래 기본법", &["전자문서법"]),
];

/// Normalize any recognized form (official or short) to the **canonical
/// long name**. Unknown names pass through unchanged.
///
/// Whitespace is trimmed; internal spacing is preserved so forms like
/// `근로자퇴직급여 보장법` (with space) and `근로자퇴직급여보장법`
/// (without) both map to the same canonical.
pub fn canonical_name(input: &str) -> String {
    let key = input.trim();
    if key.is_empty() {
        return String::new();
    }

    let idx = alias_index();
    if let Some(canon) = idx.get_canonical(key) {
        return canon.to_string();
    }
    // Also try a whitespace-normalised lookup so
    // `근로자퇴직급여  보장법` → `근로자퇴직급여 보장법`.
    let squashed: String = key.split_whitespace().collect::<Vec<_>>().join(" ");
    if squashed != key {
        if let Some(canon) = idx.get_canonical(&squashed) {
            return canon.to_string();
        }
    }
    // No alias table hit — return the trimmed (but space-preserved) input
    // so downstream code sees a stable form.
    key.to_string()
}

/// Every known short form for `canonical`, empty if the name is unknown
/// or has no registered short forms. Never includes `canonical` itself.
pub fn short_forms(canonical: &str) -> &'static [&'static str] {
    let idx = alias_index();
    idx.shorts.get(canonical).copied().unwrap_or(&[])
}

/// Returns true if `name` is a recognized form (canonical OR short) of
/// SOME law in the table.
pub fn is_known(name: &str) -> bool {
    let idx = alias_index();
    let key = name.trim();
    idx.get_canonical(key).is_some()
        || idx
            .get_canonical(&key.split_whitespace().collect::<Vec<_>>().join(" "))
            .is_some()
}

// ───────── Internals ─────────

struct AliasIndex {
    // name → canonical (includes canonical→canonical self-mappings)
    canon: std::collections::HashMap<&'static str, &'static str>,
    // canonical → list of short forms
    shorts: std::collections::HashMap<&'static str, &'static [&'static str]>,
}

impl AliasIndex {
    fn get_canonical(&self, key: &str) -> Option<&'static str> {
        self.canon.get(key).copied()
    }
}

fn alias_index() -> &'static AliasIndex {
    static INDEX: OnceLock<AliasIndex> = OnceLock::new();
    INDEX.get_or_init(|| {
        let mut canon = std::collections::HashMap::new();
        let mut shorts = std::collections::HashMap::new();
        for (canonical, short_list) in LAW_ALIAS_TABLE {
            canon.insert(*canonical, *canonical);
            for s in *short_list {
                canon.insert(*s, *canonical);
            }
            shorts.insert(*canonical, *short_list);
        }
        AliasIndex { canon, shorts }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_roundtrip_for_official_names() {
        for (canonical, _) in LAW_ALIAS_TABLE {
            assert_eq!(canonical_name(canonical), *canonical);
        }
    }

    #[test]
    fn short_forms_map_to_canonical() {
        assert_eq!(canonical_name("근기법"), "근로기준법");
        assert_eq!(canonical_name("근퇴법"), "근로자퇴직급여 보장법");
        assert_eq!(canonical_name("민소법"), "민사소송법");
        assert_eq!(canonical_name("형소"), "형사소송법");
        assert_eq!(canonical_name("공정거래법"), "독점규제 및 공정거래에 관한 법률");
        assert_eq!(canonical_name("주임법"), "주택임대차보호법");
        assert_eq!(canonical_name("약관법"), "약관의 규제에 관한 법률");
    }

    #[test]
    fn canonical_unknown_passes_through() {
        assert_eq!(canonical_name("존재하지않는법"), "존재하지않는법");
        assert_eq!(canonical_name(""), "");
        assert_eq!(canonical_name("   "), "");
    }

    #[test]
    fn hanja_and_whitespace_handled() {
        assert_eq!(canonical_name("民法"), "민법");
        assert_eq!(canonical_name("刑法"), "형법");
        // Whitespace-collapsed forms of the spaced canonicals:
        assert_eq!(
            canonical_name("근로자퇴직급여보장법"),
            "근로자퇴직급여 보장법"
        );
    }

    #[test]
    fn is_known_distinguishes_table_entries() {
        assert!(is_known("민법"));
        assert!(is_known("근기법"));
        assert!(is_known("刑法"));
        assert!(!is_known("지어낸법"));
    }

    #[test]
    fn short_forms_are_complete() {
        let kr = short_forms("근로기준법");
        assert!(kr.contains(&"근기법"));
        let none_known = short_forms("지어낸법");
        assert!(none_known.is_empty());
    }

    #[test]
    fn table_has_no_duplicate_short_forms_across_laws() {
        // A short form must never map to two different canonicals —
        // that would make citations ambiguous. This is the single most
        // important invariant of this table.
        let mut seen: std::collections::HashMap<&'static str, &'static str> =
            std::collections::HashMap::new();
        for (canonical, shorts) in LAW_ALIAS_TABLE {
            for s in *shorts {
                if let Some(prev) = seen.get(*s) {
                    panic!(
                        "short form `{s}` maps to both `{prev}` and `{canonical}` — ambiguous"
                    );
                }
                seen.insert(*s, *canonical);
            }
        }
    }

    #[test]
    fn canonicals_are_unique() {
        // Same canonical name must not appear twice in the table.
        let mut seen: std::collections::HashSet<&'static str> =
            std::collections::HashSet::new();
        for (canonical, _) in LAW_ALIAS_TABLE {
            assert!(
                seen.insert(*canonical),
                "duplicate canonical: {canonical}"
            );
        }
    }
}
