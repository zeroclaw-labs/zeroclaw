//! Independent executable witness for the public runtime composition contract.
//! No concrete provider, memory, channel, tool or application crate is a dependency.

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use async_trait::async_trait;
    use serde_json::{Value, json};
    use zeroclaw_api::attribution::{
        Attributable, ChannelKind, MemoryKind, ModelProviderKind, ProviderKind, Role, ToolKind,
    };
    use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};
    use zeroclaw_api::ingress::TurnOrigin;
    use zeroclaw_api::memory_traits::{Memory, MemoryCategory, MemoryEntry};
    use zeroclaw_api::model_provider::{ChatRequest, ChatResponse, ModelProvider, ToolCall};
    use zeroclaw_api::observability_traits::{Observer, ObserverEvent, ObserverMetric};
    use zeroclaw_api::tool::{Tool, ToolResult};
    use zeroclaw_config::multi_agent::{
        AgentMemoryConfig, AgentWorkspaceConfig, MemoryBackendKind,
    };
    use zeroclaw_config::schema::{
        AliasedAgentConfig, Config, CustomModelProviderConfig, ModelProviderConfig,
        PeripheralBoardConfig, RiskProfileConfig, RuntimeProfileConfig,
    };
    use zeroclaw_runtime::composition::{
        ChannelSource, MemoryRequest, MemorySource, ProviderRequest, ProviderSource,
        RuntimeCapabilities, ToolRequest, ToolSource,
    };

    const AGENT: &str = "consumer";
    const TOOL: &str = "consumer_echo";
    const CONTENT: &str = "independent capability round trip";

    #[derive(Default)]
    struct Evidence {
        provider_requests: AtomicUsize,
        memory_requests: AtomicUsize,
        memory_recalls: AtomicUsize,
        tool_requests: AtomicUsize,
        tool_calls: AtomicUsize,
        channel_lookups: AtomicUsize,
        native_peripheral_constructions: AtomicUsize,
        observed_events: AtomicUsize,
        flushes: AtomicUsize,
        catalogs: Mutex<Vec<Vec<String>>>,
        stored: Mutex<Vec<(String, String)>>,
        delivered: Mutex<Vec<String>>,
    }

    struct SuppliedProvider(Arc<Evidence>);
    impl Attributable for SuppliedProvider {
        fn role(&self) -> Role {
            Role::Provider(ProviderKind::Model(ModelProviderKind::Custom))
        }
        fn alias(&self) -> &str {
            "consumer"
        }
    }
    #[async_trait]
    impl ModelProvider for SuppliedProvider {
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
            anyhow::bail!("the consumer requires the real structured tool loop")
        }
        async fn chat(
            &self,
            request: ChatRequest<'_>,
            model: &str,
            _: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            anyhow::ensure!(model == "consumer-model", "resolved model changed");
            self.0.catalogs.lock().unwrap().push(
                request
                    .tools
                    .unwrap_or_default()
                    .iter()
                    .map(|t| t.name.clone())
                    .collect(),
            );
            let first = self.0.catalogs.lock().unwrap().len() == 1;
            if first {
                Ok(ChatResponse {
                    text: None,
                    tool_calls: vec![ToolCall {
                        id: "consumer-call".into(),
                        name: TOOL.into(),
                        arguments: json!({"text": CONTENT}).to_string(),
                        extra_content: None,
                    }],
                    usage: None,
                    reasoning_content: None,
                })
            } else {
                anyhow::ensure!(
                    self.0.tool_calls.load(Ordering::SeqCst) == 1,
                    "supplied tool did not run exactly once"
                );
                anyhow::ensure!(
                    request.messages.iter().any(|m| m.content.contains(CONTENT)),
                    "tool result did not reach provider"
                );
                anyhow::ensure!(
                    self.0.delivered.lock().unwrap().as_slice() == [CONTENT],
                    "supplied channel did not deliver"
                );
                Ok(ChatResponse {
                    text: Some("consumer turn complete".into()),
                    tool_calls: Vec::new(),
                    usage: None,
                    reasoning_content: None,
                })
            }
        }
    }
    struct Providers(Arc<Evidence>);
    impl ProviderSource for Providers {
        fn model_provider(
            &self,
            r: &ProviderRequest<'_>,
        ) -> anyhow::Result<Arc<dyn ModelProvider>> {
            anyhow::ensure!(r.agent_alias == AGENT, "wrong agent requested");
            self.0.provider_requests.fetch_add(1, Ordering::SeqCst);
            Ok(Arc::new(SuppliedProvider(self.0.clone())))
        }
    }

    struct SuppliedMemory(Arc<Evidence>);
    impl Attributable for SuppliedMemory {
        fn role(&self) -> Role {
            Role::Memory(MemoryKind::InMemory)
        }
        fn alias(&self) -> &str {
            AGENT
        }
    }
    #[async_trait]
    impl Memory for SuppliedMemory {
        fn name(&self) -> &str {
            "consumer-memory"
        }
        async fn store(
            &self,
            key: &str,
            content: &str,
            _: MemoryCategory,
            _: Option<&str>,
        ) -> anyhow::Result<()> {
            self.0
                .stored
                .lock()
                .unwrap()
                .push((key.into(), content.into()));
            Ok(())
        }
        async fn recall(
            &self,
            _: &str,
            _: usize,
            _: Option<&str>,
            _: Option<&str>,
            _: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            self.0.memory_recalls.fetch_add(1, Ordering::SeqCst);
            Ok(Vec::new())
        }
        async fn store_with_agent(
            &self,
            key: &str,
            content: &str,
            category: MemoryCategory,
            session_id: Option<&str>,
            _: Option<&str>,
            _: Option<f64>,
            agent_id: Option<&str>,
        ) -> anyhow::Result<()> {
            anyhow::ensure!(
                agent_id.is_none_or(|agent| agent == AGENT),
                "wrong scoped memory agent"
            );
            self.store(key, content, category, session_id).await
        }
        async fn recall_for_agents(
            &self,
            agents: &[&str],
            query: &str,
            limit: usize,
            session_id: Option<&str>,
            since: Option<&str>,
            until: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            anyhow::ensure!(
                agents.iter().all(|agent| *agent == AGENT),
                "wrong recall agent scope"
            );
            self.recall(query, limit, session_id, since, until).await
        }
        async fn get(&self, _: &str) -> anyhow::Result<Option<MemoryEntry>> {
            Ok(None)
        }
        async fn list(
            &self,
            _: Option<&MemoryCategory>,
            _: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            Ok(Vec::new())
        }
        async fn forget(&self, _: &str) -> anyhow::Result<bool> {
            Ok(false)
        }
        async fn forget_for_agent(&self, _: &str, _: &str) -> anyhow::Result<bool> {
            Ok(false)
        }
        async fn count(&self) -> anyhow::Result<usize> {
            Ok(self.0.stored.lock().unwrap().len())
        }
        async fn health_check(&self) -> bool {
            true
        }
    }
    struct Memories(Arc<Evidence>);
    #[async_trait]
    impl MemorySource for Memories {
        async fn memory(&self, r: &MemoryRequest<'_>) -> anyhow::Result<Arc<dyn Memory>> {
            anyhow::ensure!(r.agent_alias == AGENT, "wrong memory requested");
            self.0.memory_requests.fetch_add(1, Ordering::SeqCst);
            Ok(Arc::new(SuppliedMemory(self.0.clone())))
        }
    }

    struct SuppliedChannel(Arc<Evidence>);
    impl Attributable for SuppliedChannel {
        fn role(&self) -> Role {
            Role::Channel(ChannelKind::Plugin)
        }
        fn alias(&self) -> &str {
            "consumer-output"
        }
    }
    #[async_trait]
    impl Channel for SuppliedChannel {
        fn name(&self) -> &str {
            "consumer-output"
        }
        async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
            self.0
                .delivered
                .lock()
                .unwrap()
                .push(message.content.clone());
            Ok(())
        }
        async fn listen(&self, _: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
            anyhow::bail!("the consumer channel is outbound only")
        }
    }
    struct Channels(Arc<Evidence>);
    impl ChannelSource for Channels {
        fn channel(&self, alias: &str) -> Option<Arc<dyn Channel>> {
            self.0.channel_lookups.fetch_add(1, Ordering::SeqCst);
            (alias == "consumer-output")
                .then(|| Arc::new(SuppliedChannel(self.0.clone())) as Arc<dyn Channel>)
        }
    }

    struct SuppliedTool {
        evidence: Arc<Evidence>,
        memory: Arc<dyn Memory>,
        channel: Arc<dyn Channel>,
    }
    impl Attributable for SuppliedTool {
        fn role(&self) -> Role {
            Role::Tool(ToolKind::Plugin)
        }
        fn alias(&self) -> &str {
            TOOL
        }
    }
    #[async_trait]
    impl Tool for SuppliedTool {
        fn name(&self) -> &str {
            TOOL
        }
        fn description(&self) -> &str {
            "Record the input using the supplied memory and channel."
        }
        fn parameters_schema(&self) -> Value {
            json!({"type":"object", "properties":{"text":{"type":"string"}}, "required":["text"]})
        }
        async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
            anyhow::ensure!(args["text"] == CONTENT, "tool arguments changed");
            self.memory
                .store("consumer-proof", CONTENT, MemoryCategory::Core, None)
                .await?;
            self.channel
                .send(&SendMessage::new(CONTENT, "fixture-recipient"))
                .await?;
            self.evidence.tool_calls.fetch_add(1, Ordering::SeqCst);
            Ok(ToolResult {
                success: true,
                output: CONTENT.into(),
                error: None,
            })
        }
    }
    struct Tools {
        evidence: Arc<Evidence>,
        channels: Arc<dyn ChannelSource>,
    }
    impl ToolSource for Tools {
        fn tools(&self, r: &ToolRequest<'_>) -> anyhow::Result<Vec<Box<dyn Tool>>> {
            anyhow::ensure!(
                r.agent_alias == AGENT && r.memory.name() == "consumer-memory",
                "tool source did not receive canonical agent memory"
            );
            anyhow::ensure!(
                r.security.is_tool_allowed(TOOL),
                "runtime resolved the wrong tool policy"
            );
            self.evidence.tool_requests.fetch_add(1, Ordering::SeqCst);
            let channel = self
                .channels
                .channel("consumer-output")
                .ok_or_else(|| anyhow::Error::msg("supplied channel missing"))?;
            Ok(vec![Box::new(SuppliedTool {
                evidence: self.evidence.clone(),
                memory: r.memory.clone(),
                channel,
            })])
        }
    }
    struct SuppliedObserver(Arc<Evidence>);
    impl Observer for SuppliedObserver {
        fn record_event(&self, _: &ObserverEvent) {
            self.0.observed_events.fetch_add(1, Ordering::SeqCst);
        }
        fn record_metric(&self, _: &ObserverMetric) {}
        fn flush(&self) {
            self.0.flushes.fetch_add(1, Ordering::SeqCst);
        }
        fn name(&self) -> &str {
            "consumer-observer"
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    fn config(root: &std::path::Path) -> Config {
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let mut c = Config {
            data_dir: workspace.clone(),
            config_path: root.join("config.toml"),
            ..Config::default()
        };
        c.memory.backend = "none".into();
        c.memory.auto_save = false;
        c.memory.response_cache_enabled = false;
        c.channels.session_backend = "sqlite".into();
        c.providers.models.custom.insert(
            "consumer".into(),
            CustomModelProviderConfig {
                base: ModelProviderConfig {
                    uri: None,
                    model: Some("consumer-model".into()),
                    ..ModelProviderConfig::default()
                },
            },
        );
        // Unrestricted discovery is deliberate: a restrictive allowlist could hide native construction.
        c.risk_profiles.insert(
            "consumer".into(),
            RiskProfileConfig {
                auto_approve: vec![TOOL.into()],
                ..RiskProfileConfig::default()
            },
        );
        c.runtime_profiles.insert(
            "consumer".into(),
            RuntimeProfileConfig {
                max_tool_iterations: 3,
                ..RuntimeProfileConfig::default()
            },
        );
        c.agents.insert(
            AGENT.into(),
            AliasedAgentConfig {
                model_provider: "custom.consumer".into(),
                risk_profile: "consumer".into(),
                runtime_profile: "consumer".into(),
                memory: AgentMemoryConfig {
                    backend: MemoryBackendKind::None,
                },
                workspace: AgentWorkspaceConfig {
                    path: Some(workspace),
                    ..AgentWorkspaceConfig::default()
                },
                ..AliasedAgentConfig::default()
            },
        );
        c.peripherals.enabled = true;
        c.peripherals.boards.push(PeripheralBoardConfig {
            board: "consumer-construction-trap".into(),
            ..PeripheralBoardConfig::default()
        });
        c
    }

    /// The configured native factory must refuse this config. A generic turn
    /// succeeds only if it keeps using the supplied provider source.
    #[test]
    fn native_provider_construction_trap_is_armed() {
        let root = tempfile::TempDir::new().unwrap();
        let c = config(root.path());
        let request = ProviderRequest {
            config: &c,
            agent_alias: AGENT,
            provider_ref: None,
            model: Some("consumer-model"),
            principal: None,
        };
        let error = match zeroclaw_runtime::composition::defaults::ConfigProviders
            .model_provider(&request)
        {
            Ok(_) => panic!("native provider construction trap did not refuse"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("requires `uri`"),
            "unexpected native trap error: {error:#}"
        );
    }

    /// The canonical native tool factory opens the session backend while
    /// constructing its session tools. This externally visible trap catches
    /// constructing-and-discarding native tools, not only catalog leaks.
    #[test]
    fn native_tool_construction_trap_is_armed() {
        let root = tempfile::TempDir::new().unwrap();
        let c = config(root.path());
        let marker = c.data_dir.join("sessions/sessions.db");
        assert!(!marker.exists());
        let security =
            Arc::new(zeroclaw_config::policy::SecurityPolicy::for_agent(&c, AGENT).unwrap());
        let memory: Arc<dyn Memory> = Arc::new(SuppliedMemory(Arc::new(Evidence::default())));
        let built = zeroclaw_runtime::tools::all_tools(
            Arc::new(c.clone()),
            &security,
            c.risk_profile_for_agent(AGENT).unwrap(),
            AGENT,
            memory,
            None,
            None,
            &c.browser,
            &c.http_request,
            &c.web_fetch,
            &c.data_dir,
            &c.agents,
            None,
            &c,
            None,
            false,
            None,
        )
        .unwrap();
        assert!(
            built.tools.iter().any(|t| t.name() == "sessions_list"),
            "native session tool was not constructed"
        );
        assert!(
            marker.is_file(),
            "native factory no longer arms the construction trap"
        );
    }

    #[test]
    fn real_turn_uses_only_supplied_capabilities() {
        let evidence = Arc::new(Evidence::default());
        let trap = evidence.clone();
        zeroclaw_runtime::agent::loop_::register_peripheral_tools_fn(Box::new(move |config| {
            let trap = trap.clone();
            Box::pin(async move {
                if config.enabled
                    && config
                        .boards
                        .iter()
                        .any(|b| b.board == "consumer-construction-trap")
                {
                    trap.native_peripheral_constructions
                        .fetch_add(1, Ordering::SeqCst);
                }
                Ok(Vec::new())
            })
        }));
        let channels: Arc<dyn ChannelSource> = Arc::new(Channels(evidence.clone()));
        let capabilities = RuntimeCapabilities {
            providers: Arc::new(Providers(evidence.clone())),
            memory: Arc::new(Memories(evidence.clone())),
            tools: Arc::new(Tools {
                evidence: evidence.clone(),
                channels: channels.clone(),
            }),
            channels,
            observer: Arc::new(SuppliedObserver(evidence.clone())),
        };
        let turn_evidence = evidence.clone();
        std::thread::Builder::new().name("public-consumer".into()).stack_size(8 * 1024 * 1024).spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).thread_stack_size(8 * 1024 * 1024).enable_all().build().unwrap();
            runtime.block_on(async move {
                let root = tempfile::TempDir::new().unwrap();
                let c = config(root.path());
                let native_marker = c.data_dir.join("sessions/sessions.db");
                assert!(!native_marker.exists(), "tool construction trap must start unarmed");
                let reply = tokio::time::timeout(Duration::from_secs(30), Box::pin(zeroclaw_runtime::agent::run_with_capabilities(
                    c, capabilities, None, AGENT, Some("Use consumer_echo to record the supplied value.".into()),
                    None, None, None, Vec::new(), false, None, None, TurnOrigin::Interactive,
                    zeroclaw_runtime::agent::loop_::AgentRunOverrides::default(),
                ))).await.expect("public consumer turn timed out").expect("public consumer turn failed");
                assert_eq!(reply, "consumer turn complete");
                assert_eq!(turn_evidence.tool_calls.load(Ordering::SeqCst), 1);
                println!("consumer catalogs: {:?}", turn_evidence.catalogs.lock().unwrap());
                println!("native tool constructor marker present: {}", native_marker.exists());
                assert!(!native_marker.exists(), "generic turn constructed native session tools, even if their catalog is later hidden");
            });
        }).unwrap().join().unwrap_or_else(|p| std::panic::resume_unwind(p));
        assert_eq!(evidence.provider_requests.load(Ordering::SeqCst), 1);
        assert_eq!(evidence.memory_requests.load(Ordering::SeqCst), 1);
        assert_eq!(evidence.tool_requests.load(Ordering::SeqCst), 1);
        assert_eq!(evidence.channel_lookups.load(Ordering::SeqCst), 1);
        assert!(
            evidence.observed_events.load(Ordering::SeqCst) > 0,
            "supplied observer was unused"
        );
        assert_eq!(
            evidence.stored.lock().unwrap().as_slice(),
            [("consumer-proof".to_owned(), CONTENT.to_owned())]
        );
        assert_eq!(evidence.delivered.lock().unwrap().as_slice(), [CONTENT]);
        assert_eq!(
            evidence
                .native_peripheral_constructions
                .load(Ordering::SeqCst),
            0,
            "generic turn invoked the native peripheral constructor"
        );
        let catalogs = evidence.catalogs.lock().unwrap();
        assert_eq!(
            catalogs.len(),
            2,
            "the supplied tool must round-trip through the real loop"
        );
        for catalog in catalogs.iter() {
            assert_eq!(
                catalog,
                &[TOOL.to_owned()],
                "generic turn leaked concrete native tools"
            );
        }
    }

    fn capabilities(evidence: &Arc<Evidence>) -> RuntimeCapabilities {
        let channels: Arc<dyn ChannelSource> = Arc::new(Channels(evidence.clone()));
        RuntimeCapabilities {
            providers: Arc::new(Providers(evidence.clone())),
            memory: Arc::new(Memories(evidence.clone())),
            tools: Arc::new(Tools {
                evidence: evidence.clone(),
                channels: channels.clone(),
            }),
            channels,
            observer: Arc::new(SuppliedObserver(evidence.clone())),
        }
    }

    fn on_large_runtime(body: impl FnOnce(tokio::runtime::Runtime) + Send + 'static) {
        std::thread::Builder::new()
            .name("consumer-lifecycle".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .thread_stack_size(8 * 1024 * 1024)
                    .enable_all()
                    .build()
                    .unwrap();
                body(runtime);
            })
            .unwrap()
            .join()
            .unwrap_or_else(|p| std::panic::resume_unwind(p));
    }

    struct EmptyTools;
    impl ToolSource for EmptyTools {
        fn tools(&self, _: &ToolRequest<'_>) -> anyhow::Result<Vec<Box<dyn Tool>>> {
            Ok(Vec::new())
        }
    }
    struct NoToolProvider(Arc<Evidence>);
    impl Attributable for NoToolProvider {
        fn role(&self) -> Role {
            Role::Provider(ProviderKind::Model(ModelProviderKind::Custom))
        }
        fn alias(&self) -> &str {
            AGENT
        }
    }
    #[async_trait]
    impl ModelProvider for NoToolProvider {
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
            Ok("empty turn complete".into())
        }
        async fn chat(
            &self,
            request: ChatRequest<'_>,
            _: &str,
            _: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            self.0.catalogs.lock().unwrap().push(
                request
                    .tools
                    .unwrap_or_default()
                    .iter()
                    .map(|t| t.name.clone())
                    .collect(),
            );
            Ok(ChatResponse {
                text: Some("empty turn complete".into()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
            })
        }
    }
    struct NoToolProviders(Arc<Evidence>);
    impl ProviderSource for NoToolProviders {
        fn model_provider(
            &self,
            _: &ProviderRequest<'_>,
        ) -> anyhow::Result<Arc<dyn ModelProvider>> {
            Ok(Arc::new(NoToolProvider(self.0.clone())))
        }
    }

    #[test]
    fn empty_supplied_registry_is_authoritative() {
        on_large_runtime(|runtime| {
            runtime.block_on(async {
                let root = tempfile::TempDir::new().unwrap();
                let mut c = config(root.path());
                // Native integrations in config cannot enlarge an empty source.
                c.pipeline.enabled = true;
                let marker = c.data_dir.join("sessions/sessions.db");
                let evidence = Arc::new(Evidence::default());
                let mut caps = capabilities(&evidence);
                caps.providers = Arc::new(NoToolProviders(evidence.clone()));
                caps.tools = Arc::new(EmptyTools);
                let reply = zeroclaw_runtime::agent::run_with_capabilities(
                    c,
                    caps,
                    None,
                    AGENT,
                    Some("Complete a turn without tools.".into()),
                    None,
                    None,
                    None,
                    Vec::new(),
                    false,
                    None,
                    None,
                    TurnOrigin::Interactive,
                    zeroclaw_runtime::agent::loop_::AgentRunOverrides::default(),
                )
                .await
                .unwrap();
                assert_eq!(reply, "empty turn complete");
                assert_eq!(
                    evidence.catalogs.lock().unwrap().as_slice(),
                    [Vec::<String>::new()]
                );
                assert!(
                    !marker.exists(),
                    "empty source must not invoke native fallback"
                );
            })
        });
    }

    struct RefusingProviders;
    impl ProviderSource for RefusingProviders {
        fn model_provider(
            &self,
            _: &ProviderRequest<'_>,
        ) -> anyhow::Result<Arc<dyn ModelProvider>> {
            anyhow::bail!("consumer provider startup refusal")
        }
    }
    #[test]
    fn startup_refusal_never_falls_back_and_flushes() {
        on_large_runtime(|runtime| {
            runtime.block_on(async {
                let root = tempfile::TempDir::new().unwrap();
                let c = config(root.path());
                let marker = c.data_dir.join("sessions/sessions.db");
                let evidence = Arc::new(Evidence::default());
                let mut caps = capabilities(&evidence);
                caps.providers = Arc::new(RefusingProviders);
                let error = zeroclaw_runtime::agent::run_with_capabilities(
                    c,
                    caps,
                    None,
                    AGENT,
                    Some("This turn must refuse.".into()),
                    None,
                    None,
                    None,
                    Vec::new(),
                    true,
                    None,
                    None,
                    TurnOrigin::Interactive,
                    zeroclaw_runtime::agent::loop_::AgentRunOverrides::default(),
                )
                .await
                .unwrap_err();
                assert!(format!("{error:#}").contains("consumer provider startup refusal"));
                assert_eq!(evidence.tool_calls.load(Ordering::SeqCst), 0);
                assert!(evidence.delivered.lock().unwrap().is_empty());
                assert_eq!(evidence.flushes.load(Ordering::SeqCst), 1);
                assert!(!marker.exists());
            })
        });
    }

    struct ConstructionLease(Arc<AtomicUsize>);
    impl Drop for ConstructionLease {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    struct BlockingMemory {
        entered: Arc<tokio::sync::Notify>,
        released: Arc<AtomicUsize>,
    }
    #[async_trait]
    impl MemorySource for BlockingMemory {
        async fn memory(&self, _: &MemoryRequest<'_>) -> anyhow::Result<Arc<dyn Memory>> {
            let _lease = ConstructionLease(self.released.clone());
            self.entered.notify_one();
            std::future::pending().await
        }
    }
    #[test]
    fn cancellation_during_memory_construction_releases_and_flushes() {
        on_large_runtime(|runtime| {
            runtime.block_on(async {
            let root = tempfile::TempDir::new().unwrap(); let c = config(root.path());
            let marker = c.data_dir.join("sessions/sessions.db");
            let evidence = Arc::new(Evidence::default()); let mut caps = capabilities(&evidence);
            let entered = Arc::new(tokio::sync::Notify::new()); let released = Arc::new(AtomicUsize::new(0));
            caps.memory = Arc::new(BlockingMemory { entered: entered.clone(), released: released.clone() });
            let mut turn = Box::pin(zeroclaw_runtime::agent::run_with_capabilities(c, caps, None, AGENT, Some("Cancel while constructing memory.".into()),
                None, None, None, Vec::new(), true, None, None, TurnOrigin::Interactive,
                zeroclaw_runtime::agent::loop_::AgentRunOverrides::default()));
            tokio::time::timeout(Duration::from_secs(5), async {
                tokio::select! { _ = entered.notified() => {}, result = &mut turn => panic!("construction did not wait: {result:?}") }
            }).await.expect("memory source must reach construction");
            drop(turn);
            assert_eq!(released.load(Ordering::SeqCst), 1, "cancel must drop the active construction future");
            assert_eq!(evidence.flushes.load(Ordering::SeqCst), 1);
            assert_eq!(evidence.tool_requests.load(Ordering::SeqCst), 0);
            assert!(!marker.exists());
        })
        });
    }
    #[test]
    fn completed_turn_flushes_and_releases_generation_observer() {
        on_large_runtime(|runtime| {
            runtime.block_on(async {
                let root = tempfile::TempDir::new().unwrap();
                let c = config(root.path());
                let evidence = Arc::new(Evidence::default());
                let mut caps = capabilities(&evidence);
                let observer = Arc::new(SuppliedObserver(evidence.clone()));
                let weak = Arc::downgrade(&observer);
                caps.observer = observer;
                let reply = zeroclaw_runtime::agent::run_with_capabilities(
                    c,
                    caps,
                    None,
                    AGENT,
                    Some("Use consumer_echo.".into()),
                    None,
                    None,
                    None,
                    Vec::new(),
                    true,
                    None,
                    None,
                    TurnOrigin::Interactive,
                    zeroclaw_runtime::agent::loop_::AgentRunOverrides::default(),
                )
                .await
                .unwrap();
                assert_eq!(reply, "consumer turn complete");
                assert_eq!(evidence.flushes.load(Ordering::SeqCst), 1);
                assert!(
                    weak.upgrade().is_none(),
                    "completed turn retains its capability generation"
                );
            })
        });
    }
}

#[cfg(test)]
mod dependency;
