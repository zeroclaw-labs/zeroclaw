use crate::config::Config;
use crate::memory::{self, Memory, MemoryCategory};
use crate::observability::{self, Observer, ObserverEvent};
use crate::providers::{self, Provider};
use crate::runtime;
use crate::security::SecurityPolicy;
use anyhow::Result;
use std::sync::Arc;
use std::time::Instant;

#[allow(clippy::too_many_lines)]
pub async fn run(
    config: Config,
    message: Option<String>,
    provider_override: Option<String>,
    model_override: Option<String>,
    temperature: f64,
) -> Result<()> {
    // ── Wire up agnostic subsystems ──────────────────────────────
    let observer: Arc<dyn Observer> =
        Arc::from(observability::create_observer(&config.observability));
    let _runtime = runtime::create_runtime(&config.runtime)?;
    let security = Arc::new(SecurityPolicy::from_config(
        &config.autonomy,
        &config.workspace_dir,
    ));

    // ── Memory (the brain) ────────────────────────────────────────
    let mem: Arc<dyn Memory> = Arc::from(memory::create_memory(
        &config.memory,
        &config.workspace_dir,
        config.api_key.as_deref(),
    )?);
    tracing::info!(backend = mem.name(), "Memory initialized");

    // ── Tool registry context ─────────────────────────────────────
    let composio_key = if config.composio.enabled {
        config.composio.api_key.as_deref()
    } else {
        None
    };
    let registry_db = crate::aria::db::AriaDb::open(&config.registry_db_path())?;
    let tenant = crate::tenant::resolve_tenant_from_token(&registry_db, "");

    // ── Resolve provider ─────────────────────────────────────────
    let provider_name = provider_override
        .as_deref()
        .or(config.default_provider.as_deref())
        .unwrap_or("openrouter");

    let model_name = model_override
        .as_deref()
        .or(config.default_model.as_deref())
        .unwrap_or("anthropic/claude-sonnet-4-20250514");

    let provider: Box<dyn Provider> = providers::create_resilient_provider(
        provider_name,
        config.api_key.as_deref(),
        &config.reliability,
    )?;

    observer.record_event(&ObserverEvent::AgentStart {
        provider: provider_name.to_string(),
        model: model_name.to_string(),
    });

    // ── Execute ──────────────────────────────────────────────────
    let start = Instant::now();

    if let Some(msg) = message {
        // Auto-save user message to memory
        if config.memory.auto_save {
            let _ = mem
                .store("user_msg", &msg, MemoryCategory::Conversation)
                .await;
        }

        let result = super::orchestrator::run_live_turn(
            super::orchestrator::LiveTurnConfig {
                provider: provider.as_ref(),
                security: &security,
                memory: mem.clone(),
                composio_api_key: composio_key,
                browser_config: &config.browser,
                registry_db: &registry_db,
                workspace_dir: &config.workspace_dir,
                tenant_id: &tenant,
                model: model_name,
                temperature,
                mode_hint: "",
                max_turns: Some(25),
                external_tool_context: None,
            },
            &msg,
            None,
        )
        .await?;

        println!("{}", result.output);

        if result.tool_calls > 0 {
            tracing::info!(
                turns = result.turns,
                tool_calls = result.tool_calls,
                duration_ms = result.duration_ms,
                "Agent completed with tool use"
            );
        }

        // Auto-save assistant response to daily log
        if config.memory.auto_save {
            let summary = if result.output.len() > 100 {
                format!("{}...", &result.output[..100])
            } else {
                result.output.clone()
            };
            let _ = mem
                .store("assistant_resp", &summary, MemoryCategory::Daily)
                .await;
        }
    } else {
        println!("🦀 Aria Interactive Mode (agentic)");
        println!("Type /quit to exit.\n");

        let (tx, mut rx) = tokio::sync::mpsc::channel(32);
        let cli = crate::channels::CliChannel::new();

        // Spawn listener
        let listen_handle = tokio::spawn(async move {
            let _ = crate::channels::Channel::listen(&cli, tx).await;
        });

        while let Some(msg) = rx.recv().await {
            // Auto-save conversation turns
            if config.memory.auto_save {
                let _ = mem
                    .store("user_msg", &msg.content, MemoryCategory::Conversation)
                    .await;
            }

            let result = super::orchestrator::run_live_turn(
                super::orchestrator::LiveTurnConfig {
                    provider: provider.as_ref(),
                    security: &security,
                    memory: mem.clone(),
                    composio_api_key: composio_key,
                    browser_config: &config.browser,
                    registry_db: &registry_db,
                    workspace_dir: &config.workspace_dir,
                    tenant_id: &tenant,
                    model: model_name,
                    temperature,
                    mode_hint: "",
                    max_turns: Some(25),
                    external_tool_context: None,
                },
                &msg.content,
                None,
            )
            .await?;

            println!("\n{}\n", result.output);

            if result.tool_calls > 0 {
                tracing::info!(
                    turns = result.turns,
                    tool_calls = result.tool_calls,
                    duration_ms = result.duration_ms,
                    "Agent turn completed with tool use"
                );
            }

            if config.memory.auto_save {
                let summary = if result.output.len() > 100 {
                    format!("{}...", &result.output[..100])
                } else {
                    result.output.clone()
                };
                let _ = mem
                    .store("assistant_resp", &summary, MemoryCategory::Daily)
                    .await;
            }
        }

        listen_handle.abort();
    }

    let duration = start.elapsed();
    observer.record_event(&ObserverEvent::AgentEnd {
        duration,
        tokens_used: None,
    });

    Ok(())
}
