use super::*;

#[tokio::test]
async fn principal_cron_rpc_refuses_agent_jobs_but_keeps_shell_jobs() {
    for posture in ["scoped", "wildcard", "demoted"] {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = cron_roster_config_in(&tmp, 4242);
        let profile = config.permission_profiles.get_mut("cron-alpha").unwrap();
        profile.admin = posture == "demoted";
        profile.allowed_tools = vec!["*".into()];
        if posture != "scoped" {
            profile.allowed_agents = vec!["*".into()];
        }
        let job = seed_cron_job(&config, "alpha", "principal-unaware");
        crate::cron::pause_job(&config, &job.id).unwrap();
        let before = serde_json::to_value(crate::cron::get_job(&config, &job.id).unwrap()).unwrap();
        let ctx = enforcement_ctx(config.clone());
        let (mut caller, mut rx) = roster_peer(&ctx, 4242).await;
        let patch = json!({"id": job.id, "agent": "alpha", "prompt": "read foreign history"});
        if posture == "demoted" {
            {
                let mut cfg = ctx.config_authority.snapshot_config();
                cfg.permission_profiles.get_mut("cron-alpha").unwrap().admin = false;
                ctx.auth.refresh_from_config(&cfg).unwrap();
                ctx.config_authority.publish_for_test(cfg);
            }
            assert!(
                caller.has_admin_grants(),
                "the connection stamp is deliberately stale"
            );
            // Direct handlers bypass request-time refresh, so this proves the
            // new boundary consults current authority rather than the stamp.
            for result in [
                caller.handle_cron_patch(&patch).await,
                caller.handle_cron_trigger(&json!({"id": job.id})).await,
            ] {
                assert_eq!(result.unwrap_err().code, FORBIDDEN);
            }
        }
        for (id, method, params) in [
            (1, "cron/patch", patch.clone()),
            (
                2,
                "cron/patch",
                json!({"id":job.id, "agent":"alpha", "schedule":"* * * * *"}),
            ),
            (3, "cron/trigger", json!({"id":job.id})),
        ] {
            let response = rpc(&mut caller, &mut rx, id, method, params).await;
            assert_eq!(
                response["error"]["code"],
                json!(FORBIDDEN),
                "{posture}: {response}"
            );
            assert_eq!(
                response["error"]["message"],
                json!(crate::i18n::get_required_cli_string(
                    "rpc-cron-agent-principal-required"
                )),
                "{posture}: {response}"
            );
        }
        assert_eq!(
            serde_json::to_value(crate::cron::get_job(&config, &job.id).unwrap()).unwrap(),
            before,
            "denial must not modify, claim, or execute the job"
        );
        assert!(
            crate::cron::list_runs(&config, &job.id, 10)
                .unwrap()
                .is_empty()
        );

        // The same caller's existing shell-job grants remain usable through
        // the real dispatcher, including persistence and output.
        let added = rpc(
            &mut caller,
            &mut rx,
            4,
            "cron/add",
            json!({
                "agent":"alpha", "schedule":"0 0 * * *", "command":"echo original"
            }),
        )
        .await;
        let shell = added["result"]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("{added}"));
        let patched = rpc(
            &mut caller,
            &mut rx,
            5,
            "cron/patch",
            json!({
                "id":shell, "agent":"alpha", "command":"echo permitted-shell"
            }),
        )
        .await;
        assert!(patched.get("error").is_none(), "{patched}");
        let triggered = rpc(&mut caller, &mut rx, 6, "cron/trigger", json!({"id":shell})).await;
        assert_eq!(triggered["result"]["success"], json!(true), "{triggered}");
        assert!(
            triggered["result"]["output"]
                .as_str()
                .unwrap()
                .contains("permitted-shell")
        );

        // A current administrator can still edit the agent job. Trigger
        // execution itself is covered by the existing scheduler tests.
        let (mut operator, mut op_rx) = local_operator(&ctx).await;
        let response = rpc(&mut operator, &mut op_rx, 7, "cron/patch", patch).await;
        assert_eq!(
            response["result"]["prompt"],
            json!("read foreign history"),
            "{response}"
        );
    }
}

const TARGETS: [&str; 8] = [
    "cron_add",
    "cron_update",
    "cron_run",
    "cron_list",
    "cron_runs",
    "cron_remove",
    "schedule",
    "send_message_to_peer",
];

struct HeadlessToolProbe {
    job: String,
    observed: Arc<std::sync::Mutex<Vec<String>>>,
}

fn probe_args(target: &str, job: &str) -> Value {
    match target {
        "cron_add" => {
            json!({"job_type":"agent", "prompt":"read session history", "schedule":{"kind":"every", "every_ms":3600000}})
        }
        "cron_update" => json!({"job_id":job, "patch":{"prompt":"read session history"}}),
        "cron_run" | "cron_runs" | "cron_remove" => json!({"job_id":job}),
        "schedule" => json!({"action":"resume", "id":job}),
        "send_message_to_peer" => {
            json!({"channel":"telegram.fixture", "target":"target-agent", "message":"read session history"})
        }
        _ => json!({}),
    }
}

#[async_trait]
impl zeroclaw_api::model_provider::ModelProvider for HeadlessToolProbe {
    async fn chat_with_system(
        &self,
        _: Option<&str>,
        _: &str,
        _: &str,
        _: Option<f64>,
    ) -> anyhow::Result<String> {
        anyhow::bail!("the probe must use the tool-capable chat path")
    }

    async fn chat(
        &self,
        request: zeroclaw_providers::ChatRequest<'_>,
        _: &str,
        _: Option<f64>,
    ) -> anyhow::Result<zeroclaw_providers::ChatResponse> {
        let results: Vec<String> = request
            .messages
            .iter()
            .filter(|m| m.role == "tool")
            .map(|m| m.content.clone())
            .collect();
        let calls: Vec<zeroclaw_providers::ToolCall> = if results.is_empty() {
            TARGETS
                .iter()
                .flat_map(|target| {
                    [target.to_string(), format!("headless__{target}")].map(|name| {
                        zeroclaw_providers::ToolCall {
                            id: name.clone(),
                            name,
                            arguments: probe_args(target, &self.job).to_string(),
                            extra_content: None,
                        }
                    })
                })
                .collect()
        } else {
            *self.observed.lock().unwrap() = results;
            vec![]
        };
        Ok(zeroclaw_providers::ChatResponse {
            text: calls.is_empty().then(|| "probe finished".into()),
            tool_calls: calls,
            usage: None,
            reasoning_content: None,
        })
    }
}

impl zeroclaw_api::attribution::Attributable for HeadlessToolProbe {
    fn role(&self) -> zeroclaw_api::attribution::Role {
        zeroclaw_api::attribution::Role::Provider(zeroclaw_api::attribution::ProviderKind::Model(
            zeroclaw_api::attribution::ModelProviderKind::Custom,
        ))
    }
    fn alias(&self) -> &str {
        "headless-tool-probe"
    }
}

#[tokio::test]
async fn principal_sessions_refuse_headless_tools_and_captured_aliases() {
    for initially_admin in [false, true] {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = principal_test_config(&tmp, &["*"], &["*"]);
        let profile = config.risk_profiles.get_mut("test-profile").unwrap();
        profile.level = zeroclaw_config::autonomy::AutonomyLevel::Full;
        profile.allowed_tools = TARGETS
            .iter()
            .flat_map(|n| [n.to_string(), format!("headless__{n}")])
            .chain(["calculator".into()])
            .collect();
        config
            .permission_profiles
            .get_mut("principal-test")
            .unwrap()
            .admin = initially_admin;
        let target = config.agents["test-agent"].clone();
        config.agents.insert("target-agent".into(), target);
        let skill = config
            .agent_workspace_dir("test-agent")
            .join("skills/headless");
        std::fs::create_dir_all(&skill).unwrap();
        let mut manifest =
            "[skill]\nname='headless'\ndescription='fixture'\nversion='1.0.0'\n".to_string();
        for name in TARGETS {
            manifest.push_str(&format!("\n[[tools]]\nname='{name}'\ndescription='fixture'\nkind='builtin'\ncommand=''\ntarget='{name}'\n"));
        }
        std::fs::write(skill.join("SKILL.toml"), manifest).unwrap();
        let job = crate::cron::add_agent_job(
            &config,
            "test-agent",
            None,
            crate::cron::Schedule::Every { every_ms: 3600000 },
            "private job prompt",
            crate::cron::SessionTarget::Isolated,
            None,
            None,
            false,
            None,
            false,
        )
        .unwrap();
        crate::cron::pause_job(&config, &job.id).unwrap();
        let now = chrono::Utc::now();
        crate::cron::record_run(
            &config,
            &job.id,
            now,
            now,
            "ok",
            crate::cron::RunOutcomes {
                execution: "ok",
                delivery: "not_required",
                persistence: "not_bound",
            },
            crate::cron::RunProvenance {
                principal: None,
                executing_agent: Some("test-agent"),
                job_source: Some("imperative"),
            },
            Some("foreign session secret"),
            1,
        )
        .unwrap();
        let before = serde_json::to_value(crate::cron::list_jobs(&config).unwrap()).unwrap();
        let history =
            serde_json::to_value(crate::cron::list_runs(&config, &job.id, 10).unwrap()).unwrap();
        let data_dir = config.data_dir.clone();
        let (operator, sessions, _, _) = make_persistence_test_dispatcher(config, &data_dir);
        let (mut caller, mut rx) = roster_peer(&operator.ctx, 4242).await;
        let response = rpc(
            &mut caller,
            &mut rx,
            1,
            "session/new",
            json!({"agent_alias":"test-agent", "session_id":"headless-probe", "chat_mode":"acp"}),
        )
        .await;
        assert!(response.get("error").is_none(), "{response}");
        let agent = sessions.get_agent("headless-probe").await.unwrap();
        if initially_admin {
            let guard = agent.lock().await;
            for name in TARGETS
                .iter()
                .flat_map(|n| [n.to_string(), format!("headless__{n}")])
            {
                assert!(
                    guard.tool_names().contains(&name.as_str()),
                    "admin factory must construct {name}"
                );
            }
            let control = guard
                .dispatch_tool_for_test("headless__cron_runs", json!({"job_id":job.id}))
                .await;
            assert!(
                control.success && control.output.contains("foreign session secret"),
                "{}",
                control.output
            );
            drop(guard);
            let mut cfg = operator.ctx.config_authority.snapshot_config();
            cfg.permission_profiles
                .get_mut("principal-test")
                .unwrap()
                .admin = false;
            operator.ctx.auth.refresh_from_config(&cfg).unwrap();
            operator.ctx.config_authority.publish_for_test(cfg);
        }
        let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
        agent
            .lock()
            .await
            .set_model_provider(Box::new(HeadlessToolProbe {
                job: job.id.clone(),
                observed: Arc::clone(&observed),
            }));
        let wire = json!({"jsonrpc":"2.0", "id":2, "method":"session/prompt", "params":{"session_id":"headless-probe", "prompt":"probe headless tools"}}).to_string();
        // Drain notifications while the forged tool calls execute, so the
        // bounded writer cannot turn this regression into a backpressure test.
        let response = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let (_, response) = tokio::join!(caller.process_line(&wire), async {
                loop {
                    let frame = rx.recv().await.expect("writer stays open");
                    let value: Value = serde_json::from_str(&frame).unwrap();
                    if value.get("id") == Some(&json!(2)) {
                        break value;
                    }
                }
            });
            response
        })
        .await
        .expect("prompt must finish");
        assert!(response.get("error").is_none(), "{response}");
        let results = observed.lock().unwrap().clone();
        assert_eq!(
            results.len(),
            TARGETS.len() * 2,
            "every requested tool must return a result"
        );
        for name in TARGETS
            .iter()
            .flat_map(|n| [n.to_string(), format!("headless__{n}")])
        {
            assert!(
                results.iter().any(
                    |r| r
                        .split_once(&format!("Unknown tool: {name}"))
                        .is_some_and(|(_, tail)| {
                            // `cron_runs` must not satisfy the `cron_run` refusal.
                            tail.chars()
                                .next()
                                .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_' && c != '-')
                        })
                ),
                "{name}: {results:?}"
            );
        }
        let ordinary = agent
            .lock()
            .await
            .dispatch_tool_for_test("calculator", json!({"function":"add", "values":[2,3]}))
            .await;
        assert!(ordinary.success);
        let cfg = operator.ctx.config.read().clone();
        assert_eq!(
            serde_json::to_value(crate::cron::list_jobs(&cfg).unwrap()).unwrap(),
            before
        );
        assert_eq!(
            serde_json::to_value(crate::cron::list_runs(&cfg, &job.id, 10).unwrap()).unwrap(),
            history
        );
        assert!(sessions.remove("headless-probe").await);
        let current = caller.current_prompt_authority().unwrap();
        let restored = current
            .rehydrate_reaped_session("headless-probe", current.stamped_grants())
            .await
            .unwrap()
            .unwrap();
        for name in TARGETS
            .iter()
            .flat_map(|n| [n.to_string(), format!("headless__{n}")])
        {
            assert!(
                !restored.lock().await.tool_names().contains(&name.as_str()),
                "rehydration restores {name}"
            );
        }
    }
}
