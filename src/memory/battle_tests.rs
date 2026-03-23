//! Battle tests for the memory system improvements.
//!
//! Exercises all 6 phases end-to-end: retrieval pipeline, namespace isolation,
//! importance scoring, conflict resolution, audit trail, and policy engine.
//! Designed to surface regressions in edge cases and multi-feature interactions.

#[cfg(test)]
mod tests {
    use crate::config::MemoryPolicyConfig;
    use crate::memory::audit::AuditedMemory;
    use crate::memory::conflict;
    use crate::memory::importance;
    use crate::memory::policy::{PolicyEnforcer, PolicyViolation};
    use crate::memory::retrieval::{RetrievalConfig, RetrievalPipeline};
    use crate::memory::sqlite::SqliteMemory;
    use crate::memory::traits::{Memory, MemoryCategory, MemoryEntry};
    use std::sync::Arc;
    use tempfile::TempDir;

    fn temp_sqlite() -> (TempDir, SqliteMemory) {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new(tmp.path()).unwrap();
        (tmp, mem)
    }

    // ═══════════════════════════════════════════════════════════════
    // Phase 1: Multi-stage retrieval pipeline
    // ═══════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn retrieval_pipeline_caches_sqlite_results() {
        let (_tmp, mem) = temp_sqlite();
        mem.store(
            "fact1",
            "Rust is a systems language",
            MemoryCategory::Core,
            None,
        )
        .await
        .unwrap();

        let pipeline = RetrievalPipeline::new(Arc::new(mem), RetrievalConfig::default());

        // First call — cache miss, hits FTS
        let r1 = pipeline
            .recall("Rust", 10, None, None, None, None)
            .await
            .unwrap();
        assert_eq!(r1.len(), 1);
        assert_eq!(pipeline.cache_size(), 1);

        // Second call — cache hit
        let r2 = pipeline
            .recall("Rust", 10, None, None, None, None)
            .await
            .unwrap();
        assert_eq!(r2.len(), 1);
        assert_eq!(r2[0].content, r1[0].content);
    }

    #[tokio::test]
    async fn retrieval_pipeline_invalidation_forces_fresh_results() {
        let (_tmp, mem) = temp_sqlite();
        mem.store(
            "k1",
            "original content searchable",
            MemoryCategory::Core,
            None,
        )
        .await
        .unwrap();

        let mem = Arc::new(mem);
        let pipeline = RetrievalPipeline::new(mem.clone(), RetrievalConfig::default());

        let _ = pipeline
            .recall("searchable", 10, None, None, None, None)
            .await
            .unwrap();
        assert_eq!(pipeline.cache_size(), 1);

        pipeline.invalidate_cache();
        assert_eq!(pipeline.cache_size(), 0);
    }

    #[tokio::test]
    async fn retrieval_pipeline_respects_limit() {
        let (_tmp, mem) = temp_sqlite();
        for i in 0..20 {
            mem.store(
                &format!("k{i}"),
                &format!("retrieval pipeline test item {i}"),
                MemoryCategory::Core,
                None,
            )
            .await
            .unwrap();
        }

        let pipeline = RetrievalPipeline::new(Arc::new(mem), RetrievalConfig::default());

        let results = pipeline
            .recall("retrieval pipeline test", 3, None, None, None, None)
            .await
            .unwrap();
        assert!(results.len() <= 3);
    }

    #[tokio::test]
    async fn retrieval_pipeline_empty_query_works() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("k1", "some data", MemoryCategory::Core, None)
            .await
            .unwrap();

        let pipeline = RetrievalPipeline::new(Arc::new(mem), RetrievalConfig::default());

        let results = pipeline
            .recall("", 10, None, None, None, None)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn retrieval_pipeline_with_namespace_filter() {
        let (_tmp, mem) = temp_sqlite();
        mem.store_with_metadata(
            "k1",
            "data in ns1",
            MemoryCategory::Core,
            None,
            Some("ns1"),
            None,
            None,
        )
        .await
        .unwrap();
        mem.store_with_metadata(
            "k2",
            "data in ns2",
            MemoryCategory::Core,
            None,
            Some("ns2"),
            None,
            None,
        )
        .await
        .unwrap();

        let pipeline = RetrievalPipeline::new(Arc::new(mem), RetrievalConfig::default());

        let results = pipeline
            .recall("data", 10, None, Some("ns1"), None, None)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].namespace, "ns1");
    }

    // ═══════════════════════════════════════════════════════════════
    // Phase 2: Namespace isolation
    // ═══════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn namespace_isolation_between_agents() {
        let (_tmp, mem) = temp_sqlite();

        mem.store_with_metadata(
            "agent_a_pref",
            "Agent A likes concise answers",
            MemoryCategory::Core,
            None,
            Some("agent-a"),
            None,
            None,
        )
        .await
        .unwrap();

        mem.store_with_metadata(
            "agent_b_pref",
            "Agent B likes verbose answers",
            MemoryCategory::Core,
            None,
            Some("agent-b"),
            None,
            None,
        )
        .await
        .unwrap();

        mem.store_with_metadata(
            "shared_fact",
            "The sky is blue",
            MemoryCategory::Core,
            None,
            Some("shared"),
            None,
            None,
        )
        .await
        .unwrap();

        // Agent A namespace only sees its own memories
        let results = mem
            .recall_namespaced("agent-a", "answers", 10, None, None, None)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].content.contains("concise"));

        // Agent B namespace only sees its own memories
        let results = mem
            .recall_namespaced("agent-b", "answers", 10, None, None, None)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].content.contains("verbose"));

        // Cross-namespace query should not leak
        let results = mem
            .recall_namespaced("agent-a", "verbose", 10, None, None, None)
            .await
            .unwrap();
        assert!(results.is_empty(), "agent-a should not see agent-b data");
    }

    #[tokio::test]
    async fn namespace_default_assignment() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("basic_key", "basic value", MemoryCategory::Core, None)
            .await
            .unwrap();

        let entry = mem.get("basic_key").await.unwrap().unwrap();
        assert_eq!(
            entry.namespace, "default",
            "entries without explicit namespace should be 'default'"
        );
    }

    #[tokio::test]
    async fn namespace_with_special_characters() {
        let (_tmp, mem) = temp_sqlite();
        let ns = "org/team-alpha/v2";
        mem.store_with_metadata("k1", "data", MemoryCategory::Core, None, Some(ns), None, None)
            .await
            .unwrap();

        let results = mem
            .recall_namespaced(ns, "data", 10, None, None, None)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].namespace, ns);
    }

    #[tokio::test]
    async fn namespace_empty_string_works() {
        let (_tmp, mem) = temp_sqlite();
        mem.store_with_metadata(
            "k1",
            "empty ns data",
            MemoryCategory::Core,
            None,
            Some(""),
            None,
            None,
        )
        .await
        .unwrap();

        let results = mem
            .recall_namespaced("", "data", 10, None, None, None)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
    }

    // ═══════════════════════════════════════════════════════════════
    // Phase 3: Importance scoring
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn importance_core_higher_than_daily() {
        let core = importance::compute_importance("some fact", &MemoryCategory::Core);
        let daily = importance::compute_importance("some fact", &MemoryCategory::Daily);
        assert!(core > daily, "Core should score higher: {core} vs {daily}");
    }

    #[test]
    fn importance_keywords_increase_score() {
        let without = importance::compute_importance("the cat sat", &MemoryCategory::Core);
        let with = importance::compute_importance(
            "important decision: always use Rust",
            &MemoryCategory::Core,
        );
        assert!(
            with > without,
            "Keyword content should score higher: {with} vs {without}"
        );
    }

    #[test]
    fn importance_score_stays_in_bounds() {
        // Even with every keyword + Core category
        let max_content =
            "important critical decision rule policy must always never requirement principle";
        let score = importance::compute_importance(max_content, &MemoryCategory::Core);
        assert!(score <= 1.0, "Score should be capped at 1.0, got {score}");
        assert!(score >= 0.0, "Score should be non-negative, got {score}");
    }

    #[test]
    fn importance_empty_content() {
        let score = importance::compute_importance("", &MemoryCategory::Core);
        assert!(
            (score - 0.7).abs() < f64::EPSILON,
            "Empty content should use base score"
        );
    }

    #[tokio::test]
    async fn importance_persists_in_sqlite() {
        let (_tmp, mem) = temp_sqlite();
        mem.store_with_metadata(
            "high_importance",
            "critical decision",
            MemoryCategory::Core,
            None,
            None,
            Some(0.95),
            None,
        )
        .await
        .unwrap();

        let entry = mem.get("high_importance").await.unwrap().unwrap();
        assert!((entry.importance.unwrap() - 0.95).abs() < 0.01);
    }

    #[test]
    fn weighted_final_score_all_zeros() {
        let score = importance::weighted_final_score(0.0, 0.0, 0.0);
        assert!(score.abs() < f64::EPSILON);
    }

    #[test]
    fn weighted_final_score_all_ones() {
        let score = importance::weighted_final_score(1.0, 1.0, 1.0);
        assert!((score - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn weighted_final_score_hybrid_dominant() {
        // hybrid_score dominates (0.7 weight)
        let score = importance::weighted_final_score(1.0, 0.0, 0.0);
        assert!((score - 0.7).abs() < f64::EPSILON);
    }

    // ═══════════════════════════════════════════════════════════════
    // Phase 4: Conflict resolution
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn jaccard_similarity_identical() {
        let sim = conflict::jaccard_similarity("the quick brown fox", "the quick brown fox");
        assert!((sim - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn jaccard_similarity_no_overlap() {
        let sim = conflict::jaccard_similarity("hello world", "foo bar baz");
        assert!(sim.abs() < f64::EPSILON);
    }

    #[test]
    fn jaccard_similarity_case_sensitive() {
        // Jaccard is case-sensitive (words don't match with different case)
        let sim = conflict::jaccard_similarity("Hello World", "hello world");
        assert!(sim < 1.0, "Should be case-sensitive");
    }

    #[test]
    fn conflict_detection_skips_non_core() {
        let entries = vec![MemoryEntry {
            id: "1".into(),
            key: "daily1".into(),
            content: "User prefers Rust".into(),
            category: MemoryCategory::Daily,
            timestamp: "now".into(),
            session_id: None,
            score: None,
            namespace: "default".into(),
            importance: None,
            superseded_by: None,
            metadata: None,
        }];

        let conflicts = conflict::find_text_conflicts(&entries, "User prefers Go", 0.3);
        assert!(
            conflicts.is_empty(),
            "Non-core entries should not be flagged"
        );
    }

    #[test]
    fn conflict_detection_skips_already_superseded() {
        let entries = vec![MemoryEntry {
            id: "1".into(),
            key: "old_pref".into(),
            content: "User prefers Rust for systems work".into(),
            category: MemoryCategory::Core,
            timestamp: "now".into(),
            session_id: None,
            score: None,
            namespace: "default".into(),
            importance: Some(0.7),
            superseded_by: Some("newer_id".into()), // already superseded
            metadata: None,
        }];

        let conflicts =
            conflict::find_text_conflicts(&entries, "User prefers Go for systems work", 0.3);
        assert!(
            conflicts.is_empty(),
            "Already-superseded entries should be skipped"
        );
    }

    #[test]
    fn conflict_detection_identical_content_not_flagged() {
        let entries = vec![MemoryEntry {
            id: "1".into(),
            key: "pref".into(),
            content: "User prefers Rust".into(),
            category: MemoryCategory::Core,
            timestamp: "now".into(),
            session_id: None,
            score: None,
            namespace: "default".into(),
            importance: Some(0.7),
            superseded_by: None,
            metadata: None,
        }];

        // Exact same content should not be a conflict
        let conflicts = conflict::find_text_conflicts(&entries, "User prefers Rust", 0.3);
        assert!(
            conflicts.is_empty(),
            "Identical content should not be flagged as conflict"
        );
    }

    #[tokio::test]
    async fn superseded_entries_hidden_from_recall() {
        let (_tmp, mem) = temp_sqlite();

        // Store an entry
        mem.store(
            "old_pref",
            "User prefers Python",
            MemoryCategory::Core,
            None,
        )
        .await
        .unwrap();

        // Mark it as superseded via raw SQL
        {
            let conn = mem.connection().lock();
            conn.execute(
                "UPDATE memories SET superseded_by = 'new_id' WHERE key = 'old_pref'",
                [],
            )
            .unwrap();
        }

        // Store the new entry
        mem.store("new_pref", "User prefers Rust", MemoryCategory::Core, None)
            .await
            .unwrap();

        // Recall should only return the non-superseded entry
        let results = mem.recall("prefers", 10, None, None, None).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].key, "new_pref");
        assert_eq!(results[0].content, "User prefers Rust");

        // List should also filter superseded
        let all = mem.list(Some(&MemoryCategory::Core), None).await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].key, "new_pref");
    }

    #[tokio::test]
    async fn superseded_entry_still_accessible_via_get() {
        let (_tmp, mem) = temp_sqlite();

        mem.store("versioned", "version 1", MemoryCategory::Core, None)
            .await
            .unwrap();

        {
            let conn = mem.connection().lock();
            conn.execute(
                "UPDATE memories SET superseded_by = 'v2_id' WHERE key = 'versioned'",
                [],
            )
            .unwrap();
        }

        // Direct get still works (for audit purposes)
        let entry = mem.get("versioned").await.unwrap().unwrap();
        assert_eq!(entry.content, "version 1");
        assert_eq!(entry.superseded_by.as_deref(), Some("v2_id"));
    }

    // ═══════════════════════════════════════════════════════════════
    // Phase 5: Audit trail
    // ═══════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn audit_logs_all_operation_types() {
        let tmp = TempDir::new().unwrap();
        let inner = crate::memory::NoneMemory::new();
        let audited = AuditedMemory::new(inner, tmp.path()).unwrap();

        audited
            .store("k1", "v1", MemoryCategory::Core, None)
            .await
            .unwrap();
        let _ = audited.recall("query", 10, None, None, None).await;
        let _ = audited.get("k1").await;
        let _ = audited.list(None, None).await;
        let _ = audited.forget("k1").await;

        assert_eq!(
            audited.audit_count().unwrap(),
            5,
            "Should have 5 audit entries"
        );
    }

    #[tokio::test]
    async fn audit_with_namespaced_operations() {
        let tmp = TempDir::new().unwrap();
        let inner = crate::memory::NoneMemory::new();
        let audited = AuditedMemory::new(inner, tmp.path()).unwrap();

        audited
            .store_with_metadata(
                "k1",
                "v1",
                MemoryCategory::Core,
                None,
                Some("ns1"),
                Some(0.8),
                None,
            )
            .await
            .unwrap();

        let _ = audited
            .recall_namespaced("ns1", "query", 10, None, None, None)
            .await;

        assert_eq!(audited.audit_count().unwrap(), 2);
    }

    #[tokio::test]
    async fn audit_wrapping_sqlite_backend() {
        let tmp = TempDir::new().unwrap();
        let inner = SqliteMemory::new(tmp.path()).unwrap();
        let audited = AuditedMemory::new(inner, tmp.path()).unwrap();

        // Full round-trip through audited sqlite
        audited
            .store("audit_test", "audit value", MemoryCategory::Core, None)
            .await
            .unwrap();

        let entry = audited.get("audit_test").await.unwrap().unwrap();
        assert_eq!(entry.content, "audit value");

        let results = audited.recall("audit", 10, None, None, None).await.unwrap();
        assert_eq!(results.len(), 1);

        // 3 operations: store, get, recall
        assert_eq!(audited.audit_count().unwrap(), 3);
    }

    #[tokio::test]
    async fn audit_concurrent_operations() {
        let tmp = TempDir::new().unwrap();
        let inner = crate::memory::NoneMemory::new();
        let audited = Arc::new(AuditedMemory::new(inner, tmp.path()).unwrap());

        let mut handles = Vec::new();
        for i in 0..10 {
            let a = audited.clone();
            handles.push(tokio::spawn(async move {
                a.store(
                    &format!("k{i}"),
                    &format!("v{i}"),
                    MemoryCategory::Core,
                    None,
                )
                .await
                .unwrap();
            }));
        }

        for h in handles {
            h.await.unwrap();
        }

        assert_eq!(audited.audit_count().unwrap(), 10);
    }

    // ═══════════════════════════════════════════════════════════════
    // Phase 6: Policy engine
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn policy_read_only_multiple_namespaces() {
        let policy = MemoryPolicyConfig {
            read_only_namespaces: vec!["archive".into(), "system".into()],
            ..Default::default()
        };
        let enforcer = PolicyEnforcer::new(&policy);

        assert!(enforcer.is_read_only("archive"));
        assert!(enforcer.is_read_only("system"));
        assert!(!enforcer.is_read_only("user"));
        assert!(!enforcer.is_read_only("default"));
    }

    #[test]
    fn policy_validate_store_rejects_read_only() {
        let policy = MemoryPolicyConfig {
            read_only_namespaces: vec!["frozen".into()],
            ..Default::default()
        };
        let enforcer = PolicyEnforcer::new(&policy);

        let result = enforcer.validate_store("frozen", &MemoryCategory::Core);
        assert!(result.is_err());

        if let Err(PolicyViolation::ReadOnlyNamespace(ns)) = result {
            assert_eq!(ns, "frozen");
        } else {
            panic!("Expected ReadOnlyNamespace violation");
        }
    }

    #[test]
    fn policy_quota_boundary_conditions() {
        let policy = MemoryPolicyConfig {
            max_entries_per_namespace: 1,
            ..Default::default()
        };
        let enforcer = PolicyEnforcer::new(&policy);

        assert!(
            enforcer.check_namespace_limit(0).is_ok(),
            "0/1 should be ok"
        );
        assert!(
            enforcer.check_namespace_limit(1).is_err(),
            "1/1 should fail (at limit)"
        );
        assert!(
            enforcer.check_namespace_limit(2).is_err(),
            "2/1 should fail (over limit)"
        );
    }

    #[test]
    fn policy_zero_quota_means_unlimited() {
        let policy = MemoryPolicyConfig::default();
        let enforcer = PolicyEnforcer::new(&policy);

        // max_entries_per_namespace = 0 means no limit
        assert!(enforcer.check_namespace_limit(999_999).is_ok());
        assert!(enforcer.check_category_limit(999_999).is_ok());
    }

    #[test]
    fn policy_custom_category_retention() {
        let mut retention = std::collections::HashMap::new();
        retention.insert("core".into(), 365);
        retention.insert("daily".into(), 14);
        retention.insert("my_custom".into(), 7);

        let policy = MemoryPolicyConfig {
            retention_days_by_category: retention,
            ..Default::default()
        };
        let enforcer = PolicyEnforcer::new(&policy);

        assert_eq!(
            enforcer.retention_days_for_category(&MemoryCategory::Core, 30),
            365,
        );
        assert_eq!(
            enforcer.retention_days_for_category(&MemoryCategory::Daily, 30),
            14,
        );
        assert_eq!(
            enforcer.retention_days_for_category(&MemoryCategory::Custom("my_custom".into()), 30),
            7,
        );
        // Unknown category falls back to default
        assert_eq!(
            enforcer.retention_days_for_category(&MemoryCategory::Custom("unknown".into()), 30),
            30,
        );
    }

    // ═══════════════════════════════════════════════════════════════
    // Cross-phase integration tests
    // ═══════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn full_lifecycle_store_recall_supersede() {
        let (_tmp, mem) = temp_sqlite();

        // Store initial fact
        mem.store_with_metadata(
            "user_lang",
            "User prefers Python for data science",
            MemoryCategory::Core,
            None,
            Some("agent-1"),
            Some(0.7),
            None,
        )
        .await
        .unwrap();

        // Recall it
        let results = mem
            .recall_namespaced("agent-1", "prefers", 10, None, None, None)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].namespace, "agent-1");
        assert!((results[0].importance.unwrap() - 0.7).abs() < 0.01);

        // Supersede it
        let old_id = results[0].id.clone();
        {
            let conn = mem.connection().lock();
            conn.execute(
                "UPDATE memories SET superseded_by = 'new_entry_id' WHERE id = ?1",
                rusqlite::params![old_id],
            )
            .unwrap();
        }

        // Store updated fact
        mem.store_with_metadata(
            "user_lang_v2",
            "User now prefers Rust for systems programming",
            MemoryCategory::Core,
            None,
            Some("agent-1"),
            Some(0.9),
            None,
        )
        .await
        .unwrap();

        // Recall should only see the new fact
        let results = mem
            .recall_namespaced("agent-1", "prefers", 10, None, None, None)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].content.contains("Rust"));
        assert!((results[0].importance.unwrap() - 0.9).abs() < 0.01);
    }

    #[tokio::test]
    async fn pipeline_with_audited_sqlite() {
        let tmp = TempDir::new().unwrap();
        let sqlite = SqliteMemory::new(tmp.path()).unwrap();
        let audited = AuditedMemory::new(sqlite, tmp.path()).unwrap();
        let audited = Arc::new(audited);

        // Store through audited backend
        audited
            .store("pipeline_test", "pipeline data", MemoryCategory::Core, None)
            .await
            .unwrap();

        // Create pipeline on top of audited backend
        let pipeline = RetrievalPipeline::new(
            audited.clone() as Arc<dyn Memory>,
            RetrievalConfig::default(),
        );

        let results = pipeline
            .recall("pipeline", 10, None, None, None, None)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "pipeline data");

        // Audit should have logged: 1 store + 1 recall (from pipeline)
        assert!(audited.audit_count().unwrap() >= 2);
    }

    #[tokio::test]
    async fn namespace_isolation_with_session_id_cross_filter() {
        let (_tmp, mem) = temp_sqlite();

        // Store in ns1/sess-a
        mem.store_with_metadata(
            "k1",
            "fact for ns1 sess-a",
            MemoryCategory::Core,
            Some("sess-a"),
            Some("ns1"),
            None,
            None,
        )
        .await
        .unwrap();

        // Store in ns1/sess-b
        mem.store_with_metadata(
            "k2",
            "fact for ns1 sess-b",
            MemoryCategory::Core,
            Some("sess-b"),
            Some("ns1"),
            None,
            None,
        )
        .await
        .unwrap();

        // Store in ns2/sess-a
        mem.store_with_metadata(
            "k3",
            "fact for ns2 sess-a",
            MemoryCategory::Core,
            Some("sess-a"),
            Some("ns2"),
            None,
            None,
        )
        .await
        .unwrap();

        // Namespace + session double filter
        let results = mem
            .recall_namespaced("ns1", "fact", 10, Some("sess-a"), None, None)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].key, "k1");
    }

    #[tokio::test]
    async fn many_namespaces_sequential_writes() {
        let (_tmp, mem) = temp_sqlite();

        // Write entries across multiple namespaces sequentially
        // (SQLite is single-writer; concurrent spawns cause locking issues)
        for ns_idx in 0..5 {
            let ns = format!("ns-{ns_idx}");
            for entry_idx in 0..5 {
                mem.store_with_metadata(
                    &format!("key_{ns_idx}_{entry_idx}"),
                    &format!("value {entry_idx} in namespace {ns_idx}"),
                    MemoryCategory::Core,
                    None,
                    Some(&ns),
                    Some(0.5),
                    None,
                )
                .await
                .unwrap();
            }
        }

        assert_eq!(mem.count().await.unwrap(), 25);

        // Each namespace should have its own entries and not leak across
        for ns_idx in 0..5 {
            let ns = format!("ns-{ns_idx}");
            let results = mem
                .recall_namespaced(&ns, "value", 20, None, None, None)
                .await
                .unwrap();
            assert!(!results.is_empty(), "namespace {ns} should have entries");
            for entry in &results {
                assert_eq!(
                    entry.namespace, ns,
                    "entry in namespace recall should belong to that namespace"
                );
            }
        }
    }

    #[tokio::test]
    async fn deterministic_tiebreaker_in_hybrid_merge() {
        use crate::memory::vector;

        // Two results with identical scores
        let vec_results = vec![("b".into(), 0.8_f32), ("a".into(), 0.8_f32)];
        let merged = vector::hybrid_merge(&vec_results, &[], 1.0, 0.0, 10);

        // With deterministic tiebreaker, "a" should come before "b"
        assert_eq!(merged.len(), 2);
        assert_eq!(
            merged[0].id, "a",
            "Deterministic tiebreaker should sort by id"
        );
        assert_eq!(merged[1].id, "b");
    }

    #[tokio::test]
    async fn schema_migration_idempotent_with_new_columns() {
        let tmp = TempDir::new().unwrap();

        // First open: creates schema with all columns
        {
            let mem = SqliteMemory::new(tmp.path()).unwrap();
            mem.store_with_metadata(
                "persist_key",
                "persisted data",
                MemoryCategory::Core,
                None,
                Some("test-ns"),
                Some(0.8),
                None,
            )
            .await
            .unwrap();
        }

        // Second open: migrations run again but are idempotent
        {
            let mem = SqliteMemory::new(tmp.path()).unwrap();
            let entry = mem.get("persist_key").await.unwrap().unwrap();
            assert_eq!(entry.content, "persisted data");
            assert_eq!(entry.namespace, "test-ns");
            assert!((entry.importance.unwrap() - 0.8).abs() < 0.01);
        }

        // Third open: still fine
        {
            let mem = SqliteMemory::new(tmp.path()).unwrap();
            assert!(mem.health_check().await);
            assert_eq!(mem.count().await.unwrap(), 1);
        }
    }

    // ═══════════════════════════════════════════════════════════════
    // Edge cases and stress tests
    // ═══════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn importance_survives_upsert() {
        let (_tmp, mem) = temp_sqlite();

        mem.store_with_metadata(
            "k1",
            "original",
            MemoryCategory::Core,
            None,
            None,
            Some(0.9),
            None,
        )
        .await
        .unwrap();

        // Upsert with different importance
        mem.store_with_metadata("k1", "updated", MemoryCategory::Core, None, None, Some(0.3), None)
            .await
            .unwrap();

        let entry = mem.get("k1").await.unwrap().unwrap();
        assert_eq!(entry.content, "updated");
        assert!((entry.importance.unwrap() - 0.3).abs() < 0.01);
        assert_eq!(mem.count().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn namespace_survives_upsert() {
        let (_tmp, mem) = temp_sqlite();

        mem.store_with_metadata("k1", "v1", MemoryCategory::Core, None, Some("ns-old"), None, None)
            .await
            .unwrap();

        mem.store_with_metadata("k1", "v2", MemoryCategory::Core, None, Some("ns-new"), None, None)
            .await
            .unwrap();

        let entry = mem.get("k1").await.unwrap().unwrap();
        assert_eq!(entry.namespace, "ns-new");
        assert_eq!(entry.content, "v2");
    }

    #[tokio::test]
    async fn forget_cleans_up_namespaced_entry() {
        let (_tmp, mem) = temp_sqlite();

        mem.store_with_metadata(
            "k1",
            "data",
            MemoryCategory::Core,
            None,
            Some("ns1"),
            Some(0.9),
            None,
        )
        .await
        .unwrap();

        let removed = mem.forget("k1").await.unwrap();
        assert!(removed);
        assert!(mem.get("k1").await.unwrap().is_none());
        assert_eq!(mem.count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn empty_namespace_recall_returns_nothing() {
        let (_tmp, mem) = temp_sqlite();

        mem.store_with_metadata("k1", "data", MemoryCategory::Core, None, Some("ns1"), None, None)
            .await
            .unwrap();

        let results = mem
            .recall_namespaced("nonexistent-ns", "data", 10, None, None, None)
            .await
            .unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn memory_entry_serde_roundtrip_with_new_fields() {
        let entry = MemoryEntry {
            id: "test-id".into(),
            key: "test-key".into(),
            content: "test content".into(),
            category: MemoryCategory::Core,
            timestamp: "2026-03-21T00:00:00Z".into(),
            session_id: Some("sess-1".into()),
            score: Some(0.85),
            namespace: "my-namespace".into(),
            importance: Some(0.7),
            superseded_by: Some("newer-id".into()),
            metadata: None,
        };

        let json = serde_json::to_string(&entry).unwrap();
        let parsed: MemoryEntry = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.namespace, "my-namespace");
        assert_eq!(parsed.importance, Some(0.7));
        assert_eq!(parsed.superseded_by.as_deref(), Some("newer-id"));
    }

    #[test]
    fn memory_entry_deserialize_without_new_fields_uses_defaults() {
        // Simulate legacy JSON without new fields
        let json = r#"{
            "id": "1",
            "key": "k",
            "content": "v",
            "category": "core",
            "timestamp": "2026-01-01T00:00:00Z",
            "session_id": null,
            "score": null
        }"#;

        let parsed: MemoryEntry = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.namespace, "default", "Should default to 'default'");
        assert!(parsed.importance.is_none());
        assert!(parsed.superseded_by.is_none());
    }
}
