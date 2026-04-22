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

/// Compress the advisor's categorized issue lists into a bullet block the
/// executor can consume as a revision directive. Keeps the four concern
/// dimensions (correctness / architecture / security / silent failures)
/// labelled so the executor can prioritize correctness fixes over
/// architecture tweaks when the message is long.
fn collect_review_issues(review: &crate::advisor::ReviewOutput) -> String {
    let mut out = String::new();
    let sections: [(&str, &Vec<String>); 4] = [
        ("Correctness", &review.correctness_issues),
        ("Architecture", &review.architecture_concerns),
        ("Security", &review.security_flags),
        ("Silent failures", &review.silent_failures),
    ];
    for (label, items) in sections {
        if items.is_empty() {
            continue;
        }
        out.push_str(&format!("{label}:\n"));
        for item in items {
            out.push_str(&format!("  - {item}\n"));
        }
    }
    if out.is_empty() && !review.summary.is_empty() {
        out.push_str(&format!("Summary: {}\n", review.summary));
    }
    out.trim_end().to_string()
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

    // ── SLM-first gatekeeper (★ MoA core workflow) ──
    //
    // Every post-login chat message is classified by the on-device SLM
    // before the pipeline even looks at cloud credentials. The router's
    // `process_message` returns:
    //   * `local_response = Some(..)` → SLM judged the task simple enough
    //     to answer on-device → short-circuit and return, no cloud cost.
    //   * `local_response = None`     → SLM decided the task needs a
    //     full LLM (advanced reasoning / document generation / tools) →
    //     fall through to the agent loop, which in turn summons the LLM
    //     using the user's own API key first, then the operator proxy
    //     with 2.2× credit billing (see billing/llm_router.rs).
    //
    // If the gatekeeper is disabled or the Ollama daemon is unreachable,
    // this block is a no-op — behaviour matches the pre-SLM pipeline.
    //
    // When the gatekeeper hands off to the cloud LLM, we preserve its
    // `decision` in `gatekeeper_decision` so the advisor-policy block
    // downstream can route PLAN/REVIEW checkpoints appropriately.
    let mut gatekeeper_decision: Option<crate::gatekeeper::router::RoutingDecision> = None;
    if let Some(router) = state.gatekeeper.as_ref() {
        let result = router.process_message(message).await;
        if let Some(local_reply) = result.local_response {
            tracing::info!(
                category = ?result.decision.category,
                confidence = result.decision.confidence,
                reason = %result.decision.reason,
                "SLM gatekeeper answered locally — skipping LLM"
            );
            let body = serde_json::json!({
                "reply": local_reply,
                "model": router.model(),
                "session_id": chat_body.session_id,
                "active_provider": "ollama",
                "active_model": router.model(),
                "is_local_path": true,
                "network_status": "local",
                "gatekeeper": {
                    "category": format!("{:?}", result.decision.category),
                    "confidence": result.decision.confidence,
                    "reason": result.decision.reason,
                },
            });
            return (StatusCode::OK, Json(body));
        }
        tracing::debug!(
            category = ?result.decision.category,
            confidence = result.decision.confidence,
            reason = %result.decision.reason,
            "SLM gatekeeper summoning LLM for complex task"
        );
        gatekeeper_decision = Some(result.decision);
    }

    // ── PII redactor at the SLM→LLM escalation boundary (spec, 2026-04-22) ──
    //
    // When the gatekeeper has decided to escalate (above) AND the
    // operator hasn't disabled it, every piece of PII in the prompt
    // is replaced with numbered placeholders before the message
    // crosses the local boundary. The bidirectional map lives only
    // for the duration of this request and is dropped at function
    // return — never persisted, never logged. The LLM response is
    // run through `restore_text` further down before reaching the
    // user, so the placeholders never appear in the surface UI.
    let pii_redact_enabled = state.config.lock().gatekeeper.redact_pii_on_escalation;
    let mut pii_map = crate::security::pii_redaction::PiiRedactionMap::new();
    let pii_active = pii_redact_enabled && gatekeeper_decision.is_some();

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

    // Redact PII from the prompt + recent context when escalating to a
    // cloud LLM. Workspace / coding-context strings injected below
    // contain no PII so they intentionally bypass this step.
    if pii_active {
        enriched_message = crate::security::pii_redaction::redact_text(
            &enriched_message,
            &mut pii_map,
        );
        if !pii_map.is_empty() {
            tracing::debug!(
                redacted_count = pii_map.len(),
                "PII redacted before LLM escalation"
            );
        }
    }

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
    //
    // NOTE: `provider_name` below may be overridden by the §3.1 routing
    // decision after the key is resolved (for offline/strict/app-default
    // paths). Key resolution uses the user's originally-configured
    // provider name so env-var lookup (`ANTHROPIC_API_KEY` etc.) still
    // picks the right slot; the override only flips the active primary
    // provider, not the key source.
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

    // ── §3.1 routing decision (App-chat mode) ─────────────────────────
    // Run the decision tree BEFORE the cloud-key validation so that
    // `ForcedLocalByMissingKey`, `ForcedLocalByOffline`, and
    // `ForcedLocalByConfig` can bypass the "no API key" error entirely —
    // those paths intentionally don't need a cloud key because they
    // route to on-device Ollama.
    //
    // When the decision is `Local`, swap `config.default_provider` to
    // `"ollama"` and append the user's original primary (e.g. `"gemini"`)
    // to `fallback_providers` so a local failure still degrades to cloud
    // instead of killing the chat. Cloud decisions leave the config
    // untouched.
    //
    // ChatMode::App is the correct classification here — this handler
    // backs MoA's own desktop/mobile Tauri client. The Channel and Web
    // handlers live in separate files and will pick up the same helper
    // (`apply_routing_decision`) when they're migrated.
    let has_cloud_api_key = providers::has_provider_credential(
        provider_name,
        config.api_key.as_deref(),
    );
    let network_online = crate::local_llm::shared_health().is_online();
    let routing_decision = crate::local_llm::decide_active_provider_v2(
        &crate::local_llm::RoutingContext {
            primary_cloud: provider_name,
            has_cloud_api_key,
            network_online,
            reliability: &config.reliability,
            chat_mode: crate::local_llm::ChatMode::App,
            // `quality_first` is a user preference MoA surfaces in the
            // Settings panel; for now we read it from the reliability
            // block's `prefer_cloud_on_app` flag if set (falls back to
            // false). Hard-coded `false` here until that flag is wired.
            quality_first: false,
        },
    );
    if routing_decision.provider.is_local() {
        let new_primary = crate::local_llm::apply_routing_decision(
            provider_name,
            &mut config.reliability,
            &routing_decision,
        );
        tracing::info!(
            reason = ?routing_decision.reason,
            original = provider_name,
            new_primary = %new_primary,
            "§3.1 routing: swapping primary provider to on-device Gemma 4"
        );
        config.default_provider = Some(new_primary);
    }
    let provider_name = config.default_provider.as_deref().unwrap_or("gemini");

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

    // ── Advisor PLAN checkpoint ──
    //
    // Only fires for tasks the SLM gatekeeper classified as Medium+. For
    // Simple / greeting / short-Q&A messages the gatekeeper already
    // answered above and this handler returned early — so by the time we
    // reach here, the task has been judged "SLM cannot handle alone" and
    // the advisor is invited to shape the executor's plan.
    //
    // Plan output is prepended to `enriched_message` as a structured
    // directive block so the downstream agent loop sees the advisor's
    // end-state + critical path as high-priority system context.
    let mut advisor_plan: Option<crate::advisor::PlanOutput> = None;
    if let (Some(advisor), Some(decision)) = (state.advisor.as_ref(), gatekeeper_decision.as_ref())
    {
        let policy = crate::advisor::AdvisorPolicy::for_category(decision.category);
        if policy.plan {
            let kind = crate::advisor::TaskKind::infer(
                decision.category,
                decision.tool_needed.as_deref(),
                message,
            );
            let req = crate::advisor::AdvisorRequest {
                task_summary: message,
                background: "",
                recent_output: "",
                question: "Produce a strategic plan for this user request before execution.",
                kind,
            };
            match advisor.plan(&req).await {
                Ok(plan) => {
                    tracing::info!(
                        model = advisor.model(),
                        steps = plan.critical_path.len(),
                        kind = kind.label(),
                        "Advisor PLAN checkpoint completed"
                    );
                    let steps = plan
                        .critical_path
                        .iter()
                        .enumerate()
                        .map(|(i, s)| format!("  {}. {}", i + 1, s))
                        .collect::<Vec<_>>()
                        .join("\n");
                    let risks = if plan.risks.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "\nRisks:\n{}\n",
                            plan.risks
                                .iter()
                                .map(|r| format!("  - {r}"))
                                .collect::<Vec<_>>()
                                .join("\n")
                        )
                    };
                    let tools_hint = if plan.suggested_tools.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "\nSuggested Tools (use these first):\n{}\n",
                            plan.suggested_tools
                                .iter()
                                .map(|t| format!("  - {t}"))
                                .collect::<Vec<_>>()
                                .join("\n")
                        )
                    };
                    let plan_block = format!(
                        "[Advisor Plan — follow this strategy]\n\
                         End State: {}\n\
                         First Move: {}\n\
                         Critical Path:\n{}{}{}\n\
                         ---\n\n",
                        plan.end_state, plan.first_move, steps, risks, tools_hint,
                    );
                    enriched_message = format!("{plan_block}{enriched_message}");
                    advisor_plan = Some(plan);
                }
                Err(e) => tracing::warn!(
                    error = %e,
                    "Advisor PLAN failed — proceeding without plan"
                ),
            }
        }
    }

    // ── Phase 3 — SLM executor attempt ──
    //
    // Before falling through to the full cloud agent loop, try letting
    // the on-device SLM close the task via its own prompt-guided tool
    // loop. The executor shares `tools_registry_exec` so every tool the
    // cloud LLM would call is equally available here. We only attempt
    // this when:
    //   - `state.slm_executor` is wired (requires gatekeeper enabled),
    //   - the gatekeeper flagged `Medium` or a concrete `tool_needed`
    //     (Simple was already answered above; Complex/Specialized tend
    //     to need stronger reasoning than a 4B model provides and we
    //     route them straight to the cloud).
    //
    // On success, we record the SLM output, skip the cloud agent loop,
    // and still pass through the advisor REVIEW checkpoint downstream.
    // On exceeded-iterations / error, we silently fall through to the
    // cloud path — the user gets an answer regardless.
    let mut slm_produced_reply: Option<String> = None;
    let mut slm_tools_invoked: Vec<String> = Vec::new();
    if let (Some(executor), Some(decision)) =
        (state.slm_executor.as_ref(), gatekeeper_decision.as_ref())
    {
        let eligible = matches!(
            decision.category,
            crate::gatekeeper::router::TaskCategory::Medium
        ) || decision.tool_needed.is_some();
        if eligible {
            // Borrow the registered executable tools without reallocating.
            // `tools_registry_exec: Arc<Vec<Box<dyn Tool>>>` maps cleanly
            // to `&[&dyn Tool]` which is what the SLM executor accepts.
            // Filter out tools the individual impls have flagged as
            // unsafe for the on-device SLM (Tool::safe_for_slm=false) —
            // shell, delegate, file_write, file_edit, apply_patch, cron_*
            // currently. The cloud LLM agent loop (fallback) still sees
            // every tool.
            let tool_refs: Vec<&dyn crate::tools::Tool> = state
                .tools_registry_exec
                .as_ref()
                .iter()
                .filter_map(|boxed| {
                    let t = boxed.as_ref();
                    t.safe_for_slm().then_some(t)
                })
                .collect();
            match executor.run(&enriched_message, &tool_refs).await {
                Ok(outcome) if !outcome.exceeded_iterations => {
                    tracing::info!(
                        iterations = outcome.iterations,
                        tools = outcome.tools_invoked.len(),
                        "SLM executor closed the task locally — skipping cloud LLM"
                    );
                    slm_tools_invoked = outcome.tools_invoked;
                    slm_produced_reply = Some(outcome.reply);
                }
                Ok(outcome) => {
                    tracing::info!(
                        iterations = outcome.iterations,
                        "SLM executor exceeded iteration budget — falling back to cloud LLM"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "SLM executor errored — falling back to cloud LLM"
                    );
                }
            }
        }
    }

    // ── Run the full agent loop ──
    let agent_outcome = if let Some(reply) = slm_produced_reply.clone() {
        // SLM already answered — short-circuit.
        Ok(reply)
    } else {
        crate::agent::process_message_with_session(config, &enriched_message, session_id).await
    };
    match agent_outcome {
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
                    provider: provider_label.clone(),
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

            // ── Advisor REVIEW checkpoint (+ one revision pass if needed) ──
            //
            // Per user spec: "SLM은 어드바이저 LLM의 조언을 받아 실행한 후
            // 결과를 이용자에게 반환하기 이전에 반드시 advisor의 리뷰를 받도록".
            //
            // If the first review verdict is `RevisionNeeded`, we run the
            // executor one more time with the advisor's issues appended to
            // the enriched message as a "[Advisor review — please revise]"
            // directive block, then re-review the revised answer. The
            // revised verdict is what gets attached to the response body.
            //
            // Revision is capped at a single pass so a pathological
            // advisor-executor ping-pong cannot indefinitely burn credits.
            // `Block` never triggers a revision — it's a hard stop that
            // surfaces as a warning banner on the final reply.
            let mut advisor_review: Option<crate::advisor::ReviewOutput> = None;
            let mut revised_response: Option<String> = None;
            let mut executor_model_for_revision = model_label.clone();
            if let (Some(advisor), Some(decision)) =
                (state.advisor.as_ref(), gatekeeper_decision.as_ref())
            {
                let policy = crate::advisor::AdvisorPolicy::for_category(decision.category);
                if policy.review {
                    let kind = crate::advisor::TaskKind::infer(
                        decision.category,
                        decision.tool_needed.as_deref(),
                        message,
                    );
                    let plan_background = advisor_plan
                        .as_ref()
                        .map(|p| {
                            format!("Plan end state: {}\nFirst move: {}", p.end_state, p.first_move)
                        })
                        .unwrap_or_default();
                    let first_req = crate::advisor::AdvisorRequest {
                        task_summary: message,
                        background: plan_background.as_str(),
                        recent_output: &safe_response,
                        question: "Review the executor's answer above for correctness, architecture, security, and silent failures.",
                        kind,
                    };
                    match advisor.review(&first_req).await {
                        Ok(first_review) => {
                            tracing::info!(
                                verdict = ?first_review.verdict,
                                correctness = first_review.correctness_issues.len(),
                                security = first_review.security_flags.len(),
                                kind = kind.label(),
                                "Advisor REVIEW checkpoint completed (pass 1)"
                            );

                            if first_review.verdict == crate::advisor::ReviewVerdict::RevisionNeeded {
                                // One revision pass: append the advisor's
                                // specific issues as a directive block and
                                // re-run the executor.
                                let issues = collect_review_issues(&first_review);
                                let revision_directive = format!(
                                    "[Advisor review — please revise addressing these issues]\n{issues}\n\n\
                                     Produce a corrected answer. Keep everything the prior answer got \
                                     right, only change what the reviewer flagged.\n\n---\n\n\
                                     {enriched_message}"
                                );
                                let revision_config = state.config.lock().clone();
                                match crate::agent::process_message_with_session(
                                    revision_config,
                                    &revision_directive,
                                    session_id,
                                )
                                .await
                                {
                                    Ok(revised_raw) => {
                                        let leak_guard = state
                                            .config
                                            .lock()
                                            .security
                                            .outbound_leak_guard
                                            .clone();
                                        let safe_revised = sanitize_gateway_response(
                                            &revised_raw,
                                            state.tools_registry_exec.as_ref(),
                                            &leak_guard,
                                        );
                                        tracing::info!(
                                            revised_chars = safe_revised.len(),
                                            "Executor produced revised answer — re-reviewing"
                                        );
                                        let second_req = crate::advisor::AdvisorRequest {
                                            task_summary: message,
                                            background: plan_background.as_str(),
                                            recent_output: &safe_revised,
                                            question: "Re-review the revised answer. Issues flagged in the prior review should now be fixed; raise only new blocking issues.",
                                            kind,
                                        };
                                        match advisor.review(&second_req).await {
                                            Ok(second_review) => {
                                                tracing::info!(
                                                    verdict = ?second_review.verdict,
                                                    "Advisor REVIEW checkpoint completed (pass 2 — revised)"
                                                );
                                                advisor_review = Some(second_review);
                                                revised_response = Some(safe_revised);
                                                executor_model_for_revision = format!(
                                                    "{} (+1 revision)",
                                                    model_label
                                                );
                                            }
                                            Err(e) => {
                                                tracing::warn!(
                                                    error = %e,
                                                    "Advisor re-review after revision failed — returning revision with original verdict"
                                                );
                                                advisor_review = Some(first_review);
                                                revised_response = Some(safe_revised);
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            error = %e,
                                            "Revision pass failed — returning original answer with review notes"
                                        );
                                        advisor_review = Some(first_review);
                                    }
                                }
                            } else {
                                advisor_review = Some(first_review);
                            }
                        }
                        Err(e) => tracing::warn!(
                            error = %e,
                            "Advisor REVIEW failed — returning raw answer without review"
                        ),
                    }
                }
            }
            let effective_response = revised_response
                .clone()
                .unwrap_or_else(|| safe_response.clone());

            // ── Active-provider metadata (PR #3.5) ──
            // Surfaces which provider/model actually served this request so
            // the UI can render a "via Gemma 4 (local)" badge when the
            // patent §1 cl. 4 fallback engaged. `network_status` reflects the
            // most recent probe result from the process-wide
            // `local_llm::shared_health()` cache.
            let net_online = crate::local_llm::shared_health().is_online();
            let is_local_path = provider_label.eq_ignore_ascii_case("ollama");

            // Prepend a visible warning when the advisor blocked the answer
            // so the user is not silently served a flagged result. Uses the
            // revised response when the revision pass ran, or the original
            // executor output otherwise.
            let mut final_reply = if advisor_review
                .as_ref()
                .is_some_and(|r| r.verdict == crate::advisor::ReviewVerdict::Block)
            {
                format!(
                    "⚠️ Advisor flagged this answer — review before relying on it.\n\n{effective_response}"
                )
            } else {
                effective_response.clone()
            };

            // Restore the originals that the redactor swapped out before
            // we sent the prompt to the cloud. The map lives in the
            // request scope and is dropped on return, so this is the
            // last chance to put names / phone numbers / SSNs back into
            // the answer the user actually sees.
            if pii_active && !pii_map.is_empty() {
                final_reply = crate::security::pii_redaction::restore_text(
                    &final_reply,
                    &pii_map,
                );
            }

            let advisor_meta = advisor_review.as_ref().map(|r| {
                serde_json::json!({
                    "verdict": format!("{:?}", r.verdict).to_ascii_lowercase(),
                    "summary": r.summary,
                    "correctness_issues": r.correctness_issues,
                    "architecture_concerns": r.architecture_concerns,
                    "security_flags": r.security_flags,
                    "silent_failures": r.silent_failures,
                    "revised": revised_response.is_some(),
                    "model": state.advisor.as_ref().map(|a| a.model().to_string()),
                })
            });

            let slm_meta = slm_produced_reply.as_ref().map(|_| {
                serde_json::json!({
                    "used": true,
                    "model": state.slm_executor.as_ref().map(|e| e.model().to_string()),
                    "tools_invoked": slm_tools_invoked,
                })
            });

            let body = serde_json::json!({
                "reply": final_reply,
                "model": executor_model_for_revision,
                "session_id": chat_body.session_id,
                "active_provider": if slm_produced_reply.is_some() { "ollama" } else { provider_label.as_str() },
                "active_model": model_label,
                "is_local_path": is_local_path || slm_produced_reply.is_some(),
                "network_status": if net_online { "online" } else { "offline" },
                "advisor": advisor_meta,
                "slm_executor": slm_meta,
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
