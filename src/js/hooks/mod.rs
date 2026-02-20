// Hook types for JS plugin system
//
// This module defines the result types that hook handlers return.
// Hooks allow plugins to observe, modify, or veto operations in ZeroClaw.
//
// # Security Considerations
//
// Hooks execute plugin code with access to sensitive event data.
// HookResult::Modified can change event payloads before they reach
// the core system, and HookResult::Veto can block operations entirely.
//
// Plugin code MUST NOT:
// - Expose veto reasons containing sensitive data
// - Log raw event payloads in modification hooks
// - Grant additional permissions through modifications

pub mod registry;

use serde_json::Value;

/// Result of a hook execution
///
/// Represents the outcome of a plugin hook handler. Each hook can
/// either observe events passively, modify event data, or veto the operation.
#[derive(Debug, Clone, Default)]
pub enum HookResult {
    /// Fire-and-forget observation, no effect on event
    ///
    /// The hook runs for its side effects only (logging, metrics, etc.)
    /// and does not influence the execution flow.
    #[default]
    Observation,

    /// Modified the event data
    ///
    /// The hook has transformed the event payload. Modified data
    /// will be passed to subsequent hooks and used by the core system.
    Modified(Value),

    /// Vetoed the operation (with reason)
    ///
    /// The hook has blocked the operation from proceeding. The reason
    /// should be a safe, non-sensitive description of why the veto occurred.
    Veto(String),
}

impl HookResult {
    /// Returns true if this is an observation hook (no side effects)
    pub fn is_observation(&self) -> bool {
        matches!(self, HookResult::Observation)
    }

    /// Returns true if this hook vetoes the operation
    pub fn is_veto(&self) -> bool {
        matches!(self, HookResult::Veto(_))
    }

    /// Returns true if this hook modifies the event data
    pub fn is_modified(&self) -> bool {
        matches!(self, HookResult::Modified(_))
    }

    /// Creates a new observation result
    pub fn observation() -> Self {
        HookResult::Observation
    }

    /// Creates a new veto result with the given reason
    pub fn veto(reason: impl Into<String>) -> Self {
        HookResult::Veto(reason.into())
    }

    /// Creates a new modified result with the given value
    pub fn modified(value: Value) -> Self {
        HookResult::Modified(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_result_observation() {
        let result = HookResult::observation();
        assert!(result.is_observation());
        assert!(!result.is_veto());
        assert!(!result.is_modified());
    }

    #[test]
    fn hook_result_veto() {
        let result = HookResult::veto("test reason");
        assert!(!result.is_observation());
        assert!(result.is_veto());
        assert!(!result.is_modified());
    }

    #[test]
    fn hook_result_modified() {
        let result = HookResult::modified(Value::Null);
        assert!(!result.is_observation());
        assert!(!result.is_veto());
        assert!(result.is_modified());
    }

    #[test]
    fn hook_result_default() {
        let result = HookResult::default();
        assert!(result.is_observation());
    }
}
