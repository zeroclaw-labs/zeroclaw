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
                if target != "sop_execute" && matches!(posture, "wildcard" | "admin") {
                    continue;
                }
                let tmp = tempfile::TempDir::new().unwrap();
                let mut config = principal_test_config(&tmp, &["*"], &["*"]);
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
                    let mut config = ctx.config.write();
                    let profile = config
                        .permission_profiles
                        .get_mut("principal-test")
                        .unwrap();
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
                let constrained = matches!(posture, "tool-scoped" | "agent-scoped");
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
