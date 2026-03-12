use axum::{
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{Html, IntoResponse, Json},
};
use serde_json::{json, Value};

use super::AppState;

fn check_auth(headers: &HeaderMap, state: &AppState) -> Option<(StatusCode, Json<Value>)> {
    if !state.pairing.require_pairing() {
        return None;
    }
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let token = auth.strip_prefix("Bearer ").unwrap_or("");
    if !state.pairing.is_authenticated(token) {
        return Some((
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Unauthorized — pair first via POST /pair"})),
        ));
    }
    None
}

pub async fn handle_dashboard() -> impl IntoResponse {
    Html(DASHBOARD_HTML)
}

pub async fn handle_api_status(State(state): State<AppState>) -> impl IntoResponse {
    let config = state.config.lock();

    let channels: Vec<&str> = {
        let mut ch = Vec::new();
        if config.channels_config.telegram.is_some() {
            ch.push("telegram");
        }
        if config.channels_config.discord.is_some() {
            ch.push("discord");
        }
        if config.channels_config.slack.is_some() {
            ch.push("slack");
        }
        if config.channels_config.mattermost.is_some() {
            ch.push("mattermost");
        }
        if config.channels_config.matrix.is_some() {
            ch.push("matrix");
        }
        if config.channels_config.whatsapp.is_some() {
            ch.push("whatsapp");
        }
        if config.channels_config.webhook.is_some() {
            ch.push("webhook");
        }
        if config.channels_config.signal.is_some() {
            ch.push("signal");
        }
        if config.channels_config.email.is_some() {
            ch.push("email");
        }
        if config.channels_config.irc.is_some() {
            ch.push("irc");
        }
        if config.channels_config.imessage.is_some() {
            ch.push("imessage");
        }
        if config.channels_config.lark.is_some() {
            ch.push("lark");
        }
        if config.channels_config.dingtalk.is_some() {
            ch.push("dingtalk");
        }
        if config.channels_config.qq.is_some() {
            ch.push("qq");
        }
        ch
    };

    let tools_enabled: Vec<String> = config.autonomy.auto_approve.clone();

    let agent_names: Vec<String> = config.agents.keys().cloned().collect();

    Json(json!({
        "provider": config.default_provider,
        "model": config.default_model,
        "temperature": config.default_temperature,
        "memory_backend": format!("{}", config.memory.backend),
        "channels": channels,
        "channels_count": channels.len(),
        "tools_enabled": tools_enabled,
        "tools_count": tools_enabled.len(),
        "agents": agent_names,
        "agents_count": agent_names.len(),
        "gateway": {
            "host": &config.gateway.host,
            "port": config.gateway.port,
            "require_pairing": config.gateway.require_pairing,
        },
        "security": {
            "autonomy_level": format!("{:?}", config.autonomy.level),
            "sandbox_enabled": config.security.sandbox.enabled,
        },
        "identity": {
            "format": &config.identity.format,
            "aieos_path": &config.identity.aieos_path,
        },
    }))
}

pub async fn handle_api_channels(State(state): State<AppState>) -> impl IntoResponse {
    let config = state.config.lock();
    let cc = &config.channels_config;

    let channels = json!([
        {
            "name": "telegram",
            "label": "Telegram",
            "category": "messaging",
            "enabled": cc.telegram.is_some(),
            "required_keys": ["bot_token", "allowed_users"],
            "optional_keys": ["stream_mode", "mention_only", "voice"],
            "hint": "Create a bot via @BotFather, get the token, add allowed user IDs."
        },
        {
            "name": "discord",
            "label": "Discord",
            "category": "messaging",
            "enabled": cc.discord.is_some(),
            "required_keys": ["bot_token"],
            "optional_keys": ["guild_id", "allowed_users", "listen_to_bots", "mention_only"],
            "hint": "Create a bot at discord.com/developers, enable Message Content intent."
        },
        {
            "name": "slack",
            "label": "Slack",
            "category": "messaging",
            "enabled": cc.slack.is_some(),
            "required_keys": ["bot_token"],
            "optional_keys": ["app_token", "channel_id", "allowed_users"],
            "hint": "Create a Slack app, add Bot Token Scopes, install to workspace."
        },
        {
            "name": "mattermost",
            "label": "Mattermost",
            "category": "messaging",
            "enabled": cc.mattermost.is_some(),
            "required_keys": ["url", "bot_token"],
            "optional_keys": ["channel_id", "allowed_users", "thread_replies", "mention_only"],
            "hint": "Create a bot account in Mattermost, use the Personal Access Token."
        },
        {
            "name": "matrix",
            "label": "Matrix",
            "category": "messaging",
            "enabled": cc.matrix.is_some(),
            "required_keys": ["homeserver", "access_token", "room_id", "allowed_users"],
            "optional_keys": ["user_id", "device_id"],
            "hint": "Register a bot user, generate access token, invite to room."
        },
        {
            "name": "whatsapp",
            "label": "WhatsApp",
            "category": "messaging",
            "enabled": cc.whatsapp.is_some(),
            "required_keys": ["access_token", "phone_number_id", "verify_token"],
            "optional_keys": ["app_secret", "allowed_numbers"],
            "hint": "Set up Meta Business API, configure webhook URL to /whatsapp."
        },
        {
            "name": "signal",
            "label": "Signal",
            "category": "messaging",
            "enabled": cc.signal.is_some(),
            "required_keys": ["http_url", "account"],
            "optional_keys": ["group_id", "allowed_from", "ignore_attachments", "ignore_stories"],
            "hint": "Run signal-cli REST API daemon, register a phone number."
        },
        {
            "name": "email",
            "label": "Email",
            "category": "communication",
            "enabled": cc.email.is_some(),
            "required_keys": ["imap_host", "smtp_host", "username", "password"],
            "optional_keys": ["imap_port", "smtp_port", "allowed_senders", "folder"],
            "hint": "Configure IMAP for receiving and SMTP for sending email."
        },
        {
            "name": "irc",
            "label": "IRC",
            "category": "communication",
            "enabled": cc.irc.is_some(),
            "required_keys": ["server", "nickname"],
            "optional_keys": ["port", "channels", "allowed_users", "server_password", "nickserv_password", "sasl_password", "verify_tls"],
            "hint": "Connect to any IRC server. Default port 6697 (TLS)."
        },
        {
            "name": "webhook",
            "label": "Webhook",
            "category": "integration",
            "enabled": cc.webhook.is_some(),
            "required_keys": ["port"],
            "optional_keys": ["secret"],
            "hint": "Generic HTTP webhook. POST JSON to receive, configurable secret."
        },
        {
            "name": "imessage",
            "label": "iMessage",
            "category": "messaging",
            "enabled": cc.imessage.is_some(),
            "required_keys": ["allowed_contacts"],
            "optional_keys": [],
            "hint": "macOS only. Uses AppleScript to read/send iMessages."
        },
        {
            "name": "lark",
            "label": "Lark / Feishu",
            "category": "enterprise",
            "enabled": cc.lark.is_some(),
            "required_keys": ["app_id", "app_secret"],
            "optional_keys": ["encrypt_key", "verification_token", "allowed_users", "use_feishu", "receive_mode", "port"],
            "hint": "Create app in Lark/Feishu console. Supports WebSocket (default) or webhook."
        },
        {
            "name": "dingtalk",
            "label": "DingTalk",
            "category": "enterprise",
            "enabled": cc.dingtalk.is_some(),
            "required_keys": ["client_id", "client_secret"],
            "optional_keys": ["allowed_users"],
            "hint": "Create a DingTalk enterprise bot, get AppKey and AppSecret."
        },
        {
            "name": "qq",
            "label": "QQ",
            "category": "messaging",
            "enabled": cc.qq.is_some(),
            "required_keys": ["app_id", "app_secret"],
            "optional_keys": ["allowed_users"],
            "hint": "Register at Tencent QQ Bot developer console."
        }
    ]);

    let enabled_count = [
        cc.telegram.is_some(),
        cc.discord.is_some(),
        cc.slack.is_some(),
        cc.mattermost.is_some(),
        cc.matrix.is_some(),
        cc.whatsapp.is_some(),
        cc.signal.is_some(),
        cc.email.is_some(),
        cc.irc.is_some(),
        cc.webhook.is_some(),
        cc.imessage.is_some(),
        cc.lark.is_some(),
        cc.dingtalk.is_some(),
        cc.qq.is_some(),
    ]
    .iter()
    .filter(|&&e| e)
    .count();

    Json(json!({
        "channels": channels,
        "total": 14,
        "enabled": enabled_count,
        "cli_enabled": cc.cli,
    }))
}

pub async fn handle_api_system(State(state): State<AppState>) -> impl IntoResponse {
    let config = state.config.lock();

    let has_api_key = |key: &str| -> bool { std::env::var(key).is_ok() };

    let providers = json!([
        { "name": "anthropic", "label": "Anthropic", "category": "frontier", "enabled": has_api_key("ANTHROPIC_API_KEY") || has_api_key("ANTHROPIC_OAUTH_TOKEN"), "env_var": "ANTHROPIC_API_KEY", "hint": "Claude models. Get key at console.anthropic.com" },
        { "name": "openai", "label": "OpenAI", "category": "frontier", "enabled": has_api_key("OPENAI_API_KEY"), "env_var": "OPENAI_API_KEY", "hint": "GPT-4/o models. Get key at platform.openai.com" },
        { "name": "openrouter", "label": "OpenRouter", "category": "aggregator", "enabled": has_api_key("OPENROUTER_API_KEY"), "env_var": "OPENROUTER_API_KEY", "hint": "Multi-model gateway. Access 100+ models via openrouter.ai" },
        { "name": "ollama", "label": "Ollama", "category": "local", "enabled": has_api_key("OLLAMA_API_KEY") || config.default_provider.as_deref() == Some("ollama"), "env_var": "OLLAMA_API_KEY", "hint": "Local models. Run ollama serve, pull models with ollama pull" },
        { "name": "gemini", "label": "Google Gemini", "category": "frontier", "enabled": has_api_key("GOOGLE_API_KEY") || has_api_key("GEMINI_API_KEY"), "env_var": "GOOGLE_API_KEY", "hint": "Gemini models. Get key at aistudio.google.com" },
        { "name": "groq", "label": "Groq", "category": "inference", "enabled": has_api_key("GROQ_API_KEY"), "env_var": "GROQ_API_KEY", "hint": "Ultra-fast inference. Get key at console.groq.com" },
        { "name": "mistral", "label": "Mistral", "category": "frontier", "enabled": has_api_key("MISTRAL_API_KEY"), "env_var": "MISTRAL_API_KEY", "hint": "Mistral/Mixtral models. Get key at console.mistral.ai" },
        { "name": "xai", "label": "xAI (Grok)", "category": "frontier", "enabled": has_api_key("XAI_API_KEY"), "env_var": "XAI_API_KEY", "hint": "Grok models. Get key at console.x.ai" },
        { "name": "deepseek", "label": "DeepSeek", "category": "frontier", "enabled": has_api_key("DEEPSEEK_API_KEY"), "env_var": "DEEPSEEK_API_KEY", "hint": "DeepSeek V3/R1 models. Get key at platform.deepseek.com" },
        { "name": "together", "label": "Together AI", "category": "inference", "enabled": has_api_key("TOGETHER_API_KEY"), "env_var": "TOGETHER_API_KEY", "hint": "Open-source model hosting. Get key at api.together.ai" },
        { "name": "fireworks", "label": "Fireworks AI", "category": "inference", "enabled": has_api_key("FIREWORKS_API_KEY"), "env_var": "FIREWORKS_API_KEY", "hint": "Fast open-source inference. Get key at fireworks.ai" },
        { "name": "perplexity", "label": "Perplexity", "category": "search", "enabled": has_api_key("PERPLEXITY_API_KEY"), "env_var": "PERPLEXITY_API_KEY", "hint": "Search-augmented models. Get key at perplexity.ai" },
        { "name": "cohere", "label": "Cohere", "category": "frontier", "enabled": has_api_key("COHERE_API_KEY"), "env_var": "COHERE_API_KEY", "hint": "Command-R models. Get key at dashboard.cohere.com" },
        { "name": "copilot", "label": "GitHub Copilot", "category": "aggregator", "enabled": has_api_key("GITHUB_TOKEN"), "env_var": "GITHUB_TOKEN", "hint": "Use Copilot API with GitHub token" },
        { "name": "minimax", "label": "MiniMax", "category": "china", "enabled": has_api_key("MINIMAX_API_KEY"), "env_var": "MINIMAX_API_KEY", "hint": "MiniMax models (international and CN)" },
        { "name": "glm", "label": "GLM / Zhipu", "category": "china", "enabled": has_api_key("GLM_API_KEY") || has_api_key("ZHIPU_API_KEY"), "env_var": "GLM_API_KEY", "hint": "GLM-4 models from Zhipu AI" },
        { "name": "moonshot", "label": "Moonshot / Kimi", "category": "china", "enabled": has_api_key("MOONSHOT_API_KEY"), "env_var": "MOONSHOT_API_KEY", "hint": "Kimi models from Moonshot AI" },
        { "name": "qwen", "label": "Qwen", "category": "china", "enabled": has_api_key("QWEN_API_KEY") || has_api_key("DASHSCOPE_API_KEY"), "env_var": "QWEN_API_KEY", "hint": "Qwen models from Alibaba (CN/Intl/US)" },
        { "name": "zai", "label": "ZAI / 01.AI", "category": "china", "enabled": has_api_key("ZAI_API_KEY"), "env_var": "ZAI_API_KEY", "hint": "Yi models from 01.AI" },
        { "name": "qianfan", "label": "Qianfan / Baidu", "category": "china", "enabled": has_api_key("QIANFAN_API_KEY"), "env_var": "QIANFAN_API_KEY", "hint": "ERNIE models from Baidu" },
        { "name": "codex", "label": "OpenAI Codex", "category": "inference", "enabled": has_api_key("OPENAI_API_KEY"), "env_var": "OPENAI_API_KEY", "hint": "Codex/code models via OpenAI API" }
    ]);

    let tools = json!([
        { "name": "shell", "category": "system", "hint": "Execute shell commands" },
        { "name": "file_read", "category": "system", "hint": "Read file contents" },
        { "name": "file_write", "category": "system", "hint": "Write/create files" },
        { "name": "memory_store", "category": "memory", "hint": "Store key-value in memory" },
        { "name": "memory_recall", "category": "memory", "hint": "Recall from memory by query" },
        { "name": "memory_forget", "category": "memory", "hint": "Delete a memory entry" },
        { "name": "browser", "category": "browser", "hint": "Browse and extract web content" },
        { "name": "browser_open", "category": "browser", "hint": "Open URL in browser" },
        { "name": "screenshot", "category": "browser", "hint": "Take browser screenshot" },
        { "name": "http_request", "category": "network", "hint": "Make HTTP requests" },
        { "name": "web_search", "category": "network", "hint": "Search the web (DDG/Brave)" },
        { "name": "git_operations", "category": "system", "hint": "Git commands (status, diff, log)" },
        { "name": "schedule", "category": "scheduling", "hint": "Schedule one-time tasks" },
        { "name": "cron_add", "category": "scheduling", "hint": "Add recurring cron job" },
        { "name": "cron_list", "category": "scheduling", "hint": "List all cron jobs" },
        { "name": "cron_remove", "category": "scheduling", "hint": "Remove a cron job" },
        { "name": "cron_run", "category": "scheduling", "hint": "Manually trigger a cron job" },
        { "name": "cron_runs", "category": "scheduling", "hint": "View cron run history" },
        { "name": "cron_update", "category": "scheduling", "hint": "Update an existing cron job" },
        { "name": "wallet_info", "category": "wallet", "hint": "Get wallet address and info" },
        { "name": "wallet_balance", "category": "wallet", "hint": "Check wallet balance" },
        { "name": "wallet_send", "category": "wallet", "hint": "Send native token" },
        { "name": "wallet_pay", "category": "wallet", "hint": "Pay to address or ENS" },
        { "name": "wallet_sign", "category": "wallet", "hint": "Sign a message" },
        { "name": "wallet_token_balance", "category": "wallet", "hint": "Check ERC-20 token balance" },
        { "name": "wallet_token_send", "category": "wallet", "hint": "Send ERC-20 tokens" },
        { "name": "hardware_board_info", "category": "hardware", "hint": "Get connected board info" },
        { "name": "hardware_memory_read", "category": "hardware", "hint": "Read memory from MCU via probe" },
        { "name": "hardware_memory_map", "category": "hardware", "hint": "Show MCU memory map" },
        { "name": "delegate", "category": "agent", "hint": "Delegate task to sub-agent" },
        { "name": "composio", "category": "integration", "hint": "Run Composio actions" },
        { "name": "pushover", "category": "integration", "hint": "Send push notifications" },
        { "name": "proxy_config", "category": "system", "hint": "View/update proxy settings" },
        { "name": "soul_status", "category": "soul", "hint": "Get soul/identity status" },
        { "name": "soul_reflect", "category": "soul", "hint": "Trigger self-reflection" },
        { "name": "soul_replicate", "category": "soul", "hint": "Replicate soul to another instance" },
        { "name": "image_info", "category": "media", "hint": "Analyze image metadata" }
    ]);

    let memory_backend = config.memory.backend.clone();
    let memory_backends = json!([
        { "name": "sqlite", "label": "SQLite", "enabled": memory_backend == "sqlite", "hint": "Default. Local file-based, zero setup." },
        { "name": "lucid", "label": "Lucid", "enabled": memory_backend == "lucid", "hint": "High-performance embedded engine with vector search." },
        { "name": "postgres", "label": "PostgreSQL", "enabled": memory_backend == "postgres", "hint": "Production-grade. Requires DATABASE_URL env var." },
        { "name": "markdown", "label": "Markdown", "enabled": memory_backend == "markdown", "hint": "File-based markdown storage. Human-readable." },
        { "name": "none", "label": "None", "enabled": memory_backend == "none", "hint": "No persistent memory. Stateless operation." }
    ]);

    let observer_type = config.observability.backend.clone();
    let observers = json!([
        { "name": "noop", "label": "Noop", "enabled": observer_type == "noop" || observer_type == "none", "hint": "No observability. Silent operation." },
        { "name": "log", "label": "Log", "enabled": observer_type == "log", "hint": "Structured logging to stdout/file." },
        { "name": "prometheus", "label": "Prometheus", "enabled": observer_type == "prometheus", "hint": "Exposes /metrics endpoint for scraping." },
        { "name": "otel", "label": "OpenTelemetry", "enabled": observer_type == "otel" || observer_type == "opentelemetry", "hint": "OTLP export. Set OTEL_EXPORTER_OTLP_ENDPOINT." },
        { "name": "verbose", "label": "Verbose", "enabled": observer_type == "verbose", "hint": "Detailed debug output for development." },
        { "name": "selfhealth", "label": "Self-Health", "enabled": observer_type == "selfhealth", "hint": "Internal health monitoring and alerts." }
    ]);

    let runtime_type = config.runtime.kind.clone();
    let runtimes = json!([
        { "name": "native", "label": "Native", "enabled": runtime_type == "native", "hint": "Direct host execution. Default and fastest." },
        { "name": "docker", "label": "Docker", "enabled": runtime_type == "docker", "hint": "Container isolation. Requires Docker daemon." },
        { "name": "wasm", "label": "WebAssembly", "enabled": runtime_type == "wasm", "hint": "Sandboxed WASM runtime. Experimental." }
    ]);

    let autonomy_level = format!("{:?}", config.autonomy.level);
    let security = json!({
        "autonomy_level": &autonomy_level,
        "workspace_only": config.autonomy.workspace_only,
        "sandbox_enabled": config.security.sandbox.enabled,
        "auto_approve": &config.autonomy.auto_approve,
        "levels": [
            { "name": "ReadOnly", "label": "Read-Only", "active": autonomy_level == "ReadOnly", "hint": "Agent can only read. No writes, no commands." },
            { "name": "Supervised", "label": "Supervised", "active": autonomy_level == "Supervised", "hint": "Default. Agent asks before risky actions." },
            { "name": "Full", "label": "Full Autonomy", "active": autonomy_level == "Full", "hint": "Agent acts without confirmation. Use with caution." }
        ]
    });

    let tunnel_provider = &config.tunnel.provider;
    let tunnels = json!([
        { "name": "none", "label": "None", "enabled": tunnel_provider == "none", "hint": "No tunnel. Direct access only." },
        { "name": "cloudflare", "label": "Cloudflare Tunnel", "enabled": tunnel_provider == "cloudflare" || config.tunnel.cloudflare.is_some(), "required_keys": ["token"], "hint": "Zero Trust tunnel. Get token from CF dashboard." },
        { "name": "tailscale", "label": "Tailscale", "enabled": tunnel_provider == "tailscale" || config.tunnel.tailscale.is_some(), "required_keys": [], "optional_keys": ["funnel", "hostname"], "hint": "Mesh VPN. Optional Funnel for public access." },
        { "name": "ngrok", "label": "ngrok", "enabled": tunnel_provider == "ngrok" || config.tunnel.ngrok.is_some(), "required_keys": ["auth_token"], "optional_keys": ["domain"], "hint": "Instant public URLs. Get token at ngrok.com" },
        { "name": "custom", "label": "Custom", "enabled": tunnel_provider == "custom" || config.tunnel.custom.is_some(), "required_keys": ["start_command"], "hint": "Any tunnel via command template. Use {port} placeholder." }
    ]);

    let active_provider = &config.default_provider;
    let active_model = &config.default_model;

    Json(json!({
        "providers": { "items": providers, "active": active_provider, "active_model": active_model },
        "channels": { "total": 14, "items": "see /api/channels" },
        "tools": { "items": tools, "total": 37 },
        "memory": { "items": memory_backends, "active": memory_backend },
        "observers": { "items": observers, "active": observer_type },
        "runtimes": { "items": runtimes, "active": runtime_type },
        "security": security,
        "tunnels": { "items": tunnels, "active": tunnel_provider },
    }))
}

pub async fn handle_api_config(State(state): State<AppState>) -> impl IntoResponse {
    let config = state.config.lock();

    let mut config_json = serde_json::to_value(&*config).unwrap_or(json!({}));

    if let Some(obj) = config_json.as_object_mut() {
        obj.remove("api_key");
        if let Some(channels) = obj.get_mut("channels_config") {
            if let Some(ch_obj) = channels.as_object_mut() {
                for (_name, ch_config) in ch_obj.iter_mut() {
                    if let Some(ch) = ch_config.as_object_mut() {
                        for field in SECRET_FIELDS {
                            ch.remove(*field);
                        }
                    }
                }
            }
        }
    }

    Json(config_json)
}

pub async fn handle_api_memories(State(state): State<AppState>) -> impl IntoResponse {
    match state.mem.list(None, None).await {
        Ok(entries) => {
            let items: Vec<serde_json::Value> = entries
                .iter()
                .map(|e| {
                    json!({
                        "key": e.key,
                        "content": if e.content.len() > 200 {
                            format!("{}...", &e.content[..200])
                        } else {
                            e.content.clone()
                        },
                        "category": format!("{:?}", e.category),
                        "timestamp": &e.timestamp,
                    })
                })
                .collect();
            (
                StatusCode::OK,
                Json(json!({ "count": items.len(), "entries": items })),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub async fn handle_api_metrics(State(state): State<AppState>) -> impl IntoResponse {
    if let Some(prom) = state
        .observer
        .as_any()
        .downcast_ref::<crate::observability::PrometheusObserver>()
    {
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/plain")],
            prom.encode(),
        )
            .into_response()
    } else {
        (
            StatusCode::OK,
            Json(json!({ "message": "Prometheus observer not active" })),
        )
            .into_response()
    }
}

pub async fn handle_admin_provider(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if let Some(err) = check_auth(&headers, &state) {
        return err;
    }
    let mut config = state.config.lock();
    if let Some(provider) = body.get("provider").and_then(|v| v.as_str()) {
        if !VALID_PROVIDERS.contains(&provider) {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("Unknown provider: {provider}")})),
            );
        }
        config.default_provider = Some(provider.to_string());
    }
    if let Some(model) = body.get("model").and_then(|v| v.as_str()) {
        config.default_model = Some(model.to_string());
    }
    match config.save() {
        Ok(()) => (
            StatusCode::OK,
            Json(json!({"ok": true, "restart_required": true})),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to save: {e}")})),
        ),
    }
}

pub async fn handle_admin_channel(
    headers: HeaderMap,
    Path(name): Path<String>,
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if let Some(err) = check_auth(&headers, &state) {
        return err;
    }
    let mut config = state.config.lock();
    let cc = &mut config.channels_config;
    let result = match name.as_str() {
        "cli" => {
            cc.cli = body
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            Ok(false)
        }
        "telegram" => {
            if let Some(obj) = body.as_object() {
                let val = serde_json::to_value(obj).unwrap_or_default();
                match serde_json::from_value(val) {
                    Ok(c) => {
                        cc.telegram = Some(c);
                        Ok(true)
                    }
                    Err(e) => Err(format!("Invalid telegram config: {e}")),
                }
            } else {
                Err("Expected JSON object".into())
            }
        }
        "discord" => {
            if let Some(obj) = body.as_object() {
                let val = serde_json::to_value(obj).unwrap_or_default();
                match serde_json::from_value(val) {
                    Ok(c) => {
                        cc.discord = Some(c);
                        Ok(true)
                    }
                    Err(e) => Err(format!("Invalid discord config: {e}")),
                }
            } else {
                Err("Expected JSON object".into())
            }
        }
        "slack" => {
            if let Some(obj) = body.as_object() {
                let val = serde_json::to_value(obj).unwrap_or_default();
                match serde_json::from_value(val) {
                    Ok(c) => {
                        cc.slack = Some(c);
                        Ok(true)
                    }
                    Err(e) => Err(format!("Invalid slack config: {e}")),
                }
            } else {
                Err("Expected JSON object".into())
            }
        }
        "whatsapp" => {
            if let Some(obj) = body.as_object() {
                let val = serde_json::to_value(obj).unwrap_or_default();
                match serde_json::from_value(val) {
                    Ok(c) => {
                        cc.whatsapp = Some(c);
                        Ok(true)
                    }
                    Err(e) => Err(format!("Invalid whatsapp config: {e}")),
                }
            } else {
                Err("Expected JSON object".into())
            }
        }
        "webhook" => {
            if let Some(obj) = body.as_object() {
                let val = serde_json::to_value(obj).unwrap_or_default();
                match serde_json::from_value(val) {
                    Ok(c) => {
                        cc.webhook = Some(c);
                        Ok(true)
                    }
                    Err(e) => Err(format!("Invalid webhook config: {e}")),
                }
            } else {
                Err("Expected JSON object".into())
            }
        }
        "matrix" => {
            if let Some(obj) = body.as_object() {
                let val = serde_json::to_value(obj).unwrap_or_default();
                match serde_json::from_value(val) {
                    Ok(c) => {
                        cc.matrix = Some(c);
                        Ok(true)
                    }
                    Err(e) => Err(format!("Invalid matrix config: {e}")),
                }
            } else {
                Err("Expected JSON object".into())
            }
        }
        "mattermost" => {
            if let Some(obj) = body.as_object() {
                let val = serde_json::to_value(obj).unwrap_or_default();
                match serde_json::from_value(val) {
                    Ok(c) => {
                        cc.mattermost = Some(c);
                        Ok(true)
                    }
                    Err(e) => Err(format!("Invalid mattermost config: {e}")),
                }
            } else {
                Err("Expected JSON object".into())
            }
        }
        "signal" => {
            if let Some(obj) = body.as_object() {
                let val = serde_json::to_value(obj).unwrap_or_default();
                match serde_json::from_value(val) {
                    Ok(c) => {
                        cc.signal = Some(c);
                        Ok(true)
                    }
                    Err(e) => Err(format!("Invalid signal config: {e}")),
                }
            } else {
                Err("Expected JSON object".into())
            }
        }
        "email" => {
            if let Some(obj) = body.as_object() {
                let val = serde_json::to_value(obj).unwrap_or_default();
                match serde_json::from_value(val) {
                    Ok(c) => {
                        cc.email = Some(c);
                        Ok(true)
                    }
                    Err(e) => Err(format!("Invalid email config: {e}")),
                }
            } else {
                Err("Expected JSON object".into())
            }
        }
        "irc" => {
            if let Some(obj) = body.as_object() {
                let val = serde_json::to_value(obj).unwrap_or_default();
                match serde_json::from_value(val) {
                    Ok(c) => {
                        cc.irc = Some(c);
                        Ok(true)
                    }
                    Err(e) => Err(format!("Invalid irc config: {e}")),
                }
            } else {
                Err("Expected JSON object".into())
            }
        }
        "imessage" => {
            if let Some(obj) = body.as_object() {
                let val = serde_json::to_value(obj).unwrap_or_default();
                match serde_json::from_value(val) {
                    Ok(c) => {
                        cc.imessage = Some(c);
                        Ok(true)
                    }
                    Err(e) => Err(format!("Invalid imessage config: {e}")),
                }
            } else {
                Err("Expected JSON object".into())
            }
        }
        "lark" => {
            if let Some(obj) = body.as_object() {
                let val = serde_json::to_value(obj).unwrap_or_default();
                match serde_json::from_value(val) {
                    Ok(c) => {
                        cc.lark = Some(c);
                        Ok(true)
                    }
                    Err(e) => Err(format!("Invalid lark config: {e}")),
                }
            } else {
                Err("Expected JSON object".into())
            }
        }
        other => Err(format!("Unknown channel: {other}")),
    };
    match result {
        Ok(restart) => match config.save() {
            Ok(()) => (
                StatusCode::OK,
                Json(json!({"ok": true, "restart_required": restart})),
            ),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("Failed to save: {e}")})),
            ),
        },
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"error": e}))),
    }
}

pub async fn handle_admin_channel_delete(
    headers: HeaderMap,
    Path(name): Path<String>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    if let Some(err) = check_auth(&headers, &state) {
        return err;
    }
    let mut config = state.config.lock();
    let cc = &mut config.channels_config;
    match name.as_str() {
        "telegram" => cc.telegram = None,
        "discord" => cc.discord = None,
        "slack" => cc.slack = None,
        "whatsapp" => cc.whatsapp = None,
        "webhook" => cc.webhook = None,
        "matrix" => cc.matrix = None,
        "mattermost" => cc.mattermost = None,
        "signal" => cc.signal = None,
        "email" => cc.email = None,
        "irc" => cc.irc = None,
        "imessage" => cc.imessage = None,
        "lark" => cc.lark = None,
        other => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("Unknown channel: {other}")})),
            );
        }
    }
    match config.save() {
        Ok(()) => (
            StatusCode::OK,
            Json(json!({"ok": true, "restart_required": true})),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to save: {e}")})),
        ),
    }
}

pub async fn handle_admin_memory(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if let Some(err) = check_auth(&headers, &state) {
        return err;
    }
    let mut config = state.config.lock();
    if let Some(backend) = body.get("backend").and_then(|v| v.as_str()) {
        if !VALID_MEMORY_BACKENDS.contains(&backend) {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("Unknown memory backend: {backend}")})),
            );
        }
        config.memory.backend = backend.to_string();
    }
    if let Some(auto_save) = body.get("auto_save").and_then(|v| v.as_bool()) {
        config.memory.auto_save = auto_save;
    }
    match config.save() {
        Ok(()) => (
            StatusCode::OK,
            Json(json!({"ok": true, "restart_required": true})),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to save: {e}")})),
        ),
    }
}

pub async fn handle_admin_observer(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if let Some(err) = check_auth(&headers, &state) {
        return err;
    }
    let mut config = state.config.lock();
    if let Some(backend) = body.get("backend").and_then(|v| v.as_str()) {
        if !VALID_OBSERVER_BACKENDS.contains(&backend) {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("Unknown observer backend: {backend}")})),
            );
        }
        config.observability.backend = backend.to_string();
    }
    if let Some(ep) = body.get("otel_endpoint").and_then(|v| v.as_str()) {
        config.observability.otel_endpoint = Some(ep.to_string());
    }
    match config.save() {
        Ok(()) => (
            StatusCode::OK,
            Json(json!({"ok": true, "restart_required": true})),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to save: {e}")})),
        ),
    }
}

pub async fn handle_admin_runtime(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if let Some(err) = check_auth(&headers, &state) {
        return err;
    }
    let mut config = state.config.lock();
    if let Some(kind) = body.get("kind").and_then(|v| v.as_str()) {
        if !VALID_RUNTIME_KINDS.contains(&kind) {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("Unknown runtime kind: {kind}")})),
            );
        }
        config.runtime.kind = kind.to_string();
    }
    match config.save() {
        Ok(()) => (
            StatusCode::OK,
            Json(json!({"ok": true, "restart_required": true})),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to save: {e}")})),
        ),
    }
}

pub async fn handle_admin_security(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if let Some(err) = check_auth(&headers, &state) {
        return err;
    }
    let mut config = state.config.lock();
    if let Some(level) = body.get("level").and_then(|v| v.as_str()) {
        config.autonomy.level = match level {
            "ReadOnly" | "readonly" => crate::security::AutonomyLevel::ReadOnly,
            "Supervised" | "supervised" => crate::security::AutonomyLevel::Supervised,
            "Full" | "full" => crate::security::AutonomyLevel::Full,
            other => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": format!("Unknown autonomy level: {other}")})),
                );
            }
        };
    }
    if let Some(ws) = body.get("workspace_only").and_then(|v| v.as_bool()) {
        config.autonomy.workspace_only = ws;
    }
    if let Some(arr) = body.get("auto_approve").and_then(|v| v.as_array()) {
        config.autonomy.auto_approve = arr
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();
    }
    match config.save() {
        Ok(()) => (
            StatusCode::OK,
            Json(json!({"ok": true, "restart_required": true})),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to save: {e}")})),
        ),
    }
}

pub async fn handle_admin_tunnel(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if let Some(err) = check_auth(&headers, &state) {
        return err;
    }
    let mut config = state.config.lock();
    if let Some(provider) = body.get("provider").and_then(|v| v.as_str()) {
        if !VALID_TUNNEL_PROVIDERS.contains(&provider) {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("Unknown tunnel provider: {provider}")})),
            );
        }
        config.tunnel.provider = provider.to_string();
    }
    match config.save() {
        Ok(()) => (
            StatusCode::OK,
            Json(json!({"ok": true, "restart_required": true})),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to save: {e}")})),
        ),
    }
}

const DASHBOARD_HTML: &str = r##"
<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>ZeroClaw Dashboard</title>
<link rel="preconnect" href="https://fonts.googleapis.com">
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
<link href="https://fonts.googleapis.com/css2?family=IBM+Plex+Sans:wght@300;400;500;600;700&family=JetBrains+Mono:wght@400;500&family=Sora:wght@400;500;600;700&display=swap" rel="stylesheet">
<style>
*,*::before,*::after{box-sizing:border-box;margin:0;padding:0}
:root{
--bg:#0a1222;--surface:#111827;--surface-muted:#1e293b;--border:#1e3a5f;
--text:#e2e8f0;--text-muted:#94a3b8;--accent:#3b82f6;--success:#22c55e;
--warning:#eab308;--danger:#ef4444;--sidebar-w:260px;--radius:8px;
--shadow:0 1px 3px rgba(0,0,0,.3),0 1px 2px rgba(0,0,0,.2);
--shadow-lg:0 10px 15px -3px rgba(0,0,0,.4),0 4px 6px rgba(0,0,0,.3);
}
html{font-size:14px;scroll-behavior:smooth}
body{font-family:'IBM Plex Sans',sans-serif;background:var(--bg);color:var(--text);min-height:100vh;overflow-x:hidden}
h1,h2,h3,h4,h5,h6{font-family:'Sora',sans-serif;font-weight:600}
code,pre,.mono{font-family:'JetBrains Mono',monospace}
a{color:var(--accent);text-decoration:none}

@keyframes fadeInUp{from{opacity:0;transform:translateY(12px)}to{opacity:1;transform:translateY(0)}}
@keyframes pulse{0%,100%{opacity:1}50%{opacity:.4}}
@keyframes shimmer{0%{background-position:-200% 0}100%{background-position:200% 0}}
.fade-in-up{animation:fadeInUp .6s ease both}
.pulse{animation:pulse 2s ease-in-out infinite}
.shimmer{background:linear-gradient(90deg,var(--surface-muted) 25%,var(--border) 50%,var(--surface-muted) 75%);background-size:200% 100%;animation:shimmer 1.5s infinite}
.surface-card{background:var(--surface);border:1px solid var(--border);border-radius:var(--radius);box-shadow:var(--shadow);transition:transform .2s,box-shadow .2s}
.surface-card:hover{transform:translateY(-2px);box-shadow:var(--shadow-lg)}
.surface-panel{background:var(--surface);border:1px solid var(--border);border-radius:var(--radius);padding:1.25rem}

.sidebar{position:fixed;top:0;left:0;width:var(--sidebar-w);height:100vh;background:var(--surface);border-right:1px solid var(--border);display:flex;flex-direction:column;z-index:100;transition:transform .3s}
.sidebar-brand{padding:1.25rem 1rem;border-bottom:1px solid var(--border);display:flex;align-items:center;gap:.75rem}
.sidebar-brand svg{width:32px;height:32px;flex-shrink:0}
.sidebar-brand .brand-text{font-family:'Sora',sans-serif;font-size:1.1rem;font-weight:700;color:var(--text)}
.sidebar-brand .brand-sub{font-size:.7rem;color:var(--text-muted);text-transform:uppercase;letter-spacing:.08em}
.sidebar-nav{flex:1;overflow-y:auto;padding:.5rem 0}
.nav-group{margin-bottom:.25rem}
.nav-group-label{padding:.5rem 1rem .25rem;font-size:.65rem;font-weight:600;text-transform:uppercase;letter-spacing:.1em;color:var(--text-muted);cursor:pointer;display:flex;align-items:center;justify-content:space-between;user-select:none}
.nav-group-label:hover{color:var(--text)}
.nav-group-label .chevron{transition:transform .2s;font-size:.6rem}
.nav-group.collapsed .nav-group-items{display:none}
.nav-group.collapsed .chevron{transform:rotate(-90deg)}
.nav-item{display:flex;align-items:center;gap:.6rem;padding:.45rem 1rem .45rem 1.25rem;cursor:pointer;color:var(--text-muted);transition:all .15s;font-size:.85rem;border-left:3px solid transparent}
.nav-item:hover{background:rgba(59,130,246,.08);color:var(--text)}
.nav-item.active{color:var(--accent);background:rgba(59,130,246,.12);border-left-color:var(--accent);font-weight:500}
.nav-item svg{width:16px;height:16px;flex-shrink:0;opacity:.7}
.nav-item.active svg{opacity:1}

.main{margin-left:var(--sidebar-w);min-height:100vh;padding:1.5rem 2rem 3rem}
.page-header{margin-bottom:1.5rem}
.page-header h1{font-size:1.5rem;margin-bottom:.25rem}
.page-header p{color:var(--text-muted);font-size:.85rem}

.hamburger{display:none;position:fixed;top:.75rem;left:.75rem;z-index:200;background:var(--surface);border:1px solid var(--border);border-radius:6px;padding:.5rem;cursor:pointer;color:var(--text)}
.hamburger svg{width:20px;height:20px}
.overlay{display:none;position:fixed;inset:0;background:rgba(0,0,0,.5);z-index:90}

@media(max-width:768px){
  .sidebar{transform:translateX(-100%)}
  .sidebar.open{transform:translateX(0)}
  .overlay.open{display:block}
  .hamburger{display:block}
  .main{margin-left:0;padding:1rem 1rem 2rem;padding-top:3rem}
}

.kpi-grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(200px,1fr));gap:1rem;margin-bottom:1.5rem}
.kpi-card{padding:1.25rem;display:flex;justify-content:space-between;align-items:flex-start}
.kpi-label{font-size:.65rem;text-transform:uppercase;letter-spacing:.08em;color:var(--text-muted);font-weight:600;margin-bottom:.35rem}
.kpi-value{font-family:'Sora',sans-serif;font-size:1.75rem;font-weight:700;line-height:1.1}
.kpi-sub{font-size:.75rem;color:var(--text-muted);margin-top:.25rem}
.kpi-icon{width:42px;height:42px;border-radius:10px;display:flex;align-items:center;justify-content:center;flex-shrink:0}
.kpi-icon svg{width:20px;height:20px}

.info-grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(280px,1fr));gap:1rem;margin-bottom:1.5rem}
.info-block{padding:1rem 1.25rem}
.info-block h3{font-size:.85rem;margin-bottom:.75rem;display:flex;align-items:center;gap:.5rem}
.info-row{display:flex;justify-content:space-between;padding:.35rem 0;border-bottom:1px solid rgba(30,58,95,.4);font-size:.8rem}
.info-row:last-child{border-bottom:none}
.info-row .label{color:var(--text-muted)}
.info-row .value{font-weight:500;text-align:right}

.data-table{width:100%;border-collapse:collapse}
.data-table th{text-align:left;padding:.6rem .75rem;font-size:.7rem;text-transform:uppercase;letter-spacing:.06em;color:var(--text-muted);border-bottom:1px solid var(--border);font-weight:600}
.data-table td{padding:.55rem .75rem;border-bottom:1px solid rgba(30,58,95,.3);font-size:.8rem}
.data-table tr:hover td{background:rgba(59,130,246,.04)}

.badge{display:inline-block;padding:.15rem .5rem;border-radius:4px;font-size:.65rem;font-weight:600;text-transform:uppercase;letter-spacing:.04em}
.badge-success{background:rgba(34,197,94,.15);color:var(--success)}
.badge-warning{background:rgba(234,179,8,.15);color:var(--warning)}
.badge-danger{background:rgba(239,68,68,.15);color:var(--danger)}
.badge-accent{background:rgba(59,130,246,.15);color:var(--accent)}
.badge-muted{background:var(--surface-muted);color:var(--text-muted)}

.status-dot{width:8px;height:8px;border-radius:50%;display:inline-block;margin-right:.4rem}
.status-dot.online{background:var(--success)}
.status-dot.offline{background:var(--danger)}
.status-dot.live{background:var(--success);animation:pulse 2s infinite}

.cards-grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(260px,1fr));gap:1rem}
.item-card{padding:1rem 1.25rem}
.item-header{display:flex;align-items:center;justify-content:space-between;margin-bottom:.5rem}
.item-name{font-weight:600;font-size:.9rem}
.item-hint{color:var(--text-muted);font-size:.78rem;line-height:1.4}

.btn{padding:.4rem .85rem;border-radius:6px;font-size:.78rem;font-weight:500;cursor:pointer;border:1px solid var(--border);background:var(--surface-muted);color:var(--text);transition:all .15s;font-family:inherit}
.btn:hover{background:var(--border)}
.btn-primary{background:var(--accent);border-color:var(--accent);color:#fff}
.btn-primary:hover{background:#2563eb}
.btn-danger{background:var(--danger);border-color:var(--danger);color:#fff}
.btn-danger:hover{background:#dc2626}
.btn-sm{padding:.25rem .6rem;font-size:.7rem}

.activity-item{display:flex;gap:.75rem;padding:.5rem 0;border-bottom:1px solid rgba(30,58,95,.3);font-size:.8rem}
.activity-item:last-child{border-bottom:none}
.activity-time{color:var(--text-muted);font-size:.7rem;white-space:nowrap;min-width:5rem;font-family:'JetBrains Mono',monospace}
.activity-text{flex:1}

.code-block{background:var(--bg);border:1px solid var(--border);border-radius:var(--radius);padding:1rem;overflow-x:auto;font-family:'JetBrains Mono',monospace;font-size:.75rem;line-height:1.6;color:var(--text-muted);max-height:70vh;overflow-y:auto;white-space:pre-wrap;word-break:break-all}

.event-feed{max-height:70vh;overflow-y:auto}
.event-item{padding:.5rem .75rem;border-bottom:1px solid rgba(30,58,95,.3);font-size:.8rem;font-family:'JetBrains Mono',monospace}
.event-item:nth-child(odd){background:rgba(17,24,39,.5)}

.consciousness-grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(300px,1fr));gap:1rem}
.coherence-bar{height:8px;background:var(--surface-muted);border-radius:4px;overflow:hidden;margin-top:.35rem}
.coherence-fill{height:100%;border-radius:4px;transition:width .5s}

.loading-placeholder{padding:2rem;text-align:center;color:var(--text-muted);font-size:.85rem}
.empty-state{padding:3rem;text-align:center;color:var(--text-muted)}
.empty-state svg{width:48px;height:48px;margin-bottom:1rem;opacity:.4}
.empty-state p{font-size:.9rem}
</style>
</head>
<body>

<button class="hamburger" id="hamburger" aria-label="Toggle sidebar">
<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M3 12h18M3 6h18M3 18h18"/></svg>
</button>
<div class="overlay" id="overlay"></div>

<aside class="sidebar" id="sidebar">
<div class="sidebar-brand">
<svg viewBox="0 0 32 32" fill="none"><circle cx="16" cy="16" r="14" stroke="var(--accent)" stroke-width="2"/><path d="M10 12l6 4-6 4V12z" fill="var(--accent)"/><circle cx="22" cy="12" r="2" fill="var(--success)"/><circle cx="22" cy="20" r="2" fill="var(--warning)"/></svg>
<div><div class="brand-text">ZeroClaw</div><div class="brand-sub">Mission Control</div></div>
</div>
<nav class="sidebar-nav" id="sidebarNav"></nav>
</aside>

<main class="main" id="main">
<div id="content"></div>
</main>

<script>
/*
 * ZeroClaw Dashboard - SPA Controller
 * All data fetched from local API endpoints; all rendering uses
 * textContent for user-controlled strings and sanitized template
 * assembly for structural markup (no untrusted content in markup).
 * API endpoints: /api/system, /api/channels, /api/status,
 * /api/control/, /api/consciousness
 */
(function(){
"use strict";

var ICONS = {
  overview:'<path d="M3 13h8V3H3v10zm0 8h8v-6H3v6zm10 0h8V11h-8v10zm0-18v6h8V3h-8z"/>',
  providers:'<path d="M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm-2 15l-5-5 1.41-1.41L10 14.17l7.59-7.59L19 8l-9 9z"/>',
  channels:'<path d="M20 2H4c-1.1 0-2 .9-2 2v18l4-4h14c1.1 0 2-.9 2-2V4c0-1.1-.9-2-2-2z"/>',
  tools:'<path d="M22.7 19l-9.1-9.1c.9-2.3.4-5-1.5-6.9-2-2-5-2.4-7.4-1.3L9 6 6 9 1.6 4.7C.4 7.1.9 10.1 2.9 12.1c1.9 1.9 4.6 2.4 6.9 1.5l9.1 9.1c.4.4 1 .4 1.4 0l2.3-2.3c.5-.4.5-1.1.1-1.4z"/>',
  memory:'<path d="M15 9H9v6h6V9zm-2 4h-2v-2h2v2zm8-2V9h-2V7c0-1.1-.9-2-2-2h-2V3h-2v2h-2V3H9v2H7c-1.1 0-2 .9-2 2v2H3v2h2v2H3v2h2v2c0 1.1.9 2 2 2h2v2h2v-2h2v2h2v-2h2c1.1 0 2-.9 2-2v-2h2v-2h-2v-2h2zm-4 6H7V7h10v10z"/>',
  observers:'<path d="M12 4.5C7 4.5 2.73 7.61 1 12c1.73 4.39 6 7.5 11 7.5s9.27-3.11 11-7.5c-1.73-4.39-6-7.5-11-7.5zM12 17c-2.76 0-5-2.24-5-5s2.24-5 5-5 5 2.24 5 5-2.24 5-5 5zm0-8c-1.66 0-3 1.34-3 3s1.34 3 3 3 3-1.34 3-3-1.34-3-3-3z"/>',
  runtimes:'<path d="M9 16.17L4.83 12l-1.42 1.41L9 19 21 7l-1.41-1.41z"/>',
  security:'<path d="M12 1L3 5v6c0 5.55 3.84 10.74 9 12 5.16-1.26 9-6.45 9-12V5l-9-4zm0 10.99h7c-.53 4.12-3.28 7.79-7 8.94V12H5V6.3l7-3.11v8.8z"/>',
  tunnels:'<path d="M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm-1 17.93c-3.95-.49-7-3.85-7-7.93 0-.62.08-1.21.21-1.79L9 15v1c0 1.1.9 2 2 2v1.93zm6.9-2.54c-.26-.81-1-1.39-1.9-1.39h-1v-3c0-.55-.45-1-1-1H8v-2h2c.55 0 1-.45 1-1V7h2c1.1 0 2-.9 2-2v-.41c2.93 1.19 5 4.06 5 7.41 0 2.08-.8 3.97-2.1 5.39z"/>',
  config:'<path d="M19.14 12.94c.04-.3.06-.61.06-.94 0-.32-.02-.64-.07-.94l2.03-1.58a.49.49 0 00.12-.61l-1.92-3.32a.49.49 0 00-.59-.22l-2.39.96c-.5-.38-1.03-.7-1.62-.94l-.36-2.54a.484.484 0 00-.48-.41h-3.84c-.24 0-.43.17-.47.41l-.36 2.54c-.59.24-1.13.57-1.62.94l-2.39-.96c-.22-.08-.47 0-.59.22L2.74 8.87c-.12.21-.08.47.12.61l2.03 1.58c-.05.3-.07.62-.07.94s.02.64.07.94l-2.03 1.58a.49.49 0 00-.12.61l1.92 3.32c.12.22.37.29.59.22l2.39-.96c.5.38 1.03.7 1.62.94l.36 2.54c.05.24.24.41.48.41h3.84c.24 0 .44-.17.47-.41l.36-2.54c.59-.24 1.13-.56 1.62-.94l2.39.96c.22.08.47 0 .59-.22l1.92-3.32c.12-.22.07-.47-.12-.61l-2.01-1.58zM12 15.6c-1.98 0-3.6-1.62-3.6-3.6s1.62-3.6 3.6-3.6 3.6 1.62 3.6 3.6-1.62 3.6-3.6 3.6z"/>',
  bots:'<path d="M12 2a2 2 0 012 2c0 .74-.4 1.39-1 1.73V7h1a7 7 0 017 7h1a1 1 0 011 1v3a1 1 0 01-1 1h-1.17A7 7 0 015.17 19H4a1 1 0 01-1-1v-3a1 1 0 011-1h1a7 7 0 017-7h1V5.73c-.6-.34-1-.99-1-1.73a2 2 0 012-2zM7.5 14a1.5 1.5 0 100 3 1.5 1.5 0 000-3zm9 0a1.5 1.5 0 100 3 1.5 1.5 0 000-3z"/>',
  commands:'<path d="M20 19V7H4v12h16m0-16a2 2 0 012 2v14a2 2 0 01-2 2H4a2 2 0 01-2-2V5a2 2 0 012-2h16zM7 9l4 2.5L7 14V9zm5 6h6v2h-6v-2z"/>',
  approvals:'<path d="M18 7l-1.41-1.41-6.34 6.34 1.41 1.41L18 7zm4.24-1.41L11.66 16.17 7.48 12l-1.41 1.41L11.66 19l12-12-1.42-1.41zM.41 13.41L6 19l1.41-1.41L1.83 12 .41 13.41z"/>',
  audit:'<path d="M19 3h-4.18C14.4 1.84 13.3 1 12 1c-1.3 0-2.4.84-2.82 2H5c-1.1 0-2 .9-2 2v14c0 1.1.9 2 2 2h14c1.1 0 2-.9 2-2V5c0-1.1-.9-2-2-2zm-7 0c.55 0 1 .45 1 1s-.45 1-1 1-1-.45-1-1 .45-1 1-1zm2 14H7v-2h7v2zm3-4H7v-2h10v2zm0-4H7V7h10v2z"/>',
  events:'<path d="M7.58 4.08L6.15 2.65C3.75 4.48 2.17 7.3 2.03 10.5h2c.15-2.65 1.51-4.97 3.55-6.42zm12.39 6.42h2c-.15-3.2-1.73-6.02-4.12-7.85l-1.42 1.43c2.02 1.45 3.39 3.77 3.54 6.42zM18 11c0-3.07-1.64-5.64-4.5-6.32V4c0-.83-.67-1.5-1.5-1.5s-1.5.67-1.5 1.5v.68C7.63 5.36 6 7.92 6 11v5l-2 2v1h16v-1l-2-2v-5zm-6 11c.14 0 .27-.01.4-.04.65-.14 1.18-.58 1.44-1.18.1-.24.15-.5.15-.78h-4c.01 1.1.9 2 2.01 2z"/>',
  consciousness:'<path d="M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm0 18c-4.42 0-8-3.58-8-8s3.58-8 8-8 8 3.58 8 8-3.58 8-8 8zm-1-4h2v-2h-2v2zm1-12C9.79 4 8 5.79 8 8h2c0-1.1.9-2 2-2s2 .9 2 2c0 2-3 1.75-3 5h2c0-2.25 3-2.5 3-5 0-2.21-1.79-4-4-4z"/>',
  peripherals:'<path d="M15 7.5V2H9v5.5l3 3 3-3zM7.5 9H2v6h5.5l3-3-3-3zM9 16.5V22h6v-5.5l-3-3-3 3zM16.5 9l-3 3 3 3H22V9h-5.5z"/>'
};

var NAV_GROUPS = [
  { name:'System', items:[
    { id: 'overview', label:'Overview', icon:'overview', group:'System' },
    { id: 'providers', label:'Providers', icon:'providers', group:'System' },
    { id: 'channels', label:'Channels', icon:'channels', group:'System' },
    { id: 'tools', label:'Tools', icon:'tools', group:'System' }
  ]},
  { name:'Services', items:[
    { id: 'memory', label:'Memory', icon:'memory', group:'Services' },
    { id: 'observers', label:'Observers', icon:'observers', group:'Services' },
    { id: 'runtimes', label:'Runtimes', icon:'runtimes', group:'Services' }
  ]},
  { name:'Infrastructure', items:[
    { id: 'security', label:'Security', icon:'security', group:'Infrastructure' },
    { id: 'tunnels', label:'Tunnels', icon:'tunnels', group:'Infrastructure' },
    { id: 'config', label:'Configuration', icon:'config', group:'Infrastructure' }
  ]},
  { name:'Control', items:[
    { id: 'bots', label:'Bots', icon:'bots', group:'Control' },
    { id: 'commands', label:'Commands', icon:'commands', group:'Control' },
    { id: 'approvals', label:'Approvals', icon:'approvals', group:'Control' },
    { id: 'audit', label:'Audit Log', icon:'audit', group:'Control' },
    { id: 'events', label:'Events', icon:'events', group:'Control' }
  ]},
  { name:'Intelligence', items:[
    { id: 'consciousness', label:'Consciousness', icon:'consciousness', group:'Intelligence' }
  ]},
  { name:'Settings', items:[
    { id: 'peripherals', label:'Peripherals', icon:'peripherals', group:'Settings' }
  ]}
];

var state = {
  currentPage: 'overview',
  cache: {},
  eventSource: null,
  refreshInterval: null,
  events: []
};

function esc(s) {
  if (s == null) return '';
  var div = document.createElement('div');
  div.textContent = String(s);
  return div.innerHTML;
}

function fmtTime(ts) {
  if (!ts) return '';
  try { return new Date(ts).toLocaleString(); } catch(e) { return String(ts); }
}

function setContent(el, html) {
  el.innerHTML = html;
}

function svgIcon(name, size) {
  size = size || 24;
  return '<svg viewBox="0 0 24 24" width="' + size + '" height="' + size + '" fill="currentColor">' + (ICONS[name] || '') + '</svg>';
}

function buildNav() {
  var nav = document.getElementById('sidebarNav');
  var html = '';
  NAV_GROUPS.forEach(function(g) {
    html += '<div class="nav-group" data-group="' + g.name + '">';
    html += '<div class="nav-group-label">' + esc(g.name) + '<span class="chevron">&#9660;</span></div>';
    html += '<div class="nav-group-items">';
    g.items.forEach(function(item) {
      var iconSvg = '<svg viewBox="0 0 24 24" fill="currentColor">' + (ICONS[item.icon] || '') + '</svg>';
      html += '<div class="nav-item' + (state.currentPage === item.id ? ' active' : '') + '" data-page="' + item.id + '">' + iconSvg + '<span>' + esc(item.label) + '</span></div>';
    });
    html += '</div></div>';
  });
  setContent(nav, html);

  nav.querySelectorAll('.nav-group-label').forEach(function(label) {
    label.addEventListener('click', function() {
      label.parentElement.classList.toggle('collapsed');
    });
  });
  nav.querySelectorAll('.nav-item').forEach(function(el) {
    el.addEventListener('click', function() {
      navigate(el.dataset.page);
    });
  });
}

function navigate(page) {
  state.currentPage = page;
  if (state.eventSource && page !== 'events') {
    state.eventSource.close();
    state.eventSource = null;
  }
  if (state.refreshInterval) {
    clearInterval(state.refreshInterval);
    state.refreshInterval = null;
  }
  document.querySelectorAll('.nav-item').forEach(function(el) {
    el.classList.toggle('active', el.dataset.page === page);
  });
  closeMobile();
  renderPage();
}

function fetchJSON(url) {
  return fetch(url).then(function(r) {
    if (!r.ok) throw new Error(r.status);
    return r.json();
  }).catch(function() { return null; });
}

function statusDot(active) {
  return '<span class="status-dot ' + (active ? 'online' : 'offline') + '"></span>';
}

function badgeFor(val) {
  if (val == null) return '<span class="badge badge-muted">unknown</span>';
  var s = String(val).toLowerCase();
  if (s === 'true' || s === 'enabled' || s === 'active' || s === 'online')
    return '<span class="badge badge-success">' + esc(val) + '</span>';
  if (s === 'false' || s === 'disabled' || s === 'offline')
    return '<span class="badge badge-muted">' + esc(val) + '</span>';
  return '<span class="badge badge-accent">' + esc(val) + '</span>';
}

function loadingBlock() {
  return '<div class="loading-placeholder shimmer" style="height:120px;border-radius:8px"></div>';
}

function emptyState(msg) {
  return '<div class="empty-state"><svg viewBox="0 0 24 24" fill="currentColor"><path d="M20 6h-8l-2-2H4c-1.1 0-2 .9-2 2v12c0 1.1.9 2 2 2h16c1.1 0 2-.9 2-2V8c0-1.1-.9-2-2-2zm0 12H4V8h16v10z"/></svg><p>' + esc(msg) + '</p></div>';
}

function kpiCard(label, value, sub, color, iconHtml) {
  return '<div class="surface-card kpi-card fade-in-up"><div><div class="kpi-label">' + esc(label) + '</div><div class="kpi-value">' + esc(String(value)) + '</div><div class="kpi-sub">' + esc(sub) + '</div></div><div class="kpi-icon" style="background:' + color + '20;color:' + color + '">' + iconHtml + '</div></div>';
}

function infoBlock(title, color, rows) {
  var html = '<div class="surface-card info-block fade-in-up"><h3><span style="color:' + color + '">&#9679;</span> ' + esc(title) + '</h3>';
  rows.forEach(function(r) {
    html += '<div class="info-row"><span class="label">' + esc(r[0]) + '</span><span class="value">' + esc(String(r[1])) + '</span></div>';
  });
  return html + '</div>';
}

function renderOverview() {
  var el = document.getElementById('content');
  setContent(el, '<div class="page-header fade-in-up"><h1>Dashboard Overview</h1><p>Real-time system health and activity</p></div>' + loadingBlock());

  Promise.all([
    fetchJSON('/api/system'),
    fetchJSON('/api/status'),
    fetchJSON('/api/control/approvals'),
    fetchJSON('/api/control/audit')
  ]).then(function(results) {
    var sys = results[0], status = results[1], approvals = results[2], audit = results[3];
    state.cache.system = sys;
    state.cache.status = status;

    var channelsCount = (status && status.channels_count != null) ? status.channels_count : (sys && sys.channels ? sys.channels.count : '--');
    var providersCount = sys && sys.providers ? sys.providers.count : '--';
    var toolsCount = (status && status.tools_count != null) ? status.tools_count : (sys && sys.tools ? sys.tools.count : '--');
    var memoryBackend = status && status.memory_backend ? status.memory_backend : (sys && sys.memory && sys.memory.items && sys.memory.items.length ? sys.memory.items[0].name : '--');

    var html = '<div class="page-header fade-in-up"><h1>Dashboard Overview</h1><p>Real-time system health and activity</p></div>';

    html += '<div class="kpi-grid">';
    html += kpiCard('Active Channels', channelsCount, 'Connected streams', 'var(--accent)', svgIcon('channels', 20));
    html += kpiCard('Providers', providersCount, 'AI backends', 'var(--success)', svgIcon('providers', 20));
    html += kpiCard('Tools Count', toolsCount, 'Registered tools', 'var(--warning)', svgIcon('tools', 20));
    html += kpiCard('Memory Backend', memoryBackend, 'Storage layer', '#a78bfa', svgIcon('memory', 20));
    html += '</div>';

    html += '<div class="info-grid">';
    html += infoBlock('System Status', 'var(--accent)', [
      ['Provider', sys && sys.provider ? sys.provider : '--'],
      ['Model', sys && sys.model ? sys.model : '--'],
      ['Temperature', sys && sys.temperature != null ? sys.temperature : '--'],
      ['Auto-Save', status && status.auto_save != null ? String(status.auto_save) : '--']
    ]);
    html += infoBlock('Security', 'var(--warning)', [
      ['Autonomy Level', sys && sys.security ? sys.security.autonomy_level || '--' : '--'],
      ['Sandbox', sys && sys.security ? String(sys.security.sandbox_enabled || '--') : '--'],
      ['Pairing', sys && sys.security ? sys.security.pairing || '--' : '--']
    ]);
    html += infoBlock('Gateway Health', 'var(--success)', [
      ['Host', status && status.host ? status.host : '--'],
      ['Port', status && status.port != null ? status.port : '--'],
      ['Tunnels', sys && sys.tunnels ? sys.tunnels.count : '--']
    ]);
    html += '</div>';

    html += '<div class="surface-panel fade-in-up" style="margin-bottom:1rem"><h3 style="margin-bottom:.75rem;font-size:.9rem">Pending Approvals</h3>';
    if (approvals && approvals.length) {
      html += '<table class="data-table"><thead><tr><th>ID</th><th>Action</th><th>Status</th><th>Time</th></tr></thead><tbody>';
      approvals.forEach(function(a) {
        html += '<tr><td class="mono">' + esc(a.id) + '</td><td>' + esc(a.action || a.description || '') + '</td><td>' + badgeFor(a.status) + '</td><td>' + esc(fmtTime(a.created_at || a.timestamp)) + '</td></tr>';
      });
      html += '</tbody></table>';
    } else {
      html += '<div style="color:var(--text-muted);font-size:.8rem">No pending approvals</div>';
    }
    html += '</div>';

    html += '<div class="surface-panel fade-in-up"><h3 style="margin-bottom:.75rem;font-size:.9rem">Recent Activity</h3>';
    if (audit && audit.length) {
      audit.slice(0, 8).forEach(function(a) {
        html += '<div class="activity-item"><span class="activity-time">' + esc(fmtTime(a.timestamp || a.created_at)) + '</span><span class="activity-text">' + esc(a.message || a.action || a.event || JSON.stringify(a)) + '</span></div>';
      });
    } else {
      html += '<div style="color:var(--text-muted);font-size:.8rem">No recent activity</div>';
    }
    html += '</div>';

    setContent(el, html);
  });

  state.refreshInterval = setInterval(function() {
    if (state.currentPage === 'overview') renderOverview();
  }, 15000);
}

function renderChannels() {
  var el = document.getElementById('content');
  setContent(el, '<div class="page-header fade-in-up"><h1>Channels</h1><p>Communication channels and message streams</p></div>' + loadingBlock());
  fetchJSON('/api/channels').then(function(data) {
    var html = '<div class="page-header fade-in-up"><h1>Channels</h1><p>Communication channels and message streams</p></div>';
    html += '<div class="surface-panel fade-in-up">';
    var items = data ? (Array.isArray(data) ? data : (data.items || [])) : [];
    if (items.length) {
      html += '<table class="data-table"><thead><tr><th>Name</th><th>Category</th><th>Status</th><th>Hint</th></tr></thead><tbody>';
      items.forEach(function(ch) {
        var active = ch.enabled !== false && ch.status !== 'offline';
        html += '<tr><td><strong>' + esc(ch.name) + '</strong></td><td>' + badgeFor(ch.category || ch.type) + '</td><td>' + statusDot(active) + (active ? 'Online' : 'Offline') + '</td><td style="color:var(--text-muted)">' + esc(ch.hint || ch.description || '') + '</td></tr>';
      });
      html += '</tbody></table>';
    } else {
      html += emptyState('No channels configured');
    }
    html += '</div>';
    setContent(el, html);
  });
}

function renderItemsPage(key, title, subtitle) {
  var el = document.getElementById('content');
  setContent(el, '<div class="page-header fade-in-up"><h1>' + esc(title) + '</h1><p>' + esc(subtitle) + '</p></div>' + loadingBlock());

  var p = state.cache.system ? Promise.resolve(state.cache.system) : fetchJSON('/api/system');
  p.then(function(sys) {
    state.cache.system = sys;
    var html = '<div class="page-header fade-in-up"><h1>' + esc(title) + '</h1><p>' + esc(subtitle) + '</p></div>';
    var section = sys && sys[key];
    var items = section ? (section.items || []) : [];
    if (items.length) {
      html += '<div class="cards-grid">';
      items.forEach(function(item) {
        var active = item.enabled !== false && item.active !== false;
        html += '<div class="surface-card item-card fade-in-up"><div class="item-header"><span class="item-name">' + statusDot(active) + ' ' + esc(item.name) + '</span>' + badgeFor(item.category || item.type || '') + '</div><div class="item-hint">' + esc(item.hint || item.description || '') + '</div></div>';
      });
      html += '</div>';
    } else {
      html += '<div class="surface-panel">' + emptyState('No ' + title.toLowerCase() + ' registered') + '</div>';
    }
    setContent(el, html);
  });
}

function renderSecurity() {
  var el = document.getElementById('content');
  setContent(el, '<div class="page-header fade-in-up"><h1>Security</h1><p>Autonomy levels, sandbox, and access control</p></div>' + loadingBlock());

  var p = state.cache.system ? Promise.resolve(state.cache.system) : fetchJSON('/api/system');
  p.then(function(sys) {
    state.cache.system = sys;
    var sec = sys && sys.security ? sys.security : {};
    var html = '<div class="page-header fade-in-up"><h1>Security</h1><p>Autonomy levels, sandbox, and access control</p></div>';
    html += '<div class="info-grid">';
    html += infoBlock('Autonomy', 'var(--warning)', [
      ['Level', sec.autonomy_level || '--'],
      ['Mode', sec.mode || '--'],
      ['Max Autonomy', sec.max_autonomy || '--']
    ]);
    html += infoBlock('Sandbox', 'var(--accent)', [
      ['Enabled', sec.sandbox_enabled != null ? String(sec.sandbox_enabled) : '--'],
      ['Type', sec.sandbox_type || '--'],
      ['Isolation', sec.isolation || '--']
    ]);
    html += '</div>';
    if (sec.auto_approve_list && sec.auto_approve_list.length) {
      html += '<div class="surface-panel fade-in-up" style="margin-top:1rem"><h3 style="margin-bottom:.75rem;font-size:.9rem">Auto-Approve List</h3>';
      html += '<div style="display:flex;flex-wrap:wrap;gap:.4rem">';
      sec.auto_approve_list.forEach(function(a) {
        html += '<span class="badge badge-accent">' + esc(a) + '</span>';
      });
      html += '</div></div>';
    }
    setContent(el, html);
  });
}

function renderConfig() {
  var el = document.getElementById('content');
  setContent(el, '<div class="page-header fade-in-up"><h1>Configuration</h1><p>Current runtime configuration (read-only)</p></div>' + loadingBlock());
  fetchJSON('/api/status').then(function(data) {
    var html = '<div class="page-header fade-in-up"><h1>Configuration</h1><p>Current runtime configuration (read-only)</p></div>';
    html += '<div class="surface-panel fade-in-up"><pre class="code-block">' + (data ? esc(JSON.stringify(data, null, 2)) : 'Unable to load configuration') + '</pre></div>';
    setContent(el, html);
  });
}

function renderTablePage(endpoint, title, subtitle, columns) {
  var el = document.getElementById('content');
  setContent(el, '<div class="page-header fade-in-up"><h1>' + esc(title) + '</h1><p>' + esc(subtitle) + '</p></div>' + loadingBlock());
  fetchJSON(endpoint).then(function(data) {
    var items = data ? (Array.isArray(data) ? data : (data.items || [])) : [];
    var html = '<div class="page-header fade-in-up"><h1>' + esc(title) + '</h1><p>' + esc(subtitle) + '</p></div>';
    html += '<div class="surface-panel fade-in-up">';
    if (items.length) {
      html += '<table class="data-table"><thead><tr>';
      columns.forEach(function(c) { html += '<th>' + esc(c.label) + '</th>'; });
      html += '</tr></thead><tbody>';
      items.forEach(function(item) {
        html += '<tr>';
        columns.forEach(function(c) {
          var v = item[c.key];
          if (c.type === 'badge') html += '<td>' + badgeFor(v) + '</td>';
          else if (c.type === 'time') html += '<td>' + esc(fmtTime(v)) + '</td>';
          else html += '<td>' + esc(v != null ? String(v) : '') + '</td>';
        });
        html += '</tr>';
      });
      html += '</tbody></table>';
    } else {
      html += emptyState('No ' + title.toLowerCase() + ' found');
    }
    html += '</div>';
    setContent(el, html);
  });
}

function renderApprovals() {
  var el = document.getElementById('content');
  setContent(el, '<div class="page-header fade-in-up"><h1>Approvals</h1><p>Pending action approvals</p></div>' + loadingBlock());
  fetchJSON('/api/control/approvals').then(function(data) {
    var items = data ? (Array.isArray(data) ? data : (data.items || [])) : [];
    var html = '<div class="page-header fade-in-up"><h1>Approvals</h1><p>Pending action approvals</p></div>';
    html += '<div class="surface-panel fade-in-up">';
    if (items.length) {
      html += '<table class="data-table"><thead><tr><th>ID</th><th>Action</th><th>Status</th><th>Time</th><th>Actions</th></tr></thead><tbody>';
      items.forEach(function(a) {
        var safeId = esc(a.id);
        html += '<tr><td class="mono">' + safeId + '</td><td>' + esc(a.action || a.description || '') + '</td><td>' + badgeFor(a.status) + '</td><td>' + esc(fmtTime(a.created_at || a.timestamp)) + '</td><td>';
        if (!a.status || a.status === 'pending') {
          html += '<button class="btn btn-primary btn-sm" data-approval-id="' + safeId + '" data-approval-action="approve">Approve</button> ';
          html += '<button class="btn btn-danger btn-sm" data-approval-id="' + safeId + '" data-approval-action="reject">Reject</button>';
        } else {
          html += '<span style="color:var(--text-muted);font-size:.75rem">Resolved</span>';
        }
        html += '</td></tr>';
      });
      html += '</tbody></table>';
    } else {
      html += emptyState('No approvals pending');
    }
    html += '</div>';
    setContent(el, html);

    el.querySelectorAll('[data-approval-id]').forEach(function(btn) {
      btn.addEventListener('click', function() {
        var id = btn.getAttribute('data-approval-id');
        var action = btn.getAttribute('data-approval-action');
        fetch('/api/control/approvals/' + encodeURIComponent(id), {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ action: action })
        }).then(function() { renderApprovals(); }).catch(function() {});
      });
    });
  });
}

function renderAudit() {
  var el = document.getElementById('content');
  setContent(el, '<div class="page-header fade-in-up"><h1>Audit Log</h1><p>Chronological system event log</p></div>' + loadingBlock());
  fetchJSON('/api/control/audit').then(function(data) {
    var items = data ? (Array.isArray(data) ? data : (data.items || [])) : [];
    var html = '<div class="page-header fade-in-up"><h1>Audit Log</h1><p>Chronological system event log</p></div>';
    html += '<div class="surface-panel fade-in-up">';
    if (items.length) {
      items.forEach(function(a) {
        html += '<div class="activity-item"><span class="activity-time">' + esc(fmtTime(a.timestamp || a.created_at)) + '</span><span class="activity-text">' + esc(a.message || a.action || a.event || JSON.stringify(a)) + '</span></div>';
      });
    } else {
      html += emptyState('No audit entries');
    }
    html += '</div>';
    setContent(el, html);
  });
}

function renderEvents() {
  var el = document.getElementById('content');
  var html = '<div class="page-header fade-in-up"><h1>Events</h1><p>Live event stream via SSE</p></div>';
  html += '<div class="surface-panel fade-in-up"><div style="display:flex;align-items:center;gap:.5rem;margin-bottom:.75rem"><span class="status-dot live" id="sseStatus"></span><span style="font-size:.8rem" id="sseLabel">Connecting...</span></div>';
  html += '<div class="event-feed" id="eventFeed"></div></div>';
  setContent(el, html);

  var feed = document.getElementById('eventFeed');
  var statusEl = document.getElementById('sseStatus');
  var labelEl = document.getElementById('sseLabel');

  if (state.eventSource) state.eventSource.close();
  try {
    state.eventSource = new EventSource('/api/control/events/stream');
    state.eventSource.onopen = function() {
      statusEl.className = 'status-dot live';
      labelEl.textContent = 'Connected';
    };
    state.eventSource.onmessage = function(e) {
      var div = document.createElement('div');
      div.className = 'event-item';
      div.textContent = '[' + new Date().toLocaleTimeString() + '] ' + e.data;
      feed.insertBefore(div, feed.firstChild);
      if (feed.children.length > 200) feed.removeChild(feed.lastChild);
    };
    state.eventSource.onerror = function() {
      statusEl.className = 'status-dot offline';
      labelEl.textContent = 'Disconnected';
    };
  } catch(e) {
    statusEl.className = 'status-dot offline';
    labelEl.textContent = 'SSE not available';
  }
}

function renderConsciousness() {
  var el = document.getElementById('content');
  setContent(el, '<div class="page-header fade-in-up"><h1>Consciousness</h1><p>Phenomenal state, coherence, and neural dynamics</p></div>' + loadingBlock());

  Promise.all([
    fetchJSON('/api/consciousness'),
    fetchJSON('/api/consciousness/state'),
    fetchJSON('/api/consciousness/brain_scan')
  ]).then(function(results) {
    var cdata = results[0], cstate = results[1], bscan = results[2];
    var c = cdata || cstate || {};
    var html = '<div class="page-header fade-in-up"><h1>Consciousness</h1><p>Phenomenal state, coherence, and neural dynamics</p></div>';

    html += '<div class="consciousness-grid">';

    var coherence = c.coherence != null ? c.coherence : (c.global_coherence != null ? c.global_coherence : null);
    html += '<div class="surface-card fade-in-up" style="padding:1.25rem"><h3 style="font-size:.9rem;margin-bottom:.75rem">Coherence</h3>';
    if (coherence != null) {
      var pct = Math.round(Number(coherence) * 100);
      var clr = pct > 70 ? 'var(--success)' : pct > 40 ? 'var(--warning)' : 'var(--danger)';
      html += '<div style="font-size:2rem;font-weight:700;font-family:Sora;color:' + clr + '">' + esc(pct) + '%</div>';
      html += '<div class="coherence-bar"><div class="coherence-fill" style="width:' + pct + '%;background:' + clr + '"></div></div>';
    } else {
      html += '<div style="color:var(--text-muted)">No coherence data</div>';
    }
    html += '</div>';

    html += '<div class="surface-card fade-in-up" style="padding:1.25rem"><h3 style="font-size:.9rem;margin-bottom:.75rem">Phenomenal State</h3>';
    var phenomenal = c.phenomenal_state || c.state || null;
    if (phenomenal) {
      if (typeof phenomenal === 'object') {
        Object.keys(phenomenal).forEach(function(k) {
          html += '<div class="info-row"><span class="label">' + esc(k) + '</span><span class="value">' + esc(String(phenomenal[k])) + '</span></div>';
        });
      } else {
        html += '<div style="font-size:1.1rem;font-weight:600">' + esc(String(phenomenal)) + '</div>';
      }
    } else {
      html += '<div style="color:var(--text-muted)">No phenomenal state data</div>';
    }
    html += '</div>';

    html += '<div class="surface-card fade-in-up" style="padding:1.25rem"><h3 style="font-size:.9rem;margin-bottom:.75rem">Flow &amp; Wisdom</h3>';
    html += '<div class="info-row"><span class="label">Flow State</span><span class="value">' + esc(c.flow_state || c.flow || '--') + '</span></div>';
    html += '<div class="info-row"><span class="label">Wisdom Count</span><span class="value">' + esc(c.wisdom_count != null ? String(c.wisdom_count) : '--') + '</span></div>';
    html += '<div class="info-row"><span class="label">Attention</span><span class="value">' + esc(c.attention || '--') + '</span></div>';
    html += '</div>';

    html += '<div class="surface-card fade-in-up" style="padding:1.25rem"><h3 style="font-size:.9rem;margin-bottom:.75rem">NCN Neuromodulatory</h3>';
    var ncn = c.ncn || c.neuromodulatory || c.neurotransmitters || null;
    if (ncn && typeof ncn === 'object') {
      Object.keys(ncn).forEach(function(k) {
        var val = Number(ncn[k]);
        var npct = Math.round(val * 100);
        html += '<div style="margin-bottom:.4rem"><div style="display:flex;justify-content:space-between;font-size:.75rem;margin-bottom:.15rem"><span>' + esc(k) + '</span><span>' + esc(npct) + '%</span></div>';
        html += '<div class="coherence-bar"><div class="coherence-fill" style="width:' + npct + '%;background:var(--accent)"></div></div></div>';
      });
    } else {
      html += '<div style="color:var(--text-muted)">No NCN data available</div>';
    }
    html += '</div>';

    html += '<div class="surface-card fade-in-up" style="padding:1.25rem;grid-column:1/-1"><h3 style="font-size:.9rem;margin-bottom:.75rem">Brain Scan</h3>';
    if (bscan) {
      html += '<pre class="code-block">' + esc(JSON.stringify(bscan, null, 2)) + '</pre>';
    } else {
      html += '<div style="color:var(--text-muted)">No brain scan data available</div>';
    }
    html += '</div>';

    html += '</div>';
    setContent(el, html);
  });
}

function renderPeripherals() {
  var el = document.getElementById('content');
  var html = '<div class="page-header fade-in-up"><h1>Peripherals</h1><p>Connected hardware peripherals</p></div>';
  html += '<div class="surface-panel fade-in-up">' + emptyState('No peripherals connected') + '</div>';
  setContent(el, html);
}

function renderPage() {
  var page = state.currentPage;
  switch (page) {
    case 'overview': renderOverview(); break;
    case 'channels': renderChannels(); break;
    case 'providers': renderItemsPage('providers', 'Providers', 'AI model providers and backends'); break;
    case 'tools': renderItemsPage('tools', 'Tools', 'Registered tool capabilities'); break;
    case 'memory': renderItemsPage('memory', 'Memory', 'Memory backends and storage'); break;
    case 'observers': renderItemsPage('observers', 'Observers', 'System observers and monitors'); break;
    case 'runtimes': renderItemsPage('runtimes', 'Runtimes', 'Runtime adapters and engines'); break;
    case 'tunnels': renderItemsPage('tunnels', 'Tunnels', 'Network tunnels and endpoints'); break;
    case 'security': renderSecurity(); break;
    case 'config': renderConfig(); break;
    case 'bots': renderTablePage('/api/control/bots', 'Bots', 'Registered bot instances', [
      { key: 'id', label: 'ID' }, { key: 'name', label: 'Name' }, { key: 'status', label: 'Status', type: 'badge' }, { key: 'created_at', label: 'Created', type: 'time' }
    ]); break;
    case 'commands': renderTablePage('/api/control/commands', 'Commands', 'Available control commands', [
      { key: 'name', label: 'Name' }, { key: 'description', label: 'Description' }, { key: 'category', label: 'Category', type: 'badge' }
    ]); break;
    case 'approvals': renderApprovals(); break;
    case 'audit': renderAudit(); break;
    case 'events': renderEvents(); break;
    case 'consciousness': renderConsciousness(); break;
    case 'peripherals': renderPeripherals(); break;
    default: renderOverview();
  }
}

function closeMobile() {
  document.getElementById('sidebar').classList.remove('open');
  document.getElementById('overlay').classList.remove('open');
}

document.getElementById('hamburger').addEventListener('click', function() {
  document.getElementById('sidebar').classList.toggle('open');
  document.getElementById('overlay').classList.toggle('open');
});
document.getElementById('overlay').addEventListener('click', closeMobile);

buildNav();
renderPage();

})();
</script>
</body>
</html>
</html>
"##;

const VALID_PROVIDERS: &[&str] = &[
    "openai",
    "anthropic",
    "google",
    "ollama",
    "groq",
    "mistral",
    "cohere",
    "deepseek",
    "xai",
    "openrouter",
    "fireworks",
    "together",
    "perplexity",
    "aws_bedrock",
    "azure",
    "cloudflare_ai",
    "cerebras",
    "sambanova",
    "hyperbolic",
    "lmstudio",
    "custom",
];

const VALID_MEMORY_BACKENDS: &[&str] = &["sqlite", "lucid", "postgres", "markdown", "none"];

const VALID_OBSERVER_BACKENDS: &[&str] = &["none", "log", "prometheus", "otel"];

const VALID_RUNTIME_KINDS: &[&str] = &["native", "docker", "wasm"];

const VALID_TUNNEL_PROVIDERS: &[&str] = &["none", "cloudflare", "tailscale", "ngrok", "custom"];

const SECRET_FIELDS: &[&str] = &[
    "bot_token",
    "app_token",
    "app_secret",
    "api_key",
    "access_token",
    "client_secret",
    "server_password",
    "nickserv_password",
    "sasl_password",
    "password",
    "verify_token",
    "encrypt_key",
    "verification_token",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dashboard_html_is_valid_document() {
        let html = DASHBOARD_HTML.trim();
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.ends_with("</html>"));
        assert!(DASHBOARD_HTML.contains("<head>"));
        assert!(DASHBOARD_HTML.contains("</head>"));
        assert!(DASHBOARD_HTML.contains("<body"));
        assert!(DASHBOARD_HTML.contains("</body>"));
    }

    #[test]
    fn dashboard_html_contains_zeroclaw_branding() {
        assert!(DASHBOARD_HTML.contains("ZeroClaw"));
        assert!(DASHBOARD_HTML.contains("Dashboard"));
    }

    #[test]
    fn dashboard_html_references_all_api_endpoints() {
        let expected_endpoints = [
            "/api/system",
            "/api/channels",
            "/api/status",
            "/api/control/",
            "/api/consciousness",
        ];
        for ep in &expected_endpoints {
            assert!(
                DASHBOARD_HTML.contains(ep),
                "Dashboard HTML missing endpoint reference: {ep}"
            );
        }
    }

    #[test]
    fn dashboard_html_has_all_nav_sections() {
        let nav_sections = [
            "overview",
            "providers",
            "channels",
            "tools",
            "memory",
            "observers",
            "runtimes",
            "security",
            "tunnels",
            "config",
            "bots",
            "commands",
            "approvals",
            "audit",
            "events",
            "consciousness",
            "peripherals",
        ];
        for section in &nav_sections {
            let nav_id = format!("id: '{section}'");
            assert!(
                DASHBOARD_HTML.contains(&nav_id),
                "Dashboard HTML missing nav section: {section}"
            );
        }
    }

    #[test]
    fn valid_providers_contains_major_providers() {
        let required = ["openai", "anthropic", "ollama", "openrouter", "deepseek"];
        for p in &required {
            assert!(
                VALID_PROVIDERS.contains(p),
                "Missing required provider: {p}"
            );
        }
        assert_eq!(VALID_PROVIDERS.len(), 21);
    }

    #[test]
    fn valid_memory_backends_are_complete() {
        assert!(VALID_MEMORY_BACKENDS.contains(&"sqlite"));
        assert!(VALID_MEMORY_BACKENDS.contains(&"none"));
        assert_eq!(VALID_MEMORY_BACKENDS.len(), 5);
    }

    #[test]
    fn valid_observer_backends_are_complete() {
        assert!(VALID_OBSERVER_BACKENDS.contains(&"none"));
        assert!(VALID_OBSERVER_BACKENDS.contains(&"prometheus"));
        assert_eq!(VALID_OBSERVER_BACKENDS.len(), 4);
    }

    #[test]
    fn valid_runtime_kinds_are_complete() {
        assert!(VALID_RUNTIME_KINDS.contains(&"native"));
        assert_eq!(VALID_RUNTIME_KINDS.len(), 3);
    }

    #[test]
    fn valid_tunnel_providers_are_complete() {
        assert!(VALID_TUNNEL_PROVIDERS.contains(&"none"));
        assert!(VALID_TUNNEL_PROVIDERS.contains(&"cloudflare"));
        assert_eq!(VALID_TUNNEL_PROVIDERS.len(), 5);
    }

    #[test]
    fn secret_fields_covers_sensitive_keys() {
        let critical = [
            "bot_token",
            "access_token",
            "api_key",
            "password",
            "client_secret",
        ];
        for field in &critical {
            assert!(
                SECRET_FIELDS.contains(field),
                "Missing secret field: {field}"
            );
        }
        assert!(SECRET_FIELDS.len() >= 10);
    }

    #[test]
    fn no_duplicate_entries_in_validation_lists() {
        fn has_duplicates<'a>(items: &'a [&'a str]) -> Option<&'a str> {
            let mut seen = std::collections::HashSet::new();
            items.iter().find(|&item| !seen.insert(item)).copied()
        }

        assert_eq!(
            has_duplicates(VALID_PROVIDERS),
            None,
            "Duplicate in VALID_PROVIDERS"
        );
        assert_eq!(
            has_duplicates(VALID_MEMORY_BACKENDS),
            None,
            "Duplicate in VALID_MEMORY_BACKENDS"
        );
        assert_eq!(
            has_duplicates(VALID_OBSERVER_BACKENDS),
            None,
            "Duplicate in VALID_OBSERVER_BACKENDS"
        );
        assert_eq!(
            has_duplicates(VALID_RUNTIME_KINDS),
            None,
            "Duplicate in VALID_RUNTIME_KINDS"
        );
        assert_eq!(
            has_duplicates(VALID_TUNNEL_PROVIDERS),
            None,
            "Duplicate in VALID_TUNNEL_PROVIDERS"
        );
        assert_eq!(
            has_duplicates(SECRET_FIELDS),
            None,
            "Duplicate in SECRET_FIELDS"
        );
    }

    #[test]
    fn dashboard_html_does_not_contain_inline_secrets() {
        let forbidden = ["sk-", "xoxb-", "ghp_", "AKIA", "password123"];
        for pattern in &forbidden {
            assert!(
                !DASHBOARD_HTML.contains(pattern),
                "Dashboard HTML contains potential secret pattern: {pattern}"
            );
        }
    }
}
