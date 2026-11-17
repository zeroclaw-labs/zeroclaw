use super::*;

struct SopEntryProbe {
    name: String,
    arguments: Value,
    result: Arc<std::sync::Mutex<Option<String>>>,
}

#[async_trait]
impl zeroclaw_api::model_provider::ModelProvider for SopEntryProbe {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        anyhow::bail!("the probe must use the tool-capable chat path")
    }

    async fn chat(
        &self,
        request: zeroclaw_providers::ChatRequest<'_>,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<zeroclaw_providers::ChatResponse> {
        if let Some(result) = request.messages.iter().rev().find(|m| m.role == "tool") {
            *self.result.lock().unwrap() = Some(result.content.clone());
            return Ok(zeroclaw_providers::ChatResponse {
                text: Some("probe finished".into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            });
        }
        // Request the forbidden name even when the registry does not advertise
        // it: omission from the prompt alone must not be the security boundary.
        Ok(zeroclaw_providers::ChatResponse {
            text: None,
            tool_calls: vec![zeroclaw_providers::ToolCall {
                id: "sop-entry-probe".into(),
                name: self.name.clone(),
                arguments: self.arguments.to_string(),
                extra_content: None,
            }],
            usage: None,
            reasoning_content: None,
        })
    }
}

impl zeroclaw_api::attribution::Attributable for SopEntryProbe {
    fn role(&self) -> zeroclaw_api::attribution::Role {
        zeroclaw_api::attribution::Role::Provider(zeroclaw_api::attribution::ProviderKind::Model(
            zeroclaw_api::attribution::ModelProviderKind::Custom,
        ))
    }

    fn alias(&self) -> &str {
        "sop-entry-probe"
    }
}

#[tokio::test]
async fn sop_entry_tools_and_aliases_cannot_escape_principal_ceilings() {
    use zeroclaw_infra::session_queue::SessionActorQueue;

    for target in ["sop_execute", "sop_approve", "sop_advance"] {
        for alias in [false, true] {
            for posture in ["tool-scoped", "agent-scoped", "wildcard", "admin"] {
                // A permitted execute control parks at approval, so it reaches
                // the real engine without starting a headless provider turn.
                if target != "sop_execute" && posture == "admin" {
                    continue;
                }
                let tmp = tempfile::TempDir::new().unwrap();
                let mut config = principal_test_config(&tmp, &["*"], &["*"]);
                // The shared fixture selects a minimal built-in set; select the
                // entry tool so the unrestricted control arm constructs it.
                config.tools.optional.push(target.into());
                config
                    .permission_profiles
                    .get_mut("principal-test")
                    .unwrap()
                    .admin = true;
                let target_config = config.agents["test-agent"].clone();
                config.agents.insert("target-agent".into(), target_config);
                let name = if alias { "sop_guard__invoke" } else { target };
                let profile = config.risk_profiles.get_mut("test-profile").unwrap();
                profile.level = zeroclaw_config::autonomy::AutonomyLevel::Full;
                profile.allowed_tools = vec![target.into(), name.into(), "calculator".into()];
                let skill_dir = config
                    .agent_workspace_dir("test-agent")
                    .join("skills/sop_guard");
                std::fs::create_dir_all(&skill_dir).unwrap();
                std::fs::write(skill_dir.join("SKILL.toml"), format!(
                    "[skill]\nname = 'sop_guard'\ndescription = 'SOP fixture'\nversion = '1.0.0'\n\
                     [[tools]]\nname = 'invoke'\ndescription = 'SOP fixture'\nkind = 'builtin'\ncommand = ''\ntarget = '{target}'\n"
                )).unwrap();
                let mut engine = crate::sop::SopEngine::new(config.sop.clone());
                engine.set_sops_for_test(vec![gated_sop("target-sop", "target-agent")]);
                let engine = Arc::new(std::sync::Mutex::new(engine));
                let run_id = if target == "sop_execute" {
                    None
                } else {
                    Some(park_sop_run(&engine, "target-sop"))
                };
                let before = serde_json::to_value(engine.lock().unwrap().active_runs()).unwrap();
                let sessions = Arc::new(crate::rpc::session::SessionStore::new(
                    16,
                    Arc::new(SessionActorQueue::new(4, 10, 60)),
                ));
                let audit = Arc::new(crate::sop::SopAuditLogger::new(Arc::new(
                    zeroclaw_memory::NoneMemory::new("sop-fixture"),
                )));
                let ctx = RpcContext::minimal_with_sop_engine_and_audit(
                    config,
                    sessions,
                    Arc::clone(&engine),
                    audit,
                    Some(crate::sop::SopDriverHandles::default()),
                );
                let (mut caller, mut rx) = roster_peer(&ctx, 4242).await;
                let created = rpc(
                    &mut caller,
                    &mut rx,
                    1,
                    "session/new",
                    json!({
                        "agent_alias": "test-agent", "session_id": "sop-entry", "chat_mode": "chat",
                    }),
                )
                .await;
                assert!(created.get("error").is_none(), "{created}");
                let agent = ctx.sessions.get_agent("sop-entry").await.unwrap();
                assert!(
                    agent.lock().await.tool_names().contains(&name),
                    "the unrestricted factory must actually construct {name}"
                );

                // Exercise live narrowing of an already-assembled registry and
                // its captured skill wrappers through the real prompt handler.
                {
                    let mut config = ctx.config_authority.snapshot_config();
                    let profile = config
                        .permission_profiles
                        .get_mut("principal-test")
                        .unwrap();
                    profile.admin = posture == "admin";
                    match posture {
                        "tool-scoped" => {
                            profile.allowed_tools = vec![name.into(), "calculator".into()]
                        }
                        "agent-scoped" => profile.allowed_agents = vec!["test-agent".into()],
                        "admin" => {
                            profile.admin = true;
                            profile.allowed_tools.clear();
                            profile.allowed_agents.clear();
                        }
                        "wildcard" => {}
                        _ => unreachable!(),
                    }
                    ctx.auth.refresh_from_config(&config).unwrap();
                    ctx.config_authority.publish_for_test(config);
                }
                let observed = Arc::new(std::sync::Mutex::new(None));
                let arguments = match target {
                    "sop_execute" => json!({"name": "target-sop"}),
                    "sop_approve" => json!({"run_id": run_id}),
                    "sop_advance" => {
                        json!({"run_id": run_id, "status": "completed", "output": "fixture"})
                    }
                    _ => unreachable!(),
                };
                agent
                    .lock()
                    .await
                    .set_model_provider(Box::new(SopEntryProbe {
                        name: name.into(),
                        arguments,
                        result: Arc::clone(&observed),
                    }));
                let response = tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    rpc(
                        &mut caller,
                        &mut rx,
                        2,
                        "session/prompt",
                        json!({
                            "session_id": "sop-entry", "prompt": "Run the SOP tool probe",
                        }),
                    ),
                )
                .await
                .expect("the scripted prompt must settle");
                assert!(
                    response.get("error").is_none(),
                    "{posture} {name}: {response}"
                );
                let output = observed
                    .lock()
                    .unwrap()
                    .clone()
                    .expect("the model receives the tool result");
                let constrained = posture != "admin";
                assert_eq!(
                    agent.lock().await.tool_names().contains(&name),
                    !constrained
                );
                if constrained {
                    assert!(
                        output.contains(&format!("Unknown tool: {name}")),
                        "{posture} {name}: {output}"
                    );
                    assert_eq!(
                        serde_json::to_value(engine.lock().unwrap().active_runs()).unwrap(),
                        before,
                        "refusal must leave the foreign SOP state unchanged"
                    );
                    let calculator = agent
                        .lock()
                        .await
                        .dispatch_tool_for_test(
                            "calculator",
                            json!({"function": "add", "values": [2, 3]}),
                        )
                        .await;
                    assert!(
                        calculator.success,
                        "the ordinary parent tool remains usable"
                    );
                } else {
                    assert!(
                        output.contains("waiting for approval"),
                        "{posture} {name}: {output}"
                    );
                    assert_eq!(engine.lock().unwrap().active_runs().len(), 1);
                }
            }
        }
    }
}

fn sop_workshop_principal_fixture(
    tmp: &tempfile::TempDir,
    tools: &[&str],
    agents: &[&str],
    admin: bool,
) -> (RpcDispatcher, Arc<std::sync::Mutex<crate::sop::SopEngine>>) {
    let mut config = principal_test_config(tmp, tools, agents);
    // The shared fixture selects a minimal built-in set; select the SOP tools
    // so the admin control arm constructs them and the workshop alias target.
    config
        .tools
        .optional
        .extend(["sop_workshop", "sop_list"].map(str::to_owned));
    std::fs::create_dir_all(config.agent_workspace_dir("test-agent")).unwrap();
    config
        .permission_profiles
        .get_mut("principal-test")
        .unwrap()
        .admin = admin;
    config.sop.sops_dir = Some(tmp.path().join("shared/sops").display().to_string());
    config.sop.procedural_memory_enabled = true;
    let risk = config.risk_profiles.get_mut("test-profile").unwrap();
    risk.level = zeroclaw_config::autonomy::AutonomyLevel::Full;
    risk.allowed_tools
        .extend(["sop_workshop", "workshop__apply", "sop_list"].map(str::to_owned));
    let bundle = tmp.path().join("skills");
    std::fs::create_dir_all(bundle.join("workshop")).unwrap();
    std::fs::write(bundle.join("workshop/SKILL.toml"), "[skill]\nname='workshop'\ndescription='SOP permission fixture'\n[[tools]]\nname='apply'\ndescription='Apply a proposal'\nkind='builtin'\ncommand=''\ntarget='sop_workshop'\n").unwrap();
    config.skill_bundles.insert(
        "workshop-fixture".into(),
        zeroclaw_config::schema::SkillBundleConfig {
            directory: Some(bundle.display().to_string()),
            ..Default::default()
        },
    );
    config.agents.get_mut("test-agent").unwrap().skill_bundles = vec!["workshop-fixture".into()];
    let engine = Arc::new(std::sync::Mutex::new(crate::sop::SopEngine::new(
        config.sop.clone(),
    )));
    let data_dir = config.data_dir.clone();
    let (mut fixture, _, _, _) = make_persistence_test_dispatcher(config, &data_dir);
    let ctx = Arc::get_mut(&mut fixture.ctx).expect("fixture context is not yet shared");
    ctx.sop_engine = Some(Arc::clone(&engine));
    ctx.sop_audit = Some(Arc::new(crate::sop::SopAuditLogger::new(Arc::new(
        zeroclaw_memory::NoneMemory::new("none"),
    ))));
    (fixture, engine)
}

async fn propose_workshop_fixture(agent: &crate::agent::agent::Agent, name: &str) -> String {
    let result = agent
        .dispatch_tool_for_test(
            "sop_workshop",
            json!({
                "action":"propose", "sop_name":name, "description":"Permission fixture",
                "procedure_markdown":"## Steps\n\n1. **Check** - Record a harmless result.\n",
            }),
        )
        .await;
    assert!(result.success, "operator can propose: {}", result.output);
    serde_json::from_str::<Value>(&result.output).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn principal_sop_workshop_refuses_direct_and_alias_effects() {
    // Exercise both selector gates and the non-admin wildcard posture.
    for (tools, agents) in [
        (&["*"][..], &["*"][..]),
        (
            &["sop_workshop", "workshop__apply", "sop_list", "calculator"][..],
            &["*"][..],
        ),
        (&["*"][..], &["test-agent"][..]),
    ] {
        for mode in ["chat", "acp"] {
            let tmp = tempfile::TempDir::new().unwrap();
            let (operator, engine) = sop_workshop_principal_fixture(&tmp, tools, agents, false);
            let sessions = &operator.ctx.sessions;
            operator.handle_session_new_for_test(&json!({"agent_alias":"test-agent", "session_id":"operator", "keep_siblings":true})).await.unwrap();
            let operator_agent = sessions.get_agent("operator").await.unwrap();
            let proposal = {
                let agent = operator_agent.lock().await;
                assert!(
                    agent.tool_names().contains(&"workshop__apply"),
                    "factory must install the real alias"
                );
                propose_workshop_fixture(&agent, "guarded-change").await
            };
            let before =
                serde_json::to_value(engine.lock().unwrap().load_proposal(&proposal).unwrap())
                    .unwrap();
            let principal = scoped_dispatcher(&operator.ctx, 4242).await;
            principal.handle_session_new_for_test(&json!({"agent_alias":"test-agent", "session_id":"member", "chat_mode":mode, "keep_siblings":true})).await.unwrap();
            let member = sessions.get_agent("member").await.unwrap();
            let member = member.lock().await;
            for name in ["sop_workshop", "workshop__apply"] {
                let result = member
                    .dispatch_tool_for_test(name, json!({"action":"apply", "id":proposal}))
                    .await;
                assert!(
                    !result.success,
                    "non-admin workshop effect escaped through {name}: {}",
                    result.output
                );
                assert_eq!(result.output, format!("Unknown tool: {name}"));
                assert!(!member.tool_names().contains(&name));
            }
            let ordinary = member
                .dispatch_tool_for_test("calculator", json!({"function":"add", "values":[2,2]}))
                .await;
            assert!(ordinary.success);
            assert_eq!(ordinary.output, "4");
            assert!(
                member
                    .dispatch_tool_for_test("sop_list", json!({}))
                    .await
                    .success
            );
            assert!(!tmp.path().join("shared/sops/guarded-change").exists());
            assert_eq!(
                serde_json::to_value(engine.lock().unwrap().load_proposal(&proposal).unwrap())
                    .unwrap(),
                before
            );
            assert!(engine.lock().unwrap().get_sop("guarded-change").is_none());
            // Same proposal and engine: the unrestricted operator can apply
            // through either entry point after both refusals.
            let name = if mode == "chat" {
                "sop_workshop"
            } else {
                "workshop__apply"
            };
            let result = operator_agent
                .lock()
                .await
                .dispatch_tool_for_test(name, json!({"action":"apply", "id":proposal}))
                .await;
            assert!(result.success, "operator apply: {}", result.output);
            assert!(
                tmp.path()
                    .join("shared/sops/guarded-change/SOP.toml")
                    .is_file()
            );
            assert!(
                tmp.path()
                    .join("shared/sops/guarded-change/SOP.md")
                    .is_file()
            );
            assert!(engine.lock().unwrap().get_sop("guarded-change").is_some());
        }
    }
}

struct SopWorkshopApplyProbe {
    calls: std::sync::atomic::AtomicUsize,
    proposals: [String; 2],
    results: Arc<std::sync::Mutex<Vec<Value>>>,
}

impl zeroclaw_api::attribution::Attributable for SopWorkshopApplyProbe {
    fn role(&self) -> zeroclaw_api::attribution::Role {
        zeroclaw_api::attribution::Role::Provider(zeroclaw_api::attribution::ProviderKind::Model(
            zeroclaw_api::attribution::ModelProviderKind::Custom,
        ))
    }
    fn alias(&self) -> &str {
        "workshop-permission-probe"
    }
}

#[async_trait]
impl zeroclaw_api::model_provider::ModelProvider for SopWorkshopApplyProbe {
    fn supports_native_tools(&self) -> bool {
        true
    }
    async fn chat_with_system(
        &self,
        _: Option<&str>,
        _: &str,
        _: &str,
        _: Option<f64>,
    ) -> anyhow::Result<String> {
        Ok("unused".into())
    }
    async fn chat(
        &self,
        request: zeroclaw_providers::ChatRequest<'_>,
        _: &str,
        _: Option<f64>,
    ) -> anyhow::Result<zeroclaw_providers::ChatResponse> {
        let first = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
        if !first {
            *self.results.lock().unwrap() = request
                .messages
                .iter()
                .filter(|message| message.role == "tool")
                .map(|message| serde_json::from_str(&message.content).unwrap())
                .collect();
        }
        Ok(zeroclaw_providers::ChatResponse {
            text: (!first).then(|| "workshop probe complete".into()),
            tool_calls: if first {
                [
                    (
                        "sop_workshop",
                        json!({"action":"apply", "id":self.proposals[0]}),
                    ),
                    (
                        "workshop__apply",
                        json!({"action":"apply", "id":self.proposals[1]}),
                    ),
                    ("calculator", json!({"function":"add", "values":[2,2]})),
                ]
                .into_iter()
                .enumerate()
                .map(|(index, (name, args))| zeroclaw_providers::ToolCall {
                    id: format!("workshop-{index}"),
                    name: name.into(),
                    arguments: args.to_string(),
                    extra_content: None,
                })
                .collect()
            } else {
                Vec::new()
            },
            usage: None,
            reasoning_content: None,
        })
    }
}

#[tokio::test]
async fn principal_sop_workshop_queued_demotion_prevents_persistence() {
    for demote in [false, true] {
        let tmp = tempfile::TempDir::new().unwrap();
        let (fixture, engine) = sop_workshop_principal_fixture(&tmp, &["*"], &["*"], true);
        let actor = Arc::new(scoped_dispatcher(&fixture.ctx, 4242).await);
        actor
            .handle_session_new_for_test(
                &json!({"agent_alias":"test-agent", "session_id":"workshop-actor"}),
            )
            .await
            .unwrap();
        let sessions = &actor.ctx.sessions;
        let handle = sessions.get_agent("workshop-actor").await.unwrap();
        let results = Arc::new(std::sync::Mutex::new(Vec::new()));
        let proposals = {
            let mut agent = handle.lock().await;
            assert!(agent.tool_names().contains(&"workshop__apply"));
            let proposals = [
                propose_workshop_fixture(&agent, "direct-change").await,
                propose_workshop_fixture(&agent, "alias-change").await,
            ];
            agent.set_tool_dispatcher(Box::new(crate::agent::dispatcher::NativeToolDispatcher));
            agent.set_model_provider(Box::new(SopWorkshopApplyProbe {
                calls: std::sync::atomic::AtomicUsize::new(0),
                proposals: proposals.clone(),
                results: Arc::clone(&results),
            }));
            proposals
        };
        let before: Vec<_> = proposals
            .iter()
            .map(|id| {
                serde_json::to_value(engine.lock().unwrap().load_proposal(id).unwrap()).unwrap()
            })
            .collect();
        let provider_guard = sessions
            .lock_model_provider_update("workshop-actor")
            .await
            .unwrap();
        let provider_wait = sessions.model_provider_update_waiting();
        let arrived = provider_wait.notified();
        let call = Arc::clone(&actor);
        let prompt = zeroclaw_spawn::spawn!(async move {
            call.handle_session_prompt(
                &json!({"session_id":"workshop-actor", "prompt":"Apply the prepared proposals"}),
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(10), arrived)
            .await
            .unwrap();
        if demote {
            let mut config = actor.ctx.config_authority.snapshot_config();
            config
                .permission_profiles
                .get_mut("principal-test")
                .unwrap()
                .admin = false;
            actor.ctx.auth.refresh_from_config(&config).unwrap();
            actor.ctx.config_authority.publish_for_test(config);
        }
        drop(provider_guard);
        tokio::time::timeout(std::time::Duration::from_secs(10), prompt)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let output = results.lock().unwrap().clone();
        assert_eq!(output.len(), 3, "{output:?}");
        for (index, name) in ["sop_workshop", "workshop__apply"].into_iter().enumerate() {
            let result = output
                .iter()
                .find(|row| row["tool_call_id"] == format!("workshop-{index}"))
                .unwrap();
            if demote {
                assert_eq!(
                    result["content"],
                    format!("Unknown tool: {name}"),
                    "queued demotion must withhold {name}"
                );
                assert_eq!(
                    serde_json::to_value(
                        engine
                            .lock()
                            .unwrap()
                            .load_proposal(&proposals[index])
                            .unwrap()
                    )
                    .unwrap(),
                    before[index]
                );
            }
            let sop = if index == 0 {
                "direct-change"
            } else {
                "alias-change"
            };
            assert_eq!(
                tmp.path().join("shared/sops").join(sop).exists(),
                !demote,
                "queued workshop persistence: {sop}"
            );
            assert_eq!(engine.lock().unwrap().get_sop(sop).is_some(), !demote);
        }
        assert!(
            output
                .iter()
                .any(|row| row["tool_call_id"] == "workshop-2" && row["content"] == "4"),
            "ordinary tool remains executable: {output:?}"
        );
    }
}
