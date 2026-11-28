//! Opt-in real storage regressions. Use disposable, credential-free local services.
//! Qdrant's embedder is deterministic test data, not a mocked Qdrant server.
use std::sync::Arc;
use zeroclaw::agent::TurnMeta;
use zeroclaw::agent::memory_inject::{MemoryInjectConfig, render_memory_context};
use zeroclaw::observability::noop::NoopObserver;
use zeroclaw_memory::{
    AgentScopedMemory, AuditedMemory, Memory, MemoryCategory, PrincipalPlaneMemory,
    RetrievalConfig, RetrievalPipeline, ScannedMemory,
};

const FACT: &str = "orchid orchid orchid orchid orchid prefers shade";
const RAW: &str = "orchid orchid orchid orchid orchid orchid orchid orchid orchid orchid";
const SESSION: Option<&str> = Some("prompt-session");

/// Exercise the real backend -> scanner -> audit -> agent scope -> cached
/// retrieval -> production prompt renderer, not a hand-built candidate list.
async fn assert_prompt_delivery<M: Memory + 'static>(backend: M) {
    let own = backend.ensure_agent_uuid("own").await.unwrap();
    let other = backend.ensure_agent_uuid("other").await.unwrap();
    let tmp = tempfile::TempDir::new().unwrap();
    let storage: Arc<dyn Memory> = Arc::new(
        AuditedMemory::new(
            ScannedMemory::new(
                backend,
                &zeroclaw_config::schema::MemoryPolicyConfig::default(),
            ),
            tmp.path(),
        )
        .unwrap(),
    );
    let memory = RetrievalPipeline::new(
        Arc::new(AgentScopedMemory::new(
            storage.clone(),
            own.clone(),
            Vec::<String>::new(),
        )),
        RetrievalConfig {
            cache_enabled: true,
            ..RetrievalConfig::default()
        },
    );
    let cfg = MemoryInjectConfig::from_memory_config(
        &zeroclaw_config::schema::MemoryConfig::default(),
        5,
    );
    assert_eq!(cfg.min_relevance_score, 0.4);
    assert!(!cfg.rerank_enabled);
    memory
        .store("plant_fact", FACT, MemoryCategory::Core, SESSION)
        .await
        .unwrap();
    let mut keys = Vec::new();
    // Exceed both the renderer budget and scanner's 4x candidate pool.
    for _ in 0..48 {
        let key = format!("user_msg_{}", uuid::Uuid::new_v4());
        memory
            .store(&key, RAW, MemoryCategory::Conversation, SESSION)
            .await
            .unwrap();
        keys.push(key);
    }
    memory
        .store(
            "other_session_fact",
            RAW,
            MemoryCategory::Core,
            Some("other-session"),
        )
        .await
        .unwrap();
    storage
        .store_with_agent(
            "other_agent_fact",
            RAW,
            MemoryCategory::Core,
            SESSION,
            None,
            None,
            Some(&other),
        )
        .await
        .unwrap();

    // Verify actual backend ordering and a fact above the production floor.
    let crowded = Memory::recall(&memory, "orchid", cfg.limit, SESSION, None, None)
        .await
        .unwrap();
    assert_eq!(crowded.len(), cfg.limit);
    assert!(
        crowded
            .iter()
            .all(|e| zeroclaw_memory::is_user_autosave_key(&e.key))
    );
    let all = Memory::recall(&memory, "orchid", 64, SESSION, None, None)
        .await
        .unwrap();
    assert_eq!(all.len(), 49);
    let fact_score = all
        .iter()
        .find(|e| e.key == "plant_fact")
        .unwrap()
        .score
        .unwrap();
    assert!(
        fact_score >= cfg.min_relevance_score,
        "fact score: {fact_score}"
    );
    assert_eq!(
        all.iter()
            .filter(
                |e| zeroclaw_memory::is_user_autosave_key(&e.key) && e.score.unwrap() > fact_score
            )
            .count(),
        48
    );

    for _ in 0..2 {
        let prompt = render_memory_context(
            &memory,
            &NoopObserver,
            "orchid",
            &[SESSION],
            &cfg,
            false,
            TurnMeta {
                parent_agent_alias: None,
                agent_alias: Some("own"),
                turn_id: "prompt-regression",
                channel_name: "test",
            },
        )
        .await;
        assert!(prompt.contains(&format!("plant_fact: {FACT}")), "{prompt}");
        for excluded in ["user_msg", "other_session_fact", "other_agent_fact", RAW] {
            assert!(!prompt.contains(excluded), "{prompt}");
        }
    }
    // Caller allowlists cannot widen the bound agent handle.
    assert!(
        memory
            .recall_for_prompt_for_agents(&[&other], "orchid", 5, SESSION)
            .await
            .unwrap()
            .is_empty()
    );
    for query in ["orchid", "", "*", "   "] {
        let prompt = memory.recall_for_prompt(query, 5, SESSION).await.unwrap();
        assert_eq!(prompt.len(), 1);
        assert_eq!(prompt[0].key, "plant_fact");
    }
    assert!(
        memory
            .recall_for_prompt("orchid", 0, SESSION)
            .await
            .unwrap()
            .is_empty()
    );
    let archive = Memory::recall(&memory, "orchid", 5, SESSION, None, None)
        .await
        .unwrap();
    assert_eq!(
        archive.iter().map(|e| &e.key).collect::<Vec<_>>(),
        crowded.iter().map(|e| &e.key).collect::<Vec<_>>()
    );
    let uncached_archive = storage
        .recall_for_agents(&[&own], "orchid", 64, SESSION, None, None)
        .await
        .unwrap();
    assert_eq!(uncached_archive.len(), 49);
    assert_eq!(
        uncached_archive
            .iter()
            .filter(|e| zeroclaw_memory::is_user_autosave_key(&e.key))
            .count(),
        48
    );
    for key in keys {
        let entry = memory.get(&key).await.unwrap().unwrap();
        assert_eq!(entry.content, RAW);
        assert_eq!(entry.session_id.as_deref(), SESSION);
        assert_eq!(entry.agent_id.as_deref(), Some(own.as_str()));
    }
    // Authorized peer categories stay constrained even after backend refilling.
    storage
        .store_with_agent(
            "peer_allowed",
            FACT,
            MemoryCategory::Core,
            SESSION,
            None,
            None,
            Some(&other),
        )
        .await
        .unwrap();
    storage
        .store_with_agent(
            "peer_denied",
            RAW,
            MemoryCategory::Conversation,
            SESSION,
            None,
            None,
            Some(&other),
        )
        .await
        .unwrap();
    let granted = AgentScopedMemory::new_with_grants(
        storage.clone(),
        own.clone(),
        [zeroclaw_memory::AgentMemoryGrant {
            agent_id: other.clone(),
            categories: Some(["core".to_string()].into_iter().collect()),
        }],
    );
    let granted_entries = granted
        .recall_for_prompt("orchid", 10, SESSION)
        .await
        .unwrap();
    assert!(granted_entries.iter().any(|e| e.key == "peer_allowed"));
    assert!(!granted_entries.iter().any(|e| e.key == "peer_denied"));
    let own_entries = granted
        .recall_for_prompt_for_agents(&[&own], "orchid", 10, SESSION)
        .await
        .unwrap();
    assert!(
        own_entries
            .iter()
            .all(|e| e.agent_id.as_deref() == Some(own.as_str()))
    );

    // Both backends currently reject private-plane operations. Never fall back
    // to shared recall when a fully scoped private prompt is requested.
    let scope = zeroclaw_api::memory_traits::PrincipalScope::new("private-owner")
        .with_agent(Some("own".into()))
        .with_namespace(Some("notes".into()))
        .with_tenant(Some("tenant".into()));
    let private = PrincipalPlaneMemory::new(storage.clone(), scope);
    assert!(
        private
            .store("private_fact", FACT, MemoryCategory::Core, SESSION)
            .await
            .is_err()
    );
    assert!(
        private
            .recall_for_prompt("orchid", 5, SESSION)
            .await
            .is_err()
    );
    assert!(
        Memory::recall(&private, "orchid", 5, SESSION, None, None)
            .await
            .is_err()
    );

    // Direct backend entry points (including the established empty-allowlist
    // meaning) and legacy key normalization must agree, without schema markers.
    for key in [
        "user_msg",
        "user_msg_",
        " USER_MSG_legacy ",
        "\tuser_msg_tab\n",
        "\u{2003}USER_MSG_unicode\u{a0}",
    ] {
        memory
            .store(key, RAW, MemoryCategory::Conversation, SESSION)
            .await
            .unwrap();
    }
    for key in ["userXmsg_keep", "user_message", "assistant_resp_keep"] {
        memory
            .store(key, FACT, MemoryCategory::Core, SESSION)
            .await
            .unwrap();
    }
    for query in ["orchid", "*"] {
        for entries in [
            storage.recall_for_prompt(query, 64, SESSION).await.unwrap(),
            storage
                .recall_for_prompt_for_agents(&[], query, 64, SESSION)
                .await
                .unwrap(),
            storage
                .recall_for_prompt_for_agents(&[&own], query, 64, SESSION)
                .await
                .unwrap(),
        ] {
            assert!(
                entries
                    .iter()
                    .all(|e| !zeroclaw_memory::is_user_autosave_key(&e.key))
            );
            for key in [
                "plant_fact",
                "userXmsg_keep",
                "user_message",
                "assistant_resp_keep",
            ] {
                assert!(entries.iter().any(|e| e.key == key), "missing {key}");
            }
            assert!(entries.iter().all(|e| e.session_id.as_deref() == SESSION));
        }
    }
}

#[cfg(feature = "memory-postgres")]
#[tokio::test]
#[ignore = "requires disposable ZEROCLAW_TEST_POSTGRES_URL"]
async fn postgres_autosaves_do_not_crowd_out_prompt_facts() {
    let url = std::env::var("ZEROCLAW_TEST_POSTGRES_URL").expect("set disposable PostgreSQL URL");
    let schema = format!("zc_prompt_{}", uuid::Uuid::new_v4().simple());
    eprintln!("Disposable PostgreSQL test schema: {schema}");
    let backend = zeroclaw_memory::PostgresMemory::new(
        "prompt-test",
        &url,
        &schema,
        "memories",
        Some(5),
        Some(false),
        None,
    )
    .unwrap();
    // No pre-existing schema is dropped or reused. The disposable database
    // owns cleanup, including after assertion failures.
    assert_prompt_delivery(backend).await;
}

struct RankedEmbedding;

#[async_trait::async_trait]
impl zeroclaw_memory::embeddings::EmbeddingProvider for RankedEmbedding {
    fn name(&self) -> &str {
        "prompt-test"
    }
    fn dimensions(&self) -> usize {
        2
    }
    async fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|text| {
                if *text == "fallback-query" {
                    Vec::new()
                } else if text.contains("prefers shade") {
                    vec![0.8, 0.6]
                } else {
                    vec![1.0, 0.0]
                }
            })
            .collect())
    }
}

#[tokio::test]
#[ignore = "requires disposable ZEROCLAW_TEST_QDRANT_URL"]
async fn qdrant_autosaves_do_not_crowd_out_prompt_facts() {
    let url = std::env::var("ZEROCLAW_TEST_QDRANT_URL").expect("set disposable Qdrant URL");
    let collection = format!("zc_prompt_{}", uuid::Uuid::new_v4().simple());
    eprintln!("Disposable Qdrant test collection: {collection}");
    let backend = zeroclaw_memory::QdrantMemory::new(
        "prompt-test",
        &url,
        &collection,
        None,
        Arc::new(RankedEmbedding),
    )
    .await
    .unwrap();
    assert_prompt_delivery(backend).await;
    // Reopen the same real collection with an embedder that returns no vector
    // for this query: exercise the production bounded-scroll fallback as well.
    let backend = zeroclaw_memory::QdrantMemory::new(
        "prompt-test",
        &url,
        &collection,
        None,
        Arc::new(RankedEmbedding),
    )
    .await
    .unwrap();
    let entries = backend
        .recall_for_prompt_for_agents(&["own"], "fallback-query", 64, SESSION)
        .await
        .unwrap();
    assert_eq!(entries.len(), 4);
    assert!(
        entries
            .iter()
            .all(|e| !zeroclaw_memory::is_user_autosave_key(&e.key))
    );
    assert!(entries.iter().any(|e| e.key == "plant_fact"));
    for query in ["*", "fallback-query"] {
        let one = backend
            .recall_for_prompt_for_agents(&["own"], query, 1, SESSION)
            .await
            .unwrap();
        assert_eq!(one.len(), 1);
        assert!(!zeroclaw_memory::is_user_autosave_key(&one[0].key));
        assert_eq!(one[0].agent_id.as_deref(), Some("own"));
        assert_eq!(one[0].session_id.as_deref(), SESSION);
    }
    // Unique collection retained for inspection; discard the test service to clean up.
}
