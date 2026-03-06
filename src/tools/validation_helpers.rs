//! Common validation helpers for tool execution.
//!
//! This module provides shared validation functions to reduce code duplication
//! across tool implementations. All helpers follow a simple pattern:
//! - Return `Option<ToolResult>` where `Some(err)` means validation failed
//! - Return `None` when validation passes
//!
//! This design allows chaining validations with early returns:
//! ```ignore
//! if let Some(err) = check_rate_limit(&security) { return Ok(err); }
//! if let Some(err) = check_path_allowed(&security, path) { return Ok(err); }
//! // ... actual tool logic
//! ```

use super::traits::ToolResult;
use crate::security::SecurityPolicy;
use std::sync::Arc;

/// Check rate limit and return error if exceeded.
///
/// Fast path check before expensive operations.
#[inline]
pub fn check_rate_limit(security: &SecurityPolicy) -> Option<ToolResult> {
    if security.is_rate_limited() {
        Some(ToolResult {
            success: false,
            output: String::new(),
            error: Some("Rate limit exceeded: too many actions in the last hour".into()),
        })
    } else {
        None
    }
}

/// Record an action and check budget; return error if exhausted.
///
/// Should be called after all pre-validation passes but before
/// the actual side-effecting operation.
#[inline]
pub fn check_record_action(security: &SecurityPolicy) -> Option<ToolResult> {
    if !security.record_action() {
        Some(ToolResult {
            success: false,
            output: String::new(),
            error: Some("Rate limit exceeded: action budget exhausted".into()),
        })
    } else {
        None
    }
}

/// Check if autonomy level permits acting; return error if read-only.
#[inline]
pub fn check_can_act(security: &SecurityPolicy) -> Option<ToolResult> {
    if !security.can_act() {
        Some(ToolResult {
            success: false,
            output: String::new(),
            error: Some("Action blocked: autonomy is read-only".into()),
        })
    } else {
        None
    }
}

/// Check if a path is allowed by security policy.
///
/// Validates against path traversal, absolute paths (when workspace_only),
/// and forbidden paths.
#[inline]
pub fn check_path_allowed(security: &SecurityPolicy, path: &str) -> Option<ToolResult> {
    if !security.is_path_allowed(path) {
        Some(ToolResult {
            success: false,
            output: String::new(),
            error: Some(format!("Path not allowed by security policy: {path}")),
        })
    } else {
        None
    }
}

/// Check if a resolved (canonicalized) path is allowed.
///
/// Use after canonicalizing a path to block symlink escapes.
#[inline]
pub fn check_resolved_path_allowed(
    security: &SecurityPolicy,
    resolved: &std::path::Path,
) -> Option<ToolResult> {
    if !security.is_resolved_path_allowed(resolved) {
        Some(ToolResult {
            success: false,
            output: String::new(),
            error: Some(security.resolved_path_violation_message(resolved)),
        })
    } else {
        None
    }
}

/// Extract a required string parameter from JSON arguments.
///
/// Returns an error if the parameter is missing or not a string.
///
/// # Example
/// ```ignore
/// let path = extract_str_param(&args, "path")?;
/// ```
pub fn extract_str_param(args: &serde_json::Value, key: &str) -> anyhow::Result<&str> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing '{}' parameter", key))
}

/// Extract a required u64 parameter from JSON arguments.
///
/// Returns an error if the parameter is missing or not a number.
pub fn extract_u64_param(args: &serde_json::Value, key: &str) -> anyhow::Result<u64> {
    args.get(key)
        .and_then(|v| v.as_u64())
        .ok_or_else(|| anyhow::anyhow!("Missing '{}' parameter (expected number)", key))
}

/// Check if a string represents an absolute path (starts with / or \).
///
/// Used for rejecting absolute paths in search patterns.
#[inline]
pub fn is_absolute_path(s: &str) -> bool {
    s.starts_with('/') || s.starts_with('\\')
}

/// Check if a string contains path traversal patterns.
///
/// Detects `..` as a path component (not as part of a filename).
#[inline]
pub fn has_path_traversal(s: &str) -> bool {
    s.contains("../") || s.contains("..\\") || s == ".."
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_policy() -> SecurityPolicy {
        SecurityPolicy {
            max_actions_per_hour: 0,
            ..SecurityPolicy::default()
        }
    }

    #[test]
    fn check_rate_limit_returns_none_when_allowed() {
        let policy = SecurityPolicy::default();
        assert!(check_rate_limit(&policy).is_none());
    }

    #[test]
    fn check_rate_limit_returns_error_when_limited() {
        let policy = test_policy();
        let result = check_rate_limit(&policy).unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Rate limit exceeded"));
    }

    #[test]
    fn check_record_action_consumes_budget() {
        let policy = SecurityPolicy {
            max_actions_per_hour: 1,
            ..SecurityPolicy::default()
        };
        // First call should pass
        assert!(check_record_action(&policy).is_none());
        // Second call should fail
        let result = check_record_action(&policy).unwrap();
        assert!(!result.success);
    }

    #[test]
    fn check_can_act_returns_none_when_supervised() {
        let policy = SecurityPolicy::default();
        assert!(check_can_act(&policy).is_none());
    }

    #[test]
    fn extract_str_param_works() {
        let args = serde_json::json!({"path": "test.txt"});
        assert_eq!(extract_str_param(&args, "path").unwrap(), "test.txt");
    }

    #[test]
    fn extract_str_param_errors_on_missing() {
        let args = serde_json::json!({});
        assert!(extract_str_param(&args, "path").is_err());
    }

    #[test]
    fn extract_str_param_errors_on_wrong_type() {
        let args = serde_json::json!({"path": 123});
        assert!(extract_str_param(&args, "path").is_err());
    }

    #[test]
    fn is_absolute_path_detects_unix() {
        assert!(is_absolute_path("/etc/passwd"));
        assert!(is_absolute_path("/home/user/file.txt"));
    }

    #[test]
    fn is_absolute_path_detects_windows() {
        assert!(is_absolute_path("\\windows\\system32"));
        assert!(is_absolute_path("\\\\server\\share"));
    }

    #[test]
    fn is_absolute_path_allows_relative() {
        assert!(!is_absolute_path("file.txt"));
        assert!(!is_absolute_path("./file.txt"));
        assert!(!is_absolute_path("../file.txt"));
    }

    #[test]
    fn has_path_traversal_detects_double_dot() {
        assert!(has_path_traversal("../etc/passwd"));
        assert!(has_path_traversal("..\\windows\\system32"));
        assert!(has_path_traversal(".."));
    }

    #[test]
    fn has_path_traversal_allows_safe() {
        assert!(!has_path_traversal("file.txt"));
        assert!(!has_path_traversal("./file.txt"));
        assert!(!has_path_traversal("../file.txt")); // Contains .. but not as a separate path component
        assert!(!has_path_traversal("file..txt"));
    }
}
