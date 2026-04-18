//! Lightweight per-channel message statistics registry.
//!
//! Process-scoped counters that reset on daemon restart.
//! Used by the gateway `/api/channels` endpoint.

use chrono::Utc;
use parking_lot::Mutex;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Debug, Clone, Serialize)]
pub struct ChannelStatEntry {
    pub message_count: u64,
    pub last_message_at: Option<String>,
}

struct StatsRegistry {
    entries: Mutex<HashMap<String, ChannelStatEntry>>,
}

static REGISTRY: OnceLock<StatsRegistry> = OnceLock::new();

fn registry() -> &'static StatsRegistry {
    REGISTRY.get_or_init(|| StatsRegistry {
        entries: Mutex::new(HashMap::new()),
    })
}

/// Record an inbound message for the given channel.
pub fn record_message(channel: &str) {
    let mut map = registry().entries.lock();
    let entry = map.entry(channel.to_string()).or_insert(ChannelStatEntry {
        message_count: 0,
        last_message_at: None,
    });
    entry.message_count += 1;
    entry.last_message_at = Some(Utc::now().to_rfc3339());
}

/// Return a snapshot of all per-channel statistics.
pub fn get_stats() -> HashMap<String, ChannelStatEntry> {
    registry().entries.lock().clone()
}
