//! Policy engine for memory operations.
//!
//! Validates operations against configurable rules before they reach the
//! backend. Enforces namespace quotas, category limits, read-only namespaces,
//! and per-category retention rules.

use super::traits::MemoryCategory;
use crate::config::MemoryPolicyConfig;

/// Policy enforcer that validates memory operations.
pub struct PolicyEnforcer {
    config: MemoryPolicyConfig,
}

impl PolicyEnforcer {
    pub fn new(config: &MemoryPolicyConfig) -> Self {
        Self {
            config: config.clone(),
        }
    }

    /// Check if a namespace is read-only.
    pub fn is_read_only(&self, namespace: &str) -> bool {
        self.config
            .read_only_namespaces
            .iter()
            .any(|ns| ns == namespace)
    }

    /// Validate a store operation against policy rules.
    pub fn validate_store(
        &self,
        namespace: &str,
        _category: &MemoryCategory,
    ) -> Result<(), PolicyViolation> {
        if self.is_read_only(namespace) {
            return Err(PolicyViolation::ReadOnlyNamespace(namespace.to_string()));
        }
        Ok(())
    }

    /// Check if adding an entry would exceed namespace limits.
    pub fn check_namespace_limit(&self, current_count: usize) -> Result<(), PolicyViolation> {
        if self.config.max_entries_per_namespace > 0
            && current_count >= self.config.max_entries_per_namespace
        {
            return Err(PolicyViolation::NamespaceQuotaExceeded {
                max: self.config.max_entries_per_namespace,
                current: current_count,
            });
        }
        Ok(())
    }

    /// Check if adding an entry would exceed category limits.
    pub fn check_category_limit(&self, current_count: usize) -> Result<(), PolicyViolation> {
        if self.config.max_entries_per_category > 0
            && current_count >= self.config.max_entries_per_category
        {
            return Err(PolicyViolation::CategoryQuotaExceeded {
                max: self.config.max_entries_per_category,
                current: current_count,
            });
        }
        Ok(())
    }

    /// Get the retention days for a specific category, falling back to the
    /// provided default if no per-category override exists.
    pub fn retention_days_for_category(&self, category: &MemoryCategory, default_days: u32) -> u32 {
        let key = category.to_string();
        self.config
            .retention_days_by_category
            .get(&key)
            .copied()
            .unwrap_or(default_days)
    }
}

/// Policy violation errors.
#[derive(Debug, Clone)]
pub enum PolicyViolation {
    ReadOnlyNamespace(String),
    NamespaceQuotaExceeded { max: usize, current: usize },
    CategoryQuotaExceeded { max: usize, current: usize },
}

impl std::fmt::Display for PolicyViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReadOnlyNamespace(ns) => write!(f, "namespace '{ns}' is read-only"),
            Self::NamespaceQuotaExceeded { max, current } => {
                write!(f, "namespace quota exceeded: {current}/{max} entries")
            }
            Self::CategoryQuotaExceeded { max, current } => {
                write!(f, "category quota exceeded: {current}/{max} entries")
            }
        }
    }
}

impl std::error::Error for PolicyViolation {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn empty_policy() -> MemoryPolicyConfig {
        MemoryPolicyConfig::default()
    }

    #[test]
    fn default_policy_allows_everything() {
        let enforcer = PolicyEnforcer::new(&empty_policy());
        assert!(!enforcer.is_read_only("default"));
        assert!(
            enforcer
                .validate_store("default", &MemoryCategory::Core)
                .is_ok()
        );
        assert!(enforcer.check_namespace_limit(100).is_ok());
        assert!(enforcer.check_category_limit(100).is_ok());
    }

    #[test]
    fn read_only_namespace_blocks_writes() {
        let policy = MemoryPolicyConfig {
            read_only_namespaces: vec!["archive".into()],
            ..empty_policy()
        };
        let enforcer = PolicyEnforcer::new(&policy);

        assert!(enforcer.is_read_only("archive"));
        assert!(!enforcer.is_read_only("default"));
        assert!(
            enforcer
                .validate_store("archive", &MemoryCategory::Core)
                .is_err()
        );
        assert!(
            enforcer
                .validate_store("default", &MemoryCategory::Core)
                .is_ok()
        );
    }

    #[test]
    fn namespace_quota_enforced() {
        let policy = MemoryPolicyConfig {
            max_entries_per_namespace: 10,
            ..empty_policy()
        };
        let enforcer = PolicyEnforcer::new(&policy);

        assert!(enforcer.check_namespace_limit(5).is_ok());
        assert!(enforcer.check_namespace_limit(10).is_err());
        assert!(enforcer.check_namespace_limit(15).is_err());
    }

    #[test]
    fn category_quota_enforced() {
        let policy = MemoryPolicyConfig {
            max_entries_per_category: 50,
            ..empty_policy()
        };
        let enforcer = PolicyEnforcer::new(&policy);

        assert!(enforcer.check_category_limit(25).is_ok());
        assert!(enforcer.check_category_limit(50).is_err());
    }

    #[test]
    fn per_category_retention_overrides_default() {
        let mut retention = HashMap::new();
        retention.insert("core".into(), 365);
        retention.insert("conversation".into(), 7);

        let policy = MemoryPolicyConfig {
            retention_days_by_category: retention,
            ..empty_policy()
        };
        let enforcer = PolicyEnforcer::new(&policy);

        assert_eq!(
            enforcer.retention_days_for_category(&MemoryCategory::Core, 30),
            365
        );
        assert_eq!(
            enforcer.retention_days_for_category(&MemoryCategory::Conversation, 30),
            7
        );
        assert_eq!(
            enforcer.retention_days_for_category(&MemoryCategory::Daily, 30),
            30
        );
    }
}
