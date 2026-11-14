//! Server error model — maps to the spec JSON-RPC codes + `reason`.
//!
//! Handlers produce error responses from this; errors collect here instead of as raw strings.

/// Unified server error -> JSON-RPC code + `reason`.
#[derive(Debug, Clone)]
pub enum ServerError {
    /// -32001 task not found.
    TaskNotFound(String),
    /// -32601 unknown method.
    MethodNotFound(String),
    /// -32602 invalid params.
    InvalidParams(String),
    /// -32603 internal error.
    Internal(String),
    /// -32004 unsupported operation (e.g. `returnImmediately`, unimplemented continuation).
    UnsupportedOperation(String),
}

impl ServerError {
    #[must_use]
    pub fn jsonrpc_code(&self) -> i64 {
        match self {
            ServerError::TaskNotFound(_) => -32001,
            ServerError::MethodNotFound(_) => -32601,
            ServerError::InvalidParams(_) => -32602,
            ServerError::Internal(_) => -32603,
            ServerError::UnsupportedOperation(_) => -32004,
        }
    }

    #[must_use]
    pub fn reason(&self) -> &'static str {
        match self {
            ServerError::TaskNotFound(_) => "TASK_NOT_FOUND",
            ServerError::MethodNotFound(_) => "METHOD_NOT_FOUND",
            ServerError::InvalidParams(_) => "INVALID_PARAMS",
            ServerError::Internal(_) => "INTERNAL_ERROR",
            ServerError::UnsupportedOperation(_) => "UNSUPPORTED_OPERATION",
        }
    }

    #[must_use]
    pub fn message(&self) -> String {
        match self {
            ServerError::TaskNotFound(id) => format!("task not found: {id}"),
            ServerError::MethodNotFound(m) => format!("method not found: {m}"),
            ServerError::InvalidParams(s) => format!("invalid params: {s}"),
            ServerError::Internal(s) => format!("internal error: {s}"),
            ServerError::UnsupportedOperation(s) => s.clone(),
        }
    }
}

impl std::fmt::Display for ServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message())
    }
}

impl std::error::Error for ServerError {}
