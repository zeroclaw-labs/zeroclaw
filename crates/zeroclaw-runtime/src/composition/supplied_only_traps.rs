//! A supplied-only build constructs no native integration at any eager entry
//! point, even with MCP, peripherals and workspace skills configured, and in
//! native compatibility mode those native tools keep their names.
//!
//! Each native integration is configured with a trap that leaves evidence when
//! it is constructed: a real stdio MCP server that writes a launch marker, a
//! peripheral board the registered peripheral factory counts (and one that
//! builds a named tool), and a workspace skill that declares a tool. The
//! native-recipe controls show each trap fires, so the supplied-only tests
//! cannot pass vacuously.

use super::test_support::{
    PERIPHERAL_TRAP_TOOL, RecordingMemory, RecordingProviders, peripheral_trap_loads,
    recording_capabilities, register_peripheral_trap,
};
use super::{
    ProviderRequest, ProviderSource, RegistryConstruction, RuntimeCapabilities, ToolRequest,
    ToolSource,
};
use crate::tools::Tool;
use async_trait::async_trait;
use parking_lot::{Mutex, RwLock};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use zeroclaw_api::ingress::TurnOrigin;
use zeroclaw_api::model_provider::{ChatRequest, ChatResponse, ModelProvider};
use zeroclaw_api::tool::{ToolOutput, ToolResult};
use zeroclaw_config::schema::{
    AliasedAgentConfig, Config, McpBundleConfig, McpServerConfig, McpTransport,
    ModelProviderConfig, OpenAIModelProviderConfig, PeripheralBoardConfig, RiskProfileConfig,
};

const AGENT: &str = "trap-agent";
const SOURCE_TOOL: &str = "trap_source_tool";
const SOURCE_DESCRIPTION: &str = "the supplied tool";
const REPLY: &str = "trap turn answered";

struct NativeTraps {
    mcp_marker: PathBuf,
    /// Counted board: any peripheral load counts it.
    board: String,
    /// The tool the second trap board makes the peripheral factory build.
    peripheral_tool: String,
    skill: String,
    /// The composed name of the tool the trap skill declares.
    skill_tool: String,
}

impl NativeTraps {
    fn arm(config: &mut Config, root: &Path) -> Self {
        let label = uuid::Uuid::new_v4().simple().to_string();
        // A symlink to the checked-in executable fixture: writing a fresh
        // script in a multithreaded test process can race a concurrent fork.
        let server = root.join("native-trap-mcp-server.sh");
        std::os::unix::fs::symlink(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mcp-server.sh"),
            &server,
        )
        .expect("link the stdio MCP fixture");
        let mcp_marker = root.join("native-trap-mcp-launched");
        config.mcp.enabled = true;
        config.mcp.deferred_loading = false;
        config.mcp.servers.push(McpServerConfig {
            name: "native_trap".into(),
            command: server.display().to_string(),
            args: vec![mcp_marker.display().to_string()],
            transport: McpTransport::Stdio,
            ..Default::default()
        });
        config.mcp_bundles.insert(
            "native-trap".into(),
            McpBundleConfig {
                servers: vec!["native_trap".into()],
                exclude: Vec::new(),
            },
        );
        config
            .agents
            .get_mut(AGENT)
            .expect("trap agent")
            .mcp_bundles
            .push("native-trap".into());

        register_peripheral_trap();
        let board = format!("native-trap-{label}");
        let peripheral_tool = format!("trap_gpio_{label}");
        config.peripherals.enabled = true;
        for name in [board.clone(), format!("native-trap-tool-{peripheral_tool}")] {
            config.peripherals.boards.push(PeripheralBoardConfig {
                board: name,
                ..Default::default()
            });
        }

        let skill = format!("trapskill{label}");
        let skill_dir = config
            .agent_workspace_dir(AGENT)
            .join("skills")
            .join(&skill);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.toml"),
            format!(
                "[skill]\nname = \"{skill}\"\ndescription = \"Native skill loading trap.\"\n\
                 version = \"0.1.0\"\n\n[[tools]]\nname = \"probe\"\n\
                 description = \"Native skill tool trap.\"\nkind = \"shell\"\ncommand = \"true\"\n"
            ),
        )
        .unwrap();
        let skill_tool = crate::tools::skill_tool::composed_tool_name(&skill, "probe");

        Self {
            mcp_marker,
            board,
            peripheral_tool,
            skill,
            skill_tool,
        }
    }

    fn mcp_launched(&self) -> bool {
        self.mcp_marker.exists()
    }

    fn peripheral_loads(&self) -> usize {
        peripheral_trap_loads(&self.board)
    }
}

fn trap_config(root: &Path) -> Config {
    let mut config = Config {
        data_dir: root.join("data"),
        config_path: root.join("config.toml"),
        ..Default::default()
    };
    config.memory.backend = "none".into();
    config.memory.auto_save = false;
    config.plugins.enabled = false;
    config.plugins.auto_discover = false;
    config.risk_profiles.insert(
        "trap".into(),
        RiskProfileConfig {
            auto_approve: vec![SOURCE_TOOL.into()],
            ..Default::default()
        },
    );
    config.providers.models.openai.insert(
        "trap".into(),
        OpenAIModelProviderConfig {
            base: ModelProviderConfig {
                model: Some("trap-model".into()),
                // Unroutable: only the supplied provider can answer.
                uri: Some("http://127.0.0.1:9".into()),
                api_key: Some("trap-key".into()),
                ..Default::default()
            },
        },
    );
    config.agents.insert(
        AGENT.into(),
        AliasedAgentConfig {
            model_provider: "openai.trap".into(),
            risk_profile: "trap".into(),
            ..Default::default()
        },
    );
    config
}

struct SourceTool(String);

impl zeroclaw_api::attribution::Attributable for SourceTool {
    fn role(&self) -> zeroclaw_api::attribution::Role {
        zeroclaw_api::attribution::Role::Tool(zeroclaw_api::attribution::ToolKind::Plugin)
    }
    fn alias(&self) -> &str {
        &self.0
    }
}

#[async_trait]
impl Tool for SourceTool {
    fn name(&self) -> &str {
        &self.0
    }
    fn description(&self) -> &str {
        SOURCE_DESCRIPTION
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
        Ok(ToolResult {
            success: true,
            output: ToolOutput::default(),
            error: None,
        })
    }
}

/// Supplies one tool; `native` also selects the native compatibility recipe.
struct TrapTools {
    native: bool,
}

impl ToolSource for TrapTools {
    fn native_recipe<'s>(
        &'s self,
        _request: &ToolRequest<'_>,
    ) -> anyhow::Result<Option<&'s super::NativeRegistryRecipe>> {
        Ok(self
            .native
            .then_some(&super::defaults::native_compat_registry))
    }

    fn tools(&self, _request: &ToolRequest<'_>) -> anyhow::Result<Vec<Box<dyn Tool>>> {
        Ok(vec![Box::new(SourceTool(SOURCE_TOOL.into()))])
    }
}

/// What the turn sent the provider: each system prompt, and the names of the
/// tools each request offered.
#[derive(Default)]
struct Sent {
    prompts: Vec<String>,
    offered: Vec<Vec<String>>,
}

/// Answers every chat and records what it was sent.
struct PromptProvider(Arc<Mutex<Sent>>);

impl zeroclaw_api::attribution::Attributable for PromptProvider {
    fn role(&self) -> zeroclaw_api::attribution::Role {
        zeroclaw_api::attribution::Role::Provider(zeroclaw_api::attribution::ProviderKind::Model(
            zeroclaw_api::attribution::ModelProviderKind::Custom,
        ))
    }
    fn alias(&self) -> &str {
        "trap"
    }
}

#[async_trait]
impl ModelProvider for PromptProvider {
    // Native tool calling, so each request lists the tools it offers.
    fn capabilities(&self) -> zeroclaw_api::model_provider::ProviderCapabilities {
        zeroclaw_api::model_provider::ProviderCapabilities {
            native_tool_calling: true,
            ..Default::default()
        }
    }

    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        let mut sent = self.0.lock();
        sent.prompts
            .push(system_prompt.unwrap_or_default().to_string());
        sent.offered.push(Vec::new());
        Ok(REPLY.into())
    }

    async fn chat(
        &self,
        request: ChatRequest<'_>,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<ChatResponse> {
        let system = request
            .messages
            .iter()
            .filter(|message| message.role == "system")
            .map(|message| message.content.clone())
            .collect::<Vec<_>>()
            .join("\n");
        let offered = request
            .tools
            .unwrap_or_default()
            .iter()
            .map(|tool| tool.name.clone())
            .collect();
        let mut sent = self.0.lock();
        sent.prompts.push(system);
        sent.offered.push(offered);
        Ok(ChatResponse {
            text: Some(REPLY.into()),
            tool_calls: Vec::new(),
            usage: None,
            reasoning_content: None,
        })
    }
}

struct PromptProviders(Arc<Mutex<Sent>>);

impl ProviderSource for PromptProviders {
    fn model_provider(
        &self,
        _request: &ProviderRequest<'_>,
    ) -> anyhow::Result<Arc<dyn ModelProvider>> {
        Ok(Arc::new(PromptProvider(Arc::clone(&self.0))))
    }
}

fn trap_capabilities(native: bool, sent: &Arc<Mutex<Sent>>) -> RuntimeCapabilities {
    let mut capabilities = recording_capabilities(
        Arc::new(RecordingProviders::default()),
        Arc::new(RecordingMemory::default()),
        Arc::new(TrapTools { native }),
    );
    capabilities.providers = Arc::new(PromptProviders(Arc::clone(sent)));
    capabilities
}

/// What one entry point left behind.
struct Evidence {
    mcp_launched: bool,
    peripheral_loads: usize,
    /// The system prompts the turn sent, or the agent's prompt; empty for
    /// entry points that only assemble a registry.
    prompts: Vec<String>,
    /// The registry's names, or for a turn every tool name it offered.
    tools: Vec<String>,
}

impl Evidence {
    fn skill_in_prompt(&self, traps: &NativeTraps) -> bool {
        self.prompts
            .iter()
            .any(|prompt| prompt.contains(&traps.skill))
    }

    fn has_tool(&self, name: &str) -> bool {
        self.tools.iter().any(|tool| tool == name)
    }

    fn assert_supplied_only(&self, traps: &NativeTraps, entry: &str) {
        assert!(
            !self.mcp_launched,
            "{entry}: a supplied-only build launched the MCP server"
        );
        assert_eq!(
            self.peripheral_loads, 0,
            "{entry}: a supplied-only build loaded peripherals"
        );
        assert!(
            !self.skill_in_prompt(traps),
            "{entry}: a supplied-only build loaded workspace skills"
        );
        assert_eq!(
            self.tools,
            [SOURCE_TOOL.to_string()],
            "{entry}: the supplied vector is the whole registry"
        );
    }
}

/// Run `entry` against a trapped config with the given recipe choice.
async fn exercise(entry: &str, native: bool) -> (Evidence, NativeTraps, tempfile::TempDir) {
    let root = tempfile::TempDir::new().unwrap();
    let mut config = trap_config(root.path());
    let traps = NativeTraps::arm(&mut config, root.path());
    let sent = Arc::new(Mutex::new(Sent::default()));
    let capabilities = trap_capabilities(native, &sent);
    let offered = |sent: &Arc<Mutex<Sent>>| {
        let mut names: Vec<String> = sent.lock().offered.iter().flatten().cloned().collect();
        names.sort();
        names.dedup();
        names
    };
    let tools = match entry {
        "agent" => {
            let agent = crate::agent::agent::Agent::from_config_with_capabilities(
                &config,
                AGENT,
                &capabilities,
                None,
            )
            .await
            .expect("the agent builds");
            sent.lock().prompts.push(
                agent
                    .system_prompt_for_test()
                    .expect("the agent prompt renders"),
            );
            agent.tool_names().into_iter().map(str::to_string).collect()
        }
        "run" => {
            let reply = crate::agent::loop_::run_with_capabilities(
                config,
                capabilities,
                None,
                AGENT,
                Some("hello".into()),
                None,
                None,
                None,
                Vec::new(),
                false,
                None,
                None,
                TurnOrigin::Interactive,
                crate::agent::loop_::AgentRunOverrides::default(),
            )
            .await
            .expect("the CLI turn runs");
            assert_eq!(reply, REPLY);
            offered(&sent)
        }
        "process_message" => {
            let reply = crate::agent::loop_::process_message_with_capabilities(
                Arc::new(config),
                capabilities,
                None,
                AGENT,
                "hello",
                None,
                TurnOrigin::SubTurn,
                None,
            )
            .await
            .expect("the message turn runs");
            assert_eq!(reply, REPLY);
            offered(&sent)
        }
        "owned_sop" => {
            let engine = Arc::new(std::sync::Mutex::new(crate::sop::SopEngine::new(
                zeroclaw_config::schema::SopConfig::default(),
            )));
            let owned = crate::agent::turn::assemble_owned_execution_with_capabilities(
                &config,
                None,
                &capabilities,
                None,
                AGENT,
                engine,
                None,
                None,
                None,
            )
            .await
            .expect("the owned SOP step assembles");
            owned
                .tools_registry
                .iter()
                .map(|tool| tool.name().to_string())
                .collect()
        }
        "independent_delegate" => {
            let config = Arc::new(config);
            let policy = Arc::new(
                zeroclaw_config::policy::SecurityPolicy::for_agent(&config, AGENT)
                    .expect("trap policy resolves"),
            );
            let delegate = crate::tools::delegate::DelegateTool::new(
                config.agents.clone(),
                None,
                Arc::clone(&policy),
            )
            .with_root_config(Arc::clone(&config))
            .with_caller_alias(AGENT)
            .with_runtime(Arc::new(crate::platform::NativeRuntime::new()))
            .with_capabilities(capabilities, None);
            let target = delegate
                .independent_agentic_tools_for_target(AGENT, policy)
                .await
                .expect("the independent target registry builds");
            target
                .tools
                .iter()
                .map(|tool| tool.name().to_string())
                .collect()
        }
        _ => unreachable!("unknown entry point"),
    };
    let evidence = Evidence {
        mcp_launched: traps.mcp_launched(),
        peripheral_loads: traps.peripheral_loads(),
        prompts: sent.lock().prompts.clone(),
        tools,
    };
    (evidence, traps, root)
}

async fn supplied_only(entry: &str) {
    let (evidence, traps, _root) = exercise(entry, false).await;
    evidence.assert_supplied_only(&traps, entry);
}

#[tokio::test]
async fn supplied_only_agent_constructs_no_native_integration() {
    supplied_only("agent").await;
}

#[tokio::test]
async fn supplied_only_cli_turn_constructs_no_native_integration() {
    supplied_only("run").await;
}

#[tokio::test]
async fn supplied_only_process_message_constructs_no_native_integration() {
    supplied_only("process_message").await;
}

#[tokio::test]
async fn supplied_only_owned_sop_step_constructs_no_native_integration() {
    supplied_only("owned_sop").await;
}

#[tokio::test]
async fn supplied_only_independent_delegate_constructs_no_native_integration() {
    supplied_only("independent_delegate").await;
}

/// The same builds with the native recipe trip every trap their entry point
/// reaches, so the supplied-only tests above are not passing vacuously.
#[tokio::test]
async fn native_recipe_trips_the_construction_traps() {
    for (entry, peripherals, prompt) in [
        ("agent", false, true),
        ("run", true, true),
        ("process_message", true, true),
        ("owned_sop", false, false),
        ("independent_delegate", false, false),
    ] {
        let (evidence, traps, _root) = exercise(entry, true).await;
        assert!(evidence.mcp_launched, "{entry}: the MCP trap did not fire");
        assert!(
            evidence.has_tool(&traps.skill_tool),
            "{entry}: the skill tool trap did not fire: {:?}",
            evidence.tools
        );
        assert!(
            evidence.has_tool(SOURCE_TOOL),
            "{entry}: the source tool is kept alongside native tools"
        );
        if peripherals {
            assert!(
                evidence.peripheral_loads > 0,
                "{entry}: the peripheral trap did not fire"
            );
            assert!(
                evidence.has_tool(&traps.peripheral_tool),
                "{entry}: the peripheral tool was not offered: {:?}",
                evidence.tools
            );
        }
        if prompt {
            assert!(
                evidence.skill_in_prompt(&traps),
                "{entry}: the skill was not in the prompt"
            );
        }
    }
}

/// Assemble `source` tools in native compatibility mode with peripherals and
/// the given skills, and a delegate parent handle.
async fn assemble_native_compat(
    config: &Config,
    source: Vec<Box<dyn Tool>>,
    skills: &[crate::skills::Skill],
) -> (
    crate::tools::scoped::ScopedAssembled,
    crate::tools::DelegateParentToolsHandle,
) {
    let tail = 0..source.len();
    let mut built = crate::tools::AllToolsResult::from_prebuilt_tools(source);
    let parent: crate::tools::DelegateParentToolsHandle = Arc::new(RwLock::new(Vec::new()));
    built.delegate_handle = Some(Arc::clone(&parent));
    let security = Arc::new(
        zeroclaw_config::policy::SecurityPolicy::for_agent(config, AGENT)
            .expect("trap policy resolves"),
    );
    let assembled = crate::tools::scoped::ScopedToolRegistry::assemble_prepared(
        crate::tools::scoped::ScopedAssembly {
            config,
            agent_alias: AGENT,
            security: &security,
            built,
            skills,
            runtime: Arc::new(crate::platform::NativeRuntime::new()),
            caller_allowed: None,
            connect_mcp: false,
            connect_peripherals: true,
            exclude_memory: false,
            acp_delivery: false,
            list_deferred_mcp_specs: false,
            emit_assembly_logs: false,
            mcp_registry: None,
        },
        RegistryConstruction::NativeCompat,
        tail,
    )
    .await;
    (assembled, parent)
}

/// One tool owns `name`: it is not the source tool, and the delegate's parent
/// view holds no source tool with that name either.
fn assert_native_owns(
    assembled: &crate::tools::scoped::ScopedAssembled,
    parent: &crate::tools::DelegateParentToolsHandle,
    name: &str,
) -> String {
    let named: Vec<_> = assembled
        .registry
        .iter()
        .filter(|tool| tool.name() == name)
        .collect();
    assert_eq!(named.len(), 1, "{name}: exactly one tool owns the name");
    let found = crate::agent::tool_execution::find_tool(&assembled.registry, name)
        .expect("the tool is registered");
    assert_ne!(
        found.description(),
        SOURCE_DESCRIPTION,
        "{name}: dispatch reaches the native tool"
    );
    assert!(
        !parent
            .read()
            .iter()
            .any(|tool| tool.name() == name && tool.description() == SOURCE_DESCRIPTION),
        "{name}: the delegate's parent view holds no source tool with the name"
    );
    found.description().to_string()
}

/// A peripheral tool is native: a source tool with its name gives way, in the
/// registry and in the delegate's parent view.
#[tokio::test]
async fn native_compat_peripheral_tool_retires_a_same_name_source_tool() {
    let root = tempfile::TempDir::new().unwrap();
    let mut config = trap_config(root.path());
    let traps = NativeTraps::arm(&mut config, root.path());
    let (assembled, parent) = assemble_native_compat(
        &config,
        vec![Box::new(SourceTool(traps.peripheral_tool.clone()))],
        &[],
    )
    .await;
    let description = assert_native_owns(&assembled, &parent, &traps.peripheral_tool);
    assert_eq!(description, PERIPHERAL_TRAP_TOOL);
}

/// A skill tool is native too: a source tool with its name gives way instead
/// of making the skill tool get skipped.
#[tokio::test]
async fn native_compat_skill_tool_retires_a_same_name_source_tool() {
    let root = tempfile::TempDir::new().unwrap();
    let mut config = trap_config(root.path());
    let traps = NativeTraps::arm(&mut config, root.path());
    let skills = crate::skills::load_skills_for_agent_from_config(&config, AGENT);
    assert!(
        skills.iter().any(|skill| skill.name == traps.skill),
        "the trap skill loads"
    );
    let (assembled, parent) = assemble_native_compat(
        &config,
        vec![Box::new(SourceTool(traps.skill_tool.clone()))],
        &skills,
    )
    .await;
    assert_native_owns(&assembled, &parent, &traps.skill_tool);
}

/// A source vector with a repeated name keeps the first tool, in both modes.
#[tokio::test]
async fn duplicate_source_names_keep_the_first_tool_in_both_modes() {
    struct Repeated;
    impl ToolSource for Repeated {
        fn tools(&self, _request: &ToolRequest<'_>) -> anyhow::Result<Vec<Box<dyn Tool>>> {
            Ok(vec![
                Box::new(SourceTool(SOURCE_TOOL.into())),
                Box::new(SourceTool(SOURCE_TOOL.into())),
            ])
        }
    }
    let root = tempfile::TempDir::new().unwrap();
    let config = trap_config(root.path());
    let capabilities = recording_capabilities(
        Arc::new(RecordingProviders::default()),
        Arc::new(RecordingMemory::default()),
        Arc::new(Repeated),
    );
    let policy = Arc::new(
        zeroclaw_config::policy::SecurityPolicy::for_agent(&config, AGENT)
            .expect("trap policy resolves"),
    );
    let runtime: Arc<dyn zeroclaw_api::runtime_traits::RuntimeAdapter> =
        Arc::new(crate::platform::NativeRuntime::new());
    let memory: Arc<dyn zeroclaw_api::memory_traits::Memory> =
        Arc::new(zeroclaw_memory::NoneMemory::new("none"));
    let tool_config = Arc::new(config.clone());
    let request = ToolRequest {
        config: &tool_config,
        agent_alias: AGENT,
        security: &policy,
        runtime: &runtime,
        memory: &memory,
    };
    for construction in [
        RegistryConstruction::SuppliedOnly,
        RegistryConstruction::NativeCompat,
    ] {
        let prepared = capabilities
            .prepare_registry(
                crate::tools::AllToolsResult::from_prebuilt_tools(Vec::new()),
                construction,
                &request,
                None,
            )
            .expect("the registry prepares");
        let names: Vec<_> = prepared
            .built
            .tools
            .iter()
            .map(|tool| tool.name())
            .collect();
        assert_eq!(names, [SOURCE_TOOL], "{construction:?}: one tool per name");
        assert_eq!(prepared.supplied_tail, 0..1, "{construction:?}");
    }
}
