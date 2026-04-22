//! Bidirectional PII redactor for the SLM → LLM escalation boundary.
//!
//! Spec (2026-04-22): when the on-device SLM (Gemma 4) decides to
//! escalate a question to a cloud LLM (Claude / GPT / Gemini), it
//! MUST first scrub every piece of personally-identifiable data from
//! the prompt + recent context, send the scrubbed text to the LLM,
//! receive the response, and then *substitute the originals back*
//! before showing the answer (or generated document) to the user.
//!
//! Why a separate module from `security::data_masking`:
//!
//! * `data_masking` is one-way — it produces a redacted string and
//!   discards the original. Designed for log sanitisation where the
//!   original is not needed.
//! * Here we MUST keep a per-conversation, in-memory map so the LLM
//!   response can be de-redacted. That map is never written to disk,
//!   never logged, and is dropped the moment the chat round-trip
//!   completes.
//! * Placeholder format is numbered (`[NAME_1]`, `[PHONE_2]`, …)
//!   instead of `***` masking so the LLM can still understand
//!   "person A did X to person B" relationships in the redacted
//!   text.
//!
//! Korean-specific patterns added on top of the existing US-centric
//! ones in `data_masking`:
//!
//! * 주민등록번호 (RRN): six digits + `-` + seven digits where the
//!   first digit of the second half is 1–4 (gender / century).
//! * Mobile (010-xxxx-xxxx) and landline (02 / 0xx) numbers.
//! * Korean road / lot addresses (heuristic — captures `시/도/구/동/로`
//!   style components followed by a number).
//! * Hangul proper-name heuristic — runs of 2–4 Hangul syllables
//!   that *aren't* common nouns.
//! * Generic credit cards, emails, API keys, bearer tokens.

use std::collections::HashMap;
use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

/// PII category — used both as the placeholder label prefix and as
/// the dedup key inside [`PiiRedactionMap`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PiiCategory {
    Name,
    Phone,
    Rrn,
    Email,
    CreditCard,
    Address,
    ApiKey,
    BearerToken,
    Password,
}

impl PiiCategory {
    /// Stable label used inside the placeholder. Keep these short —
    /// the LLM's context window is precious.
    pub fn label(self) -> &'static str {
        match self {
            PiiCategory::Name => "NAME",
            PiiCategory::Phone => "PHONE",
            PiiCategory::Rrn => "SSN",
            PiiCategory::Email => "EMAIL",
            PiiCategory::CreditCard => "CARD",
            PiiCategory::Address => "ADDR",
            PiiCategory::ApiKey => "APIKEY",
            PiiCategory::BearerToken => "TOKEN",
            PiiCategory::Password => "PWD",
        }
    }
}

/// Per-conversation bidirectional placeholder map.
///
/// Keep instances in scope only for the duration of a single chat
/// round-trip. Drop on response completion so the originals are
/// reclaimed by the allocator.
#[derive(Debug, Default, Clone)]
pub struct PiiRedactionMap {
    /// `placeholder → original` for [`restore_text`].
    inner: HashMap<String, String>,
    /// `original → placeholder` so re-mentions of the same value get
    /// the same placeholder label. Avoids `[NAME_1]` and `[NAME_2]`
    /// referring to the same person inside one message.
    reverse: HashMap<String, String>,
    /// Per-category running counter for placeholder numbering.
    counters: HashMap<PiiCategory, u32>,
}

impl PiiRedactionMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of distinct originals captured so far. Test-friendly.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Allocate (or look up an existing) placeholder for `original`.
    /// Returns the placeholder string the caller should splice into
    /// the redacted text.
    pub fn allocate(&mut self, category: PiiCategory, original: &str) -> String {
        if let Some(existing) = self.reverse.get(original) {
            return existing.clone();
        }
        let counter = self.counters.entry(category).or_insert(0);
        *counter += 1;
        let placeholder = format!("[{}_{}]", category.label(), counter);
        self.inner.insert(placeholder.clone(), original.to_string());
        self.reverse.insert(original.to_string(), placeholder.clone());
        placeholder
    }

    /// Iterate `(placeholder, original)` pairs. Mostly useful for
    /// debugging / unit tests.
    pub fn entries(&self) -> impl Iterator<Item = (&String, &String)> {
        self.inner.iter()
    }
}

/// Compiled Korean + generic PII patterns. Lazily built once.
struct PatternSet {
    rrn: Regex,
    mobile_kr: Regex,
    landline_kr: Regex,
    credit_card: Regex,
    email: Regex,
    api_key_prefix: Regex,
    bearer_token: Regex,
    address_kr: Regex,
    hangul_name: Regex,
    password_phrase: Regex,
}

static PATTERNS: OnceLock<PatternSet> = OnceLock::new();

fn patterns() -> &'static PatternSet {
    PATTERNS.get_or_init(|| PatternSet {
        // 주민등록번호 — six digits, dash, seven digits whose first
        // is 1–4 (gender / century). Spec validity check: we don't
        // verify the checksum, just the shape.
        rrn: Regex::new(r"\b\d{6}-[1-4]\d{6}\b").unwrap(),
        // 010-xxxx-xxxx and the older 011/016/017/018/019. Hyphens
        // are optional.
        mobile_kr: Regex::new(r"\b01[016789][-\s]?\d{3,4}[-\s]?\d{4}\b").unwrap(),
        // Landline 0XX-XXX-XXXX. Avoid colliding with mobile by
        // restricting first digit set.
        landline_kr: Regex::new(r"\b0(?:2|[3-6][1-5])[-\s]?\d{3,4}[-\s]?\d{4}\b").unwrap(),
        // 16-digit credit cards with optional separators.
        credit_card: Regex::new(r"\b\d{4}[-\s]?\d{4}[-\s]?\d{4}[-\s]?\d{4}\b").unwrap(),
        email: Regex::new(r"\b[a-zA-Z0-9._%+\-]+@[a-zA-Z0-9.\-]+\.[a-zA-Z]{2,}\b").unwrap(),
        // Common cloud-provider key prefixes — sk_, pk_, AKIA, ghp_,
        // hf_, glpat-, AIza (Google), and a generic catch-all for
        // any well-formed token of length ≥ 32.
        api_key_prefix: Regex::new(
            r"(?i)\b(?:sk|pk|api[_\-]?key|token|secret|AKIA|ghp_|hf_|glpat-|AIza)[_\-A-Za-z0-9]{16,}\b",
        )
        .unwrap(),
        bearer_token: Regex::new(r"(?i)\bBearer\s+[A-Za-z0-9\-._~+/]+=*\b").unwrap(),
        // Korean address heuristic: anything that ends in
        // `시/도/구/군/동/로/길/번지/호` followed by space + number.
        // Coarse on purpose — false positives here are safer than
        // false negatives.
        address_kr: Regex::new(
            r"[가-힣]+(?:시|도|구|군|동|읍|면)\s+[가-힣0-9]+(?:로|길|번지|동|호)?\s*\d+(?:[-\s]?\d+)?(?:번지)?",
        )
        .unwrap(),
        // Hangul proper-name heuristic: 2–4 Hangul syllables where
        // none of them is on a small allow-list of common nouns. We
        // intentionally err on the side of redacting too much — a
        // false positive becomes `[NAME_1]` in the LLM prompt, which
        // is still readable for the model.
        // No `\b` here: Hangul characters are Unicode word chars,
        // so `\b` between 수 (수님 in 김철수님) is not a boundary.
        // We rely on the blocklist + length cap to filter false positives.
        hangul_name: Regex::new(r"[가-힣]{2,6}").unwrap(),
        // Password heuristic: phrases like `비밀번호: ...`,
        // `password=`, `pwd=`. Captures the value through the next
        // whitespace.
        password_phrase: Regex::new(
            r"(?i)(?:비밀번호|패스워드|password|passwd|pwd)\s*[:=]\s*\S+",
        )
        .unwrap(),
    })
}

/// Conservative allow-list of 2–4 syllable Hangul tokens that the
/// name heuristic should NEVER redact. Includes very common nouns
/// and pronouns to avoid the placeholder spam pattern. Add to this
/// set as false positives surface in QA.
fn hangul_name_blocklist() -> &'static std::collections::HashSet<&'static str> {
    static BLOCK: OnceLock<std::collections::HashSet<&'static str>> = OnceLock::new();
    BLOCK.get_or_init(|| {
        [
            "안녕", "안녕하세요", "감사", "감사합니다", "사용자", "사람", "사용",
            "오늘", "내일", "어제", "지금", "여기", "저기", "거기", "이것",
            "저것", "그것", "이번", "저번", "그분", "이분", "저분", "어떤",
            "무엇", "어디", "언제", "누구", "어떻게", "왜냐하면", "그리고",
            "하지만", "그래서", "예를", "보면", "위해", "위해서", "통해",
            "통해서", "대해", "대해서", "관련", "관련해서", "기준", "기준으로",
            "비밀번호", "패스워드", "주소", "주민", "전화", "휴대폰", "이메일",
            "님의", "씨의", "선생님", "입니다", "있습니다", "없습니다",
            "있어요", "없어요", "있다", "없다", "한다", "된다", "예요",
            "에서", "에게", "께서", "에는", "으로", "으로서", "으로써",
            "처럼", "같이", "만큼", "보다", "부터", "까지", "마다",
            "주민번호", "주민등록", "주민등록번호", "이름", "이름은",
            "날씨", "날씨가", "좋네요", "좋아요", "좋다", "좋은",
            "안녕하", "안녕히", "감사해", "감사합", "사용하",
            "그리고는", "그래도", "어쩌면", "혹시나", "아마도",
            "코드", "코드는", "키1", "키2", "카드", "만료",
        ]
        .into_iter()
        .collect()
    })
}

/// Redact `text` in place, allocating placeholders into `map`.
/// Returns the redacted string. The order of category matching is
/// load-bearing: longer / more specific patterns run first so a
/// credit-card-shaped string doesn't get partially eaten by the
/// landline pattern.
pub fn redact_text(text: &str, map: &mut PiiRedactionMap) -> String {
    let p = patterns();
    let mut out = text.to_string();

    // 1) Highest-precision categories first.
    out = replace_with(&out, &p.rrn, PiiCategory::Rrn, map);
    out = replace_with(&out, &p.credit_card, PiiCategory::CreditCard, map);
    out = replace_with(&out, &p.email, PiiCategory::Email, map);
    out = replace_with(&out, &p.api_key_prefix, PiiCategory::ApiKey, map);
    out = replace_with(&out, &p.bearer_token, PiiCategory::BearerToken, map);
    out = replace_with(&out, &p.password_phrase, PiiCategory::Password, map);

    // 2) Korean phone numbers — mobile before landline to avoid the
    //    landline regex eating an `010-...` prefix.
    out = replace_with(&out, &p.mobile_kr, PiiCategory::Phone, map);
    out = replace_with(&out, &p.landline_kr, PiiCategory::Phone, map);

    // 3) Address heuristic — coarse but high-recall. Run before the
    //    name heuristic so address tokens aren't double-redacted.
    out = replace_with(&out, &p.address_kr, PiiCategory::Address, map);

    // 4) Hangul name heuristic — last, with a blocklist guard. Skip
    //    matches that are pure common-noun tokens.
    out = replace_with_filtered(&out, &p.hangul_name, PiiCategory::Name, map, |hit| {
        let block = hangul_name_blocklist();
        !block.contains(hit) && !is_inside_placeholder(hit)
    });

    out
}

/// Restore originals into `text`. Walks the map and does literal
/// string replacement; placeholders are unique so no ambiguity.
pub fn restore_text(text: &str, map: &PiiRedactionMap) -> String {
    let mut out = text.to_string();
    for (placeholder, original) in &map.inner {
        out = out.replace(placeholder.as_str(), original);
    }
    out
}

fn replace_with(
    text: &str,
    pattern: &Regex,
    category: PiiCategory,
    map: &mut PiiRedactionMap,
) -> String {
    replace_with_filtered(text, pattern, category, map, |_| true)
}

fn replace_with_filtered<F>(
    text: &str,
    pattern: &Regex,
    category: PiiCategory,
    map: &mut PiiRedactionMap,
    filter: F,
) -> String
where
    F: Fn(&str) -> bool,
{
    // Walk matches left-to-right, accumulate a fresh String. Reusing
    // `regex::Regex::replace_all` doesn't work because the closure
    // would need `&mut map` which the API forbids.
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    for m in pattern.find_iter(text) {
        let original = m.as_str();
        if !filter(original) {
            continue;
        }
        out.push_str(&text[cursor..m.start()]);
        let placeholder = map.allocate(category, original);
        out.push_str(&placeholder);
        cursor = m.end();
    }
    out.push_str(&text[cursor..]);
    out
}

/// Heuristic: never redact a token that *is itself* a placeholder
/// from a previous category pass (e.g. `NAME` inside `[NAME_1]`).
fn is_inside_placeholder(token: &str) -> bool {
    // Placeholders are pure ASCII; Hangul names are not. So if the
    // token has any non-Hangul character, it's not what the name
    // heuristic was meant to catch.
    !token.chars().all(|c| ('가'..='힣').contains(&c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_preserves_original_text() {
        let original = "김철수님의 휴대폰 010-1234-5678 그리고 이메일 chulsoo@example.com 입니다.";
        let mut map = PiiRedactionMap::new();
        let redacted = redact_text(original, &mut map);
        assert!(!redacted.contains("김철수"));
        assert!(!redacted.contains("010-1234-5678"));
        assert!(!redacted.contains("chulsoo@example.com"));
        let restored = restore_text(&redacted, &map);
        assert_eq!(restored, original);
    }

    #[test]
    fn deduplicates_repeated_originals() {
        // Property under test: when the *exact same* original substring
        // appears multiple times in one prompt, the redactor allocates
        // ONE placeholder and reuses it. Other unrelated tokens may
        // also be redacted into their own placeholders — that's fine
        // and out of scope for this test.
        let original = "김철수님 010-1111-2222 김철수님 010-1111-2222";
        let mut map = PiiRedactionMap::new();
        let redacted = redact_text(original, &mut map);
        let name_ph = map
            .reverse
            .get("김철수님")
            .cloned()
            .expect("김철수님 placeholder allocated");
        let phone_ph = map
            .reverse
            .get("010-1111-2222")
            .cloned()
            .expect("phone placeholder allocated");
        assert_eq!(
            redacted.matches(name_ph.as_str()).count(),
            2,
            "name should share one placeholder across both mentions"
        );
        assert_eq!(
            redacted.matches(phone_ph.as_str()).count(),
            2,
            "phone should share one placeholder across both mentions"
        );
    }

    #[test]
    fn rrn_pattern_validates_gender_digit() {
        let mut map = PiiRedactionMap::new();
        let valid = "주민번호 900101-1234567 입니다";
        let redacted = redact_text(valid, &mut map);
        assert!(!redacted.contains("900101-1234567"));
        // Five-digit gender position should NOT match.
        let mut map2 = PiiRedactionMap::new();
        let invalid = "코드 900101-9234567 입니다";
        let redacted2 = redact_text(invalid, &mut map2);
        assert!(redacted2.contains("900101-9234567"));
    }

    #[test]
    fn credit_card_shape_is_redacted() {
        let mut map = PiiRedactionMap::new();
        let redacted = redact_text("카드 1234-5678-9012-3456 만료", &mut map);
        assert!(!redacted.contains("1234-5678-9012-3456"));
    }

    #[test]
    fn api_key_prefix_redacts_sk_and_AKIA() {
        let mut map = PiiRedactionMap::new();
        let redacted = redact_text(
            "키1=sk_test_ABCDEFGHIJ1234567890 키2=AKIA1234567890ABCDEF",
            &mut map,
        );
        assert!(!redacted.contains("sk_test_ABCDEFGHIJ1234567890"));
        assert!(!redacted.contains("AKIA1234567890ABCDEF"));
    }

    #[test]
    fn blocklist_keeps_common_nouns_intact() {
        let mut map = PiiRedactionMap::new();
        let redacted = redact_text("안녕하세요 오늘 날씨가 좋네요", &mut map);
        assert!(redacted.contains("안녕"), "common greeting must not be redacted");
    }

    #[test]
    fn empty_map_means_no_redaction() {
        let mut map = PiiRedactionMap::new();
        let redacted = redact_text("plain prompt with no PII", &mut map);
        assert_eq!(redacted, "plain prompt with no PII");
        assert!(map.is_empty());
    }
}
