//! `zeroclaw vault domain <subcommand>` — lifecycle operations on the
//! swappable domain corpus DB.
//!
//! Surface
//! ───────
//! - `info`       — show install state, file size, row counts.
//! - `extract`    — Step A5: migrate legal rows from `brain.db` into `domain.db`.
//! - `install`    — Step B: fetch a manifest + bundle, verify sha256, atomic-rename.
//! - `update`     — same as install, using the configured registry URL.
//! - `swap`       — alias for install with explicit detach guidance.
//! - `uninstall`  — remove `domain.db` from the workspace.
//! - `build`      — Step C1: ingest a corpus directory into a fresh domain.db.
//! - `publish`    — Step C2: emit a manifest JSON for a built bundle.
//!
//! Distribution model
//! ──────────────────
//! Operators run `vault domain build <corpus_dir> --out <bundle.db>` to
//! produce a baked corpus, then `vault domain publish <bundle.db> --url
//! <https://r2.example.com/...>` to print a manifest JSON. The JSON is
//! uploaded alongside the bundle (any S3-compatible tool: aws-cli,
//! rclone, mc) — we deliberately avoid baking an S3 SDK into the
//! binary since R2 credentials live in operator tooling, not in
//! `zeroclaw`'s runtime config.
//!
//! End users on a fresh install run `vault domain install --from
//! <manifest_url>` once; the bundle lands at
//! `<workspace>/memory/domain.db` and is auto-ATTACHed by
//! `VaultStore::open_for_workspace`.

use crate::config::Config;
use anyhow::{Context, Result};
use console::style;
use parking_lot::Mutex;
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::domain;
use super::domain_manifest::{self, BundleSpec, DomainManifest, ManifestStats};
use super::domain_migrate;

// ───────── info ─────────

pub fn info(config: &Config) -> Result<()> {
    let info = domain::info(&config.workspace_dir)?;
    println!("{}", style("vault domain — info").bold().cyan());
    println!("  installed:        {}", info.installed);
    println!("  path:             {}", info.path.display());
    println!(
        "  size_bytes:       {} ({})",
        info.size_bytes,
        human_size(info.size_bytes)
    );
    println!("  vault_documents:  {}", info.vault_documents_count);
    println!("  vault_links:      {}", info.vault_links_count);
    Ok(())
}

// ───────── extract (A5) ─────────

pub fn extract(config: &Config, delete_source: bool) -> Result<()> {
    let brain = config.workspace_dir.join("memory").join("brain.db");
    let domain_path = domain::domain_db_path(&config.workspace_dir);
    println!(
        "{} brain={} → domain={}",
        style("vault domain extract:").bold().cyan(),
        brain.display(),
        domain_path.display()
    );
    if delete_source {
        println!(
            "  {} {} flag set — migrated rows will be REMOVED from brain.db",
            style("warning:").yellow(),
            style("--delete").bold()
        );
    } else {
        println!(
            "  {} default safe-copy mode (source rows kept; pass --delete to remove)",
            style("note:").dim()
        );
    }
    let report = domain_migrate::migrate_legal_to_domain(&brain, &domain_path, delete_source)?;
    println!();
    println!("  {} {}", style("source legal rows:").dim(), report.source_legal_docs);
    println!("  {} {}", style("copied to domain: ").dim(), report.copied);
    println!(
        "  {} {}",
        style("skipped (slug):   ").dim(),
        report.skipped_slug_collision
    );
    println!(
        "  {} {}",
        style("aux rows copied:  ").dim(),
        report.aux_rows_copied
    );
    println!(
        "  {} {}",
        style("deleted from brain:").dim(),
        report.deleted_from_brain
    );
    if !report.skipped_reasons.is_empty() {
        println!();
        println!("  {} (first 50)", style("skip details:").yellow());
        for r in &report.skipped_reasons {
            println!("    - {r}");
        }
    }
    Ok(())
}

// ───────── install / update / swap ─────────

pub async fn install(config: &Config, manifest_src: &str) -> Result<()> {
    println!(
        "{} {}",
        style("vault domain install — manifest:").bold().cyan(),
        manifest_src
    );
    let manifest = domain_manifest::fetch(manifest_src)
        .await
        .with_context(|| format!("fetching manifest {manifest_src}"))?;
    println!(
        "  {} {} v{} ({} docs, {} bytes bundle)",
        style("manifest:").dim(),
        manifest.name,
        manifest.version,
        manifest.stats.vault_documents,
        manifest.bundle.size_bytes
    );

    let staging_dir = config.workspace_dir.join("memory");
    let staging = domain_manifest::download_bundle(&manifest, &staging_dir)
        .await
        .context("downloading + verifying bundle")?;
    println!("  {} sha256 verified", style("✓").green());

    // Install: detach any current ATTACH (caller may have one open
    // separately — we have nothing to detach here since this CLI runs
    // its own short-lived connection), then atomic-rename into place.
    let placed = domain::install_from(&config.workspace_dir, &staging)?;
    let _ = std::fs::remove_file(&staging);
    println!(
        "  {} installed at {}",
        style("✓").green(),
        placed.display()
    );

    // Stamp the baseline-meta keys so PR 2's update decision tree can
    // tell `current_version` from a stale manifest. For v1 manifests
    // the baseline IS the bundle, so its version + sha go straight in.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    domain::write_baseline_meta(
        &placed,
        &manifest.version,
        &manifest.bundle.sha256,
        now,
    )
    .context("stamping baseline meta on installed domain.db")?;

    let info = domain::info(&config.workspace_dir)?;
    println!(
        "  {} {} docs, {} links, {} bytes",
        style("ready:").bold().green(),
        info.vault_documents_count,
        info.vault_links_count,
        info.size_bytes
    );
    Ok(())
}

/// Resolve the registry URL the client should poll. Order:
///   1. `[domain].registry_url` in `config.toml` (the supported way).
///   2. `MOA_DOMAIN_MANIFEST_URL` env var (legacy / ops override).
///
/// Returns `None` when neither is set — the caller is expected to
/// treat that as a friendly no-op (general-public MoA build).
fn resolve_registry_url(config: &Config) -> Option<String> {
    if let Some(url) = config.domain.registry_url.as_ref() {
        let trimmed = url.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    std::env::var("MOA_DOMAIN_MANIFEST_URL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub async fn update(config: &Config) -> Result<()> {
    let Some(url) = resolve_registry_url(config) else {
        // No corpus subscribed. This is the general-public MoA build's
        // happy path — no network, no error, no surprise. Stays
        // intentionally quiet so the weekly cron output looks clean.
        println!(
            "{} no domain registry configured (skipped)",
            style("vault domain update:").dim()
        );
        return Ok(());
    };

    // Detect the manifest's schema_version once and dispatch. v1
    // manifests still flow through the legacy `install` path (full
    // re-download every time). v2 manifests route through the
    // decision tree from docs/domain-db-incremental-design.md §3.
    let raw = fetch_manifest_text(&url)
        .await
        .with_context(|| format!("fetching manifest {url}"))?;
    let schema_version = serde_json::from_str::<ManifestVersionPeek>(&raw)
        .map(|p| p.schema_version)
        .unwrap_or(0);

    match schema_version {
        1 => {
            // v1: same as `install`. The user opted in by setting a
            // registry_url, so a full download is the contract.
            install(config, &url).await
        }
        2 => update_v2(config, &url, &raw).await,
        n => anyhow::bail!(
            "unsupported manifest.schema_version {n}; this build understands 1 or 2"
        ),
    }
}

#[derive(serde::Deserialize)]
struct ManifestVersionPeek {
    schema_version: u32,
}

async fn fetch_manifest_text(url_or_path: &str) -> Result<String> {
    if url_or_path.starts_with("http://") || url_or_path.starts_with("https://") {
        let client = reqwest::Client::builder()
            .user_agent(concat!("zeroclaw/", env!("CARGO_PKG_VERSION")))
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .context("building reqwest client for manifest peek")?;
        let res = client
            .get(url_or_path)
            .send()
            .await
            .with_context(|| format!("GET {url_or_path}"))?;
        if !res.status().is_success() {
            anyhow::bail!(
                "manifest fetch failed: HTTP {} from {url_or_path}",
                res.status().as_u16()
            );
        }
        Ok(res.text().await?)
    } else {
        Ok(std::fs::read_to_string(url_or_path)
            .with_context(|| format!("reading manifest file {url_or_path}"))?)
    }
}

async fn update_v2(config: &Config, manifest_url: &str, manifest_raw: &str) -> Result<()> {
    use super::domain_delta::{self, UpdateOutcome};
    use super::domain_manifest::DomainManifestV2;

    let manifest: DomainManifestV2 = serde_json::from_str(manifest_raw)
        .with_context(|| format!("parsing v2 manifest from {manifest_url}"))?;
    domain_manifest::validate_v2(&manifest).context("validating v2 manifest")?;

    println!(
        "{} {} v{} (baseline {}, {} delta(s))",
        style("vault domain update:").bold().cyan(),
        manifest.name,
        manifest.version,
        manifest.baseline.version,
        manifest.deltas.len()
    );

    let installed = domain::read_meta(&config.workspace_dir)
        .context("reading installed domain.db meta")?;

    match domain_delta::decide(&manifest, &installed) {
        UpdateOutcome::AlreadyCurrent => {
            println!(
                "  {} already at v{} (no bytes to download)",
                style("✓").green(),
                manifest.version
            );
            Ok(())
        }
        UpdateOutcome::FullInstall => full_install_v2(config, &manifest).await,
        UpdateOutcome::ApplyDelta { delta_index } => {
            apply_delta_outcome(config, &manifest, delta_index).await
        }
    }
}

async fn full_install_v2(config: &Config, manifest: &super::domain_manifest::DomainManifestV2) -> Result<()> {
    use super::domain_manifest::{BundleSpec, DomainManifest};

    println!(
        "  {} full install of baseline v{} ({} bytes)",
        style("→").cyan(),
        manifest.baseline.version,
        manifest.baseline.size_bytes
    );

    // Reuse the v1 download_bundle path by adapting the baseline
    // into a v1-shaped manifest. That avoids duplicating the
    // download/sha gate, and the staging file lands in the same
    // place either way.
    let v1_shaped = DomainManifest {
        schema_version: 1,
        name: manifest.name.clone(),
        version: manifest.baseline.version.clone(),
        generated_at: manifest.generated_at.clone(),
        generator: manifest.generator.clone(),
        bundle: BundleSpec {
            url: manifest.baseline.url.clone(),
            sha256: manifest.baseline.sha256.clone(),
            size_bytes: manifest.baseline.size_bytes,
            compression: "none".into(),
        },
        stats: manifest.baseline.stats.clone(),
    };

    let staging_dir = config.workspace_dir.join("memory");
    let staging = domain_manifest::download_bundle(&v1_shaped, &staging_dir)
        .await
        .context("downloading + verifying baseline bundle")?;
    println!("  {} sha256 verified", style("✓").green());

    let placed = domain::install_from(&config.workspace_dir, &staging)?;
    let _ = std::fs::remove_file(&staging);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    domain::write_baseline_meta(
        &placed,
        &manifest.baseline.version,
        &manifest.baseline.sha256,
        now,
    )
    .context("stamping baseline meta on installed domain.db")?;

    // After the baseline, if the manifest also carries deltas the
    // installed `current_version` ended up at `baseline.version`,
    // not the chain head. Apply the latest delta in the same run so
    // the user reaches `manifest.version` in one update.
    if let Some(latest) = manifest.deltas.last() {
        println!(
            "  {} catching up via latest delta v{} ({} bytes)",
            style("→").cyan(),
            latest.version,
            latest.size_bytes
        );
        let staging = super::domain_delta::download_delta(latest, &staging_dir)
            .await
            .context("downloading + verifying latest delta")?;
        let now2 = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let report = super::domain_delta::apply_delta(
            &config.workspace_dir,
            &staging,
            &manifest.baseline.version,
            &latest.version,
            now2,
        )?;
        let _ = std::fs::remove_file(&staging);
        println!(
            "  {} applied v{} → v{} (+{} upserts, -{} deletes)",
            style("✓").green(),
            report.previous_version,
            report.new_version,
            report.upserted_documents,
            report.deleted_documents
        );
    }

    let info = domain::info(&config.workspace_dir)?;
    println!(
        "  {} {} docs, {} links, {} bytes",
        style("ready:").bold().green(),
        info.vault_documents_count,
        info.vault_links_count,
        info.size_bytes
    );
    Ok(())
}

async fn apply_delta_outcome(
    config: &Config,
    manifest: &super::domain_manifest::DomainManifestV2,
    delta_index: usize,
) -> Result<()> {
    let delta = manifest
        .deltas
        .get(delta_index)
        .ok_or_else(|| anyhow::anyhow!("delta_index {delta_index} out of range"))?;
    println!(
        "  {} applying latest delta v{} ({} bytes)",
        style("→").cyan(),
        delta.version,
        delta.size_bytes
    );

    let staging_dir = config.workspace_dir.join("memory");
    let staging = super::domain_delta::download_delta(delta, &staging_dir)
        .await
        .context("downloading + verifying delta")?;
    println!("  {} sha256 verified", style("✓").green());

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let report = super::domain_delta::apply_delta(
        &config.workspace_dir,
        &staging,
        &manifest.baseline.version,
        &delta.version,
        now,
    )?;
    let _ = std::fs::remove_file(&staging);

    println!(
        "  {} v{} → v{} (+{} upserts, -{} deletes)",
        style("ready:").bold().green(),
        report.previous_version,
        report.new_version,
        report.upserted_documents,
        report.deleted_documents
    );
    Ok(())
}

pub async fn swap(config: &Config, manifest_src: &str) -> Result<()> {
    // `swap` is functionally identical to `install` from the file-system
    // perspective (atomic rename), but we surface a louder warning so
    // operators know existing connections holding the file open will
    // need to reconnect.
    println!(
        "  {} any live process holding `domain.db` ATTACHed must reconnect after swap",
        style("warning:").yellow()
    );
    install(config, manifest_src).await
}

pub fn uninstall(config: &Config) -> Result<()> {
    println!("{}", style("vault domain uninstall").bold().cyan());
    let path = domain::domain_db_path(&config.workspace_dir);
    if !path.exists() {
        println!("  {} no domain.db installed (no-op)", style("note:").dim());
        return Ok(());
    }
    domain::uninstall(&config.workspace_dir)?;
    println!("  {} removed {}", style("✓").green(), path.display());
    Ok(())
}

// ───────── build (C1) ─────────

/// Walk `corpus_dir` and ingest every legal markdown into a freshly-
/// created domain.db at `out_path`. Writes are routed through the same
/// `ingest_statute_to(Domain)` / `ingest_case_to(Domain)` path the live
/// CLI uses, so the on-disk shape is byte-identical to a domain.db
/// produced by repeated `vault legal ingest` runs against an empty
/// workspace.
///
/// Refuses to overwrite an existing `out_path` — bundles are immutable
/// once published, so the operator is expected to bump the version
/// string in the output filename rather than re-baking on top of an
/// older bundle.
pub async fn build(corpus_dir: &Path, out_path: &Path) -> Result<()> {
    use crate::vault::legal::{resolve_pending_links_in, IngestTarget};

    if !corpus_dir.exists() {
        anyhow::bail!("corpus_dir not found: {}", corpus_dir.display());
    }
    if out_path.exists() {
        anyhow::bail!(
            "out path already exists ({}); pick a new versioned filename rather than overwriting a baked bundle",
            out_path.display()
        );
    }
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating output dir {}", parent.display()))?;
    }

    println!(
        "{} {} → {}",
        style("vault domain build:").bold().cyan(),
        corpus_dir.display(),
        out_path.display()
    );

    // Build into a staging file so a crash mid-build doesn't leave a
    // partially-baked bundle at the operator's chosen out_path.
    let staging = out_path.with_extension("db.staging");
    if staging.exists() {
        std::fs::remove_file(&staging).context("removing stale staging file")?;
    }
    domain::ensure_schema(&staging)
        .with_context(|| format!("init schema on staging {}", staging.display()))?;

    // Open the staging DB as the connection's MAIN, then ATTACH a
    // disposable in-memory main so the existing ingest_*_to(Domain)
    // path can target the staging file via its `domain.*` qualifiers.
    let conn = Connection::open(&staging)?;
    super::schema::init_schema(&conn)?;
    // ATTACH the SAME staging file under the alias `domain` so SQL
    // qualifiers `domain.vault_*` route correctly.
    domain::attach(&conn, &staging)?;
    let conn = Arc::new(Mutex::new(conn));

    let mut stats = BuildStats::default();
    for entry in walk_markdown(corpus_dir)? {
        match ingest_one(&conn, &entry).await {
            Ok(IngestKind::Statute(n)) => {
                stats.statute_files += 1;
                stats.statute_articles += n;
            }
            Ok(IngestKind::Case) => stats.case_files += 1,
            Ok(IngestKind::Skipped) => stats.skipped_files += 1,
            Err(e) => {
                stats.errors += 1;
                eprintln!("  {} {}: {e}", style("skip").yellow(), entry.display());
            }
        }
        if (stats.statute_files + stats.case_files) % 100 == 0
            && (stats.statute_files + stats.case_files) > 0
        {
            println!(
                "  {} statutes={} cases={} (errors={})",
                style("progress:").dim(),
                stats.statute_files,
                stats.case_files,
                stats.errors
            );
        }
    }
    let resolved = resolve_pending_links_in(&conn, IngestTarget::Domain)?;
    stats.edges_resolved_after_pass += resolved;

    // Drop conn so SQLite file handles release before rename.
    drop(conn);
    std::fs::rename(&staging, out_path).with_context(|| {
        format!(
            "atomic rename {} → {}",
            staging.display(),
            out_path.display()
        )
    })?;

    let bundle_size = std::fs::metadata(out_path)?.len();
    println!();
    println!(
        "  {} statute files:  {}",
        style("✓").green(),
        stats.statute_files
    );
    println!(
        "  {} statute articles: {}",
        style("✓").green(),
        stats.statute_articles
    );
    println!("  {} case files:     {}", style("✓").green(), stats.case_files);
    println!(
        "  {} skipped:         {}",
        style("·").dim(),
        stats.skipped_files
    );
    println!("  {} errors:          {}", style("·").dim(), stats.errors);
    println!(
        "  {} edges resolved:  {}",
        style("✓").green(),
        stats.edges_resolved_after_pass
    );
    println!(
        "  {} bundle: {} ({})",
        style("→").bold().green(),
        out_path.display(),
        human_size(bundle_size)
    );
    println!(
        "  {} run `vault domain publish {} --url <r2-url> --name <slug> --version <ver>` to emit the manifest JSON",
        style("next:").bold(),
        out_path.display()
    );
    Ok(())
}

#[derive(Debug, Default)]
struct BuildStats {
    statute_files: usize,
    statute_articles: usize,
    case_files: usize,
    skipped_files: usize,
    errors: usize,
    edges_resolved_after_pass: usize,
}

enum IngestKind {
    Statute(usize),
    Case,
    Skipped,
}

// `async` is preserved because callers `.await` this; the body is
// sync today but converting it would force an API change at the
// caller side and is out of scope here.
#[allow(clippy::unused_async)]
async fn ingest_one(conn: &Arc<Mutex<Connection>>, path: &Path) -> Result<IngestKind> {
    use crate::vault::legal::{
        encoding, extract_case, extract_statute, ingest_case_to, ingest_statute_to,
        looks_like_case, looks_like_statute, IngestTarget,
    };

    let decoded = encoding::read_markdown_auto(path)
        .with_context(|| format!("reading {}", path.display()))?;
    let body = decoded.content;
    let source_path = path.to_string_lossy().to_string();

    if looks_like_case(&body) {
        let doc = extract_case(&body, &source_path)?;
        ingest_case_to(conn, &doc, IngestTarget::Domain)?;
        Ok(IngestKind::Case)
    } else if looks_like_statute(&body) {
        let doc = extract_statute(&body, &source_path)?;
        let n = doc.articles.len();
        ingest_statute_to(conn, &doc, IngestTarget::Domain)?;
        Ok(IngestKind::Statute(n))
    } else {
        Ok(IngestKind::Skipped)
    }
}

fn walk_markdown(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    if root.is_file() {
        if is_markdown(root) {
            out.push(root.to_path_buf());
        }
        return Ok(out);
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .with_context(|| format!("reading {}", dir.display()))?;
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if is_markdown(&p) {
                out.push(p);
            }
        }
    }
    out.sort();
    Ok(out)
}

fn is_markdown(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|s| s.to_str()),
        Some("md" | "markdown" | "MD")
    )
}

// ───────── publish (C2) ─────────

pub fn publish(
    bundle_path: &Path,
    url: &str,
    name: &str,
    version: &str,
    out_manifest: Option<&Path>,
) -> Result<()> {
    if !bundle_path.exists() {
        anyhow::bail!("bundle not found: {}", bundle_path.display());
    }
    println!(
        "{} {} v{} → {}",
        style("vault domain publish:").bold().cyan(),
        name,
        version,
        url
    );

    let (sha, size) = domain_manifest::sha256_file(bundle_path)?;
    let stats = read_bundle_stats(bundle_path)?;

    let manifest = DomainManifest {
        schema_version: domain_manifest::MANIFEST_SCHEMA_VERSION,
        name: name.to_string(),
        version: version.to_string(),
        generated_at: now_rfc3339(),
        generator: Some(format!("zeroclaw {}", env!("CARGO_PKG_VERSION"))),
        bundle: BundleSpec {
            url: url.to_string(),
            sha256: sha.clone(),
            size_bytes: size,
            compression: "none".to_string(),
        },
        stats,
    };
    let json = serde_json::to_string_pretty(&manifest)?;

    let target_path = out_manifest
        .map(Path::to_path_buf)
        .unwrap_or_else(|| bundle_path.with_extension("manifest.json"));
    std::fs::write(&target_path, &json)
        .with_context(|| format!("writing manifest to {}", target_path.display()))?;

    println!("  {} sha256:    {sha}", style("✓").green());
    println!(
        "  {} size:      {} bytes ({})",
        style("✓").green(),
        size,
        human_size(size)
    );
    println!(
        "  {} manifest:  {}",
        style("✓").green(),
        target_path.display()
    );
    println!();
    println!(
        "  {} upload both files to your bucket so the bundle URL matches the manifest:",
        style("next:").bold()
    );
    println!(
        "    aws s3 cp {} s3://<bucket>/$(basename {url})",
        bundle_path.display()
    );
    println!(
        "    aws s3 cp {} s3://<bucket>/<manifest-key>.json",
        target_path.display()
    );
    println!(
        "    {} clients install with `vault domain install --from <manifest-url>`",
        style("then:").dim()
    );
    Ok(())
}

// ───────── stamp-baseline (PR 3) ─────────

/// Write the baseline meta keys into a freshly-built domain.db so it
/// can ship as the year's baseline. Run after `vault domain build`
/// (or `scripts/build_domain_db_fast.py`).
///
/// `meta.baseline_sha256` is intentionally left empty here. The bundle
/// file's checksum is **circular** (it would have to include the meta
/// row that contains itself), so the authoritative sha lives in the
/// emitted manifest, not in the bundle's meta. `publish_v2` writes the
/// manifest sha; readers needing the bundle's identity should use the
/// manifest, not `domain.db.meta.baseline_sha256`.
///
/// We document this contract by stamping an empty string here instead
/// of a placeholder string like `"auto"`, so downstream code that
/// might one day try to validate the meta sha sees a clearly-empty
/// value rather than a fake-but-non-empty one.
pub fn stamp_baseline(bundle_path: &Path, baseline_version: &str) -> Result<()> {
    if !bundle_path.exists() {
        anyhow::bail!("bundle not found: {}", bundle_path.display());
    }
    println!(
        "{} {} as baseline v{}",
        style("vault domain stamp-baseline:").bold().cyan(),
        bundle_path.display(),
        baseline_version
    );

    let conn = Connection::open(bundle_path)
        .with_context(|| format!("opening {}", bundle_path.display()))?;
    super::schema::init_schema(&conn).context("init schema before stamping")?;

    super::domain::write_baseline_meta_on_conn(&conn, baseline_version, "", 0)
        .context("writing baseline meta")?;
    drop(conn);

    println!(
        "  {} stamped (manifest's baseline.sha256 is authoritative; bundle meta sha left empty)",
        style("✓").green()
    );
    Ok(())
}

// ───────── publish_v2 (PR 3) ─────────

/// Emit a v2 manifest pointing at a freshly-published baseline. Run
/// once per annual cut (default: January 15). Use [`publish_delta`]
/// for subsequent weekly delta publications on top of the same
/// baseline.
pub fn publish_v2(
    baseline_path: &Path,
    baseline_url: &str,
    name: &str,
    baseline_version: &str,
    out_manifest: &Path,
) -> Result<()> {
    use super::domain_manifest::{BaselineSpec, DomainManifestV2};

    if !baseline_path.exists() {
        anyhow::bail!("baseline DB not found: {}", baseline_path.display());
    }
    println!(
        "{} {} v{} → {}",
        style("vault domain publish-v2:").bold().cyan(),
        name,
        baseline_version,
        baseline_url
    );

    let (sha, size) = domain_manifest::sha256_file(baseline_path)?;
    let stats = read_bundle_stats(baseline_path)?;

    let manifest = DomainManifestV2 {
        schema_version: domain_manifest::MANIFEST_SCHEMA_VERSION_V2,
        name: name.to_string(),
        // Top-level version equals the chain head — with no deltas
        // yet, that's the baseline.
        version: baseline_version.to_string(),
        generated_at: now_rfc3339(),
        generator: Some(format!("zeroclaw {}", env!("CARGO_PKG_VERSION"))),
        baseline: BaselineSpec {
            version: baseline_version.to_string(),
            url: baseline_url.to_string(),
            sha256: sha.clone(),
            size_bytes: size,
            stats: stats.clone(),
        },
        deltas: Vec::new(),
        stats,
    };
    domain_manifest::validate_v2(&manifest).context("self-validating emitted manifest")?;

    let json = serde_json::to_string_pretty(&manifest)?;
    std::fs::write(out_manifest, &json)
        .with_context(|| format!("writing manifest to {}", out_manifest.display()))?;

    println!("  {} sha256:   {sha}", style("✓").green());
    println!(
        "  {} size:     {size} bytes ({})",
        style("✓").green(),
        human_size(size)
    );
    println!(
        "  {} manifest: {}",
        style("✓").green(),
        out_manifest.display()
    );
    println!();
    println!(
        "  {} upload the baseline file under the URL above, then publish the manifest:",
        style("next:").bold()
    );
    println!(
        "    aws s3 cp {} s3://<bucket>/$(basename {baseline_url})",
        baseline_path.display()
    );
    println!(
        "    aws s3 cp {} s3://<bucket>/<manifest-key>.json",
        out_manifest.display()
    );
    Ok(())
}

// ───────── publish_delta (PR 3) ─────────

/// Append a new delta to an existing v2 manifest and write the
/// updated manifest out. Both `applies_to_baseline` and the bundle
/// integrity (sha256 + size) are verified before the manifest is
/// rewritten — a delta that's incompatible with the manifest's
/// baseline is rejected loudly so the operator can't accidentally
/// poison the chain.
pub fn publish_delta(
    delta_path: &Path,
    delta_url: &str,
    delta_version: &str,
    in_manifest: &Path,
    out_manifest: &Path,
) -> Result<()> {
    use super::domain_manifest::{DeltaOps, DeltaSpec, DomainManifestV2};

    if !delta_path.exists() {
        anyhow::bail!("delta file not found: {}", delta_path.display());
    }
    if !in_manifest.exists() {
        anyhow::bail!("in-manifest not found: {}", in_manifest.display());
    }
    println!(
        "{} delta v{} → {}",
        style("vault domain publish-delta:").bold().cyan(),
        delta_version,
        delta_url
    );

    let raw = std::fs::read_to_string(in_manifest)
        .with_context(|| format!("reading in-manifest {}", in_manifest.display()))?;
    let mut manifest: DomainManifestV2 = serde_json::from_str(&raw)
        .with_context(|| format!("parsing in-manifest {}", in_manifest.display()))?;
    domain_manifest::validate_v2(&manifest).context("validating in-manifest")?;

    // Read the delta SQLite to confirm `applies_to_baseline` matches
    // the manifest's baseline. Operators sometimes diff against the
    // wrong DB; this catches it before publish.
    let conn = Connection::open_with_flags(
        delta_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .with_context(|| format!("opening delta {}", delta_path.display()))?;
    let kind: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key='schema_kind'",
            [],
            |r| r.get(0),
        )
        .ok();
    if kind.as_deref() != Some("domain-delta") {
        anyhow::bail!(
            "delta file is not a domain-delta (meta.schema_kind = {:?})",
            kind
        );
    }
    let delta_baseline: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key='applies_to_baseline'",
            [],
            |r| r.get(0),
        )
        .ok();
    if delta_baseline.as_deref() != Some(manifest.baseline.version.as_str()) {
        anyhow::bail!(
            "delta.applies_to_baseline = {:?}, manifest.baseline.version = `{}`",
            delta_baseline,
            manifest.baseline.version
        );
    }
    let upsert_count: u64 = conn
        .query_row("SELECT COUNT(*) FROM vault_documents", [], |r| r.get(0))
        .unwrap_or(0);
    let delete_count: u64 = conn
        .query_row("SELECT COUNT(*) FROM vault_deletes", [], |r| r.get(0))
        .unwrap_or(0);
    drop(conn);

    let (sha, size) = domain_manifest::sha256_file(delta_path)?;

    let new_delta = DeltaSpec {
        version: delta_version.to_string(),
        applies_to_baseline: manifest.baseline.version.clone(),
        url: delta_url.to_string(),
        sha256: sha.clone(),
        size_bytes: size,
        generated_at: Some(now_rfc3339()),
        ops: DeltaOps {
            upsert: upsert_count,
            delete: delete_count,
        },
    };

    // Don't append a delta whose version equals the current chain
    // head — the operator probably re-ran by mistake.
    if manifest
        .deltas
        .iter()
        .any(|d| d.version == new_delta.version)
    {
        anyhow::bail!(
            "delta version `{}` already present in manifest.deltas",
            new_delta.version
        );
    }

    manifest.deltas.push(new_delta);
    manifest.version = delta_version.to_string(); // chain head bumps
    manifest.generated_at = now_rfc3339();
    domain_manifest::validate_v2(&manifest).context("validating updated manifest")?;

    let json = serde_json::to_string_pretty(&manifest)?;
    std::fs::write(out_manifest, &json)
        .with_context(|| format!("writing out-manifest {}", out_manifest.display()))?;

    println!("  {} sha256:    {sha}", style("✓").green());
    println!(
        "  {} size:      {size} bytes ({})",
        style("✓").green(),
        human_size(size)
    );
    println!(
        "  {} ops:       +{} upserts, -{} deletes",
        style("✓").green(),
        upsert_count,
        delete_count
    );
    println!(
        "  {} manifest:  {} (was: {})",
        style("✓").green(),
        out_manifest.display(),
        in_manifest.display()
    );
    println!(
        "  {} chain head: v{} ({} delta{})",
        style("✓").green(),
        manifest.version,
        manifest.deltas.len(),
        if manifest.deltas.len() == 1 { "" } else { "s" }
    );
    Ok(())
}

fn read_bundle_stats(bundle_path: &Path) -> Result<ManifestStats> {
    let conn = Connection::open_with_flags(
        bundle_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .with_context(|| format!("opening bundle read-only {}", bundle_path.display()))?;
    let docs: u64 = conn
        .query_row("SELECT COUNT(*) FROM vault_documents", [], |r| r.get(0))
        .unwrap_or(0);
    let links: u64 = conn
        .query_row("SELECT COUNT(*) FROM vault_links", [], |r| r.get(0))
        .unwrap_or(0);
    let laws: u64 = conn
        .query_row(
            "SELECT COUNT(DISTINCT value) FROM vault_frontmatter WHERE key='law_name'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let cases: u64 = conn
        .query_row(
            "SELECT COUNT(*) FROM vault_documents WHERE doc_type='case'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    Ok(ManifestStats {
        vault_documents: docs,
        vault_links: links,
        laws,
        cases,
    })
}

// ───────── helpers ─────────

fn human_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

fn now_rfc3339() -> String {
    use chrono::Utc;
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_corpus(dir: &Path) {
        std::fs::create_dir_all(dir.join("현행법령").join("20251001")).unwrap();
        std::fs::write(
            dir.join("현행법령")
                .join("20251001")
                .join("근로기준법.md"),
            r#"# 근로기준법

```json
{
  "meta": {"lsNm": "근로기준법", "ancYd": "20251001", "efYd": "20251001"},
  "title": "근로기준법",
  "articles": [
    {"anchor": "J36:0", "number": "제36조(금품 청산)", "text": "제36조 14일 이내."}
  ],
  "supplements": []
}
```
"#,
        )
        .unwrap();
    }

    #[tokio::test]
    async fn build_creates_bundle_with_legal_rows() {
        let tmp = tempfile::TempDir::new().unwrap();
        let corpus = tmp.path().join("corpus");
        make_corpus(&corpus);
        let out = tmp.path().join("baked.db");

        build(&corpus, &out).await.unwrap();

        assert!(out.exists());
        let conn = Connection::open(&out).unwrap();
        // Current-corpus ingest writes canonical + versioned slugs per
        // article, so a single article fixture produces 2 rows.
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM vault_documents", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2, "expected canonical + versioned row");
        let canonical: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM vault_documents \
                 WHERE title='statute::근로기준법::36' \
                   AND doc_type='statute_article'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(canonical, 1);
    }

    #[tokio::test]
    async fn build_refuses_to_overwrite_existing_out() {
        let tmp = tempfile::TempDir::new().unwrap();
        let corpus = tmp.path().join("corpus");
        make_corpus(&corpus);
        let out = tmp.path().join("existing.db");
        std::fs::write(&out, b"already here").unwrap();
        let err = build(&corpus, &out).await.unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn publish_writes_manifest_json_with_verified_sha() {
        let tmp = tempfile::TempDir::new().unwrap();
        let bundle = tmp.path().join("bundle.db");
        // Write a valid SQLite file (empty schema is fine for stats path).
        domain::ensure_schema(&bundle).unwrap();

        let out_manifest = tmp.path().join("bundle.manifest.json");
        publish(
            &bundle,
            "https://r2.example.com/test/bundle.db",
            "korean-legal-test",
            "0.1",
            Some(&out_manifest),
        )
        .unwrap();

        assert!(out_manifest.exists());
        let parsed: DomainManifest =
            serde_json::from_str(&std::fs::read_to_string(&out_manifest).unwrap()).unwrap();
        assert_eq!(parsed.name, "korean-legal-test");
        assert_eq!(parsed.bundle.sha256.len(), 64);
        assert!(parsed.bundle.size_bytes > 0);
        domain_manifest::validate(&parsed).unwrap();
    }

    #[test]
    fn human_size_formats_units() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2.00 KB");
        assert_eq!(human_size(2 * 1024 * 1024), "2.00 MB");
        assert_eq!(human_size(3 * 1024 * 1024 * 1024), "3.00 GB");
    }

    // ── registry_url resolution ─────────────────────────────────────

    /// Guards `MOA_DOMAIN_MANIFEST_URL` so concurrent tests in this
    /// module don't observe each other's env mutations. The whole
    /// resolution function reads/writes process-global state, so we
    /// serialise the relevant tests through a Mutex.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_clean_env<F: FnOnce() -> R, R>(f: F) -> R {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var("MOA_DOMAIN_MANIFEST_URL").ok();
        std::env::remove_var("MOA_DOMAIN_MANIFEST_URL");
        let out = f();
        match prev {
            Some(v) => std::env::set_var("MOA_DOMAIN_MANIFEST_URL", v),
            None => std::env::remove_var("MOA_DOMAIN_MANIFEST_URL"),
        }
        out
    }

    #[test]
    fn resolve_registry_url_returns_none_when_unset() {
        with_clean_env(|| {
            let cfg = Config::default();
            assert!(resolve_registry_url(&cfg).is_none());
        });
    }

    #[test]
    fn resolve_registry_url_prefers_config_over_env() {
        with_clean_env(|| {
            let mut cfg = Config::default();
            cfg.domain.registry_url = Some("https://from-config.example.com/m.json".into());
            std::env::set_var(
                "MOA_DOMAIN_MANIFEST_URL",
                "https://from-env.example.com/m.json",
            );
            assert_eq!(
                resolve_registry_url(&cfg).as_deref(),
                Some("https://from-config.example.com/m.json")
            );
        });
    }

    #[test]
    fn resolve_registry_url_falls_back_to_env() {
        with_clean_env(|| {
            std::env::set_var(
                "MOA_DOMAIN_MANIFEST_URL",
                "https://from-env.example.com/m.json",
            );
            let cfg = Config::default();
            assert_eq!(
                resolve_registry_url(&cfg).as_deref(),
                Some("https://from-env.example.com/m.json")
            );
        });
    }

    #[test]
    fn resolve_registry_url_treats_empty_string_as_unset() {
        with_clean_env(|| {
            let mut cfg = Config::default();
            cfg.domain.registry_url = Some("   ".into());
            std::env::set_var("MOA_DOMAIN_MANIFEST_URL", "");
            assert!(resolve_registry_url(&cfg).is_none());
        });
    }

    #[tokio::test]
    async fn update_with_no_registry_is_a_silent_no_op() {
        with_clean_env(|| {
            // The async block has to capture the lock-free copy; we
            // simulate by checking resolve directly here, then call the
            // actual update in a separate guarded scope.
        });
        // Now run the real update — env is already clean.
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var("MOA_DOMAIN_MANIFEST_URL").ok();
        std::env::remove_var("MOA_DOMAIN_MANIFEST_URL");

        let cfg = Config::default();
        // Should return Ok(()) and touch nothing. Any network call
        // would fail because no URL is configured.
        let res = update(&cfg).await;
        assert!(res.is_ok(), "update should succeed silently: {:?}", res);

        if let Some(v) = prev {
            std::env::set_var("MOA_DOMAIN_MANIFEST_URL", v);
        }
    }

    // ── PR 3: stamp_baseline / publish_v2 / publish_delta ────────────

    use super::super::domain_delta;
    use super::super::domain_manifest::DomainManifestV2;

    /// Build a tiny baseline DB with two seed documents at
    /// `dest_dir/baseline.db`. Returns the path.
    fn build_seed_baseline(dest_dir: &Path) -> std::path::PathBuf {
        let baseline = dest_dir.join("baseline.db");
        super::super::domain::ensure_schema(&baseline).unwrap();
        let conn = Connection::open(&baseline).unwrap();
        conn.execute(
            "INSERT INTO vault_documents
              (uuid, title, content, source_type, source_device_id,
               checksum, char_count, created_at, updated_at)
             VALUES ('uuid-1','statute::민법::750','old',
                     'local_file','dev','old-cs',3,1,1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO vault_documents
              (uuid, title, content, source_type, source_device_id,
               checksum, char_count, created_at, updated_at)
             VALUES ('uuid-doomed','statute::구법::1','dies',
                     'local_file','dev','dcs',4,1,1)",
            [],
        )
        .unwrap();
        baseline
    }

    /// Build a tiny delta with one upsert (uuid-1 new content), one
    /// new doc (uuid-2), and one delete (uuid-doomed). Returns path.
    fn build_seed_delta(dest_dir: &Path, baseline_version: &str) -> std::path::PathBuf {
        let delta = dest_dir.join("delta.sqlite");
        domain_delta::ensure_delta_schema(&delta).unwrap();
        let conn = Connection::open(&delta).unwrap();
        conn.execute(
            "INSERT INTO vault_documents
              (id, uuid, title, content, source_type, source_device_id,
               checksum, char_count, created_at, updated_at)
             VALUES (1,'uuid-1','statute::민법::750','new',
                     'local_file','dev','new-cs',3,1,2)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO vault_documents
              (id, uuid, title, content, source_type, source_device_id,
               checksum, char_count, created_at, updated_at)
             VALUES (10,'uuid-2','statute::상법::5','fresh',
                     'local_file','dev','ncs',5,1,2)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO vault_deletes (uuid, deleted_at) VALUES ('uuid-doomed', 2)",
            [],
        )
        .unwrap();
        domain_delta::stamp_delta_meta(&conn, "2026.01.22", baseline_version, &"0".repeat(64))
            .unwrap();
        delta
    }

    #[test]
    fn stamp_baseline_writes_meta_keys() {
        let tmp = tempfile::TempDir::new().unwrap();
        let baseline = build_seed_baseline(tmp.path());
        stamp_baseline(&baseline, "2026.01.15").unwrap();
        let conn = Connection::open(&baseline).unwrap();
        let m = super::super::domain::read_meta_from_conn(&conn).unwrap();
        assert!(m.is_stamped());
        assert_eq!(m.baseline_version.as_deref(), Some("2026.01.15"));
        assert_eq!(m.current_version.as_deref(), Some("2026.01.15"));
    }

    #[test]
    fn publish_v2_emits_valid_manifest_with_no_deltas() {
        let tmp = tempfile::TempDir::new().unwrap();
        let baseline = build_seed_baseline(tmp.path());
        stamp_baseline(&baseline, "2026.01.15").unwrap();
        let manifest_path = tmp.path().join("manifest.json");
        publish_v2(
            &baseline,
            "https://r2.example.com/baseline.db",
            "korean-legal",
            "2026.01.15",
            &manifest_path,
        )
        .unwrap();
        assert!(manifest_path.exists());
        let m: DomainManifestV2 =
            serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
        assert_eq!(m.schema_version, 2);
        assert_eq!(m.name, "korean-legal");
        assert_eq!(m.version, "2026.01.15");
        assert_eq!(m.baseline.version, "2026.01.15");
        assert_eq!(m.baseline.sha256.len(), 64);
        assert!(m.baseline.size_bytes > 0);
        assert!(m.deltas.is_empty());
        // The emitted manifest must round-trip through validate_v2.
        super::super::domain_manifest::validate_v2(&m).unwrap();
    }

    #[test]
    fn publish_delta_appends_and_bumps_version() {
        let tmp = tempfile::TempDir::new().unwrap();
        let baseline = build_seed_baseline(tmp.path());
        stamp_baseline(&baseline, "2026.01.15").unwrap();
        let manifest_path = tmp.path().join("manifest.json");
        publish_v2(
            &baseline,
            "https://r2.example.com/baseline.db",
            "korean-legal",
            "2026.01.15",
            &manifest_path,
        )
        .unwrap();

        let delta = build_seed_delta(tmp.path(), "2026.01.15");
        publish_delta(
            &delta,
            "https://r2.example.com/delta-2026.01.22.sqlite",
            "2026.01.22",
            &manifest_path,
            &manifest_path,
        )
        .unwrap();

        let m: DomainManifestV2 =
            serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
        assert_eq!(m.version, "2026.01.22"); // chain head bumped
        assert_eq!(m.deltas.len(), 1);
        assert_eq!(m.deltas[0].version, "2026.01.22");
        assert_eq!(m.deltas[0].applies_to_baseline, "2026.01.15");
        assert_eq!(m.deltas[0].ops.upsert, 2);
        assert_eq!(m.deltas[0].ops.delete, 1);
        super::super::domain_manifest::validate_v2(&m).unwrap();
    }

    #[test]
    fn publish_delta_rejects_baseline_mismatch() {
        let tmp = tempfile::TempDir::new().unwrap();
        let baseline = build_seed_baseline(tmp.path());
        stamp_baseline(&baseline, "2026.01.15").unwrap();
        let manifest_path = tmp.path().join("manifest.json");
        publish_v2(
            &baseline,
            "https://r2.example.com/baseline.db",
            "korean-legal",
            "2026.01.15",
            &manifest_path,
        )
        .unwrap();

        // Build a delta whose `applies_to_baseline` doesn't match.
        let delta = tmp.path().join("delta.sqlite");
        domain_delta::ensure_delta_schema(&delta).unwrap();
        let conn = Connection::open(&delta).unwrap();
        domain_delta::stamp_delta_meta(&conn, "bad-version", "2025.07.01", &"0".repeat(64))
            .unwrap();
        drop(conn);

        let err = publish_delta(
            &delta,
            "https://r2.example.com/bad.sqlite",
            "bad-version",
            &manifest_path,
            &manifest_path,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("applies_to_baseline"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn publish_delta_rejects_duplicate_version() {
        let tmp = tempfile::TempDir::new().unwrap();
        let baseline = build_seed_baseline(tmp.path());
        stamp_baseline(&baseline, "2026.01.15").unwrap();
        let manifest_path = tmp.path().join("manifest.json");
        publish_v2(
            &baseline,
            "https://r2.example.com/baseline.db",
            "korean-legal",
            "2026.01.15",
            &manifest_path,
        )
        .unwrap();
        let delta = build_seed_delta(tmp.path(), "2026.01.15");
        publish_delta(
            &delta,
            "https://r2.example.com/d.sqlite",
            "2026.01.22",
            &manifest_path,
            &manifest_path,
        )
        .unwrap();
        // Try again with the same version → must reject.
        let err = publish_delta(
            &delta,
            "https://r2.example.com/d.sqlite",
            "2026.01.22",
            &manifest_path,
            &manifest_path,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("already present"),
            "unexpected error: {err}"
        );
    }

    /// End-to-end round trip: operator publishes baseline + delta,
    /// client downloads baseline, applies delta, ends at chain head.
    #[tokio::test]
    async fn round_trip_publish_then_apply_e2e() {
        let tmp = tempfile::TempDir::new().unwrap();

        // ── Operator side ──────────────────────────────────────────
        let baseline_src = build_seed_baseline(tmp.path());
        stamp_baseline(&baseline_src, "2026.01.15").unwrap();
        let manifest_path = tmp.path().join("manifest.json");
        let baseline_url = format!(
            "{}",
            baseline_src.to_string_lossy() // local-path "URL" works for fetch_v2/download_bundle
        );
        publish_v2(
            &baseline_src,
            &baseline_url,
            "korean-legal",
            "2026.01.15",
            &manifest_path,
        )
        .unwrap();

        let delta_src = build_seed_delta(tmp.path(), "2026.01.15");
        let delta_url = format!("{}", delta_src.to_string_lossy());
        publish_delta(
            &delta_src,
            &delta_url,
            "2026.01.22",
            &manifest_path,
            &manifest_path,
        )
        .unwrap();

        // ── Client side ────────────────────────────────────────────
        // Fresh workspace (no domain.db). Configure registry_url to
        // the local manifest path. update() must:
        //   - dispatch to v2,
        //   - decide FullInstall (no installed meta),
        //   - download baseline + apply latest delta in one run,
        //   - leave domain.db at current_version=2026.01.22.
        let workspace = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(workspace.path().join("memory")).unwrap();

        let mut cfg = Config::default();
        cfg.workspace_dir = workspace.path().to_path_buf();
        cfg.domain.registry_url = Some(manifest_path.to_string_lossy().into_owned());

        // Lock env (other tests in this module mutate
        // MOA_DOMAIN_MANIFEST_URL) and clear it.
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var("MOA_DOMAIN_MANIFEST_URL").ok();
        std::env::remove_var("MOA_DOMAIN_MANIFEST_URL");

        let res = update(&cfg).await;
        assert!(res.is_ok(), "update failed: {:?}", res);

        // Verify post-state.
        let installed = super::super::domain::read_meta(&cfg.workspace_dir).unwrap();
        assert!(installed.is_stamped());
        assert_eq!(installed.baseline_version.as_deref(), Some("2026.01.15"));
        assert_eq!(installed.current_version.as_deref(), Some("2026.01.22"));

        let domain_path = super::super::domain::domain_db_path(&cfg.workspace_dir);
        let conn = Connection::open(&domain_path).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM vault_documents", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2, "1 upserted survivor + 1 new = 2 (doomed deleted)");
        let updated_content: String = conn
            .query_row(
                "SELECT content FROM vault_documents WHERE uuid = 'uuid-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(updated_content, "new", "delta upsert not applied");

        // ── Second update (AlreadyCurrent fast path) ───────────────
        // Re-running update on the same manifest must download zero
        // bytes and leave current_version unchanged.
        let res2 = update(&cfg).await;
        assert!(res2.is_ok());
        let m2 = super::super::domain::read_meta(&cfg.workspace_dir).unwrap();
        assert_eq!(m2.current_version.as_deref(), Some("2026.01.22"));

        if let Some(v) = prev {
            std::env::set_var("MOA_DOMAIN_MANIFEST_URL", v);
        }
    }
}
