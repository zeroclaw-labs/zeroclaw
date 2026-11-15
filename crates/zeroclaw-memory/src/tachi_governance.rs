//! Tachi / memcore light-sleep governance (feature `tachi`).
//!
//! Driven from the memory factory hygiene cadence
//! ([`crate::create_memory_with_storage_and_routes`]) and from
//! [`Memory::run_light_sleep_governance`](zeroclaw_api::memory_traits::Memory::run_light_sleep_governance)
//! on a live [`crate::TachiMemory`] handle. Uses memcore public APIs only —
//! no direct SQL into the kernel schema, and no second DB open on production
//! paths.

use parking_lot::Mutex;
use portable_kernel::{MemoryEntry, MemoryStore, NEAR_DUP_RAW_SCAN_CAP, near_duplicate_raw_pairs};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Near-dup Jaccard threshold for light-sleep governance (Sigil parity: 0.9).
///
/// Intentionally **not** [`zeroclaw_config::schema::MemoryConfig::dedup_jaccard_threshold`]:
/// that knob is write-time whitespace Jaccard, while governance uses memcore's
/// tokenize Jaccard over full text. Archival is also semi-irreversible, so we
/// keep a fixed Sigil-aligned floor rather than overloading the write-time key
/// (no speculative new config key — AGENTS.md).
const NEAR_DUP_GOVERNANCE_THRESHOLD: f64 = 0.9;

/// Report counters from one governance pass (observability only — not persisted).
///
/// SSOT: this is the source of truth for per-call counters; it is not a cache of
/// store or config state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TachiGovernanceReport {
    pub near_dup_archived: usize,
    pub near_dup_survivors_updated: usize,
    pub promoted: usize,
    pub stale_archived: usize,
}

/// Run tachi light-sleep governance on the live store handle.
///
/// Production paths must pass the same [`MemoryStore`] owned by [`crate::TachiMemory`]
/// — never open a second connection to the same path.
pub fn run_tachi_governance(store: &mut MemoryStore) -> anyhow::Result<TachiGovernanceReport> {
    let mut report = TachiGovernanceReport::default();
    let near = run_near_dup_light_sleep(store, NEAR_DUP_GOVERNANCE_THRESHOLD)?;
    report.near_dup_archived = near.0;
    report.near_dup_survivors_updated = near.1;

    // memcore >= 1.9.1 gates batch promotion on the recall regime: under the
    // new provenance-recency default it is a deliberate no-op (promotion moved
    // to an inline gate on the recall path). This governance pass IS zeroclaw's
    // batch promotion surface — its contract (one diversely-recalled raw row
    // gets consolidated per sweep, asserted by the tests below) predates that
    // regime, so run it under the legacy regime explicitly rather than
    // silently losing promotion.
    let legacy_recall = portable_kernel::RecallConfig {
        use_provenance_recency: false,
        ..portable_kernel::RecallConfig::default()
    };
    report.promoted = store
        .promote_diversely_recalled_raw_memories(&legacy_recall)
        .map_err(|e| anyhow::Error::msg(format!("tachi governance: promote failed: {e}")))?;

    report.stale_archived = store
        .archive_stale_low_value_memories()
        .map_err(|e| anyhow::Error::msg(format!("tachi governance: stale archive failed: {e}")))?;

    Ok(report)
}

/// Lock the shared store and run governance (same handle as [`crate::TachiMemory`]).
pub fn run_tachi_governance_on_handle(
    store: &Arc<Mutex<MemoryStore>>,
) -> anyhow::Result<TachiGovernanceReport> {
    let mut guard = store.lock();
    run_tachi_governance(&mut guard)
}

// Partition on the same metadata keys the TachiMemory write path uses —
// shared consts so a rename cannot drift between store and governance.
use crate::tachi::{
    METADATA_SUPERSEDED_BY, METADATA_ZC_AGENT as META_ZC_AGENT,
    METADATA_ZC_CATEGORY as META_ZC_CATEGORY, METADATA_ZC_NAMESPACE as META_ZC_NAMESPACE,
    TachiMemory,
};
use portable_kernel::store::scoped_read::{MetadataTextFilter, RecentMemoryQuery, TextMembership};

/// Scope key for near-dup partitions. Conversation rows never merge across
/// sessions or into a global fact; non-Conversation session provenance does not
/// split the global partition.
///
/// Missing metadata fields are `None` and form their own bucket — a metadata-less
/// row must not merge with a `default` / named-agent row.
#[derive(Hash, PartialEq, Eq)]
struct NearDupScope {
    agent: Option<String>,
    namespace: Option<String>,
    // Outer None: global. Some(None): legacy Conversation without a session.
    conversation_session: Option<Option<String>>,
}

fn near_dup_scope_key(entry: &MemoryEntry) -> NearDupScope {
    let agent = entry
        .metadata
        .get(META_ZC_AGENT)
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let namespace = entry
        .metadata
        .get(META_ZC_NAMESPACE)
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let conversation_session = (entry
        .metadata
        .get(META_ZC_CATEGORY)
        .and_then(|v| v.as_str())
        == Some("conversation"))
    .then(|| TachiMemory::conversation_session(&entry.metadata).map(str::to_owned));
    NearDupScope {
        agent,
        namespace,
        conversation_session,
    }
}

/// Near-dup light-sleep: collapse connected components of raw-tier text twins
/// into one survivor (higher importance, then newer), merge keywords, archive
/// sources. Mirrors Sigil consolidate star-collapse ordering.
///
/// **Scope rule (unattended vs Sigil):** Sigil's `near_dup_merge` is a
/// human-reviewed proposal flow. ZeroClaw governance auto-applies archival, so
/// merges must stay inside one agent, namespace and Conversation scope.
/// Crossing namespaces would drop a `user:kyle` row from
/// `recall_namespaced("user:kyle")` after fold-into-`default`; crossing agents
/// would fold one RomanBath character's memory into another's. Crossing a
/// Conversation session could archive the only fact visible to that session.
///
/// **Scan window:** the intake list queries `/agents` via
/// [`MemoryStore::list_recent_matching`] (`timestamp DESC, id ASC`, bounded to
/// [`NEAR_DUP_RAW_SCAN_CAP`] active rows with soft-superseded entries excluded
/// in SQL before `LIMIT`); only the O(n²) pair scan runs per partition.
fn run_near_dup_light_sleep(
    store: &mut MemoryStore,
    threshold: f64,
) -> anyhow::Result<(usize, usize)> {
    let scan_limit = NEAR_DUP_RAW_SCAN_CAP.max(64);
    let mut query = RecentMemoryQuery::new("/agents", scan_limit);
    query.filters.push(MetadataTextFilter {
        field: METADATA_SUPERSEDED_BY.into(),
        values: vec![String::new()],
        membership: TextMembership::AnyOf,
        missing_value: Some(String::new()),
    });
    let mut entries = store
        .list_recent_matching(&query)
        .map_err(|e| anyhow::Error::msg(format!("tachi governance: list failed: {e}")))?;
    entries.retain(|entry| !TachiMemory::is_soft_superseded(entry));
    if entries.len() < 2 {
        return Ok((0, 0));
    }

    let mut partitions: HashMap<NearDupScope, Vec<MemoryEntry>> = HashMap::new();
    for entry in entries {
        partitions
            .entry(near_dup_scope_key(&entry))
            .or_default()
            .push(entry);
    }

    let mut archived = 0usize;
    let mut survivors_updated = 0usize;
    for partition in partitions.into_values() {
        if partition.len() < 2 {
            continue;
        }
        let (a, s) = collapse_near_dups_in_partition(store, &partition, threshold)?;
        archived += a;
        survivors_updated += s;
    }
    Ok((archived, survivors_updated))
}

/// Run memcore near-dup + star-collapse within one admitted memory scope.
fn collapse_near_dups_in_partition(
    store: &mut MemoryStore,
    entries: &[MemoryEntry],
    threshold: f64,
) -> anyhow::Result<(usize, usize)> {
    let pairs = near_duplicate_raw_pairs(entries, threshold);
    if pairs.is_empty() {
        return Ok((0, 0));
    }

    let mut parent: Vec<usize> = (0..entries.len()).collect();
    let find = |parent: &mut [usize], mut x: usize| -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    };
    for (left, right, _) in &pairs {
        let root_left = find(&mut parent, *left);
        let root_right = find(&mut parent, *right);
        if root_left != root_right {
            parent[root_right] = root_left;
        }
    }

    let mut components: HashMap<usize, Vec<usize>> = HashMap::new();
    for index in 0..entries.len() {
        let root = find(&mut parent, index);
        components.entry(root).or_default().push(index);
    }

    let mut archived = 0usize;
    let mut survivors_updated = 0usize;
    for mut members in components.into_values() {
        if members.len() < 2 {
            continue;
        }
        // Only raw members participate (pairs already filter raw, but a
        // component can only contain indices that appeared in an edge).
        members.retain(|&idx| entries[idx].tier.eq_ignore_ascii_case("raw"));
        if members.len() < 2 {
            continue;
        }
        let survivor_idx = pick_survivor_index(entries, &members);
        let mut survivor = entries[survivor_idx].clone();
        let mut keyword_set: HashSet<String> = survivor.keywords.iter().cloned().collect();
        let mut source_ids = Vec::new();
        for &idx in &members {
            if idx == survivor_idx {
                continue;
            }
            for kw in &entries[idx].keywords {
                keyword_set.insert(kw.clone());
            }
            source_ids.push(entries[idx].id.clone());
        }
        let mut merged_keywords: Vec<String> = keyword_set.into_iter().collect();
        merged_keywords.sort();
        // Crash-safety invariant: upsert the survivor (keyword merge) *before*
        // archiving sources. A crash between upsert and archive leaves sources
        // unarchived and re-processable on the next governance pass; the reverse
        // order could archive twins while losing merged keywords.
        if merged_keywords != survivor.keywords {
            survivor.keywords = merged_keywords;
            store.upsert(&survivor).map_err(|e| {
                anyhow::Error::msg(format!("tachi governance: survivor upsert: {e}"))
            })?;
            survivors_updated += 1;
        }
        for id in source_ids {
            if store
                .archive_memory(&id)
                .map_err(|e| anyhow::Error::msg(format!("tachi governance: archive: {e}")))?
            {
                archived += 1;
            }
        }
    }
    Ok((archived, survivors_updated))
}

fn pick_survivor_index(entries: &[MemoryEntry], members: &[usize]) -> usize {
    let mut best = members[0];
    for &candidate in members.iter().skip(1) {
        if is_better_survivor(&entries[candidate], &entries[best]) {
            best = candidate;
        }
    }
    best
}

/// Higher importance wins; on tie prefer newer timestamp, then higher id.
fn is_better_survivor(a: &MemoryEntry, b: &MemoryEntry) -> bool {
    match a.importance.partial_cmp(&b.importance) {
        Some(std::cmp::Ordering::Greater) => true,
        Some(std::cmp::Ordering::Less) => false,
        _ => match TachiMemory::effective_updated_sort_key(a)
            .cmp(&TachiMemory::effective_updated_sort_key(b))
        {
            std::cmp::Ordering::Greater => true,
            std::cmp::Ordering::Less => false,
            std::cmp::Ordering::Equal => a.id > b.id,
        },
    }
}

#[cfg(all(test, feature = "tachi"))]
mod tests {
    use super::*;
    use crate::create_memory_with_storage_and_routes;
    use crate::tachi::TachiMemory;
    use crate::traits::{Memory, MemoryCategory, StoreOptions};
    use std::sync::Arc;
    use tempfile::TempDir;
    use zeroclaw_config::schema::{ActiveStorage, MemoryConfig};

    fn tachi_config() -> MemoryConfig {
        MemoryConfig {
            backend: "tachi".into(),
            hygiene_enabled: true,
            ..MemoryConfig::default()
        }
    }

    fn seed_raw(
        store: &mut MemoryStore,
        id: &str,
        path: &str,
        text: &str,
        importance: f64,
        keywords: &[&str],
        tier: &str,
    ) {
        let now = chrono::Local::now().to_rfc3339();
        let entry = MemoryEntry {
            id: id.into(),
            path: path.into(),
            summary: text.chars().take(80).collect(),
            text: text.into(),
            importance,
            timestamp: now.clone(),
            valid_from: now,
            valid_until: None,
            category: "fact".into(),
            topic: String::new(),
            keywords: keywords.iter().map(|s| (*s).to_string()).collect(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: "zeroclaw-test".into(),
            scope: "general".into(),
            archived: false,
            access_count: 0,
            scored_count: 0,
            last_use_at: None,
            last_access: None,
            revision: 1,
            vector: None,
            retention_policy: None,
            domain: None,
            metadata: serde_json::json!({}),
            recall_count: 0,
            query_diversity: 0,
            tier: tier.into(),
        };
        store.upsert(&entry).expect("seed upsert");
    }

    /// Exercises the factory hygiene-cadence wiring in
    /// `create_memory_with_storage_and_routes` (not `run_tachi_governance` alone).
    /// Deleting the `run_light_sleep_governance` one-liner there turns this RED.
    #[test]
    fn near_dup_merge_collapses_transitive_raw_chain_via_factory_hygiene() {
        let tmp = TempDir::new().unwrap();
        // Seed on a short-lived handle, then drop so the factory owns the live store.
        {
            let mem = TachiMemory::new("tachi", tmp.path()).unwrap();
            let mut store = mem.store_handle().lock();
            let base = "The operator prefers concise Rust answers for equity trading notes";
            seed_raw(
                &mut store,
                "raw-a",
                "/agents/default/ns/core/a",
                base,
                0.5,
                &["alpha"],
                "raw",
            );
            seed_raw(
                &mut store,
                "raw-b",
                "/agents/default/ns/core/b",
                &format!("{base} today"),
                0.9,
                &["bravo"],
                "raw",
            );
            seed_raw(
                &mut store,
                "raw-c",
                "/agents/default/ns/core/c",
                &format!("{base} always"),
                0.6,
                &["charlie"],
                "raw",
            );
        }

        // Live cadence entry point — factory constructs TachiMemory and runs
        // light-sleep when hygiene is due (no state file => due).
        let _mem = create_memory_with_storage_and_routes(
            &tachi_config(),
            &[],
            ActiveStorage::None,
            tmp.path(),
            None,
            None,
        )
        .expect("factory tachi");

        let mem = TachiMemory::new("tachi", tmp.path()).unwrap();
        let store = mem.store_handle().lock();
        let survivor = store.get("raw-b").unwrap().expect("survivor");
        assert!(!survivor.archived);
        assert!(survivor.keywords.iter().any(|k| k == "alpha"));
        assert!(survivor.keywords.iter().any(|k| k == "bravo"));
        assert!(survivor.keywords.iter().any(|k| k == "charlie"));
        assert!(
            store
                .get_with_options("raw-a", true)
                .unwrap()
                .unwrap()
                .archived
        );
        assert!(
            store
                .get_with_options("raw-c", true)
                .unwrap()
                .unwrap()
                .archived
        );
    }

    #[test]
    fn near_dup_leaves_non_raw_tiers_untouched() {
        let tmp = TempDir::new().unwrap();
        {
            let mem = TachiMemory::new("tachi", tmp.path()).unwrap();
            let mut store = mem.store_handle().lock();
            let text = "Shared consolidated preference text about Rust tooling";
            seed_raw(
                &mut store,
                "cons-a",
                "/agents/default/ns/core/cons_a",
                text,
                0.9,
                &[],
                "consolidated",
            );
            seed_raw(
                &mut store,
                "cons-b",
                "/agents/default/ns/core/cons_b",
                &format!("{text} extra"),
                0.5,
                &[],
                "consolidated",
            );
            seed_raw(
                &mut store,
                "raw-twin",
                "/agents/default/ns/core/raw_twin",
                text,
                0.4,
                &[],
                "raw",
            );
        }

        let report = {
            let mem = TachiMemory::new("tachi", tmp.path()).unwrap();
            mem.run_light_sleep_governance_report().unwrap()
        };
        assert_eq!(
            report.near_dup_archived, 0,
            "consolidated twins must not archive via near-dup: {report:?}"
        );

        let mem = TachiMemory::new("tachi", tmp.path()).unwrap();
        let store = mem.store_handle().lock();
        assert!(!store.get("cons-a").unwrap().expect("cons-a").archived);
        assert!(!store.get("cons-b").unwrap().expect("cons-b").archived);
        assert!(
            !store
                .get("raw-twin")
                .unwrap()
                .expect("raw-twin alone has no raw pair")
                .archived
        );
    }

    #[test]
    fn promote_and_stale_archive_smoke() {
        let tmp = TempDir::new().unwrap();
        let mem = TachiMemory::new("tachi", tmp.path()).unwrap();
        {
            let mut store = mem.store_handle().lock();
            let now = chrono::Local::now().to_rfc3339();
            let promotable = MemoryEntry {
                id: "promo".into(),
                path: "/agents/default/ns/core/promo".into(),
                summary: "promotable".into(),
                text: "Diversely recalled raw note for promotion smoke".into(),
                importance: 0.8,
                timestamp: now.clone(),
                valid_from: now.clone(),
                valid_until: None,
                category: "fact".into(),
                topic: String::new(),
                keywords: Vec::new(),
                persons: Vec::new(),
                entities: Vec::new(),
                location: String::new(),
                source: "zeroclaw-test".into(),
                scope: "general".into(),
                archived: false,
                access_count: 0,
                scored_count: 0,
                last_use_at: None,
                last_access: None,
                revision: 1,
                vector: None,
                retention_policy: None,
                domain: None,
                metadata: serde_json::json!({}),
                recall_count: 3,
                query_diversity: 3,
                tier: "raw".into(),
            };
            store.upsert(&promotable).unwrap();

            let mut stale = promotable.clone();
            stale.id = "stale".into();
            stale.path = "/agents/default/ns/core/stale".into();
            stale.text = "Old unused low-value raw note for stale archive".into();
            stale.importance = 0.2;
            stale.access_count = 0;
            stale.recall_count = 0;
            stale.query_diversity = 0;
            store.upsert(&stale).unwrap();
            // Backdate only this disposable fixture; governance keeps the live handle.
            let fixture = rusqlite::Connection::open_with_flags(
                tmp.path().join("memory").join("tachi.db"),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
            )
            .expect("open existing fixture database");
            fixture
                .execute(
                    "UPDATE memories SET created_at = '2020-01-01T00:00:00Z' WHERE id = 'stale'",
                    [],
                )
                .unwrap();
        }

        let report = mem.run_light_sleep_governance_report().unwrap();
        assert_eq!(report.promoted, 1, "expected one promotion: {report:?}");
        assert_eq!(
            report.stale_archived, 1,
            "expected one stale archive: {report:?}"
        );

        let store = mem.store_handle().lock();
        assert_eq!(store.get("promo").unwrap().unwrap().tier, "consolidated");
        assert!(
            store
                .get_with_options("stale", true)
                .unwrap()
                .unwrap()
                .archived
        );
    }

    #[tokio::test]
    async fn pinned_row_survives_stale_archive_via_governance() {
        let tmp = TempDir::new().unwrap();
        let mem = TachiMemory::with_embedder(
            "tachi",
            tmp.path(),
            Arc::new(crate::embeddings::NoopEmbedding),
            0.7,
            0.3,
        )
        .unwrap();
        mem.store_with_options(
            "pinned-old",
            "Pinned old low-importance note that must survive stale archive",
            MemoryCategory::Core,
            None,
            StoreOptions::default().pinned(true).with_importance(0.1),
        )
        .await
        .unwrap();

        {
            let store = mem.store_handle().lock();
            let row = store
                .list_by_path("/agents", 64, false)
                .unwrap()
                .into_iter()
                .find(|e| {
                    e.metadata.get("zeroclaw_key").and_then(|v| v.as_str()) == Some("pinned-old")
                })
                .expect("pinned row");
            assert_eq!(row.retention_policy.as_deref(), Some("pinned"));
            // Backdate only this disposable fixture; governance keeps the live handle.
            let fixture = rusqlite::Connection::open_with_flags(
                tmp.path().join("memory").join("tachi.db"),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
            )
            .expect("open existing fixture database");
            fixture
                .execute(
                    "UPDATE memories SET created_at = '2020-01-01T00:00:00Z', importance = 0.1, access_count = 0 WHERE id = ?1",
                    rusqlite::params![row.id],
                )
                .unwrap();
        }

        let report = mem.run_light_sleep_governance_report().unwrap();
        assert_eq!(
            report.stale_archived, 0,
            "pinned row must be spared: {report:?}"
        );
        let store = mem.store_handle().lock();
        let row = store
            .list_by_path("/agents", 64, false)
            .unwrap()
            .into_iter()
            .find(|e| e.metadata.get("zeroclaw_key").and_then(|v| v.as_str()) == Some("pinned-old"))
            .expect("still present");
        assert!(!row.archived);
    }

    #[tokio::test]
    async fn store_after_governance_preserves_merged_keywords() {
        let tmp = TempDir::new().unwrap();
        let mem = TachiMemory::with_embedder(
            "tachi",
            tmp.path(),
            Arc::new(crate::embeddings::NoopEmbedding),
            0.7,
            0.3,
        )
        .unwrap();

        let base = "Keyword carry-over preference note for equity trading workflow";
        let path_a = TachiMemory::storage_path(None, None, &MemoryCategory::Core, "kw-a");
        let path_b = TachiMemory::storage_path(None, None, &MemoryCategory::Core, "kw-b");
        {
            let mut store = mem.store_handle().lock();
            seed_raw(&mut store, "kw-a", &path_a, base, 0.5, &["alpha"], "raw");
            // Attach identity metadata so a later store("kw-b") updates this row.
            let mut entry_a = store.get("kw-a").unwrap().unwrap();
            entry_a.metadata = serde_json::json!({
                "zeroclaw_key": "kw-a",
                "zeroclaw_category": "core",
                "zeroclaw_namespace": "default",
                "zeroclaw_agent": "default",
            });
            store.upsert(&entry_a).unwrap();

            seed_raw(
                &mut store,
                "kw-b",
                &path_b,
                &format!("{base} today"),
                0.95,
                &["bravo"],
                "raw",
            );
            let mut entry_b = store.get("kw-b").unwrap().unwrap();
            entry_b.metadata = serde_json::json!({
                "zeroclaw_key": "kw-b",
                "zeroclaw_category": "core",
                "zeroclaw_namespace": "default",
                "zeroclaw_agent": "default",
            });
            store.upsert(&entry_b).unwrap();
        }

        let report = mem.run_light_sleep_governance_report().unwrap();
        assert!(
            report.near_dup_archived >= 1,
            "expected near-dup merge: {report:?}"
        );

        {
            let store = mem.store_handle().lock();
            let survivor = store.get("kw-b").unwrap().expect("survivor kw-b");
            assert!(survivor.keywords.iter().any(|k| k == "alpha"));
            assert!(survivor.keywords.iter().any(|k| k == "bravo"));
        }

        mem.store(
            "kw-b",
            &format!("{base} today (edited)"),
            MemoryCategory::Core,
            None,
        )
        .await
        .unwrap();

        let store = mem.store_handle().lock();
        let after = store.get("kw-b").unwrap().expect("survivor after store");
        assert!(
            after.keywords.iter().any(|k| k == "alpha"),
            "merged keywords must survive re-store: {:?}",
            after.keywords
        );
        assert!(after.keywords.iter().any(|k| k == "bravo"));
        assert!(
            after.text.contains("(edited)"),
            "carry-over must not block legitimate content updates: {:?}",
            after.text
        );
    }

    #[tokio::test]
    async fn unpin_clears_retention_policy() {
        let tmp = TempDir::new().unwrap();
        let mem = TachiMemory::with_embedder(
            "tachi",
            tmp.path(),
            Arc::new(crate::embeddings::NoopEmbedding),
            0.7,
            0.3,
        )
        .unwrap();
        mem.store_with_options(
            "pin-toggle",
            "Row that gets pinned then unpinned",
            MemoryCategory::Core,
            None,
            StoreOptions::default().pinned(true),
        )
        .await
        .unwrap();

        let find_row = |store: &portable_kernel::MemoryStore| {
            store
                .list_by_path("/agents", 64, false)
                .unwrap()
                .into_iter()
                .find(|e| {
                    e.metadata.get("zeroclaw_key").and_then(|v| v.as_str()) == Some("pin-toggle")
                })
                .expect("pin-toggle row")
        };
        assert_eq!(
            find_row(&mem.store_handle().lock())
                .retention_policy
                .as_deref(),
            Some("pinned")
        );

        mem.store_with_options(
            "pin-toggle",
            "Row that gets pinned then unpinned",
            MemoryCategory::Core,
            None,
            StoreOptions::default().pinned(false),
        )
        .await
        .unwrap();
        assert_eq!(
            find_row(&mem.store_handle().lock()).retention_policy,
            None,
            "unpin must clear memcore retention_policy"
        );
    }

    #[test]
    fn light_sleep_noop_default_on_non_tachi_memory_trait() {
        // Sqlite/other backends keep the trait default (no-op).
        assert!(
            crate::none::NoneMemory::new("none")
                .run_light_sleep_governance()
                .is_ok()
        );
    }

    #[tokio::test]
    async fn near_dup_governance_ignores_soft_superseded_rows() {
        let tmp = TempDir::new().unwrap();
        let mem = TachiMemory::with_embedder(
            "tachi",
            tmp.path(),
            Arc::new(crate::embeddings::NoopEmbedding),
            0.7,
            0.3,
        )
        .unwrap();
        mem.store_with_options(
            "old_superseded",
            "deploy freeze begins at 02:00 utc tonight for release cut",
            MemoryCategory::Core,
            None,
            StoreOptions {
                importance: Some(0.95),
                ..StoreOptions::default()
            },
        )
        .await
        .unwrap();
        mem.store_with_options(
            "active_replacement",
            "deploy freeze begins at 02:00 utc tonight for release",
            MemoryCategory::Core,
            None,
            StoreOptions {
                importance: Some(0.70),
                ..StoreOptions::default()
            },
        )
        .await
        .unwrap();

        let old = mem.get("old_superseded").await.unwrap().expect("old row");
        let active = mem
            .get("active_replacement")
            .await
            .unwrap()
            .expect("active replacement");
        mem.supersede(std::slice::from_ref(&old.id), &active.id)
            .await
            .unwrap();

        let report = mem.run_light_sleep_governance_report().unwrap();
        assert_eq!(
            report.near_dup_archived, 0,
            "soft-superseded row must not collapse/archive active replacement"
        );
        let kept = mem
            .get("active_replacement")
            .await
            .unwrap()
            .expect("active replacement must remain unarchived");
        assert!(kept.superseded_by.is_none());
        let recalled = mem
            .recall("deploy freeze", 5, None, None, None)
            .await
            .unwrap();
        assert_eq!(
            recalled
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            vec![active.id.as_str()]
        );
    }

    #[test]
    fn near_dup_governance_not_starved_by_pinned_soft_superseded_backlog() {
        let tmp = TempDir::new().unwrap();
        let mem = TachiMemory::new("tachi", tmp.path()).unwrap();
        {
            let mut store = mem.store_handle().lock();
            let mut backlog = Vec::with_capacity(4_050);
            for i in 0..4_050 {
                backlog.push(MemoryEntry {
                    id: format!("sup-{i:04}"),
                    path: format!("/agents/aaa/default/core/sup-{i:04}"),
                    summary: format!("superseded {i}"),
                    text: format!("superseded historical fact number {i}"),
                    importance: 0.95,
                    timestamp: "2026-07-01T00:00:00.000Z".into(),
                    valid_from: "2026-07-01T00:00:00.000Z".into(),
                    valid_until: None,
                    category: "fact".into(),
                    topic: String::new(),
                    keywords: Vec::new(),
                    persons: Vec::new(),
                    entities: Vec::new(),
                    location: String::new(),
                    source: "zeroclaw-test".into(),
                    scope: "general".into(),
                    archived: false,
                    access_count: 0,
                    scored_count: 0,
                    last_use_at: None,
                    last_access: None,
                    revision: 1,
                    vector: None,
                    retention_policy: Some("pinned".into()),
                    domain: None,
                    metadata: serde_json::json!({
                        "zeroclaw_key": format!("sup-{i:04}"),
                        "zeroclaw_category": "core",
                        "zeroclaw_namespace": "default",
                        "zeroclaw_agent": "aaa",
                        "pinned": true,
                        "superseded_by": "winner",
                    }),
                    recall_count: 0,
                    query_diversity: 0,
                    tier: "raw".into(),
                });
            }
            store.upsert_batch(&backlog).unwrap();

            let base = "Active duplicate deployment runbook step for zzz agent production release checklist verification";
            for (id, text, importance) in [
                ("zzz-dup-lose", base.to_string(), 0.5),
                ("zzz-dup-win", format!("{base} v2"), 0.9),
            ] {
                store
                    .upsert(&MemoryEntry {
                        id: id.into(),
                        path: format!("/agents/zzz/default/core/{id}"),
                        summary: text.clone(),
                        text,
                        importance,
                        timestamp: "2026-07-10T00:00:00.000Z".into(),
                        valid_from: "2026-07-10T00:00:00.000Z".into(),
                        valid_until: None,
                        category: "fact".into(),
                        topic: String::new(),
                        keywords: Vec::new(),
                        persons: Vec::new(),
                        entities: Vec::new(),
                        location: String::new(),
                        source: "zeroclaw-test".into(),
                        scope: "general".into(),
                        archived: false,
                        access_count: 0,
                        scored_count: 0,
                        last_use_at: None,
                        last_access: None,
                        revision: 1,
                        vector: None,
                        retention_policy: None,
                        domain: None,
                        metadata: serde_json::json!({
                            "zeroclaw_key": id,
                            "zeroclaw_category": "core",
                            "zeroclaw_namespace": "default",
                            "zeroclaw_agent": "zzz",
                        }),
                        recall_count: 0,
                        query_diversity: 0,
                        tier: "raw".into(),
                    })
                    .unwrap();
            }
        }

        let report = mem.run_light_sleep_governance_report().unwrap();
        assert_eq!(
            report.near_dup_archived, 1,
            "active near-dup pair must not be starved by >4000 pinned soft-superseded rows: {report:?}"
        );
        let store = mem.store_handle().lock();
        assert!(
            store
                .get_with_options("zzz-dup-lose", true)
                .unwrap()
                .unwrap()
                .archived
        );
        assert!(!store.get("zzz-dup-win").unwrap().unwrap().archived);
    }
}
