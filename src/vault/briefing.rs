// @Ref: SUMMARY §6D-7 — focus briefing (Ephemeral Wiki per case).
//
// Given a `case_number`, collect all vault documents whose frontmatter
// `case_number` matches, expand 1-depth via wikilinks to pull in
// tangentially referenced docs, assemble a markdown briefing. LLM-backed
// narrative synthesis is optional — the default path produces a
// deterministic structured summary so the feature works offline.

use super::store::VaultStore;
use anyhow::Result;
use rusqlite::params;
use std::collections::HashSet;

#[derive(Debug, Clone)]
pub struct BriefingReport {
    pub case_number: String,
    pub primary_docs: Vec<(i64, String)>,
    pub related_docs: Vec<(i64, String)>,
    pub markdown: String,
}

/// Build a focus briefing for `case_number`. Persists to `briefings`
/// table and returns the rendered markdown + metadata.
pub fn generate(vault: &VaultStore, case_number: &str) -> Result<BriefingReport> {
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

    // 3. Render.
    let markdown = render(case_number, &primary, &related);

    // 4. Persist.
    {
        let conn = vault.connection().lock();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        conn.execute(
            "INSERT INTO briefings (case_number, created_at, last_updated, status, briefing_path)
             VALUES (?1, ?2, ?2, 'active', NULL)
             ON CONFLICT(case_number) DO UPDATE SET
                 last_updated = excluded.last_updated,
                 status = 'active'",
            params![case_number, now as i64],
        )?;
    }

    Ok(BriefingReport {
        case_number: case_number.to_string(),
        primary_docs: primary,
        related_docs: related,
        markdown,
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

fn render(
    case_number: &str,
    primary: &[(i64, String)],
    related: &[(i64, String)],
) -> String {
    let mut md = String::with_capacity(2048);
    md.push_str(&format!("# 사건 포커스 브리핑 — {case_number}\n\n"));
    md.push_str(&format!(
        "> 생성 시각: {}\n\n",
        chrono::Local::now().format("%Y-%m-%d %H:%M")
    ));
    md.push_str("## 사건 경과 (시계열)\n\n");
    if primary.is_empty() {
        md.push_str("⚠️ 이 사건번호로 매칭되는 문서가 아직 없습니다.\n\n");
    } else {
        for (id, title) in primary {
            md.push_str(&format!("- [Doc-{id}] {title}\n"));
        }
        md.push('\n');
    }

    md.push_str("## 관련 자료 (1-depth 그래프 확장)\n\n");
    if related.is_empty() {
        md.push_str("관련 자료 없음.\n\n");
    } else {
        for (id, title) in related {
            md.push_str(&format!("- [Doc-{id}] {title}\n"));
        }
        md.push('\n');
    }

    md.push_str("## 준비 체크리스트 (수동 작성)\n\n");
    md.push_str("- [ ] 쟁점 정리\n- [ ] 증거 현황\n- [ ] 다음 기일 준비사항\n");
    md
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
        let report = generate(&vault, "2024가합99999").unwrap();
        assert!(report.primary_docs.is_empty());
        assert!(report.markdown.contains("⚠️"));
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

        let report = generate(&vault, "2024가합12345").unwrap();
        assert_eq!(report.primary_docs.len(), 1);
        assert!(report.markdown.contains("기소장"));

        // Archive.
        let archived = archive(&vault, "2024가합12345").unwrap();
        assert!(archived);
    }
}
