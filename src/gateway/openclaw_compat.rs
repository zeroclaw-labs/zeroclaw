//! OpenClaw migration compatibility layer.
//!
//! Provides two endpoints for callers migrating from OpenClaw to ZeroClaw:
//!
//! 1. **`POST /api/chat`** (recommended) — ZeroClaw-native endpoint that invokes the
//!    full agent loop (`process_message`) with tools, memory recall, and context
//!    enrichment. Same code path as Linq/WhatsApp/Nextcloud Talk handlers.
//!
//! 2. **`POST /v1/chat/completions`** override — OpenAI-compatible shim that accepts
//!    standard `messages[]` arrays, extracts the last user message plus recent history,
//!    and routes through the full agent loop. Drop-in replacement for OpenClaw callers.
//!
//! ## Why this exists
//!
//! OpenClaw exposed `/v1/chat/completions` as an OpenAI-compatible API server.
//! ZeroClaw's existing `/v1/chat/completions` (in `openai_compat.rs`) uses the
//! simpler `provider.chat_with_history()` path — no tools, no memory, no agent loop.
//!
//! This module bridges the gap so callers coming from OpenClaw get the full agent
//! experience without code changes on their side.
//!
//! ## Migration path
//!
//! New integrations should use `POST /api/chat`. The `/v1/chat/completions` shim
//! is provided for backward compatibility and may be deprecated once all callers
//! have migrated to the native endpoint.

use super::{
    client_key_from_request, run_gateway_chat_with_tools, sanitize_gateway_response, AppState,
    RATE_LIMIT_WINDOW_SECS,
};
use crate::memory::MemoryCategory;
use crate::providers;
use axum::{
    body::Body,
    extract::{ConnectInfo, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Json},
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::time::Instant;
use uuid::Uuid;

// ══════════════════════════════════════════════════════════════════════════════
// /api/chat — ZeroClaw-native endpoint
// ══════════════════════════════════════════════════════════════════════════════

/// Request body for `POST /api/chat`.
#[derive(Debug, Deserialize)]
pub struct ApiChatBody {
    /// The user message to process.
    pub message: String,

    /// Optional session ID for memory scoping.
    /// When provided, memory store/recall operations are isolated to this session.
    #[serde(default)]
    pub session_id: Option<String>,

    /// Optional context lines to prepend to the message.
    /// Use this to inject recent conversation history that ZeroClaw's
    /// semantic memory might not surface (e.g., the last few exchanges).
    #[serde(default)]
    pub context: Vec<String>,

    /// Optional LLM provider override (e.g. "anthropic", "openai", "gemini").
    /// When provided, overrides the server's default_provider for this request.
    #[serde(default)]
    pub provider: Option<String>,

    /// Optional model ID override (e.g. "claude-opus-4-6", "gpt-4o").
    /// When provided, overrides the server's default_model for this request.
    #[serde(default)]
    pub model: Option<String>,

    /// Optional API key override for the selected provider.
    /// When provided, takes highest priority over server-side stored keys.
    /// This allows the client to pass a key from its local storage directly.
    #[serde(default)]
    pub api_key: Option<String>,

    /// When true, signals that the user has explicitly connected a workspace
    /// (folder or GitHub repo). The agent will receive coding-aware instructions
    /// to proactively use file tools (file_read, file_write, glob_search, etc.).
    #[serde(default)]
    pub workspace_connected: bool,

    /// When true, restricts file operations to read/download only.
    /// Set automatically for remote web access to prevent unauthorized
    /// file modification through the relay.
    #[serde(default)]
    pub remote_read_only: bool,

    /// LLM proxy URL for hybrid relay mode.
    /// When provided (together with `proxy_token`), the agent loop routes
    /// LLM calls through this proxy endpoint instead of calling the LLM API
    /// directly. This keeps the operator's API key on the server.
    #[serde(default)]
    pub proxy_url: Option<String>,

    /// Short-lived proxy token for authenticating with the LLM proxy endpoint.
    /// Used together with `proxy_url` for hybrid relay mode.
    #[serde(default)]
    pub proxy_token: Option<String>,
}

fn api_chat_memory_key() -> String {
    format!("api_chat_msg_{}", Uuid::new_v4())
}

/// `POST /api/chat` — full agent loop with tools and memory.
///
/// Request:  `{ "message": "...", "session_id": "...", "context": [...] }`
/// Response: `{ "reply": "...", "model": "..." }`
pub async fn handle_api_chat(
    State(state): State<AppState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Result<Json<ApiChatBody>, axum::extract::rejection::JsonRejection>,
) -> impl IntoResponse {
    // ── Rate limit ──
    let rate_key =
        client_key_from_request(Some(peer_addr), &headers, state.trust_forwarded_headers);
    if !state.rate_limiter.allow_webhook(&rate_key) {
        tracing::warn!("/api/chat rate limit exceeded");
        let err = serde_json::json!({
            "error": "Too many chat requests. Please retry later.",
            "retry_after": RATE_LIMIT_WINDOW_SECS,
        });
        return (StatusCode::TOO_MANY_REQUESTS, Json(err));
    }

    // ── Auth: require at least one layer for non-loopback ──
    // Accept either pairing token or JWT session token for non-loopback requests
    let is_loopback = peer_addr.ip().is_loopback();

    if !is_loopback {
        let auth_header = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let bearer_token = auth_header.strip_prefix("Bearer ").unwrap_or("");

        // Try JWT session auth first (from /api/auth/login)
        let jwt_ok = state
            .auth_store
            .as_ref()
            .and_then(|store| store.validate_session(bearer_token))
            .is_some();

        // Then try pairing auth
        let pairing_ok =
            state.pairing.require_pairing() && state.pairing.is_authenticated(bearer_token);

        // Then try webhook secret (verify X-Webhook-Secret header against stored hash)
        let webhook_ok = if let Some(ref secret_hash) = state.webhook_secret_hash {
            headers
                .get("X-Webhook-Secret")
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|v| {
                    use sha2::{Digest, Sha256};
                    let digest = Sha256::digest(v.as_bytes());
                    let hashed = hex::encode(digest);
                    crate::security::pairing::constant_time_eq(&hashed, secret_hash.as_ref())
                })
                .unwrap_or(false)
        } else {
            false
        };

        if !jwt_ok && !pairing_ok && !webhook_ok {
            tracing::warn!("/api/chat: rejected unauthenticated non-loopback request");
            let err = serde_json::json!({
                "error": "Unauthorized — please login or configure pairing"
            });
            return (StatusCode::UNAUTHORIZED, Json(err));
        }
    }

    // ── Parse body ──
    let Json(chat_body) = match body {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("/api/chat JSON parse error: {e}");
            let err = serde_json::json!({
                "error": "Invalid JSON body. Expected: {\"message\": \"...\"}"
            });
            return (StatusCode::BAD_REQUEST, Json(err));
        }
    };

    let message = chat_body.message.trim();
    let session_id = chat_body
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if message.is_empty() {
        let err = serde_json::json!({ "error": "Message cannot be empty" });
        return (StatusCode::BAD_REQUEST, Json(err));
    }

    // ── Auto-save to memory ──
    if state.auto_save {
        let key = api_chat_memory_key();
        let _ = state
            .mem
            .store(&key, message, MemoryCategory::Conversation, session_id)
            .await;
    }

    // ── Build enriched message with optional context ──
    let mut enriched_message = if chat_body.context.is_empty() {
        message.to_string()
    } else {
        let recent: Vec<&String> = chat_body.context.iter().rev().take(10).rev().collect();
        let context_block = recent
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<&str>>()
            .join("\n");
        format!(
            "Recent conversation context:\n{}\n\nCurrent message:\n{}",
            context_block, message
        )
    };

    // ── Inject coding-aware workspace context ──
    // When the user has explicitly connected a workspace (folder or GitHub repo),
    // prepend instructions so the agent proactively uses file/coding tools.
    if chat_body.workspace_connected {
        let workspace_dir = state.config.lock().workspace_dir.clone();
        let coding_context = format!(
            "[Workspace Context — gstack Coding Methodology]\n\
             The user has connected workspace: `{}`\n\
             You have full access to coding tools. Use them directly — never ask the user to do it manually.\n\n\
             ## Available Tools\n\
             - `glob_search` — find files by pattern (e.g. **/*.rs, src/**/*.tsx)\n\
             - `content_search` — search file contents by regex\n\
             - `file_read` — read file contents (with line range)\n\
             - `file_write` — create or overwrite files\n\
             - `file_edit` — edit files with exact string replacement\n\
             - `apply_patch` — apply unified diff patches\n\
             - `shell` — run commands (build, test, lint, npm, cargo, etc.)\n\
             - `git_operations` — git status, diff, commit, branch, log\n\
             - `browser` — test web apps in real Chromium (open URL, click, screenshot)\n\
             - `shell` + Python scripts — create DOCX/PDF/XLSX/PPTX documents\n\
             - `document_process` — convert uploaded documents to HTML/Markdown\n\n\
             ## gstack Development Methodology (FOLLOW THIS)\n\n\
             When the user asks you to build, fix, or modify code, follow this structured workflow:\n\n\
             ### Phase 1: Think & Plan\n\
             - **Understand the request** — read relevant existing code first (`file_read`, `glob_search`)\n\
             - **Analyze the codebase** — understand architecture, dependencies, conventions\n\
             - **Plan the changes** — identify which files to modify, what to add/remove\n\
             - **State your plan** to the user before executing: \"3개 파일을 수정하겠습니다: ...\"\n\n\
             ### Phase 2: Build\n\
             - **Make changes** using `file_edit` for modifications, `file_write` for new files\n\
             - **One concern per change** — don't mix unrelated edits\n\
             - **Follow existing code style** — match indentation, naming, patterns\n\
             - **Run build/compile** after changes: `shell` with the project's build command\n\n\
             ### Phase 3: Review\n\
             - **Self-review** — re-read changed files to verify correctness\n\
             - **Check for regressions** — did your change break anything else?\n\
             - **Run linter/formatter** if the project has one (eslint, rustfmt, prettier)\n\n\
             ### Phase 4: Test & Verify with Playwright (CRITICAL)\n\
             Testing has THREE layers. All three MUST pass before shipping.\n\n\
             **Layer 1: Unit/Integration Tests (automated)**\n\
             - Run the project's test suite: `shell` with npm test / cargo test / pytest\n\
             - If tests fail: read the error log, identify root cause, fix, re-run\n\
             - Do NOT proceed to Layer 2 until Layer 1 passes\n\n\
             **Layer 2: Build & Runtime Verification**\n\
             - Run build command: `shell` with npm run build / cargo build\n\
             - If build fails: read compiler/bundler errors, fix, re-build\n\
             - Start dev server if needed: `shell` with npm run dev / cargo run\n\
             - Check server logs for startup errors\n\n\
             **Layer 3: Playwright Browser Verification (for web/UI projects)**\n\
             Use the `browser` tool with the Playwright daemon to do REAL browser testing:\n\n\
             Step 1. Open the app:\n\
               `browser open http://localhost:3000` (or the project's dev URL)\n\n\
             Step 2. Take initial screenshot:\n\
               `browser screenshot` → verify the page renders correctly\n\n\
             Step 3. Interactive element testing — snapshot and click ALL interactive elements:\n\
               `browser snapshot` → get @ref map of all buttons, links, inputs\n\
               For EACH interactive element found:\n\
               - `browser click @e1` → verify navigation/action works\n\
               - `browser screenshot` → capture result state\n\
               - `browser back` → return to previous page\n\
               - Repeat for @e2, @e3, ... (all links and buttons)\n\n\
             Step 4. Form testing (if forms exist):\n\
               `browser snapshot` → find input fields\n\
               `browser fill @input_field \"test data\"` → fill forms\n\
               `browser click @submit_button` → submit\n\
               `browser screenshot` → verify success/error handling\n\n\
             Step 5. Responsive testing (if web project):\n\
               Test at mobile width: `browser js \"window.innerWidth = 375; window.innerHeight = 667;\"`\n\
               `browser screenshot` → verify mobile layout\n\n\
             Step 6. Error detection:\n\
               `browser js \"return window.__errors || []\"` → check for JS console errors\n\
               If any errors found: identify source, fix, re-test\n\n\
             **Error Investigation Protocol:**\n\
             When any test fails:\n\
             1. Read the FULL error message/stack trace\n\
             2. Identify the exact file and line number\n\
             3. `file_read` that file at the relevant line\n\
             4. Understand the root cause (don't guess)\n\
             5. Fix the specific issue\n\
             6. Re-run the failing test to confirm the fix\n\
             7. Re-run ALL tests to check for regressions\n\
             8. Maximum 3 fix attempts per issue — if still failing after 3, report to user\n\n\
             ### Phase 5: Ship\n\
             - **Commit with clear message**: `git_operations` commit\n\
             - **Report results** to the user: what changed, what was tested, what to verify\n\n\
             ### Phase 6: Final Verification & Report\n\
             - **Web app**: take before/after screenshots, show side-by-side comparison\n\
             - **API/backend**: show test output, demonstrate with curl/http_request\n\
             - **Report to user with evidence**:\n\
               - What was changed (file list + summary)\n\
               - Test results (all passed / N failed)\n\
               - Screenshots (before → after, if applicable)\n\
               - Any warnings or known limitations\n\
             - **Suggest follow-ups**: \"추가로 테스트가 필요한 부분이 있습니까?\"\n\n\
             ## Key Rules\n\
             - **Read before write** — always inspect existing code before modifying\n\
             - **Build after every change** — catch errors immediately\n\
             - **Test after every change** — don't accumulate untested code\n\
             - **Never guess** — search the codebase for existing patterns before inventing new ones\n\
             - **Explain what you did** — the user should understand every change\n\n",
            workspace_dir.display()
        );
        enriched_message = format!("{coding_context}{enriched_message}");
    }

    // ── Remote read-only enforcement ──
    // When accessed via web relay (remote_read_only=true), restrict to
    // file download only. File write/edit/shell are physically blocked.
    if chat_body.remote_read_only {
        enriched_message = format!(
            "[REMOTE ACCESS — READ ONLY]\n\
             이 요청은 원격 웹 접속입니다. 보안을 위해 다음 도구만 사용 가능합니다:\n\
             - file_read (파일 읽기/다운로드)\n\
             - glob_search, content_search (파일 검색)\n\
             - memory_recall, memory_store (기억)\n\
             - web_search (웹 검색)\n\n\
             파일 수정(file_write, file_edit), 셸 명령(shell), 삭제 등은 \
             보안상 차단됩니다. 이용자에게 \"원격 접속에서는 파일 다운로드만 가능합니다\"라고 안내하세요.\n\n\
             {enriched_message}"
        );
    }

    // ── Build config with client-provided overrides ──
    // Reload tool/feature config from disk so runtime changes (e.g. web_search_config
    // enabling search) take effect without a gateway restart.
    let mut config = state.config.lock().clone();
    if let Ok(disk_cfg) = super::reload_disk_config(&config) {
        config.web_search = disk_cfg.web_search;
        config.web_fetch = disk_cfg.web_fetch;
        config.model_routes = disk_cfg.model_routes;
    }

    // Physical enforcement: remote read-only → set autonomy to ReadOnly
    // This physically blocks file_write, file_edit, shell at the SecurityPolicy level
    if chat_body.remote_read_only {
        config.autonomy.level = crate::security::AutonomyLevel::ReadOnly;
        config.autonomy.allow_sensitive_file_writes = false;
        tracing::info!("Remote read-only mode: autonomy set to read-only");
    }

    // Map frontend provider names to backend provider names
    if let Some(ref client_provider) = chat_body.provider {
        let backend_provider = match client_provider.as_str() {
            "claude" => "anthropic",
            p => p,
        };
        config.default_provider = Some(backend_provider.to_string());
    }

    if let Some(ref client_model) = chat_body.model {
        if !client_model.trim().is_empty() {
            config.default_model = Some(client_model.clone());
        }
    }

    // ── Resolve API key: client-provided > provider_api_keys > env ──
    // Priority order:
    // 1. Client-provided api_key (from request body — user's own key)
    // 2. Server-side provider_api_keys map (from Settings / config)
    // 3. Environment variables (checked later by provider factory)
    let provider_name = config.default_provider.as_deref().unwrap_or("gemini");

    let client_key = chat_body
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|k| !k.is_empty());

    if let Some(key) = client_key {
        config.api_key = Some(key.to_string());
    } else if let Some(stored_key) = config.provider_api_keys.get(provider_name) {
        if stored_key.trim().is_empty() {
            // Clear stale key from a different provider
            config.api_key = None;
        } else {
            config.api_key = Some(stored_key.clone());
        }
    } else {
        // No key found for this provider — clear any previous
        // provider's key so we don't send a mismatched key.
        // The provider factory will check env vars as fallback.
        config.api_key = None;
    }

    // ── Validate API key for cloud providers ──
    // Check both the config-level key (from request body or config.toml) AND
    // provider-specific env vars (e.g. GEMINI_API_KEY, ANTHROPIC_API_KEY).
    // The provider factory uses resolve_provider_credential() which checks
    // env vars as fallback, so we mirror that logic here.
    //
    // ★ Hybrid relay: if proxy_url + proxy_token are provided, the agent loop
    // will use ProxyProvider for LLM calls instead of direct API — so we
    // don't need a local LLM key. Set the proxy config and continue.
    let use_proxy = chat_body.proxy_url.is_some() && chat_body.proxy_token.is_some();
    if providers::provider_requires_credential(provider_name) {
        let has_key = providers::has_provider_credential(provider_name, config.api_key.as_deref());
        if !has_key && !use_proxy {
            let env_hint = match provider_name {
                "anthropic" => "ANTHROPIC_API_KEY",
                "openai" => "OPENAI_API_KEY",
                "gemini" | "google" | "google-gemini" => "GEMINI_API_KEY",
                _ => "<PROVIDER>_API_KEY",
            };
            let err = serde_json::json!({
                "error": format!(
                    "No API key configured for provider '{}'. Please add your API key in Settings or set {} env var.",
                    provider_name, env_hint
                ),
                "code": "missing_api_key",
                "fallback_to_relay": true
            });
            return (StatusCode::BAD_REQUEST, Json(err));
        }
    }

    // Store proxy config for agent loop (if provided)
    if use_proxy {
        config.llm_proxy_url = chat_body.proxy_url.clone();
        config.llm_proxy_token = chat_body.proxy_token.clone();
    }

    // ── Observability ──
    let provider_label = config
        .default_provider
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    let model_label = config
        .default_model
        .clone()
        .unwrap_or_else(|| state.model.clone());
    let started_at = Instant::now();

    state
        .observer
        .record_event(&crate::observability::ObserverEvent::AgentStart {
            provider: provider_label.clone(),
            model: model_label.clone(),
        });
    state
        .observer
        .record_event(&crate::observability::ObserverEvent::LlmRequest {
            provider: provider_label.clone(),
            model: model_label.clone(),
            messages_count: 1,
        });

    // ── Run the full agent loop ──
    match crate::agent::process_message_with_session(config, &enriched_message, session_id).await {
        Ok(response) => {
            let leak_guard_cfg = state.config.lock().security.outbound_leak_guard.clone();
            let safe_response = sanitize_gateway_response(
                &response,
                state.tools_registry_exec.as_ref(),
                &leak_guard_cfg,
            );
            let duration = started_at.elapsed();

            state
                .observer
                .record_event(&crate::observability::ObserverEvent::LlmResponse {
                    provider: provider_label.clone(),
                    model: model_label.clone(),
                    duration,
                    success: true,
                    error_message: None,
                    input_tokens: None,
                    output_tokens: None,
                });
            state.observer.record_metric(
                &crate::observability::traits::ObserverMetric::RequestLatency(duration),
            );
            state
                .observer
                .record_event(&crate::observability::ObserverEvent::AgentEnd {
                    provider: provider_label,
                    model: model_label.clone(),
                    duration,
                    tokens_used: None,
                    cost_usd: None,
                });

            // ── Record usage analytics for admin dashboard ──
            if let Some(ref auth_store) = state.auth_store {
                let category = crate::memory::traits::InteractionCategory::classify(
                    &enriched_message,
                    &[],
                )
                .to_string();
                let user_id = chat_body
                    .session_id
                    .as_deref()
                    .unwrap_or("anonymous");
                let chars = enriched_message.len() as i64 + safe_response.len() as i64;
                let _ = auth_store.record_usage(user_id, &category, chars);
            }

            let body = serde_json::json!({
                "reply": safe_response,
                "model": model_label,
                "session_id": chat_body.session_id,
            });
            (StatusCode::OK, Json(body))
        }
        Err(e) => {
            let duration = started_at.elapsed();
            let sanitized = providers::sanitize_api_error(&e.to_string());

            state
                .observer
                .record_event(&crate::observability::ObserverEvent::LlmResponse {
                    provider: provider_label.clone(),
                    model: model_label.clone(),
                    duration,
                    success: false,
                    error_message: Some(sanitized.clone()),
                    input_tokens: None,
                    output_tokens: None,
                });
            state.observer.record_metric(
                &crate::observability::traits::ObserverMetric::RequestLatency(duration),
            );
            state
                .observer
                .record_event(&crate::observability::ObserverEvent::AgentEnd {
                    provider: provider_label.clone(),
                    model: model_label,
                    duration,
                    tokens_used: None,
                    cost_usd: None,
                });

            tracing::error!("/api/chat provider error: {sanitized}");

            // Detect provider authentication errors (401 Unauthorized) so the
            // client can fall back to the relay server with operator keys.
            let is_auth_error = sanitized.contains("401")
                || sanitized.contains("Unauthorized")
                || sanitized.contains("authentication");
            if is_auth_error {
                let user_message = format!(
                    "API key for '{}' is invalid or expired. Please update your API key in Settings.",
                    provider_label
                );
                let err = serde_json::json!({
                    "error": user_message,
                    "detail": sanitized,
                    "code": "provider_auth_error",
                    "fallback_to_relay": true,
                });
                return (StatusCode::BAD_REQUEST, Json(err));
            }

            // Detect context window / token limit errors for user-friendly message
            let is_context_error = sanitized.contains("context")
                || sanitized.contains("token limit")
                || sanitized.contains("too long");

            // Detect credit/billing exhaustion errors
            let is_credit_error = sanitized.contains("credit balance")
                || sanitized.contains("billing")
                || sanitized.contains("purchase credits")
                || sanitized.contains("insufficient_quota")
                || sanitized.contains("exceeded your current quota");

            let user_message = if is_credit_error {
                let provider = &provider_label;
                let console_url = match provider.as_str() {
                    "anthropic" => "https://console.anthropic.com/settings/billing",
                    "openai" => "https://platform.openai.com/account/billing",
                    "gemini" | "google" => "https://aistudio.google.com/billing",
                    _ => "",
                };
                if console_url.is_empty() {
                    format!(
                        "⚠️ {provider} API 크레딧이 소진되었습니다.\n\
                         API 크레딧을 충전하거나 결제 정보를 확인해주세요.\n\n\
                         또는 설정에서 다른 모델로 변경할 수 있습니다."
                    )
                } else {
                    format!(
                        "⚠️ {provider} API 크레딧이 소진되었습니다.\n\
                         크레딧 충전: {console_url}\n\n\
                         충전 후 다시 시도해주세요. \
                         또는 설정에서 다른 모델로 변경할 수 있습니다."
                    )
                }
            } else if is_context_error {
                "메시지가 너무 깁니다. 더 짧은 메시지로 다시 시도하거나, \
                 설정에서 더 큰 컨텍스트 윈도우를 지원하는 모델로 변경해주세요."
                    .to_string()
            } else if sanitized.contains("rate limit")
                || sanitized.contains("429")
                || sanitized.contains("Too Many")
            {
                format!(
                    "⏳ {} API 요청 한도에 도달했습니다.\n\
                     잠시 후 다시 시도해주세요. (보통 1-2분 후 자동 해제됩니다)",
                    provider_label
                )
            } else if sanitized.contains("overloaded")
                || sanitized.contains("503")
                || sanitized.contains("capacity")
            {
                format!(
                    "🔄 {} 서버가 일시적으로 과부하 상태입니다.\n\
                     잠시 후 다시 시도하거나, 설정에서 다른 모델로 변경해주세요.",
                    provider_label
                )
            } else if sanitized.contains("timeout") || sanitized.contains("timed out") {
                "⏱️ 응답 시간이 초과되었습니다. 네트워크 연결을 확인하고 다시 시도해주세요."
                    .to_string()
            } else if sanitized.contains("connection")
                || sanitized.contains("network")
                || sanitized.contains("DNS")
            {
                "🌐 네트워크 연결에 문제가 있습니다.\n\
                 인터넷 연결을 확인하고 다시 시도해주세요."
                    .to_string()
            } else if sanitized.contains("invalid_api_key") || sanitized.contains("API key") {
                format!(
                    "🔑 {} API 키가 올바르지 않습니다.\n\
                     설정에서 API 키를 확인해주세요.",
                    provider_label
                )
            } else {
                format!(
                    "⚠️ AI 응답 생성 중 문제가 발생했습니다.\n\
                     다시 시도해주세요. 문제가 계속되면 설정에서 다른 모델로 변경하거나, \
                     앱을 재시작해보세요.\n\n(상세: {})",
                    if sanitized.len() > 200 {
                        format!("{}...", &sanitized[..200])
                    } else {
                        sanitized.clone()
                    }
                )
            };

            let err = serde_json::json!({
                "error": user_message,
            });
            (StatusCode::INTERNAL_SERVER_ERROR, Json(err))
        }
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// /v1/chat/completions — OpenAI-compatible shim (full agent loop)
// ══════════════════════════════════════════════════════════════════════════════

/// Maximum context messages extracted from the `messages[]` array for injection.
const MAX_CONTEXT_MESSAGES: usize = 10;

/// OpenAI-compatible request body.
#[derive(Debug, Deserialize)]
pub struct OaiChatRequest {
    pub messages: Vec<OaiMessage>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub stream: Option<bool>,
    // Accept and ignore other OpenAI params for compat
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub top_p: Option<f64>,
    #[serde(default)]
    pub frequency_penalty: Option<f64>,
    #[serde(default)]
    pub presence_penalty: Option<f64>,
    #[serde(default)]
    pub stop: Option<serde_json::Value>,
    #[serde(default)]
    pub user: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct OaiMessage {
    pub role: String,
    pub content: String,
}

// Response types — reuse the ones from openai_compat.rs via the same format
#[derive(Debug, Serialize)]
struct OaiChatResponse {
    id: String,
    object: &'static str,
    created: u64,
    model: String,
    choices: Vec<OaiChoice>,
    usage: OaiUsage,
}

#[derive(Debug, Serialize)]
struct OaiChoice {
    index: u32,
    message: OaiMessage,
    finish_reason: &'static str,
}

#[derive(Debug, Serialize)]
struct OaiUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
}

#[derive(Debug, Serialize)]
struct OaiStreamChunk {
    id: String,
    object: &'static str,
    created: u64,
    model: String,
    choices: Vec<OaiStreamChoice>,
}

#[derive(Debug, Serialize)]
struct OaiStreamChoice {
    index: u32,
    delta: OaiDelta,
    finish_reason: Option<&'static str>,
}

#[derive(Debug, Serialize)]
struct OaiDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
}

/// `POST /v1/chat/completions` — OpenAI-compatible shim over ZeroClaw's agent loop.
///
/// This replaces the simple `provider.chat_with_history()` path from `openai_compat.rs`
/// with the full `run_gateway_chat_with_tools()` agent loop, giving OpenClaw callers
/// the same tools + memory experience as native ZeroClaw channels.
pub async fn handle_v1_chat_completions_with_tools(
    State(state): State<AppState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    // ── Rate limit ──
    let rate_key =
        client_key_from_request(Some(peer_addr), &headers, state.trust_forwarded_headers);
    if !state.rate_limiter.allow_webhook(&rate_key) {
        tracing::warn!("/v1/chat/completions (compat) rate limit exceeded");
        let err = serde_json::json!({
            "error": {
                "message": "Rate limit exceeded. Please retry later.",
                "type": "rate_limit_error",
                "code": "rate_limit_exceeded"
            }
        });
        return (StatusCode::TOO_MANY_REQUESTS, Json(err)).into_response();
    }

    // ── Auth: require at least one layer for non-loopback ──
    if !state.pairing.require_pairing()
        && state.webhook_secret_hash.is_none()
        && !peer_addr.ip().is_loopback()
    {
        tracing::warn!(
            "/v1/chat/completions (compat): rejected unauthenticated non-loopback request"
        );
        let err = serde_json::json!({
            "error": {
                "message": "Unauthorized — configure pairing or X-Webhook-Secret for non-local access",
                "type": "invalid_request_error",
                "code": "unauthorized"
            }
        });
        return (StatusCode::UNAUTHORIZED, Json(err)).into_response();
    }

    // ── Bearer token auth (pairing) ──
    if state.pairing.require_pairing() {
        let auth = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let token = auth.strip_prefix("Bearer ").unwrap_or("");
        if !state.pairing.is_authenticated(token) {
            tracing::warn!(
                "/v1/chat/completions (compat): rejected — not paired / invalid bearer token"
            );
            let err = serde_json::json!({
                "error": {
                    "message": "Invalid API key. Pair first via POST /pair, then use Authorization: Bearer <token>",
                    "type": "invalid_request_error",
                    "code": "invalid_api_key"
                }
            });
            return (StatusCode::UNAUTHORIZED, Json(err)).into_response();
        }
    }

    // ── Body size ──
    if body.len() > super::openai_compat::CHAT_COMPLETIONS_MAX_BODY_SIZE {
        let err = serde_json::json!({
            "error": {
                "message": format!(
                    "Request body too large ({} bytes, max {})",
                    body.len(),
                    super::openai_compat::CHAT_COMPLETIONS_MAX_BODY_SIZE
                ),
                "type": "invalid_request_error",
                "code": "request_too_large"
            }
        });
        return (StatusCode::PAYLOAD_TOO_LARGE, Json(err)).into_response();
    }

    // ── Parse body ──
    let request: OaiChatRequest = match serde_json::from_slice(&body) {
        Ok(req) => req,
        Err(e) => {
            tracing::warn!("/v1/chat/completions (compat) JSON parse error: {e}");
            let err = serde_json::json!({
                "error": {
                    "message": format!("Invalid JSON body: {e}"),
                    "type": "invalid_request_error",
                    "code": "invalid_json"
                }
            });
            return (StatusCode::BAD_REQUEST, Json(err)).into_response();
        }
    };

    if request.messages.is_empty() {
        let err = serde_json::json!({
            "error": {
                "message": "messages array must not be empty",
                "type": "invalid_request_error",
                "code": "invalid_messages"
            }
        });
        return (StatusCode::BAD_REQUEST, Json(err)).into_response();
    }

    // ── Extract last user message + context ──
    let last_user_msg = request
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .map(|m| m.content.clone());

    let message = match last_user_msg {
        Some(m) if !m.trim().is_empty() => m,
        _ => {
            let err = serde_json::json!({
                "error": {
                    "message": "No user message found in messages array",
                    "type": "invalid_request_error",
                    "code": "invalid_messages"
                }
            });
            return (StatusCode::BAD_REQUEST, Json(err)).into_response();
        }
    };

    // Build context from conversation history (exclude the last user message)
    let context_messages: Vec<String> = request
        .messages
        .iter()
        .rev()
        .skip(1)
        .rev()
        .filter(|m| m.role == "user" || m.role == "assistant")
        .map(|m| {
            let role_label = if m.role == "user" {
                "User"
            } else {
                "Assistant"
            };
            format!("{}: {}", role_label, m.content)
        })
        .collect();

    let enriched_message = if context_messages.is_empty() {
        message.clone()
    } else {
        let recent: Vec<&String> = context_messages
            .iter()
            .rev()
            .take(MAX_CONTEXT_MESSAGES)
            .rev()
            .collect();
        let context_block = recent
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<&str>>()
            .join("\n");
        format!(
            "Recent conversation context:\n{}\n\nCurrent message:\n{}",
            context_block, message
        )
    };

    let is_stream = request.stream.unwrap_or(false);
    let session_id = request
        .user
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let request_id = format!("chatcmpl-{}", Uuid::new_v4().to_string().replace('-', ""));
    let created = unix_timestamp();

    // ── Auto-save ──
    if state.auto_save {
        let key = api_chat_memory_key();
        let _ = state
            .mem
            .store(&key, &message, MemoryCategory::Conversation, session_id)
            .await;
    }

    // ── Resolve provider-specific key from provider_api_keys map ──
    // Users configure API keys once via Settings (stored in provider_api_keys).
    // At chat time, we always look up the correct key for the effective provider.
    // This prevents the 401 bug where config.api_key holds a DIFFERENT
    // provider's key (e.g. Gemini key when using Anthropic).
    {
        let mut config_guard = state.config.lock();
        let provider_name = config_guard
            .default_provider
            .clone()
            .unwrap_or_else(|| "gemini".to_string());

        if let Some(stored_key) = config_guard.provider_api_keys.get(&provider_name).cloned() {
            if stored_key.trim().is_empty() {
                config_guard.api_key = None;
            } else {
                config_guard.api_key = Some(stored_key);
            }
        } else {
            // No key for this provider — clear stale key from another provider
            config_guard.api_key = None;
        }
    }

    // ── Observability ──
    let provider_label = state
        .config
        .lock()
        .default_provider
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    let model_label = state.model.clone();
    let started_at = Instant::now();

    state
        .observer
        .record_event(&crate::observability::ObserverEvent::AgentStart {
            provider: provider_label.clone(),
            model: model_label.clone(),
        });
    state
        .observer
        .record_event(&crate::observability::ObserverEvent::LlmRequest {
            provider: provider_label.clone(),
            model: model_label.clone(),
            messages_count: request.messages.len(),
        });

    tracing::info!(
        stream = is_stream,
        messages_count = request.messages.len(),
        "Processing /v1/chat/completions (compat shim — full agent loop)"
    );

    // ── Run the full agent loop ──
    let reply = match run_gateway_chat_with_tools(&state, &enriched_message, session_id).await {
        Ok(response) => {
            let leak_guard_cfg = state.config.lock().security.outbound_leak_guard.clone();
            let safe = sanitize_gateway_response(
                &response,
                state.tools_registry_exec.as_ref(),
                &leak_guard_cfg,
            );
            let duration = started_at.elapsed();

            state
                .observer
                .record_event(&crate::observability::ObserverEvent::LlmResponse {
                    provider: provider_label.clone(),
                    model: model_label.clone(),
                    duration,
                    success: true,
                    error_message: None,
                    input_tokens: None,
                    output_tokens: None,
                });
            state.observer.record_metric(
                &crate::observability::traits::ObserverMetric::RequestLatency(duration),
            );
            state
                .observer
                .record_event(&crate::observability::ObserverEvent::AgentEnd {
                    provider: provider_label,
                    model: model_label,
                    duration,
                    tokens_used: None,
                    cost_usd: None,
                });

            safe
        }
        Err(e) => {
            let duration = started_at.elapsed();
            let sanitized = providers::sanitize_api_error(&e.to_string());

            state
                .observer
                .record_event(&crate::observability::ObserverEvent::LlmResponse {
                    provider: provider_label.clone(),
                    model: model_label.clone(),
                    duration,
                    success: false,
                    error_message: Some(sanitized.clone()),
                    input_tokens: None,
                    output_tokens: None,
                });
            state.observer.record_metric(
                &crate::observability::traits::ObserverMetric::RequestLatency(duration),
            );
            state
                .observer
                .record_event(&crate::observability::ObserverEvent::AgentEnd {
                    provider: provider_label,
                    model: model_label,
                    duration,
                    tokens_used: None,
                    cost_usd: None,
                });

            tracing::error!("/v1/chat/completions (compat) provider error: {sanitized}");
            let err = serde_json::json!({
                "error": {
                    "message": format!("LLM request failed: {sanitized}"),
                    "type": "server_error",
                    "code": "provider_error"
                }
            });
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(err)).into_response();
        }
    };

    let model_name = request.model.unwrap_or_else(|| state.model.clone());

    #[allow(clippy::cast_possible_truncation)]
    let prompt_tokens = (enriched_message.len() / 4) as u32;
    #[allow(clippy::cast_possible_truncation)]
    let completion_tokens = (reply.len() / 4) as u32;

    if is_stream {
        // ── Simulated streaming SSE ──
        // The full agent loop returns a complete response; we chunk it into SSE format.
        let role_chunk = OaiStreamChunk {
            id: request_id.clone(),
            object: "chat.completion.chunk",
            created,
            model: model_name.clone(),
            choices: vec![OaiStreamChoice {
                index: 0,
                delta: OaiDelta {
                    role: Some("assistant"),
                    content: None,
                },
                finish_reason: None,
            }],
        };

        let content_chunk = OaiStreamChunk {
            id: request_id.clone(),
            object: "chat.completion.chunk",
            created,
            model: model_name.clone(),
            choices: vec![OaiStreamChoice {
                index: 0,
                delta: OaiDelta {
                    role: None,
                    content: Some(reply),
                },
                finish_reason: None,
            }],
        };

        let stop_chunk = OaiStreamChunk {
            id: request_id,
            object: "chat.completion.chunk",
            created,
            model: model_name,
            choices: vec![OaiStreamChoice {
                index: 0,
                delta: OaiDelta {
                    role: None,
                    content: None,
                },
                finish_reason: Some("stop"),
            }],
        };

        let mut output = String::new();
        output.push_str("data: ");
        output.push_str(&serde_json::to_string(&role_chunk).unwrap_or_else(|_| "{}".into()));
        output.push_str("\n\n");
        output.push_str("data: ");
        output.push_str(&serde_json::to_string(&content_chunk).unwrap_or_else(|_| "{}".into()));
        output.push_str("\n\n");
        output.push_str("data: ");
        output.push_str(&serde_json::to_string(&stop_chunk).unwrap_or_else(|_| "{}".into()));
        output.push_str("\n\n");
        output.push_str("data: [DONE]\n\n");

        axum::response::Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .header(header::CACHE_CONTROL, "no-cache")
            .header(header::CONNECTION, "keep-alive")
            .body(Body::from(output))
            .expect("static SSE headers are valid")
            .into_response()
    } else {
        // ── Non-streaming JSON ──
        let response = OaiChatResponse {
            id: request_id,
            object: "chat.completion",
            created,
            model: model_name,
            choices: vec![OaiChoice {
                index: 0,
                message: OaiMessage {
                    role: "assistant".into(),
                    content: reply,
                },
                finish_reason: "stop",
            }],
            usage: OaiUsage {
                prompt_tokens,
                completion_tokens,
                total_tokens: prompt_tokens + completion_tokens,
            },
        };
        Json(response).into_response()
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// HELPERS
// ══════════════════════════════════════════════════════════════════════════════

fn unix_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ══════════════════════════════════════════════════════════════════════════════
// TESTS
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_chat_body_deserializes_minimal() {
        let json = r#"{"message": "Hello"}"#;
        let body: ApiChatBody = serde_json::from_str(json).unwrap();
        assert_eq!(body.message, "Hello");
        assert!(body.session_id.is_none());
        assert!(body.context.is_empty());
    }

    #[test]
    fn api_chat_body_deserializes_full() {
        let json = r#"{
            "message": "What's my schedule?",
            "session_id": "sess-123",
            "context": ["User: hi", "Assistant: hello"],
            "provider": "anthropic",
            "model": "claude-opus-4-6"
        }"#;
        let body: ApiChatBody = serde_json::from_str(json).unwrap();
        assert_eq!(body.message, "What's my schedule?");
        assert_eq!(body.session_id.as_deref(), Some("sess-123"));
        assert_eq!(body.context.len(), 2);
        assert_eq!(body.provider.as_deref(), Some("anthropic"));
        assert_eq!(body.model.as_deref(), Some("claude-opus-4-6"));
        assert!(body.api_key.is_none());
    }

    #[test]
    fn api_chat_body_deserializes_with_api_key() {
        let json = r#"{
            "message": "Hello",
            "provider": "anthropic",
            "api_key": "sk-ant-test-key"
        }"#;
        let body: ApiChatBody = serde_json::from_str(json).unwrap();
        assert_eq!(body.message, "Hello");
        assert_eq!(body.api_key.as_deref(), Some("sk-ant-test-key"));
    }

    #[test]
    fn oai_request_deserializes_with_extra_fields() {
        let json = r#"{
            "messages": [{"role": "user", "content": "Hi"}],
            "model": "claude-sonnet-4-6",
            "temperature": 0.5,
            "stream": false,
            "max_tokens": 1000,
            "top_p": 0.9,
            "frequency_penalty": 0.1,
            "presence_penalty": 0.0,
            "user": "test-user"
        }"#;
        let req: OaiChatRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.model.as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(req.temperature, Some(0.5));
        assert_eq!(req.stream, Some(false));
        assert_eq!(req.max_tokens, Some(1000));
    }

    #[test]
    fn oai_response_serializes_correctly() {
        let response = OaiChatResponse {
            id: "chatcmpl-test".into(),
            object: "chat.completion",
            created: 1_234_567_890,
            model: "test-model".into(),
            choices: vec![OaiChoice {
                index: 0,
                message: OaiMessage {
                    role: "assistant".into(),
                    content: "Hello!".into(),
                },
                finish_reason: "stop",
            }],
            usage: OaiUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            },
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("chatcmpl-test"));
        assert!(json.contains("chat.completion"));
        assert!(json.contains("Hello!"));
    }

    #[test]
    fn streaming_chunk_omits_none_fields() {
        let chunk = OaiStreamChunk {
            id: "chatcmpl-test".into(),
            object: "chat.completion.chunk",
            created: 1_234_567_890,
            model: "test-model".into(),
            choices: vec![OaiStreamChoice {
                index: 0,
                delta: OaiDelta {
                    role: None,
                    content: None,
                },
                finish_reason: None,
            }],
        };
        let json = serde_json::to_string(&chunk).unwrap();
        assert!(!json.contains("role"));
        assert!(!json.contains("content"));
    }

    #[test]
    fn memory_key_is_unique() {
        let k1 = api_chat_memory_key();
        let k2 = api_chat_memory_key();
        assert_ne!(k1, k2);
        assert!(k1.starts_with("api_chat_msg_"));
    }

    // ── Handler-level validation tests ──
    // These verify the input shapes that the handlers validate at runtime.

    #[test]
    fn api_chat_body_rejects_missing_message() {
        let json = r#"{"session_id": "s1"}"#;
        let result: Result<ApiChatBody, _> = serde_json::from_str(json);
        assert!(
            result.is_err(),
            "missing `message` field should fail deserialization"
        );
    }

    #[test]
    fn oai_request_rejects_empty_messages() {
        let json = r#"{"messages": []}"#;
        let req: OaiChatRequest = serde_json::from_str(json).unwrap();
        assert!(
            req.messages.is_empty(),
            "empty messages should parse but be caught by handler"
        );
    }

    #[test]
    fn oai_request_no_user_message_detected() {
        let json = r#"{"messages": [{"role": "system", "content": "You are helpful."}]}"#;
        let req: OaiChatRequest = serde_json::from_str(json).unwrap();
        let last_user = req.messages.iter().rev().find(|m| m.role == "user");
        assert!(last_user.is_none(), "should detect no user message");
    }

    #[test]
    fn oai_request_whitespace_only_user_message() {
        let json = r#"{"messages": [{"role": "user", "content": "   "}]}"#;
        let req: OaiChatRequest = serde_json::from_str(json).unwrap();
        let last_user = req.messages.iter().rev().find(|m| m.role == "user");
        assert!(
            last_user.map_or(true, |m| m.content.trim().is_empty()),
            "whitespace-only user message should be treated as empty"
        );
    }

    #[test]
    fn oai_context_extraction_skips_last_user_message() {
        let json = r#"{
            "messages": [
                {"role": "user", "content": "first"},
                {"role": "assistant", "content": "reply"},
                {"role": "user", "content": "second"}
            ]
        }"#;
        let req: OaiChatRequest = serde_json::from_str(json).unwrap();

        // Replicate the handler's context extraction logic
        let context_messages: Vec<String> = req
            .messages
            .iter()
            .rev()
            .skip(1)
            .rev()
            .filter(|m| m.role == "user" || m.role == "assistant")
            .map(|m| {
                format!(
                    "{}: {}",
                    if m.role == "user" {
                        "User"
                    } else {
                        "Assistant"
                    },
                    m.content
                )
            })
            .collect();

        assert_eq!(context_messages.len(), 2);
        assert!(context_messages[0].starts_with("User: first"));
        assert!(context_messages[1].starts_with("Assistant: reply"));
    }
}
