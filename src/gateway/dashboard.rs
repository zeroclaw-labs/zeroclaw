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

const DASHBOARD_HTML: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>ZeroClaw Admin</title>
<script src="https://cdn.tailwindcss.com"></script>
<script>
tailwind.config = {
  theme: {
    extend: {
      colors: {
        zc: {
          bg: '#06060b',
          surface: '#0c0c14',
          card: '#11111b',
          border: '#1a1a2e',
          accent: '#3b82f6',
          green: '#22c55e',
          amber: '#f59e0b',
          red: '#ef4444',
          purple: '#a855f7',
          cyan: '#06b6d4',
          pink: '#ec4899',
          lime: '#84cc16',
        }
      }
    }
  }
}
</script>
<style>
@import url('https://fonts.googleapis.com/css2?family=Inter:wght@300;400;500;600;700&family=JetBrains+Mono:wght@400;500&display=swap');
body { font-family: 'Inter', -apple-system, sans-serif; }
code, .mono { font-family: 'JetBrains Mono', monospace; }
.pulse { animation: pulse 2s infinite; }
@keyframes pulse { 0%, 100% { opacity: 1; } 50% { opacity: 0.5; } }
.nav-active { background: rgba(59, 130, 246, 0.12); color: #3b82f6; border-color: #3b82f6; }
.card { transition: all 0.15s ease; }
.card:hover { border-color: rgba(59, 130, 246, 0.4); }
.enabled-bar { border-left: 3px solid #22c55e; }
.available-bar { border-left: 3px solid #374151; }
.active-bar { border-left: 3px solid #3b82f6; }
.fade-in { animation: fadeIn 0.2s ease; }
@keyframes fadeIn { from { opacity: 0; transform: translateY(4px); } to { opacity: 1; transform: translateY(0); } }
.badge { font-size: 10px; letter-spacing: 0.05em; }
.scroll-area { max-height: calc(100vh - 140px); overflow-y: auto; }
.scroll-area::-webkit-scrollbar { width: 4px; }
.scroll-area::-webkit-scrollbar-track { background: transparent; }
.scroll-area::-webkit-scrollbar-thumb { background: #1a1a2e; border-radius: 2px; }
.toast { position: fixed; top: 1rem; right: 1rem; z-index: 9999; padding: 0.75rem 1rem; border-radius: 0.75rem; font-size: 0.8rem; font-weight: 500; animation: slideIn 0.2s ease; }
.toast-ok { background: rgba(34,197,94,0.15); border: 1px solid rgba(34,197,94,0.3); color: #22c55e; }
.toast-err { background: rgba(239,68,68,0.15); border: 1px solid rgba(239,68,68,0.3); color: #ef4444; }
.toast-warn { background: rgba(245,158,11,0.15); border: 1px solid rgba(245,158,11,0.3); color: #f59e0b; }
@keyframes slideIn { from { opacity: 0; transform: translateX(20px); } to { opacity: 1; transform: translateX(0); } }
.modal-bg { position: fixed; inset: 0; background: rgba(0,0,0,0.6); z-index: 9000; display: flex; align-items: center; justify-content: center; }
.modal-box { background: #11111b; border: 1px solid #1a1a2e; border-radius: 1rem; padding: 1.5rem; min-width: 20rem; max-width: 32rem; max-height: 80vh; overflow-y: auto; }
.admin-input { background: #06060b; border: 1px solid #1a1a2e; border-radius: 0.5rem; padding: 0.5rem 0.75rem; font-size: 0.8rem; color: #e5e7eb; width: 100%; outline: none; }
.admin-input:focus { border-color: #3b82f6; }
.admin-btn { background: rgba(59,130,246,0.15); color: #3b82f6; border: 1px solid rgba(59,130,246,0.3); padding: 0.5rem 1rem; border-radius: 0.5rem; font-size: 0.8rem; font-weight: 500; cursor: pointer; }
.admin-btn:hover { background: rgba(59,130,246,0.25); }
.admin-btn-red { background: rgba(239,68,68,0.15); color: #ef4444; border: 1px solid rgba(239,68,68,0.3); }
.admin-btn-red:hover { background: rgba(239,68,68,0.25); }
.radio-card { cursor: pointer; transition: all 0.15s ease; }
.radio-card:hover { border-color: rgba(59,130,246,0.4); }
.radio-card.selected { border-color: #3b82f6; background: rgba(59,130,246,0.05); }
</style>
</head>
<body class="bg-zc-bg text-gray-300 min-h-screen flex">

<!-- SIDEBAR NAV -->
<nav class="w-56 min-h-screen bg-zc-surface border-r border-zc-border flex flex-col shrink-0">
  <div class="px-4 py-4 border-b border-zc-border">
    <div class="flex items-center gap-2.5">
      <div class="w-8 h-8 rounded-lg bg-gradient-to-br from-zc-accent to-zc-purple flex items-center justify-center">
        <span class="text-white text-sm font-bold">Z</span>
      </div>
      <div>
        <div class="text-sm font-semibold text-white tracking-tight">ZeroClaw</div>
        <div class="text-[10px] text-gray-500">Admin Dashboard</div>
      </div>
    </div>
  </div>

  <div class="flex-1 py-3 px-2 space-y-0.5">
    <div class="px-2 py-1.5 text-[10px] text-gray-600 uppercase tracking-widest font-medium">System</div>
    <button onclick="showSection('overview')" id="nav-overview" class="nav-btn w-full text-left px-3 py-2 rounded-lg text-sm flex items-center gap-2.5 text-gray-400 hover:text-gray-200 hover:bg-white/5 nav-active">
      <span class="w-4 text-center text-xs">&#9632;</span> Overview
    </button>

    <div class="px-2 pt-3 pb-1.5 text-[10px] text-gray-600 uppercase tracking-widest font-medium">Traits</div>
    <button onclick="showSection('providers')" id="nav-providers" class="nav-btn w-full text-left px-3 py-2 rounded-lg text-sm flex items-center gap-2.5 text-gray-400 hover:text-gray-200 hover:bg-white/5">
      <span class="w-4 text-center text-xs">&#9883;</span> Providers <span id="nav-providers-count" class="ml-auto text-[10px] text-gray-600">0</span>
    </button>
    <button onclick="showSection('channels')" id="nav-channels" class="nav-btn w-full text-left px-3 py-2 rounded-lg text-sm flex items-center gap-2.5 text-gray-400 hover:text-gray-200 hover:bg-white/5">
      <span class="w-4 text-center text-xs">&#9993;</span> Channels <span id="nav-channels-count" class="ml-auto text-[10px] text-gray-600">0/14</span>
    </button>
    <button onclick="showSection('tools')" id="nav-tools" class="nav-btn w-full text-left px-3 py-2 rounded-lg text-sm flex items-center gap-2.5 text-gray-400 hover:text-gray-200 hover:bg-white/5">
      <span class="w-4 text-center text-xs">&#9881;</span> Tools <span id="nav-tools-count" class="ml-auto text-[10px] text-gray-600">37</span>
    </button>
    <button onclick="showSection('memory')" id="nav-memory" class="nav-btn w-full text-left px-3 py-2 rounded-lg text-sm flex items-center gap-2.5 text-gray-400 hover:text-gray-200 hover:bg-white/5">
      <span class="w-4 text-center text-xs">&#9683;</span> Memory
    </button>
    <button onclick="showSection('observers')" id="nav-observers" class="nav-btn w-full text-left px-3 py-2 rounded-lg text-sm flex items-center gap-2.5 text-gray-400 hover:text-gray-200 hover:bg-white/5">
      <span class="w-4 text-center text-xs">&#9673;</span> Observers
    </button>
    <button onclick="showSection('runtimes')" id="nav-runtimes" class="nav-btn w-full text-left px-3 py-2 rounded-lg text-sm flex items-center gap-2.5 text-gray-400 hover:text-gray-200 hover:bg-white/5">
      <span class="w-4 text-center text-xs">&#9654;</span> Runtimes
    </button>
    <button onclick="showSection('security')" id="nav-security" class="nav-btn w-full text-left px-3 py-2 rounded-lg text-sm flex items-center gap-2.5 text-gray-400 hover:text-gray-200 hover:bg-white/5">
      <span class="w-4 text-center text-xs">&#9888;</span> Security
    </button>
    <button onclick="showSection('tunnels')" id="nav-tunnels" class="nav-btn w-full text-left px-3 py-2 rounded-lg text-sm flex items-center gap-2.5 text-gray-400 hover:text-gray-200 hover:bg-white/5">
      <span class="w-4 text-center text-xs">&#8644;</span> Tunnels
    </button>

    <div class="px-2 pt-3 pb-1.5 text-[10px] text-gray-600 uppercase tracking-widest font-medium">Data</div>
    <button onclick="showSection('memories')" id="nav-memories" class="nav-btn w-full text-left px-3 py-2 rounded-lg text-sm flex items-center gap-2.5 text-gray-400 hover:text-gray-200 hover:bg-white/5">
      <span class="w-4 text-center text-xs">&#128451;</span> Entries
    </button>
    <button onclick="showSection('config')" id="nav-config" class="nav-btn w-full text-left px-3 py-2 rounded-lg text-sm flex items-center gap-2.5 text-gray-400 hover:text-gray-200 hover:bg-white/5">
      <span class="w-4 text-center text-xs">&#128196;</span> Config
    </button>
    <button onclick="showSection('metrics')" id="nav-metrics" class="nav-btn w-full text-left px-3 py-2 rounded-lg text-sm flex items-center gap-2.5 text-gray-400 hover:text-gray-200 hover:bg-white/5">
      <span class="w-4 text-center text-xs">&#128200;</span> Metrics
    </button>

    <div class="px-2 pt-3 pb-1.5 text-[10px] text-gray-600 uppercase tracking-widest font-medium">Control Plane</div>
    <button onclick="showSection('bots')" id="nav-bots" class="nav-btn w-full text-left px-3 py-2 rounded-lg text-sm flex items-center gap-2.5 text-gray-400 hover:text-gray-200 hover:bg-white/5">
      <span class="w-4 text-center text-xs">&#9741;</span> Bots <span id="nav-bots-count" class="ml-auto text-[10px] text-gray-600">0</span>
    </button>
    <button onclick="showSection('commands')" id="nav-commands" class="nav-btn w-full text-left px-3 py-2 rounded-lg text-sm flex items-center gap-2.5 text-gray-400 hover:text-gray-200 hover:bg-white/5">
      <span class="w-4 text-center text-xs">&#9655;</span> Commands
    </button>
    <button onclick="showSection('approvals')" id="nav-approvals" class="nav-btn w-full text-left px-3 py-2 rounded-lg text-sm flex items-center gap-2.5 text-gray-400 hover:text-gray-200 hover:bg-white/5">
      <span class="w-4 text-center text-xs">&#9745;</span> Approvals <span id="nav-approvals-count" class="ml-auto text-[10px] text-zc-amber hidden">0</span>
    </button>
    <button onclick="showSection('audit')" id="nav-audit" class="nav-btn w-full text-left px-3 py-2 rounded-lg text-sm flex items-center gap-2.5 text-gray-400 hover:text-gray-200 hover:bg-white/5">
      <span class="w-4 text-center text-xs">&#128220;</span> Audit
    </button>
    <button onclick="showSection('events')" id="nav-events" class="nav-btn w-full text-left px-3 py-2 rounded-lg text-sm flex items-center gap-2.5 text-gray-400 hover:text-gray-200 hover:bg-white/5">
      <span class="w-4 text-center text-xs">&#9889;</span> Events
    </button>
  </div>

  <div class="px-4 py-3 border-t border-zc-border">
    <div class="flex items-center gap-2">
      <span id="status-dot" class="w-2 h-2 rounded-full bg-zc-green pulse"></span>
      <span id="status-text" class="text-[10px] text-gray-500">Connected</span>
    </div>
  </div>
</nav>

<!-- MAIN CONTENT -->
<main class="flex-1 min-h-screen">
  <div class="px-6 py-5 scroll-area">

    <!-- OVERVIEW -->
    <div id="section-overview">
      <h1 class="text-lg font-semibold text-white mb-4">System Overview</h1>
      <div class="grid grid-cols-2 lg:grid-cols-4 gap-3 mb-6">
        <div class="bg-zc-card border border-zc-border rounded-xl p-4">
          <div class="text-[10px] text-gray-500 uppercase tracking-wider mb-1">Provider</div>
          <div id="ov-provider" class="text-base font-semibold text-white">-</div>
          <div id="ov-model" class="text-[11px] text-gray-500 mt-0.5 mono truncate">-</div>
        </div>
        <div class="bg-zc-card border border-zc-border rounded-xl p-4">
          <div class="text-[10px] text-gray-500 uppercase tracking-wider mb-1">Channels</div>
          <div id="ov-channels" class="text-base font-semibold text-white">0</div>
          <div id="ov-channels-list" class="text-[11px] text-gray-500 mt-0.5 truncate">-</div>
        </div>
        <div class="bg-zc-card border border-zc-border rounded-xl p-4">
          <div class="text-[10px] text-gray-500 uppercase tracking-wider mb-1">Memory</div>
          <div id="ov-memory" class="text-base font-semibold text-white">-</div>
          <div id="ov-runtime" class="text-[11px] text-gray-500 mt-0.5">-</div>
        </div>
        <div class="bg-zc-card border border-zc-border rounded-xl p-4">
          <div class="text-[10px] text-gray-500 uppercase tracking-wider mb-1">Security</div>
          <div id="ov-security" class="text-base font-semibold text-white">-</div>
          <div id="ov-sandbox" class="text-[11px] text-gray-500 mt-0.5">-</div>
        </div>
      </div>
      <div class="grid grid-cols-1 lg:grid-cols-2 gap-3">
        <div class="bg-zc-card border border-zc-border rounded-xl p-4">
          <h3 class="text-xs font-semibold text-white uppercase tracking-wider mb-3">Gateway</h3>
          <div id="ov-gateway" class="text-sm space-y-1.5 text-gray-400"></div>
        </div>
        <div class="bg-zc-card border border-zc-border rounded-xl p-4">
          <h3 class="text-xs font-semibold text-white uppercase tracking-wider mb-3">Agents</h3>
          <div id="ov-agents" class="text-sm space-y-1.5 text-gray-400"></div>
        </div>
      </div>
    </div>

    <!-- PROVIDERS -->
    <div id="section-providers" class="hidden">
      <div class="flex items-center justify-between mb-4">
        <div>
          <h1 class="text-lg font-semibold text-white">Providers</h1>
          <p class="text-xs text-gray-500 mt-0.5"><span id="prov-enabled" class="text-zc-green font-medium">0</span> of 21 providers available</p>
        </div>
        <div class="flex gap-2">
          <button onclick="showProviderModal()" class="admin-btn text-xs">Set Default</button>
          <button onclick="filterItems('providers','all')" class="filter-btn-providers text-xs px-3 py-1.5 rounded-lg bg-zc-accent/15 text-zc-accent font-medium" data-f="all">All</button>
          <button onclick="filterItems('providers','enabled')" class="filter-btn-providers text-xs px-3 py-1.5 rounded-lg bg-zc-card text-gray-400 border border-zc-border" data-f="enabled">Available</button>
        </div>
      </div>
      <div id="providers-grid" class="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4 gap-3"></div>
    </div>

    <!-- CHANNELS -->
    <div id="section-channels" class="hidden">
      <div class="flex items-center justify-between mb-4">
        <div>
          <h1 class="text-lg font-semibold text-white">Channels</h1>
          <p class="text-xs text-gray-500 mt-0.5"><span id="ch-enabled" class="text-zc-green font-medium">0</span> of 14 channels enabled</p>
        </div>
        <div class="flex gap-2">
          <button onclick="showChannelModal()" class="admin-btn text-xs">Configure Channel</button>
          <button onclick="filterItems('channels','all')" class="filter-btn-channels text-xs px-3 py-1.5 rounded-lg bg-zc-accent/15 text-zc-accent font-medium" data-f="all">All</button>
          <button onclick="filterItems('channels','enabled')" class="filter-btn-channels text-xs px-3 py-1.5 rounded-lg bg-zc-card text-gray-400 border border-zc-border" data-f="enabled">Enabled</button>
          <button onclick="filterItems('channels','disabled')" class="filter-btn-channels text-xs px-3 py-1.5 rounded-lg bg-zc-card text-gray-400 border border-zc-border" data-f="disabled">Available</button>
        </div>
      </div>
      <div id="channels-grid" class="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4 gap-3"></div>
    </div>

    <!-- TOOLS -->
    <div id="section-tools" class="hidden">
      <div class="flex items-center justify-between mb-4">
        <h1 class="text-lg font-semibold text-white">Tools <span class="text-sm text-gray-500 font-normal">(37 available)</span></h1>
        <div class="flex gap-2">
          <button onclick="filterItems('tools','all')" class="filter-btn-tools text-xs px-3 py-1.5 rounded-lg bg-zc-accent/15 text-zc-accent font-medium" data-f="all">All</button>
          <button onclick="toolCatFilter('')" id="tool-cat-all" class="text-xs px-3 py-1.5 rounded-lg bg-zc-card text-gray-400 border border-zc-border">Categories</button>
        </div>
      </div>
      <div id="tool-categories" class="flex flex-wrap gap-1.5 mb-4 hidden"></div>
      <div id="tools-grid" class="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4 gap-3"></div>
    </div>

    <!-- MEMORY BACKEND -->
    <div id="section-memory" class="hidden">
      <div class="flex items-center justify-between mb-1">
        <h1 class="text-lg font-semibold text-white">Memory Backend</h1>
        <button onclick="showSelectModal('memory','backend',['sqlite','lucid','postgres','markdown','none'])" class="admin-btn text-xs">Switch Backend</button>
      </div>
      <p class="text-xs text-gray-500 mb-4">Active: <span id="mem-active" class="text-zc-green font-medium mono">-</span></p>
      <div id="memory-grid" class="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-3"></div>
    </div>

    <!-- OBSERVERS -->
    <div id="section-observers" class="hidden">
      <div class="flex items-center justify-between mb-1">
        <h1 class="text-lg font-semibold text-white">Observers</h1>
        <button onclick="showSelectModal('observer','backend',['none','log','prometheus','otel'])" class="admin-btn text-xs">Switch Backend</button>
      </div>
      <p class="text-xs text-gray-500 mb-4">Active: <span id="obs-active" class="text-zc-green font-medium mono">-</span></p>
      <div id="observers-grid" class="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-3"></div>
    </div>

    <!-- RUNTIMES -->
    <div id="section-runtimes" class="hidden">
      <div class="flex items-center justify-between mb-1">
        <h1 class="text-lg font-semibold text-white">Runtime Adapters</h1>
        <button onclick="showSelectModal('runtime','kind',['native','docker','wasm'])" class="admin-btn text-xs">Switch Runtime</button>
      </div>
      <p class="text-xs text-gray-500 mb-4">Active: <span id="rt-active" class="text-zc-green font-medium mono">-</span></p>
      <div id="runtimes-grid" class="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-3"></div>
    </div>

    <!-- SECURITY -->
    <div id="section-security" class="hidden">
      <div class="flex items-center justify-between mb-4">
        <h1 class="text-lg font-semibold text-white">Security & Autonomy</h1>
        <button onclick="showSecurityModal()" class="admin-btn text-xs">Configure</button>
      </div>
      <div id="security-content"></div>
    </div>

    <!-- TUNNELS -->
    <div id="section-tunnels" class="hidden">
      <div class="flex items-center justify-between mb-1">
        <h1 class="text-lg font-semibold text-white">Tunnels</h1>
        <button onclick="showSelectModal('tunnel','provider',['none','cloudflare','tailscale','ngrok','custom'])" class="admin-btn text-xs">Switch Provider</button>
      </div>
      <p class="text-xs text-gray-500 mb-4">Active: <span id="tun-active" class="text-zc-green font-medium mono">-</span></p>
      <div id="tunnels-grid" class="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-3"></div>
    </div>

    <!-- MEMORY ENTRIES -->
    <div id="section-memories" class="hidden">
      <div class="bg-zc-card border border-zc-border rounded-xl p-4">
        <div class="flex items-center justify-between mb-3">
          <h3 class="text-xs font-semibold text-white uppercase tracking-wider">Memory Entries (<span id="mem-count">0</span>)</h3>
          <button onclick="loadMemories()" class="text-xs bg-zc-accent/15 text-zc-accent px-3 py-1.5 rounded-lg hover:bg-zc-accent/25 font-medium">Refresh</button>
        </div>
        <div id="memory-list" class="space-y-2 max-h-[600px] overflow-auto">Loading...</div>
      </div>
    </div>

    <!-- CONFIG -->
    <div id="section-config" class="hidden">
      <div class="bg-zc-card border border-zc-border rounded-xl p-4">
        <div class="flex items-center justify-between mb-3">
          <h3 class="text-xs font-semibold text-white uppercase tracking-wider">Configuration (secrets redacted)</h3>
          <button onclick="loadConfig()" class="text-xs bg-zc-accent/15 text-zc-accent px-3 py-1.5 rounded-lg hover:bg-zc-accent/25 font-medium">Refresh</button>
        </div>
        <pre id="config-json" class="text-xs overflow-auto max-h-[600px] text-gray-400 bg-zc-bg p-4 rounded-lg mono leading-relaxed">Loading...</pre>
      </div>
    </div>

    <!-- METRICS -->
    <div id="section-metrics" class="hidden">
      <div class="bg-zc-card border border-zc-border rounded-xl p-4">
        <div class="flex items-center justify-between mb-3">
          <h3 class="text-xs font-semibold text-white uppercase tracking-wider">Prometheus Metrics</h3>
          <button onclick="loadMetrics()" class="text-xs bg-zc-accent/15 text-zc-accent px-3 py-1.5 rounded-lg hover:bg-zc-accent/25 font-medium">Refresh</button>
        </div>
        <pre id="metrics-raw" class="text-xs overflow-auto max-h-[600px] text-gray-400 bg-zc-bg p-4 rounded-lg mono leading-relaxed">Loading...</pre>
      </div>
    </div>

    <!-- BOTS -->
    <div id="section-bots" class="hidden">
      <div class="flex items-center justify-between mb-4">
        <h1 class="text-lg font-semibold text-white">Bot Fleet</h1>
        <button onclick="loadBots()" class="text-xs bg-zc-accent/15 text-zc-accent px-3 py-1.5 rounded-lg hover:bg-zc-accent/25 font-medium">Refresh</button>
      </div>
      <div id="bots-grid" class="grid grid-cols-1 lg:grid-cols-2 xl:grid-cols-3 gap-3">
        <div class="text-xs text-gray-500">Loading bots...</div>
      </div>
    </div>

    <!-- BOT DETAIL (inline, toggled) -->
    <div id="section-bot-detail" class="hidden">
      <div class="flex items-center gap-3 mb-4">
        <button onclick="showSection('bots')" class="text-xs text-gray-400 hover:text-white">&larr; Back</button>
        <h1 class="text-lg font-semibold text-white" id="bot-detail-name">Bot Detail</h1>
        <span id="bot-detail-status" class="badge px-2 py-0.5 rounded-full"></span>
      </div>
      <div class="grid grid-cols-2 lg:grid-cols-4 gap-3 mb-4">
        <div class="bg-zc-card border border-zc-border rounded-xl p-3">
          <div class="text-[10px] text-gray-500 uppercase">Host</div>
          <div id="bot-detail-host" class="text-sm text-white mono mt-1">-</div>
        </div>
        <div class="bg-zc-card border border-zc-border rounded-xl p-3">
          <div class="text-[10px] text-gray-500 uppercase">Version</div>
          <div id="bot-detail-version" class="text-sm text-white mono mt-1">-</div>
        </div>
        <div class="bg-zc-card border border-zc-border rounded-xl p-3">
          <div class="text-[10px] text-gray-500 uppercase">Uptime</div>
          <div id="bot-detail-uptime" class="text-sm text-white mono mt-1">-</div>
        </div>
        <div class="bg-zc-card border border-zc-border rounded-xl p-3">
          <div class="text-[10px] text-gray-500 uppercase">Last Heartbeat</div>
          <div id="bot-detail-hb" class="text-sm text-white mono mt-1">-</div>
        </div>
      </div>
      <div class="flex gap-2 mb-4">
        <button onclick="showCommandModalFor()" class="admin-btn">Send Command</button>
        <button onclick="doDeleteBot()" class="admin-btn admin-btn-red">Remove Bot</button>
      </div>
      <div class="grid grid-cols-1 lg:grid-cols-2 gap-4">
        <div class="bg-zc-card border border-zc-border rounded-xl p-4">
          <h3 class="text-xs font-semibold text-white uppercase tracking-wider mb-3">Recent Commands</h3>
          <div id="bot-detail-commands" class="space-y-2 text-xs text-gray-400">Loading...</div>
        </div>
        <div class="bg-zc-card border border-zc-border rounded-xl p-4">
          <h3 class="text-xs font-semibold text-white uppercase tracking-wider mb-3">Recent Events</h3>
          <div id="bot-detail-events" class="space-y-2 text-xs text-gray-400">Loading...</div>
        </div>
      </div>
    </div>

    <!-- COMMANDS -->
    <div id="section-commands" class="hidden">
      <div class="flex items-center justify-between mb-4">
        <h1 class="text-lg font-semibold text-white">Commands</h1>
        <div class="flex gap-2">
          <button onclick="showCommandModal()" class="admin-btn">New Command</button>
          <button onclick="loadCommands()" class="text-xs bg-zc-accent/15 text-zc-accent px-3 py-1.5 rounded-lg hover:bg-zc-accent/25 font-medium">Refresh</button>
        </div>
      </div>
      <div class="bg-zc-card border border-zc-border rounded-xl overflow-hidden">
        <table class="w-full text-xs">
          <thead>
            <tr class="border-b border-zc-border text-gray-500 text-left">
              <th class="px-4 py-2.5 font-medium">ID</th>
              <th class="px-4 py-2.5 font-medium">Bot</th>
              <th class="px-4 py-2.5 font-medium">Kind</th>
              <th class="px-4 py-2.5 font-medium">Status</th>
              <th class="px-4 py-2.5 font-medium">Created</th>
              <th class="px-4 py-2.5 font-medium">Result</th>
            </tr>
          </thead>
          <tbody id="commands-tbody" class="text-gray-300"></tbody>
        </table>
      </div>
    </div>

    <!-- APPROVALS -->
    <div id="section-approvals" class="hidden">
      <div class="flex items-center justify-between mb-4">
        <h1 class="text-lg font-semibold text-white">Approval Queue</h1>
        <button onclick="loadApprovals()" class="text-xs bg-zc-accent/15 text-zc-accent px-3 py-1.5 rounded-lg hover:bg-zc-accent/25 font-medium">Refresh</button>
      </div>
      <div id="approvals-list" class="space-y-3">
        <div class="text-xs text-gray-500">Loading approvals...</div>
      </div>
    </div>

    <!-- AUDIT -->
    <div id="section-audit" class="hidden">
      <div class="flex items-center justify-between mb-4">
        <h1 class="text-lg font-semibold text-white">Audit Log</h1>
        <button onclick="loadAudit()" class="text-xs bg-zc-accent/15 text-zc-accent px-3 py-1.5 rounded-lg hover:bg-zc-accent/25 font-medium">Refresh</button>
      </div>
      <div class="bg-zc-card border border-zc-border rounded-xl overflow-hidden">
        <table class="w-full text-xs">
          <thead>
            <tr class="border-b border-zc-border text-gray-500 text-left">
              <th class="px-4 py-2.5 font-medium">Time</th>
              <th class="px-4 py-2.5 font-medium">Actor</th>
              <th class="px-4 py-2.5 font-medium">Action</th>
              <th class="px-4 py-2.5 font-medium">Target</th>
              <th class="px-4 py-2.5 font-medium">Detail</th>
            </tr>
          </thead>
          <tbody id="audit-tbody" class="text-gray-300"></tbody>
        </table>
      </div>
    </div>

    <!-- EVENTS (Live) -->
    <div id="section-events" class="hidden">
      <div class="flex items-center justify-between mb-4">
        <h1 class="text-lg font-semibold text-white">Live Events</h1>
        <div class="flex items-center gap-3">
          <span id="sse-status" class="text-[10px] text-gray-500">Disconnected</span>
          <button onclick="toggleSSE()" id="sse-toggle" class="text-xs bg-zc-green/15 text-zc-green px-3 py-1.5 rounded-lg hover:bg-zc-green/25 font-medium">Connect</button>
          <button onclick="loadEvents()" class="text-xs bg-zc-accent/15 text-zc-accent px-3 py-1.5 rounded-lg hover:bg-zc-accent/25 font-medium">History</button>
        </div>
      </div>
      <div id="events-stream" class="space-y-1 max-h-[600px] overflow-y-auto bg-zc-card border border-zc-border rounded-xl p-4">
        <div class="text-xs text-gray-500">Click Connect to start SSE stream...</div>
      </div>
    </div>

  </div>
</main>

<script>
var BASE = window.location.origin;
var SYS = null;
var channelData = [];
var currentSection = 'overview';
var toolCat = '';

var SECTIONS = ['overview','providers','channels','tools','memory','observers','runtimes','security','tunnels','memories','config','metrics','bots','bot-detail','commands','approvals','audit','events'];

var CAT_COLORS = {
  frontier: 'bg-zc-accent/15 text-zc-accent',
  aggregator: 'bg-zc-purple/15 text-zc-purple',
  inference: 'bg-zc-cyan/15 text-zc-cyan',
  local: 'bg-zc-green/15 text-zc-green',
  search: 'bg-zc-amber/15 text-zc-amber',
  china: 'bg-zc-pink/15 text-zc-pink',
  messaging: 'bg-zc-accent/15 text-zc-accent',
  communication: 'bg-zc-cyan/15 text-zc-cyan',
  integration: 'bg-zc-amber/15 text-zc-amber',
  enterprise: 'bg-zc-purple/15 text-zc-purple',
  system: 'bg-gray-700/40 text-gray-300',
  browser: 'bg-zc-cyan/15 text-zc-cyan',
  network: 'bg-zc-amber/15 text-zc-amber',
  scheduling: 'bg-zc-purple/15 text-zc-purple',
  wallet: 'bg-zc-green/15 text-zc-green',
  hardware: 'bg-zc-red/15 text-zc-red',
  agent: 'bg-zc-accent/15 text-zc-accent',
  soul: 'bg-zc-pink/15 text-zc-pink',
  media: 'bg-zc-lime/15 text-zc-lime',
  memory: 'bg-zc-cyan/15 text-zc-cyan',
};

var CHANNEL_ICONS = {
  telegram:'\u2708\uFE0F', discord:'\uD83C\uDFAE', slack:'\uD83D\uDCAC', mattermost:'\uD83D\uDD17',
  matrix:'\uD83C\uDF10', whatsapp:'\uD83D\uDCF1', signal:'\uD83D\uDD12', email:'\u2709\uFE0F',
  irc:'\uD83D\uDCBB', webhook:'\u26A1', imessage:'\uD83D\uDCE8', lark:'\uD83D\uDC26',
  dingtalk:'\uD83D\uDD14', qq:'\uD83D\uDC27'
};

function showSection(name) {
  currentSection = name;
  SECTIONS.forEach(function(s) {
    var el = document.getElementById('section-' + s);
    if (el) el.classList.toggle('hidden', s !== name);
    var nav = document.getElementById('nav-' + s);
    if (nav) {
      if (s === name) nav.classList.add('nav-active');
      else nav.classList.remove('nav-active');
    }
  });
  if (name === 'memories') loadMemories();
  if (name === 'config') loadConfig();
  if (name === 'metrics') loadMetrics();
  if (name === 'bots') loadBots();
  if (name === 'commands') loadCommands();
  if (name === 'approvals') loadApprovals();
  if (name === 'audit') loadAudit();
  if (name === 'events') loadEvents();
}

function setText(id, text) { var el = document.getElementById(id); if (el) el.textContent = text; }

function makeCard(opts) {
  var d = document.createElement('div');
  var barClass = opts.active ? 'active-bar' : (opts.enabled ? 'enabled-bar' : 'available-bar');
  d.className = 'card bg-zc-card border border-zc-border rounded-xl p-4 fade-in ' + barClass;

  var statusHtml = '';
  if (opts.active) {
    statusHtml = '<span class="w-2 h-2 rounded-full bg-zc-accent pulse"></span><span class="text-zc-accent text-[10px] font-medium uppercase tracking-wider">Active</span>';
  } else if (opts.enabled) {
    statusHtml = '<span class="w-2 h-2 rounded-full bg-zc-green pulse"></span><span class="text-zc-green text-[10px] font-medium uppercase tracking-wider">Ready</span>';
  } else {
    statusHtml = '<span class="w-2 h-2 rounded-full bg-gray-600"></span><span class="text-gray-500 text-[10px] font-medium uppercase tracking-wider">Not configured</span>';
  }

  var catHtml = opts.category ? '<span class="badge ' + (CAT_COLORS[opts.category] || 'bg-gray-800 text-gray-400') + ' px-1.5 py-0.5 rounded-full font-medium uppercase tracking-wider">' + opts.category + '</span>' : '';

  var iconHtml = opts.icon ? '<span class="text-lg mr-1">' + opts.icon + '</span>' : '';

  var envHtml = opts.env_var ? '<div class="mt-2"><span class="text-[10px] text-gray-600 uppercase tracking-wider">Env var</span><div class="mt-0.5"><span class="inline-block bg-zc-bg text-gray-400 text-[10px] px-1.5 py-0.5 rounded mono">' + opts.env_var + '</span></div></div>' : '';

  var keysHtml = '';
  if (opts.required_keys && opts.required_keys.length) {
    keysHtml = '<div class="mt-2"><span class="text-[10px] text-gray-600 uppercase tracking-wider">Required</span><div class="mt-0.5 flex flex-wrap gap-1">' +
      opts.required_keys.map(function(k) { return '<span class="inline-block bg-zc-bg text-gray-400 text-[10px] px-1.5 py-0.5 rounded mono">' + k + '</span>'; }).join('') +
      '</div></div>';
  }
  if (opts.optional_keys && opts.optional_keys.length) {
    var shown = opts.optional_keys.slice(0, 3);
    var more = opts.optional_keys.length > 3 ? ' <span class="text-gray-600 text-[10px]">+' + (opts.optional_keys.length - 3) + '</span>' : '';
    keysHtml += '<div class="mt-1.5"><span class="text-[10px] text-gray-600">Optional: </span>' +
      shown.map(function(k) { return '<span class="inline-block bg-zc-bg text-gray-500 text-[10px] px-1.5 py-0.5 rounded mono">' + k + '</span>'; }).join(' ') + more + '</div>';
  }

  d.innerHTML =
    '<div class="flex items-start justify-between mb-2">' +
      '<div class="flex items-center gap-1.5">' + iconHtml +
        '<div><div class="text-sm font-semibold text-white">' + opts.label + '</div>' + catHtml + '</div>' +
      '</div>' +
      '<div class="flex items-center gap-1.5">' + statusHtml + '</div>' +
    '</div>' +
    envHtml + keysHtml +
    '<div class="text-[11px] text-gray-500 leading-relaxed mt-2 border-t border-zc-border pt-2">' + (opts.hint || '') + '</div>';

  return d;
}

function filterItems(section, mode) {
  var btns = document.querySelectorAll('.filter-btn-' + section);
  btns.forEach(function(b) {
    if (b.getAttribute('data-f') === mode) {
      b.className = 'filter-btn-' + section + ' text-xs px-3 py-1.5 rounded-lg bg-zc-accent/15 text-zc-accent font-medium';
    } else {
      b.className = 'filter-btn-' + section + ' text-xs px-3 py-1.5 rounded-lg bg-zc-card text-gray-400 border border-zc-border';
    }
  });

  if (section === 'providers') renderProviders(mode);
  if (section === 'channels') renderChannels(mode);
  if (section === 'tools') renderTools(mode);
}

function renderProviders(filter) {
  if (!SYS) return;
  var grid = document.getElementById('providers-grid');
  grid.innerHTML = '';
  var items = SYS.providers.items;
  var count = 0;
  items.forEach(function(p) {
    if (p.enabled) count++;
    if (filter === 'enabled' && !p.enabled) return;
    grid.appendChild(makeCard({
      label: p.label,
      category: p.category,
      enabled: p.enabled,
      active: SYS.providers.active === p.name,
      env_var: p.env_var,
      hint: p.hint,
    }));
  });
  setText('prov-enabled', String(count));
  setText('nav-providers-count', String(count));
}

function renderChannels(filter) {
  if (!channelData.length) return;
  var grid = document.getElementById('channels-grid');
  grid.innerHTML = '';
  var filtered = channelData.filter(function(ch) {
    if (filter === 'enabled') return ch.enabled;
    if (filter === 'disabled') return !ch.enabled;
    return true;
  });
  if (!filtered.length) {
    grid.innerHTML = '<div class="col-span-full text-sm text-gray-500 text-center py-8">No channels match.</div>';
    return;
  }
  filtered.forEach(function(ch) {
    grid.appendChild(makeCard({
      label: ch.label,
      category: ch.category,
      enabled: ch.enabled,
      icon: CHANNEL_ICONS[ch.name],
      required_keys: ch.required_keys,
      optional_keys: ch.optional_keys,
      hint: ch.hint,
    }));
  });
}

function renderTools(filter) {
  if (!SYS) return;
  var grid = document.getElementById('tools-grid');
  grid.innerHTML = '';
  var items = SYS.tools.items;
  items.forEach(function(t) {
    if (toolCat && t.category !== toolCat) return;
    grid.appendChild(makeCard({
      label: t.name,
      category: t.category,
      enabled: true,
      hint: t.hint,
    }));
  });
}

function toolCatFilter(cat) {
  toolCat = cat;
  var catEl = document.getElementById('tool-categories');
  if (!SYS) return;
  if (!cat) {
    catEl.classList.toggle('hidden');
    if (!catEl.classList.contains('hidden')) {
      catEl.innerHTML = '';
      var cats = {};
      SYS.tools.items.forEach(function(t) { cats[t.category] = (cats[t.category] || 0) + 1; });
      Object.keys(cats).sort().forEach(function(c) {
        var b = document.createElement('button');
        b.className = 'text-[10px] px-2.5 py-1 rounded-lg border border-zc-border text-gray-400 hover:text-white hover:border-zc-accent';
        b.textContent = c + ' (' + cats[c] + ')';
        b.onclick = function() { toolCatFilter(c); };
        catEl.appendChild(b);
      });
      var all = document.createElement('button');
      all.className = 'text-[10px] px-2.5 py-1 rounded-lg bg-zc-accent/15 text-zc-accent font-medium';
      all.textContent = 'Show all';
      all.onclick = function() { toolCat = ''; renderTools('all'); };
      catEl.appendChild(all);
    }
  } else {
    renderTools('all');
  }
}

function renderSingleSelect(gridId, items, activeKey) {
  var grid = document.getElementById(gridId);
  if (!grid) return;
  grid.innerHTML = '';
  items.forEach(function(item) {
    grid.appendChild(makeCard({
      label: item.label,
      enabled: item.enabled || item.active,
      active: item.enabled || item.active,
      hint: item.hint,
      required_keys: item.required_keys,
      optional_keys: item.optional_keys,
      env_var: item.env_var,
    }));
  });
}

function renderSecurity() {
  if (!SYS) return;
  var sec = SYS.security;
  var el = document.getElementById('security-content');
  el.innerHTML = '';

  var levels = document.createElement('div');
  levels.className = 'grid grid-cols-1 md:grid-cols-3 gap-3 mb-4';
  sec.levels.forEach(function(l) {
    levels.appendChild(makeCard({
      label: l.label,
      enabled: l.active,
      active: l.active,
      hint: l.hint,
    }));
  });
  el.appendChild(levels);

  var details = document.createElement('div');
  details.className = 'bg-zc-card border border-zc-border rounded-xl p-4';
  var wsOnly = sec.workspace_only ? 'Yes' : 'No';
  var sandbox = sec.sandbox_enabled == null ? 'Auto' : (sec.sandbox_enabled ? 'Yes' : 'No');
  var approved = sec.auto_approve && sec.auto_approve.length ? sec.auto_approve.join(', ') : 'None';
  details.innerHTML =
    '<h3 class="text-xs font-semibold text-white uppercase tracking-wider mb-3">Details</h3>' +
    '<div class="grid grid-cols-2 gap-3 text-sm">' +
      '<div><span class="text-gray-500">Workspace only:</span> <span class="text-white">' + wsOnly + '</span></div>' +
      '<div><span class="text-gray-500">Sandbox:</span> <span class="text-white">' + sandbox + '</span></div>' +
      '<div class="col-span-2"><span class="text-gray-500">Auto-approved tools:</span> <span class="text-white mono text-xs">' + approved + '</span></div>' +
    '</div>';
  el.appendChild(details);
}

async function loadSystem() {
  try {
    var r = await fetch(BASE + '/api/system');
    SYS = await r.json();

    setText('mem-active', SYS.memory.active);
    setText('obs-active', SYS.observers.active);
    setText('rt-active', SYS.runtimes.active);
    setText('tun-active', SYS.tunnels.active);

    renderProviders('all');
    renderSingleSelect('memory-grid', SYS.memory.items);
    renderSingleSelect('observers-grid', SYS.observers.items);
    renderSingleSelect('runtimes-grid', SYS.runtimes.items);
    renderSingleSelect('tunnels-grid', SYS.tunnels.items);
    renderSecurity();
    renderTools('all');
  } catch(e) { console.error('System load error:', e); }
}

async function loadChannels() {
  try {
    var r = await fetch(BASE + '/api/channels');
    var d = await r.json();
    channelData = d.channels;
    setText('ch-enabled', String(d.enabled));
    setText('nav-channels-count', d.enabled + '/14');
    renderChannels('all');
  } catch(e) { console.error('Channels load error:', e); }
}

async function loadStatus() {
  try {
    var r = await fetch(BASE + '/api/status');
    var d = await r.json();
    setText('ov-provider', d.provider || 'none');
    setText('ov-model', d.model || 'none');
    setText('ov-channels', String(d.channels_count));
    setText('ov-channels-list', d.channels.join(', ') || 'none');
    setText('ov-memory', d.memory_backend);
    setText('ov-runtime', 'Runtime: native');
    setText('ov-security', d.security.autonomy_level);
    setText('ov-sandbox', 'Sandbox: ' + (d.security.sandbox_enabled == null ? 'auto' : d.security.sandbox_enabled));

    var gw = document.getElementById('ov-gateway');
    gw.innerHTML = '';
    [['Host', d.gateway.host + ':' + d.gateway.port], ['Pairing', d.gateway.require_pairing ? 'Required' : 'Disabled'],
     ['Identity', d.identity.format]].forEach(function(p) {
      var div = document.createElement('div');
      div.innerHTML = '<span class="text-gray-500">' + p[0] + ':</span> ' + (p[1] || '-');
      gw.appendChild(div);
    });

    var ag = document.getElementById('ov-agents');
    ag.innerHTML = '';
    if (!d.agents.length) { ag.textContent = 'No delegate agents configured'; }
    else { d.agents.forEach(function(a) {
      var div = document.createElement('div');
      div.className = 'flex items-center gap-2';
      div.innerHTML = '<span class="w-1.5 h-1.5 rounded-full bg-zc-green"></span>' + a;
      ag.appendChild(div);
    }); }
  } catch(e) {
    document.getElementById('status-dot').className = 'w-2 h-2 rounded-full bg-zc-red';
    setText('status-text', 'Disconnected');
  }
}

async function loadConfig() {
  try {
    var r = await fetch(BASE + '/api/config');
    var d = await r.json();
    setText('config-json', JSON.stringify(d, null, 2));
  } catch(e) { setText('config-json', 'Error: ' + e.message); }
}

async function loadMemories() {
  try {
    var r = await fetch(BASE + '/api/memories');
    var d = await r.json();
    setText('mem-count', String(d.count));
    var container = document.getElementById('memory-list');
    container.textContent = '';
    if (!d.entries.length) { container.textContent = 'No memories stored yet'; return; }
    d.entries.forEach(function(e) {
      var card = document.createElement('div');
      card.className = 'bg-zc-bg p-3 rounded-lg border border-zc-border';
      card.innerHTML =
        '<div class="flex justify-between items-center mb-1">' +
          '<span class="text-xs font-semibold text-zc-accent mono">' + e.key + '</span>' +
          '<span class="text-[10px] text-gray-600 uppercase tracking-wider">' + e.category + '</span>' +
        '</div>' +
        '<div class="text-xs text-gray-400 leading-relaxed">' + e.content + '</div>' +
        '<div class="text-[10px] text-gray-600 mt-1.5 mono">' + e.timestamp + '</div>';
      container.appendChild(card);
    });
  } catch(e) { document.getElementById('memory-list').textContent = 'Error: ' + e.message; }
}

async function loadMetrics() {
  try {
    var r = await fetch(BASE + '/api/metrics');
    var text = await r.text();
    setText('metrics-raw', text);
  } catch(e) { setText('metrics-raw', 'Error: ' + e.message); }
}

loadSystem();
loadChannels();
loadStatus();
setInterval(loadStatus, 15000);

var AUTH_TOKEN = localStorage.getItem('zc_token') || '';

function toast(msg, type) {
  var t = document.createElement('div');
  t.className = 'toast toast-' + (type || 'ok');
  t.textContent = msg;
  document.body.appendChild(t);
  setTimeout(function() { t.remove(); }, 4000);
}

async function adminPost(path, body) {
  try {
    var r = await fetch(BASE + '/api/admin/' + path, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', 'Authorization': 'Bearer ' + AUTH_TOKEN },
      body: JSON.stringify(body),
    });
    var d = await r.json();
    if (!r.ok) { toast(d.error || 'Error', 'err'); return null; }
    if (d.restart_required) toast('Saved. Restart daemon to apply changes.', 'warn');
    else toast('Saved successfully', 'ok');
    loadSystem(); loadChannels(); loadStatus();
    return d;
  } catch(e) { toast('Network error: ' + e.message, 'err'); return null; }
}

function closeModal() {
  var m = document.querySelector('.modal-bg');
  if (m) m.remove();
}

function createModal(title, html) {
  closeModal();
  var bg = document.createElement('div');
  bg.className = 'modal-bg';
  bg.onclick = function(e) { if (e.target === bg) closeModal(); };
  bg.innerHTML = '<div class="modal-box"><div class="flex items-center justify-between mb-4"><h2 class="text-base font-semibold text-white">' + title + '</h2><button onclick="closeModal()" class="text-gray-500 hover:text-white text-lg">&times;</button></div>' + html + '</div>';
  document.body.appendChild(bg);
}

function showProviderModal() {
  if (!SYS) return;
  var options = SYS.providers.items.filter(function(p) { return p.enabled; }).map(function(p) { return p.name; });
  var html = '<div class="space-y-3"><div><label class="text-xs text-gray-500 block mb-1">Provider</label><select id="m-provider" class="admin-input">' +
    options.map(function(o) { return '<option value="' + o + '"' + (SYS.providers.active === o ? ' selected' : '') + '>' + o + '</option>'; }).join('') +
    '</select></div>' +
    '<div><label class="text-xs text-gray-500 block mb-1">Model (optional)</label><input id="m-model" class="admin-input" placeholder="e.g. gpt-4o"></div>' +
    '<div class="flex gap-2 mt-4"><button onclick="doSetProvider()" class="admin-btn flex-1">Save</button><button onclick="closeModal()" class="admin-btn flex-1" style="opacity:0.5">Cancel</button></div></div>';
  createModal('Set Default Provider', html);
}

function doSetProvider() {
  var p = document.getElementById('m-provider').value;
  var m = document.getElementById('m-model').value;
  var body = { provider: p };
  if (m) body.model = m;
  adminPost('provider', body);
  closeModal();
}

function showChannelModal(name) {
  var channels = ['telegram','discord','slack','whatsapp','webhook','matrix','mattermost','signal','email','irc','imessage','lark'];
  var html = '<div class="space-y-3"><div><label class="text-xs text-gray-500 block mb-1">Channel</label><select id="m-chan-name" class="admin-input" onchange="updateChannelFields()">' +
    channels.map(function(c) { return '<option value="' + c + '"' + (name === c ? ' selected' : '') + '>' + c + '</option>'; }).join('') +
    '</select></div>' +
    '<div id="m-chan-fields"></div>' +
    '<div class="flex gap-2 mt-4"><button onclick="doSetChannel()" class="admin-btn flex-1">Save</button><button onclick="doDeleteChannel()" class="admin-btn admin-btn-red flex-1">Remove</button><button onclick="closeModal()" class="admin-btn flex-1" style="opacity:0.5">Cancel</button></div></div>';
  createModal('Configure Channel', html);
  updateChannelFields();
}

var CHAN_FIELDS = {
  telegram: [{k:'bot_token',l:'Bot Token',r:true},{k:'allowed_users',l:'Allowed Users (comma-sep)',r:false}],
  discord: [{k:'bot_token',l:'Bot Token',r:true},{k:'allowed_users',l:'Allowed Users (comma-sep)',r:false}],
  slack: [{k:'bot_token',l:'Bot Token',r:true},{k:'app_token',l:'App Token',r:true},{k:'allowed_users',l:'Allowed Users (comma-sep)',r:false}],
  whatsapp: [{k:'phone_number_id',l:'Phone Number ID',r:true},{k:'access_token',l:'Access Token',r:true},{k:'verify_token',l:'Verify Token',r:false}],
  webhook: [{k:'url',l:'Webhook URL',r:true},{k:'secret',l:'Secret',r:false}],
  matrix: [{k:'homeserver_url',l:'Homeserver URL',r:true},{k:'access_token',l:'Access Token',r:true}],
  mattermost: [{k:'url',l:'Server URL',r:true},{k:'token',l:'Bot Token',r:true}],
  signal: [{k:'phone_number',l:'Phone Number',r:true},{k:'signal_cli_path',l:'Signal CLI Path',r:false}],
  email: [{k:'imap_host',l:'IMAP Host',r:true},{k:'smtp_host',l:'SMTP Host',r:true},{k:'username',l:'Username',r:true},{k:'password',l:'Password',r:true}],
  irc: [{k:'server',l:'Server',r:true},{k:'nickname',l:'Nickname',r:true},{k:'channels',l:'Channels (comma-sep)',r:true}],
  imessage: [{k:'applescript_bridge',l:'AppleScript Bridge',r:false}],
  lark: [{k:'app_id',l:'App ID',r:true},{k:'app_secret',l:'App Secret',r:true}],
};

function updateChannelFields() {
  var name = document.getElementById('m-chan-name').value;
  var fields = CHAN_FIELDS[name] || [];
  var container = document.getElementById('m-chan-fields');
  container.innerHTML = fields.map(function(f) {
    return '<div class="mt-2"><label class="text-xs text-gray-500 block mb-1">' + f.l + (f.r ? ' *' : '') + '</label>' +
      '<input id="m-cf-' + f.k + '" class="admin-input" placeholder="' + f.l + '"></div>';
  }).join('');
}

function doSetChannel() {
  var name = document.getElementById('m-chan-name').value;
  var fields = CHAN_FIELDS[name] || [];
  var body = {};
  fields.forEach(function(f) {
    var el = document.getElementById('m-cf-' + f.k);
    if (el && el.value) body[f.k] = el.value;
  });
  adminPost('channel/' + name, body);
  closeModal();
}

function doDeleteChannel() {
  var name = document.getElementById('m-chan-name').value;
  if (!confirm('Remove channel ' + name + '?')) return;
  adminPost('channel/' + name + '/delete', {});
  closeModal();
}

function showSelectModal(endpoint, key, options) {
  var html = '<div class="space-y-2">' +
    options.map(function(o) {
      return '<div onclick="doSelect(\'' + endpoint + '\',\'' + key + '\',\'' + o + '\')" class="radio-card bg-zc-card border border-zc-border rounded-lg p-3 flex items-center gap-3">' +
        '<span class="text-sm text-white font-medium">' + o + '</span></div>';
    }).join('') +
    '<div class="mt-3"><button onclick="closeModal()" class="admin-btn w-full" style="opacity:0.5">Cancel</button></div></div>';
  createModal('Select ' + key.charAt(0).toUpperCase() + key.slice(1), html);
}

function doSelect(endpoint, key, value) {
  var body = {};
  body[key] = value;
  adminPost(endpoint, body);
  closeModal();
}

function showSecurityModal() {
  if (!SYS) return;
  var sec = SYS.security;
  var curLevel = sec.levels.find(function(l) { return l.active; });
  var curName = curLevel ? curLevel.label : 'Supervised';
  var html = '<div class="space-y-3">' +
    '<div><label class="text-xs text-gray-500 block mb-1">Autonomy Level</label><select id="m-sec-level" class="admin-input">' +
      '<option value="readonly"' + (curName==='ReadOnly'?' selected':'') + '>ReadOnly</option>' +
      '<option value="supervised"' + (curName==='Supervised'?' selected':'') + '>Supervised</option>' +
      '<option value="full"' + (curName==='Full'?' selected':'') + '>Full</option>' +
    '</select></div>' +
    '<div><label class="text-xs text-gray-500 block mb-1">Workspace Only</label><select id="m-sec-ws" class="admin-input">' +
      '<option value="true"' + (sec.workspace_only?' selected':'') + '>Yes</option>' +
      '<option value="false"' + (!sec.workspace_only?' selected':'') + '>No</option>' +
    '</select></div>' +
    '<div><label class="text-xs text-gray-500 block mb-1">Auto-Approve Tools (comma-sep)</label>' +
      '<input id="m-sec-approve" class="admin-input" value="' + (sec.auto_approve||[]).join(', ') + '"></div>' +
    '<div class="flex gap-2 mt-4"><button onclick="doSetSecurity()" class="admin-btn flex-1">Save</button><button onclick="closeModal()" class="admin-btn flex-1" style="opacity:0.5">Cancel</button></div></div>';
  createModal('Security Configuration', html);
}

function doSetSecurity() {
  var level = document.getElementById('m-sec-level').value;
  var ws = document.getElementById('m-sec-ws').value === 'true';
  var approveStr = document.getElementById('m-sec-approve').value;
  var approve = approveStr ? approveStr.split(',').map(function(s) { return s.trim(); }).filter(Boolean) : [];
  adminPost('security', { level: level, workspace_only: ws, auto_approve: approve });
  closeModal();
}

function showTokenModal() {
  createModal('Set Auth Token', '<div class="space-y-3"><p class="text-xs text-gray-400">Enter your bearer token from pairing to enable admin actions.</p><input id="m-token" class="admin-input" type="password" placeholder="Bearer token" value="' + AUTH_TOKEN + '"><div class="flex gap-2 mt-4"><button onclick="doSetToken()" class="admin-btn flex-1">Save</button><button onclick="closeModal()" class="admin-btn flex-1" style="opacity:0.5">Cancel</button></div></div>');
}

function doSetToken() {
  AUTH_TOKEN = document.getElementById('m-token').value;
  localStorage.setItem('zc_token', AUTH_TOKEN);
  toast('Token saved', 'ok');
  closeModal();
}

var currentBotId = null;
var sseSource = null;

var CMD_STATUS_COLORS = {
  pending: 'bg-zc-amber/15 text-zc-amber',
  pending_approval: 'bg-zc-purple/15 text-zc-purple',
  approved: 'bg-zc-accent/15 text-zc-accent',
  acked: 'bg-zc-green/15 text-zc-green',
  running: 'bg-zc-cyan/15 text-zc-cyan',
  failed: 'bg-zc-red/15 text-zc-red',
  rejected: 'bg-zc-red/15 text-zc-red',
};

var BOT_STATUS_COLORS = {
  online: 'bg-zc-green/15 text-zc-green border-zc-green/30',
  offline: 'bg-gray-700/40 text-gray-400 border-gray-600',
  unknown: 'bg-zc-amber/15 text-zc-amber border-zc-amber/30',
  degraded: 'bg-zc-red/15 text-zc-red border-zc-red/30',
};

function controlFetch(path) {
  var headers = {'Content-Type':'application/json'};
  if (AUTH_TOKEN) headers['Authorization'] = 'Bearer ' + AUTH_TOKEN;
  return fetch(BASE + '/api/control/' + path, {headers:headers}).then(function(r){return r.json();});
}

function controlPost(path, body) {
  var headers = {'Content-Type':'application/json'};
  if (AUTH_TOKEN) headers['Authorization'] = 'Bearer ' + AUTH_TOKEN;
  return fetch(BASE + '/api/control/' + path, {method:'POST',headers:headers,body:JSON.stringify(body)}).then(function(r){return r.json();});
}

function esc(s) { var d=document.createElement('span'); d.textContent=s; return d.innerHTML; }

function loadBots() {
  controlFetch('bots').then(function(data) {
    var grid = document.getElementById('bots-grid');
    if (!data.bots || !data.bots.length) { grid.textContent='No bots registered. Bots self-register via heartbeat.'; return; }
    var el = document.getElementById('nav-bots-count');
    if (el) el.textContent = data.bots.length;
    grid.textContent = '';
    data.bots.forEach(function(b) {
      var sc = BOT_STATUS_COLORS[b.status] || BOT_STATUS_COLORS.unknown;
      var uptimeStr = b.uptime_secs > 3600 ? Math.floor(b.uptime_secs/3600)+'h' : b.uptime_secs > 60 ? Math.floor(b.uptime_secs/60)+'m' : b.uptime_secs+'s';
      var card = document.createElement('div');
      card.className = 'card bg-zc-card border border-zc-border rounded-xl p-4 cursor-pointer fade-in';
      card.onclick = function(){showBotDetail(b.id);};
      card.innerHTML =
        '<div class="flex items-start justify-between mb-2">' +
          '<div><div class="text-sm font-semibold text-white">'+esc(b.name)+'</div><div class="text-[10px] text-gray-500 mono mt-0.5">'+esc(b.id.substring(0,8))+'...</div></div>' +
          '<span class="badge '+sc+' px-2 py-0.5 rounded-full border font-medium uppercase">'+esc(b.status)+'</span>' +
        '</div>' +
        '<div class="grid grid-cols-2 gap-2 mt-3 text-[11px]">' +
          '<div><span class="text-gray-500">Host</span><div class="text-gray-300 mono">'+esc(b.host+':'+b.port)+'</div></div>' +
          '<div><span class="text-gray-500">Version</span><div class="text-gray-300 mono">'+esc(b.version)+'</div></div>' +
          '<div><span class="text-gray-500">Uptime</span><div class="text-gray-300">'+esc(uptimeStr)+'</div></div>' +
          '<div><span class="text-gray-500">Provider</span><div class="text-gray-300">'+esc(b.provider)+'</div></div>' +
        '</div>';
      grid.appendChild(card);
    });
  });
}

function showBotDetail(botId) {
  currentBotId = botId;
  controlFetch('bots/'+encodeURIComponent(botId)).then(function(data) {
    var b = data.bot;
    if (!b) { toast('Bot not found','err'); return; }
    setText('bot-detail-name', b.name);
    var sc = BOT_STATUS_COLORS[b.status] || BOT_STATUS_COLORS.unknown;
    var statusEl = document.getElementById('bot-detail-status');
    statusEl.className = 'badge px-2 py-0.5 rounded-full border font-medium uppercase ' + sc;
    statusEl.textContent = b.status;
    setText('bot-detail-host', b.host+':'+b.port);
    setText('bot-detail-version', b.version);
    var uptimeStr = b.uptime_secs > 3600 ? Math.floor(b.uptime_secs/3600)+'h '+Math.floor((b.uptime_secs%3600)/60)+'m' : Math.floor(b.uptime_secs/60)+'m';
    setText('bot-detail-uptime', uptimeStr);
    setText('bot-detail-hb', b.last_heartbeat || 'Never');

    var cmdEl = document.getElementById('bot-detail-commands');
    cmdEl.textContent = '';
    if (data.commands && data.commands.length) {
      data.commands.forEach(function(c) {
        var cc = CMD_STATUS_COLORS[c.status] || '';
        var row = document.createElement('div');
        row.className = 'flex items-center justify-between py-1.5 border-b border-zc-border';
        row.innerHTML = '<div><span class="text-white font-medium">'+esc(c.kind)+'</span> <span class="'+cc+' badge px-1.5 py-0.5 rounded-full">'+esc(c.status)+'</span></div><div class="text-gray-500 text-[10px]">'+esc(c.created_at)+'</div>';
        cmdEl.appendChild(row);
      });
    } else { cmdEl.textContent = 'No commands'; }

    var evtEl = document.getElementById('bot-detail-events');
    evtEl.textContent = '';
    if (data.events && data.events.length) {
      data.events.forEach(function(e) {
        var row = document.createElement('div');
        row.className = 'flex items-center justify-between py-1.5 border-b border-zc-border';
        row.innerHTML = '<span class="text-white">'+esc(e.kind)+'</span><span class="text-gray-500 text-[10px]">'+esc(e.timestamp)+'</span>';
        evtEl.appendChild(row);
      });
    } else { evtEl.textContent = 'No events'; }

    showSection('bot-detail');
  });
}

function doDeleteBot() {
  if (!currentBotId) return;
  if (!confirm('Remove bot '+currentBotId+'?')) return;
  controlPost('bots/'+encodeURIComponent(currentBotId)+'/delete', {}).then(function(d) {
    if (d.ok) { toast('Bot removed','ok'); showSection('bots'); }
    else toast(d.error||'Failed','err');
  });
}

function showCommandModal() {
  controlFetch('bots').then(function(data) {
    var bots = (data.bots||[]);
    var botOpts = bots.map(function(b){return '<option value="'+esc(b.id)+'">'+esc(b.name)+' ('+esc(b.id.substring(0,8))+')</option>';}).join('');
    var kinds = ['reload_config','restart','stop','update_provider','update_channel','update_memory','update_security','run_agent','shell'];
    var kindOpts = kinds.map(function(k){return '<option value="'+k+'">'+k+'</option>';}).join('');
    var html = '<div class="space-y-3">' +
      '<div><label class="text-xs text-gray-500 block mb-1">Target Bot</label><select id="m-cmd-bot" class="admin-input">'+botOpts+'</select></div>' +
      '<div><label class="text-xs text-gray-500 block mb-1">Command Kind</label><select id="m-cmd-kind" class="admin-input">'+kindOpts+'</select></div>' +
      '<div><label class="text-xs text-gray-500 block mb-1">Payload (JSON)</label><textarea id="m-cmd-payload" class="admin-input" rows="3" placeholder="{}">{}</textarea></div>' +
      '<div class="flex gap-2 mt-4"><button onclick="doCreateCommand()" class="admin-btn flex-1">Send</button><button onclick="closeModal()" class="admin-btn flex-1" style="opacity:0.5">Cancel</button></div></div>';
    createModal('New Command', html);
  });
}

function showCommandModalFor() {
  if (!currentBotId) return;
  var kinds = ['reload_config','restart','stop','update_provider','update_channel','update_memory','update_security','run_agent','shell'];
  var kindOpts = kinds.map(function(k){return '<option value="'+k+'">'+k+'</option>';}).join('');
  var html = '<div class="space-y-3">' +
    '<div><label class="text-xs text-gray-500 block mb-1">Command Kind</label><select id="m-cmd-kind" class="admin-input">'+kindOpts+'</select></div>' +
    '<div><label class="text-xs text-gray-500 block mb-1">Payload (JSON)</label><textarea id="m-cmd-payload" class="admin-input" rows="3" placeholder="{}">{}</textarea></div>' +
    '<div class="flex gap-2 mt-4"><button onclick="doCreateCommandFor()" class="admin-btn flex-1">Send</button><button onclick="closeModal()" class="admin-btn flex-1" style="opacity:0.5">Cancel</button></div></div>';
  createModal('Command to Bot', html);
}

function doCreateCommand() {
  var botId = document.getElementById('m-cmd-bot').value;
  var kind = document.getElementById('m-cmd-kind').value;
  var payload = document.getElementById('m-cmd-payload').value || '{}';
  controlPost('commands/create', {bot_id:botId,kind:kind,payload:payload}).then(function(d) {
    if (d.id) { toast('Command created: '+d.id.substring(0,8),'ok'); loadCommands(); }
    else toast(d.error||'Failed','err');
  });
  closeModal();
}

function doCreateCommandFor() {
  var kind = document.getElementById('m-cmd-kind').value;
  var payload = document.getElementById('m-cmd-payload').value || '{}';
  controlPost('commands/create', {bot_id:currentBotId,kind:kind,payload:payload}).then(function(d) {
    if (d.id) { toast('Command created: '+d.id.substring(0,8),'ok'); }
    else toast(d.error||'Failed','err');
  });
  closeModal();
}

function loadCommands() {
  controlFetch('commands?limit=50').then(function(data) {
    var tbody = document.getElementById('commands-tbody');
    tbody.textContent = '';
    if (!data.commands || !data.commands.length) {
      var tr = document.createElement('tr');
      var td = document.createElement('td');
      td.colSpan = 6; td.className = 'px-4 py-3 text-gray-500'; td.textContent = 'No commands';
      tr.appendChild(td); tbody.appendChild(tr); return;
    }
    data.commands.forEach(function(c) {
      var cc = CMD_STATUS_COLORS[c.status] || '';
      var res = c.result ? (c.result.length > 40 ? c.result.substring(0,40)+'...' : c.result) : '-';
      var tr = document.createElement('tr');
      tr.className = 'border-b border-zc-border hover:bg-white/[0.02]';
      tr.innerHTML =
        '<td class="px-4 py-2.5 mono text-gray-400">'+esc(c.id.substring(0,8))+'</td>' +
        '<td class="px-4 py-2.5 mono text-gray-400">'+esc(c.bot_id.substring(0,8))+'</td>' +
        '<td class="px-4 py-2.5 text-white font-medium">'+esc(c.kind)+'</td>' +
        '<td class="px-4 py-2.5"><span class="badge '+cc+' px-2 py-0.5 rounded-full font-medium uppercase">'+esc(c.status)+'</span></td>' +
        '<td class="px-4 py-2.5 text-gray-500">'+esc(c.created_at)+'</td>' +
        '<td class="px-4 py-2.5 text-gray-400">'+esc(res)+'</td>';
      tbody.appendChild(tr);
    });
  });
}

function loadApprovals() {
  controlFetch('approvals?limit=50').then(function(data) {
    var list = document.getElementById('approvals-list');
    list.textContent = '';
    if (!data.approvals || !data.approvals.length) { list.textContent = 'No pending approvals'; return; }
    var pending = data.approvals.filter(function(a){return a.status==='pending';});
    var countEl = document.getElementById('nav-approvals-count');
    if (pending.length > 0) { countEl.textContent = pending.length; countEl.classList.remove('hidden'); }
    else { countEl.classList.add('hidden'); }
    data.approvals.forEach(function(a) {
      var isPending = a.status === 'pending';
      var statusColor = a.status === 'approved' ? 'text-zc-green' : a.status === 'rejected' ? 'text-zc-red' : 'text-zc-amber';
      var card = document.createElement('div');
      card.className = 'bg-zc-card border border-zc-border rounded-xl p-4 fade-in';
      card.innerHTML =
        '<div class="flex items-center justify-between">' +
          '<div class="text-sm text-white font-medium">Command <span class="mono text-gray-400">'+esc(a.command_id.substring(0,8))+'</span></div>' +
          '<span class="badge '+statusColor+' uppercase font-medium">'+esc(a.status)+'</span>' +
        '</div>' +
        '<div class="text-[11px] text-gray-500 mt-1">Reviewer: '+esc(a.reviewer||'none')+' | Reviewed: '+esc(a.reviewed_at||'pending')+'</div>' +
        (a.reason ? '<div class="text-[11px] text-gray-400 mt-1">Reason: '+esc(a.reason)+'</div>' : '');
      if (isPending) {
        var actions = document.createElement('div');
        actions.className = 'flex gap-2 mt-3';
        var approveBtn = document.createElement('button');
        approveBtn.className = 'admin-btn text-[11px] py-1';
        approveBtn.textContent = 'Approve';
        approveBtn.onclick = function(){doApprove(a.command_id);};
        var rejectBtn = document.createElement('button');
        rejectBtn.className = 'admin-btn admin-btn-red text-[11px] py-1';
        rejectBtn.textContent = 'Reject';
        rejectBtn.onclick = function(){doReject(a.command_id);};
        actions.appendChild(approveBtn);
        actions.appendChild(rejectBtn);
        card.appendChild(actions);
      }
      list.appendChild(card);
    });
  });
}

function doApprove(cmdId) {
  controlPost('approvals/'+encodeURIComponent(cmdId), {action:'approve',reviewer:'admin'}).then(function(d) {
    if (d.ok) { toast('Approved','ok'); loadApprovals(); }
    else toast(d.error||'Failed','err');
  });
}

function doReject(cmdId) {
  var reason = prompt('Rejection reason (optional):');
  controlPost('approvals/'+encodeURIComponent(cmdId), {action:'reject',reviewer:'admin',reason:reason||''}).then(function(d) {
    if (d.ok) { toast('Rejected','ok'); loadApprovals(); }
    else toast(d.error||'Failed','err');
  });
}

function loadAudit() {
  controlFetch('audit?limit=100').then(function(data) {
    var tbody = document.getElementById('audit-tbody');
    tbody.textContent = '';
    if (!data.entries || !data.entries.length) {
      var tr = document.createElement('tr');
      var td = document.createElement('td');
      td.colSpan = 5; td.className = 'px-4 py-3 text-gray-500'; td.textContent = 'No audit entries';
      tr.appendChild(td); tbody.appendChild(tr); return;
    }
    data.entries.forEach(function(e) {
      var tr = document.createElement('tr');
      tr.className = 'border-b border-zc-border hover:bg-white/[0.02]';
      tr.innerHTML =
        '<td class="px-4 py-2.5 text-gray-500 mono text-[10px]">'+esc(e.timestamp)+'</td>' +
        '<td class="px-4 py-2.5 text-white">'+esc(e.actor)+'</td>' +
        '<td class="px-4 py-2.5"><span class="badge bg-zc-accent/15 text-zc-accent px-2 py-0.5 rounded-full">'+esc(e.action)+'</span></td>' +
        '<td class="px-4 py-2.5 text-gray-400 mono">'+esc(e.target)+'</td>' +
        '<td class="px-4 py-2.5 text-gray-500">'+esc(e.detail)+'</td>';
      tbody.appendChild(tr);
    });
  });
}

function loadEvents() {
  controlFetch('events?limit=50').then(function(data) {
    var el = document.getElementById('events-stream');
    el.textContent = '';
    if (!data.events || !data.events.length) { el.textContent = 'No events yet'; return; }
    data.events.forEach(function(e) {
      var row = document.createElement('div');
      row.className = 'flex items-center gap-3 py-1.5 border-b border-zc-border/50 text-xs fade-in';
      row.innerHTML =
        '<span class="text-gray-500 mono text-[10px] shrink-0">'+esc(e.timestamp)+'</span>' +
        '<span class="text-gray-400 mono shrink-0">'+esc(e.bot_id.substring(0,8))+'</span>' +
        '<span class="text-white font-medium">'+esc(e.kind)+'</span>' +
        '<span class="text-gray-500 truncate">'+esc(e.payload)+'</span>';
      el.appendChild(row);
    });
  });
}

function toggleSSE() {
  if (sseSource) {
    sseSource.close(); sseSource = null;
    document.getElementById('sse-status').textContent = 'Disconnected';
    document.getElementById('sse-toggle').textContent = 'Connect';
    document.getElementById('sse-toggle').className = 'text-xs bg-zc-green/15 text-zc-green px-3 py-1.5 rounded-lg hover:bg-zc-green/25 font-medium';
    return;
  }
  var url = BASE + '/api/control/events/stream';
  if (AUTH_TOKEN) url += '?token=' + encodeURIComponent(AUTH_TOKEN);
  sseSource = new EventSource(url);
  document.getElementById('sse-status').textContent = 'Connecting...';
  sseSource.onopen = function() {
    document.getElementById('sse-status').textContent = 'Connected';
    document.getElementById('sse-toggle').textContent = 'Disconnect';
    document.getElementById('sse-toggle').className = 'text-xs bg-zc-red/15 text-zc-red px-3 py-1.5 rounded-lg hover:bg-zc-red/25 font-medium';
  };
  sseSource.onmessage = function(ev) {
    var el = document.getElementById('events-stream');
    if (el.children.length === 0 || (el.children.length === 1 && el.firstChild.textContent === 'No events yet')) el.textContent = '';
    var line = document.createElement('div');
    line.className = 'flex items-center gap-3 py-1.5 border-b border-zc-border/50 text-xs fade-in';
    var now = new Date().toISOString().substring(11,19);
    var timeSpan = document.createElement('span');
    timeSpan.className = 'text-gray-500 mono text-[10px] shrink-0';
    timeSpan.textContent = now;
    var msgSpan = document.createElement('span');
    msgSpan.className = 'text-white';
    msgSpan.textContent = ev.data;
    line.appendChild(timeSpan);
    line.appendChild(msgSpan);
    el.insertBefore(line, el.firstChild);
    if (el.children.length > 200) el.removeChild(el.lastChild);
  };
  sseSource.onerror = function() {
    document.getElementById('sse-status').textContent = 'Error - retrying';
  };
}
</script>
<script>
var oldNav = document.querySelector('nav .px-4.py-3.border-t');
if (oldNav) {
  var tokenBtn = document.createElement('button');
  tokenBtn.className = 'text-[10px] text-gray-500 hover:text-zc-accent mt-1.5 block';
  tokenBtn.textContent = 'Set Auth Token';
  tokenBtn.onclick = showTokenModal;
  oldNav.appendChild(tokenBtn);
}
</script>
</body>
</html>"##;

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
        assert!(DASHBOARD_HTML.starts_with("<!DOCTYPE html>"));
        assert!(DASHBOARD_HTML.ends_with("</html>"));
        assert!(DASHBOARD_HTML.contains("<head>"));
        assert!(DASHBOARD_HTML.contains("</head>"));
        assert!(DASHBOARD_HTML.contains("<body"));
        assert!(DASHBOARD_HTML.contains("</body>"));
    }

    #[test]
    fn dashboard_html_contains_zeroclaw_branding() {
        assert!(DASHBOARD_HTML.contains("ZeroClaw"));
        assert!(DASHBOARD_HTML.contains("Admin Dashboard"));
    }

    #[test]
    fn dashboard_html_references_all_api_endpoints() {
        let expected_endpoints = [
            "/api/system",
            "/api/channels",
            "/api/status",
            "/api/config",
            "/api/memories",
            "/api/metrics",
            "/api/admin/",
            "/api/control/",
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
            "memories",
            "config",
            "metrics",
            "bots",
            "commands",
            "approvals",
            "audit",
            "events",
        ];
        for section in &nav_sections {
            let section_id = format!("section-{section}");
            assert!(
                DASHBOARD_HTML.contains(&section_id),
                "Dashboard HTML missing section: {section_id}"
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
