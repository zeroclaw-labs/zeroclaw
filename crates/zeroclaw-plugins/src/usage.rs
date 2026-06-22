//! Resource-usage accounting for one WIT tool execution.
//!
//! Local replacement for `ironclaw_host_api::ResourceUsage` (zeroclaw has no
//! host-api crate). Captured per execution and returned alongside the result so
//! callers can meter plugin cost.

/// Resources consumed by a single WIT tool execution.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResourceUsage {
    /// Wall-clock time spent in `execute`, in milliseconds.
    pub wall_clock_ms: u64,
    /// Bytes of output the tool returned.
    pub output_bytes: u64,
    /// Bytes sent as HTTP request bodies through the host egress boundary.
    pub network_egress_bytes: u64,
}
