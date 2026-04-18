// @Ref: SUMMARY §6D-7 — vault health check.
//
// Computes a weighted 0–100 health score from 5 signals:
//   1. Orphan documents (no inbound links, no outbound either)
//   2. Unresolved wikilinks (target_doc_id IS NULL)
//   3. Tag hygiene (detected via approximate-duplicate tag names)
//   4. Hub staleness (hub_notes with last_compiled NULL but backlink_count >= threshold)
//   5. Conflict pending (hub_notes.conflict_pending > 0)
//
// Persists a markdown report to `health_reports`. Deterministic — runs
// synchronously on the caller thread, no AI calls.

use super::store::VaultStore;
use anyhow::Result;
use rusqlite::params;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct HealthReport {
    pub score: f32,
    pub orphan_count: i64,
    pub unresolved_count: i64,
    pub tag_hygiene_issues: i64,
    pub hub_stale_count: i64,
    pub conflict_count: i64,
    pub markdown: String,
}

/// Run a health check and persist the resulting report.
///
/// The score starts at 100 and is reduced by normalised penalties per
/// signal. Total deduction capped at 100 so the score stays non-negative.
pub fn run(vault: &VaultStore) -> Result<HealthReport> {
    let conn = vault.connection().lock();

    let total_docs: i64 = conn
        .query_row("SELECT COUNT(*) FROM vault_documents", [], |r| r.get(0))
        .unwrap_or(0);
    let total_links: i64 = conn
        .query_row("SELECT COUNT(*) FROM vault_links", [], |r| r.get(0))
        .unwrap_or(0);

    // 1. Orphan docs — no inbound links and no outbound links either.
    let orphan_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM vault_documents d
             WHERE NOT EXISTS (SELECT 1 FROM vault_links WHERE target_doc_id = d.id)
               AND NOT EXISTS (SELECT 1 FROM vault_links WHERE source_doc_id = d.id)",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);

    // 2. Unresolved links — target_doc_id IS NULL.
    let unresolved_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM vault_links WHERE target_doc_id IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);

    // 3. Tag hygiene — approximate-duplicate pairs (prefix normalisation).
    let tag_hygiene_issues = detect_tag_hygiene_issues(&conn).unwrap_or(0);

    // 4. Hub staleness — over-threshold backlinks but not yet compiled.
    let hub_stale_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM hub_notes
             WHERE last_compiled IS NULL AND backlink_count >= hub_threshold",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);

    // 5. Pending conflicts.
    let conflict_count: i64 = conn
        .query_row(
            "SELECT COALESCE(SUM(conflict_pending),0) FROM hub_notes",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);

    drop(conn);

    // Normalised penalty computation. Each signal contributes at most 20
    // points — the sum caps at 100.
    let orphan_penalty =
        pct_penalty(orphan_count, total_docs, 20.0);
    let unresolved_penalty =
        pct_penalty(unresolved_count, total_links.max(1), 20.0);
    let tag_penalty = pct_penalty(tag_hygiene_issues, total_docs.max(1), 15.0);
    let hub_penalty = pct_penalty(hub_stale_count, 10, 20.0); // capped at 10 stale = full 20
    let conflict_penalty = pct_penalty(conflict_count, 5, 25.0);

    let total_penalty = orphan_penalty
        + unresolved_penalty
        + tag_penalty
        + hub_penalty
        + conflict_penalty;
    let score = (100.0 - total_penalty.min(100.0)).max(0.0);

    let markdown = render_report(
        score,
        total_docs,
        orphan_count,
        unresolved_count,
        tag_hygiene_issues,
        hub_stale_count,
        conflict_count,
    );

    // Persist.
    {
        let conn = vault.connection().lock();
        conn.execute(
            "INSERT INTO health_reports
                (health_score, orphan_count, unresolved_count, conflict_count, report_md)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                f64::from(score),
                orphan_count,
                unresolved_count,
                conflict_count,
                markdown,
            ],
        )?;
    }

    Ok(HealthReport {
        score,
        orphan_count,
        unresolved_count,
        tag_hygiene_issues,
        hub_stale_count,
        conflict_count,
        markdown,
    })
}

fn pct_penalty(offending: i64, universe: i64, max_penalty: f32) -> f32 {
    if universe == 0 {
        return 0.0;
    }
    let ratio = (offending as f32 / universe as f32).clamp(0.0, 1.0);
    max_penalty * ratio
}

/// A cluster of tag names that the semantic detector believes refer to
/// the same concept. `representative` is the canonical form (longest
/// name — usually the most descriptive).
#[derive(Debug, Clone)]
pub struct TagCluster {
    pub representative: String,
    pub members: Vec<String>,
    /// Average pairwise cosine similarity within the cluster.
    pub avg_similarity: f32,
}

/// Cluster similar tags via cosine similarity of their embeddings.
/// Requires a non-Noop embedder on the vault; returns empty otherwise.
///
/// Deterministic: single-linkage clustering with threshold; tie-breaks
/// by lexicographic tag order.
pub async fn semantic_tag_clusters(
    vault: &VaultStore,
    threshold: f32,
) -> Result<Vec<TagCluster>> {
    let tags: Vec<String> = {
        let conn = vault.connection().lock();
        let mut stmt = conn.prepare(
            "SELECT DISTINCT tag_name FROM vault_tags ORDER BY tag_name",
        )?;
        let rows: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .collect();
        rows
    };
    if tags.len() < 2 {
        return Ok(Vec::new());
    }

    // Embed each tag. Skip tags with empty embeddings (NoopEmbedding).
    let embedder = vault.embedder();
    let mut embeddings: Vec<(String, Vec<f32>)> = Vec::with_capacity(tags.len());
    for t in tags {
        match embedder.embed_one(&t).await {
            Ok(v) if !v.is_empty() => embeddings.push((t, v)),
            _ => {}
        }
    }
    if embeddings.len() < 2 {
        return Ok(Vec::new());
    }

    // Single-linkage clustering.
    let n = embeddings.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &mut [usize], i: usize) -> usize {
        if p[i] == i {
            return i;
        }
        let r = find(p, p[i]);
        p[i] = r;
        r
    }
    let mut sim_accum: std::collections::HashMap<(usize, usize), Vec<f32>> =
        std::collections::HashMap::new();

    for i in 0..n {
        for j in (i + 1)..n {
            let score = crate::memory::vector::cosine_similarity(
                &embeddings[i].1,
                &embeddings[j].1,
            );
            if score >= threshold {
                let ri = find(&mut parent, i);
                let rj = find(&mut parent, j);
                if ri != rj {
                    parent[ri] = rj;
                }
                let root = find(&mut parent, i);
                sim_accum.entry((root, root)).or_default().push(score);
            }
        }
    }

    // Group members by root.
    let mut groups: std::collections::HashMap<usize, Vec<String>> =
        std::collections::HashMap::new();
    for (idx, (tag, _)) in embeddings.iter().enumerate() {
        let root = find(&mut parent, idx);
        groups.entry(root).or_default().push(tag.clone());
    }

    let mut clusters: Vec<TagCluster> = Vec::new();
    for (root, members) in groups {
        if members.len() < 2 {
            continue;
        }
        // Representative: longest name (usually most descriptive).
        let representative = members
            .iter()
            .max_by_key(|s| s.chars().count())
            .cloned()
            .unwrap_or_default();
        let scores = sim_accum.get(&(root, root)).cloned().unwrap_or_default();
        let avg = if scores.is_empty() {
            0.0
        } else {
            scores.iter().sum::<f32>() / scores.len() as f32
        };
        let mut members_sorted = members;
        members_sorted.sort();
        clusters.push(TagCluster {
            representative,
            members: members_sorted,
            avg_similarity: avg,
        });
    }
    clusters.sort_by(|a, b| a.representative.cmp(&b.representative));
    Ok(clusters)
}

/// Detect tag hygiene issues by counting approximate-duplicate pairs.
/// A "pair" is two tags whose lowercase+whitespace-normalised forms
/// coincide but raw strings differ (e.g. "민사/손해배상" and "민사 / 손해배상").
fn detect_tag_hygiene_issues(conn: &rusqlite::Connection) -> Result<i64> {
    let mut stmt = conn.prepare("SELECT DISTINCT tag_name FROM vault_tags")?;
    let names: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .filter_map(|r| r.ok())
        .collect();

    let mut by_normal: HashMap<String, Vec<String>> = HashMap::new();
    for n in &names {
        let norm = n.to_lowercase().split_whitespace().collect::<String>();
        by_normal.entry(norm).or_default().push(n.clone());
    }
    let dup_pairs: i64 = by_normal
        .values()
        .filter(|v| v.len() >= 2)
        .map(|v| v.len() as i64 - 1)
        .sum();
    Ok(dup_pairs)
}

fn render_report(
    score: f32,
    total_docs: i64,
    orphan: i64,
    unresolved: i64,
    tag_issues: i64,
    hub_stale: i64,
    conflicts: i64,
) -> String {
    format!(
        "# MoA Vault Health Report\n\n\
**Score**: {score:.0} / 100\n\n\
- Total documents: {total_docs}\n\
- Orphan documents: {orphan}\n\
- Unresolved wikilinks: {unresolved}\n\
- Tag hygiene issues (approximate duplicates): {tag_issues}\n\
- Stale hub notes (over-threshold, never compiled): {hub_stale}\n\
- Pending conflicts: {conflicts}\n"
    )
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
    async fn empty_vault_has_perfect_score() {
        let vault = mem_store().await;
        let report = run(&vault).unwrap();
        assert!((report.score - 100.0).abs() < 0.1);
    }

    // ── R3: Semantic tag clustering ────────────────────────────
    use crate::memory::embeddings::EmbeddingProvider;
    use async_trait::async_trait;

    /// Deterministic fake embedder: 3-dim vector keyed on the first
    /// character of the input. Tags sharing the first char produce
    /// identical vectors → cosine similarity 1.0.
    struct PrefixEmbedder;
    #[async_trait]
    impl EmbeddingProvider for PrefixEmbedder {
        fn name(&self) -> &str { "prefix-fake" }
        fn dimensions(&self) -> usize { 3 }
        async fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            let mut out = Vec::with_capacity(texts.len());
            for t in texts {
                let code = t.chars().next().map(|c| c as u32).unwrap_or(0) as f32;
                let a = (code % 17.0) / 17.0;
                let b = (code % 31.0) / 31.0;
                let c = (code % 13.0) / 13.0;
                let norm = (a * a + b * b + c * c).sqrt().max(1e-6);
                out.push(vec![a / norm, b / norm, c / norm]);
            }
            Ok(out)
        }
    }

    #[tokio::test]
    async fn semantic_clusters_group_similar_embeddings() {
        let (_tmp, vault_owned) = {
            let tmp = tempfile::TempDir::new().unwrap();
            let conn = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
            let v = VaultStore::with_shared_connection(conn)
                .unwrap()
                .with_embedder(Arc::new(PrefixEmbedder));
            (tmp, v)
        };

        let pad = "본문 ".repeat(200);
        for (tag, title) in [
            ("민사소송", "doc1"),
            ("민사조정", "doc2"),
            ("형사기소", "doc3"),
            ("형사수사", "doc4"),
        ] {
            let md = format!(
                "---\ntitle: {title}\ntags: {tag}\n---\n# {title}\n\n민법 제750조. {pad}"
            );
            vault_owned
                .ingest_markdown(IngestInput {
                    source_type: SourceType::LocalFile,
                    source_device_id: "dev",
                    original_path: None,
                    title: Some(title),
                    markdown: &md,
                    html_content: None,
                    doc_type: None,
                    domain: "legal",
                })
                .await
                .unwrap();
        }
        // Threshold 0.99 — PrefixEmbedder groups tags with identical first char.
        let clusters = semantic_tag_clusters(&vault_owned, 0.99).await.unwrap();
        // Expect at least one cluster (e.g. 민사소송 ↔ 민사조정 share '민').
        assert!(
            clusters.iter().any(|c| c.members.contains(&"민사소송".into())
                && c.members.contains(&"민사조정".into())),
            "expected clustering of 민사* tags; got {clusters:?}"
        );
    }

    #[tokio::test]
    async fn semantic_clusters_empty_when_no_embedder() {
        let (_tmp, vault) = {
            let tmp = tempfile::TempDir::new().unwrap();
            let conn = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
            let v = VaultStore::with_shared_connection(conn).unwrap();
            (tmp, v)
        };
        let pad = "본문 ".repeat(200);
        vault
            .ingest_markdown(IngestInput {
                source_type: SourceType::LocalFile,
                source_device_id: "dev",
                original_path: None,
                title: Some("x"),
                markdown: &format!("---\ntags: a, b\n---\n# x\n\n{pad}"),
                html_content: None,
                doc_type: None,
                domain: "legal",
            })
            .await
            .unwrap();
        // Noop embedder → no embeddings → no clusters.
        let clusters = semantic_tag_clusters(&vault, 0.5).await.unwrap();
        assert!(clusters.is_empty());
    }

    #[tokio::test]
    async fn unresolved_links_reduce_score() {
        let vault = mem_store().await;
        // Ingest a doc that wikilinks to a non-existent entity.
        let md = format!(
            "# 테스트\n\n본문은 민법 제999조를 계속 언급한다. 민법 제999조는 존재하지 않는다. {}",
            "긴 본문 ".repeat(60)
        );
        vault
            .ingest_markdown(IngestInput {
                source_type: SourceType::LocalFile,
                source_device_id: "dev",
                original_path: None,
                title: Some("테스트"),
                markdown: &md,
                html_content: None,
                doc_type: None,
                domain: "legal",
            })
            .await
            .unwrap();

        let report = run(&vault).unwrap();
        assert!(report.unresolved_count >= 1);
        assert!(report.score < 100.0);
    }
}
