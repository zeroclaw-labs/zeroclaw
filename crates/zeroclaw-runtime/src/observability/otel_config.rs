use zeroclaw_config::schema::{ObservabilityConfig, OtelContentPolicy};

/// Per-observer OTel content policy, derived once from [`ObservabilityConfig`]
/// at the `OtelObserver` construction boundary.
///
/// This is NOT a source of truth — [`ObservabilityConfig`] is. It is an
/// immutable, instance-owned snapshot that the OTel export boundary
/// (`OtelObserver::record_event` and its attribute builders) consult to decide
/// whether/how to emit LLM prompt/completion and tool argument/result content.
///
/// Storing this on each observer (rather than in a process-global mutable
/// cell) prevents last-writer-wins drift between concurrently live observers
/// with different privacy policies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OtelContentConfig {
    pub(crate) genai_policy: OtelContentPolicy,
    pub(crate) genai_max_chars: usize,
    pub(crate) tool_io_policy: OtelContentPolicy,
    pub(crate) tool_io_max_chars: usize,
}

impl OtelContentConfig {
    /// All-off policy: emit no GenAI or tool I/O content attributes.
    /// Used by tests and as a sensible default; non-test builds construct
    /// configs via [`Self::from_observability_config`].
    #[allow(dead_code)]
    pub(crate) fn off() -> Self {
        Self {
            genai_policy: OtelContentPolicy::Off,
            genai_max_chars: 0,
            tool_io_policy: OtelContentPolicy::Off,
            tool_io_max_chars: 0,
        }
    }

    /// Derive the per-observer content config from the source-of-truth
    /// [`ObservabilityConfig`], normalizing `*_max_chars == 0` to `Off` so the
    /// export boundary only has to check the policy variant.
    pub(crate) fn from_observability_config(config: &ObservabilityConfig) -> Self {
        let genai_policy = if config.otel_genai_content_max_chars == 0 {
            OtelContentPolicy::Off
        } else {
            config.otel_genai_content
        };

        let tool_io_policy = if config.otel_tool_io_max_chars == 0 {
            OtelContentPolicy::Off
        } else {
            config.otel_tool_io
        };

        Self {
            genai_policy,
            genai_max_chars: config.otel_genai_content_max_chars,
            tool_io_policy,
            tool_io_max_chars: config.otel_tool_io_max_chars,
        }
    }
}
