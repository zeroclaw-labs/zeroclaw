use parking_lot::RwLock;
use std::sync::OnceLock;
use zeroclaw_config::schema::OtelContentPolicy;

/// Runtime OTel content policy configuration.
/// Stored globally so both `capture_llm_messages` (in agent::loop_) and
/// `otel.rs` can read it without introducing circular dependencies.
#[derive(Debug, Clone, Copy)]
pub struct OtelContentConfig {
    pub genai_policy: OtelContentPolicy,
    pub genai_max_chars: usize,
    pub tool_io_policy: OtelContentPolicy,
    pub tool_io_max_chars: usize,
}

static OTEL_CONTENT_CONFIG: OnceLock<RwLock<OtelContentConfig>> = OnceLock::new();

/// Set the global OTel content configuration.
/// Called by `create_primary_observer` when initializing the OtelObserver.
///
/// Uses `get_or_init` + `write()` (rather than `OnceLock::set`) so the value
/// can be updated if the observer is rebuilt, and so tests can install a
/// non-default policy. Last writer wins.
pub fn set_otel_content_config(config: OtelContentConfig) {
    let lock = OTEL_CONTENT_CONFIG.get_or_init(|| RwLock::new(config));
    *lock.write() = config;
}

/// Get the global OTel content configuration.
/// Returns a default (all-off) config if not yet set.
pub fn otel_content_config() -> OtelContentConfig {
    OTEL_CONTENT_CONFIG
        .get()
        .map(|lock| *lock.read())
        .unwrap_or(OtelContentConfig {
            genai_policy: OtelContentPolicy::Off,
            genai_max_chars: 0,
            tool_io_policy: OtelContentPolicy::Off,
            tool_io_max_chars: 0,
        })
}
