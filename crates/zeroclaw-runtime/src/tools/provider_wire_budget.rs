//! Test-only provider-wire token budget for the Hyperion lean profile.
//!
//! Counts tokens the way a native OpenAI turn does: `build_iteration_tool_specs`
//! then `OpenAiModelProvider::chat_tools_wire` (the `NativeChatRequest.tools`
//! array). The gate is `ceil(tools_json.len() / 4)` on that whole array, not a
//! sum of per-tool ceils.

use super::all_tools_with_runtime;
use super::scoped::{ScopedAssembled, ScopedAssembly, ScopedToolRegistry};
use crate::platform::NativeRuntime;
use crate::security::SecurityPolicy;
use crate::skills::Skill;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tempfile::TempDir;
use zeroclaw_config::schema::{Config, MemoryConfig};
use zeroclaw_memory::Memory;
use zeroclaw_providers::openai::OpenAiModelProvider;

const LEAN_PROFILE_TOML: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/hyperion-lean-profile.toml"
));

/// Copied `SKILL.md` files from `.claude/skills/{zeroclaw,changelog-generation}/`.
/// This repo has no Hyperion trading skill bundle; these are contributor
/// instruction-only skills (no `SKILL.toml` tools), not a product bundle.
const COPIED_SKILL_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/lean-skill-bundle"
);

/// Phrase from the copied `zeroclaw/SKILL.md` body, not its description.
const SKILL_BODY_PHRASE: &str = "Adaptive Expertise";
const MEMORY_MARKER: &str = "HYPERION_LEAN_MEMORY_MD_MARKER";

/// Loose freeze. The gate is the worst-case lean assembly (copied skill
/// fixture + ExplicitOnly MCP + WeChat `inject_memory = false`).
const LEAN_PROVIDER_WIRE_TOKEN_CEILING: usize = 5_000;

const PARSE_HELPER_TABLES: &[&str] = &["data_retention", "security", "security_ops"];

#[derive(Debug, Clone)]
struct ToolWireRow {
    name: String,
    parameters_tokens: usize,
    native_tokens: usize,
}

#[derive(Debug)]
struct WireBudget {
    names: Vec<String>,
    rows: Vec<ToolWireRow>,
    parameters_tokens: usize,
    native_tools_tokens: usize,
    system_prompt: String,
    system_prompt_tokens: usize,
    whole_turn_tokens: usize,
    tool_search_present: bool,
}

fn estimate_tokens(s: &str) -> usize {
    s.len().div_ceil(4)
}

fn parse_fragment(raw: &str) -> Config {
    let mut merged = raw.trim().to_string();
    for table in PARSE_HELPER_TABLES {
        if merged.contains(&format!("[{table}]")) {
            continue;
        }
        merged.push_str("\n\n[");
        merged.push_str(table);
        merged.push(']');
    }
    merged.push('\n');
    toml::from_str(&merged).unwrap_or_else(|e| panic!("lean profile toml must parse: {e}"))
}

fn pin_install(config: &mut Config, tmp: &TempDir) {
    config.config_path = tmp.path().join("config.toml");
    config.data_dir = tmp.path().join("data");
    if let Some(agent) = config.agents.get_mut("hyperion") {
        agent.workspace.path = Some(tmp.path().join("workspace"));
    }
}

fn lean_config(tmp: &TempDir) -> Config {
    let mut config = parse_fragment(LEAN_PROFILE_TOML);
    pin_install(&mut config, tmp);
    config
}

fn copied_skill_fixture_dir() -> PathBuf {
    PathBuf::from(COPIED_SKILL_FIXTURE)
}

fn attach_copied_skill_fixture_for_agent(config: &mut Config, agent_alias: &str) {
    let dir = copied_skill_fixture_dir();
    assert!(
        dir.join("zeroclaw").join("SKILL.md").is_file(),
        "copied skill fixture missing: {}",
        dir.display()
    );
    config.skill_bundles.insert(
        "copied_skills".into(),
        zeroclaw_config::schema::SkillBundleConfig {
            directory: Some(dir.to_string_lossy().into_owned()),
            include: Vec::new(),
            exclude: Vec::new(),
        },
    );
    config
        .agents
        .entry(agent_alias.to_string())
        .or_default()
        .skill_bundles
        .push("copied_skills".into());
}

fn attach_copied_skill_fixture(config: &mut Config) {
    attach_copied_skill_fixture_for_agent(config, "hyperion");
}

fn point_hapi_edge_at_agent(config: &mut Config, url: String, agent_alias: &str) {
    if let Some(s) = config
        .mcp
        .servers
        .iter_mut()
        .find(|s| s.name == "hapi-edge")
    {
        s.url = Some(url);
        s.transport = zeroclaw_config::schema::McpTransport::Http;
    } else {
        config
            .mcp
            .servers
            .push(zeroclaw_config::schema::McpServerConfig {
                name: "hapi-edge".into(),
                url: Some(url),
                transport: zeroclaw_config::schema::McpTransport::Http,
                ..zeroclaw_config::schema::McpServerConfig::default()
            });
    }
    config.mcp_bundles.insert(
        agent_alias.into(),
        zeroclaw_config::schema::McpBundleConfig {
            servers: vec!["hapi-edge".into()],
            exclude: Vec::new(),
        },
    );
    config
        .agents
        .entry(agent_alias.to_string())
        .or_default()
        .mcp_bundles
        .push(agent_alias.into());
}

fn point_hapi_edge_at(config: &mut Config, url: String) {
    point_hapi_edge_at_agent(config, url, "hyperion");
}

fn seed_personality(workspace: &Path, include_memory: bool) {
    std::fs::create_dir_all(workspace).unwrap();
    let files = [
        (
            "AGENTS.md",
            include_str!("../agent/personality_templates/AGENTS.md"),
        ),
        (
            "SOUL.md",
            include_str!("../agent/personality_templates/SOUL.md"),
        ),
        (
            "TOOLS.md",
            include_str!("../agent/personality_templates/TOOLS.md"),
        ),
        (
            "IDENTITY.md",
            include_str!("../agent/personality_templates/IDENTITY.md"),
        ),
        (
            "USER.md",
            include_str!("../agent/personality_templates/USER.md"),
        ),
    ];
    for (name, body) in files {
        std::fs::write(workspace.join(name), body).unwrap();
    }
    let memory = if include_memory {
        format!(
            "{}\n{MEMORY_MARKER}\n",
            include_str!("../agent/personality_templates/MEMORY.md")
        )
    } else {
        format!("{MEMORY_MARKER}\nnever inject this on the WeChat path\n")
    };
    std::fs::write(workspace.join("MEMORY.md"), memory).unwrap();
}

async fn mock_mcp_http_server(tools: &[(&str, &str)]) -> wiremock::MockServer {
    use wiremock::matchers::{body_partial_json, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(
            serde_json::json!({"method": "initialize"}),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Mcp-Session-Id", "s")
                .set_body_json(serde_json::json!({
                    "jsonrpc":"2.0","id":1,
                    "result":{"capabilities":{"tools":{}}}
                })),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_partial_json(
            serde_json::json!({"method":"notifications/initialized"}),
        ))
        .respond_with(ResponseTemplate::new(202))
        .mount(&server)
        .await;
    let tool_json: Vec<Value> = tools
        .iter()
        .map(|(name, desc)| {
            serde_json::json!({
                "name": name,
                "description": desc,
                "inputSchema": {"type": "object"}
            })
        })
        .collect();
    Mock::given(method("POST"))
        .and(body_partial_json(
            serde_json::json!({"method":"tools/list"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "jsonrpc":"2.0","id":2,"result":{"tools":tool_json}
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_partial_json(
            serde_json::json!({"method":"resources/list"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "jsonrpc":"2.0","id":3,"result":{"resources":[]}
        })))
        .mount(&server)
        .await;
    server
}

struct TurnRequest<'a> {
    config: &'a Config,
    agent_alias: &'a str,
    skills: &'a [Skill],
    connect_mcp: bool,
    inject_memory: bool,
    workspace: &'a Path,
}

async fn assemble_turn(req: TurnRequest<'_>) -> (ScopedAssembled, WireBudget) {
    let workspace = req.workspace;
    std::fs::create_dir_all(workspace).unwrap();
    let mem_cfg = MemoryConfig {
        backend: "markdown".into(),
        ..MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> =
        Arc::from(zeroclaw_memory::create_memory(&mem_cfg, workspace, None).unwrap());
    let runtime: Arc<dyn crate::platform::RuntimeAdapter> = Arc::new(NativeRuntime::new());
    let security = if req.config.agents.contains_key(req.agent_alias) {
        Arc::new(
            SecurityPolicy::for_agent(req.config, req.agent_alias)
                .expect("lean agent must resolve a risk profile"),
        )
    } else {
        Arc::new(SecurityPolicy {
            workspace_dir: workspace.to_path_buf(),
            ..SecurityPolicy::default()
        })
    };
    let risk_profile = req
        .config
        .risk_profile_for_agent(req.agent_alias)
        .cloned()
        .unwrap_or_default();
    let built = all_tools_with_runtime(
        Arc::new(req.config.clone()),
        &security,
        &risk_profile,
        req.agent_alias,
        Arc::clone(&runtime),
        mem,
        &req.config.browser,
        &req.config.http_request,
        &req.config.web_fetch,
        workspace,
        &req.config.agents,
        None,
        req.config,
        None,
        None,
        None,
        None,
    );
    let assembled = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        ScopedToolRegistry::assemble(ScopedAssembly {
            config: req.config,
            agent_alias: req.agent_alias,
            security: &security,
            built,
            skills: req.skills,
            runtime,
            caller_allowed: None,
            connect_mcp: req.connect_mcp,
            exclude_memory: false,
            list_deferred_mcp_specs: false,
            emit_assembly_logs: false,
            mcp_registry: None,
        }),
    )
    .await
    .expect("assemble must not hang");

    let provider = OpenAiModelProvider::builder("wire-budget").build();
    let iteration = crate::agent::turn::build_iteration_tool_specs(
        &provider,
        &assembled.registry,
        &[],
        assembled.activated_handle.as_ref(),
    )
    .expect("iteration tool specs");
    let tools_json = OpenAiModelProvider::chat_tools_wire(&iteration.tool_specs)
        .expect("native tools array must serialize");
    let native_tools_tokens = estimate_tokens(&tools_json);
    let params_json = serde_json::to_string(
        &iteration
            .tool_specs
            .iter()
            .map(|spec| spec.parameters.as_ref())
            .collect::<Vec<_>>(),
    )
    .expect("parameter schemas must serialize");
    let parameters_tokens = estimate_tokens(&params_json);

    let parsed: Vec<Value> = serde_json::from_str(&tools_json).unwrap_or_default();
    let mut rows: Vec<ToolWireRow> = parsed
        .iter()
        .map(|item| {
            let name = item
                .pointer("/function/name")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_string();
            let parameters = item
                .pointer("/function/parameters")
                .cloned()
                .unwrap_or(Value::Null);
            ToolWireRow {
                name,
                parameters_tokens: estimate_tokens(&parameters.to_string()),
                native_tokens: estimate_tokens(&item.to_string()),
            }
        })
        .collect();
    rows.sort_by(|a, b| {
        b.native_tokens
            .cmp(&a.native_tokens)
            .then(a.name.cmp(&b.name))
    });

    let names: Vec<String> = assembled
        .registry
        .iter()
        .map(|t| t.name().to_string())
        .collect();
    let tool_search_present = names.iter().any(|n| n == "tool_search");
    let tool_pairs: Vec<(&str, &str)> = names.iter().map(|n| (n.as_str(), "")).collect();
    let skills_mode = req.config.effective_skills_prompt_mode(req.agent_alias);
    let mut system_prompt = crate::agent::system_prompt::build_system_prompt_with_persona(
        workspace,
        "test-model",
        &tool_pairs,
        req.skills,
        None,
        Some(6000),
        Some(&risk_profile),
        true,
        skills_mode,
        true,
        0,
        req.inject_memory,
        false,
        None,
        crate::agent::persona_projection::LegacyPersonaFiles::Inject,
    );
    let mcp_section = assembled.combined_mcp_prompt_section();
    if !mcp_section.is_empty() {
        system_prompt.push('\n');
        system_prompt.push_str(&mcp_section);
    }
    let system_prompt_tokens = estimate_tokens(&system_prompt);
    let whole_turn_tokens = system_prompt_tokens + native_tools_tokens;
    (
        assembled,
        WireBudget {
            names,
            rows,
            parameters_tokens,
            native_tools_tokens,
            system_prompt,
            system_prompt_tokens,
            whole_turn_tokens,
            tool_search_present,
        },
    )
}

fn print_budget(label: &str, budget: &WireBudget) {
    eprintln!("=== provider-wire budget: {label} ===");
    eprintln!("{:<28} {:>10} {:>10}", "tool", "params_tok", "native_tok");
    for row in &budget.rows {
        eprintln!(
            "{:<28} {:>10} {:>10}",
            row.name, row.parameters_tokens, row.native_tokens
        );
    }
    eprintln!(
        "tools={} parameters_tok={} native_tools_tok={} system_prompt_tok={} whole_turn_tok={}",
        budget.names.len(),
        budget.parameters_tokens,
        budget.native_tools_tokens,
        budget.system_prompt_tokens,
        budget.whole_turn_tokens
    );
}

#[test]
fn lean_profile_toml_parses_as_config() {
    let tmp = TempDir::new().unwrap();
    let config = lean_config(&tmp);
    assert!(
        config.mcp.deferred_loading,
        "deferred_loading is global [mcp]"
    );
    let risk = config
        .risk_profiles
        .get("hyperion_lean")
        .expect("risk profile");
    let allowed = risk
        .allowed_tools
        .as_ref()
        .expect("allowed_tools must be set");
    assert!(
        allowed.iter().any(|n| n == "hapi-edge__snapshot"),
        "ExplicitOnly requires named facades: {allowed:?}"
    );
    assert!(
        allowed.iter().any(|n| n == "hapi-memory__hapi_memory"),
        "memory facade must be named: {allowed:?}"
    );
    assert!(
        !allowed.iter().any(|n| n == "hapi-memory__hapi_save"),
        "withdrawn independent memory tools must stay off the lean list"
    );
    let agent = config.agents.get("hyperion").expect("agent alias");
    assert_eq!(agent.risk_profile.as_str(), "hyperion_lean");
    assert_eq!(agent.runtime_profile.as_str(), "hyperion_lean");
    assert_eq!(
        config.effective_skills_prompt_mode("hyperion"),
        zeroclaw_config::schema::SkillsPromptInjectionMode::Compact
    );
}

#[tokio::test]
async fn provider_wire_budget_default_and_lean_no_skills() {
    // Default counts are informational. Upstream adding a built-in must not
    // fail this test; the freeze lives on the worst-case lean assembly.
    let default_tmp = TempDir::new().unwrap();
    seed_personality(default_tmp.path(), true);
    let default_config = Config {
        config_path: default_tmp.path().join("config.toml"),
        data_dir: default_tmp.path().join("data"),
        ..Config::default()
    };
    let (_, default_budget) = assemble_turn(TurnRequest {
        config: &default_config,
        agent_alias: "default",
        skills: &[],
        connect_mcp: false,
        inject_memory: true,
        workspace: default_tmp.path(),
    })
    .await;
    print_budget("default (informational)", &default_budget);

    let lean_tmp = TempDir::new().unwrap();
    let lean = lean_config(&lean_tmp);
    let workspace = lean.agent_workspace_dir("hyperion");
    seed_personality(&workspace, true);
    let (_, lean_budget) = assemble_turn(TurnRequest {
        config: &lean,
        agent_alias: "hyperion",
        skills: &[],
        connect_mcp: false,
        inject_memory: false,
        workspace: &workspace,
    })
    .await;
    print_budget("hyperion_lean", &lean_budget);

    assert!(
        default_budget.names.len() >= 35,
        // Lowered from 40 when calculator and weather retired (the knowledge
        // tool was config-gated off and never counted); the guard only needs
        // to stay far above the default_tools(6) failure mode, and the
        // retirement campaign may shave a few more built-ins.
        "production path is all_tools_with_runtime, not default_tools(6); got {:?}",
        default_budget.names
    );
    assert!(
        default_budget.names.iter().any(|n| n == "cron_add"),
        "default registry must still carry the fat built-ins: {:?}",
        default_budget.names
    );
    assert!(
        !lean_budget.names.iter().any(|n| n == "cron_add"),
        "lean allow-list must drop cron_add: {:?}",
        lean_budget.names
    );
    for required in [
        "shell",
        "file_read",
        "file_write",
        "file_edit",
        "glob_search",
        "content_search",
        "memory_recall",
        "memory_store",
        "web_search_tool",
        "web_fetch",
        "read_skill",
    ] {
        assert!(
            lean_budget.names.iter().any(|n| n == required),
            "lean registry missing {required}: {:?}",
            lean_budget.names
        );
    }
    assert!(
        lean_budget.native_tools_tokens < default_budget.native_tools_tokens,
        "lean native tools {} should be below default {}",
        lean_budget.native_tools_tokens,
        default_budget.native_tools_tokens
    );
}

#[tokio::test]
async fn lean_compact_skills_omit_skill_bodies() {
    let tmp = TempDir::new().unwrap();
    let mut config = lean_config(&tmp);
    attach_copied_skill_fixture(&mut config);
    let workspace = config.agent_workspace_dir("hyperion");
    seed_personality(&workspace, false);
    let skills = crate::skills::load_skills_for_agent_from_config(&config, "hyperion");
    assert!(
        skills.iter().any(|s| s.name == "zeroclaw"),
        "expected copied SKILL.md fixture to load zeroclaw, got {:?}",
        skills.iter().map(|s| s.name.as_str()).collect::<Vec<_>>()
    );
    let (_, budget) = assemble_turn(TurnRequest {
        config: &config,
        agent_alias: "hyperion",
        skills: &skills,
        connect_mcp: false,
        inject_memory: false,
        workspace: &workspace,
    })
    .await;
    print_budget("hyperion_lean+copied_skills", &budget);
    assert!(
        budget.system_prompt.contains("zeroclaw"),
        "compact mode still lists skill name"
    );
    assert!(
        !budget.system_prompt.contains(SKILL_BODY_PHRASE),
        "compact mode must not inline SKILL.md body"
    );
}

#[tokio::test]
async fn lean_explicit_only_mcp_filters_unlisted_and_needs_named_facades() {
    let server = mock_mcp_http_server(&[
        ("snapshot", "read a quote"),
        ("secret_write", "must never reach the model"),
    ])
    .await;

    let listed_tmp = TempDir::new().unwrap();
    let mut listed = lean_config(&listed_tmp);
    point_hapi_edge_at(&mut listed, server.uri());
    let workspace = listed.agent_workspace_dir("hyperion");
    seed_personality(&workspace, false);
    let (assembled, budget) = assemble_turn(TurnRequest {
        config: &listed,
        agent_alias: "hyperion",
        skills: &[],
        connect_mcp: true,
        inject_memory: false,
        workspace: &workspace,
    })
    .await;
    print_budget("hyperion_lean+explicit_mcp", &budget);
    let deferred = assembled.combined_mcp_prompt_section();
    assert!(
        budget.tool_search_present,
        "named facades must keep tool_search registered: {:?}",
        budget.names
    );
    assert!(
        deferred.contains("hapi-edge__snapshot"),
        "listed facade must appear in the deferred section: {deferred}"
    );
    assert!(
        !deferred.contains("secret_write"),
        "unlisted MCP tool must not appear: {deferred}"
    );
    assert!(
        !budget.names.iter().any(|n| n.contains("secret_write")),
        "unlisted MCP tool must not enter the registry: {:?}",
        budget.names
    );

    let empty_tmp = TempDir::new().unwrap();
    let mut empty = listed.clone();
    pin_install(&mut empty, &empty_tmp);
    let empty_workspace = empty.agent_workspace_dir("hyperion");
    seed_personality(&empty_workspace, false);
    if let Some(risk) = empty.risk_profiles.get_mut("hyperion_lean") {
        risk.allowed_tools = Some(vec![
            "shell".into(),
            "file_read".into(),
            "file_write".into(),
            "file_edit".into(),
            "glob_search".into(),
            "content_search".into(),
            "memory_recall".into(),
            "memory_store".into(),
            "web_search_tool".into(),
            "web_fetch".into(),
            "read_skill".into(),
        ]);
    }
    let (_, empty_budget) = assemble_turn(TurnRequest {
        config: &empty,
        agent_alias: "hyperion",
        skills: &[],
        connect_mcp: true,
        inject_memory: false,
        workspace: &empty_workspace,
    })
    .await;
    print_budget("hyperion_lean+mcp_no_facades", &empty_budget);
    assert!(
        !empty_budget.tool_search_present,
        "ExplicitOnly with no named facades must not register tool_search: {:?}",
        empty_budget.names
    );
}

#[tokio::test]
async fn lean_wechat_path_skips_memory_md() {
    let tmp = TempDir::new().unwrap();
    let config = lean_config(&tmp);
    let workspace = config.agent_workspace_dir("hyperion");
    seed_personality(&workspace, true);
    let (_, wechat) = assemble_turn(TurnRequest {
        config: &config,
        agent_alias: "hyperion",
        skills: &[],
        connect_mcp: false,
        inject_memory: false,
        workspace: &workspace,
    })
    .await;
    let (_, interactive) = assemble_turn(TurnRequest {
        config: &config,
        agent_alias: "hyperion",
        skills: &[],
        connect_mcp: false,
        inject_memory: true,
        workspace: &workspace,
    })
    .await;
    print_budget("hyperion_lean wechat inject_memory=false", &wechat);
    print_budget("hyperion_lean interactive inject_memory=true", &interactive);
    assert!(
        !wechat.system_prompt.contains(MEMORY_MARKER),
        "WeChat process_message path must omit MEMORY.md"
    );
    assert!(
        interactive.system_prompt.contains(MEMORY_MARKER),
        "interactive inject_memory=true must include MEMORY.md"
    );
}

#[tokio::test]
async fn lean_worst_case_skills_and_mcp_under_ceiling() {
    let server = mock_mcp_http_server(&[
        ("snapshot", "read a quote"),
        ("secret_write", "must never reach the model"),
    ])
    .await;
    let tmp = TempDir::new().unwrap();
    let mut config = lean_config(&tmp);
    attach_copied_skill_fixture(&mut config);
    point_hapi_edge_at(&mut config, server.uri());
    let workspace = config.agent_workspace_dir("hyperion");
    seed_personality(&workspace, true);
    let skills = crate::skills::load_skills_for_agent_from_config(&config, "hyperion");
    assert!(
        !skills.is_empty(),
        "copied SKILL.md fixture must load at least one skill"
    );
    let (assembled, budget) = assemble_turn(TurnRequest {
        config: &config,
        agent_alias: "hyperion",
        skills: &skills,
        connect_mcp: true,
        inject_memory: false,
        workspace: &workspace,
    })
    .await;
    print_budget("hyperion_lean+copied_skills+explicit_mcp (gate)", &budget);
    eprintln!(
        "loaded_skills={} registry_tools={:?} skill_tools={:?}",
        skills.len(),
        budget.names,
        skills
            .iter()
            .flat_map(|s| s.tools.iter().map(|t| format!("{}__{}", s.name, t.name)))
            .collect::<Vec<_>>()
    );
    let deferred = assembled.combined_mcp_prompt_section();
    assert!(
        budget.tool_search_present,
        "worst-case lean must keep tool_search: {:?}",
        budget.names
    );
    assert!(
        !deferred.contains("secret_write"),
        "unlisted MCP tool must not appear: {deferred}"
    );
    assert!(
        !budget.system_prompt.contains(SKILL_BODY_PHRASE),
        "compact mode must not inline SKILL.md body"
    );
    assert!(
        budget.whole_turn_tokens <= LEAN_PROVIDER_WIRE_TOKEN_CEILING,
        "worst-case lean whole-turn {} exceeded freeze {} (system {} + tools[] {} ; registry {:?})",
        budget.whole_turn_tokens,
        LEAN_PROVIDER_WIRE_TOKEN_CEILING,
        budget.system_prompt_tokens,
        budget.native_tools_tokens,
        budget.names
    );
}

/// Owner-ratified hard CI ceiling on the provider-wire `tools[]` array for
/// the `composition = "minimal"` profile. This number is a ratified budget,
/// not a tuning knob: an over-budget regression is resolved by shrinking the
/// wire surface or by recording an owner-ratified exception on the fork's
/// governance tracker, never by editing this constant.
const MINIMAL_COMPOSITION_TOOLS_WIRE_TOKEN_CEILING: usize = 5_000;

#[tokio::test]
async fn minimal_composition_tools_wire_under_owner_ceiling() {
    // A fresh default install pinned to `composition = "minimal"`: every
    // tool[] entry below is what the model sees on the wire. The gate is
    // the measured `NativeChatRequest.tools` array (ceil(len/4) over the
    // whole serialized array), assembled through the real production path
    // (`all_tools_with_runtime` + `ScopedToolRegistry::assemble` +
    // `build_iteration_tool_specs` + `chat_tools_wire`); every stage
    // `.expect`s, so a failure to measure fails this test rather than
    // skipping the gate.
    let tmp = TempDir::new().unwrap();
    seed_personality(tmp.path(), true);
    let mut config = Config {
        config_path: tmp.path().join("config.toml"),
        data_dir: tmp.path().join("data"),
        ..Config::default()
    };
    config.composition = Some(zeroclaw_config::composition::Composition::Minimal);
    let (_, budget) = assemble_turn(TurnRequest {
        config: &config,
        agent_alias: "default",
        skills: &[],
        connect_mcp: false,
        inject_memory: true,
        workspace: tmp.path(),
    })
    .await;
    print_budget("composition=minimal default install (gate)", &budget);

    // Fail-closed measurement: the wire rows must cover every assembled
    // registry tool, otherwise the token count describes an array this test
    // never fully measured.
    assert!(
        !budget.names.is_empty(),
        "minimal assembly must register tools; empty registry means the fixture measured nothing"
    );
    assert_eq!(
        budget.rows.len(),
        budget.names.len(),
        "wire rows must match the assembled registry (rows={} names={:?}); a mismatch means the measurement dropped entries",
        budget.rows.len(),
        budget.names
    );

    // The membership table is the canonical allowlist: nothing outside it
    // may appear on the wire under minimal (the banned-category tripwire on
    // the table itself lives with the table in the config crate).
    for name in &budget.names {
        assert!(
            zeroclaw_config::composition::is_minimal_member(name),
            "non-member reached the minimal wire surface: {name}"
        );
    }

    // Measured report: tool count, system-prompt tokens, largest individual
    // schema, and total pre-history tokens (system prompt + tools[]).
    let largest = budget
        .rows
        .first()
        .expect("rows are non-empty; largest schema must be reportable");
    eprintln!(
        "minimal profile report: tools={} tools_array_tok={} system_prompt_tok={} largest_schema={} ({} tok) pre_history_tok={}",
        budget.names.len(),
        budget.native_tools_tokens,
        budget.system_prompt_tokens,
        largest.name,
        largest.native_tokens,
        budget.whole_turn_tokens
    );

    assert!(
        budget.native_tools_tokens <= MINIMAL_COMPOSITION_TOOLS_WIRE_TOKEN_CEILING,
        "minimal composition tools[] {} exceeded owner ceiling {} (tools={:?} largest={} at {} tok)",
        budget.native_tools_tokens,
        MINIMAL_COMPOSITION_TOOLS_WIRE_TOKEN_CEILING,
        budget.names,
        largest.name,
        largest.native_tokens
    );
}

#[tokio::test]
async fn minimal_composition_no_bypass_subsystem_flags() {
    let tmp = TempDir::new().unwrap();
    seed_personality(tmp.path(), true);
    let mut config = Config {
        config_path: tmp.path().join("config.toml"),
        data_dir: tmp.path().join("data"),
        ..Config::default()
    };
    // Explicitly configure and enable non-minimal subsystems with valid credentials
    config.pipeline.enabled = true;
    config.pipeline.allowed_tools = vec!["shell".to_string(), "file_read".to_string()];
    config.browser.enabled = true;
    config.http_request.enabled = true;
    config.web_search.enabled = true;

    // Positive control: under full composition, EVERY enabled non-minimal subsystem tool is registered
    config.composition = Some(zeroclaw_config::composition::Composition::Full);
    let (_, full_budget) = assemble_turn(TurnRequest {
        config: &config,
        agent_alias: "default",
        skills: &[],
        connect_mcp: false,
        inject_memory: true,
        workspace: tmp.path(),
    })
    .await;
    assert!(
        full_budget.names.contains(&"execute_pipeline".to_string()),
        "execute_pipeline must be registered under full composition: {:?}",
        full_budget.names
    );
    assert!(
        full_budget.names.contains(&"browser".to_string()),
        "browser must be registered under full composition: {:?}",
        full_budget.names
    );
    assert!(
        full_budget.names.contains(&"browser_open".to_string()),
        "browser_open must be registered under full composition: {:?}",
        full_budget.names
    );
    assert!(
        full_budget.names.contains(&"http_request".to_string()),
        "http_request must be registered under full composition: {:?}",
        full_budget.names
    );

    // Negative assertion: under minimal composition, all non-minimal subsystems are excluded
    config.composition = Some(zeroclaw_config::composition::Composition::Minimal);
    let (_, budget) = assemble_turn(TurnRequest {
        config: &config,
        agent_alias: "default",
        skills: &[],
        connect_mcp: false,
        inject_memory: true,
        workspace: tmp.path(),
    })
    .await;
    print_budget(
        "composition=minimal subsystem overrides (no-bypass test)",
        &budget,
    );

    // 1. Every assembled tool MUST be an explicit minimal member
    for name in &budget.names {
        assert!(
            zeroclaw_config::composition::is_minimal_member(name),
            "non-member tool penetrated minimal composition: {name}"
        );
    }

    // 2. None of the banned/demoted categories can enter the minimal wire surface
    for banned in [
        "claude_code",
        "codex_cli",
        "git_operations",
        "git_forge",
        "model_routing_config",
        "backup",
        "composio",
        "cron_add",
        "cron_update",
        "browser",
        "browser_open",
        "http_request",
        "pipeline",
        "execute_pipeline",
    ] {
        assert!(
            !budget.names.iter().any(|n| n == banned),
            "banned/excluded tool `{banned}` bypassed minimal composition into registry: {:?}",
            budget.names
        );
    }
}

#[tokio::test]
async fn skill_builtin_elevation_cannot_bypass_minimal_composition() {
    let tmp = TempDir::new().unwrap();
    seed_personality(tmp.path(), true);
    let mut config = Config {
        config_path: tmp.path().join("config.toml"),
        data_dir: tmp.path().join("data"),
        ..Config::default()
    };
    config.http_request.enabled = true;

    // Create a skill attempting to elevate non-minimal built-in tools (cron_add / http_request)
    let bypass_skill = Skill {
        name: "malicious_skill".into(),
        description: "attempting bypass".into(),
        description_localizations: Default::default(),
        version: "1.0.0".into(),
        author: None,
        tags: vec![],
        tools: vec![
            crate::skills::SkillTool {
                name: "elevate_cron".into(),
                description: "cron bypass".into(),
                kind: "builtin".into(),
                command: String::new(),
                args: Default::default(),
                target: Some("cron_add".into()),
                locked_args: Default::default(),
                timeout_secs: None,
            },
            crate::skills::SkillTool {
                name: "elevate_http".into(),
                description: "http_request bypass".into(),
                kind: "builtin".into(),
                command: String::new(),
                args: Default::default(),
                target: Some("http_request".into()),
                locked_args: Default::default(),
                timeout_secs: None,
            },
        ],
        prompts: vec![],
        slash_options: vec![],
        location: None,
    };

    // Positive control: under full composition, skill elevation of available built-in tools succeeds for both cron_add and http_request
    config.composition = Some(zeroclaw_config::composition::Composition::Full);
    let (_, full_budget) = assemble_turn(TurnRequest {
        config: &config,
        agent_alias: "default",
        skills: std::slice::from_ref(&bypass_skill),
        connect_mcp: false,
        inject_memory: true,
        workspace: tmp.path(),
    })
    .await;
    assert!(
        full_budget
            .names
            .contains(&"malicious_skill__elevate_cron".to_string()),
        "full composition must permit skill elevation of available cron_add tool: {:?}",
        full_budget.names
    );
    assert!(
        full_budget
            .names
            .contains(&"malicious_skill__elevate_http".to_string()),
        "full composition must permit skill elevation of available http_request tool: {:?}",
        full_budget.names
    );

    // Negative assertion: under minimal composition, non-minimal built-ins are never in resolution registry
    config.composition = Some(zeroclaw_config::composition::Composition::Minimal);
    let (_, budget) = assemble_turn(TurnRequest {
        config: &config,
        agent_alias: "default",
        skills: &[bypass_skill],
        connect_mcp: false,
        inject_memory: true,
        workspace: tmp.path(),
    })
    .await;

    assert!(
        !budget
            .names
            .contains(&"malicious_skill__elevate_cron".to_string()),
        "skill elevation must not resurrect excluded non-minimal tool: {:?}",
        budget.names
    );
    assert!(
        !budget
            .names
            .contains(&"malicious_skill__elevate_http".to_string()),
        "skill elevation must not resurrect excluded non-minimal tool: {:?}",
        budget.names
    );
}

#[tokio::test]
async fn mcp_explicit_only_eager_drops_unlisted_tools_in_minimal_composition() {
    let server = mock_mcp_http_server(&[
        ("snapshot", "allowed trading tool"),
        ("git_operations", "unlisted repo tool attempt"),
        ("jira_write", "unlisted SaaS tool attempt"),
    ])
    .await;

    let tmp = TempDir::new().unwrap();
    seed_personality(tmp.path(), true);
    let mut config = Config {
        config_path: tmp.path().join("config.toml"),
        data_dir: tmp.path().join("data"),
        ..Config::default()
    };
    config.composition = Some(zeroclaw_config::composition::Composition::Minimal);
    config.mcp.enabled = true;
    config.mcp.deferred_loading = false;
    let risk_profile = zeroclaw_config::schema::RiskProfileConfig {
        mcp_discovered_tool_policy:
            zeroclaw_config::autonomy::McpDiscoveredToolPolicy::ExplicitOnly,
        allowed_tools: Some(vec![
            "shell".into(),
            "file_read".into(),
            "file_write".into(),
            "hapi-edge__snapshot".into(),
        ]),
        ..Default::default()
    };
    config.risk_profiles.insert("lean".into(), risk_profile);
    let agent_cfg = zeroclaw_config::schema::AliasedAgentConfig {
        risk_profile: "lean".into(),
        ..Default::default()
    };
    config.agents.insert("lean".into(), agent_cfg);
    point_hapi_edge_at_agent(&mut config, server.uri(), "lean");

    let (_, budget) = assemble_turn(TurnRequest {
        config: &config,
        agent_alias: "lean",
        skills: &[],
        connect_mcp: true,
        inject_memory: true,
        workspace: tmp.path(),
    })
    .await;
    print_budget("composition=minimal + eager explicit_only mcp", &budget);

    // Snapshot is explicitly listed, so it is admitted on the wire
    assert!(budget.names.contains(&"hapi-edge__snapshot".to_string()));

    // Unlisted MCP tools MUST be dropped from eager registration
    assert!(!budget.names.iter().any(|n| n.contains("git_operations")));
    assert!(!budget.names.iter().any(|n| n.contains("jira_write")));
}

#[tokio::test]
async fn mcp_explicit_only_deferred_drops_unlisted_tools_in_minimal_composition() {
    let server = mock_mcp_http_server(&[
        ("snapshot", "allowed trading tool"),
        ("git_operations", "unlisted repo tool attempt"),
        ("jira_write", "unlisted SaaS tool attempt"),
    ])
    .await;

    let tmp = TempDir::new().unwrap();
    seed_personality(tmp.path(), true);
    let mut config = Config {
        config_path: tmp.path().join("config.toml"),
        data_dir: tmp.path().join("data"),
        ..Config::default()
    };
    config.composition = Some(zeroclaw_config::composition::Composition::Minimal);
    config.mcp.enabled = true;
    config.mcp.deferred_loading = true;
    let risk_profile = zeroclaw_config::schema::RiskProfileConfig {
        mcp_discovered_tool_policy:
            zeroclaw_config::autonomy::McpDiscoveredToolPolicy::ExplicitOnly,
        allowed_tools: Some(vec![
            "shell".into(),
            "file_read".into(),
            "file_write".into(),
            "hapi-edge__snapshot".into(),
        ]),
        ..Default::default()
    };
    config.risk_profiles.insert("lean".into(), risk_profile);
    let agent_cfg = zeroclaw_config::schema::AliasedAgentConfig {
        risk_profile: "lean".into(),
        ..Default::default()
    };
    config.agents.insert("lean".into(), agent_cfg);
    point_hapi_edge_at_agent(&mut config, server.uri(), "lean");

    let (assembled, budget) = assemble_turn(TurnRequest {
        config: &config,
        agent_alias: "lean",
        skills: &[],
        connect_mcp: true,
        inject_memory: true,
        workspace: tmp.path(),
    })
    .await;
    print_budget("composition=minimal + deferred explicit_only mcp", &budget);

    // 1. Under deferred loading, tool_search is registered instead of eager snapshot
    assert!(budget.names.contains(&"tool_search".to_string()));
    assert!(!budget.names.contains(&"hapi-edge__snapshot".to_string()));

    // 2. Deferred prompt section advertises allowed tool and strictly omits unlisted tools
    let deferred = assembled.combined_mcp_prompt_section();
    assert!(deferred.contains("hapi-edge__snapshot"));
    assert!(!deferred.contains("git_operations"));
    assert!(!deferred.contains("jira_write"));

    // 3. Search and activation boundaries: allowed tool resolves, unlisted tools return not found
    let tool_search = assembled
        .registry
        .iter()
        .find(|t| t.name() == "tool_search")
        .expect("tool_search must be assembled under deferred loading with admitted tools");

    let allowed_res = tool_search
        .execute(serde_json::json!({"query": "select:hapi-edge__snapshot"}))
        .await
        .expect("tool_search must execute");
    assert!(
        allowed_res
            .output
            .contains("\"name\": \"hapi-edge__snapshot\""),
        "allowed tool must resolve in tool_search: {}",
        allowed_res.output
    );

    let unlisted_git_res = tool_search
        .execute(serde_json::json!({"query": "select:hapi-edge__git_operations"}))
        .await
        .expect("tool_search must execute");
    assert!(
        !unlisted_git_res
            .output
            .contains("\"name\": \"hapi-edge__git_operations\""),
        "unlisted tool must not resolve in tool_search: {}",
        unlisted_git_res.output
    );
    assert!(
        unlisted_git_res
            .output
            .contains("Not found: hapi-edge__git_operations"),
        "unlisted tool must report not found: {}",
        unlisted_git_res.output
    );

    let unlisted_jira_res = tool_search
        .execute(serde_json::json!({"query": "select:hapi-edge__jira_write"}))
        .await
        .expect("tool_search must execute");
    assert!(
        !unlisted_jira_res
            .output
            .contains("\"name\": \"hapi-edge__jira_write\""),
        "unlisted tool must not resolve in tool_search: {}",
        unlisted_jira_res.output
    );
    assert!(
        unlisted_jira_res
            .output
            .contains("Not found: hapi-edge__jira_write"),
        "unlisted tool must report not found: {}",
        unlisted_jira_res.output
    );

    // 4. Activation set verification
    let activated_handle = assembled
        .activated_handle
        .expect("tool_search registers the activation handle");
    let guard = activated_handle.lock().unwrap();
    assert!(
        guard.is_activated("hapi-edge__snapshot"),
        "allowed tool must be activated"
    );
    assert!(
        !guard.is_activated("hapi-edge__git_operations"),
        "unlisted git tool must not be activated"
    );
    assert!(
        !guard.is_activated("hapi-edge__jira_write"),
        "unlisted jira tool must not be activated"
    );
}
