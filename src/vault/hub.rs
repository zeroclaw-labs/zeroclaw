// @Ref: SUMMARY §6D-6 — structure-mapped hub note engine (Phase 2 MVP).
//
// Hub compilation is bottom-up and driven by backlink accumulation.
// When the number of inbound wikilinks to an entity crosses a threshold
// (default 5) it becomes hub-worthy and is queued for compile. Compile
// classifies the entity type (statute / person / case / general concept),
// renders a skeleton template, maps backlinked docs into each section,
// warns on empty sections (Evidence Gap), and persists the markdown back
// to `hub_notes.content_md`. On-demand trigger only in this Phase; Phase 5
// adds idle-time automatic dispatch.

use super::store::VaultStore;
use crate::vault::wikilink::tokens::{detect_compound_tokens, CompoundTokenKind};
use anyhow::Result;
use rusqlite::params;
use std::collections::HashMap;

/// Default minimum backlink count that qualifies an entity for hub compile.
pub const HUB_THRESHOLD_DEFAULT: i64 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HubSubtype {
    /// §6D-6 법조문 뼈대: 조문 → 요건사실 → 법적효과 → 관련조문
    StatuteArticle,
    /// 인물 뼈대: 프로필 → 관련인물 → 관련사건 → 행위 시계열
    Person,
    /// 사건 뼈대: 6하원칙
    Case,
    /// 일반 개념 뼈대: 정의 → 하위분류 → 장단점 → 적용사례
    GeneralConcept,
}

impl HubSubtype {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::StatuteArticle => "statute_article",
            Self::Person => "person",
            Self::Case => "case",
            Self::GeneralConcept => "general_concept",
        }
    }

    /// Heuristic classifier. Uses the wikilink compound-token detector
    /// on the entity name itself.
    pub fn classify(entity_name: &str) -> Self {
        let toks = detect_compound_tokens(entity_name);
        for t in &toks {
            match t.kind {
                CompoundTokenKind::StatuteArticle => return Self::StatuteArticle,
                CompoundTokenKind::CaseNumber | CompoundTokenKind::PrecedentCitation => {
                    return Self::Case
                }
                CompoundTokenKind::Organization => return Self::Person,
            }
        }
        // Lightweight person check: Korean surname-prefixed short name.
        static KR_SURNAMES: &[&str] = &[
            "김", "이", "박", "최", "정", "강", "조", "윤", "장", "임", "한", "오",
            "서", "신", "권", "황", "안", "송", "류", "전", "홍", "고", "문", "양",
            "손", "배", "백", "허", "남", "심",
        ];
        let t = entity_name.trim();
        if let Some(first) = t.chars().next() {
            if KR_SURNAMES.contains(&first.to_string().as_str())
                && t.chars().count() >= 2
                && t.chars().count() <= 4
            {
                return Self::Person;
            }
        }
        Self::GeneralConcept
    }
}

#[derive(Debug, Clone)]
pub struct HubCompileReport {
    pub entity_name: String,
    pub subtype: HubSubtype,
    pub backlink_count: i64,
    pub sections: usize,
    /// Number of skeleton sections with zero mapped documents.
    /// Surfaced to the UI as Evidence Gap warnings.
    pub evidence_gaps: usize,
    pub markdown: String,
}

/// Accumulate backlink counts from every row in `vault_links` into the
/// `hub_notes` table (upsert). Runs in O(links) — called after bulk
/// ingest or on demand. Hubs below `threshold` are still created but
/// marked with `importance_score=0`.
pub fn refresh_backlink_counts(vault: &VaultStore) -> Result<()> {
    let conn = vault.connection().lock();
    // Aggregate per target_raw.
    let mut stmt = conn.prepare(
        "SELECT target_raw, COUNT(*) FROM vault_links GROUP BY target_raw",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?
        .filter_map(|r| r.ok())
        .collect::<Vec<_>>();
    drop(stmt);

    for (entity, count) in rows {
        let subtype = HubSubtype::classify(&entity);
        conn.execute(
            "INSERT INTO hub_notes
                (entity_name, hub_subtype, backlink_count, importance_score,
                 hub_threshold, pending_backlinks)
             VALUES (?1, ?2, ?3, ?4, ?5, ?3)
             ON CONFLICT(entity_name) DO UPDATE SET
                backlink_count = excluded.backlink_count,
                hub_subtype = excluded.hub_subtype,
                pending_backlinks = excluded.backlink_count,
                importance_score = excluded.importance_score",
            params![
                entity,
                subtype.as_str(),
                count,
                (count as f64).log2().max(0.0),
                HUB_THRESHOLD_DEFAULT,
            ],
        )?;
    }
    Ok(())
}

/// List entities that have crossed the threshold and are awaiting compile.
pub fn list_compile_candidates(vault: &VaultStore) -> Result<Vec<(String, i64)>> {
    let conn = vault.connection().lock();
    let mut stmt = conn.prepare(
        "SELECT entity_name, backlink_count FROM hub_notes
         WHERE backlink_count >= hub_threshold
         ORDER BY backlink_count DESC",
    )?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

/// Compile one hub note on demand. Idempotent — running twice overwrites
/// `content_md` with the latest render and updates `last_compiled`.
pub fn compile_hub(vault: &VaultStore, entity_name: &str) -> Result<HubCompileReport> {
    let subtype = HubSubtype::classify(entity_name);
    let backlinking_docs = fetch_backlinking_docs(vault, entity_name)?;
    let (markdown, sections, gaps) = render(subtype, entity_name, &backlinking_docs);

    let conn = vault.connection().lock();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    conn.execute(
        "INSERT INTO hub_notes
            (entity_name, hub_subtype, backlink_count, compile_type,
             last_compiled, structure_elements, mapped_documents,
             hub_threshold, content_md)
         VALUES (?1, ?2, ?3, 'full', ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(entity_name) DO UPDATE SET
            hub_subtype = excluded.hub_subtype,
            backlink_count = excluded.backlink_count,
            compile_type = 'full',
            last_compiled = excluded.last_compiled,
            structure_elements = excluded.structure_elements,
            mapped_documents = excluded.mapped_documents,
            pending_backlinks = 0,
            content_md = excluded.content_md",
        params![
            entity_name,
            subtype.as_str(),
            backlinking_docs.len() as i64,
            now as i64,
            sections as i64,
            backlinking_docs.len() as i64,
            HUB_THRESHOLD_DEFAULT,
            markdown,
        ],
    )?;

    Ok(HubCompileReport {
        entity_name: entity_name.to_string(),
        subtype,
        backlink_count: backlinking_docs.len() as i64,
        sections,
        evidence_gaps: gaps,
        markdown,
    })
}

/// Fetch source docs (title + id) that link to `entity_name`.
fn fetch_backlinking_docs(vault: &VaultStore, entity_name: &str) -> Result<Vec<(i64, String)>> {
    let conn = vault.connection().lock();
    let mut stmt = conn.prepare(
        "SELECT DISTINCT d.id, COALESCE(d.title, CAST(d.id AS TEXT))
         FROM vault_links l JOIN vault_documents d ON d.id = l.source_doc_id
         WHERE l.target_raw = ?1
         ORDER BY d.id ASC",
    )?;
    let rows = stmt
        .query_map(params![entity_name], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

/// Render skeleton markdown per subtype. Sections without mapped docs
/// emit ⚠️ Evidence Gap warnings.
fn render(
    subtype: HubSubtype,
    entity: &str,
    docs: &[(i64, String)],
) -> (String, usize, usize) {
    let skeleton: &[&str] = match subtype {
        HubSubtype::StatuteArticle => &[
            "조문 원문 / 정의",
            "요건사실",
            "법적 효과",
            "관련 조문 체계",
            "판례 / 해설 (백링크 종합)",
        ],
        HubSubtype::Person => &[
            "프로필",
            "관련 인물",
            "관련 사건",
            "주요 행위 시계열",
        ],
        HubSubtype::Case => &[
            "① 누가 (당사자 관계)",
            "② 언제 (시계열)",
            "③ 어디서 (장소 / 관할)",
            "④ 무엇을 (청구 취지)",
            "⑤ 어떻게 (행위 경위)",
            "⑥ 왜 (법적 근거)",
            "⑦ 쟁점 구조",
        ],
        HubSubtype::GeneralConcept => &[
            "정의",
            "하위 분류",
            "장점 / 단점",
            "적용 사례",
            "관련 개념 비교",
        ],
    };

    // Naive even-distribution mapping: each doc goes to ONE section
    // chosen by (doc_id mod sections). Phase 3 will replace this with
    // LLM-driven section assignment.
    let section_count = skeleton.len();
    let mut section_docs: HashMap<usize, Vec<&(i64, String)>> = HashMap::new();
    for d in docs {
        let idx = (d.0.unsigned_abs() as usize) % section_count;
        section_docs.entry(idx).or_default().push(d);
    }

    let mut md = String::with_capacity(512 + docs.len() * 32);
    md.push_str(&format!("# {entity}\n\n"));
    md.push_str(&format!(
        "> **Hub subtype**: `{}` · **Backlinks**: {}\n\n",
        subtype.as_str(),
        docs.len()
    ));

    let mut gaps = 0usize;
    for (idx, section_title) in skeleton.iter().enumerate() {
        let mapped = section_docs.get(&idx).cloned().unwrap_or_default();
        if mapped.is_empty() {
            md.push_str(&format!("## {section_title}\n\n"));
            md.push_str("⚠️ **Evidence Gap** — 매핑된 문서 0건.\n\n");
            gaps += 1;
        } else {
            md.push_str(&format!(
                "## {section_title}\n\n📎 {}건: {}\n\n",
                mapped.len(),
                mapped
                    .iter()
                    .map(|(id, t)| format!("[Doc-{id}] {t}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }

    (md, section_count, gaps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::ingest::{IngestInput, SourceType};
    use parking_lot::Mutex;
    use rusqlite::Connection;
    use std::sync::Arc;

    async fn mem_store() -> VaultStore {
        let conn = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
        VaultStore::with_shared_connection(conn).unwrap()
    }

    #[test]
    fn subtype_classifies_statute() {
        assert_eq!(HubSubtype::classify("민법 제750조"), HubSubtype::StatuteArticle);
    }

    #[test]
    fn subtype_classifies_case() {
        assert_eq!(HubSubtype::classify("2024가합12345"), HubSubtype::Case);
    }

    #[test]
    fn subtype_classifies_person() {
        assert_eq!(HubSubtype::classify("홍길동"), HubSubtype::Person);
    }

    #[test]
    fn subtype_falls_back_to_general_concept() {
        assert_eq!(HubSubtype::classify("투자사기"), HubSubtype::GeneralConcept);
    }

    #[tokio::test]
    async fn refresh_counts_and_compile() {
        let vault = mem_store().await;
        // Ingest several docs all linking to "민법 제750조".
        for i in 0..6 {
            let md = format!(
                "# 사건{i}\n\n본 사건은 민법 제750조에 근거한 손해배상 청구다. \
민법 제750조의 요건은 고의/과실과 위법성이다. {body}",
                body = "추가 본문 ".repeat(80)
            );
            vault
                .ingest_markdown(IngestInput {
                    source_type: SourceType::LocalFile,
                    source_device_id: "dev",
                    original_path: None,
                    title: Some(&format!("사건 {i}")),
                    markdown: &md,
                    html_content: None,
                    doc_type: None,
                    domain: "legal",
                })
                .await
                .unwrap();
        }

        refresh_backlink_counts(&vault).unwrap();
        let cands = list_compile_candidates(&vault).unwrap();
        assert!(
            cands.iter().any(|(e, n)| e == "민법 제750조" && *n >= 6),
            "expected 민법 제750조 to be a compile candidate; got {cands:?}"
        );

        let report = compile_hub(&vault, "민법 제750조").unwrap();
        assert_eq!(report.subtype, HubSubtype::StatuteArticle);
        assert!(report.sections >= 4);
        assert!(report.markdown.contains("# 민법 제750조"));
        // At least one section should have a 📎 mapping with backlink docs.
        assert!(report.markdown.contains("📎"));
    }

    #[tokio::test]
    async fn evidence_gap_flagged_when_no_backlinks() {
        let vault = mem_store().await;
        // Compile with no backlinks — all sections empty.
        let report = compile_hub(&vault, "고립된엔티티").unwrap();
        assert!(report.evidence_gaps >= 1);
        assert!(report.markdown.contains("Evidence Gap"));
    }
}
