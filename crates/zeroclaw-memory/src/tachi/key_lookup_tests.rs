use super::*;
use tempfile::TempDir;
use zeroclaw_api::TOOL_LOOP_MEMORY_SESSIONS;

#[tokio::test]
async fn point_reads_deletes_and_new_writes_do_not_grow_full_entry_pages() {
    let tmp = TempDir::new().unwrap();
    {
        let memory = TachiMemory::new("tachi", tmp.path()).unwrap();
        for i in 0..12 {
            memory
                .store(
                    &format!("key-{i}"),
                    &"payload ".repeat(8192),
                    MemoryCategory::Core,
                    None,
                )
                .await
                .unwrap();
        }
    }
    // Full-entry listing still fails closed at this deliberately tiny cap.
    // Point operations must not use that listing to find one logical key.
    let memory = TachiMemory::with_embedder_and_page_size(
        "tachi",
        tmp.path(),
        Arc::new(crate::embeddings::NoopEmbedding),
        0.7,
        0.3,
        2,
        2,
    )
    .unwrap();
    assert!(memory.list(None, None).await.is_err());
    assert_eq!(memory.get("key-11").await.unwrap().unwrap().key, "key-11");
    assert_eq!(
        memory
            .get_for_agent("key-11", "default")
            .await
            .unwrap()
            .unwrap()
            .key,
        "key-11"
    );
    assert!(memory.get("absent").await.unwrap().is_none());
    memory
        .store("new-key", "new value", MemoryCategory::Core, None)
        .await
        .unwrap();
    memory
        .store_with_metadata(
            "key-11",
            "updated value",
            MemoryCategory::Daily,
            None,
            Some("updated-namespace"),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        memory.get("key-11").await.unwrap().unwrap().content,
        "updated value"
    );
    assert!(memory.forget("key-11").await.unwrap());
    assert!(!memory.forget("absent").await.unwrap());
    assert!(memory.get("key-11").await.unwrap().is_none());
    assert_eq!(memory.count().await.unwrap(), 12);
}

#[tokio::test]
async fn metadata_projection_preserves_unicode_keys_and_archived_visibility() {
    let tmp = TempDir::new().unwrap();
    let memory = TachiMemory::new("tachi", tmp.path()).unwrap();
    let key = "quote%'_\"雪/..";
    for i in 0..12 {
        memory
            .store_with_agent(
                &format!("other-{i}"),
                "unrelated",
                MemoryCategory::Core,
                None,
                None,
                None,
                Some("alpha"),
            )
            .await
            .unwrap();
    }
    for session in ["A", "B"] {
        memory
            .store_with_agent(
                key,
                session,
                MemoryCategory::Conversation,
                Some(session),
                None,
                None,
                Some("alpha"),
            )
            .await
            .unwrap();
    }
    let a = TOOL_LOOP_MEMORY_SESSIONS
        .scope(vec!["A".into()], memory.get_for_agent(key, "alpha"))
        .await
        .unwrap()
        .unwrap();
    {
        let store = memory.store_handle().lock();
        let pattern = format!("{}%", TachiMemory::agent_path_prefix(Some("alpha")));
        let candidates = store.list_memories_by_path_prefix(&pattern).unwrap();
        assert_eq!(
            candidates.len(),
            14,
            "one complete projection includes unrelated metadata without bodies"
        );
        let matching = TachiMemory::rows_matching_key(&store, key, Some("alpha")).unwrap();
        assert_eq!(matching.len(), 2);
        assert!(
            matching
                .iter()
                .all(|row| row.text == "A" || row.text == "B")
        );
        store.archive_memory(&a.id).unwrap();
        let live = TachiMemory::rows_matching_key(&store, key, Some("alpha")).unwrap();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].text, "B");
    }
    assert!(
        TOOL_LOOP_MEMORY_SESSIONS
            .scope(vec!["A".into()], memory.get_for_agent(key, "alpha"))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        TOOL_LOOP_MEMORY_SESSIONS
            .scope(vec!["B".into()], memory.get_for_agent(key, "alpha"))
            .await
            .unwrap()
            .unwrap()
            .content,
        "B"
    );
}

#[tokio::test]
async fn legacy_metadata_keeps_session_precedence_and_complete_delete() {
    let tmp = TempDir::new().unwrap();
    let memory = TachiMemory::new("tachi", tmp.path()).unwrap();
    memory
        .store_with_agent(
            "shared",
            "legacy A",
            MemoryCategory::Conversation,
            Some("A"),
            None,
            None,
            Some("alpha"),
        )
        .await
        .unwrap();
    let a_id = TOOL_LOOP_MEMORY_SESSIONS
        .scope(vec!["A".into()], memory.get_for_agent("shared", "alpha"))
        .await
        .unwrap()
        .unwrap()
        .id;
    {
        let mut store = memory.store_handle().lock();
        let mut row = store.get(&a_id).unwrap().unwrap();
        row.path = format!(
            "{}historical/opaque",
            TachiMemory::agent_path_prefix(Some("alpha"))
        );
        store.upsert(&row).unwrap();
    }
    memory
        .store_with_agent(
            "shared",
            "global",
            MemoryCategory::Core,
            None,
            None,
            None,
            Some("alpha"),
        )
        .await
        .unwrap();
    assert_eq!(
        TOOL_LOOP_MEMORY_SESSIONS
            .scope(vec!["A".into()], memory.get_for_agent("shared", "alpha"))
            .await
            .unwrap()
            .unwrap()
            .content,
        "legacy A"
    );
    assert_eq!(
        TOOL_LOOP_MEMORY_SESSIONS
            .scope(vec!["B".into()], memory.get_for_agent("shared", "alpha"))
            .await
            .unwrap()
            .unwrap()
            .content,
        "global"
    );
    memory
        .store_with_agent(
            "shared",
            "A updated",
            MemoryCategory::Conversation,
            Some("A"),
            None,
            None,
            Some("alpha"),
        )
        .await
        .unwrap();
    let updated = TOOL_LOOP_MEMORY_SESSIONS
        .scope(vec!["A".into()], memory.get_for_agent("shared", "alpha"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated.id, a_id);
    assert_eq!(updated.content, "A updated");
    // A delete must include historical duplicates even when the canonical
    // path already matches. No shape/UUID is accepted as session authority.
    {
        let mut store = memory.store_handle().lock();
        let mut duplicate = store.get(&a_id).unwrap().unwrap();
        duplicate.id = Uuid::new_v4().to_string();
        duplicate.path = format!(
            "{}historical/duplicate",
            TachiMemory::agent_path_prefix(Some("alpha"))
        );
        store.upsert(&duplicate).unwrap();
    }
    assert!(
        TOOL_LOOP_MEMORY_SESSIONS
            .scope(vec!["A".into()], memory.forget_for_agent("shared", "alpha"))
            .await
            .unwrap()
    );
    let rows = memory.export_agent("alpha").await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].content, "global");
}

#[tokio::test]
async fn conversation_duplicates_keep_legacy_winner_and_session_delete_scope() {
    let tmp = TempDir::new().unwrap();
    let memory = TachiMemory::new("tachi", tmp.path()).unwrap();
    memory
        .store_with_agent(
            "shared",
            "global",
            MemoryCategory::Core,
            None,
            None,
            None,
            Some("alpha"),
        )
        .await
        .unwrap();
    for session in ["A", "B"] {
        memory
            .store_with_agent(
                "shared",
                session,
                MemoryCategory::Conversation,
                Some(session),
                None,
                None,
                Some("alpha"),
            )
            .await
            .unwrap();
    }
    let canonical = TOOL_LOOP_MEMORY_SESSIONS
        .scope(vec!["A".into()], memory.get_for_agent("shared", "alpha"))
        .await
        .unwrap()
        .unwrap();
    let (newer_id, winner_id) = {
        let mut store = memory.store_handle().lock();
        let mut duplicate = store.get(&canonical.id).unwrap().unwrap();
        duplicate.path = format!(
            "{}zz-historical/duplicate",
            TachiMemory::agent_path_prefix(Some("alpha"))
        );
        assert!(duplicate.path > store.get(&canonical.id).unwrap().unwrap().path);
        duplicate.id = Uuid::new_v4().to_string();
        duplicate.timestamp = "2025-01-02T00:00:00+00:00".into();
        duplicate.text = "newer legacy A".into();
        store.upsert(&duplicate).unwrap();
        let newer_id = duplicate.id.clone();
        duplicate.id = Uuid::new_v4().to_string();
        duplicate.timestamp = "2025-01-01T00:00:00+00:00".into();
        duplicate.text = "legacy A winner".into();
        store.upsert(&duplicate).unwrap();
        (newer_id, duplicate.id)
    };

    // Historical point reads sort path ASC, timestamp DESC, then select the
    // host scope and pop the last row. Canonical paths do not have priority.
    let winner = TOOL_LOOP_MEMORY_SESSIONS
        .scope(vec!["A".into()], memory.get_for_agent("shared", "alpha"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(winner.id, winner_id);
    assert_eq!(winner.content, "legacy A winner");
    assert_eq!(
        TOOL_LOOP_MEMORY_SESSIONS
            .scope(vec!["A".into()], memory.get("shared"))
            .await
            .unwrap()
            .unwrap()
            .id,
        winner_id
    );
    assert_eq!(
        TOOL_LOOP_MEMORY_SESSIONS
            .scope(vec!["B".into()], memory.get_for_agent("shared", "alpha"))
            .await
            .unwrap()
            .unwrap()
            .content,
        "B"
    );
    assert!(
        TOOL_LOOP_MEMORY_SESSIONS
            .scope(vec!["A".into()], memory.forget_for_agent("shared", "alpha"))
            .await
            .unwrap()
    );
    {
        let store = memory.store_handle().lock();
        for id in [&canonical.id, &newer_id, &winner_id] {
            assert!(store.get(id).unwrap().is_none());
        }
    }
    assert_eq!(
        TOOL_LOOP_MEMORY_SESSIONS
            .scope(vec!["B".into()], memory.get_for_agent("shared", "alpha"))
            .await
            .unwrap()
            .unwrap()
            .content,
        "B"
    );
    assert_eq!(
        TOOL_LOOP_MEMORY_SESSIONS
            .scope(vec!["A".into()], memory.get_for_agent("shared", "alpha"))
            .await
            .unwrap()
            .unwrap()
            .content,
        "global"
    );
}

#[tokio::test]
async fn unscoped_global_duplicates_keep_legacy_winner_and_complete_agent_delete() {
    let tmp = TempDir::new().unwrap();
    let memory = TachiMemory::new("tachi", tmp.path()).unwrap();
    for agent in ["alpha", "beta"] {
        memory
            .store_with_agent(
                "shared",
                agent,
                MemoryCategory::Core,
                None,
                None,
                None,
                Some(agent),
            )
            .await
            .unwrap();
    }
    let canonical = memory
        .get_for_agent("shared", "alpha")
        .await
        .unwrap()
        .unwrap();
    let winner_id = {
        let mut store = memory.store_handle().lock();
        let mut duplicate = store.get(&canonical.id).unwrap().unwrap();
        duplicate.id = Uuid::new_v4().to_string();
        duplicate.path = format!(
            "{}zz-historical/global",
            TachiMemory::agent_path_prefix(Some("alpha"))
        );
        duplicate.text = "legacy global winner".into();
        store.upsert(&duplicate).unwrap();
        duplicate.id
    };
    // No task-local host scope: keep the historical administrative winner.
    let winner = memory
        .get_for_agent("shared", "alpha")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(winner.id, winner_id);
    assert_eq!(winner.content, "legacy global winner");
    assert!(memory.forget_for_agent("shared", "alpha").await.unwrap());
    {
        let store = memory.store_handle().lock();
        for id in [&canonical.id, &winner_id] {
            assert!(store.get(id).unwrap().is_none());
        }
    }
    assert!(
        memory
            .get_for_agent("shared", "alpha")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        memory
            .get_for_agent("shared", "beta")
            .await
            .unwrap()
            .unwrap()
            .content,
        "beta"
    );
}

#[tokio::test]
async fn writes_keep_exact_path_priority_and_legacy_fallback_identity_order() {
    let tmp = TempDir::new().unwrap();
    let memory = TachiMemory::new("tachi", tmp.path()).unwrap();
    memory
        .store("shared", "canonical", MemoryCategory::Core, None)
        .await
        .unwrap();
    let canonical = memory.get("shared").await.unwrap().unwrap();
    let legacy_id = {
        let mut store = memory.store_handle().lock();
        let mut duplicate = store.get(&canonical.id).unwrap().unwrap();
        duplicate.id = Uuid::new_v4().to_string();
        duplicate.path = format!(
            "{}aaa-historical/first",
            TachiMemory::agent_path_prefix(Some("default"))
        );
        assert!(duplicate.path < store.get(&canonical.id).unwrap().unwrap().path);
        duplicate.text = "legacy first".into();
        store.upsert(&duplicate).unwrap();
        duplicate.id
    };
    memory
        .store("shared", "exact updated", MemoryCategory::Core, None)
        .await
        .unwrap();
    {
        let store = memory.store_handle().lock();
        assert_eq!(
            store.get(&canonical.id).unwrap().unwrap().text,
            "exact updated"
        );
        assert_eq!(store.get(&legacy_id).unwrap().unwrap().text, "legacy first");
    }
    // Moving namespace/category misses the exact path. Preserve the first
    // complete path-ordered identity, even with a canonical duplicate present.
    memory
        .store_with_metadata(
            "shared",
            "moved legacy",
            MemoryCategory::Daily,
            None,
            Some("updated-namespace"),
            None,
        )
        .await
        .unwrap();
    let store = memory.store_handle().lock();
    assert_eq!(
        store.get(&canonical.id).unwrap().unwrap().text,
        "exact updated"
    );
    let updated = store.get(&legacy_id).unwrap().unwrap();
    assert_eq!(updated.text, "moved legacy");
    assert_eq!(updated.metadata[METADATA_ZC_NAMESPACE], "updated-namespace");
    assert_eq!(updated.metadata[METADATA_ZC_CATEGORY], "daily");
}

#[tokio::test]
async fn candidate_path_never_overrides_exact_agent_and_key_metadata() {
    let tmp = TempDir::new().unwrap();
    let memory = TachiMemory::new("tachi", tmp.path()).unwrap();
    memory
        .store_with_agent(
            "shared",
            "foreign",
            MemoryCategory::Core,
            None,
            None,
            None,
            Some("beta"),
        )
        .await
        .unwrap();
    let foreign = memory
        .get_for_agent("shared", "beta")
        .await
        .unwrap()
        .unwrap();
    {
        let mut store = memory.store_handle().lock();
        let mut row = store.get(&foreign.id).unwrap().unwrap();
        row.path = TachiMemory::storage_path(Some("alpha"), None, &MemoryCategory::Core, "shared");
        store.upsert(&row).unwrap();
    }
    assert!(
        memory
            .get_for_agent("shared", "alpha")
            .await
            .unwrap()
            .is_none()
    );
    assert!(!memory.forget_for_agent("shared", "alpha").await.unwrap());
    memory
        .store_with_agent(
            "shared",
            "own",
            MemoryCategory::Core,
            None,
            None,
            None,
            Some("alpha"),
        )
        .await
        .unwrap();
    let own = memory
        .get_for_agent("shared", "alpha")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(own.content, "own");
    assert_ne!(own.id, foreign.id);
    assert!(
        memory
            .store_handle()
            .lock()
            .get(&foreign.id)
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn streamed_key_lookup_isolates_like_wildcards_case_and_corrupt_metadata() {
    let tmp = TempDir::new().unwrap();
    let memory = TachiMemory::new("tachi", tmp.path()).unwrap();

    for (agent, key, content) in [
        ("a_one", "k_1", "agent a_one key k_1"),
        ("a_one", "k%1", "agent a_one key k%1"),
        ("a_one", "kx1", "agent a_one key kx1"),
        ("axone", "k_1", "agent axone key k_1"),
        ("A_one", "k_1", "agent A_one key k_1"),
    ] {
        memory
            .store_with_agent(
                key,
                content,
                MemoryCategory::Core,
                None,
                None,
                None,
                Some(agent),
            )
            .await
            .unwrap();
    }

    assert_eq!(
        memory
            .get_for_agent("k_1", "a_one")
            .await
            .unwrap()
            .unwrap()
            .content,
        "agent a_one key k_1"
    );
    assert_eq!(
        memory
            .get_for_agent("k%1", "a_one")
            .await
            .unwrap()
            .unwrap()
            .content,
        "agent a_one key k%1"
    );
    assert_eq!(
        memory
            .get_for_agent("k_1", "axone")
            .await
            .unwrap()
            .unwrap()
            .content,
        "agent axone key k_1"
    );
    assert_eq!(
        memory
            .get_for_agent("k_1", "A_one")
            .await
            .unwrap()
            .unwrap()
            .content,
        "agent A_one key k_1"
    );
    assert!(
        memory
            .get_for_agent("k%1", "axone")
            .await
            .unwrap()
            .is_none()
    );

    // Archive one row and confirm it stays hidden before adding an active legacy duplicate.
    let a_one_row = memory.get_for_agent("kx1", "a_one").await.unwrap().unwrap();
    let mut duplicate = {
        let store = memory.store_handle().lock();
        let entry = store.get(&a_one_row.id).unwrap().unwrap();
        store.archive_memory(&a_one_row.id).unwrap();
        entry
    };
    assert!(
        memory
            .get_for_agent("kx1", "a_one")
            .await
            .unwrap()
            .is_none(),
        "archived row with no active duplicate must stay hidden"
    );
    {
        let mut store = memory.store_handle().lock();
        duplicate.id = Uuid::new_v4().to_string();
        duplicate.path = format!(
            "{}legacy/kx1",
            TachiMemory::agent_path_prefix(Some("a_one"))
        );
        duplicate.text = "legacy active kx1".into();
        store.upsert(&duplicate).unwrap();
    }
    assert_eq!(
        memory
            .get_for_agent("kx1", "a_one")
            .await
            .unwrap()
            .unwrap()
            .content,
        "legacy active kx1"
    );

    // Insert spoofed rows sharing `a_one`'s exact hash suffix and metadata, differing
    // only in `_` wildcard (`axone`) and ASCII case (`A_one`). Binary prefix matching
    // must ignore both.
    let a_one_prefix = TachiMemory::agent_path_prefix(Some("a_one"));
    let k_1_id = memory
        .get_for_agent("k_1", "a_one")
        .await
        .unwrap()
        .unwrap()
        .id;
    {
        let mut store = memory.store_handle().lock();
        let base_entry = store.get(&k_1_id).unwrap().unwrap();
        for (replacement, spoof_key) in [
            ("/agents/axone__", "spoof_only_wildcard"),
            ("/agents/A_one__", "spoof_only_case"),
        ] {
            let spoof_prefix = a_one_prefix.replacen("/agents/a_one__", replacement, 1);
            assert_ne!(spoof_prefix, a_one_prefix);
            let mut spoof = base_entry.clone();
            spoof.id = Uuid::new_v4().to_string();
            spoof.path = format!("{spoof_prefix}spoofed/k_1");
            spoof.text = "spoofed path".into();
            store.upsert(&spoof).unwrap();

            // Distinct key absent from real `a_one` rows prevents ASCII `path ASC`
            // ordering + `pop()` from masking a prefix-check regression.
            let mut isolated_spoof = base_entry.clone();
            isolated_spoof.id = Uuid::new_v4().to_string();
            isolated_spoof.path = format!("{spoof_prefix}spoofed/{spoof_key}");
            isolated_spoof.text = "spoofed isolated path".into();
            isolated_spoof.metadata[METADATA_ZC_KEY] = serde_json::json!(spoof_key);
            store.upsert(&isolated_spoof).unwrap();
        }
        assert_eq!(
            TachiMemory::rows_matching_key(&store, "k_1", Some("a_one"))
                .unwrap()
                .len(),
            1
        );
    }
    for spoof_key in ["spoof_only_wildcard", "spoof_only_case"] {
        assert!(
            memory
                .get_for_agent(spoof_key, "a_one")
                .await
                .unwrap()
                .is_none(),
            "spoofed prefix for {spoof_key} must never match a_one"
        );
    }
    assert_eq!(
        memory
            .get_for_agent("k_1", "a_one")
            .await
            .unwrap()
            .unwrap()
            .content,
        "agent a_one key k_1"
    );

    // Inject corrupt metadata inside `axone`'s literal prefix: `axone` must fail
    // closed even when querying an unrelated key, while `a_one` stays healthy.
    memory
        .store_with_agent(
            "poison_target",
            "poison",
            MemoryCategory::Core,
            None,
            None,
            None,
            Some("axone"),
        )
        .await
        .unwrap();
    let poison_id = memory
        .get_for_agent("poison_target", "axone")
        .await
        .unwrap()
        .unwrap()
        .id;
    let db_path = tmp.path().join("memory").join("tachi.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch("DROP TRIGGER IF EXISTS memories_reserved_refs_update_guard;")
            .unwrap();
        conn.execute(
            "UPDATE memories SET metadata = '{corrupt' WHERE id = ?1",
            [&poison_id],
        )
        .unwrap();
    }
    let err = memory
        .get_for_agent("k_1", "axone")
        .await
        .expect_err("corrupt metadata in literal agent prefix must fail closed");
    assert!(err.to_string().contains("key candidate lookup failed"));
    assert_eq!(
        memory
            .get_for_agent("k_1", "a_one")
            .await
            .unwrap()
            .unwrap()
            .content,
        "agent a_one key k_1"
    );
}
