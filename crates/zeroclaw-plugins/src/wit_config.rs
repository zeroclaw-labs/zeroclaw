//! Runtime configuration and resource limits for the WIT tool runtime.
//! Ported from `ironclaw_wasm::config`.

use std::time::Duration;

/// WIT package version supported by this runtime (`zeroclaw:plugin@0.1.0`).
pub const WIT_TOOL_VERSION: &str = "0.1.0";

/// How often the background ticker advances the wasmtime epoch. Execution
/// deadlines are rounded up to a whole number of ticks.
pub(crate) const EPOCH_TICK_INTERVAL: Duration = Duration::from_millis(500);
/// Default HTTP timeout when a guest omits `timeout-ms`.
pub(crate) const DEFAULT_HTTP_TIMEOUT_MS: u32 = 30_000;
/// Max log records captured per execution (drop the rest).
pub(crate) const MAX_LOGS_PER_EXECUTION: usize = 1_000;
/// Max bytes of any single captured log message.
pub(crate) const MAX_LOG_MESSAGE_BYTES: usize = 4 * 1024;

const DEFAULT_MEMORY_BYTES: u64 = 10 * 1024 * 1024;
const DEFAULT_FUEL: u64 = 500_000_000;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// Resource limits enforced for one WIT tool execution.
#[derive(Debug, Clone)]
pub struct WitToolLimits {
    /// Aggregate linear-memory ceiling across all of a component's memories.
    pub memory_bytes: u64,
    /// wasmtime fuel budget (CPU work cap).
    pub fuel: u64,
    /// Wall-clock execution deadline (enforced via epoch interruption).
    pub timeout: Duration,
}

impl Default for WitToolLimits {
    fn default() -> Self {
        Self {
            memory_bytes: DEFAULT_MEMORY_BYTES,
            fuel: DEFAULT_FUEL,
            timeout: DEFAULT_TIMEOUT,
        }
    }
}

impl WitToolLimits {
    pub fn with_memory_bytes(mut self, memory_bytes: u64) -> Self {
        self.memory_bytes = memory_bytes;
        self
    }

    pub fn with_fuel(mut self, fuel: u64) -> Self {
        self.fuel = fuel;
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

/// Configuration for the WIT tool runtime.
#[derive(Debug, Clone, Default)]
pub struct WitToolRuntimeConfig {
    pub default_limits: WitToolLimits,
}

impl WitToolRuntimeConfig {
    /// Tighter limits used by the contract test suite.
    pub fn for_testing() -> Self {
        Self {
            default_limits: WitToolLimits::default()
                .with_memory_bytes(1024 * 1024)
                .with_fuel(100_000)
                .with_timeout(Duration::from_secs(5)),
        }
    }
}
