//! ZeroTrading REST API — 交易账户管理接口
//!
//! ## 端点总览
//!
//! | Method | Path | 说明 |
//! |--------|------|------|
//! | GET  | `/api/trading/status`                      | 引擎状态 + Skills 统计 |
//! | GET  | `/api/trading/accounts`                    | 列出所有账户（凭证已屏蔽）|
//! | POST | `/api/trading/accounts`                    | 新增或覆盖更新账户 |
//! | PATCH| `/api/trading/accounts/{exchange}/{label}` | 仅更新 API Key/Secret |
//! | DELETE | `/api/trading/accounts/{exchange}/{label}` | 删除账户 |
//! | GET  | `/api/trading/exchanges`                   | 列出已知交易所 |
//! | POST | `/api/trading/reload`                      | 热重载 Skills（无需重启）|
//!
//! ## 鉴权
//!
//! 所有接口复用 zeroclaw 的 `require_auth`（pairing bearer token 检查）。
//! 在 gateway 路由注册时注入 `TradingApiState`（独立于主 `AppState`）。

use super::config::{
    known_exchanges, list_accounts_masked, patch_account_credentials, remove_account,
    upsert_account, validate_exchange, TradingAccountEntry, TradingAccountStore,
};
use super::engine::TradingEngine;
use crate::gateway::AppState;
use axum::{
    extract::{Path, State},
    http::HeaderMap,
    response::{IntoResponse, Json},
};
use serde::Deserialize;
use std::sync::Arc;

// ─── 附加在 AppState 上的 ZeroTrading 扩展 ──────────────────────────────────

/// ZeroTrading 扩展数据（挂载在 axum 的独立 State 上注入）
#[derive(Clone)]
pub struct TradingApiState {
    pub engine: Arc<TradingEngine>,
    pub store: Arc<TradingAccountStore>,
}

// ─── 请求体类型 ───────────────────────────────────────────────────────────────

/// POST /api/trading/accounts 请求体
#[derive(Debug, Deserialize)]
pub struct UpsertAccountBody {
    pub label: String,
    pub exchange: String,
    pub api_key: String,
    pub api_secret: String,
    #[serde(default)]
    pub passphrase: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub read_only: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub note: Option<String>,
}

fn default_true() -> bool {
    true
}

/// PATCH 请求体（所有字段可选）
#[derive(Debug, Deserialize)]
pub struct PatchAccountBody {
    pub api_key: Option<String>,
    pub api_secret: Option<String>,
    pub passphrase: Option<String>,
}

// ─── 复合 State —— AppState + TradingApiState ─────────────────────────────────

/// 注入到 ZeroTrading 路由的复合 State
///
/// 通过 `axum::Router::with_state` 传入，包含主 AppState（用于鉴权）
/// 和 ZeroTrading 扩展数据。
#[derive(Clone)]
pub struct TradingRouterState {
    pub app: AppState,
    pub trading: TradingApiState,
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

/// GET /api/trading/status — 引擎状态 + Skills 统计
pub async fn handle_trading_status(
    State(state): State<TradingRouterState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = crate::gateway::api::require_auth(&state.app, &headers) {
        return e.into_response();
    }

    let status = state.trading.engine.status();
    Json(serde_json::json!({
        "engine": {
            "enabled": status.enabled,
            "skills_loaded": status.skills_loaded,
            "skills_dir": status.skills_dir,
            "skills_by_category": {
                "risk_control": status.skills_by_category.risk_control,
                "factor": status.skills_by_category.factor,
                "strategy": status.skills_by_category.strategy,
                "experience": status.skills_by_category.experience,
            },
        }
    }))
    .into_response()
}

/// GET /api/trading/accounts — 列出所有账户（凭证已掩码）
pub async fn handle_trading_accounts_list(
    State(state): State<TradingRouterState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = crate::gateway::api::require_auth(&state.app, &headers) {
        return e.into_response();
    }

    match list_accounts_masked(&state.trading.store).await {
        Ok(accounts) => Json(serde_json::json!({ "accounts": accounts })).into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("Failed to load accounts: {e}") })),
        )
            .into_response(),
    }
}

/// POST /api/trading/accounts — 新增 / 覆盖更新账户
pub async fn handle_trading_accounts_upsert(
    State(state): State<TradingRouterState>,
    headers: HeaderMap,
    Json(body): Json<UpsertAccountBody>,
) -> impl IntoResponse {
    if let Err(e) = crate::gateway::api::require_auth(&state.app, &headers) {
        return e.into_response();
    }

    if !validate_exchange(&body.exchange) {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "Invalid exchange identifier. Use alphanumeric + underscore only.",
                "known_exchanges": known_exchanges(),
            })),
        )
            .into_response();
    }

    let entry = TradingAccountEntry {
        label: body.label,
        exchange: body.exchange,
        api_key: body.api_key,
        api_secret: body.api_secret,
        passphrase: body.passphrase,
        base_url: body.base_url,
        read_only: body.read_only,
        enabled: body.enabled,
        note: body.note,
    };

    match upsert_account(&state.trading.store, entry).await {
        Ok(saved) => Json(serde_json::json!({
            "status": "ok",
            "account": saved.masked(),
        }))
        .into_response(),
        Err(e) => (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": format!("{e}") })),
        )
            .into_response(),
    }
}

/// PATCH /api/trading/accounts/{exchange}/{label} — 部分更新凭证
pub async fn handle_trading_accounts_patch(
    State(state): State<TradingRouterState>,
    headers: HeaderMap,
    Path((exchange, label)): Path<(String, String)>,
    Json(body): Json<PatchAccountBody>,
) -> impl IntoResponse {
    if let Err(e) = crate::gateway::api::require_auth(&state.app, &headers) {
        return e.into_response();
    }

    match patch_account_credentials(
        &state.trading.store,
        &exchange,
        &label,
        body.api_key,
        body.api_secret,
        body.passphrase,
    )
    .await
    {
        Ok(Some(updated)) => Json(serde_json::json!({
            "status": "ok",
            "account": updated.masked(),
        }))
        .into_response(),
        Ok(None) => (
            axum::http::StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": format!("Account {exchange}:{label} not found")
            })),
        )
            .into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("{e}") })),
        )
            .into_response(),
    }
}

/// DELETE /api/trading/accounts/{exchange}/{label} — 删除账户
pub async fn handle_trading_accounts_delete(
    State(state): State<TradingRouterState>,
    headers: HeaderMap,
    Path((exchange, label)): Path<(String, String)>,
) -> impl IntoResponse {
    if let Err(e) = crate::gateway::api::require_auth(&state.app, &headers) {
        return e.into_response();
    }

    match remove_account(&state.trading.store, &exchange, &label).await {
        Ok(true) => Json(serde_json::json!({ "status": "ok", "deleted": true })).into_response(),
        Ok(false) => (
            axum::http::StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": format!("Account {exchange}:{label} not found")
            })),
        )
            .into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("{e}") })),
        )
            .into_response(),
    }
}

/// GET /api/trading/exchanges — 列出所有已知交易所 ID
pub async fn handle_trading_exchanges(
    State(state): State<TradingRouterState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = crate::gateway::api::require_auth(&state.app, &headers) {
        return e.into_response();
    }
    Json(serde_json::json!({ "exchanges": known_exchanges() })).into_response()
}

/// POST /api/trading/reload — 热重载 Skills 目录（无需重启）
pub async fn handle_trading_reload(
    State(state): State<TradingRouterState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = crate::gateway::api::require_auth(&state.app, &headers) {
        return e.into_response();
    }

    let count = state.trading.engine.reload();
    tracing::info!(count, "zerotrading skills reloaded via API");

    Json(serde_json::json!({
        "status": "ok",
        "skills_loaded": count,
    }))
    .into_response()
}

/// 构建 ZeroTrading 子路由（供 gateway/mod.rs 注册时调用）
///
/// ```rust,ignore
/// // 在 run_gateway() 内：
/// use zeroclaw::zerotrading::api::{TradingRouterState, TradingApiState, trading_router};
/// let trading_state = TradingRouterState {
///     app: state.clone(),
///     trading: TradingApiState { engine, store },
/// };
/// let inner = inner.merge(trading_router(trading_state));
/// ```
pub fn trading_router(state: TradingRouterState) -> axum::Router {
    use axum::routing::{delete, get, patch, post};
    axum::Router::new()
        .route("/api/trading/status", get(handle_trading_status))
        .route(
            "/api/trading/accounts",
            get(handle_trading_accounts_list).post(handle_trading_accounts_upsert),
        )
        .route(
            "/api/trading/accounts/{exchange}/{label}",
            patch(handle_trading_accounts_patch).delete(handle_trading_accounts_delete),
        )
        .route("/api/trading/exchanges", get(handle_trading_exchanges))
        .route("/api/trading/reload", post(handle_trading_reload))
        .with_state(state)
}
