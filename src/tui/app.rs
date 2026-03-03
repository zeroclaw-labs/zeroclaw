//! TUI application runtime and event loop.
//!
//! Initialization order:
//! 1) panic hook
//! 2) signal handlers
//! 3) raw mode + alternate screen
//! 4) async event loop (`EventStream` + agent delta channel)

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use futures_util::StreamExt;
use ratatui::crossterm::event::{
    Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use tokio_util::sync::CancellationToken;

use crate::agent::loop_::{
    build_shell_policy_instructions, build_tool_instructions, create_cost_enforcement_context,
    run_tool_call_loop_with_non_cli_approval_context, scope_cost_enforcement_context,
    SafetyHeartbeatConfig,
};
use crate::config::{resolve_default_model_id, Config, ProgressMode};
use crate::memory::{self, Memory};
use crate::observability::{self, Observer};
use crate::providers::{self, ChatMessage, Provider};
use crate::runtime;
use crate::security::SecurityPolicy;
use crate::tools::{self, Tool};

use super::events::{translate_delta, TuiEvent};
use super::state::{InputMode, TuiRole, TuiState};
use super::terminal::{install_panic_hook, install_signal_handlers};
use super::widgets;

const DELTA_CHANNEL_BUFFER: usize = 256;
const DOUBLE_CTRL_C_WINDOW: Duration = Duration::from_millis(300);

#[derive(Debug)]
struct AgentTaskResult {
    request_id: u64,
    history: Vec<ChatMessage>,
    output: std::result::Result<String, String>,
}

struct TuiRuntimeContext {
    provider_name: String,
    model_name: String,
    provider: Arc<dyn Provider>,
    observer: Arc<dyn Observer>,
    tools_registry: Arc<Vec<Box<dyn Tool>>>,
    multimodal: crate::config::MultimodalConfig,
    max_tool_iterations: usize,
    hooks: Option<Arc<crate::hooks::HookRunner>>,
    safety_heartbeat: Option<SafetyHeartbeatConfig>,
    cost_enforcement: Option<crate::agent::loop_::CostEnforcementContext>,
    temperature: f64,
    history: Vec<ChatMessage>,
}

pub async fn run(config: &Config) -> Result<()> {
    install_panic_hook();
    let session_cancel = CancellationToken::new();
    install_signal_handlers(session_cancel.child_token()).await;

    ratatui::crossterm::terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    ratatui::crossterm::execute!(
        stdout,
        ratatui::crossterm::terminal::EnterAlternateScreen,
        ratatui::crossterm::cursor::Hide
    )?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = ratatui::Terminal::new(backend)?;

    let app_result = run_loop(&mut terminal, config, session_cancel).await;

    let _ = ratatui::crossterm::execute!(
        terminal.backend_mut(),
        ratatui::crossterm::terminal::LeaveAlternateScreen,
        ratatui::crossterm::cursor::Show
    );
    let _ = ratatui::crossterm::terminal::disable_raw_mode();

    app_result
}

async fn run_loop(
    terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    config: &Config,
    session_cancel: CancellationToken,
) -> Result<()> {
    let mut runtime_ctx = bootstrap_runtime(config).await?;
    let mut state = TuiState::new(
        runtime_ctx.provider_name.clone(),
        runtime_ctx.model_name.clone(),
    );
    state.push_chat_message(
        TuiRole::System,
        "ZeroClaw TUI ready. Press i to edit, Enter to send, Ctrl+C to cancel, q to quit.",
    );

    let (delta_tx, mut delta_rx) = tokio::sync::mpsc::channel::<String>(DELTA_CHANNEL_BUFFER);
    let (result_tx, mut result_rx) = tokio::sync::mpsc::unbounded_channel::<AgentTaskResult>();
    let mut event_stream = EventStream::new();
    let mut active_request_cancel: Option<CancellationToken> = None;
    let mut active_request_id: Option<u64> = None;
    let mut next_request_id = 1_u64;
    let mut last_ctrl_c_at: Option<Instant> = None;

    loop {
        terminal.draw(|frame| {
            if let Some(cursor) = widgets::render(frame, &state) {
                frame.set_cursor_position((cursor.x, cursor.y));
            }
        })?;

        tokio::select! {
            maybe_event = event_stream.next() => {
                match maybe_event {
                    Some(Ok(event)) => {
                        handle_terminal_event(
                            event,
                            &mut state,
                            &mut runtime_ctx,
                            &delta_tx,
                            &result_tx,
                            &mut active_request_cancel,
                            &mut active_request_id,
                            &mut next_request_id,
                            &mut last_ctrl_c_at,
                        )?;
                    }
                    Some(Err(error)) => {
                        state.push_chat_message(TuiRole::Error, format!("Input error: {error}"));
                    }
                    None => {
                        state.should_quit = true;
                    }
                }
            }
            Some(delta) = delta_rx.recv() => {
                handle_tui_event(translate_delta(delta), &mut state);
            }
            Some(task_result) = result_rx.recv() => {
                if active_request_id != Some(task_result.request_id) {
                    continue;
                }
                active_request_cancel = None;
                active_request_id = None;
                state.awaiting_response = false;
                state.set_idle();
                state.clear_progress();

                runtime_ctx.history = task_result.history;
                match task_result.output {
                    Ok(output) => {
                        finalize_assistant_output(&mut state, output);
                    }
                    Err(error) => {
                        state.finish_streaming_assistant();
                        state.push_chat_message(TuiRole::Error, format!("Agent error: {error}"));
                    }
                }
            }
            _ = session_cancel.cancelled() => {
                state.should_quit = true;
            }
        }

        if state.should_quit {
            if let Some(cancel) = active_request_cancel.take() {
                cancel.cancel();
            }
            break;
        }
    }

    Ok(())
}

fn handle_terminal_event(
    event: Event,
    state: &mut TuiState,
    runtime_ctx: &mut TuiRuntimeContext,
    delta_tx: &tokio::sync::mpsc::Sender<String>,
    result_tx: &tokio::sync::mpsc::UnboundedSender<AgentTaskResult>,
    active_request_cancel: &mut Option<CancellationToken>,
    active_request_id: &mut Option<u64>,
    next_request_id: &mut u64,
    last_ctrl_c_at: &mut Option<Instant>,
) -> Result<()> {
    match event {
        Event::Key(key) => {
            if key.kind != KeyEventKind::Press {
                return Ok(());
            }
            let handled = handle_key_event(
                key,
                state,
                runtime_ctx,
                delta_tx,
                result_tx,
                active_request_cancel,
                active_request_id,
                next_request_id,
                last_ctrl_c_at,
            )?;
            if !handled && state.mode == InputMode::Editing {
                handle_editing_text_input(key, state);
            }
        }
        Event::Resize(_, _) => {}
        Event::Paste(payload) => {
            if state.mode == InputMode::Editing {
                super::widgets::input::append_sanitized_input(&mut state.input_buffer, &payload);
            }
        }
        _ => {}
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn handle_key_event(
    key: KeyEvent,
    state: &mut TuiState,
    runtime_ctx: &mut TuiRuntimeContext,
    delta_tx: &tokio::sync::mpsc::Sender<String>,
    result_tx: &tokio::sync::mpsc::UnboundedSender<AgentTaskResult>,
    active_request_cancel: &mut Option<CancellationToken>,
    active_request_id: &mut Option<u64>,
    next_request_id: &mut u64,
    last_ctrl_c_at: &mut Option<Instant>,
) -> Result<bool> {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('d') {
        trigger_quit(state, active_request_cancel, active_request_id);
        return Ok(true);
    }

    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        let now = Instant::now();
        if last_ctrl_c_at
            .as_ref()
            .is_some_and(|last| now.duration_since(*last) <= DOUBLE_CTRL_C_WINDOW)
        {
            trigger_quit(state, active_request_cancel, active_request_id);
            return Ok(true);
        }
        *last_ctrl_c_at = Some(now);
        if let Some(cancel) = active_request_cancel.take() {
            cancel.cancel();
            *active_request_id = None;
            state.awaiting_response = false;
            state.set_idle();
            state.clear_progress();
            state.finish_streaming_assistant();
            state.push_chat_message(TuiRole::System, "Cancelled current request.");
        }
        return Ok(true);
    }

    match state.mode {
        InputMode::Normal => match key.code {
            KeyCode::Char('q') => {
                trigger_quit(state, active_request_cancel, active_request_id);
                Ok(true)
            }
            KeyCode::Char('i') => {
                state.mode = InputMode::Editing;
                Ok(true)
            }
            KeyCode::PageUp => {
                state.scroll_page_up(12);
                Ok(true)
            }
            KeyCode::PageDown => {
                state.scroll_page_down(12);
                Ok(true)
            }
            _ => Ok(false),
        },
        InputMode::Editing => match key.code {
            KeyCode::Esc => {
                state.mode = InputMode::Normal;
                Ok(true)
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                super::widgets::input::append_sanitized_input(&mut state.input_buffer, "\n");
                Ok(true)
            }
            KeyCode::Enter => {
                if !state.awaiting_response {
                    submit_user_message(
                        state,
                        runtime_ctx,
                        delta_tx,
                        result_tx,
                        active_request_cancel,
                        active_request_id,
                        next_request_id,
                    )?;
                }
                Ok(true)
            }
            KeyCode::Up => {
                if let Some(prev) = state.history_prev() {
                    state.input_buffer = prev;
                }
                Ok(true)
            }
            KeyCode::Down => {
                if let Some(next) = state.history_next() {
                    state.input_buffer = next;
                }
                Ok(true)
            }
            KeyCode::PageUp => {
                state.scroll_page_up(12);
                Ok(true)
            }
            KeyCode::PageDown => {
                state.scroll_page_down(12);
                Ok(true)
            }
            KeyCode::Backspace => {
                state.input_buffer.pop();
                Ok(true)
            }
            KeyCode::Tab => {
                super::widgets::input::append_sanitized_input(&mut state.input_buffer, "\t");
                Ok(true)
            }
            _ => Ok(false),
        },
    }
}

fn handle_editing_text_input(key: KeyEvent, state: &mut TuiState) {
    if state.mode != InputMode::Editing {
        return;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::ALT) {
        return;
    }
    if let KeyCode::Char(ch) = key.code {
        let mut utf8_buf = [0_u8; 4];
        let as_str = ch.encode_utf8(&mut utf8_buf);
        super::widgets::input::append_sanitized_input(&mut state.input_buffer, as_str);
    }
}

fn handle_tui_event(event: TuiEvent, state: &mut TuiState) {
    match event {
        TuiEvent::Delta { text } => {
            state.append_stream_delta(&text);
        }
        TuiEvent::Clear => {
            state.start_streaming_assistant();
        }
        TuiEvent::ProgressLine { text } => {
            state.set_thinking(Some(text));
        }
        TuiEvent::ProgressBlock { content } => {
            state.set_tool_running(content);
        }
        TuiEvent::UserMessage { .. }
        | TuiEvent::Cancel
        | TuiEvent::Quit
        | TuiEvent::Key(_)
        | TuiEvent::Resize(_, _) => {}
    }
}

fn finalize_assistant_output(state: &mut TuiState, output: String) {
    if output.trim().is_empty() {
        state.finish_streaming_assistant();
        return;
    }

    if let Some(idx) = state.streaming_assistant_idx {
        if let Some(msg) = state.messages.get_mut(idx) {
            if msg.content.trim().is_empty() {
                msg.content = output;
            }
        }
        state.finish_streaming_assistant();
        return;
    }

    state.push_chat_message(TuiRole::Assistant, output);
}

#[allow(clippy::too_many_arguments)]
fn submit_user_message(
    state: &mut TuiState,
    runtime_ctx: &mut TuiRuntimeContext,
    delta_tx: &tokio::sync::mpsc::Sender<String>,
    result_tx: &tokio::sync::mpsc::UnboundedSender<AgentTaskResult>,
    active_request_cancel: &mut Option<CancellationToken>,
    active_request_id: &mut Option<u64>,
    next_request_id: &mut u64,
) -> Result<()> {
    let sanitized = super::widgets::input::sanitize_input(&state.input_buffer);
    let user_text = sanitized.trim().to_string();
    if user_text.is_empty() {
        state.input_buffer.clear();
        return Ok(());
    }

    state.note_submitted_input(&user_text);
    state.input_buffer.clear();
    state.push_chat_message(TuiRole::User, user_text.clone());
    state.awaiting_response = true;
    state.set_thinking(Some("🤔 Thinking...\n".to_string()));
    state.start_streaming_assistant();

    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S %Z");
    runtime_ctx
        .history
        .push(ChatMessage::user(format!("[{now}] {user_text}")));

    let mut task_history = std::mem::take(&mut runtime_ctx.history);
    let provider = Arc::clone(&runtime_ctx.provider);
    let observer = Arc::clone(&runtime_ctx.observer);
    let tools_registry = Arc::clone(&runtime_ctx.tools_registry);
    let provider_name = runtime_ctx.provider_name.clone();
    let model_name = runtime_ctx.model_name.clone();
    let multimodal = runtime_ctx.multimodal.clone();
    let max_tool_iterations = runtime_ctx.max_tool_iterations;
    let hooks = runtime_ctx.hooks.clone();
    let safety_heartbeat = runtime_ctx.safety_heartbeat.clone();
    let cost_enforcement = runtime_ctx.cost_enforcement.clone();
    let temperature = runtime_ctx.temperature;

    let cancel = CancellationToken::new();
    let request_id = *next_request_id;
    *next_request_id = next_request_id.saturating_add(1);
    *active_request_cancel = Some(cancel.clone());
    *active_request_id = Some(request_id);
    let child_token = cancel.child_token();
    let delta_tx = delta_tx.clone();
    let result_tx = result_tx.clone();

    tokio::spawn(async move {
        let run_result = scope_cost_enforcement_context(
            cost_enforcement,
            run_tool_call_loop_with_non_cli_approval_context(
                provider.as_ref(),
                &mut task_history,
                tools_registry.as_slice(),
                observer.as_ref(),
                &provider_name,
                &model_name,
                temperature,
                false,
                None,
                "tui",
                None,
                &multimodal,
                max_tool_iterations,
                Some(child_token),
                Some(delta_tx),
                hooks.as_deref(),
                &[],
                ProgressMode::Verbose,
                safety_heartbeat,
            ),
        )
        .await
        .map_err(|error| error.to_string());

        let _ = result_tx.send(AgentTaskResult {
            request_id,
            history: task_history,
            output: run_result,
        });
    });

    Ok(())
}

fn trigger_quit(
    state: &mut TuiState,
    active_request_cancel: &mut Option<CancellationToken>,
    active_request_id: &mut Option<u64>,
) {
    if let Some(cancel) = active_request_cancel.take() {
        cancel.cancel();
    }
    *active_request_id = None;
    state.should_quit = true;
}

async fn bootstrap_runtime(config: &Config) -> Result<TuiRuntimeContext> {
    if let Err(error) = crate::plugins::runtime::initialize_from_config(&config.plugins) {
        tracing::warn!("plugin registry initialization skipped: {error}");
    }

    let observer: Arc<dyn Observer> =
        Arc::from(observability::create_observer(&config.observability));
    let runtime_adapter: Arc<dyn runtime::RuntimeAdapter> =
        Arc::from(runtime::create_runtime(&config.runtime)?);
    let security = Arc::new(SecurityPolicy::from_config(
        &config.autonomy,
        &config.workspace_dir,
    ));
    let memory: Arc<dyn Memory> = Arc::from(memory::create_memory_with_storage(
        &config.memory,
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

    let mut tools_registry = tools::all_tools_with_runtime(
        Arc::new(config.clone()),
        &security,
        runtime_adapter,
        memory,
        composio_key,
        composio_entity_id,
        &config.browser,
        &config.http_request,
        &config.web_fetch,
        &config.workspace_dir,
        &config.agents,
        config.api_key.as_deref(),
        config,
    );

    let peripheral_tools: Vec<Box<dyn Tool>> =
        crate::peripherals::create_peripheral_tools(&config.peripherals).await?;
    if !peripheral_tools.is_empty() {
        tools_registry.extend(peripheral_tools);
    }

    let provider_name = config
        .default_provider
        .as_deref()
        .unwrap_or("openrouter")
        .to_string();
    let model_name =
        resolve_default_model_id(config.default_model.as_deref(), Some(&provider_name));

    let provider_runtime_options = providers::ProviderRuntimeOptions {
        auth_profile_override: None,
        provider_api_url: config.api_url.clone(),
        provider_transport: config.effective_provider_transport(),
        zeroclaw_dir: config.config_path.parent().map(std::path::PathBuf::from),
        secrets_encrypt: config.secrets.encrypt,
        reasoning_enabled: config.runtime.reasoning_enabled,
        reasoning_level: config.effective_provider_reasoning_level(),
        custom_provider_api_mode: config.provider_api.map(|mode| mode.as_compatible_mode()),
        max_tokens_override: None,
        model_support_vision: config.model_support_vision,
    };
    let provider_box = providers::create_routed_provider_with_options(
        &provider_name,
        config.api_key.as_deref(),
        config.api_url.as_deref(),
        &config.reliability,
        &config.model_routes,
        &model_name,
        &provider_runtime_options,
    )?;
    let provider: Arc<dyn Provider> = Arc::from(provider_box);

    let tool_descs_owned: Vec<(String, String)> = tools_registry
        .iter()
        .map(|tool| (tool.name().to_string(), tool.description().to_string()))
        .collect();
    let tool_descs: Vec<(&str, &str)> = tool_descs_owned
        .iter()
        .map(|(name, desc)| (name.as_str(), desc.as_str()))
        .collect();
    let skills = crate::skills::load_skills_with_config(&config.workspace_dir, config);
    let bootstrap_max_chars = if config.agent.compact_context {
        Some(6000)
    } else {
        None
    };
    let native_tools = provider.supports_native_tools();
    let mut system_prompt = crate::channels::build_system_prompt_with_mode(
        &config.workspace_dir,
        &model_name,
        &tool_descs,
        &skills,
        Some(&config.identity),
        bootstrap_max_chars,
        native_tools,
        config.skills.prompt_injection_mode,
    );
    if !native_tools {
        system_prompt.push_str(&build_tool_instructions(&tools_registry));
    }
    system_prompt.push_str(&build_shell_policy_instructions(&config.autonomy));

    let history = vec![ChatMessage::system(system_prompt)];
    let hooks = crate::hooks::create_runner_from_config(&config.hooks);
    let safety_heartbeat = if config.agent.safety_heartbeat_interval > 0 {
        Some(SafetyHeartbeatConfig {
            body: security.summary_for_heartbeat(),
            interval: config.agent.safety_heartbeat_interval,
        })
    } else {
        None
    };
    let cost_enforcement = create_cost_enforcement_context(&config.cost, &config.workspace_dir);

    Ok(TuiRuntimeContext {
        provider_name,
        model_name,
        provider,
        observer,
        tools_registry: Arc::new(tools_registry),
        multimodal: config.multimodal.clone(),
        max_tool_iterations: config.agent.max_tool_iterations,
        hooks,
        safety_heartbeat,
        cost_enforcement,
        temperature: config.default_temperature,
        history,
    })
}
