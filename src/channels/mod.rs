//! Channel subsystem — messaging platform adapters.
//!
//! Each channel implements the [`Channel`] trait defined in [`traits`].
//! Augusta ships with the CLI channel (stdin/stdout) and the iMessage channel
//! (macOS native, polls chat.db).

pub mod claude_sdk;
pub mod cli;
pub mod imessage;
pub mod traits;

pub use cli::CliChannel;
pub use imessage::IMessageChannel;
pub use traits::{Channel, ChannelMessage, SendMessage};

use crate::agent::loop_::{build_tool_instructions, run_tool_call_loop, scrub_credentials};
use crate::approval::ApprovalManager;
use crate::config::Config;
use crate::memory::{self, Memory};
use crate::observability;
use crate::providers::{self, ChatMessage, Provider};
use crate::runtime;
use crate::security::SecurityPolicy;
use crate::tools;
use anyhow::Result;
use std::sync::Arc;
use tracing::info;

/// Resolve which provider to use (config default or fallback)
fn resolved_default_provider(config: &Config) -> String {
    config
        .default_provider
        .clone()
        .unwrap_or_else(|| "anthropic".to_string())
}

/// Resolve which model to use
fn resolved_default_model(config: &Config) -> String {
    config
        .default_model
        .clone()
        .unwrap_or_else(|| "claude-sonnet-4-6".to_string())
}

/// Start the iMessage channel — polls macOS Messages chat.db for incoming
/// messages and sends replies via AppleScript. Requires Full Disk Access.
pub async fn start_imessage(config: Config) -> Result<()> {
    let imessage_config = config.channels_config.imessage.clone().ok_or_else(|| {
        anyhow::anyhow!("iMessage channel not configured (channels_config.imessage)")
    })?;

    let provider_name = resolved_default_provider(&config);
    let provider_runtime_options = providers::ProviderRuntimeOptions {
        auth_profile_override: None,
        provider_api_url: config.api_url.clone(),
        augusta_dir: config.config_path.parent().map(std::path::PathBuf::from),
        secrets_encrypt: config.secrets.encrypt,
        reasoning_enabled: config.runtime.reasoning_enabled,
        provider_timeout_secs: Some(config.provider_timeout_secs),
    };
    let provider: Arc<dyn Provider> = Arc::from(providers::create_resilient_provider_with_options(
        &provider_name,
        config.api_key.as_deref(),
        config.api_url.as_deref(),
        &config.reliability,
        &provider_runtime_options,
    )?);

    if let Err(e) = provider.warmup().await {
        tracing::warn!("Provider warmup failed (non-fatal): {e}");
    }

    let runtime: Arc<dyn runtime::RuntimeAdapter> =
        Arc::from(runtime::create_runtime(&config.runtime)?);
    let security = Arc::new(SecurityPolicy::from_config(
        &config.autonomy,
        &config.workspace_dir,
    ));
    let model = resolved_default_model(&config);
    let temperature = config.default_temperature;
    let mem: Arc<dyn Memory> = Arc::from(memory::create_memory(
        &config.memory,
        &config.workspace_dir,
        config.api_key.as_deref(),
    )?);

    let workspace = config.workspace_dir.clone();
    let tools_registry = Arc::new(tools::all_tools_with_runtime(
        Arc::new(config.clone()),
        &security,
        runtime,
        Arc::clone(&mem),
        &config.browser,
        &config.http_request,
        &config.web_fetch,
        &config.web_search,
        &workspace,
        &config.agents,
        config.api_key.as_deref(),
        &config,
    ));

    let native_tools = provider.supports_native_tools();
    let mut system_prompt = "You are LightWave Augusta, a local AI agent running on macOS.\n\
         You are communicating via iMessage. Keep responses concise and conversational.\n\
         Be direct. Execute tasks autonomously when possible."
        .to_string();
    if !native_tools {
        system_prompt.push_str(&build_tool_instructions(tools_registry.as_ref()));
    }

    let channel = IMessageChannel::new(imessage_config);

    // Verify health before starting
    if !channel.health_check().await {
        anyhow::bail!(
            "iMessage health check failed. Ensure Full Disk Access is granted \
             in System Settings > Privacy & Security > Full Disk Access."
        );
    }

    info!("Starting iMessage channel");
    info!("  Model:    {model}");
    info!("  Provider: {provider_name}");
    info!("  Tools:    {} registered", tools_registry.len());

    let channel: Arc<dyn Channel> = Arc::new(channel);
    let (tx, mut rx) = tokio::sync::mpsc::channel::<ChannelMessage>(64);

    // Spawn iMessage listener (polls chat.db)
    let channel_clone = Arc::clone(&channel);
    tokio::spawn(async move {
        if let Err(e) = channel_clone.listen(tx).await {
            tracing::error!("iMessage channel error: {e}");
        }
    });

    let observer = observability::create_observer(&config.observability);
    let approval = ApprovalManager::from_config(&config.autonomy);

    // Process messages via local agent
    while let Some(msg) = rx.recv().await {
        info!(sender = %msg.sender, "Processing iMessage");

        let mut history = vec![
            providers::ChatMessage::system(&system_prompt),
            providers::ChatMessage::user(&msg.content),
        ];

        let output = match crate::agent::loop_::run_tool_call_loop(
            provider.as_ref(),
            &mut history,
            tools_registry.as_ref(),
            &observer,
            &provider_name,
            &model,
            temperature,
            false,
            Some(&approval),
            "imessage",
            &config.multimodal,
            config.agent.max_tool_iterations,
            None,
            None,
            None,
            &[],
            &config.agent.tool_call_dedup_exempt,
        )
        .await
        {
            Ok(response) => crate::agent::loop_::scrub_credentials(&response),
            Err(e) => format!("Error: {e}"),
        };

        if let Err(e) = channel
            .send(&SendMessage::new(&output, &msg.reply_target))
            .await
        {
            tracing::error!(error = %e, "Failed to send iMessage reply");
        }
    }

    Ok(())
}

/// Start the CLI channel loop — the primary local interaction mode.
pub async fn start_cli(config: Config) -> Result<()> {
    let provider_name = resolved_default_provider(&config);
    let provider_runtime_options = providers::ProviderRuntimeOptions {
        auth_profile_override: None,
        provider_api_url: config.api_url.clone(),
        augusta_dir: config.config_path.parent().map(std::path::PathBuf::from),
        secrets_encrypt: config.secrets.encrypt,
        reasoning_enabled: config.runtime.reasoning_enabled,
        provider_timeout_secs: Some(config.provider_timeout_secs),
    };
    let provider: Arc<dyn Provider> = Arc::from(providers::create_resilient_provider_with_options(
        &provider_name,
        config.api_key.as_deref(),
        config.api_url.as_deref(),
        &config.reliability,
        &provider_runtime_options,
    )?);

    if let Err(e) = provider.warmup().await {
        tracing::warn!("Provider warmup failed (non-fatal): {e}");
    }

    let runtime: Arc<dyn runtime::RuntimeAdapter> =
        Arc::from(runtime::create_runtime(&config.runtime)?);
    let security = Arc::new(SecurityPolicy::from_config(
        &config.autonomy,
        &config.workspace_dir,
    ));
    let model = resolved_default_model(&config);
    let temperature = config.default_temperature;
    let mem: Arc<dyn Memory> = Arc::from(memory::create_memory(
        &config.memory,
        &config.workspace_dir,
        config.api_key.as_deref(),
    )?);

    let workspace = config.workspace_dir.clone();
    let tools_registry = Arc::new(tools::all_tools_with_runtime(
        Arc::new(config.clone()),
        &security,
        runtime,
        Arc::clone(&mem),
        &config.browser,
        &config.http_request,
        &config.web_fetch,
        &config.web_search,
        &workspace,
        &config.agents,
        config.api_key.as_deref(),
        &config,
    ));

    let native_tools = provider.supports_native_tools();
    let mut system_prompt = "You are LightWave Augusta, a local AI agent running on macOS.\n\
         You have access to shell, file, memory, browser, and desktop automation tools.\n\
         Be concise and direct. Execute tasks autonomously when possible."
        .to_string();
    if !native_tools {
        system_prompt.push_str(&build_tool_instructions(tools_registry.as_ref()));
    }

    println!("🦀 LightWave Augusta");
    println!("  Model:    {model}");
    println!("  Provider: {provider_name}");
    println!("  Tools:    {} registered", tools_registry.len());
    println!();

    let channel: Arc<dyn Channel> = Arc::new(CliChannel::new());
    let (tx, mut rx) = tokio::sync::mpsc::channel::<ChannelMessage>(64);

    // Spawn CLI listener
    let channel_clone = Arc::clone(&channel);
    tokio::spawn(async move {
        if let Err(e) = channel_clone.listen(tx).await {
            tracing::error!("CLI channel error: {e}");
        }
    });

    let observer = observability::create_observer(&config.observability);
    let approval = ApprovalManager::from_config(&config.autonomy);

    // Process messages
    while let Some(msg) = rx.recv().await {
        info!("Processing message from {}", msg.sender);

        let mut history = vec![
            ChatMessage::system(&system_prompt),
            ChatMessage::user(&msg.content),
        ];

        let result = run_tool_call_loop(
            provider.as_ref(),
            &mut history,
            tools_registry.as_ref(),
            &observer,
            &provider_name,
            &model,
            temperature,
            false,
            Some(&approval),
            "cli",
            &config.multimodal,
            config.agent.max_tool_iterations,
            None,
            None,
            None,
            &[],
            &config.agent.tool_call_dedup_exempt,
        )
        .await;

        match result {
            Ok(response) => {
                let clean = scrub_credentials(&response);
                channel
                    .send(&SendMessage::new(&clean, &msg.reply_target))
                    .await?;
            }
            Err(e) => {
                let error_msg = format!("Error: {e}");
                channel
                    .send(&SendMessage::new(&error_msg, &msg.reply_target))
                    .await?;
            }
        }
    }

    Ok(())
}
