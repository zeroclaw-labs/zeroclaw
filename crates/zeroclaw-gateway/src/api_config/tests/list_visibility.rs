use super::*;

#[tokio::test]
async fn list_keeps_retention_controls_visible_across_backend_changes() {
    let tmp = tempfile::tempdir().expect("temporary config directory");
    let mut config = temp_config(&tmp);
    config
        .storage
        .markdown
        .insert("fixture".into(), Default::default());
    let state = test_state(config);

    // Keep the retention control available while reflecting the current selector.
    for backend in ["tachi", "markdown", "markdown.fixture", ""] {
        state.config.write().memory.backend = backend.into();
        let (status, listed) = response_json(
            handle_list(
                State(state.clone()),
                HeaderMap::new(),
                Query(ListQuery {
                    prefix: Some("memory".into()),
                }),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let entries = listed["entries"].as_array().expect("memory list entries");
        assert!(
            entries
                .iter()
                .any(|entry| entry["path"] == "memory.conversation_retention_days"),
            "retention visibility for backend {backend:?}"
        );
        let backend_entry = entries
            .iter()
            .find(|entry| entry["path"] == "memory.backend")
            .expect("backend selector stays visible");
        assert_eq!(backend_entry["value"], backend);
        assert!(
            entries
                .iter()
                .any(|entry| entry["path"] == "memory.auto_save")
        );
    }

    assert!(!tmp.path().join("config.toml").exists());
}

#[tokio::test]
async fn list_prefix_excludes_neighboring_agent_aliases() {
    let tmp = tempfile::tempdir().expect("temporary config directory");
    let mut config = temp_config(&tmp);
    config.agents.insert("aaa".into(), Default::default());
    config.agents.insert("aaalore".into(), Default::default());
    let state = test_state(config);

    let (status, all_agents) = response_json(
        handle_list(
            State(state.clone()),
            HeaderMap::new(),
            Query(ListQuery {
                prefix: Some("agents".into()),
            }),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let all_entries = all_agents["entries"].as_array().expect("list entries");
    for path in ["agents.aaa.enabled", "agents.aaalore.enabled"] {
        assert!(all_entries.iter().any(|entry| entry["path"] == path));
    }

    let (status, scoped) = response_json(
        handle_list(
            State(state),
            HeaderMap::new(),
            Query(ListQuery {
                prefix: Some("agents.aaa".into()),
            }),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let entries = scoped["entries"].as_array().expect("scoped list entries");
    assert!(
        entries
            .iter()
            .any(|entry| entry["path"] == "agents.aaa.enabled")
    );
    assert!(entries.iter().all(|entry| {
        entry["path"]
            .as_str()
            .expect("entry path")
            .starts_with("agents.aaa.")
    }));
}

#[tokio::test]
async fn list_requires_paired_bearer_and_omits_configured_secret_value() {
    const SYNTHETIC_SECRET: &str = "list-visibility-synthetic-provider-secret";
    const PROVIDER_PREFIX: &str = "providers.models.anthropic.visibility_fixture";
    const SECRET_PATH: &str = "providers.models.anthropic.visibility_fixture.api_key";

    let tmp = tempfile::tempdir().expect("temporary config directory");
    let mut config = temp_config(&tmp);
    config
        .providers
        .models
        .ensure("anthropic", "visibility_fixture")
        .expect("known provider family")
        .api_key = Some(SYNTHETIC_SECRET.into());
    let mut state = test_state(config);
    state.pairing = Arc::new(PairingGuard::new(true, &[]));

    let (status, unauthorized) = response_json(
        handle_list(
            State(state.clone()),
            HeaderMap::new(),
            Query(ListQuery {
                prefix: Some(PROVIDER_PREFIX.into()),
            }),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(unauthorized.get("entries").is_none());
    assert!(!unauthorized.to_string().contains(SYNTHETIC_SECRET));

    let code = state.pairing.pairing_code().expect("initial pairing code");
    let token = state
        .pairing
        .try_pair(&code, "list-visibility-test-client")
        .await
        .expect("pairing is not locked out")
        .expect("valid pairing code");
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        format!("Bearer {token}").parse().expect("bearer header"),
    );
    let (status, authorized) = response_json(
        handle_list(
            State(state),
            headers,
            Query(ListQuery {
                prefix: Some(PROVIDER_PREFIX.into()),
            }),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let secret = authorized["entries"]
        .as_array()
        .expect("authorized list entries")
        .iter()
        .find(|entry| entry["path"] == SECRET_PATH)
        .expect("configured provider secret is listed");
    assert_eq!(secret["is_secret"], true);
    assert_eq!(secret["populated"], true);
    assert!(secret.get("value").is_none());
    assert!(!authorized.to_string().contains(SYNTHETIC_SECRET));
    assert!(!tmp.path().join("config.toml").exists());
}
