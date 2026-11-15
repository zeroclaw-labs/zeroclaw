use super::*;
use tempfile::TempDir;

fn row(
    id: &str,
    agent: Option<&str>,
    session: Option<&str>,
    namespace: Option<&str>,
    timestamp: &str,
) -> portable_kernel::MemoryEntry {
    let category = MemoryCategory::Daily;
    portable_kernel::MemoryEntry {
        id: id.into(),
        path: TachiMemory::storage_path(agent, namespace, &category, id),
        summary: format!("Summary for {id}"),
        text: format!("{id}: {}", "retained body ".repeat(160)),
        importance: 0.7,
        timestamp: timestamp.into(),
        valid_from: timestamp.into(),
        valid_until: None,
        category: "daily".into(),
        topic: String::new(),
        keywords: vec![],
        persons: vec![],
        entities: vec![],
        location: String::new(),
        source: "zeroclaw".into(),
        scope: "general".into(),
        archived: false,
        access_count: 7,
        scored_count: 0,
        last_access: None,
        last_use_at: None,
        revision: 1,
        vector: None,
        retention_policy: None,
        domain: None,
        metadata: TachiMemory::build_metadata(
            id,
            &category,
            session,
            namespace,
            agent,
            &StoreOptions::default(),
        ),
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".into(),
    }
}

fn ids(entries: &[MemoryEntry]) -> Vec<&str> {
    entries.iter().map(|entry| entry.id.as_str()).collect()
}

#[tokio::test]
async fn host_scope_filters_precede_the_recent_limit() {
    let tmp = TempDir::new().unwrap();
    let memory = TachiMemory::new("tachi", tmp.path()).unwrap();
    let expected = {
        let mut store = memory.store_handle().lock();
        for index in 0..96 {
            // Each newer population violates one predicate. Omitting any SQL
            // filter would fill the limited page with rows the host rejects.
            let (agent, session, namespace, timestamp) = match index % 4 {
                0 => ("foreign", "A", "default", "2026-07-10T00:00:00Z"),
                1 => ("alpha", "B", "default", "2026-07-10T00:00:00Z"),
                2 => ("beta", "A", crate::SOUL_NAMESPACE, "2026-07-10T00:00:00Z"),
                _ => ("alpha", "A", "default", "2026-07-11T00:00:00Z"),
            };
            store
                .upsert(&row(
                    &format!("foreign-{index:03}"),
                    Some(agent),
                    Some(session),
                    Some(namespace),
                    timestamp,
                ))
                .unwrap();
        }
        for (id, agent) in [
            ("eligible-d", "alpha"),
            ("eligible-c", "beta"),
            ("eligible-a", "alpha"),
            ("eligible-b", "beta"),
        ] {
            store
                .upsert(&row(
                    id,
                    Some(agent),
                    Some("A"),
                    None,
                    "2026-07-09T00:00:00Z",
                ))
                .unwrap();
        }
        let mut archived = row(
            "archived",
            Some("alpha"),
            Some("A"),
            None,
            "2026-07-10T00:00:00Z",
        );
        archived.archived = true;
        store.upsert(&archived).unwrap();
        ["eligible-a", "eligible-b", "eligible-c"]
            .map(|id| TachiMemory::memcore_to_zeroclaw(store.get(id).unwrap().unwrap(), Some(1.0)))
            .to_vec()
    };

    let actual = memory
        .recall_for_agents(
            &["alpha", "beta"],
            "*",
            3,
            Some("A"),
            Some("2026-07-09T00:00:00.000Z"),
            Some("2026-07-10T00:00:00.000Z"),
        )
        .await
        .unwrap();
    assert_eq!(ids(&actual), ["eligible-a", "eligible-b", "eligible-c"]);
    assert_eq!(
        serde_json::to_value(&actual).unwrap(),
        serde_json::to_value(&expected).unwrap(),
        "selection must preserve every mapped host field"
    );
    assert_eq!(
        memory
            .store_handle()
            .lock()
            .get("eligible-a")
            .unwrap()
            .unwrap()
            .access_count,
        7
    );
}

#[tokio::test]
async fn namespace_selection_and_legacy_defaults_keep_host_semantics() {
    let tmp = TempDir::new().unwrap();
    let memory = TachiMemory::new("tachi", tmp.path()).unwrap();
    {
        let mut store = memory.store_handle().lock();
        for index in 0..32 {
            store
                .upsert(&row(
                    &format!("other-{index:02}"),
                    Some("other"),
                    None,
                    Some("other-space"),
                    "2026-07-10T00:00:00Z",
                ))
                .unwrap();
        }
        store
            .upsert(&row(
                "protected",
                None,
                None,
                Some(crate::SOUL_NAMESPACE),
                "2026-07-09T00:00:00Z",
            ))
            .unwrap();
        let mut legacy = row("legacy", None, Some(""), None, "2026-07-09T00:00:00Z");
        let metadata = legacy.metadata.as_object_mut().unwrap();
        metadata.remove(METADATA_ZC_AGENT);
        metadata.insert(METADATA_ZC_NAMESPACE.into(), serde_json::Value::Null);
        store.upsert(&legacy).unwrap();
    }
    let namespaced = memory
        .recall_namespaced(crate::SOUL_NAMESPACE, "*", 1, None, None, None)
        .await
        .unwrap();
    assert_eq!(ids(&namespaced), ["protected"]);
    let defaults = memory
        .recall_namespaced(DEFAULT_NAMESPACE, "*", 1, Some(""), None, None)
        .await
        .unwrap();
    assert_eq!(ids(&defaults), ["legacy"]);
    let admitted = memory
        .recall_for_agents(&[DEFAULT_AGENT], "*", 1, Some(""), None, None)
        .await
        .unwrap();
    assert_eq!(ids(&admitted), ["legacy"]);
    assert!(admitted[0].agent_id.is_none());
    assert_eq!(admitted[0].namespace, DEFAULT_NAMESPACE);

    let ambient = memory.recall("*", 100, None, None, None).await.unwrap();
    let empty_agents = memory
        .recall_for_agents(&[], "*", 100, None, None, None)
        .await
        .unwrap();
    assert_eq!(ambient.len(), 33);
    assert!(
        ambient
            .iter()
            .all(|entry| entry.namespace != crate::SOUL_NAMESPACE)
    );
    assert_eq!(
        serde_json::to_value(&empty_agents).unwrap(),
        serde_json::to_value(&ambient).unwrap(),
        "the existing empty-allowlist entry point delegates to ambient recall"
    );
}

#[tokio::test]
async fn large_agent_allowlists_merge_batches_before_the_global_limit() {
    let tmp = TempDir::new().unwrap();
    let memory = TachiMemory::new("tachi", tmp.path()).unwrap();
    let mut agents: Vec<String> = (0..300).map(|index| format!("agent-{index:03}")).collect();
    agents.push(agents[0].clone());
    {
        let mut store = memory.store_handle().lock();
        for (id, agent, session, namespace, timestamp, archived) in [
            (
                "newest",
                "agent-000",
                "A",
                "default",
                "2026-07-11T00:00:00Z",
                false,
            ),
            (
                "tie-c",
                "agent-001",
                "A",
                "default",
                "2026-07-10T00:00:00Z",
                false,
            ),
            (
                "tie-a",
                "agent-299",
                "A",
                "default",
                "2026-07-10T00:00:00Z",
                false,
            ),
            (
                "tie-b",
                "agent-298",
                "A",
                "default",
                "2026-07-10T00:00:00Z",
                false,
            ),
            (
                "too-old",
                "agent-299",
                "A",
                "default",
                "2026-07-09T00:00:00Z",
                false,
            ),
            (
                "wrong-session",
                "agent-299",
                "B",
                "default",
                "2026-07-13T00:00:00Z",
                false,
            ),
            (
                "soul",
                "agent-299",
                "A",
                crate::SOUL_NAMESPACE,
                "2026-07-13T00:00:00Z",
                false,
            ),
            (
                "archived",
                "agent-298",
                "A",
                "default",
                "2026-07-13T00:00:00Z",
                true,
            ),
            (
                "too-new",
                "agent-298",
                "A",
                "default",
                "2026-07-14T00:00:00Z",
                false,
            ),
            (
                "foreign",
                "intruder",
                "A",
                "default",
                "2026-07-13T00:00:00Z",
                false,
            ),
        ] {
            let mut entry = row(id, Some(agent), Some(session), Some(namespace), timestamp);
            entry.archived = archived;
            store.upsert(&entry).unwrap();
        }
    }
    let allowed: Vec<&str> = agents.iter().map(String::as_str).collect();
    let actual = memory
        .recall_for_agents(
            &allowed,
            "*",
            3,
            Some("A"),
            Some("2026-07-10T00:00:00.000Z"),
            Some("2026-07-13T00:00:00.000Z"),
        )
        .await
        .unwrap();
    assert_eq!(ids(&actual), ["newest", "tie-a", "tie-b"]);
}

#[tokio::test]
async fn recent_time_bounds_honor_inclusive_rfc3339_instants_literal_fallback_and_zero_limit() {
    let tmp = TempDir::new().unwrap();
    let memory = TachiMemory::new("tachi", tmp.path()).unwrap();
    {
        let mut store = memory.store_handle().lock();
        for (id, timestamp) in [
            ("at-start", "2026-07-09T00:00:00Z"),
            ("at-end", "2026-07-10T00:00:00Z"),
            ("too-new", "2026-07-11T00:00:00Z"),
        ] {
            store.upsert(&row(id, None, None, None, timestamp)).unwrap();
        }
    }
    for (since, until, expected) in [
        (
            "2026-07-09T00:00:00.000Z",
            "2026-07-10T00:00:00.000Z",
            vec!["at-end", "at-start"],
        ),
        (
            "2026-07-09T00:00:00Z",
            "2026-07-10T00:00:00Z",
            vec!["at-end", "at-start"],
        ),
        (
            "2026-07-09T08:00:00+08:00",
            "2026-07-10T08:00:00+08:00",
            vec!["at-end", "at-start"],
        ),
        ("2026-07-10", "2026-07-10T23:59:59Z", vec!["at-end"]),
    ] {
        let actual = memory
            .recall("*", 10, None, Some(since), Some(until))
            .await
            .unwrap();
        assert_eq!(ids(&actual), expected, "since={since} until={until}");
    }
    assert!(
        memory
            .recall("*", 0, None, None, None)
            .await
            .unwrap()
            .is_empty()
    );
}
