use crate::approval::ApprovalManager;
use crate::config::Config;
use crate::memory::{self, Memory, MemoryCategory, decay};
use crate::observability::{self, Observer, ObserverEvent};
use crate::providers::{self, ChatMessage, Provider};
use crate::runtime;
use crate::security::{AutonomyLevel, SecurityPolicy};
use crate::tools::{self, Tool};
use anyhow::Result;
use std::fmt::Write;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::history::{
    load_interactive_session_history, save_interactive_session_history, trim_history,
};
use super::loop_::{
    DraftEvent, agent_turn, build_tool_instructions, clear_model_switch_request,
    compute_excluded_mcp_tools, get_model_switch_state, is_model_switch_requested,
    is_tool_loop_cancelled, run_tool_call_loop,
};

/// Minimum user-message length (in chars) for auto-save to memory.
/// Matches the channel-side constant in `channels/mod.rs`.
const AUTOSAVE_MIN_MESSAGE_CHARS: usize = 20;

/// Convert a tool registry to OpenAI function-calling format for native tool support.
pub(crate) fn tools_to_openai_format(tools_registry: &[Box<dyn Tool>]) -> Vec<serde_json::Value> {
    tools_registry
        .iter()
        .map(|tool| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": tool.name(),
                    "description": tool.description(),
                    "parameters": tool.parameters_schema()
                }
            })
        })
        .collect()
}

pub(crate) fn autosave_memory_key(prefix: &str) -> String {
    format!("{prefix}_{}", Uuid::new_v4())
}

/// Build context preamble by searching memory for relevant entries.
/// Entries with a hybrid score below `min_relevance_score` are dropped to
/// prevent unrelated memories from bleeding into the conversation.
/// Core memories are exempt from time decay (evergreen).
pub(crate) async fn build_context(
    mem: &dyn Memory,
    user_msg: &str,
    min_relevance_score: f64,
    session_id: Option<&str>,
) -> String {
    let mut context = String::new();

    // Pull relevant memories for this message
    if let Ok(mut entries) = mem.recall(user_msg, 5, session_id, None, None).await {
        // Apply time decay: older non-Core memories score lower
        decay::apply_time_decay(&mut entries, decay::DEFAULT_HALF_LIFE_DAYS);

        let relevant: Vec<_> = entries
            .iter()
            .filter(|e| match e.score {
                Some(score) => score >= min_relevance_score,
                None => true,
            })
            .collect();

        if !relevant.is_empty() {
            context.push_str("[Memory context]\n");
            for entry in &relevant {
                if memory::is_assistant_autosave_key(&entry.key) {
                    continue;
                }
                if memory::should_skip_autosave_content(&entry.content) {
                    continue;
                }
                // Skip entries containing tool_result blocks — they can leak
                // stale tool output from previous heartbeat ticks into new
                // sessions, presenting the LLM with orphan tool_result data.
                if entry.content.contains("<tool_result") {
                    continue;
                }
                let _ = writeln!(context, "- {}: {}", entry.key, entry.content);
            }
            if context == "[Memory context]\n" {
                context.clear();
            } else {
                context.push_str("[/Memory context]\n\n");
            }
        }
    }

    context
}

/// Build hardware datasheet context from RAG when peripherals are enabled.
/// Includes pin-alias lookup (e.g. "red_led" → 13) when query matches, plus retrieved chunks.
fn build_hardware_context(
    rag: &crate::rag::HardwareRag,
    user_msg: &str,
    boards: &[String],
    chunk_limit: usize,
) -> String {
    if rag.is_empty() || boards.is_empty() {
        return String::new();
    }

    let mut context = String::new();

    // Pin aliases: when user says "red led", inject "red_led: 13" for matching boards
    let pin_ctx = rag.pin_alias_context(user_msg, boards);
    if !pin_ctx.is_empty() {
        context.push_str(&pin_ctx);
    }

    let chunks = rag.retrieve(user_msg, boards, chunk_limit);
    if chunks.is_empty() && pin_ctx.is_empty() {
        return String::new();
    }

    if !chunks.is_empty() {
        context.push_str("[Hardware documentation]\n");
    }
    for chunk in chunks {
        let board_tag = chunk.board.as_deref().unwrap_or("generic");
        let _ = writeln!(
            context,
            "--- {} ({}) ---\n{}\n",
            chunk.source, board_tag, chunk.content
        );
    }
    context.push('\n');
    context
}

// ── CLI Entrypoint ───────────────────────────────────────────────────────
// Wires up all subsystems (observer, runtime, security, memory, tools,
// provider, hardware RAG, peripherals) and enters either single-shot or
// interactive REPL mode. The interactive loop manages history compaction
// and hard trimming to keep the context window bounded.

#[allow(clippy::too_many_lines)]
pub async fn run(
    config: Config,
    message: Option<String>,
    provider_override: Option<String>,
    model_override: Option<String>,
    temperature: f64,
    peripheral_overrides: Vec<String>,
    interactive: bool,
    session_state_file: Option<PathBuf>,
    allowed_tools: Option<Vec<String>>,
) -> Result<String> {
    // ── Wire up agnostic subsystems ──────────────────────────────
    let base_observer = observability::create_observer(&config.observability);
    let observer: Arc<dyn Observer> = Arc::from(base_observer);
    let runtime: Arc<dyn runtime::RuntimeAdapter> =
        Arc::from(runtime::create_runtime(&config.runtime)?);
    let security = Arc::new(SecurityPolicy::from_config(
        &config.autonomy,
        &config.workspace_dir,
    ));

    // ── Memory (the brain) ────────────────────────────────────────
    let mem: Arc<dyn Memory> = Arc::from(memory::create_memory_with_storage_and_routes(
        &config.memory,
        &config.embedding_routes,
        Some(&config.storage.provider.config),
        &config.workspace_dir,
        config.api_key.as_deref(),
    )?);
    tracing::info!(backend = mem.name(), "Memory initialized");

    // ── Peripherals (merge peripheral tools into registry) ─
    if !peripheral_overrides.is_empty() {
        tracing::info!(
            peripherals = ?peripheral_overrides,
            "Peripheral overrides from CLI (config boards take precedence)"
        );
    }

    // ── Tools (including memory tools and peripherals) ────────────
    let (composio_key, composio_entity_id) = if config.composio.enabled {
        (
            config.composio.api_key.as_deref(),
            Some(config.composio.entity_id.as_str()),
        )
    } else {
        (None, None)
    };
    let (
        mut tools_registry,
        delegate_handle,
        _reaction_handle,
        _channel_map_handle,
        _ask_user_handle,
        _escalate_handle,
    ) = tools::all_tools_with_runtime(
        Arc::new(config.clone()),
        &security,
        runtime,
        mem.clone(),
        composio_key,
        composio_entity_id,
        &config.browser,
        &config.http_request,
        &config.web_fetch,
        &config.workspace_dir,
        &config.agents,
        config.api_key.as_deref(),
        &config,
        None,
    );

    let peripheral_tools: Vec<Box<dyn Tool>> =
        crate::peripherals::create_peripheral_tools(&config.peripherals).await?;
    if !peripheral_tools.is_empty() {
        tracing::info!(count = peripheral_tools.len(), "Peripheral tools added");
        tools_registry.extend(peripheral_tools);
    }

    // ── Capability-based tool access control ─────────────────────
    // When `allowed_tools` is `Some(list)`, restrict the tool registry to only
    // those tools whose name appears in the list. Unknown names are silently
    // ignored. When `None`, all tools remain available (backward compatible).
    if let Some(ref allow_list) = allowed_tools {
        tools_registry.retain(|t| allow_list.iter().any(|name| name == t.name()));
        tracing::info!(
            allowed = allow_list.len(),
            retained = tools_registry.len(),
            "Applied capability-based tool access filter"
        );
    }

    // ── Wire MCP tools (non-fatal) — CLI path ────────────────────
    // NOTE: MCP tools are injected after built-in tool filtering
    // (filter_primary_agent_tools_or_fail / agent.allowed_tools / agent.denied_tools).
    // MCP servers are user-declared external integrations; the built-in allow/deny
    // filter is not appropriate for them and would silently drop all MCP tools when
    // a restrictive allowlist is configured. Keep this block after any such filter call.
    //
    // When `deferred_loading` is enabled, MCP tools are NOT added to the registry
    // eagerly. Instead, a `tool_search` built-in is registered so the LLM can
    // fetch schemas on demand. This reduces context window waste.
    let mut deferred_section = String::new();
    let mut activated_handle: Option<
        std::sync::Arc<std::sync::Mutex<crate::tools::ActivatedToolSet>>,
    > = None;
    if config.mcp.enabled && !config.mcp.servers.is_empty() {
        tracing::info!(
            "Initializing MCP client — {} server(s) configured",
            config.mcp.servers.len()
        );
        match crate::tools::McpRegistry::connect_all(&config.mcp.servers).await {
            Ok(registry) => {
                let registry = std::sync::Arc::new(registry);
                if config.mcp.deferred_loading {
                    // Deferred path: build stubs and register tool_search
                    let deferred_set = crate::tools::DeferredMcpToolSet::from_registry(
                        std::sync::Arc::clone(&registry),
                    )
                    .await;
                    tracing::info!(
                        "MCP deferred: {} tool stub(s) from {} server(s)",
                        deferred_set.len(),
                        registry.server_count()
                    );
                    deferred_section =
                        crate::tools::mcp_deferred::build_deferred_tools_section(&deferred_set);
                    let activated = std::sync::Arc::new(std::sync::Mutex::new(
                        crate::tools::ActivatedToolSet::new(),
                    ));
                    activated_handle = Some(std::sync::Arc::clone(&activated));
                    tools_registry.push(Box::new(crate::tools::ToolSearchTool::new(
                        deferred_set,
                        activated,
                    )));
                } else {
                    // Eager path: register all MCP tools directly
                    let names = registry.tool_names();
                    let mut registered = 0usize;
                    for name in names {
                        if let Some(def) = registry.get_tool_def(&name).await {
                            let wrapper: std::sync::Arc<dyn Tool> =
                                std::sync::Arc::new(crate::tools::McpToolWrapper::new(
                                    name,
                                    def,
                                    std::sync::Arc::clone(&registry),
                                ));
                            if let Some(ref handle) = delegate_handle {
                                handle.write().push(std::sync::Arc::clone(&wrapper));
                            }
                            tools_registry.push(Box::new(crate::tools::ArcToolRef(wrapper)));
                            registered += 1;
                        }
                    }
                    tracing::info!(
                        "MCP: {} tool(s) registered from {} server(s)",
                        registered,
                        registry.server_count()
                    );
                }
            }
            Err(e) => {
                tracing::error!("MCP registry failed to initialize: {e:#}");
            }
        }
    }

    // ── Resolve provider ─────────────────────────────────────────
    let mut provider_name = provider_override
        .as_deref()
        .or(config.default_provider.as_deref())
        .unwrap_or("openrouter")
        .to_string();

    let mut model_name = model_override
        .as_deref()
        .or(config.default_model.as_deref())
        .unwrap_or("anthropic/claude-sonnet-4")
        .to_string();

    let provider_runtime_options = providers::provider_runtime_options_from_config(&config);

    let mut provider: Box<dyn Provider> = providers::create_routed_provider_with_options(
        &provider_name,
        config.api_key.as_deref(),
        config.api_url.as_deref(),
        &config.reliability,
        &config.model_routes,
        &model_name,
        &provider_runtime_options,
    )?;

    let model_switch_callback = get_model_switch_state();

    observer.record_event(&ObserverEvent::AgentStart {
        provider: provider_name.to_string(),
        model: model_name.to_string(),
    });

    // ── Hardware RAG (datasheet retrieval when peripherals + datasheet_dir) ──
    let hardware_rag: Option<crate::rag::HardwareRag> = config
        .peripherals
        .datasheet_dir
        .as_ref()
        .filter(|d| !d.trim().is_empty())
        .map(|dir| crate::rag::HardwareRag::load(&config.workspace_dir, dir.trim()))
        .and_then(Result::ok)
        .filter(|r: &crate::rag::HardwareRag| !r.is_empty());
    if let Some(ref rag) = hardware_rag {
        tracing::info!(chunks = rag.len(), "Hardware RAG loaded");
    }

    let board_names: Vec<String> = config
        .peripherals
        .boards
        .iter()
        .map(|b| b.board.clone())
        .collect();

    // ── Load locale-aware tool descriptions ────────────────────────
    let i18n_locale = config
        .locale
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(crate::i18n::detect_locale);
    let i18n_search_dirs = crate::i18n::default_search_dirs(&config.workspace_dir);
    let i18n_descs = crate::i18n::ToolDescriptions::load(&i18n_locale, &i18n_search_dirs);

    // ── Build system prompt from workspace MD files (OpenClaw framework) ──
    let skills = crate::skills::load_skills_with_config(&config.workspace_dir, &config);

    // Register skill-defined tools as callable tool specs in the tool registry
    // so the LLM can invoke them via native function calling, not just XML prompts.
    tools::register_skill_tools(&mut tools_registry, &skills, security.clone());

    let mut tool_descs: Vec<(&str, &str)> = vec![
        (
            "shell",
            "Execute terminal commands. Use when: running local checks, build/test commands, diagnostics. Don't use when: a safer dedicated tool exists, or command is destructive without approval.",
        ),
        (
            "file_read",
            "Read file contents. Use when: inspecting project files, configs, logs. Don't use when: a targeted search is enough.",
        ),
        (
            "file_write",
            "Write file contents. Use when: applying focused edits, scaffolding files, updating docs/code. Don't use when: side effects are unclear or file ownership is uncertain.",
        ),
        (
            "memory_store",
            "Save to memory. Use when: preserving durable preferences, decisions, key context. Don't use when: information is transient/noisy/sensitive without need.",
        ),
        (
            "memory_recall",
            "Search memory. Use when: retrieving prior decisions, user preferences, historical context. Don't use when: answer is already in current context.",
        ),
        (
            "memory_forget",
            "Delete a memory entry. Use when: memory is incorrect/stale or explicitly requested for removal. Don't use when: impact is uncertain.",
        ),
    ];
    if matches!(
        config.skills.prompt_injection_mode,
        crate::config::SkillsPromptInjectionMode::Compact
    ) {
        tool_descs.push((
            "read_skill",
            "Load the full source for an available skill by name. Use when: compact mode only shows a summary and you need the complete skill instructions.",
        ));
    }
    tool_descs.push((
        "cron_add",
        "Create a cron job. Supports schedule kinds: cron, at, every; and job types: shell or agent.",
    ));
    tool_descs.push((
        "cron_list",
        "List all cron jobs with schedule, status, and metadata.",
    ));
    tool_descs.push(("cron_remove", "Remove a cron job by job_id."));
    tool_descs.push((
        "cron_update",
        "Patch a cron job (schedule, enabled, command/prompt, model, delivery, session_target).",
    ));
    tool_descs.push((
        "cron_run",
        "Force-run a cron job immediately and record a run history entry.",
    ));
    tool_descs.push(("cron_runs", "Show recent run history for a cron job."));
    tool_descs.push((
        "screenshot",
        "Capture a screenshot of the current screen. Returns file path and base64-encoded PNG. Use when: visual verification, UI inspection, debugging displays.",
    ));
    tool_descs.push((
        "image_info",
        "Read image file metadata (format, dimensions, size) and optionally base64-encode it. Use when: inspecting images, preparing visual data for analysis.",
    ));
    if config.browser.enabled {
        tool_descs.push((
            "browser_open",
            "Open approved HTTPS URLs in system browser (allowlist-only, no scraping)",
        ));
    }
    if config.composio.enabled {
        tool_descs.push((
            "composio",
            "Execute actions on 1000+ apps via Composio (Gmail, Notion, GitHub, Slack, etc.). Use action='list' to discover, 'execute' to run (optionally with connected_account_id), 'connect' to OAuth.",
        ));
    }
    tool_descs.push((
        "schedule",
        "Manage scheduled tasks (create/list/get/cancel/pause/resume). Supports recurring cron and one-shot delays.",
    ));
    tool_descs.push((
        "model_routing_config",
        "Configure default model, scenario routing, and delegate agents. Use for natural-language requests like: 'set conversation to kimi and coding to gpt-5.3-codex'.",
    ));
    if !config.agents.is_empty() {
        tool_descs.push((
            "delegate",
            "Delegate a sub-task to a specialized agent. Use when: task needs different model/capability, or to parallelize work.",
        ));
    }
    if config.peripherals.enabled && !config.peripherals.boards.is_empty() {
        tool_descs.push((
            "gpio_read",
            "Read GPIO pin value (0 or 1) on connected hardware (STM32, Arduino). Use when: checking sensor/button state, LED status.",
        ));
        tool_descs.push((
            "gpio_write",
            "Set GPIO pin high (1) or low (0) on connected hardware. Use when: turning LED on/off, controlling actuators.",
        ));
        tool_descs.push((
            "arduino_upload",
            "Upload agent-generated Arduino sketch. Use when: user asks for 'make a heart', 'blink pattern', or custom LED behavior on Arduino. You write the full .ino code; ZeroClaw compiles and uploads it. Pin 13 = built-in LED on Uno.",
        ));
        tool_descs.push((
            "hardware_memory_map",
            "Return flash and RAM address ranges for connected hardware. Use when: user asks for 'upper and lower memory addresses', 'memory map', or 'readable addresses'.",
        ));
        tool_descs.push((
            "hardware_board_info",
            "Return full board info (chip, architecture, memory map) for connected hardware. Use when: user asks for 'board info', 'what board do I have', 'connected hardware', 'chip info', or 'what hardware'.",
        ));
        tool_descs.push((
            "hardware_memory_read",
            "Read actual memory/register values from Nucleo via USB. Use when: user asks to 'read register values', 'read memory', 'dump lower memory 0-126', 'give address and value'. Params: address (hex, default 0x20000000), length (bytes, default 128).",
        ));
        tool_descs.push((
            "hardware_capabilities",
            "Query connected hardware for reported GPIO pins and LED pin. Use when: user asks what pins are available.",
        ));
    }
    let bootstrap_max_chars = if config.agent.compact_context {
        Some(6000)
    } else {
        None
    };
    let native_tools = provider.supports_native_tools();
    let mut system_prompt = crate::channels::build_system_prompt_with_mode_and_autonomy(
        &config.workspace_dir,
        &model_name,
        &tool_descs,
        &skills,
        Some(&config.identity),
        bootstrap_max_chars,
        Some(&config.autonomy),
        native_tools,
        config.skills.prompt_injection_mode,
        config.agent.compact_context,
        config.agent.max_system_prompt_chars,
    );

    // Append structured tool-use instructions with schemas (only for non-native providers)
    if !native_tools {
        system_prompt.push_str(&build_tool_instructions(&tools_registry, Some(&i18n_descs)));
    }

    // Append deferred MCP tool names so the LLM knows what is available
    if !deferred_section.is_empty() {
        system_prompt.push('\n');
        system_prompt.push_str(&deferred_section);
    }

    // ── Approval manager (supervised mode) ───────────────────────
    let approval_manager = if interactive {
        Some(ApprovalManager::from_config(&config.autonomy))
    } else {
        None
    };
    let channel_name = if interactive { "cli" } else { "daemon" };
    let memory_session_id = session_state_file.as_deref().and_then(|path| {
        let raw = path.to_string_lossy().trim().to_string();
        if raw.is_empty() {
            None
        } else {
            Some(format!("cli:{raw}"))
        }
    });

    // ── Execute ──────────────────────────────────────────────────
    let start = Instant::now();

    let mut final_output = String::new();

    // Save the base system prompt before any thinking modifications so
    // the interactive loop can restore it between turns.
    let base_system_prompt = system_prompt.clone();

    if let Some(msg) = message {
        // ── Parse thinking directive from user message ─────────
        let (thinking_directive, effective_msg) =
            match crate::agent::thinking::parse_thinking_directive(&msg) {
                Some((level, remaining)) => {
                    tracing::info!(thinking_level = ?level, "Thinking directive parsed from message");
                    (Some(level), remaining)
                }
                None => (None, msg.clone()),
            };
        let thinking_level = crate::agent::thinking::resolve_thinking_level(
            thinking_directive,
            None,
            &config.agent.thinking,
        );
        let thinking_params = crate::agent::thinking::apply_thinking_level(thinking_level);
        let effective_temperature = crate::agent::thinking::clamp_temperature(
            temperature + thinking_params.temperature_adjustment,
        );

        // Prepend thinking system prompt prefix when present.
        if let Some(ref prefix) = thinking_params.system_prompt_prefix {
            system_prompt = format!("{prefix}\n\n{system_prompt}");
        }

        // Auto-save user message to memory (skip short/trivial messages)
        if config.memory.auto_save
            && effective_msg.chars().count() >= AUTOSAVE_MIN_MESSAGE_CHARS
            && !memory::should_skip_autosave_content(&effective_msg)
        {
            let user_key = autosave_memory_key("user_msg");
            let _ = mem
                .store(
                    &user_key,
                    &effective_msg,
                    MemoryCategory::Conversation,
                    memory_session_id.as_deref(),
                )
                .await;
        }

        // Inject memory + hardware RAG context into user message
        let mem_context = build_context(
            mem.as_ref(),
            &effective_msg,
            config.memory.min_relevance_score,
            memory_session_id.as_deref(),
        )
        .await;
        let rag_limit = if config.agent.compact_context { 2 } else { 5 };
        let hw_context = hardware_rag
            .as_ref()
            .map(|r| build_hardware_context(r, &effective_msg, &board_names, rag_limit))
            .unwrap_or_default();
        let context = format!("{mem_context}{hw_context}");
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S %Z");
        let enriched = if context.is_empty() {
            format!("[{now}] {effective_msg}")
        } else {
            format!("{context}[{now}] {effective_msg}")
        };

        let mut history = vec![
            ChatMessage::system(&system_prompt),
            ChatMessage::user(&enriched),
        ];

        // Prune history for token efficiency (when enabled).
        if config.agent.history_pruning.enabled {
            let _stats = crate::agent::history_pruner::prune_history(
                &mut history,
                &config.agent.history_pruning,
            );
        }

        // Compute per-turn excluded MCP tools from tool_filter_groups.
        let excluded_tools = compute_excluded_mcp_tools(
            &tools_registry,
            &config.agent.tool_filter_groups,
            &effective_msg,
        );

        #[allow(unused_assignments)]
        let mut response = String::new();
        loop {
            match run_tool_call_loop(
                provider.as_ref(),
                &mut history,
                &tools_registry,
                observer.as_ref(),
                &provider_name,
                &model_name,
                effective_temperature,
                false,
                approval_manager.as_ref(),
                channel_name,
                None,
                &config.multimodal,
                config.agent.max_tool_iterations,
                None,
                None,
                None,
                &excluded_tools,
                &config.agent.tool_call_dedup_exempt,
                activated_handle.as_ref(),
                Some(model_switch_callback.clone()),
                &config.pacing,
                config.agent.max_tool_result_chars,
                config.agent.max_context_tokens,
                None, // shared_budget
            )
            .await
            {
                Ok(resp) => {
                    response = resp;
                    break;
                }
                Err(e) => {
                    if let Some((new_provider, new_model)) = is_model_switch_requested(&e) {
                        tracing::info!(
                            "Model switch requested, switching from {} {} to {} {}",
                            provider_name,
                            model_name,
                            new_provider,
                            new_model
                        );

                        provider = providers::create_routed_provider_with_options(
                            &new_provider,
                            config.api_key.as_deref(),
                            config.api_url.as_deref(),
                            &config.reliability,
                            &config.model_routes,
                            &new_model,
                            &provider_runtime_options,
                        )?;

                        provider_name = new_provider;
                        model_name = new_model;

                        clear_model_switch_request();

                        observer.record_event(&ObserverEvent::AgentStart {
                            provider: provider_name.to_string(),
                            model: model_name.to_string(),
                        });

                        continue;
                    }
                    return Err(e);
                }
            }
        }

        // After successful multi-step execution, attempt autonomous skill creation.
        #[cfg(feature = "skill-creation")]
        if config.skills.skill_creation.enabled {
            let tool_calls = crate::skills::creator::extract_tool_calls_from_history(&history);
            if tool_calls.len() >= 2 {
                let creator = crate::skills::creator::SkillCreator::new(
                    config.workspace_dir.clone(),
                    config.skills.skill_creation.clone(),
                );
                match creator.create_from_execution(&msg, &tool_calls, None).await {
                    Ok(Some(slug)) => {
                        tracing::info!(slug, "Auto-created skill from execution");
                    }
                    Ok(None) => {
                        tracing::debug!("Skill creation skipped (duplicate or disabled)");
                    }
                    Err(e) => tracing::warn!("Skill creation failed: {e}"),
                }
            }
        }
        final_output = response.clone();
        println!("{response}");
        observer.record_event(&ObserverEvent::TurnComplete);
    } else {
        println!("🦀 ZeroClaw Interactive Mode");
        println!("Type /help for commands.\n");
        let cli = crate::channels::CliChannel::new();

        // Persistent conversation history across turns
        let mut history = if let Some(path) = session_state_file.as_deref() {
            load_interactive_session_history(path, &system_prompt)?
        } else {
            vec![ChatMessage::system(&system_prompt)]
        };

        loop {
            print!("> ");
            let _ = std::io::stdout().flush();

            // Read raw bytes to avoid UTF-8 validation errors when PTY
            // transport splits multi-byte characters at frame boundaries
            // (e.g. CJK input with spaces over kubectl exec / SSH).
            let mut raw = Vec::new();
            match std::io::BufRead::read_until(&mut std::io::stdin().lock(), b'\n', &mut raw) {
                Ok(0) => break,
                Ok(_) => {}
                Err(e) => {
                    eprintln!("\nError reading input: {e}\n");
                    break;
                }
            }
            let input = String::from_utf8_lossy(&raw).into_owned();

            let user_input = input.trim().to_string();
            if user_input.is_empty() {
                continue;
            }
            match user_input.as_str() {
                "/quit" | "/exit" => break,
                "/help" => {
                    println!("Available commands:");
                    println!("  /help             Show this help message");
                    println!("  /clear /new       Clear conversation history");
                    println!("  /quit /exit       Exit interactive mode");
                    println!(
                        "  /think:<level>    Set reasoning depth (off|minimal|low|medium|high|max)\n"
                    );
                    continue;
                }
                "/clear" | "/new" => {
                    println!(
                        "This will clear the current conversation and delete all session memory."
                    );
                    println!("Core memories (long-term facts/preferences) will be preserved.");
                    print!("Continue? [y/N] ");
                    let _ = std::io::stdout().flush();

                    let mut confirm_raw = Vec::new();
                    if std::io::BufRead::read_until(
                        &mut std::io::stdin().lock(),
                        b'\n',
                        &mut confirm_raw,
                    )
                    .is_err()
                    {
                        continue;
                    }
                    let confirm = String::from_utf8_lossy(&confirm_raw);
                    if !matches!(confirm.trim().to_lowercase().as_str(), "y" | "yes") {
                        println!("Cancelled.\n");
                        continue;
                    }

                    history.clear();
                    history.push(ChatMessage::system(&system_prompt));
                    // Clear conversation and daily memory
                    let mut cleared = 0;
                    for category in [MemoryCategory::Conversation, MemoryCategory::Daily] {
                        let entries = mem.list(Some(&category), None).await.unwrap_or_default();
                        for entry in entries {
                            if mem.forget(&entry.key).await.unwrap_or(false) {
                                cleared += 1;
                            }
                        }
                    }
                    if cleared > 0 {
                        println!("Conversation cleared ({cleared} memory entries removed).\n");
                    } else {
                        println!("Conversation cleared.\n");
                    }
                    if let Some(path) = session_state_file.as_deref() {
                        save_interactive_session_history(path, &history)?;
                    }
                    continue;
                }
                _ => {}
            }

            // ── Parse thinking directive from interactive input ───
            let (thinking_directive, effective_input) =
                match crate::agent::thinking::parse_thinking_directive(&user_input) {
                    Some((level, remaining)) => {
                        tracing::info!(thinking_level = ?level, "Thinking directive parsed");
                        (Some(level), remaining)
                    }
                    None => (None, user_input.clone()),
                };
            let thinking_level = crate::agent::thinking::resolve_thinking_level(
                thinking_directive,
                None,
                &config.agent.thinking,
            );
            let thinking_params = crate::agent::thinking::apply_thinking_level(thinking_level);
            let turn_temperature = crate::agent::thinking::clamp_temperature(
                temperature + thinking_params.temperature_adjustment,
            );

            // For non-Medium levels, temporarily patch the system prompt with prefix.
            let turn_system_prompt;
            if let Some(ref prefix) = thinking_params.system_prompt_prefix {
                turn_system_prompt = format!("{prefix}\n\n{system_prompt}");
                // Update the system message in history for this turn.
                if let Some(sys_msg) = history.first_mut() {
                    if sys_msg.role == "system" {
                        sys_msg.content = turn_system_prompt.clone();
                    }
                }
            }

            // Auto-save conversation turns (skip short/trivial messages)
            if config.memory.auto_save
                && effective_input.chars().count() >= AUTOSAVE_MIN_MESSAGE_CHARS
                && !memory::should_skip_autosave_content(&effective_input)
            {
                let user_key = autosave_memory_key("user_msg");
                let _ = mem
                    .store(
                        &user_key,
                        &effective_input,
                        MemoryCategory::Conversation,
                        memory_session_id.as_deref(),
                    )
                    .await;
            }

            // Inject memory + hardware RAG context into user message
            let mem_context = build_context(
                mem.as_ref(),
                &effective_input,
                config.memory.min_relevance_score,
                memory_session_id.as_deref(),
            )
            .await;
            let rag_limit = if config.agent.compact_context { 2 } else { 5 };
            let hw_context = hardware_rag
                .as_ref()
                .map(|r| build_hardware_context(r, &effective_input, &board_names, rag_limit))
                .unwrap_or_default();
            let context = format!("{mem_context}{hw_context}");
            let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S %Z");
            let enriched = if context.is_empty() {
                format!("[{now}] {effective_input}")
            } else {
                format!("{context}[{now}] {effective_input}")
            };

            history.push(ChatMessage::user(&enriched));

            // Compute per-turn excluded MCP tools from tool_filter_groups.
            let excluded_tools = compute_excluded_mcp_tools(
                &tools_registry,
                &config.agent.tool_filter_groups,
                &effective_input,
            );

            // Set up streaming channel so tool progress and response
            // content are printed progressively instead of buffered.
            let (delta_tx, mut delta_rx) = tokio::sync::mpsc::channel::<DraftEvent>(64);
            let content_was_streamed =
                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let content_streamed_flag = content_was_streamed.clone();
            let is_tty = std::io::IsTerminal::is_terminal(&std::io::stderr());

            let consumer_handle = tokio::spawn(async move {
                use std::io::Write;
                while let Some(event) = delta_rx.recv().await {
                    match event {
                        DraftEvent::Clear => {
                            let _ = writeln!(std::io::stderr());
                        }
                        DraftEvent::Progress(text) => {
                            if is_tty {
                                let _ = write!(std::io::stderr(), "\x1b[2m{text}\x1b[0m");
                            } else {
                                let _ = write!(std::io::stderr(), "{text}");
                            }
                            let _ = std::io::stderr().flush();
                        }
                        DraftEvent::Content(text) => {
                            content_streamed_flag.store(true, std::sync::atomic::Ordering::Relaxed);
                            print!("{text}");
                            let _ = std::io::stdout().flush();
                        }
                    }
                }
            });

            // Ctrl+C cancels the in-flight turn instead of killing the process.
            let cancel_token = CancellationToken::new();
            let cancel_token_clone = cancel_token.clone();
            let ctrlc_handle = tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    cancel_token_clone.cancel();
                }
            });

            let response = loop {
                match run_tool_call_loop(
                    provider.as_ref(),
                    &mut history,
                    &tools_registry,
                    observer.as_ref(),
                    &provider_name,
                    &model_name,
                    turn_temperature,
                    true,
                    approval_manager.as_ref(),
                    channel_name,
                    None,
                    &config.multimodal,
                    config.agent.max_tool_iterations,
                    Some(cancel_token.clone()),
                    Some(delta_tx.clone()),
                    None,
                    &excluded_tools,
                    &config.agent.tool_call_dedup_exempt,
                    activated_handle.as_ref(),
                    Some(model_switch_callback.clone()),
                    &config.pacing,
                    config.agent.max_tool_result_chars,
                    config.agent.max_context_tokens,
                    None, // shared_budget
                )
                .await
                {
                    Ok(resp) => break resp,
                    Err(e) => {
                        if is_tool_loop_cancelled(&e) {
                            eprintln!("\n\x1b[2m(cancelled)\x1b[0m");
                            break String::new();
                        }
                        if let Some((new_provider, new_model)) = is_model_switch_requested(&e) {
                            tracing::info!(
                                "Model switch requested, switching from {} {} to {} {}",
                                provider_name,
                                model_name,
                                new_provider,
                                new_model
                            );

                            provider = providers::create_routed_provider_with_options(
                                &new_provider,
                                config.api_key.as_deref(),
                                config.api_url.as_deref(),
                                &config.reliability,
                                &config.model_routes,
                                &new_model,
                                &provider_runtime_options,
                            )?;

                            provider_name = new_provider;
                            model_name = new_model;

                            clear_model_switch_request();

                            observer.record_event(&ObserverEvent::AgentStart {
                                provider: provider_name.to_string(),
                                model: model_name.to_string(),
                            });

                            continue;
                        }
                        // Context overflow recovery: compress and retry
                        if crate::providers::reliable::is_context_window_exceeded(&e) {
                            tracing::warn!(
                                "Context overflow in interactive loop, attempting recovery"
                            );
                            let mut compressor =
                                crate::agent::context_compressor::ContextCompressor::new(
                                    config.agent.context_compression.clone(),
                                    config.agent.max_context_tokens,
                                )
                                .with_memory(mem.clone());
                            let error_msg = format!("{e}");
                            match compressor
                                .compress_on_error(
                                    &mut history,
                                    provider.as_ref(),
                                    &model_name,
                                    &error_msg,
                                )
                                .await
                            {
                                Ok(true) => {
                                    tracing::info!(
                                        "Context recovered via compression, retrying turn"
                                    );
                                    continue;
                                }
                                Ok(false) => {
                                    tracing::warn!("Compression ran but couldn't reduce enough");
                                }
                                Err(compress_err) => {
                                    tracing::warn!(
                                        error = %compress_err,
                                        "Compression failed during recovery"
                                    );
                                }
                            }
                        }

                        eprintln!("\nError: {e}\n");
                        break String::new();
                    }
                }
            };

            // Clean up: stop the Ctrl+C listener and flush streaming events.
            ctrlc_handle.abort();
            drop(delta_tx);
            let _ = consumer_handle.await;

            final_output = response.clone();
            if content_was_streamed.load(std::sync::atomic::Ordering::Relaxed) {
                println!();
            } else if let Err(e) = crate::channels::Channel::send(
                &cli,
                &crate::channels::traits::SendMessage::new(format!("\n{response}\n"), "user"),
            )
            .await
            {
                eprintln!("\nError sending CLI response: {e}\n");
            }
            observer.record_event(&ObserverEvent::TurnComplete);

            // Context compression before hard trimming to preserve long-context signal.
            {
                let compressor = crate::agent::context_compressor::ContextCompressor::new(
                    config.agent.context_compression.clone(),
                    config.agent.max_context_tokens,
                )
                .with_memory(mem.clone());
                match compressor
                    .compress_if_needed(&mut history, provider.as_ref(), &model_name)
                    .await
                {
                    Ok(result) if result.compressed => {
                        tracing::info!(
                            passes = result.passes_used,
                            before = result.tokens_before,
                            after = result.tokens_after,
                            "Context compression complete"
                        );
                    }
                    Ok(_) => {} // No compression needed
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "Context compression failed, falling back to history trim"
                        );
                        trim_history(&mut history, config.agent.max_history_messages / 2);
                    }
                }
            }

            // Hard cap as a safety net.
            trim_history(&mut history, config.agent.max_history_messages);

            // Restore base system prompt (remove per-turn thinking prefix).
            if thinking_params.system_prompt_prefix.is_some() {
                if let Some(sys_msg) = history.first_mut() {
                    if sys_msg.role == "system" {
                        sys_msg.content.clone_from(&base_system_prompt);
                    }
                }
            }

            if let Some(path) = session_state_file.as_deref() {
                save_interactive_session_history(path, &history)?;
            }
        }
    }

    let duration = start.elapsed();
    observer.record_event(&ObserverEvent::AgentEnd {
        provider: provider_name.to_string(),
        model: model_name.to_string(),
        duration,
        tokens_used: None,
        cost_usd: None,
    });

    Ok(final_output)
}

/// Process a single message through the full agent (with tools, peripherals, memory).
/// Used by channels (Telegram, Discord, etc.) to enable hardware and tool use.
pub async fn process_message(
    config: Config,
    message: &str,
    session_id: Option<&str>,
) -> Result<String> {
    let observer: Arc<dyn Observer> =
        Arc::from(observability::create_observer(&config.observability));
    let runtime: Arc<dyn runtime::RuntimeAdapter> =
        Arc::from(runtime::create_runtime(&config.runtime)?);
    let security = Arc::new(SecurityPolicy::from_config(
        &config.autonomy,
        &config.workspace_dir,
    ));
    let approval_manager = ApprovalManager::for_non_interactive(&config.autonomy);
    let mem: Arc<dyn Memory> = Arc::from(memory::create_memory_with_storage_and_routes(
        &config.memory,
        &config.embedding_routes,
        Some(&config.storage.provider.config),
        &config.workspace_dir,
        config.api_key.as_deref(),
    )?);

    let (composio_key, composio_entity_id) = if config.composio.enabled {
        (
            config.composio.api_key.as_deref(),
            Some(config.composio.entity_id.as_str()),
        )
    } else {
        (None, None)
    };
    let (
        mut tools_registry,
        delegate_handle_pm,
        _reaction_handle_pm,
        _channel_map_handle_pm,
        _ask_user_handle_pm,
        _escalate_handle_pm,
    ) = tools::all_tools_with_runtime(
        Arc::new(config.clone()),
        &security,
        runtime,
        mem.clone(),
        composio_key,
        composio_entity_id,
        &config.browser,
        &config.http_request,
        &config.web_fetch,
        &config.workspace_dir,
        &config.agents,
        config.api_key.as_deref(),
        &config,
        None,
    );
    let peripheral_tools: Vec<Box<dyn Tool>> =
        crate::peripherals::create_peripheral_tools(&config.peripherals).await?;
    tools_registry.extend(peripheral_tools);

    // ── Wire MCP tools (non-fatal) — process_message path ────────
    // NOTE: Same ordering contract as the CLI path above — MCP tools must be
    // injected after filter_primary_agent_tools_or_fail (or equivalent built-in
    // tool allow/deny filtering) to avoid MCP tools being silently dropped.
    let mut deferred_section = String::new();
    let mut activated_handle_pm: Option<
        std::sync::Arc<std::sync::Mutex<crate::tools::ActivatedToolSet>>,
    > = None;
    if config.mcp.enabled && !config.mcp.servers.is_empty() {
        tracing::info!(
            "Initializing MCP client — {} server(s) configured",
            config.mcp.servers.len()
        );
        match crate::tools::McpRegistry::connect_all(&config.mcp.servers).await {
            Ok(registry) => {
                let registry = std::sync::Arc::new(registry);
                if config.mcp.deferred_loading {
                    let deferred_set = crate::tools::DeferredMcpToolSet::from_registry(
                        std::sync::Arc::clone(&registry),
                    )
                    .await;
                    tracing::info!(
                        "MCP deferred: {} tool stub(s) from {} server(s)",
                        deferred_set.len(),
                        registry.server_count()
                    );
                    deferred_section =
                        crate::tools::mcp_deferred::build_deferred_tools_section(&deferred_set);
                    let activated = std::sync::Arc::new(std::sync::Mutex::new(
                        crate::tools::ActivatedToolSet::new(),
                    ));
                    activated_handle_pm = Some(std::sync::Arc::clone(&activated));
                    tools_registry.push(Box::new(crate::tools::ToolSearchTool::new(
                        deferred_set,
                        activated,
                    )));
                } else {
                    let names = registry.tool_names();
                    let mut registered = 0usize;
                    for name in names {
                        if let Some(def) = registry.get_tool_def(&name).await {
                            let wrapper: std::sync::Arc<dyn Tool> =
                                std::sync::Arc::new(crate::tools::McpToolWrapper::new(
                                    name,
                                    def,
                                    std::sync::Arc::clone(&registry),
                                ));
                            if let Some(ref handle) = delegate_handle_pm {
                                handle.write().push(std::sync::Arc::clone(&wrapper));
                            }
                            tools_registry.push(Box::new(crate::tools::ArcToolRef(wrapper)));
                            registered += 1;
                        }
                    }
                    tracing::info!(
                        "MCP: {} tool(s) registered from {} server(s)",
                        registered,
                        registry.server_count()
                    );
                }
            }
            Err(e) => {
                tracing::error!("MCP registry failed to initialize: {e:#}");
            }
        }
    }

    let provider_name = config.default_provider.as_deref().unwrap_or("openrouter");
    let model_name = config
        .default_model
        .clone()
        .unwrap_or_else(|| "anthropic/claude-sonnet-4-20250514".into());
    let provider_runtime_options = providers::provider_runtime_options_from_config(&config);
    let provider: Box<dyn Provider> = providers::create_routed_provider_with_options(
        provider_name,
        config.api_key.as_deref(),
        config.api_url.as_deref(),
        &config.reliability,
        &config.model_routes,
        &model_name,
        &provider_runtime_options,
    )?;

    let hardware_rag: Option<crate::rag::HardwareRag> = config
        .peripherals
        .datasheet_dir
        .as_ref()
        .filter(|d| !d.trim().is_empty())
        .map(|dir| crate::rag::HardwareRag::load(&config.workspace_dir, dir.trim()))
        .and_then(Result::ok)
        .filter(|r: &crate::rag::HardwareRag| !r.is_empty());
    let board_names: Vec<String> = config
        .peripherals
        .boards
        .iter()
        .map(|b| b.board.clone())
        .collect();

    // ── Load locale-aware tool descriptions ────────────────────────
    let i18n_locale = config
        .locale
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(crate::i18n::detect_locale);
    let i18n_search_dirs = crate::i18n::default_search_dirs(&config.workspace_dir);
    let i18n_descs = crate::i18n::ToolDescriptions::load(&i18n_locale, &i18n_search_dirs);

    let skills = crate::skills::load_skills_with_config(&config.workspace_dir, &config);

    // Register skill-defined tools as callable tool specs (process_message path).
    tools::register_skill_tools(&mut tools_registry, &skills, security.clone());

    let mut tool_descs: Vec<(&str, &str)> = vec![
        ("shell", "Execute terminal commands."),
        ("file_read", "Read file contents."),
        ("file_write", "Write file contents."),
        ("memory_store", "Save to memory."),
        ("memory_recall", "Search memory."),
        ("memory_forget", "Delete a memory entry."),
        (
            "model_routing_config",
            "Configure default model, scenario routing, and delegate agents.",
        ),
        ("screenshot", "Capture a screenshot."),
        ("image_info", "Read image metadata."),
    ];
    if matches!(
        config.skills.prompt_injection_mode,
        crate::config::SkillsPromptInjectionMode::Compact
    ) {
        tool_descs.push((
            "read_skill",
            "Load the full source for an available skill by name.",
        ));
    }
    if config.browser.enabled {
        tool_descs.push(("browser_open", "Open approved URLs in browser."));
    }
    if config.composio.enabled {
        tool_descs.push(("composio", "Execute actions on 1000+ apps via Composio."));
    }
    if config.peripherals.enabled && !config.peripherals.boards.is_empty() {
        tool_descs.push(("gpio_read", "Read GPIO pin value on connected hardware."));
        tool_descs.push((
            "gpio_write",
            "Set GPIO pin high or low on connected hardware.",
        ));
        tool_descs.push((
            "arduino_upload",
            "Upload Arduino sketch. Use for 'make a heart', custom patterns. You write full .ino code; ZeroClaw uploads it.",
        ));
        tool_descs.push((
            "hardware_memory_map",
            "Return flash and RAM address ranges. Use when user asks for memory addresses or memory map.",
        ));
        tool_descs.push((
            "hardware_board_info",
            "Return full board info (chip, architecture, memory map). Use when user asks for board info, what board, connected hardware, or chip info.",
        ));
        tool_descs.push((
            "hardware_memory_read",
            "Read actual memory/register values from Nucleo. Use when user asks to read registers, read memory, dump lower memory 0-126, or give address and value.",
        ));
        tool_descs.push((
            "hardware_capabilities",
            "Query connected hardware for reported GPIO pins and LED pin. Use when user asks what pins are available.",
        ));
    }

    // Filter out tools excluded for non-CLI channels (gateway counts as non-CLI).
    // Skip when autonomy is `Full` — full-autonomy agents keep all tools.
    if config.autonomy.level != AutonomyLevel::Full {
        let excluded = &config.autonomy.non_cli_excluded_tools;
        if !excluded.is_empty() {
            tool_descs.retain(|(name, _)| !excluded.iter().any(|ex| ex == name));
        }
    }

    let bootstrap_max_chars = if config.agent.compact_context {
        Some(6000)
    } else {
        None
    };
    let native_tools = provider.supports_native_tools();
    let mut system_prompt = crate::channels::build_system_prompt_with_mode_and_autonomy(
        &config.workspace_dir,
        &model_name,
        &tool_descs,
        &skills,
        Some(&config.identity),
        bootstrap_max_chars,
        Some(&config.autonomy),
        native_tools,
        config.skills.prompt_injection_mode,
        config.agent.compact_context,
        config.agent.max_system_prompt_chars,
    );
    if !native_tools {
        system_prompt.push_str(&build_tool_instructions(&tools_registry, Some(&i18n_descs)));
    }
    if !deferred_section.is_empty() {
        system_prompt.push('\n');
        system_prompt.push_str(&deferred_section);
    }

    // ── Parse thinking directive from user message ─────────────
    let (thinking_directive, effective_message) =
        match crate::agent::thinking::parse_thinking_directive(message) {
            Some((level, remaining)) => {
                tracing::info!(thinking_level = ?level, "Thinking directive parsed from message");
                (Some(level), remaining)
            }
            None => (None, message.to_string()),
        };
    let thinking_level = crate::agent::thinking::resolve_thinking_level(
        thinking_directive,
        None,
        &config.agent.thinking,
    );
    let thinking_params = crate::agent::thinking::apply_thinking_level(thinking_level);
    let effective_temperature = crate::agent::thinking::clamp_temperature(
        config.default_temperature + thinking_params.temperature_adjustment,
    );

    // Prepend thinking system prompt prefix when present.
    if let Some(ref prefix) = thinking_params.system_prompt_prefix {
        system_prompt = format!("{prefix}\n\n{system_prompt}");
    }

    let effective_msg_ref = effective_message.as_str();
    let mem_context = build_context(
        mem.as_ref(),
        effective_msg_ref,
        config.memory.min_relevance_score,
        session_id,
    )
    .await;
    let rag_limit = if config.agent.compact_context { 2 } else { 5 };
    let hw_context = hardware_rag
        .as_ref()
        .map(|r| build_hardware_context(r, effective_msg_ref, &board_names, rag_limit))
        .unwrap_or_default();
    let context = format!("{mem_context}{hw_context}");
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S %Z");
    let enriched = if context.is_empty() {
        format!("[{now}] {effective_message}")
    } else {
        format!("{context}[{now}] {effective_message}")
    };

    let mut history = vec![
        ChatMessage::system(&system_prompt),
        ChatMessage::user(&enriched),
    ];
    let mut excluded_tools = compute_excluded_mcp_tools(
        &tools_registry,
        &config.agent.tool_filter_groups,
        effective_msg_ref,
    );
    if config.autonomy.level != AutonomyLevel::Full {
        excluded_tools.extend(config.autonomy.non_cli_excluded_tools.iter().cloned());
    }

    agent_turn(
        provider.as_ref(),
        &mut history,
        &tools_registry,
        observer.as_ref(),
        provider_name,
        &model_name,
        effective_temperature,
        true,
        "daemon",
        None,
        &config.multimodal,
        config.agent.max_tool_iterations,
        Some(&approval_manager),
        &excluded_tools,
        &config.agent.tool_call_dedup_exempt,
        activated_handle_pm.as_ref(),
        None,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{Memory, MemoryCategory, SqliteMemory};
    use crate::security::SecurityPolicy;
    use std::sync::Arc;
    use tempfile::TempDir;

    #[test]
    fn tools_to_openai_format_produces_valid_schema() {
        use crate::security::SecurityPolicy;
        let security = Arc::new(SecurityPolicy::from_config(
            &crate::config::AutonomyConfig::default(),
            std::path::Path::new("/tmp"),
        ));
        let tools = tools::default_tools(security);
        let formatted = tools_to_openai_format(&tools);

        assert!(!formatted.is_empty());
        for tool_json in &formatted {
            assert_eq!(tool_json["type"], "function");
            assert!(tool_json["function"]["name"].is_string());
            assert!(tool_json["function"]["description"].is_string());
            assert!(!tool_json["function"]["name"].as_str().unwrap().is_empty());
        }
        // Verify known tools are present
        let names: Vec<&str> = formatted
            .iter()
            .filter_map(|t| t["function"]["name"].as_str())
            .collect();
        assert!(names.contains(&"shell"));
        assert!(names.contains(&"file_read"));
    }

    #[test]
    fn autosave_memory_key_has_prefix_and_uniqueness() {
        let key1 = autosave_memory_key("user_msg");
        let key2 = autosave_memory_key("user_msg");

        assert!(key1.starts_with("user_msg_"));
        assert!(key2.starts_with("user_msg_"));
        assert_ne!(key1, key2);
    }

    #[tokio::test]
    async fn autosave_memory_keys_preserve_multiple_turns() {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new(tmp.path()).unwrap();

        let key1 = autosave_memory_key("user_msg");
        let key2 = autosave_memory_key("user_msg");

        mem.store(&key1, "I'm Paul", MemoryCategory::Conversation, None)
            .await
            .unwrap();
        mem.store(&key2, "I'm 45", MemoryCategory::Conversation, None)
            .await
            .unwrap();

        assert_eq!(mem.count().await.unwrap(), 2);

        let recalled = mem.recall("45", 5, None, None, None).await.unwrap();
        assert!(recalled.iter().any(|entry| entry.content.contains("45")));
    }

    #[tokio::test]
    async fn build_context_ignores_legacy_assistant_autosave_entries() {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new(tmp.path()).unwrap();
        mem.store(
            "assistant_resp_poisoned",
            "User suffered a fabricated event",
            MemoryCategory::Daily,
            None,
        )
        .await
        .unwrap();
        mem.store(
            "user_msg_real",
            "User asked for concise status updates",
            MemoryCategory::Conversation,
            None,
        )
        .await
        .unwrap();

        let context = build_context(&mem, "status updates", 0.0, None).await;
        assert!(context.contains("user_msg_real"));
        assert!(!context.contains("assistant_resp_poisoned"));
        assert!(!context.contains("fabricated event"));
    }

    /// When `build_system_prompt_with_mode` is called with `native_tools = true`,
    /// the output must contain ZERO XML protocol artifacts. In the native path
    /// `build_tool_instructions` is never called, so the system prompt alone
    /// must be clean of XML tool-call protocol.
    #[test]
    fn native_tools_system_prompt_contains_zero_xml() {
        use crate::channels::build_system_prompt_with_mode;

        let tool_summaries: Vec<(&str, &str)> = vec![
            ("shell", "Execute shell commands"),
            ("file_read", "Read files"),
        ];

        let system_prompt = build_system_prompt_with_mode(
            std::path::Path::new("/tmp"),
            "test-model",
            &tool_summaries,
            &[],  // no skills
            None, // no identity config
            None, // no bootstrap_max_chars
            true, // native_tools
            crate::config::SkillsPromptInjectionMode::Full,
            crate::security::AutonomyLevel::default(),
        );

        // Must contain zero XML protocol artifacts
        assert!(
            !system_prompt.contains("<tool_call>"),
            "Native prompt must not contain <tool_call>"
        );
        assert!(
            !system_prompt.contains("</tool_call>"),
            "Native prompt must not contain </tool_call>"
        );
        assert!(
            !system_prompt.contains("<tool_result>"),
            "Native prompt must not contain <tool_result>"
        );
        assert!(
            !system_prompt.contains("</tool_result>"),
            "Native prompt must not contain </tool_result>"
        );
        assert!(
            !system_prompt.contains("## Tool Use Protocol"),
            "Native prompt must not contain XML protocol header"
        );

        // Positive: native prompt should still list tools and contain task instructions
        assert!(
            system_prompt.contains("shell"),
            "Native prompt must list tool names"
        );
        assert!(
            system_prompt.contains("## Your Task"),
            "Native prompt should contain task instructions"
        );
    }
}
