//! Error types for the wasmtime component-model (WIT) tool runtime.
//!
//! Ported from `ironclaw_wasm::error`, with `ironclaw_host_api::ResourceUsage`
//! replaced by the crate-local [`crate::usage::ResourceUsage`].

use thiserror::Error;

use crate::usage::ResourceUsage;
use crate::wit_types::WasmLogRecord;

/// Errors returned by the WIT tool runtime.
#[derive(Debug, Error)]
pub enum WasmError {
    #[error("failed to create WASM engine: {0}")]
    EngineCreationFailed(String),
    #[error("failed to compile WIT component: {0}")]
    CompilationFailed(String),
    #[error("failed to configure WASM store: {0}")]
    StoreConfiguration(String),
    #[error("failed to configure WASM linker: {0}")]
    LinkerConfiguration(String),
    #[error("failed to instantiate WIT component: {0}")]
    InstantiationFailed(String),
    #[error("failed to execute WIT component: {message}")]
    ExecutionFailed {
        message: String,
        usage: ResourceUsage,
        logs: Vec<WasmLogRecord>,
    },
    #[error("tool schema export did not return a valid JSON object: {0}")]
    InvalidSchema(String),
}

impl WasmError {
    pub(crate) fn execution_failed(message: String) -> Self {
        Self::ExecutionFailed {
            message,
            usage: ResourceUsage::default(),
            logs: Vec::new(),
        }
    }
}

/// Errors returned by injected host services (HTTP egress, tool invocation, …).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WasmHostError {
    /// The request was rejected before anything left the host (policy/validation).
    #[error("{0}")]
    Denied(String),
    /// The capability is not configured (fail-closed default).
    #[error("{0}")]
    Unavailable(String),
    /// The request failed before any bytes were sent.
    #[error("{0}")]
    Failed(String),
    /// The request failed after bytes were already sent (egress must be counted).
    #[error("{0}")]
    FailedAfterRequestSent(String),
}

impl WasmHostError {
    /// Whether the request reached the network before failing — controls whether
    /// the host counts the request body toward network egress.
    pub(crate) fn request_was_sent(&self) -> bool {
        matches!(self, Self::FailedAfterRequestSent(_))
    }
}
