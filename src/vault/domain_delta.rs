//! PR 2 — apply-delta path for the domain.db incremental update protocol.
//!
//! The protocol is laid out in `docs/domain-db-incremental-design.md`.
//! PR 1 shipped the manifest v2 data model + meta helpers; this module
//! consumes both: it downloads a delta SQLite, verifies sha256 + size,
//! sanity-checks `applies_to_baseline`, and applies it to the
//! installed `domain.db` inside one transaction.
//!
//! Failure model
//! ─────────────
//! Every gate fails *before* the live `domain.db` is touched:
//!   1. Manifest must be a well-formed v2 (handled by the caller).
//!   2. Delta `applies_to_baseline` must equal the installed
//!      `baseline_version`. Mismatch is fatal — the caller should
//!      fall back to FullInstall, never silent-apply.
//!   3. Downloaded bytes must match `delta.sha256` + `delta.size_bytes`.
//!   4. The delta SQLite must declare `meta.schema_kind = 'domain-delta'`
//!      and a matching `applies_to_baseline`.
//!
//! Once those pass, the apply runs as a single ATTACH-based
//! transaction. A crash mid-apply leaves the prior `current_version`
//! intact (SQLite WAL guarantees), so the next poll retries cleanly.

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

use super::domain;
use super::domain_manifest::{self, DeltaSpec, DomainManifestV2};

/// Outcome of `apply_delta` so the CLI can report what changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyDeltaReport {
    pub previous_version: String,
    pub new_version: String,
    pub upserted_documents: i64,
    pub deleted_documents: i64,
}

/// Download the delta pointed to by `delta` into `dest_dir` and verify
/// it (size + sha256). Returns the staging path. Mirrors
/// [`domain_manifest::download_bundle`] but for the delta variant —
/// kept as a thin wrapper so the caller never has to fabricate a
/// `BundleSpec` just to reuse the downloader.
pub async fn download_delta(delta: &DeltaSpec, dest_dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(dest_dir)
        .with_context(|| format!("creating staging dir {}", dest_dir.display()))?;
    let staging = dest_dir.join(format!(
        "domain-delta-{}.sqlite.staging",
        sanitize(&delta.version)
    ));

    let url = &delta.url;
    let raw = if url.starts_with("http://") || url.starts_with("https://") {
        let client = http_client()?;
        let res = client
            .get(url)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        if !res.status().is_success() {
            anyhow::bail!(
                "delta fetch failed: HTTP {} from {url}",
                res.status().as_u16()
            );
        }
        res.bytes()
            .await
            .with_context(|| format!("reading delta body from {url}"))?
            .to_vec()
    } else {
        std::fs::read(url).with_context(|| format!("reading delta file {url}"))?
    };

    if (raw.len() as u64) != delta.size_bytes {
        anyhow::bail!(
            "delta size mismatch: manifest declared {} bytes, downloaded {}",
            delta.size_bytes,
            raw.len()
        );
    }
    use sha2::{Digest, Sha256};
    let digest = hex::encode(Sha256::digest(&raw));
    if !digest.eq_ignore_ascii_case(&delta.sha256) {
        anyhow::bail!(
            "delta SHA-256 mismatch: manifest declared {}, downloaded {}",
            delta.sha256,
            digest
        );
    }
    std::fs::write(&staging, &raw)
        .with_context(|| format!("writing staging file {}", staging.display()))?;
    Ok(staging)
}

/// Apply `delta_path` to the workspace's installed `domain.db`. Wraps
/// the entire operation in a single ATTACH-based transaction, so a
/// crash mid-apply rolls back to the prior `current_version`.
///
/// Pre-conditions verified internally:
/// - `domain.db` exists and is stamped (`is_stamped() == true`).
/// - Installed `baseline_version` equals
///   `meta.applies_to_baseline` inside the delta file *and* matches
///   `expected_baseline_version` (the manifest the caller fetched —
///   this is the belt that catches a mid-air baseline cut).
/// - The caller has detached `domain` from any live process
///   connection. Windows refuses to write to an open file, and an
///   ATTACHed schema holds a handle.
///
/// `now_unix` is the timestamp written into `meta.last_applied_at`.
/// Pass the result of `SystemTime::now()` in production; tests pass a
/// fixed value for determinism.
pub fn apply_delta(
    workspace_dir: &Path,
    delta_path: &Path,
    expected_baseline_version: &str,
    new_current_version: &str,
    now_unix: i64,
) -> Result<ApplyDeltaReport> {
    let domain_path = domain::domain_db_path(workspace_dir);
    if !domain_path.exists() {
        anyhow::bail!(
            "domain.db not installed at {} — caller must FullInstall first",
            domain_path.display()
        );
    }

    // Read installed meta and confirm the baseline matches.
    let installed = domain::read_meta(workspace_dir)?;
    if !installed.is_stamped() {
        anyhow::bail!(
            "domain.db is not stamped (pre-PR-1 install) — caller must FullInstall first"
        );
    }
    let installed_baseline = installed.baseline_version.as_deref().unwrap_or("");
    if installed_baseline != expected_baseline_version {
        anyhow::bail!(
            "baseline version drift: installed `{}`, manifest expects `{}`",
            installed_baseline,
            expected_baseline_version
        );
    }
    let previous_version = installed
        .current_version
        .clone()
        .unwrap_or_else(|| installed_baseline.to_string());

    // Open the live domain.db, ATTACH the delta, validate its meta,
    // then run the upsert/delete transaction.
    let conn = Connection::open(&domain_path)
        .with_context(|| format!("opening domain.db at {}", domain_path.display()))?;

    let delta_path_lit = delta_path.to_string_lossy().replace('\'', "''");
    conn.execute_batch(&format!(
        "ATTACH DATABASE '{}' AS delta;",
        delta_path_lit
    ))
    .with_context(|| format!("ATTACH delta {}", delta_path.display()))?;

    let result = (|| -> Result<ApplyDeltaReport> {
        // Validate delta meta before doing any writes.
        let delta_kind: Option<String> = conn
            .query_row(
                "SELECT value FROM delta.meta WHERE key = 'schema_kind'",
                [],
                |r| r.get(0),
            )
            .ok();
        if delta_kind.as_deref() != Some("domain-delta") {
            anyhow::bail!(
                "delta file is not a domain-delta (meta.schema_kind = {:?})",
                delta_kind
            );
        }
        let delta_baseline: Option<String> = conn
            .query_row(
                "SELECT value FROM delta.meta WHERE key = 'applies_to_baseline'",
                [],
                |r| r.get(0),
            )
            .ok();
        if delta_baseline.as_deref() != Some(installed_baseline) {
            anyhow::bail!(
                "delta.meta.applies_to_baseline = {:?}, installed baseline = `{}`",
                delta_baseline,
                installed_baseline
            );
        }

        apply_inside_tx(&conn, new_current_version, now_unix)
    })();

    // Always DETACH, even on failure — we never want to leak the
    // staging file as an attached DB on the live connection.
    let _ = conn.execute_batch("DETACH DATABASE delta;");

    let mut report = result?;
    report.previous_version = previous_version;
    Ok(report)
}

fn apply_inside_tx(
    conn: &Connection,
    new_current_version: &str,
    now_unix: i64,
) -> Result<ApplyDeltaReport> {
    conn.execute_batch("BEGIN")?;
    let outcome = (|| -> Result<ApplyDeltaReport> {
        // ── Upserts ───────────────────────────────────────────────
        // Each table is `INSERT OR REPLACE` keyed by uuid where the
        // schema has it (vault_documents) and by `(doc_id, …)` for
        // the auxiliary tables. Auxiliary tables use INSERT OR IGNORE
        // because the apply re-derives them from the new doc rows
        // — duplicates are fine.
        let upserted_documents: i64 = conn.query_row(
            "SELECT COUNT(*) FROM delta.vault_documents",
            [],
            |r| r.get(0),
        )?;
        conn.execute(
            "INSERT OR REPLACE INTO main.vault_documents
              (id, uuid, title, content, html_content, source_type,
               source_device_id, original_path, checksum, doc_type,
               char_count, created_at, updated_at,
               embedding_model, embedding_dim, embedding_provider,
               embedding_version, embedding_created_at)
             SELECT id, uuid, title, content, html_content, source_type,
                    source_device_id, original_path, checksum, doc_type,
                    char_count, created_at, updated_at,
                    embedding_model, embedding_dim, embedding_provider,
                    embedding_version, embedding_created_at
               FROM delta.vault_documents",
            [],
        )?;
        // Auxiliary tables — copy across by source_doc_id / doc_id.
        // The delta is expected to carry only rows attached to the
        // documents it upserted, so we don't pre-clear anything.
        conn.execute(
            "INSERT OR REPLACE INTO main.vault_links
              (id, source_doc_id, target_raw, target_doc_id, display_text,
               link_type, context, line_number, is_resolved)
             SELECT id, source_doc_id, target_raw, target_doc_id, display_text,
                    link_type, context, line_number, is_resolved
               FROM delta.vault_links",
            [],
        )?;
        conn.execute(
            "INSERT OR IGNORE INTO main.vault_aliases (doc_id, alias)
             SELECT doc_id, alias FROM delta.vault_aliases",
            [],
        )?;
        conn.execute(
            "INSERT OR IGNORE INTO main.vault_frontmatter (doc_id, key, value)
             SELECT doc_id, key, value FROM delta.vault_frontmatter",
            [],
        )?;
        conn.execute(
            "INSERT OR IGNORE INTO main.vault_tags (doc_id, tag_name, tag_type)
             SELECT doc_id, tag_name, tag_type FROM delta.vault_tags",
            [],
        )?;
        // vault_embeddings is best-effort — older deltas may omit it.
        let _ = conn.execute(
            "INSERT OR REPLACE INTO main.vault_embeddings (doc_id, embedding, dim)
             SELECT doc_id, embedding, dim FROM delta.vault_embeddings",
            [],
        );

        // ── Deletes ───────────────────────────────────────────────
        // `delta.vault_deletes` lists uuid values that no longer exist
        // (statute repealed, case withdrawn). Order matters because
        // there are no FKs — clean child rows first.
        let deleted_documents: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM delta.vault_deletes",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        conn.execute(
            "DELETE FROM main.vault_links WHERE source_doc_id IN
               (SELECT id FROM main.vault_documents
                  WHERE uuid IN (SELECT uuid FROM delta.vault_deletes))",
            [],
        )?;
        conn.execute(
            "DELETE FROM main.vault_aliases WHERE doc_id IN
               (SELECT id FROM main.vault_documents
                  WHERE uuid IN (SELECT uuid FROM delta.vault_deletes))",
            [],
        )?;
        conn.execute(
            "DELETE FROM main.vault_frontmatter WHERE doc_id IN
               (SELECT id FROM main.vault_documents
                  WHERE uuid IN (SELECT uuid FROM delta.vault_deletes))",
            [],
        )?;
        conn.execute(
            "DELETE FROM main.vault_tags WHERE doc_id IN
               (SELECT id FROM main.vault_documents
                  WHERE uuid IN (SELECT uuid FROM delta.vault_deletes))",
            [],
        )?;
        let _ = conn.execute(
            "DELETE FROM main.vault_embeddings WHERE doc_id IN
               (SELECT id FROM main.vault_documents
                  WHERE uuid IN (SELECT uuid FROM delta.vault_deletes))",
            [],
        );
        conn.execute(
            "DELETE FROM main.vault_documents
              WHERE uuid IN (SELECT uuid FROM delta.vault_deletes)",
            [],
        )?;

        // ── Bookkeeping ───────────────────────────────────────────
        domain::write_delta_meta_on_conn(conn, new_current_version, now_unix)?;

        Ok(ApplyDeltaReport {
            // previous_version is patched in by the caller after we
            // exit this function; it's read from meta before the tx.
            previous_version: String::new(),
            new_version: new_current_version.to_string(),
            upserted_documents,
            deleted_documents,
        })
    })();

    match outcome {
        Ok(report) => {
            conn.execute_batch("COMMIT")?;
            Ok(report)
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// Outcome of [`update_outcome`] — drives the CLI's decision tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateOutcome {
    /// No domain.db installed yet, or the installed baseline is older
    /// than the manifest's. Caller should download `manifest.baseline`
    /// and stamp meta.
    FullInstall,
    /// Installed `current_version` already equals manifest top-level
    /// `version`. No bytes to download.
    AlreadyCurrent,
    /// Caller should download the latest delta in `manifest.deltas`
    /// and apply it. The chosen delta's index in the array is
    /// returned for clarity (always the last one with cumulative
    /// deltas, but exposing it makes tests crisp).
    ApplyDelta { delta_index: usize },
}

/// Decide what to do given the manifest and the installed meta. Pure
/// function — no I/O. The CLI calls this after fetching the manifest
/// and reading meta, then dispatches accordingly.
pub fn decide(manifest: &DomainManifestV2, installed: &domain::DomainMeta) -> UpdateOutcome {
    if !installed.is_stamped() {
        return UpdateOutcome::FullInstall;
    }
    let installed_baseline = installed.baseline_version.as_deref().unwrap_or("");
    if installed_baseline != manifest.baseline.version {
        return UpdateOutcome::FullInstall;
    }
    let installed_current = installed.current_version.as_deref().unwrap_or("");
    if installed_current == manifest.version {
        return UpdateOutcome::AlreadyCurrent;
    }
    // Cumulative deltas — the latest entry is the one to apply.
    if manifest.deltas.is_empty() {
        // Manifest claims a `version` newer than installed but has no
        // deltas. That can only happen when the manifest's top-level
        // version drifted from the chain head — `validate_v2` already
        // rejects this, so reaching it means the operator bypassed
        // validation. Treat as FullInstall to fail safe.
        return UpdateOutcome::FullInstall;
    }
    UpdateOutcome::ApplyDelta {
        delta_index: manifest.deltas.len() - 1,
    }
}

fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("zeroclaw/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .context("building reqwest client for delta fetch")
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Stamp a delta SQLite with its required meta keys. Used by tests
/// (and PR 3's `build_domain_delta` script will write the same set).
pub fn stamp_delta_meta(
    conn: &Connection,
    delta_version: &str,
    applies_to_baseline: &str,
    baseline_sha256: &str,
) -> Result<()> {
    let kvs: &[(&str, &str)] = &[
        ("schema_kind", "domain-delta"),
        ("delta_version", delta_version),
        ("applies_to_baseline", applies_to_baseline),
        ("baseline_sha256", baseline_sha256),
    ];
    for (k, v) in kvs {
        conn.execute(
            "INSERT INTO meta(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![k, v],
        )?;
    }
    Ok(())
}

/// Initialise the schema needed for a delta file: the standard vault
/// schema *plus* the `vault_deletes` table that only deltas carry.
/// Used by tests and (in PR 3) by the operator-side delta builder.
pub fn ensure_delta_schema(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(path)
        .with_context(|| format!("opening delta {}", path.display()))?;
    super::schema::init_schema(&conn).context("init vault schema on delta")?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS vault_deletes (
            uuid       TEXT NOT NULL PRIMARY KEY,
            deleted_at INTEGER NOT NULL
         );",
    )
    .context("creating vault_deletes table on delta")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::domain_manifest::{
        BaselineSpec, DeltaOps, DomainManifestV2, ManifestStats,
    };
    use rusqlite::params;
    use sha2::{Digest, Sha256};
    use tempfile::TempDir;

    // ── Decision-tree (pure) tests ──────────────────────────────────

    fn good_baseline() -> BaselineSpec {
        BaselineSpec {
            version: "2026.01.15".into(),
            url: "https://r2.example.com/baseline.db".into(),
            sha256: "0".repeat(64),
            size_bytes: 1_000_000,
            stats: ManifestStats::default(),
        }
    }

    fn good_delta(version: &str) -> DeltaSpec {
        DeltaSpec {
            version: version.into(),
            applies_to_baseline: "2026.01.15".into(),
            url: format!("file:///delta-{version}.sqlite"),
            sha256: "a".repeat(64),
            size_bytes: 4_096,
            generated_at: Some(format!("{version}T00:00:00Z")),
            ops: DeltaOps::default(),
        }
    }

    fn manifest(deltas: Vec<DeltaSpec>) -> DomainManifestV2 {
        let version = deltas
            .last()
            .map(|d| d.version.clone())
            .unwrap_or_else(|| "2026.01.15".into());
        DomainManifestV2 {
            schema_version: 2,
            name: "korean-legal".into(),
            version,
            generated_at: "2026-01-15T00:00:00Z".into(),
            generator: None,
            baseline: good_baseline(),
            deltas,
            stats: ManifestStats::default(),
        }
    }

    fn stamped_meta(baseline: &str, current: &str) -> domain::DomainMeta {
        domain::DomainMeta {
            schema_kind: Some("domain".into()),
            baseline_version: Some(baseline.into()),
            baseline_sha256: Some("0".repeat(64)),
            current_version: Some(current.into()),
            last_applied_at: Some(1),
        }
    }

    #[test]
    fn decide_full_install_when_unstamped() {
        let m = manifest(vec![]);
        let installed = domain::DomainMeta::default();
        assert_eq!(decide(&m, &installed), UpdateOutcome::FullInstall);
    }

    #[test]
    fn decide_full_install_on_baseline_drift() {
        let m = manifest(vec![]);
        // Installed baseline is older than the manifest's.
        let installed = stamped_meta("2025.01.15", "2025.01.15");
        assert_eq!(decide(&m, &installed), UpdateOutcome::FullInstall);
    }

    #[test]
    fn decide_already_current_on_match() {
        // Manifest with no deltas; installed already at baseline.
        let m = manifest(vec![]);
        let installed = stamped_meta("2026.01.15", "2026.01.15");
        assert_eq!(decide(&m, &installed), UpdateOutcome::AlreadyCurrent);
    }

    #[test]
    fn decide_already_current_when_caught_up_to_latest_delta() {
        let m = manifest(vec![good_delta("2026.01.22"), good_delta("2026.04.22")]);
        let installed = stamped_meta("2026.01.15", "2026.04.22");
        assert_eq!(decide(&m, &installed), UpdateOutcome::AlreadyCurrent);
    }

    #[test]
    fn decide_apply_delta_when_one_behind() {
        let m = manifest(vec![good_delta("2026.01.22"), good_delta("2026.04.22")]);
        let installed = stamped_meta("2026.01.15", "2026.01.22"); // applied first delta only
        assert_eq!(
            decide(&m, &installed),
            UpdateOutcome::ApplyDelta { delta_index: 1 }
        );
    }

    #[test]
    fn decide_apply_delta_when_arbitrarily_behind() {
        // Cumulative deltas — even an 11-week-stale client jumps to
        // the last entry in one shot.
        let m = manifest(vec![
            good_delta("2026.01.22"),
            good_delta("2026.02.05"),
            good_delta("2026.03.12"),
            good_delta("2026.04.22"),
        ]);
        let installed = stamped_meta("2026.01.15", "2026.01.15"); // never applied a delta
        assert_eq!(
            decide(&m, &installed),
            UpdateOutcome::ApplyDelta { delta_index: 3 }
        );
    }

    // ── apply_delta integration tests ───────────────────────────────

    fn fresh_workspace_with_baseline(baseline_version: &str) -> (TempDir, std::path::PathBuf) {
        let tmp = TempDir::new().unwrap();
        let domain_path = domain::domain_db_path(tmp.path());
        domain::ensure_schema(&domain_path).unwrap();

        // Seed two existing documents so we can observe upsert + delete.
        let conn = Connection::open(&domain_path).unwrap();
        conn.execute(
            "INSERT INTO vault_documents
               (uuid, title, content, source_type, source_device_id,
                checksum, char_count, created_at, updated_at)
             VALUES ('uuid-1','statute::민법::750','old content',
                     'local_file','dev','old-cs',5,1,1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO vault_documents
               (uuid, title, content, source_type, source_device_id,
                checksum, char_count, created_at, updated_at)
             VALUES ('uuid-doomed','statute::구법::1','to be deleted',
                     'local_file','dev','dcs',5,1,1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO vault_aliases (doc_id, alias)
             VALUES (1, '민법 제750조'), (2, '구법 제1조')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO vault_tags (doc_id, tag_name, tag_type)
             VALUES (1, 'domain:legal', 'domain'), (2, 'domain:legal', 'domain')",
            [],
        )
        .unwrap();
        drop(conn);

        domain::write_baseline_meta(
            &domain_path,
            baseline_version,
            &"0".repeat(64),
            1_700_000_000,
        )
        .unwrap();

        (tmp, domain_path)
    }

    fn build_delta(
        delta_path: &Path,
        applies_to: &str,
        upsert_uuid_1_new_content: Option<&str>,
        new_uuid: Option<&str>,
        delete_uuids: &[&str],
    ) {
        ensure_delta_schema(delta_path).unwrap();
        let conn = Connection::open(delta_path).unwrap();
        if let Some(new) = upsert_uuid_1_new_content {
            // Upsert the existing uuid-1 with new content.
            conn.execute(
                "INSERT INTO vault_documents
                   (id, uuid, title, content, source_type, source_device_id,
                    checksum, char_count, created_at, updated_at)
                 VALUES (1, 'uuid-1','statute::민법::750', ?1,
                         'local_file','dev','new-cs',5,1,2)",
                params![new],
            )
            .unwrap();
        }
        if let Some(u) = new_uuid {
            conn.execute(
                "INSERT INTO vault_documents
                   (id, uuid, title, content, source_type, source_device_id,
                    checksum, char_count, created_at, updated_at)
                 VALUES (10, ?1,'statute::상법::5','brand new',
                         'local_file','dev','ncs',5,1,2)",
                params![u],
            )
            .unwrap();
        }
        for u in delete_uuids {
            conn.execute(
                "INSERT INTO vault_deletes(uuid, deleted_at) VALUES (?1, 2)",
                params![u],
            )
            .unwrap();
        }
        stamp_delta_meta(&conn, "2026.01.22", applies_to, &"0".repeat(64)).unwrap();
    }

    #[test]
    fn apply_delta_upserts_and_deletes_in_one_tx() {
        let (tmp, domain_path) = fresh_workspace_with_baseline("2026.01.15");
        let delta_path = tmp.path().join("delta.sqlite");
        build_delta(
            &delta_path,
            "2026.01.15",
            Some("new content"),
            Some("uuid-2"),
            &["uuid-doomed"],
        );

        let report =
            apply_delta(tmp.path(), &delta_path, "2026.01.15", "2026.01.22", 2_000).unwrap();
        assert_eq!(report.previous_version, "2026.01.15");
        assert_eq!(report.new_version, "2026.01.22");
        assert_eq!(report.upserted_documents, 2);
        assert_eq!(report.deleted_documents, 1);

        // Verify post-state.
        let conn = Connection::open(&domain_path).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM vault_documents", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2, "1 upserted survived + 1 brand new = 2");
        let content: String = conn
            .query_row(
                "SELECT content FROM vault_documents WHERE uuid = 'uuid-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(content, "new content");
        let doomed: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM vault_documents WHERE uuid = 'uuid-doomed'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(doomed, 0);
        // Auxiliary cleanup of the deleted doc.
        let orphan_aliases: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM vault_aliases WHERE alias = '구법 제1조'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(orphan_aliases, 0);
        // Meta bookkeeping.
        let meta = domain::read_meta_from_conn(&conn).unwrap();
        assert_eq!(meta.current_version.as_deref(), Some("2026.01.22"));
        assert_eq!(meta.baseline_version.as_deref(), Some("2026.01.15")); // untouched
        assert_eq!(meta.last_applied_at, Some(2_000));
    }

    #[test]
    fn apply_delta_rejects_baseline_mismatch() {
        let (tmp, _path) = fresh_workspace_with_baseline("2026.01.15");
        let delta_path = tmp.path().join("delta.sqlite");
        // Delta declares it applies to a different baseline.
        build_delta(
            &delta_path,
            "2025.07.01", // mismatch
            None,
            Some("uuid-2"),
            &[],
        );
        let err = apply_delta(tmp.path(), &delta_path, "2026.01.15", "2026.01.22", 2_000)
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("applies_to_baseline"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn apply_delta_rejects_when_manifest_baseline_drifted() {
        // Installed baseline is 2026.01.15 but caller passes a
        // different `expected_baseline_version` — i.e. between
        // fetch_v2 and apply_delta the manifest's baseline rolled.
        let (tmp, _path) = fresh_workspace_with_baseline("2026.01.15");
        let delta_path = tmp.path().join("delta.sqlite");
        build_delta(&delta_path, "2026.01.15", None, Some("uuid-2"), &[]);
        let err = apply_delta(tmp.path(), &delta_path, "2027.01.15", "2027.04.22", 2_000)
            .unwrap_err();
        assert!(
            err.to_string().contains("baseline version drift"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn apply_delta_rejects_unstamped_domain_db() {
        let tmp = TempDir::new().unwrap();
        // domain.db exists but has no meta rows.
        domain::ensure_schema(&domain::domain_db_path(tmp.path())).unwrap();
        let delta_path = tmp.path().join("delta.sqlite");
        build_delta(&delta_path, "2026.01.15", None, Some("uuid-2"), &[]);
        let err = apply_delta(tmp.path(), &delta_path, "2026.01.15", "2026.01.22", 2_000)
            .unwrap_err();
        assert!(
            err.to_string().contains("not stamped"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn apply_delta_rejects_wrong_kind() {
        let (tmp, _path) = fresh_workspace_with_baseline("2026.01.15");
        let delta_path = tmp.path().join("delta.sqlite");
        ensure_delta_schema(&delta_path).unwrap();
        // Put a wrong schema_kind.
        let conn = Connection::open(&delta_path).unwrap();
        conn.execute(
            "INSERT INTO meta(key,value) VALUES ('schema_kind','domain')",
            [],
        )
        .unwrap();
        drop(conn);
        let err = apply_delta(tmp.path(), &delta_path, "2026.01.15", "2026.01.22", 2_000)
            .unwrap_err();
        assert!(
            err.to_string().contains("schema_kind"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn apply_delta_rolls_back_on_constraint_violation() {
        let (tmp, domain_path) = fresh_workspace_with_baseline("2026.01.15");
        let delta_path = tmp.path().join("delta.sqlite");
        ensure_delta_schema(&delta_path).unwrap();
        // Two rows with the same primary-key id and the same uuid will
        // cleanly upsert, but two rows with different uuids and the
        // same id will collide on INSERT OR REPLACE — actually that
        // also replaces. To force a real failure we craft a delta
        // where vault_links references a doc id that doesn't exist
        // and add a CHECK by inserting a row with a malformed JSON
        // — but the live schema has no such constraint. Easier path:
        // poison the delta meta so apply hits the schema_kind gate
        // *after* the connection opens, BUT we already test that.
        //
        // For an actual rollback test, we drop the vault_documents
        // table on the delta after stamping so the upsert SELECT
        // fails inside the tx.
        let conn = Connection::open(&delta_path).unwrap();
        stamp_delta_meta(&conn, "2026.01.22", "2026.01.15", &"0".repeat(64)).unwrap();
        conn.execute("DROP TABLE vault_documents", []).unwrap();
        drop(conn);

        let err = apply_delta(tmp.path(), &delta_path, "2026.01.15", "2026.01.22", 2_000)
            .unwrap_err();
        assert!(err.to_string().contains("vault_documents") || err.to_string().contains("no such"));

        // Post-state: domain.db unchanged.
        let conn = Connection::open(&domain_path).unwrap();
        let meta = domain::read_meta_from_conn(&conn).unwrap();
        assert_eq!(
            meta.current_version.as_deref(),
            Some("2026.01.15"),
            "current_version must be untouched after rollback"
        );
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM vault_documents", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2, "documents must be unchanged after rollback");
    }

    // ── download_delta tests ────────────────────────────────────────

    #[tokio::test]
    async fn download_delta_verifies_sha_and_size() {
        let tmp = TempDir::new().unwrap();
        let bundle = tmp.path().join("source.sqlite");
        let bytes = b"fake delta sqlite bytes".to_vec();
        std::fs::write(&bundle, &bytes).unwrap();
        let sha = hex::encode(Sha256::digest(&bytes));

        let mut delta = good_delta("2026.01.22");
        delta.url = bundle.to_string_lossy().into_owned();
        delta.sha256 = sha;
        delta.size_bytes = bytes.len() as u64;

        let staging = download_delta(&delta, tmp.path()).await.unwrap();
        assert!(staging.exists());
        assert_eq!(std::fs::read(&staging).unwrap(), bytes);
    }

    #[tokio::test]
    async fn download_delta_rejects_sha_mismatch() {
        let tmp = TempDir::new().unwrap();
        let bundle = tmp.path().join("source.sqlite");
        std::fs::write(&bundle, b"actual bytes").unwrap();

        let mut delta = good_delta("2026.01.22");
        delta.url = bundle.to_string_lossy().into_owned();
        delta.sha256 = "a".repeat(64); // wrong
        delta.size_bytes = std::fs::metadata(&bundle).unwrap().len();

        let err = download_delta(&delta, tmp.path()).await.unwrap_err();
        assert!(err.to_string().contains("SHA-256 mismatch"));
    }
}
