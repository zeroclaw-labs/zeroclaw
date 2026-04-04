//! One2X custom extensions for ZeroClaw.
//!
//! All custom functionality lives here, minimizing upstream file changes.
//! New files in this module have **zero merge conflict risk**.
//!
//! ## Upstream Integration Points (tiny patches, one per file)
//!
//! | File | Change | Lines |
//! |------|--------|-------|
//! | `lib.rs` | module declaration | 2 |
//! | `Cargo.toml` | `one2x = []` feature | 2 |
//! | `config/schema.rs` | `web` field on `ChannelsConfig` | ~8 |
//! | `channels/mod.rs` | `extend_channels()` call + visibility | ~5 |
//! | `gateway/mod.rs` | `extend_router()` call | ~3 |
//! | `gateway/api.rs` | memory prefix/get query fields + handler | ~50 |
//! | `memory/traits.rs` | `list_by_prefix` default method | ~13 |
//! | `memory/sqlite.rs` | `list_by_prefix` SQLite impl | ~43 |
//! | `cron/scheduler.rs` | web/lark/feishu delivery arms | ~24 |
//! | `daemon/mod.rs` | heartbeat validation arms | ~22 |
//! | `tools/cron_add.rs` | delivery enum values | 1 |
//! | `tools/shell.rs` | `ZEROCLAW_SESSION_ID` env | ~9 |
//! | `agent/loop_.rs` | `TOOL_LOOP_REPLY_TARGET` task-local | ~20 |
//!
//! ## Merge Workflow
//!
//! Run `dev/merge-upstream.sh` to automate upstream syncing.

pub mod agent_sse;
pub mod config;
pub mod gateway_ext;
pub mod web_channel;

use crate::channels::ConfiguredChannel;
use crate::config::Config;
use crate::gateway::AppState;
use axum::{
    Router,
    routing::{get, post},
};
use std::sync::Arc;

/// Extend the gateway router with One2X-specific routes.
/// Called from `gateway::run_gateway()` via `#[cfg(feature = "one2x")]`.
pub(crate) fn extend_router(router: Router<AppState>) -> Router<AppState> {
    router
        .route("/agent", post(agent_sse::handle_agent_sse))
        .route("/agent/clear", post(agent_sse::handle_agent_clear))
        .route("/ws/channel", get(gateway_ext::handle_ws_channel))
}

/// Extend the configured channels list with the Web channel.
/// Called from `channels::collect_configured_channels()` via `#[cfg(feature = "one2x")]`.
pub(crate) fn extend_channels(channels: &mut Vec<ConfiguredChannel>, config: &Config) {
    if config
        .channels_config
        .web
        .as_ref()
        .is_some_and(|w| w.enabled)
    {
        let web_channel = web_channel::get_or_init_web_channel();
        channels.push(ConfiguredChannel {
            display_name: "Web",
            channel: Arc::clone(&web_channel) as _,
        });
    }
}
