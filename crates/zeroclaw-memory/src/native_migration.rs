//! Explicit offline conversion of native ZeroClaw `brain.db` stores into a
//! new Tachi `PortableKernel` memory database (`tachi.db`).
//!
//! Safety guarantees mirror [`crate::portable_migration`]:
//! - Callers must stop all writers targeting the source database before running.
//! - Source files are captured read-only into an owner-only staging directory;
//!   SQLite only opens private copies and never mutates or checkpoints the
//!   original database or its `-wal`/`-shm` sidecars.
//! - Unmapped tables, unknown columns, unmapped `agent_id` references, Soul
//!   namespace/key rows, malformed `kind` JSON, or non-1024/non-finite vectors
//!   fail closed without publishing any output.
//! - ZeroClaw's reversible `superseded_by` marker is mapped into
//!   `metadata["superseded_by"]` (with `PortableImportEntry.superseded_by = None`)
//!   so re-storing a superseded key restores visibility under [`crate::tachi::TachiMemory`].

use crate::kernel_store::KernelStoreKind;
use crate::portable_migration::source::{self, SourceFileDigest};
use crate::portable_migration::{
    HeldDirectory, backup, checkpoint, close_connection, open_read_only_snapshot, open_store,
    private_tempfile, publish_verified_target, refuse_occupied_target, set_private_permissions,
};
use crate::tachi::{
    METADATA_SUPERSEDED_BY, METADATA_ZC_CREATED_AT, METADATA_ZC_UPDATED_AT, TachiMemory,
};
use crate::traits::{MemoryCategory, MemoryKind, StoreOptions};
use anyhow::{Context, Result, bail, ensure};
use chrono::{DateTime, NaiveDateTime, SecondsFormat, Utc};
use memcore::{DbOpenContext, PortableImportEntry};
use portable_kernel as memcore;
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

const RECEIPT_FORMAT: &str = "zeroclaw.native_brain.migration/v1";
const RECEIPT_NAMESPACE: &str = "zeroclaw_native_brain_migration";
const MEMCORE_VECTOR_DIM: usize = 1024;
const IMPORT_BATCH_SIZE: usize = 256;

const ALLOWED_NATIVE_TABLES: &[&str] = &[
    "memories",
    "agents",
    "embedding_cache",
    "memory_meta",
    "schema_version",
    "sqlite_sequence",
    "sqlite_stat1",
    "sqlite_stat4",
    "memories_fts",
    "memories_fts_data",
    "memories_fts_idx",
    "memories_fts_content",
    "memories_fts_docsize",
    "memories_fts_config",
];

const ALLOWED_MEMORIES_COLUMNS: &[&str] = &[
    "id",
    "key",
    "content",
    "category",
    "embedding",
    "created_at",
    "updated_at",
    "session_id",
    "namespace",
    "importance",
    "superseded_by",
    "agent_id",
    "kind",
    "pinned",
    "tenant_id",
];

const REQUIRED_MEMORIES_COLUMNS: &[&str] = &["id", "key", "content", "category", "created_at"];
const ALLOWED_AGENTS_COLUMNS: &[&str] = &["id", "alias", "created_at"];
const REQUIRED_AGENTS_COLUMNS: &[&str] = &["id", "alias"];

/// Structured audit receipt written into `hard_state` and emitted on stdout
/// after verifying the published native-to-Tachi migration target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NativeMigrationReceipt {
    pub format: String,
    pub receipt_namespace: String,
    pub receipt_key: String,
    pub approved_by: String,
    pub created_at: String,
    pub source: PathBuf,
    pub destination: PathBuf,
    pub target_schema: i64,
    pub target_profile: String,
    pub role: String,
    pub requires_offline_quiescent_source: bool,
    pub source_row_counts: BTreeMap<String, u64>,
    pub imported_rows: u64,
    pub superseded_rows: u64,
    pub vector_rows: u64,
    pub agent_aliases: Vec<String>,
    pub source_files: Vec<SourceFileDigest>,
    pub source_snapshot_sha256: String,
}

struct ExpectedNativeRow {
    id: String,
    key: String,
    content: String,
    category: MemoryCategory,
    timestamp: String,
    expected_created_instant: DateTime<Utc>,
    session_id: Option<String>,
    namespace: String,
    importance: f64,
    superseded_by: Option<String>,
    kind: Option<MemoryKind>,
    pinned: bool,
    tenant_id: Option<String>,
    agent_alias: String,
    vector: Option<Vec<f32>>,
}

struct PlannedRow {
    import: PortableImportEntry,
    expected: ExpectedNativeRow,
}

struct NativeImportPlan {
    rows: Vec<PlannedRow>,
    source_row_counts: BTreeMap<String, u64>,
    superseded_rows: u64,
    vector_rows: u64,
    agent_aliases: Vec<String>,
}

/// Convert a quiescent native ZeroClaw `brain.db` database into a verified new
/// Tachi `PortableKernel` memory database without modifying `source`.
pub fn migrate_native_brain_store(
    source: &Path,
    destination: &Path,
    approved_by: &str,
) -> Result<NativeMigrationReceipt> {
    ensure!(
        cfg!(unix),
        "offline native brain migration currently supports Unix physical file identity and owner-only permissions only"
    );
    migrate_native_inner(source, destination, approved_by, || Ok(()), |_| Ok(()))
}

fn migrate_native_inner(
    source_path: &Path,
    destination: &Path,
    approved_by: &str,
    after_source_copy: impl FnOnce() -> Result<()>,
    after_publication: impl FnOnce(&Path) -> Result<()>,
) -> Result<NativeMigrationReceipt> {
    ensure!(
        !approved_by.trim().is_empty(),
        "migration requires a nonblank approved_by value"
    );
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    ensure!(
        parent.is_dir(),
        "migration destination directory must already exist"
    );
    ensure!(
        destination.file_name().is_some(),
        "migration destination must name a file"
    );
    refuse_occupied_target(destination)?;

    let source_metadata = std::fs::symlink_metadata(source_path)
        .with_context(|| format!("inspect native source '{}'", source_path.display()))?;
    ensure!(
        source_metadata.is_file() && !source_metadata.file_type().is_symlink(),
        "native source must be a regular non-symlink file"
    );
    source::physical_identity(&source_metadata)?;

    let original = std::fs::canonicalize(source_path)?;
    let final_parent = std::fs::canonicalize(parent)?;
    let mut directory = HeldDirectory::open(&final_parent)?;
    let final_path = final_parent.join(
        destination
            .file_name()
            .context("destination filename is missing")?,
    );
    ensure!(original != final_path, "source and destination must differ");

    let staging = tempfile::Builder::new()
        .prefix(".zeroclaw-native-migration-")
        .tempdir_in(&final_parent)?;
    set_private_permissions(staging.path(), true)?;
    directory.require_private_owner(staging.path())?;

    let copied = staging.path().join("source-copy.db");
    let source_files = source::stable_copy(&original, &copied, after_source_copy)?;

    memcore::db::enable_simple_auto_extension()?;
    memcore::db::register_sqlite_vec();

    let original_snapshot = open_read_only_snapshot(&copied)?;
    let snapshot_file = private_tempfile(staging.path(), "snapshot-")?;
    backup(&original_snapshot, snapshot_file.path())?;
    original_snapshot.execute_batch("ROLLBACK")?;
    close_connection(original_snapshot)?;
    checkpoint(snapshot_file.path())?;
    let snapshot_hash = source::file_digest(snapshot_file.path())?;

    let source_snapshot = open_read_only_snapshot(snapshot_file.path())?;
    let plan = preflight_and_build_plan(&source_snapshot)?;
    source_snapshot.execute_batch("ROLLBACK")?;
    close_connection(source_snapshot)?;

    let target = private_tempfile(staging.path(), "target-")?;
    {
        let mut store = open_store(
            target.path(),
            KernelStoreKind::Memory,
            DbOpenContext::create_fresh(),
        )?;
        let imports: Vec<PortableImportEntry> =
            plan.rows.iter().map(|row| row.import.clone()).collect();
        for chunk in imports.chunks(IMPORT_BATCH_SIZE) {
            store.import_snapshot_batch(chunk)?;
        }
        ensure!(
            store.quick_check()?,
            "native migration target quick_check failed after import"
        );
    }

    checkpoint(target.path())?;
    let target_schema = {
        let conn = open_read_only_snapshot(target.path())?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        conn.execute_batch("ROLLBACK")?;
        close_connection(conn)?;
        version
    };

    let receipt_key = uuid::Uuid::new_v4().to_string();
    let receipt = NativeMigrationReceipt {
        format: RECEIPT_FORMAT.into(),
        receipt_namespace: RECEIPT_NAMESPACE.into(),
        receipt_key: receipt_key.clone(),
        approved_by: approved_by.into(),
        created_at: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        source: original.clone(),
        destination: final_path.clone(),
        target_schema,
        target_profile: "portable_kernel".into(),
        role: KernelStoreKind::Memory.role().into(),
        requires_offline_quiescent_source: true,
        source_row_counts: plan.source_row_counts.clone(),
        imported_rows: plan.rows.len() as u64,
        superseded_rows: plan.superseded_rows,
        vector_rows: plan.vector_rows,
        agent_aliases: plan.agent_aliases.clone(),
        source_files: source_files.clone(),
        source_snapshot_sha256: snapshot_hash,
    };
    let receipt_json = serde_json::to_string(&receipt)?;

    {
        let store = open_store(
            target.path(),
            KernelStoreKind::Memory,
            DbOpenContext::open_existing_deny(),
        )?;
        ensure!(
            store.insert_state_if_absent(RECEIPT_NAMESPACE, &receipt_key, &receipt_json)?,
            "new native migration receipt key collides with existing history"
        );
    }

    validate_native_target(target.path(), &plan, &receipt_key, &receipt_json, || Ok(()))?;
    checkpoint(target.path())?;
    set_private_permissions(target.path(), false)?;
    target.as_file().sync_all()?;
    source::verify_unchanged(&original, &source_files)?;
    refuse_occupied_target(&final_path)?;

    publish_verified_target(
        target,
        &final_path,
        directory,
        &original,
        &source_files,
        after_publication,
        |dest, boundary| validate_native_target(dest, &plan, &receipt_key, &receipt_json, boundary),
    )?;

    Ok(receipt)
}

fn preflight_and_build_plan(conn: &Connection) -> Result<NativeImportPlan> {
    let quick: String = conn.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
    ensure!(
        quick == "ok",
        "native source SQLite quick_check failed: {quick}"
    );
    let mut fk_check = conn.prepare("PRAGMA foreign_key_check")?;
    ensure!(
        fk_check.query([])?.next()?.is_none(),
        "native source database contains broken foreign key references"
    );
    drop(fk_check);

    let tables = census_tables(conn)?;
    ensure!(
        tables.contains("memories"),
        "native source database is missing required 'memories' table"
    );
    for table in &tables {
        ensure!(
            ALLOWED_NATIVE_TABLES.contains(&table.as_str()),
            "refusing unsupported table '{table}' in native brain source"
        );
    }

    let mut source_row_counts = BTreeMap::new();
    for table in ["memories", "agents", "embedding_cache", "memory_meta"] {
        if tables.contains(table) {
            source_row_counts.insert(table.to_string(), count_table_rows(conn, table)?);
        }
    }

    let mem_cols = inspect_table_columns(
        conn,
        "memories",
        ALLOWED_MEMORIES_COLUMNS,
        REQUIRED_MEMORIES_COLUMNS,
    )?;

    let agents_by_id = if tables.contains("agents") {
        let _agent_cols = inspect_table_columns(
            conn,
            "agents",
            ALLOWED_AGENTS_COLUMNS,
            REQUIRED_AGENTS_COLUMNS,
        )?;
        load_agents_map(conn)?
    } else {
        HashMap::new()
    };

    let sql = build_memories_select_sql(&mem_cols);
    let mut stmt = conn.prepare(&sql)?;
    let mut raw_rows = stmt.query([])?;

    let mut planned = Vec::new();
    let mut superseded_rows = 0u64;
    let mut vector_rows = 0u64;
    let mut agent_aliases = BTreeSet::new();

    while let Some(row) = raw_rows.next()? {
        let plan_row = build_planned_row(
            row,
            &agents_by_id,
            &mut superseded_rows,
            &mut vector_rows,
            &mut agent_aliases,
        )?;
        planned.push(plan_row);
    }

    Ok(NativeImportPlan {
        rows: planned,
        source_row_counts,
        superseded_rows,
        vector_rows,
        agent_aliases: agent_aliases.into_iter().collect(),
    })
}

fn census_tables(conn: &Connection) -> Result<BTreeSet<String>> {
    let mut stmt =
        conn.prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut tables = BTreeSet::new();
    for row in rows {
        tables.insert(row?);
    }
    Ok(tables)
}

fn count_table_rows(conn: &Connection, table: &str) -> Result<u64> {
    // Only called with static allowlisted table names verified in `ALLOWED_NATIVE_TABLES`.
    let sql = match table {
        "memories" => "SELECT COUNT(*) FROM memories",
        "agents" => "SELECT COUNT(*) FROM agents",
        "embedding_cache" => "SELECT COUNT(*) FROM embedding_cache",
        "memory_meta" => "SELECT COUNT(*) FROM memory_meta",
        other => bail!("unexpected table count target '{other}'"),
    };
    let count: i64 = conn.query_row(sql, [], |row| row.get(0))?;
    u64::try_from(count).context("negative SQLite table row count")
}

fn inspect_table_columns(
    conn: &Connection,
    table: &str,
    allowed: &[&str],
    required: &[&str],
) -> Result<HashSet<String>> {
    let pragma = match table {
        "memories" => "PRAGMA table_info(memories)",
        "agents" => "PRAGMA table_info(agents)",
        other => bail!("unexpected table inspection target '{other}'"),
    };
    let mut stmt = conn.prepare(pragma)?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    let mut cols = HashSet::new();
    for row in rows {
        let col = row?;
        ensure!(
            allowed.contains(&col.as_str()),
            "refusing unsupported column '{col}' on native '{table}' table"
        );
        cols.insert(col);
    }
    for req in required {
        ensure!(
            cols.contains(*req),
            "native '{table}' table is missing required column '{req}'"
        );
    }
    Ok(cols)
}

fn load_agents_map(conn: &Connection) -> Result<HashMap<String, String>> {
    let mut stmt = conn.prepare("SELECT id, alias FROM agents ORDER BY rowid ASC")?;
    let mut rows = stmt.query([])?;
    let mut by_id = HashMap::new();
    let mut seen_aliases = HashSet::new();
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let alias: String = row.get(1)?;
        ensure!(
            !id.trim().is_empty() && !alias.trim().is_empty(),
            "native 'agents' table contains a blank id or alias"
        );
        ensure!(
            by_id.insert(id.clone(), alias.clone()).is_none(),
            "native 'agents' table contains duplicate agent id '{id}'"
        );
        ensure!(
            seen_aliases.insert(alias.clone()),
            "native 'agents' table contains duplicate agent alias '{alias}'"
        );
    }
    Ok(by_id)
}

fn build_memories_select_sql(cols: &HashSet<String>) -> String {
    let opt_col = |name: &str| {
        if cols.contains(name) {
            name.to_string()
        } else {
            format!("NULL AS {name}")
        }
    };
    let pinned_expr = if cols.contains("pinned") {
        "pinned"
    } else {
        "0 AS pinned"
    };
    format!(
        "SELECT id, key, content, category, created_at, {}, {}, {}, {}, {}, {}, {}, {}, {}, {} FROM memories ORDER BY rowid ASC",
        opt_col("embedding"),
        opt_col("updated_at"),
        opt_col("session_id"),
        opt_col("namespace"),
        opt_col("importance"),
        opt_col("superseded_by"),
        opt_col("agent_id"),
        opt_col("kind"),
        pinned_expr,
        opt_col("tenant_id"),
    )
}

fn legacy_key_prefix(key: &str) -> String {
    use std::fmt::Write as _;
    let mut prefix = String::from("zc-key-v1:");
    for byte in key.as_bytes() {
        let _ = write!(prefix, "{byte:02x}");
    }
    prefix.push(':');
    prefix
}

fn decode_legacy_physical_key(id: &str, physical: String) -> String {
    const TAG: &str = "zc-key-v1:";
    let Some(uuid) = id.strip_prefix(TAG) else {
        return physical;
    };
    let Some(rest) = physical.strip_prefix(TAG) else {
        return physical;
    };
    let Some((_, payload)) = rest.split_once(':') else {
        return physical;
    };
    let Ok((key, _session, nonce)) =
        serde_json::from_str::<(String, Option<String>, String)>(payload)
    else {
        return physical;
    };
    if nonce == uuid
        && uuid::Uuid::parse_str(uuid).is_ok()
        && physical.starts_with(&legacy_key_prefix(&key))
    {
        key
    } else {
        physical
    }
}

fn build_planned_row(
    row: &rusqlite::Row<'_>,
    agents_by_id: &HashMap<String, String>,
    superseded_rows: &mut u64,
    vector_rows: &mut u64,
    agent_aliases: &mut BTreeSet<String>,
) -> Result<PlannedRow> {
    let id: String = row.get(0)?;
    ensure!(
        !id.trim().is_empty(),
        "native 'memories' table contains a blank row id"
    );
    let raw_key: String = row.get(1)?;
    let key = decode_legacy_physical_key(&id, raw_key);
    ensure!(
        !key.trim().is_empty(),
        "native memory row '{id}' has a blank key"
    );
    let content: String = row.get(2)?;
    let raw_category: String = row.get(3)?;
    ensure!(
        !raw_category.trim().is_empty(),
        "native memory row '{id}' has a blank category"
    );
    let category = parse_category(&raw_category);

    let raw_created_at: String = row.get(4)?;
    let expected_created_instant = parse_native_timestamp_utc(&raw_created_at, &id, "created_at")?;
    let created_at_nanos = TachiMemory::format_utc_nanos(expected_created_instant);
    let created_at_millis = TachiMemory::format_utc_millis(expected_created_instant);
    let raw_embedding: Option<Vec<u8>> = row.get(5)?;
    let raw_updated_at: Option<String> = row.get(6)?;
    let updated_instant = match raw_updated_at.filter(|s| !s.trim().is_empty()) {
        Some(ts) => parse_native_timestamp_utc(&ts, &id, "updated_at")?,
        None => expected_created_instant,
    };
    let updated_at_nanos = TachiMemory::format_utc_nanos(updated_instant);
    let updated_at_millis = TachiMemory::format_utc_millis(updated_instant);
    let session_id: Option<String> = row.get(7)?;
    let raw_namespace: Option<String> = row.get(8)?;
    let namespace = raw_namespace
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "default".to_string());

    if namespace == crate::SOUL_NAMESPACE || key.starts_with(crate::SOUL_KEY_PREFIX) {
        bail!(
            "native memory row '{id}' belongs to reserved Soul authority and cannot be imported into the generic memory store"
        );
    }

    let raw_importance: Option<f64> = row.get(9)?;
    let importance = match raw_importance {
        Some(value) => {
            ensure!(
                value.is_finite(),
                "native memory row '{id}' has non-finite importance"
            );
            value
        }
        None => 0.5,
    };

    let raw_superseded_by: Option<String> = row.get(10)?;
    let superseded_by = match raw_superseded_by.filter(|s| !s.is_empty()) {
        Some(winner) => {
            ensure!(
                !winner.trim().is_empty(),
                "native memory row '{id}' has whitespace-only superseded_by target"
            );
            *superseded_rows += 1;
            Some(winner)
        }
        None => None,
    };

    let raw_agent_id: Option<String> = row.get(11)?;
    let agent_alias = match raw_agent_id {
        Some(agent_id) => {
            ensure!(
                !agent_id.trim().is_empty(),
                "native memory row '{id}' has blank agent_id"
            );
            let Some(alias) = agents_by_id.get(&agent_id) else {
                bail!("native memory row '{id}' references unmapped agent_id '{agent_id}'");
            };
            alias.clone()
        }
        None => "default".to_string(),
    };
    agent_aliases.insert(agent_alias.clone());

    let raw_kind: Option<String> = row.get(12)?;
    let kind = match raw_kind.filter(|s| !s.trim().is_empty()) {
        Some(json) => {
            let parsed = serde_json::from_str::<MemoryKind>(&json)
                .with_context(|| format!("native memory row '{id}' has malformed kind JSON"))?;
            Some(parsed)
        }
        None => None,
    };

    let raw_pinned: i64 = row.get(13)?;
    ensure!(
        matches!(raw_pinned, 0 | 1),
        "native memory row '{id}' has invalid pinned flag {raw_pinned}"
    );
    let pinned = raw_pinned != 0;
    let tenant_id: Option<String> = row.get(14)?;

    let vector = match raw_embedding {
        Some(blob) if !blob.is_empty() => {
            let decoded = decode_embedding_blob(&id, &blob)?;
            *vector_rows += 1;
            Some(decoded)
        }
        _ => None,
    };

    let conversation_session = if category == MemoryCategory::Conversation {
        session_id.as_deref()
    } else {
        None
    };
    let path = TachiMemory::scoped_storage_path(
        Some(&agent_alias),
        Some(&namespace),
        &category,
        &key,
        conversation_session,
    );
    let options = StoreOptions {
        namespace: Some(namespace.clone()),
        importance: Some(importance),
        kind: kind.clone(),
        pinned,
        tenant_id: tenant_id.clone(),
    };
    let mut metadata = TachiMemory::build_metadata(
        &key,
        &category,
        session_id.as_deref(),
        Some(&namespace),
        Some(&agent_alias),
        &options,
    );
    if let Some(obj) = metadata.as_object_mut() {
        obj.insert(
            METADATA_ZC_CREATED_AT.to_string(),
            Value::String(created_at_nanos.clone()),
        );
        obj.insert(
            METADATA_ZC_UPDATED_AT.to_string(),
            Value::String(updated_at_nanos.clone()),
        );
        if let Some(ref winner) = superseded_by {
            obj.insert(
                METADATA_SUPERSEDED_BY.to_string(),
                Value::String(winner.clone()),
            );
        }
    }

    let entry = portable_kernel::MemoryEntry {
        id: id.clone(),
        path,
        summary: content.chars().take(100).collect(),
        text: content.clone(),
        importance,
        timestamp: updated_at_millis,
        valid_from: created_at_millis,
        valid_until: None,
        category: TachiMemory::category_to_memcore(&category),
        topic: String::new(),
        keywords: Vec::new(),
        persons: Vec::new(),
        entities: Vec::new(),
        location: String::new(),
        source: "external:zeroclaw".into(),
        scope: TachiMemory::scope_for_namespace(Some(&namespace)),
        archived: false,
        access_count: 0,
        scored_count: 0,
        last_use_at: None,
        last_access: None,
        revision: 1,
        vector: vector.clone(),
        retention_policy: pinned.then(|| "pinned".to_string()),
        domain: None,
        metadata,
        recall_count: 0,
        query_diversity: 0,
        tier: portable_kernel::types::default_tier(),
    };

    // Pass `superseded_by: None` to memcore so ZeroClaw owns reversible
    // supersession solely through `metadata[METADATA_SUPERSEDED_BY]`.
    let import = PortableImportEntry {
        entry,
        created_at: created_at_nanos.clone(),
        updated_at: updated_at_nanos,
        superseded_by: None,
    };

    let expected = ExpectedNativeRow {
        id,
        key,
        content,
        category,
        timestamp: created_at_nanos,
        expected_created_instant,
        session_id,
        namespace,
        importance,
        superseded_by,
        kind,
        pinned,
        tenant_id,
        agent_alias,
        vector,
    };

    Ok(PlannedRow { import, expected })
}

fn parse_category(raw: &str) -> MemoryCategory {
    match raw {
        "core" => MemoryCategory::Core,
        "daily" => MemoryCategory::Daily,
        "conversation" => MemoryCategory::Conversation,
        other => MemoryCategory::Custom(other.to_string()),
    }
}

fn parse_native_timestamp_utc(raw: &str, row_id: &str, field: &str) -> Result<DateTime<Utc>> {
    let trimmed = raw.trim();
    ensure!(
        !trimmed.is_empty(),
        "native memory row '{row_id}' has blank {field} timestamp"
    );
    if let Ok(dt) = DateTime::parse_from_rfc3339(trimmed) {
        return Ok(dt.with_timezone(&Utc));
    }
    if let Ok(dt) = trimmed.parse::<DateTime<Utc>>() {
        return Ok(dt);
    }
    if let Ok(naive) = NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M:%S")
        .or_else(|_| NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M:%S%.f"))
    {
        return Ok(naive.and_utc());
    }
    bail!("native memory row '{row_id}' has invalid {field} timestamp '{raw}'")
}

fn decode_embedding_blob(row_id: &str, blob: &[u8]) -> Result<Vec<f32>> {
    ensure!(
        blob.len().is_multiple_of(4),
        "native memory row '{row_id}' has misaligned embedding byte length {}",
        blob.len()
    );
    let vec: Vec<f32> = blob
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect();
    ensure!(
        vec.len() == MEMCORE_VECTOR_DIM,
        "native memory row '{row_id}' has embedding dimension {} (expected {MEMCORE_VECTOR_DIM})",
        vec.len()
    );
    ensure!(
        vec.iter().all(|v| v.is_finite()),
        "native memory row '{row_id}' has non-finite embedding coordinate"
    );
    Ok(vec)
}

fn validate_native_target(
    path: &Path,
    plan: &NativeImportPlan,
    receipt_key: &str,
    receipt_json: &str,
    mut boundary: impl FnMut() -> Result<()>,
) -> Result<()> {
    boundary().context("before native target kernel reopen")?;
    {
        let store = open_store(
            path,
            KernelStoreKind::Memory,
            DbOpenContext::open_existing_deny(),
        )?;
        let stored = store
            .get_state_kv(RECEIPT_NAMESPACE, receipt_key)?
            .context("native migration receipt was not retained")?;
        ensure!(
            stored.0 == receipt_json && stored.1 == 1,
            "native migration receipt readback differs"
        );
        let stats = store.stats(false)?;
        ensure!(
            stats.total == plan.rows.len() as u64,
            "native migration target row count mismatch: expected {}, got {}",
            plan.rows.len(),
            stats.total
        );
        for row in &plan.rows {
            let fetched = store.get(&row.expected.id)?.with_context(|| {
                format!("imported row '{}' missing on readback", row.expected.id)
            })?;
            verify_vector_eq(
                &row.expected.id,
                fetched.vector.as_deref(),
                row.expected.vector.as_deref(),
            )?;
            let decoded = TachiMemory::memcore_to_zeroclaw(fetched, None);
            verify_decoded_row(&decoded, &row.expected)?;
        }
    }
    boundary().context("after native target kernel reopen")?;
    checkpoint(path)?;
    boundary().context("after native target readback checkpoint")?;
    let conn = open_read_only_snapshot(path)?;
    ensure!(
        conn.query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))? == "ok",
        "native migration target SQLite integrity check failed"
    );
    let mut foreign_keys = conn.prepare("PRAGMA foreign_key_check")?;
    ensure!(
        foreign_keys.query([])?.next()?.is_none(),
        "native migration target foreign-key verification failed"
    );
    drop(foreign_keys);
    let sql_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))
        .optional()?
        .unwrap_or(0);
    ensure!(
        sql_count == plan.rows.len() as i64,
        "native migration target SQL row count mismatch: expected {}, got {sql_count}",
        plan.rows.len()
    );
    conn.execute_batch("ROLLBACK")?;
    close_connection(conn)?;
    boundary().context("after native target SQLite readback")?;
    Ok(())
}

fn verify_vector_eq(row_id: &str, actual: Option<&[f32]>, expected: Option<&[f32]>) -> Result<()> {
    match (actual, expected) {
        (None, None) => Ok(()),
        (Some(a), Some(b))
            if a.len() == b.len()
                && a.iter()
                    .zip(b.iter())
                    .all(|(lhs, rhs)| lhs.to_bits() == rhs.to_bits()) =>
        {
            Ok(())
        }
        _ => bail!("native migration target vector mismatch for row '{row_id}'"),
    }
}

fn verify_decoded_row(
    actual: &crate::traits::MemoryEntry,
    expected: &ExpectedNativeRow,
) -> Result<()> {
    let actual_instant = DateTime::parse_from_rfc3339(&actual.timestamp)
        .with_context(|| {
            format!(
                "invalid migrated timestamp '{}' for row '{}'",
                actual.timestamp, expected.id
            )
        })?
        .with_timezone(&Utc);
    ensure!(
        actual_instant == expected.expected_created_instant,
        "native migration timestamp instant mismatch for row '{}': expected {:?}, got {:?}",
        expected.id,
        expected.expected_created_instant,
        actual_instant
    );
    ensure!(
        actual.id == expected.id
            && actual.key == expected.key
            && actual.content == expected.content
            && actual.category == expected.category
            && actual.timestamp == expected.timestamp
            && actual.session_id == expected.session_id
            && actual.namespace == expected.namespace
            && actual.importance.map(f64::to_bits) == Some(expected.importance.to_bits())
            && actual.superseded_by == expected.superseded_by
            && actual.kind == expected.kind
            && actual.pinned == expected.pinned
            && actual.tenant_id == expected.tenant_id
            && actual.agent_alias.as_deref() == Some(expected.agent_alias.as_str())
            && actual.agent_id.as_deref() == Some(expected.agent_alias.as_str()),
        "native migration readback parity mismatch for row '{}'",
        expected.id
    );
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::embeddings::EmbeddingProvider;
    use crate::traits::{ExportFilter, Memory, SemanticSubtype};
    use async_trait::async_trait;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;

    struct Fixed1024Embedder;

    #[async_trait]
    impl EmbeddingProvider for Fixed1024Embedder {
        fn name(&self) -> &str {
            "fixed-1024"
        }

        fn dimensions(&self) -> usize {
            MEMCORE_VECTOR_DIM
        }

        async fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
            Ok(texts
                .iter()
                .enumerate()
                .map(|(idx, text)| {
                    let seed = (text.len() + idx) as f32 * 0.001 + 0.1;
                    vec![seed; MEMCORE_VECTOR_DIM]
                })
                .collect())
        }
    }

    fn fixed_vector_blob(seed_offset: usize, content: &str) -> Vec<u8> {
        let seed = (content.len() + seed_offset) as f32 * 0.001 + 0.1;
        std::iter::repeat_n(seed, MEMCORE_VECTOR_DIM)
            .flat_map(f32::to_le_bytes)
            .collect()
    }

    fn build_rich_native_brain(dir: &Path) -> PathBuf {
        let memory_dir = dir.join("memory");
        std::fs::create_dir_all(&memory_dir).unwrap();
        let db_path = memory_dir.join("brain.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE agents (
                 id TEXT PRIMARY KEY,
                 alias TEXT NOT NULL UNIQUE,
                 created_at TEXT NOT NULL
             );
             CREATE TABLE memories (
                 id TEXT PRIMARY KEY,
                 key TEXT NOT NULL,
                 content TEXT NOT NULL,
                 category TEXT NOT NULL,
                 embedding BLOB,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL,
                 session_id TEXT,
                 namespace TEXT,
                 importance REAL,
                 superseded_by TEXT,
                 agent_id TEXT,
                 kind TEXT,
                 pinned INTEGER NOT NULL DEFAULT 0,
                 tenant_id TEXT
             );
             INSERT INTO agents (id, alias, created_at) VALUES
                 ('agent-alpha', 'alpha', '2026-01-01T00:00:00Z'),
                 ('agent-beta', 'beta', '2026-01-01T00:00:00Z');",
        )
        .unwrap();

        struct FixtureRow<'a> {
            id: &'a str,
            key: &'a str,
            content: &'a str,
            category: &'a str,
            ts: &'a str,
            session_id: Option<&'a str>,
            namespace: Option<&'a str>,
            importance: Option<f64>,
            superseded_by: Option<&'a str>,
            agent_id: Option<&'a str>,
            kind: Option<&'a str>,
            pinned: i64,
            tenant_id: Option<&'a str>,
        }

        let pref_kind =
            serde_json::to_string(&MemoryKind::Semantic(SemanticSubtype::Preference)).unwrap();
        let rows = [
            FixtureRow {
                id: "row-pref-theme",
                key: "pref_theme",
                content: "User prefers dark mode",
                category: "core",
                ts: "2026-01-01T00:00:01Z",
                session_id: None,
                namespace: Some("prefs"),
                importance: Some(0.9),
                superseded_by: None,
                agent_id: Some("agent-alpha"),
                kind: Some(pref_kind.as_str()),
                pinned: 1,
                tenant_id: Some("tenant-a"),
            },
            FixtureRow {
                id: "row-turn-1",
                key: "turn_note",
                content: "Session one summary",
                category: "conversation",
                ts: "2026-01-01T00:00:02Z",
                session_id: Some("sess-1"),
                namespace: None,
                importance: None,
                superseded_by: None,
                agent_id: Some("agent-beta"),
                kind: None,
                pinned: 0,
                tenant_id: None,
            },
            FixtureRow {
                id: "row-turn-2",
                key: "turn_note",
                content: "Session two summary",
                category: "conversation",
                ts: "2026-01-01T00:00:03Z",
                session_id: Some("sess-2"),
                namespace: None,
                importance: None,
                superseded_by: None,
                agent_id: Some("agent-beta"),
                kind: None,
                pinned: 0,
                tenant_id: None,
            },
            FixtureRow {
                id: "row-office-v1",
                key: "office_city",
                content: "The office is in Denver",
                category: "core",
                ts: "2026-01-01T00:00:04Z",
                session_id: None,
                namespace: None,
                importance: None,
                superseded_by: Some("row-office-v2"),
                agent_id: None,
                kind: None,
                pinned: 0,
                tenant_id: None,
            },
            FixtureRow {
                id: "row-office-v2",
                key: "office_city_v2",
                content: "The office moved to Austin",
                category: "core",
                ts: "2026-01-01T00:00:05Z",
                session_id: None,
                namespace: None,
                importance: None,
                superseded_by: None,
                agent_id: None,
                kind: None,
                pinned: 0,
                tenant_id: None,
            },
        ];

        for (idx, row) in rows.iter().enumerate() {
            let embedding = fixed_vector_blob(idx, row.content);
            conn.execute(
                "INSERT INTO memories (
                     id, key, content, category, embedding, created_at, updated_at,
                     session_id, namespace, importance, superseded_by, agent_id, kind, pinned, tenant_id
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                rusqlite::params![
                    row.id,
                    row.key,
                    row.content,
                    row.category,
                    embedding,
                    row.ts,
                    row.session_id,
                    row.namespace,
                    row.importance,
                    row.superseded_by,
                    row.agent_id,
                    row.kind,
                    row.pinned,
                    row.tenant_id,
                ],
            )
            .unwrap();
        }

        db_path
    }

    #[tokio::test]
    async fn migrate_native_preserves_multi_agent_typed_superseded_and_vectors_and_is_reversible() {
        let src_dir = tempfile::tempdir().unwrap();
        let dst_dir = tempfile::tempdir().unwrap();
        let source = build_rich_native_brain(src_dir.path());
        let destination = dst_dir.path().join("tachi.db");

        let source_before = source::census(&source).unwrap();
        let receipt = migrate_native_brain_store(&source, &destination, "captain@test").unwrap();

        assert_eq!(receipt.format, RECEIPT_FORMAT);
        assert_eq!(receipt.imported_rows, 5);
        assert_eq!(receipt.superseded_rows, 1);
        assert_eq!(receipt.vector_rows, 5);
        assert_eq!(
            receipt.agent_aliases,
            vec![
                "alpha".to_string(),
                "beta".to_string(),
                "default".to_string()
            ]
        );
        assert_eq!(source::census(&source).unwrap(), source_before);

        let mode = std::fs::metadata(&destination)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        for sidecar in ["-wal", "-shm", "-journal", ".identity.json"] {
            assert!(
                !source::suffixed(&destination, sidecar).exists(),
                "published target must not leave sidecar {sidecar}"
            );
        }
        assert!(source::suffixed(&destination, ".migration-marker").exists());

        // Open directly via TachiMemory at dst_dir (using `db_name` or placing
        // in dst_dir/memory/tachi.db — verify opening via KernelStoreKind + TachiMemory).
        let tachi_workspace = tempfile::tempdir().unwrap();
        let memory_dir = tachi_workspace.path().join("memory");
        std::fs::create_dir_all(&memory_dir).unwrap();
        let tachi_db = memory_dir.join("tachi.db");
        let second_receipt =
            migrate_native_brain_store(&source, &tachi_db, "captain@test").unwrap();
        assert_eq!(second_receipt.imported_rows, 5);

        let tachi = TachiMemory::with_embedder(
            "tachi",
            tachi_workspace.path(),
            Arc::new(Fixed1024Embedder),
            0.7,
            0.3,
        )
        .unwrap();

        let pref = tachi
            .get_for_agent("pref_theme", "alpha")
            .await
            .unwrap()
            .expect("migrated typed row for alpha");
        assert_eq!(pref.content, "User prefers dark mode");
        assert_eq!(pref.namespace, "prefs");
        assert!(pref.pinned);
        assert_eq!(pref.tenant_id.as_deref(), Some("tenant-a"));
        assert_eq!(
            pref.kind,
            Some(MemoryKind::Semantic(SemanticSubtype::Preference))
        );
        assert_eq!(pref.agent_alias.as_deref(), Some("alpha"));

        // Superseded loser stays hidden in list/recall/recent yet reachable by get/export/stats.
        assert!(
            tachi
                .list(None, None)
                .await
                .unwrap()
                .iter()
                .all(|e| e.key != "office_city")
        );
        assert_eq!(tachi.stats().await.unwrap().superseded_rows, 1);
        let loser = tachi.get("office_city").await.unwrap().unwrap();
        assert!(loser.superseded_by.is_some());

        // Reversibility proof: re-storing the superseded key restores visibility.
        tachi
            .store(
                "office_city",
                "Denver branch reopened",
                MemoryCategory::Core,
                None,
            )
            .await
            .unwrap();
        let restored = tachi.get("office_city").await.unwrap().unwrap();
        assert!(restored.superseded_by.is_none());
        assert_eq!(tachi.stats().await.unwrap().superseded_rows, 0);
        assert!(
            tachi
                .list(None, None)
                .await
                .unwrap()
                .iter()
                .any(|e| e.key == "office_city")
        );

        let exported = tachi.export(&ExportFilter::default()).await.unwrap();
        assert_eq!(exported.len(), 5);
    }

    #[test]
    fn migrate_native_refuses_occupied_target_and_sidecars() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("brain.db");
        create_minimal_native_db(&source);

        let existing = dir.path().join("occupied.db");
        std::fs::write(&existing, b"already-here").unwrap();
        let err = migrate_native_brain_store(&source, &existing, "op").unwrap_err();
        assert!(
            err.to_string().contains("already occupied"),
            "unexpected error: {err}"
        );

        let sidecar_target = dir.path().join("with-sidecar.db");
        std::fs::write(source::suffixed(&sidecar_target, "-wal"), b"stale").unwrap();
        let err = migrate_native_brain_store(&source, &sidecar_target, "op").unwrap_err();
        assert!(
            err.to_string().contains("already occupied") && err.to_string().contains("-wal"),
            "unexpected error: {err}"
        );
        assert!(!sidecar_target.exists());
    }

    #[test]
    fn migrate_native_refuses_unmapped_agent_id() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("brain.db");
        let conn = Connection::open(&source).unwrap();
        conn.execute_batch(
            "CREATE TABLE agents (id TEXT PRIMARY KEY, alias TEXT NOT NULL UNIQUE, created_at TEXT NOT NULL);
             CREATE TABLE memories (
                 id TEXT PRIMARY KEY, key TEXT NOT NULL, content TEXT NOT NULL,
                 category TEXT NOT NULL, created_at TEXT NOT NULL, agent_id TEXT
             );
             INSERT INTO memories (id, key, content, category, created_at, agent_id)
             VALUES ('m1', 'k1', 'c1', 'core', '2026-01-01T00:00:00Z', 'missing-uuid');",
        )
        .unwrap();
        drop(conn);

        let dest = dir.path().join("tachi.db");
        let err = migrate_native_brain_store(&source, &dest, "op").unwrap_err();
        assert!(err.to_string().contains("unmapped agent_id"));
        assert!(!dest.exists());
    }

    #[test]
    fn migrate_native_refuses_soul_rows() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("brain.db");
        let conn = Connection::open(&source).unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                 id TEXT PRIMARY KEY, key TEXT NOT NULL, content TEXT NOT NULL,
                 category TEXT NOT NULL, created_at TEXT NOT NULL, namespace TEXT
             );
             INSERT INTO memories (id, key, content, category, created_at, namespace)
             VALUES ('s1', 'soul::agent::disposition', 'secret', 'core', '2026-01-01T00:00:00Z', 'soul');",
        )
        .unwrap();
        drop(conn);

        let dest = dir.path().join("tachi.db");
        let err = migrate_native_brain_store(&source, &dest, "op").unwrap_err();
        assert!(err.to_string().contains("Soul"));
        assert!(!dest.exists());
    }

    #[test]
    fn migrate_native_refuses_foreign_table_and_unknown_column() {
        let dir = tempfile::tempdir().unwrap();
        let foreign_src = dir.path().join("foreign.db");
        create_minimal_native_db(&foreign_src);
        let conn = Connection::open(&foreign_src).unwrap();
        conn.execute_batch("CREATE TABLE vault_entries (id TEXT PRIMARY KEY);")
            .unwrap();
        drop(conn);

        let dest = dir.path().join("out1.db");
        let err = migrate_native_brain_store(&foreign_src, &dest, "op").unwrap_err();
        assert!(err.to_string().contains("unsupported table"));
        assert!(!dest.exists());

        let unknown_col_src = dir.path().join("unknown-col.db");
        create_minimal_native_db(&unknown_col_src);
        let conn = Connection::open(&unknown_col_src).unwrap();
        conn.execute_batch("ALTER TABLE memories ADD COLUMN extra_secret TEXT;")
            .unwrap();
        drop(conn);

        let dest2 = dir.path().join("out2.db");
        let err = migrate_native_brain_store(&unknown_col_src, &dest2, "op").unwrap_err();
        assert!(err.to_string().contains("unsupported column"));
        assert!(!dest2.exists());
    }

    #[test]
    fn migrate_native_refuses_malformed_kind_and_bad_vectors() {
        let dir = tempfile::tempdir().unwrap();

        // Malformed kind JSON
        let bad_kind = dir.path().join("bad-kind.db");
        let conn = Connection::open(&bad_kind).unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                 id TEXT PRIMARY KEY, key TEXT NOT NULL, content TEXT NOT NULL,
                 category TEXT NOT NULL, created_at TEXT NOT NULL, kind TEXT
             );
             INSERT INTO memories (id, key, content, category, created_at, kind)
             VALUES ('k1', 'key1', 'text', 'core', '2026-01-01T00:00:00Z', '{not-json');",
        )
        .unwrap();
        drop(conn);
        let err =
            migrate_native_brain_store(&bad_kind, &dir.path().join("d1.db"), "op").unwrap_err();
        assert!(err.to_string().contains("malformed kind JSON"));

        // Wrong vector dimension (3 floats instead of 1024)
        let wrong_dim = dir.path().join("wrong-dim.db");
        let conn = Connection::open(&wrong_dim).unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                 id TEXT PRIMARY KEY, key TEXT NOT NULL, content TEXT NOT NULL,
                 category TEXT NOT NULL, created_at TEXT NOT NULL, embedding BLOB
             );",
        )
        .unwrap();
        let short_blob: Vec<u8> = [1.0f32, 2.0, 3.0]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect();
        conn.execute(
            "INSERT INTO memories (id, key, content, category, created_at, embedding)
             VALUES ('v1', 'key1', 'text', 'core', '2026-01-01T00:00:00Z', ?1)",
            rusqlite::params![short_blob],
        )
        .unwrap();
        drop(conn);
        let err =
            migrate_native_brain_store(&wrong_dim, &dir.path().join("d2.db"), "op").unwrap_err();
        assert!(err.to_string().contains("dimension 3"));

        // Non-finite vector value (NaN at valid 1024 length)
        let nan_vec = dir.path().join("nan-vec.db");
        let conn = Connection::open(&nan_vec).unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                 id TEXT PRIMARY KEY, key TEXT NOT NULL, content TEXT NOT NULL,
                 category TEXT NOT NULL, created_at TEXT NOT NULL, embedding BLOB
             );",
        )
        .unwrap();
        let mut floats = vec![0.1f32; MEMCORE_VECTOR_DIM];
        floats[7] = f32::NAN;
        let nan_blob: Vec<u8> = floats.into_iter().flat_map(f32::to_le_bytes).collect();
        conn.execute(
            "INSERT INTO memories (id, key, content, category, created_at, embedding)
             VALUES ('v2', 'key2', 'text', 'core', '2026-01-01T00:00:00Z', ?1)",
            rusqlite::params![nan_blob],
        )
        .unwrap();
        drop(conn);
        let err =
            migrate_native_brain_store(&nan_vec, &dir.path().join("d3.db"), "op").unwrap_err();
        assert!(err.to_string().contains("non-finite"));
    }

    #[test]
    fn migrate_native_refuses_concurrent_source_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("brain.db");
        create_minimal_native_db(&source);
        let dest = dir.path().join("tachi.db");

        let err = migrate_native_inner(
            &source,
            &dest,
            "op",
            || {
                let conn = Connection::open(&source)?;
                conn.execute(
                    "INSERT INTO memories (id, key, content, category, created_at)
                     VALUES ('r2', 'k2', 'mutated', 'core', '2026-01-02T00:00:00Z')",
                    [],
                )?;
                Ok(())
            },
            |_| Ok(()),
        )
        .unwrap_err();
        assert!(err.to_string().contains("source file"));
        assert!(!dest.exists());
    }

    #[tokio::test]
    async fn migrate_native_preserves_submillisecond_timestamps_and_recent_order() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("subms.db");
        let conn = Connection::open(&source).unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                 id TEXT PRIMARY KEY,
                 key TEXT NOT NULL,
                 content TEXT NOT NULL,
                 category TEXT NOT NULL,
                 created_at TEXT NOT NULL
             );
             INSERT INTO memories (id, key, content, category, created_at) VALUES
                 ('b-int', 'k-int', 'whole second note', 'core', '2026-01-01T00:00:00Z'),
                 ('a-older', 'k-old', 'older subms note', 'core', '2026-01-01T00:00:00.123100+00:00'),
                 ('z-newer', 'k-new', 'newer subms note', 'core', '2026-01-01T00:00:00.123900+00:00');",
        )
        .unwrap();
        drop(conn);

        let dst_ws = dir.path().join("dst");
        std::fs::create_dir_all(dst_ws.join("memory")).unwrap();
        let dest = dst_ws.join("memory").join("tachi.db");
        migrate_native_brain_store(&source, &dest, "captain@test").unwrap();

        let tachi = TachiMemory::new("tachi", &dst_ws).unwrap();
        let recent = crate::traits::Memory::recall(&tachi, "", 3, None, None, None)
            .await
            .unwrap();
        let ids: Vec<&str> = recent.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(ids, vec!["z-newer", "a-older", "b-int"]);
        assert_eq!(recent[0].timestamp, "2026-01-01T00:00:00.123900000Z");
        assert_eq!(recent[1].timestamp, "2026-01-01T00:00:00.123100000Z");
        assert_eq!(recent[2].timestamp, "2026-01-01T00:00:00.000000000Z");
    }

    #[tokio::test]
    async fn migrated_rows_preserve_updated_at_recency_and_created_at_window() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("updated_recency.db");
        let conn = Connection::open(&source).unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                 id TEXT PRIMARY KEY,
                 key TEXT NOT NULL,
                 content TEXT NOT NULL,
                 category TEXT NOT NULL,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL
             );
             INSERT INTO memories (id, key, content, category, created_at, updated_at) VALUES
                 ('row-a', 'k-a', 'created earlier updated latest', 'core', '2026-07-08T10:00:00.123900000Z', '2026-07-10T10:00:00.500Z'),
                 ('row-b', 'k-b', 'created later updated middle', 'core', '2026-07-09T10:00:00.200Z', '2026-07-09T10:00:00.200Z');",
        )
        .unwrap();
        drop(conn);

        let dst_ws = dir.path().join("dst");
        std::fs::create_dir_all(dst_ws.join("memory")).unwrap();
        let dest = dst_ws.join("memory").join("tachi.db");
        migrate_native_brain_store(&source, &dest, "captain@test").unwrap();

        let tachi = TachiMemory::new("tachi", &dst_ws).unwrap();
        let recent = crate::traits::Memory::recall(&tachi, "", 2, None, None, None)
            .await
            .unwrap();
        let ids: Vec<&str> = recent.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["row-a", "row-b"],
            "recent recall must order by updated_at DESC while preserving created_at on MemoryEntry"
        );
        assert_eq!(recent[0].timestamp, "2026-07-08T10:00:00.123900000Z");

        let bounded =
            crate::traits::Memory::recall(&tachi, "", 2, None, None, Some("2026-07-08T23:59:59Z"))
                .await
                .unwrap();
        assert_eq!(
            bounded
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            vec!["row-a"],
            "until filter on created_at must retain rows updated after until"
        );

        crate::traits::Memory::supersede(&tachi, &["row-a".to_string()], "row-b")
            .await
            .unwrap();
        let reloaded = crate::traits::Memory::get(&tachi, "k-a")
            .await
            .unwrap()
            .expect("superseded row still readable via get");
        assert_eq!(reloaded.timestamp, "2026-07-08T10:00:00.123900000Z");
        assert_eq!(reloaded.superseded_by.as_deref(), Some("row-b"));
    }

    #[tokio::test]
    async fn migrated_supersede_and_window_preserve_verbatim_body_vectors_and_rfc3339_bounds() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("verbatim_supersede.db");
        let verbatim_body = "  <think>keep verbatim</think>\n  exact migrated payload\n";
        let vec_values: Vec<f32> = (0..1024).map(|i| (i as f32 + 1.0) / 2048.0).collect();
        let vec_bytes: Vec<u8> = vec_values.iter().flat_map(|f| f.to_le_bytes()).collect();
        let conn = Connection::open(&source).unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                 id TEXT PRIMARY KEY,
                 key TEXT NOT NULL,
                 content TEXT NOT NULL,
                 category TEXT NOT NULL,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL,
                 embedding BLOB
             );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memories (id, key, content, category, created_at, updated_at, embedding)
             VALUES (?1, ?2, ?3, 'core', '2026-07-08T10:00:00.123Z', '2026-07-10T10:00:00.500Z', ?4)",
            rusqlite::params!["row-v", "k-v", verbatim_body, vec_bytes],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memories (id, key, content, category, created_at, updated_at, embedding)
             VALUES ('row-w', 'k-w', 'winner replacement', 'core', '2026-07-09T10:00:00.000Z', '2026-07-09T10:00:00.000Z', NULL)",
            [],
        )
        .unwrap();
        drop(conn);

        let dst_ws = dir.path().join("dst");
        std::fs::create_dir_all(dst_ws.join("memory")).unwrap();
        let dest = dst_ws.join("memory").join("tachi.db");
        migrate_native_brain_store(&source, &dest, "captain@test").unwrap();

        let tachi = TachiMemory::new("tachi", &dst_ws).unwrap();
        for (since, until) in [
            ("2026-07-08T10:00:00.123Z", "2026-07-08T10:00:00.123Z"),
            (
                "2026-07-08T18:00:00.123+08:00",
                "2026-07-08T18:00:00.123+08:00",
            ),
        ] {
            let recent =
                crate::traits::Memory::recall(&tachi, "", 5, None, Some(since), Some(until))
                    .await
                    .unwrap();
            assert_eq!(
                recent.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
                vec!["row-v"],
                "recent recall must honor inclusive RFC3339 instant bounds for {since}..={until}"
            );
            let keyword = crate::traits::Memory::recall(
                &tachi,
                "migrated payload",
                5,
                None,
                Some(since),
                Some(until),
            )
            .await
            .unwrap();
            assert_eq!(
                keyword.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
                vec!["row-v"],
                "keyword recall must honor inclusive RFC3339 instant bounds for {since}..={until}"
            );
            let exported = crate::traits::Memory::export(
                &tachi,
                &crate::traits::ExportFilter {
                    since: Some(since.into()),
                    until: Some(until.into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            assert_eq!(
                exported.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
                vec!["row-v"],
                "export must honor inclusive RFC3339 instant bounds for {since}..={until}"
            );
        }

        crate::traits::Memory::supersede(&tachi, &["row-v".to_string()], "row-w")
            .await
            .unwrap();
        let got = crate::traits::Memory::get(&tachi, "k-v")
            .await
            .unwrap()
            .expect("superseded row remains readable via get");
        assert_eq!(got.content, verbatim_body);
        assert_eq!(got.timestamp, "2026-07-08T10:00:00.123000000Z");
        assert_eq!(got.superseded_by.as_deref(), Some("row-w"));

        let raw_entry = tachi
            .store_handle()
            .lock()
            .get("row-v")
            .unwrap()
            .expect("raw row-v");
        assert_eq!(raw_entry.text, verbatim_body);
        assert_eq!(raw_entry.vector.as_deref(), Some(vec_values.as_slice()));
    }

    fn create_minimal_native_db(path: &Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                 id TEXT PRIMARY KEY,
                 key TEXT NOT NULL,
                 content TEXT NOT NULL,
                 category TEXT NOT NULL,
                 created_at TEXT NOT NULL
             );
             INSERT INTO memories (id, key, content, category, created_at)
             VALUES ('r1', 'k1', 'hello', 'core', '2026-01-01T00:00:00Z');",
        )
        .unwrap();
    }
}
