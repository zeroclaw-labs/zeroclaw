use zeroclaw_rpc_proto::feature;

struct LegacyPersonalityCore {
    endpoint: PathBuf,
    methods: Arc<std::sync::Mutex<Vec<String>>>,
    cancel: CancellationToken,
    listener: tokio::task::JoinHandle<()>,
}

impl LegacyPersonalityCore {
    fn start(endpoint: PathBuf, core: PathBuf, missing: Option<&'static str>) -> Self {
        Self::serve(endpoint, core, missing, None)
    }

    fn start_skewed(endpoint: PathBuf, core: PathBuf, missing: &'static str) -> Self {
        Self::serve(endpoint, core, Some(missing), Some("0.0.0-older"))
    }

    fn serve(
        endpoint: PathBuf,
        core: PathBuf,
        missing: Option<&'static str>,
        reported_version: Option<&'static str>,
    ) -> Self {
        use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
        let listener = tokio::net::UnixListener::bind(&endpoint).unwrap();
        let methods = Arc::new(std::sync::Mutex::new(Vec::new()));
        let cancel = CancellationToken::new();
        let task = {
            let methods = Arc::clone(&methods);
            let cancel = cancel.clone();
            zeroclaw_spawn::spawn!(async move {
                loop {
                    let stream = tokio::select! {
                        () = cancel.cancelled() => break,
                        accepted = listener.accept() => accepted.unwrap().0,
                    };
                    let core = core.clone();
                    let methods = Arc::clone(&methods);
                    let cancel = cancel.clone();
                    zeroclaw_spawn::spawn!(async move {
                        let upstream = tokio::net::UnixStream::connect(core).await.unwrap();
                        let (client_read, mut client_write) = stream.into_split();
                        let (core_read, mut core_write) = upstream.into_split();
                        let requests = async {
                            let mut lines = BufReader::new(client_read).lines();
                            while let Some(line) = lines.next_line().await? {
                                let mut frame: serde_json::Value =
                                    serde_json::from_str(&line).unwrap();
                                let method =
                                    frame["method"].as_str().unwrap_or_default().to_owned();
                                methods.lock().unwrap().push(method.clone());
                                if let Some(params) = frame["params"].as_object_mut() {
                                    let lacks = |name| missing.is_none() || missing == Some(name);
                                    match method.as_str() {
                                        "skills/delete" if lacks(feature::SKILLS_DELETE_PURGE) => {
                                            params.remove("purge");
                                        }
                                        "personality/list" | "personality/get"
                                        | "personality/put" => {
                                            if lacks(feature::PERSONALITY_CONFIGURED_AGENT) {
                                                params.remove("require_configured_agent");
                                            }
                                            if lacks(feature::PERSONALITY_EXPECTED_MTIME) {
                                                params.remove("expected_mtime_ms");
                                            }
                                            if lacks(feature::PERSONALITY_MAX_CHARS) {
                                                params.remove("max_chars");
                                            }
                                        }
                                        "personality/templates"
                                            if lacks(feature::PERSONALITY_EDITOR_TEMPLATES) =>
                                        {
                                            for field in [
                                                "preset",
                                                "agent_name",
                                                "user_name",
                                                "timezone",
                                                "communication_style",
                                                "include_memory",
                                                "defaults",
                                            ] {
                                                params.remove(field);
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                                let mut bytes = serde_json::to_vec(&frame).unwrap();
                                bytes.push(b'\n');
                                core_write.write_all(&bytes).await?;
                            }
                            Ok::<(), std::io::Error>(())
                        };
                        let responses = async {
                            let mut lines = BufReader::new(core_read).lines();
                            while let Some(line) = lines.next_line().await? {
                                let mut frame: serde_json::Value =
                                    serde_json::from_str(&line).unwrap();
                                if let Some(result) = frame
                                    .get_mut("result")
                                    .and_then(serde_json::Value::as_object_mut)
                                    && result.contains_key("server_pid")
                                    && result.contains_key("server_version")
                                {
                                    if let Some(version) = reported_version {
                                        result.insert(
                                            "server_version".into(),
                                            serde_json::Value::from(version),
                                        );
                                    }
                                    if let Some(missing) = missing {
                                        if let Some(features) = result
                                            .get_mut("features")
                                            .and_then(serde_json::Value::as_array_mut)
                                        {
                                            features.retain(|feature| {
                                                feature.as_str() != Some(missing)
                                            });
                                        }
                                    } else {
                                        result.remove("features");
                                    }
                                }
                                let mut bytes = serde_json::to_vec(&frame).unwrap();
                                bytes.push(b'\n');
                                client_write.write_all(&bytes).await?;
                            }
                            Ok::<(), std::io::Error>(())
                        };
                        tokio::select! {
                            () = cancel.cancelled() => {},
                            _ = async { tokio::try_join!(requests, responses) } => {},
                        }
                    });
                }
            })
        };
        Self {
            endpoint,
            methods,
            cancel,
            listener: task,
        }
    }

    fn router(&self, skew: VersionSkew) -> Router {
        router(
            CoreRpc::local(self.endpoint.clone(), EndpointOwner::SameAccount, skew),
            self.endpoint.clone(),
            None,
            watch::channel(false).0,
            Duration::from_secs(crate::REQUEST_TIMEOUT_SECS),
        )
    }

    fn saw_extension_method(&self) -> bool {
        self.methods
            .lock()
            .unwrap()
            .iter()
            .any(|method| method.starts_with("personality/") || method == "skills/delete")
    }
}

impl Drop for LegacyPersonalityCore {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.listener.abort();
    }
}

async fn legacy_personality_fixture(dir: &Path) -> (Core, zeroclaw_config::schema::Config) {
    use zeroclaw_runtime::skills::{ScaffoldOptions, SkillFrontmatter, SkillsService};
    let ctx = Core::context(dir);
    let mut config = ctx.config.read().clone();
    let additions: zeroclaw_config::schema::Config =
        toml::from_str("[skill_bundles.team]\n[agents.main]\n").unwrap();
    config.agents = additions.agents;
    config.skill_bundles = additions.skill_bundles;
    *ctx.config.write() = config.clone();
    let service = SkillsService::new(&config, config.install_root_dir());
    let skill = service.resolve_ref("purge-one", Some("team")).unwrap();
    service
        .scaffold_skill(
            &skill,
            SkillFrontmatter {
                name: "purge-one".into(),
                description: "capability fixture".into(),
                ..Default::default()
            },
            ScaffoldOptions::default(),
        )
        .unwrap();
    let workspace = config.agent_workspace_dir("main");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("SOUL.md"), "current fixture\n").unwrap();
    (Core::serve(ctx).await, config)
}

async fn personality_json(
    router: &Router,
    method: &str,
    path: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("authorization", format!("Bearer {TOKEN}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn same_version_legacy_core_cannot_corrupt_personality_or_purge() {
    let tmp = tempfile::tempdir().unwrap();
    let (core, config) = legacy_personality_fixture(tmp.path()).await;
    let legacy =
        LegacyPersonalityCore::start(tmp.path().join("legacy.sock"), core.endpoint.clone(), None);
    let router = legacy.router(VersionSkew::Refuse);
    let (status, diagnostic) = get(&router, CORE_LINK_PATH, Some(TOKEN)).await;
    assert_eq!(status, StatusCode::OK);
    let diagnostic = json_of(&diagnostic);
    assert_eq!(
        diagnostic["core"]["server_version"],
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(diagnostic["core"]["features"], serde_json::json!([]));
    let stale = personality_json(
        &router,
        "PUT",
        "/api/personality/SOUL.md?agent=main",
        serde_json::json!({"content":"stale overwrite", "expected_mtime_ms":1}),
    )
    .await;
    let ghost = personality_json(
        &router,
        "PUT",
        "/api/personality/SOUL.md?agent=missing",
        serde_json::json!({"content":"unexpected workspace"}),
    )
    .await;
    let (purge_status, purge_body) = send(
        &router,
        "DELETE",
        "/api/skills/bundles/team/skills/purge-one?purge=true",
        Some(TOKEN),
    )
    .await;
    let on_disk =
        std::fs::read_to_string(config.agent_workspace_dir("main").join("SOUL.md")).unwrap();
    let ghost_written = config
        .agent_workspace_dir("missing")
        .join("SOUL.md")
        .exists();
    let archived = std::fs::read_dir(config.install_root_dir().join("shared/skills/_deleted"))
        .is_ok_and(|entries| {
            entries
                .flatten()
                .any(|entry| entry.file_name().to_string_lossy().contains("purge-one"))
        });
    assert!(
        stale.0 == StatusCode::SERVICE_UNAVAILABLE
            && ghost.0 == StatusCode::SERVICE_UNAVAILABLE
            && purge_status == StatusCode::SERVICE_UNAVAILABLE
            && on_disk == "current fixture\n"
            && !ghost_written
            && !archived
            && !legacy.saw_extension_method(),
        "stale_status={}, stale_written={}, unknown_status={}, unknown_written={ghost_written}, purge_status={purge_status}, archived={archived}, dispatched={}",
        stale.0,
        on_disk == "stale overwrite",
        ghost.0,
        legacy.saw_extension_method()
    );
    for body in [stale.1, ghost.1, json_of(&purge_body)] {
        assert_eq!(body["code"], "core_capability_missing");
    }
    drop(router);
    drop(legacy);
    core.stop().await;
}

#[tokio::test]
async fn same_version_legacy_core_is_refused_before_an_unbounded_personality_read() {
    let tmp = tempfile::tempdir().unwrap();
    let (core, config) = legacy_personality_fixture(tmp.path()).await;
    std::fs::write(
        config.agent_workspace_dir("main").join("SOUL.md"),
        "x".repeat(8 * 1024 * 1024 + 1024),
    )
    .unwrap();
    let legacy = LegacyPersonalityCore::start(
        tmp.path().join("legacy.sock"),
        core.endpoint.clone(),
        Some(feature::PERSONALITY_MAX_CHARS),
    );
    let router = legacy.router(VersionSkew::Refuse);
    let (status, body) = get(&router, "/api/personality/SOUL.md?agent=main", Some(TOKEN)).await;
    assert!(
        status == StatusCode::SERVICE_UNAVAILABLE
            && json_of(&body)["code"] == "core_capability_missing"
            && !legacy.saw_extension_method(),
        "status={status}, code={}, personality_dispatched={}",
        json_of(&body)["code"],
        legacy.saw_extension_method()
    );
    drop(router);
    drop(legacy);
    core.stop().await;
}

#[tokio::test]
async fn one_missing_extension_is_refused_even_when_version_skew_is_allowed() {
    for (missing, method, route, body) in [
        (
            feature::PERSONALITY_CONFIGURED_AGENT,
            "GET",
            "/api/personality?agent=missing",
            serde_json::Value::Null,
        ),
        (
            feature::PERSONALITY_CONFIGURED_AGENT,
            "GET",
            "/api/personality/SOUL.md?agent=missing",
            serde_json::Value::Null,
        ),
        (
            feature::PERSONALITY_CONFIGURED_AGENT,
            "PUT",
            "/api/personality/SOUL.md?agent=missing",
            serde_json::json!({"content":"unexpected workspace"}),
        ),
        (
            feature::PERSONALITY_EXPECTED_MTIME,
            "PUT",
            "/api/personality/SOUL.md?agent=main",
            serde_json::json!({"content":"stale overwrite","expected_mtime_ms":1}),
        ),
        (
            feature::SKILLS_DELETE_PURGE,
            "DELETE",
            "/api/skills/bundles/team/skills/purge-one?purge=true",
            serde_json::Value::Null,
        ),
        (
            feature::PERSONALITY_EDITOR_TEMPLATES,
            "GET",
            "/api/personality/templates?agent=main&agent_name=override",
            serde_json::Value::Null,
        ),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let (core, config) = legacy_personality_fixture(tmp.path()).await;
        let legacy = LegacyPersonalityCore::start_skewed(
            tmp.path().join("legacy.sock"),
            core.endpoint.clone(),
            missing,
        );
        let refused = legacy.router(VersionSkew::Refuse);
        let (status, refusal_body) = get(&refused, CORE_LINK_PATH, Some(TOKEN)).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(json_of(&refusal_body)["code"], "core_version_mismatch");
        drop(refused);
        let router = legacy.router(VersionSkew::Allow);
        let (status, body) = personality_json(&router, method, route, body).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "missing {missing}");
        assert_eq!(body["code"], "core_capability_missing", "missing {missing}");
        assert!(body["error"].as_str().unwrap().contains(missing), "{body}");
        assert!(!legacy.saw_extension_method(), "missing {missing}");
        assert_eq!(
            std::fs::read_to_string(config.agent_workspace_dir("main").join("SOUL.md")).unwrap(),
            "current fixture\n"
        );
        assert!(
            !config
                .agent_workspace_dir("missing")
                .join("SOUL.md")
                .exists()
        );
        assert!(
            config
                .install_root_dir()
                .join("shared/skills/team/purge-one")
                .is_dir()
        );
        drop(router);
        drop(legacy);
        core.stop().await;
    }
}

#[tokio::test]
async fn a_core_without_extension_features_can_still_archive_a_skill() {
    let tmp = tempfile::tempdir().unwrap();
    let (core, config) = legacy_personality_fixture(tmp.path()).await;
    let legacy =
        LegacyPersonalityCore::start(tmp.path().join("legacy.sock"), core.endpoint.clone(), None);
    let router = legacy.router(VersionSkew::Refuse);
    let (status, _) = send(
        &router,
        "DELETE",
        "/api/skills/bundles/team/skills/purge-one",
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(
        !config
            .install_root_dir()
            .join("shared/skills/team/purge-one")
            .exists()
    );
    assert!(
        std::fs::read_dir(config.install_root_dir().join("shared/skills/_deleted"))
            .unwrap()
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().contains("purge-one"))
    );
    drop(router);
    drop(legacy);
    core.stop().await;
}

#[tokio::test]
async fn a_write_without_a_drift_guard_does_not_require_the_mtime_extension() {
    let tmp = tempfile::tempdir().unwrap();
    let (core, config) = legacy_personality_fixture(tmp.path()).await;
    let legacy = LegacyPersonalityCore::start(
        tmp.path().join("legacy.sock"),
        core.endpoint.clone(),
        Some(feature::PERSONALITY_EXPECTED_MTIME),
    );
    let router = legacy.router(VersionSkew::Refuse);
    let (status, _) = personality_json(
        &router,
        "PUT",
        "/api/personality/SOUL.md?agent=main",
        serde_json::json!({"content":"intentional unguarded write"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        std::fs::read_to_string(config.agent_workspace_dir("main").join("SOUL.md")).unwrap(),
        "intentional unguarded write"
    );
    drop(router);
    drop(legacy);
    core.stop().await;
}
