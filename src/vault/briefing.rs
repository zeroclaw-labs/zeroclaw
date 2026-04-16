// @Ref: SUMMARY §6D-7 — focus briefing (Ephemeral Wiki per case).
//
// Given a `case_number`, collect all vault documents whose frontmatter
// `case_number` matches, expand 1-depth via wikilinks to pull in
// tangentially referenced docs, assemble a markdown briefing. LLM-backed
// narrative synthesis is optional — the default path produces a
// deterministic structured summary so the feature works offline.

use super::store::VaultStore;
use super::wikilink::ai_stub::{AIEngine, BriefingNarrative, HeuristicAIEngine};
use anyhow::Result;
use rusqlite::params;
use std::collections::HashSet;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct BriefingReport {
    pub case_number: String,
    pub primary_docs: Vec<(i64, String)>,
    pub related_docs: Vec<(i64, String)>,
    pub markdown: String,
    pub narrative: BriefingNarrative,
    /// True when this briefing was served from cache (no documents changed
    /// since last_updated). Incremental-update consumers can skip rendering.
    pub cached: bool,
}

/// Build a focus briefing for `case_number` using the default
/// `HeuristicAIEngine` (no provider). Persists to `briefings` table.
/// Async because engines run async; callers inside a tokio runtime
/// simply `.await` this function.
pub async fn generate(vault: &VaultStore, case_number: &str) -> Result<BriefingReport> {
    let engine: Arc<dyn AIEngine> = Arc::new(HeuristicAIEngine);
    generate_with_engine(vault, case_number, engine.as_ref(), false).await
}

/// Async variant with pluggable AIEngine. Production callers (agent loop,
/// scheduler) invoke this directly with `LlmAIEngine` for real narrative
/// synthesis; tests use `HeuristicAIEngine`.
///
/// `force_refresh=false` makes this incremental — if no primary doc has
/// been added/updated since `briefings.last_updated`, returns the cached
/// render with `cached=true`. Pass `true` after bulk changes.
pub async fn generate_with_engine(
    vault: &VaultStore,
    case_number: &str,
    engine: &dyn AIEngine,
    force_refresh: bool,
) -> Result<BriefingReport> {
    // 0. Incremental cache check.
    if !force_refresh {
        if let Some(cached) = try_load_cached(vault, case_number)? {
            return Ok(cached);
        }
    }

    // 1. Primary docs: frontmatter case_number match.
    let primary: Vec<(i64, String)> = {
        let conn = vault.connection().lock();
        let mut stmt = conn.prepare(
            "SELECT d.id, COALESCE(d.title, CAST(d.id AS TEXT))
             FROM vault_frontmatter f
             JOIN vault_documents d ON d.id = f.doc_id
             WHERE f.key = 'case_number' AND f.value = ?1
             ORDER BY d.created_at ASC",
        )?;
        let rows = stmt
            .query_map(params![case_number], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })?
            .filter_map(|r| r.ok())
            .collect::<Vec<_>>();
        rows
    };

    // 2. Related docs: 1-depth graph expansion.
    let mut seen: HashSet<i64> = primary.iter().map(|(id, _)| *id).collect();
    let mut related: Vec<(i64, String)> = Vec::new();
    {
        let conn = vault.connection().lock();
        for (id, _) in &primary {
            // Outbound
            let mut out = conn.prepare(
                "SELECT DISTINCT d.id, COALESCE(d.title, CAST(d.id AS TEXT))
                 FROM vault_links l JOIN vault_documents d ON d.id = l.target_doc_id
                 WHERE l.source_doc_id = ?1 AND l.target_doc_id IS NOT NULL",
            )?;
            for row in out.query_map(params![id], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })? {
                let (rid, rtitle) = row?;
                if seen.insert(rid) {
                    related.push((rid, rtitle));
                }
            }
            // Inbound
            let mut inb = conn.prepare(
                "SELECT DISTINCT d.id, COALESCE(d.title, CAST(d.id AS TEXT))
                 FROM vault_links l JOIN vault_documents d ON d.id = l.source_doc_id
                 WHERE l.target_doc_id = ?1",
            )?;
            for row in inb.query_map(params![id], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })? {
                let (rid, rtitle) = row?;
                if seen.insert(rid) {
                    related.push((rid, rtitle));
                }
            }
        }
    }

    // 3. LLM (or Heuristic) narrative synthesis.
    let primary_with_preview: Vec<(i64, String, String)> = primary
        .iter()
        .map(|(id, title)| {
            let preview = fetch_content_preview(vault, *id).unwrap_or_default();
            (*id, title.clone(), preview)
        })
        .collect();
    let narrative = engine
        .narrate_briefing(case_number, &primary_with_preview, &related)
        .await
        .unwrap_or_default();

    // 4. Render markdown combining structural info + narrative.
    let markdown = render(case_number, &primary, &related, &narrative);

    // 5. Persist (briefings row + narrative JSON alongside).
    {
        let conn = vault.connection().lock();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let nar_json = serde_json::to_string(&serde_json::json!({
            "timeline": narrative.timeline,
            "contentions": narrative.contentions,
            "issues": narrative.issues,
            "evidence": narrative.evidence,
            "precedents": narrative.precedents,
            "checklist": narrative.checklist,
            "strategy": narrative.strategy,
            "markdown": markdown,
        }))
        .ok();
        conn.execute(
            "INSERT INTO briefings (case_number, created_at, last_updated, status, briefing_path)
             VALUES (?1, ?2, ?2, 'active', ?3)
             ON CONFLICT(case_number) DO UPDATE SET
                 last_updated = excluded.last_updated,
                 status = 'active',
                 briefing_path = excluded.briefing_path",
            params![case_number, now as i64, nar_json],
        )?;
    }

    Ok(BriefingReport {
        case_number: case_number.to_string(),
        primary_docs: primary,
        related_docs: related,
        markdown,
        narrative,
        cached: false,
    })
}

/// Mark a briefing archived. Use when the case is closed so it no longer
/// surfaces in the active-briefings UI.
pub fn archive(vault: &VaultStore, case_number: &str) -> Result<bool> {
    let conn = vault.connection().lock();
    let changed = conn.execute(
        "UPDATE briefings SET status = 'archived' WHERE case_number = ?1",
        params![case_number],
    )?;
    Ok(changed > 0)
}

/// Returns `Some(cached_briefing)` if a briefing already exists for
/// `case_number` and no primary doc has been created/updated since its
/// `last_updated` timestamp.
fn try_load_cached(
    vault: &VaultStore,
    case_number: &str,
) -> Result<Option<BriefingReport>> {
    let conn = vault.connection().lock();
    let cached: Option<(i64, Option<String>)> = conn
        .query_row(
            "SELECT last_updated, briefing_path FROM briefings
             WHERE case_number = ?1 AND status = 'active'",
            params![case_number],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    let Some((last_updated, json_opt)) = cached else {
        return Ok(None);
    };
    // Any primary doc newer than last_updated → invalidate.
    let newer_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM vault_frontmatter f
             JOIN vault_documents d ON d.id = f.doc_id
             WHERE f.key = 'case_number' AND f.value = ?1 AND d.updated_at > ?2",
            params![case_number, last_updated],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if newer_count > 0 {
        return Ok(None);
    }
    let Some(json) = json_opt else {
        return Ok(None);
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&json) else {
        return Ok(None);
    };
    let markdown = v
        .get("markdown")
        .and_then(|m| m.as_str())
        .unwrap_or_default()
        .to_string();
    let narrative = BriefingNarrative {
        timeline: v.get("timeline").and_then(|x| x.as_str()).unwrap_or("").into(),
        contentions: v.get("contentions").and_then(|x| x.as_str()).unwrap_or("").into(),
        issues: v.get("issues").and_then(|x| x.as_str()).unwrap_or("").into(),
        evidence: v.get("evidence").and_then(|x| x.as_str()).unwrap_or("").into(),
        precedents: v.get("precedents").and_then(|x| x.as_str()).unwrap_or("").into(),
        checklist: v.get("checklist").and_then(|x| x.as_str()).unwrap_or("").into(),
        strategy: v.get("strategy").and_then(|x| x.as_str()).unwrap_or("").into(),
    };
    // Primary and related lists aren't persisted; the incremental contract
    // is that the rendered markdown is the authoritative cached artifact.
    Ok(Some(BriefingReport {
        case_number: case_number.to_string(),
        primary_docs: Vec::new(),
        related_docs: Vec::new(),
        markdown,
        narrative,
        cached: true,
    }))
}

fn fetch_content_preview(vault: &VaultStore, doc_id: i64) -> Option<String> {
    let conn = vault.connection().lock();
    conn.query_row(
        "SELECT SUBSTR(content, 1, 600) FROM vault_documents WHERE id = ?1",
        params![doc_id],
        |r| r.get::<_, String>(0),
    )
    .ok()
}

fn render(
    case_number: &str,
    primary: &[(i64, String)],
    related: &[(i64, String)],
    narrative: &BriefingNarrative,
) -> String {
    let mut md = String::with_capacity(4096);
    md.push_str(&format!("# 사건 포커스 브리핑 — {case_number}\n\n"));
    md.push_str(&format!(
        "> 생성 시각: {}\n\n",
        chrono::Local::now().format("%Y-%m-%d %H:%M")
    ));

    // 1. 사건 경과
    md.push_str("## 사건 경과 (시계열)\n\n");
    if primary.is_empty() {
        md.push_str("⚠️ 이 사건번호로 매칭되는 문서가 아직 없습니다.\n\n");
    } else if !narrative.timeline.trim().is_empty() {
        md.push_str(&narrative.timeline);
        md.push_str("\n\n");
    } else {
        for (id, title) in primary {
            md.push_str(&format!("- [Doc-{id}] {title}\n"));
        }
        md.push('\n');
    }

    append_narrative_section(&mut md, "양측 주장 대비", &narrative.contentions);
    append_narrative_section(&mut md, "핵심 쟁점", &narrative.issues);
    append_narrative_section(&mut md, "증거 현황", &narrative.evidence);
    append_narrative_section(&mut md, "관련 판례 요약", &narrative.precedents);

    md.push_str("## 관련 자료 (1-depth 그래프 확장)\n\n");
    if related.is_empty() {
        md.push_str("관련 자료 없음.\n\n");
    } else {
        for (id, title) in related {
            md.push_str(&format!("- [Doc-{id}] {title}\n"));
        }
        md.push('\n');
    }

    append_narrative_section(&mut md, "다음 기일 준비 체크리스트", &narrative.checklist);
    append_narrative_section(&mut md, "전략 제안", &narrative.strategy);
    md
}

fn append_narrative_section(md: &mut String, title: &str, body: &str) {
    md.push_str(&format!("## {title}\n\n"));
    if body.trim().is_empty() {
        md.push_str("(LLM 서사 합성 결과 없음 — `generate_with_engine(..., LlmAIEngine, true)`로 생성 필요)\n\n");
    } else {
        md.push_str(body.trim());
        md.push_str("\n\n");
    }
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

    #[tokio::test]
    async fn empty_case_produces_warning_briefing() {
        let vault = mem_store().await;
        let report = generate(&vault, "2024가합99999").await.unwrap();
        assert!(report.primary_docs.is_empty());
        assert!(report.markdown.contains("⚠️"));
        assert!(!report.cached);
    }

    #[tokio::test]
    async fn briefing_aggregates_case_and_related_docs() {
        let vault = mem_store().await;

        let pad = "법률 본문 ".repeat(80);
        let case_doc = format!(
            "---\ncase_number: 2024가합12345\n---\n# 기소장\n\n이 사건에서 민법 제750조가 쟁점이다. {pad}"
        );
        vault
            .ingest_markdown(IngestInput {
                source_type: SourceType::LocalFile,
                source_device_id: "dev",
                original_path: None,
                title: Some("기소장"),
                markdown: &case_doc,
                html_content: None,
                doc_type: None,
                domain: "legal",
            })
            .await
            .unwrap();

        let report = generate(&vault, "2024가합12345").await.unwrap();
        assert_eq!(report.primary_docs.len(), 1);
        assert!(report.markdown.contains("기소장") || report.markdown.contains("Doc-"));

        // Archive.
        let archived = archive(&vault, "2024가합12345").unwrap();
        assert!(archived);
    }

    #[tokio::test]
    async fn incremental_cache_hits_on_unchanged_case() {
        let vault = mem_store().await;
        let pad = "본문 ".repeat(200);
        vault
            .ingest_markdown(IngestInput {
                source_type: SourceType::LocalFile,
                source_device_id: "dev",
                original_path: None,
                title: Some("cached-case"),
                markdown: &format!(
                    "---\ncase_number: CACHED-1\n---\n# cached-case\n\n{pad}"
                ),
                html_content: None,
                doc_type: None,
                domain: "legal",
            })
            .await
            .unwrap();
        // First call = fresh (not cached).
        let first = generate(&vault, "CACHED-1").await.unwrap();
        assert!(!first.cached);
        // Sleep a moment so updated_at comparison is stable; then second call
        // should return cached (no docs changed).
        let second = generate(&vault, "CACHED-1").await.unwrap();
        assert!(second.cached);
        assert_eq!(first.markdown, second.markdown);
    }

    #[tokio::test]
    async fn narrative_sections_present_after_heuristic() {
        let vault = mem_store().await;
        let pad = "본문 ".repeat(200);
        vault
            .ingest_markdown(IngestInput {
                source_type: SourceType::LocalFile,
                source_device_id: "dev",
                original_path: None,
                title: Some("전략사건"),
                markdown: &format!(
                    "---\ncase_number: STRAT-1\n---\n# 전략사건\n\n{pad}"
                ),
                html_content: None,
                doc_type: None,
                domain: "legal",
            })
            .await
            .unwrap();
        let r = generate(&vault, "STRAT-1").await.unwrap();
        assert!(r.markdown.contains("양측 주장 대비"));
        assert!(r.markdown.contains("핵심 쟁점"));
        assert!(r.markdown.contains("증거 현황"));
        assert!(r.markdown.contains("전략 제안"));
    }
}
