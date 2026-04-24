//! Parse a Korean statute markdown file into a [`StatuteDoc`].
//!
//! Input format (from the user's ingestion pipeline):
//!   1. A markdown header section with `- **Key**: value` pairs (MST, 공포일자, etc.)
//!   2. A fenced ```json block containing `meta` + `articles[]` + `supplements[]`.
//!      This is the primary source of truth — it is already structured.
//!   3. Optional HTML/outer-HTML blocks (ignored — we only need the JSON).
//!
//! We extract the JSON block, parse articles structurally, then run regex-based
//! cross-law citation extraction on each article's `text` field. Intra-law
//! references (bare `제36조`) are resolved against the outer law name.

use super::citation_patterns::{extract_statute_citations, StatuteRef};
use super::slug::{self, statute_slug};
use anyhow::{anyhow, Context, Result};
use regex::Regex;
use serde::Deserialize;
use std::sync::OnceLock;

/// Parsed representation of one statute markdown file.
#[derive(Debug, Clone)]
pub struct StatuteDoc {
    pub law_name: String,
    /// Promulgation date (`공포일자`), YYYYMMDD string — kept as string to preserve zeros.
    pub promulgated_at: Option<String>,
    /// Effective date (`시행일`), YYYYMMDD.
    pub effective_at: Option<String>,
    /// Law master number / serial — typically stable per law across revisions.
    pub ls_id: Option<String>,
    /// Revision instance sequence — changes with each promulgation.
    pub ls_seq: Option<String>,
    pub ancestry: Option<String>, // ancNo
    pub source_path: String,
    pub articles: Vec<StatuteArticle>,
    pub supplements: Vec<Supplement>,
}

#[derive(Debug, Clone)]
pub struct StatuteArticle {
    /// Canonical slug, e.g. `statute::근로기준법::43-2`.
    pub slug: String,
    /// Canonical article number, like "43" or "43-2".
    pub article_key: String,
    pub article_num: u32,
    pub article_sub: Option<u32>,
    /// Parenthetical subtitle (`체불사업주 명단 공개`), i.e. the keyword.
    pub title_kw: Option<String>,
    /// `number` field as-is from JSON (e.g. `제43조의2(체불사업주 명단 공개)`).
    pub header: String,
    /// The `text` field — article body.
    pub body: String,
    /// Citations found INSIDE this article's body (edges to other articles).
    pub citations: Vec<StatuteRef>,
}

#[derive(Debug, Clone)]
pub struct Supplement {
    /// Full title as-is — e.g. `부칙  <법률 제21065호, 2025. 10. 1.>`.
    pub title: String,
    pub body: String,
    /// Parsed promulgation number (the `N` in `법률 제N호`). `None` if the
    /// title doesn't follow the standard format (e.g. legacy supplements).
    pub promulgation_no: Option<String>,
    /// Parsed promulgation date as `YYYYMMDD`. `None` if unparseable.
    pub promulgation_date: Option<String>,
    /// The "(다른 법률)" suffix some supplements carry — typically a
    /// reference to the originating amending law (e.g.
    /// `(가족관계의 등록 등에 관한 법률)`). Preserved verbatim.
    pub note: Option<String>,
}

/// Parse a supplement title like
///   `부칙  <법률 제21065호, 2025. 10. 1.>   (정부조직법)`
/// into `(promulgation_no, "YYYYMMDD", note)`. All three fields are best-
/// effort — missing pieces return `None` rather than erroring, because
/// some historical supplements have unusual title formats that we don't
/// want to fail ingestion over.
fn parse_supplement_title(title: &str) -> (Option<String>, Option<String>, Option<String>) {
    static RE: OnceLock<Regex> = OnceLock::new();
    // <법률 제(\d+)호, (\d{4}). (\d+). (\d+).>    with optional trailing  (note)
    let re = RE.get_or_init(|| {
        Regex::new(
            r"<\s*법률\s*제\s*(\d+)\s*호\s*,\s*(\d{4})\.\s*(\d{1,2})\.\s*(\d{1,2})\.\s*>\s*(?:\(\s*([^)]+?)\s*\))?",
        )
        .unwrap()
    });
    let Some(caps) = re.captures(title) else {
        return (None, None, None);
    };
    let num = caps.get(1).map(|m| m.as_str().to_string());
    let date = match (caps.get(2), caps.get(3), caps.get(4)) {
        (Some(y), Some(m), Some(d)) => Some(format!(
            "{:04}{:02}{:02}",
            y.as_str().parse::<u32>().unwrap_or(0),
            m.as_str().parse::<u32>().unwrap_or(0),
            d.as_str().parse::<u32>().unwrap_or(0),
        )),
        _ => None,
    };
    let note = caps.get(5).map(|m| m.as_str().trim().to_string());
    (num, date, note)
}

// ───────── JSON schema (subset we actually use) ─────────

#[derive(Debug, Deserialize)]
struct RawStatute {
    meta: Option<RawMeta>,
    title: Option<String>,
    articles: Option<Vec<RawArticle>>,
    #[serde(default)]
    supplements: Vec<RawSupplement>,
}

#[derive(Debug, Deserialize)]
struct RawMeta {
    #[serde(default)]
    #[serde(rename = "lsNm")]
    ls_nm: Option<String>,
    #[serde(default, rename = "ancYd")]
    anc_yd: Option<String>,
    #[serde(default, rename = "efYd")]
    ef_yd: Option<String>,
    #[serde(default, rename = "lsId")]
    ls_id: Option<String>,
    #[serde(default, rename = "lsiSeq")]
    lsi_seq: Option<String>,
    #[serde(default, rename = "ancNo")]
    anc_no: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawArticle {
    #[serde(default)]
    number: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    anchor: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawSupplement {
    #[serde(default)]
    title: String,
    #[serde(default)]
    body: String,
}

// ───────── Entry point ─────────

/// Quick heuristic: does this markdown look like a statute file?
/// Used by the ingest CLI to route files.
pub fn looks_like_statute(md: &str) -> bool {
    md.contains("**MST(법령 마스터 번호)**")
        || md.contains("법령 마스터 번호")
        || (md.contains("\"articles\":") && md.contains("\"anchor\":"))
}

pub fn extract_statute(md: &str, source_path: &str) -> Result<StatuteDoc> {
    let json_text = find_json_block(md)
        .ok_or_else(|| anyhow!("statute markdown: no ```json block found in {source_path}"))?;
    let raw: RawStatute = serde_json::from_str(json_text)
        .with_context(|| format!("parsing statute JSON block in {source_path}"))?;

    let law_name = raw
        .meta
        .as_ref()
        .and_then(|m| m.ls_nm.clone())
        .or(raw.title.clone())
        .ok_or_else(|| anyhow!("statute missing law name (meta.lsNm / title) in {source_path}"))?;

    let meta = raw.meta.unwrap_or(RawMeta {
        ls_nm: None,
        anc_yd: None,
        ef_yd: None,
        ls_id: None,
        lsi_seq: None,
        anc_no: None,
    });

    let mut articles = Vec::new();
    for ra in raw.articles.unwrap_or_default() {
        let (num, sub) = match parse_article_number_field(&ra.number) {
            Some(v) => v,
            None => {
                tracing::warn!(
                    law = %law_name,
                    header = %ra.number,
                    "statute article with unparseable number — skipping"
                );
                continue;
            }
        };
        let title_kw = extract_title_keyword(&ra.number);
        let slug = statute_slug(&law_name, num, sub);

        // Citations inside this article's body; bare `제N조` inherits the
        // outer law name.
        let citations = extract_statute_citations(&ra.text, Some(&law_name));

        articles.push(StatuteArticle {
            slug,
            article_key: slug::article_key(num, sub),
            article_num: num,
            article_sub: sub,
            title_kw,
            header: ra.number,
            body: ra.text,
            citations,
        });
    }

    Ok(StatuteDoc {
        law_name,
        promulgated_at: meta.anc_yd,
        effective_at: meta.ef_yd,
        ls_id: meta.ls_id,
        ls_seq: meta.lsi_seq,
        ancestry: meta.anc_no,
        source_path: source_path.to_string(),
        articles,
        supplements: raw
            .supplements
            .into_iter()
            .map(|s| {
                let (promulgation_no, promulgation_date, note) = parse_supplement_title(&s.title);
                Supplement {
                    title: s.title,
                    body: s.body,
                    promulgation_no,
                    promulgation_date,
                    note,
                }
            })
            .collect(),
    })
}

// ───────── Internals ─────────

fn find_json_block(md: &str) -> Option<&str> {
    // Match the first ```json ... ``` fence. We scan literally rather than
    // using a regex with DOTALL to keep things deterministic.
    let open = "```json\n";
    let start = md.find(open)?;
    let after = &md[start + open.len()..];
    let end = after.find("\n```")?;
    Some(&after[..end])
}

/// Parse `제43조의2(체불사업주 명단 공개)` or `제35조` or `제35조 삭제` → `(43, Some(2))` / `(35, None)`.
fn parse_article_number_field(s: &str) -> Option<(u32, Option<u32>)> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"^\s*제\s*(\d+)\s*조(?:의\s*(\d+))?").unwrap());
    let caps = re.captures(s)?;
    let num: u32 = caps.get(1)?.as_str().parse().ok()?;
    let sub: Option<u32> = caps.get(2).and_then(|m| m.as_str().parse().ok());
    Some((num, sub))
}

/// Given `제43조의2(체불사업주 명단 공개)`, return `체불사업주 명단 공개`.
fn extract_title_keyword(header: &str) -> Option<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\(([^)]+)\)").unwrap());
    re.captures(header)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINI: &str = r#"# 근로기준법

- **MST(법령 마스터 번호)**: 276849
- **공포일자**: 20251001
- **시행일**: 20251001

```json
{
  "meta": {
    "lsNm": "근로기준법",
    "ancYd": "20251001",
    "efYd": "20251001",
    "lsId": "001872",
    "lsiSeq": "276849",
    "ancNo": "21065"
  },
  "title": "근로기준법",
  "articles": [
    {
      "anchor": "J36:0",
      "number": "제36조(금품 청산)",
      "text": "제36조(금품 청산) 사용자는 근로자가 사망 또는 퇴직한 경우에는 그 지급 사유가 발생한 때부터 14일 이내에 임금, 보상금, 그 밖의 모든 금품을 지급하여야 한다."
    },
    {
      "anchor": "J43:2",
      "number": "제43조의2(체불사업주 명단 공개)",
      "text": "제43조의2(체불사업주 명단 공개) ① 고용노동부장관은 제36조, 제43조, 제51조의3에 따른 임금, 보상금을 지급하지 아니한 사업주의 명단을 공개할 수 있다."
    },
    {
      "anchor": "J109:0",
      "number": "제109조(벌칙)",
      "text": "제109조(벌칙) ① 제36조, 제43조, 제56조를 위반한 자는 3년 이하의 징역 또는 3천만원 이하의 벌금에 처한다. 「근로자퇴직급여 보장법」 제2조제5호에 따른 급여를 지급하지 아니한 자도 같다."
    }
  ],
  "supplements": [
    {
      "title": "부      칙  <법률 제21065호, 2025. 10. 1.>",
      "body": "제8조 생략"
    }
  ]
}
```
"#;

    #[test]
    fn parses_statute_articles_and_metadata() {
        let doc = extract_statute(MINI, "test.md").unwrap();
        assert_eq!(doc.law_name, "근로기준법");
        assert_eq!(doc.promulgated_at.as_deref(), Some("20251001"));
        assert_eq!(doc.ls_id.as_deref(), Some("001872"));
        assert_eq!(doc.articles.len(), 3);

        let a36 = &doc.articles[0];
        assert_eq!(a36.slug, "statute::근로기준법::36");
        assert_eq!(a36.title_kw.as_deref(), Some("금품 청산"));

        let a43_2 = &doc.articles[1];
        assert_eq!(a43_2.slug, "statute::근로기준법::43-2");
        assert_eq!(a43_2.title_kw.as_deref(), Some("체불사업주 명단 공개"));
    }

    #[test]
    fn intra_law_citations_resolved_to_current_law() {
        let doc = extract_statute(MINI, "test.md").unwrap();
        // 제43조의2 cites 제36조 / 제43조 / 제51조의3 — all bare, all 근로기준법.
        let a43_2 = doc
            .articles
            .iter()
            .find(|a| a.article_key == "43-2")
            .unwrap();
        let keys: Vec<_> = a43_2
            .citations
            .iter()
            .map(|c| (c.law_name.as_str(), c.article, c.article_sub))
            .collect();
        assert!(keys.contains(&("근로기준법", 36, None)));
        assert!(keys.contains(&("근로기준법", 43, None)));
        assert!(keys.contains(&("근로기준법", 51, Some(3))));
    }

    #[test]
    fn cross_law_citations_respect_bracketed_law() {
        let doc = extract_statute(MINI, "test.md").unwrap();
        let a109 = doc
            .articles
            .iter()
            .find(|a| a.article_key == "109")
            .unwrap();
        // Bracketed switch to 근로자퇴직급여 보장법 for 제2조제5호.
        let has_cross = a109
            .citations
            .iter()
            .any(|c| c.law_name == "근로자퇴직급여 보장법" && c.article == 2);
        assert!(
            has_cross,
            "expected cross-law citation to 근로자퇴직급여 보장법 제2조, got {:?}",
            a109.citations
                .iter()
                .map(|c| (&c.law_name, c.article))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn rejects_file_without_json_block() {
        let bad = "# 근로기준법\nno json here";
        assert!(extract_statute(bad, "x.md").is_err());
    }

    #[test]
    fn supplement_title_parser_handles_standard_format() {
        let (num, date, note) = parse_supplement_title(
            "부칙  <법률 제21065호, 2025. 10. 1.>   (정부조직법)",
        );
        assert_eq!(num.as_deref(), Some("21065"));
        assert_eq!(date.as_deref(), Some("20251001"));
        assert_eq!(note.as_deref(), Some("정부조직법"));
    }

    #[test]
    fn supplement_title_parser_without_trailing_note() {
        let (num, date, note) =
            parse_supplement_title("부칙  <법률 제8561호, 2007. 7. 27.>");
        assert_eq!(num.as_deref(), Some("8561"));
        assert_eq!(date.as_deref(), Some("20070727"));
        assert!(note.is_none());
    }

    #[test]
    fn supplement_title_parser_returns_none_on_unrecognised() {
        let (num, date, note) = parse_supplement_title("부칙 (오래된 형식)");
        assert!(num.is_none() && date.is_none() && note.is_none());
    }
}
