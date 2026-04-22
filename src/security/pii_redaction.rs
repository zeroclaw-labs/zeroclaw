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
    hangul_name_surname: Regex,
    hangul_name_cue: Regex,
    company_form_prefix: Regex,
    company_name_cue: Regex,
    password_phrase: Regex,
}

static PATTERNS: OnceLock<PatternSet> = OnceLock::new();

/// Common everyday Hangul words that happen to begin with a Korean
/// surname character and would therefore false-positive against the
/// surname-driven name pattern (e.g. "안녕" begins with the surname
/// "안"; "정말" with "정"; "우리" with "우"). The list is
/// deliberately short — just the ~50 most common tokens that slip
/// through the surname anchor — not a dictionary. Add to it as QA
/// surfaces new false positives.
fn name_surname_false_positive_blocklist() -> &'static std::collections::HashSet<&'static str> {
    use std::collections::HashSet;
    use std::sync::OnceLock;
    static BLOCK: OnceLock<HashSet<&'static str>> = OnceLock::new();
    BLOCK.get_or_init(|| {
        [
            // `안` (surname) — greetings / negation / location
            "안녕", "안녕하", "안녕하세요", "안녕히", "안에", "안쪽",
            // `오` (surname) — today / long / come
            "오늘", "오래", "오랜", "오랫", "오전", "오후",
            // `우` (surname) — we / umbrella / milk
            "우리", "우산", "우유", "우선", "우회",
            // `하` (surname) — one day / lower / doing
            "하루", "하지", "하면", "하나", "하늘", "하지만",
            // `정` (surname) — really / exactly / regular
            "정말", "정확", "정기", "정도", "정리",
            // `모` (surname) — all / every / moment
            "모두", "모든", "모여", "모아", "모음",
            // `소` (surname) — precious / news / tool
            "소중", "소식", "소개", "소유", "소리",
            // `기` (surname) — basic / chance / record
            "기본", "기회", "기록", "기능", "기간",
            // `가` (surname) — able / home / value
            "가능", "가정", "가치", "가격", "가장",
            // `대` (surname) — answer / representative / most
            "대답", "대체", "대부", "대신", "대해",
            // `문` (surname) — question / text / door
            "문제", "문서", "문장", "문화", "문의",
            // `시` (surname) — city / time / attempt
            "시간", "시작", "시점", "시도", "시장",
            // `선` (surname) — pre / line / choice
            "선택", "선물", "선생", "선배", "선수",
            // `고` (surname) — high / hello / consideration
            "고려", "고객", "고민", "고맙",
            // `공` (surname) — public / study / empty
            "공부", "공간", "공유", "공지",
            // `주` (surname) — main / week / alcohol
            "주요", "주말", "주위", "주변", "주제",
            // `천` (surname) — thousand / nature
            "천천히", "천재", "천하",
            // `진` (surname) — truth / progress
            "진짜", "진행", "진심",
            // `강` (surname) — strong / river / lecture
            "강의", "강력", "강조",
            // `성` (surname) — name / achievement / castle
            "성공", "성장", "성과",
            // miscellaneous surname-start particles or function words
            "김치", "박수", "최고", "최근", "최대", "최소",
            "장소", "장면", "경우", "경험", "계속", "계획",
        ]
        .into_iter()
        .collect()
    })
}

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
        // Korean proper-name pattern (spec, 2026-04-23) — surname-driven.
        //
        // Rather than matching every 2–6 syllable Hangul run and filtering
        // false positives with a blocklist, we match ONLY strings that
        // begin with a recognised surname. Coverage rationale:
        //
        //   * ~95–99% of Korean surnames are one syllable → the big
        //     alternation below enumerates the common single-character
        //     surnames from KIPO / Statistics Korea census lists.
        //   * ~0.1% of Koreans carry a two-syllable "복성" (compound
        //     surname). We enumerate them explicitly — there are only
        //     ~10 common ones (남궁 / 황보 / 제갈 / 선우 / 사공 / 서문 /
        //     독고 / 동방 / 어금 / 무본) plus a handful of naturalised
        //     variants (망절 / 소봉 / 황목 / 강전 / 사마 / 태사). Since
        //     the absolute list is small, hard-coding is safe.
        //   * The given name is almost always 2 syllables (~90–95%);
        //     1 or 3 syllable given names are the remainder. 4-syllable
        //     given names do not exist in Korean naming conventions.
        //
        // Pattern: `<surname><given-name>` where:
        //   * Single-syllable surname immediately followed by 1–3 Hangul
        //     syllables (the given name).
        //   * OR two-syllable compound surname immediately followed by
        //     1–3 Hangul syllables.
        //
        // Total length constraints: 2..=5 Hangul syllables. Matches
        // "김철수" (3), "박지민" (3), "이해" (2), "남궁민수" (4),
        // "선우정아" (4), "제갈공명" (4).
        //
        // Optional trailing honorific ("님" / "씨" / "선생" / "선생님" /
        // "군" / "양" / "대표" / "대리" / "과장" / "차장" / "부장" /
        // "사장" / "회장") is captured too so the redacted placeholder
        // covers the full noun phrase.
        hangul_name_surname: Regex::new(
            r"(?x)
            (?:
                # Two-syllable compound surnames first (longer match wins).
                (?:남궁|황보|제갈|선우|사공|서문|독고|동방|어금|무본|
                   망절|소봉|황목|강전|사마|태사)
                [가-힣]{1,3}
              |
                # Single-syllable surnames.
                (?:가|간|갈|감|강|개|견|경|계|고|곡|공|곽|관|교|구|국|군|궁|궉|
                   권|근|금|기|길|김|
                   나|난|남|낭|내|노|뇌|누|
                   다|단|담|당|대|도|독|돈|동|두|등|
                   라|란|랑|려|로|뢰|류|리|림|
                   마|만|매|맹|명|모|목|묘|무|묵|문|미|민|
                   박|반|방|배|백|번|범|변|보|복|봉|부|비|빈|빙|
                   사|산|삼|상|서|석|선|설|섭|성|소|손|송|수|순|승|시|신|심|십|
                   아|안|애|야|양|어|엄|여|연|염|엽|영|예|오|옥|온|옹|완|왕|요|
                   용|우|운|원|위|유|육|윤|은|음|이|인|임|
                   자|잠|장|저|전|점|정|제|조|종|좌|주|준|즙|증|지|진|
                   차|창|채|천|초|총|최|추|
                   쾌|
                   탁|탄|탕|태|
                   판|팽|편|평|포|표|풍|피|필|
                   하|학|한|함|해|허|혁|현|형|호|홍|화|환|황|후|흥)
                [가-힣]{1,3}
            )
            (?:님|씨|선생님|선생|군|양|대표|대리|과장|차장|부장|사장|회장)
            ",
        )
        .unwrap(),
        // Cue-word pattern (spec, 2026-04-23): when the document
        // literally writes `이름: 홍길동` / `성명 김철수`, the value
        // after the cue is almost certainly a personal name — even
        // if the surname heuristic above would miss it (foreign name,
        // rare surname, etc.). Captures the following Hangul or
        // Latin token so the redactor picks it up unconditionally.
        hangul_name_cue: Regex::new(
            r"(?x)
            (?:이름|성명)
            \s*[:：]?\s*
            (?:[가-힣]{2,5}|[A-Za-z][A-Za-z\s]{1,40}[A-Za-z])
            ",
        )
        .unwrap(),
        // Korean company-form prefix (spec, 2026-04-23): legal forms
        // that unambiguously mark the following token as a registered
        // company name. Branded trade names without any of these
        // prefixes are intentionally out-of-scope — the false-positive
        // rate is too high to match arbitrary 상호/브랜드.
        //
        //   주식회사 OOO / (주)OOO
        //   유한회사 OOO / (유한)OOO / (유)OOO
        //   합명회사 OOO / 합자회사 OOO
        //
        // Also matches the *suffix* form ("OOO 주식회사") which is how
        // Korean filings usually render the legal form.
        company_form_prefix: Regex::new(
            r"(?x)
            (?:
                # Prefix form — the legal form comes first, company name follows.
                (?:주식회사|\(주\)|유한회사|\(유한\)|\(유\)|
                   합명회사|합자회사|유한책임회사|\(유책\))
                \s*[가-힣A-Za-z0-9&\-\.]{1,40}
              |
                # Suffix form — company name first, legal form at the end
                # (e.g. 모아 주식회사).
                [가-힣A-Za-z0-9&\-\.]{1,40}\s*
                (?:주식회사|\(주\)|유한회사|\(유한\)|\(유\)|
                   합명회사|합자회사|유한책임회사|\(유책\))
            )
            ",
        )
        .unwrap(),
        // Company-name cue pattern (spec, 2026-04-23): when the
        // document literally writes `회사명: OOO` / `법인명 OOO` /
        // `상호: OOO` / `사업자등록번호 ...의 OOO`, the value after
        // the cue is almost certainly a company name — even if no
        // 주식회사/(주) suffix is present.
        company_name_cue: Regex::new(
            r"(?x)
            (?:회사명|사명|법인명|법인|회사|상호|법인등록번호|사업자등록번호)
            \s*[:：]?\s*
            [가-힣A-Za-z0-9&\-\.]{1,40}
            ",
        )
        .unwrap(),
        // Password heuristic: phrases like `비밀번호: ...`,
        // `password=`, `pwd=`. Captures the value through the next
        // whitespace.
        password_phrase: Regex::new(
            r"(?i)(?:비밀번호|패스워드|password|passwd|pwd)\s*[:=]\s*\S+",
        )
        .unwrap(),
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

    // 4) Company names — cue-driven pass first (captures names with no
    //    legal-form suffix when the document self-labels them via
    //    "회사명:" / "법인명:" / "상호:"), then the legal-form
    //    pass for "OOO 주식회사" / "(주) OOO" style references.
    out = replace_with(&out, &p.company_name_cue, PiiCategory::Name, map);
    out = replace_with(&out, &p.company_form_prefix, PiiCategory::Name, map);

    // 5) Korean personal names — cue-driven pass first (이름: 홍길동
    //    / 성명 김철수), then the surname-driven pattern for
    //    unlabelled mentions like 김철수님의 휴대폰은...
    out = replace_with(&out, &p.hangul_name_cue, PiiCategory::Name, map);
    // The surname anchor is strong but not infallible: common
    // everyday words happen to start with a surname character
    // (안녕, 오늘, 정말, 하루, ...). Filter those through a tight
    // targeted blocklist rather than a world dictionary.
    out = replace_with_filtered(&out, &p.hangul_name_surname, PiiCategory::Name, map, |hit| {
        let block = name_surname_false_positive_blocklist();
        !block.contains(hit)
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
    #[test]
    fn name_with_honorific_is_redacted() {
        // Single-syllable surname (김) + 2-char given name (철수) +
        // honorific (님) → should be redacted even without a cue word.
        let mut map = PiiRedactionMap::new();
        let redacted = redact_text("김철수님 안녕하세요", &mut map);
        assert!(!redacted.contains("김철수"));
        assert!(redacted.contains("안녕"), "greeting must survive the new surname pattern");
    }

    #[test]
    fn compound_surname_with_honorific_is_redacted() {
        // Two-syllable surname (남궁) + 2-char given name (민수) +
        // honorific (씨).
        let mut map = PiiRedactionMap::new();
        let redacted = redact_text("남궁민수씨께 전화드렸어요", &mut map);
        assert!(!redacted.contains("남궁민수"));
    }

    #[test]
    fn bare_name_without_honorific_is_not_redacted_without_cue() {
        // Honorific + cue both missing — we deliberately miss this case
        // to keep the false-positive rate near zero. If you need this
        // covered, use the cue form (이름: 김철수) instead.
        let mut map = PiiRedactionMap::new();
        let redacted = redact_text("김철수의 휴대폰 번호", &mut map);
        assert!(redacted.contains("김철수"));
    }

    #[test]
    fn name_cue_redacts_following_token() {
        // The 이름/성명 cue marks the next Hangul run as a personal
        // name regardless of whether a known surname leads the run.
        let mut map = PiiRedactionMap::new();
        let redacted = redact_text("이름: 홍길동
성명 박지민", &mut map);
        assert!(!redacted.contains("홍길동"));
        assert!(!redacted.contains("박지민"));
    }

    #[test]
    fn company_legal_form_is_redacted() {
        // Legal-form prefix (주식회사) marks the following token as a
        // registered company name.
        let mut map = PiiRedactionMap::new();
        let redacted = redact_text("주식회사 모아에 방문했어요", &mut map);
        assert!(!redacted.contains("주식회사 모아"));
    }

    #[test]
    fn company_cue_redacts_following_token() {
        // Even without a legal-form suffix, a 회사명/법인명/상호 cue
        // marks the following token as a company name.
        let mut map = PiiRedactionMap::new();
        let redacted = redact_text("회사명: 카카오
법인명: 네이버", &mut map);
        assert!(!redacted.contains("카카오"));
        assert!(!redacted.contains("네이버"));
    }

    #[test]
    fn common_words_starting_with_surname_char_survive() {
        // 안(성) / 오(성) / 정(성) / 하(성) / 소(성) 등 성씨 글자로
        // 시작하지만 흔한 일반어인 토큰들은 honorific이 없으므로 살아남는다.
        let mut map = PiiRedactionMap::new();
        let redacted = redact_text(
            "안녕하세요 오늘 정말 하루가 소중한 우리 가족입니다",
            &mut map,
        );
        for word in ["안녕", "오늘", "정말", "하루", "소중", "우리", "가족"] {
            assert!(
                redacted.contains(word),
                "common noun {word} must not be redacted"
            );
        }
    }
}
