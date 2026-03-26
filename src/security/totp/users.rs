// Multi-user registry with role resolution (D11, D25).
//
// Resolves user identity from session data, applies role inheritance,
// and determines blocked operations per role.

use std::collections::{HashMap, HashSet};

use super::config::{RoleConfig, TotpConfig, UserConfig};
use super::types::UserIdentity;

/// Resolved user with all inherited permissions.
#[derive(Debug, Clone)]
pub struct ResolvedUser {
    pub id: String,
    pub name: String,
    pub role: String,
    pub identity: UserIdentity,
    /// All blocked operations (from this role + all parent roles).
    pub blocked_operations: HashSet<String>,
}

/// User registry — resolves identities and roles from config.
pub struct UserRegistry {
    users: Vec<UserConfig>,
    roles: HashMap<String, RoleConfig>,
}

impl UserRegistry {
    pub fn from_config(config: &TotpConfig) -> Self {
        Self {
            users: config.users.clone(),
            roles: config.roles.clone(),
        }
    }

    /// Find a user by their identity string (e.g., "telegram:123456789").
    pub fn resolve_by_identity(&self, identity_str: &str) -> Option<ResolvedUser> {
        let user_config = self.users.iter().find(|u| u.identity == identity_str)?;
        self.resolve_user(user_config)
    }

    /// Find a user by their user ID.
    pub fn resolve_by_id(&self, user_id: &str) -> Option<ResolvedUser> {
        let user_config = self.users.iter().find(|u| u.id == user_id)?;
        self.resolve_user(user_config)
    }

    /// Check if a command is blocked for a given user's role.
    pub fn is_operation_blocked(&self, user_id: &str, operation: &str) -> bool {
        self.resolve_by_id(user_id)
            .map(|u| u.blocked_operations.iter().any(|b| operation.contains(b.as_str())))
            .unwrap_or(true) // unknown user = blocked
    }

    /// List all user IDs.
    pub fn user_ids(&self) -> Vec<&str> {
        self.users.iter().map(|u| u.id.as_str()).collect()
    }

    fn resolve_user(&self, user_config: &UserConfig) -> Option<ResolvedUser> {
        let identity = UserIdentity::parse(&user_config.identity)?;
        let blocked = self.collect_blocked_operations(&user_config.role);

        Some(ResolvedUser {
            id: user_config.id.clone(),
            name: user_config.name.clone(),
            role: user_config.role.clone(),
            identity,
            blocked_operations: blocked,
        })
    }

    /// Collect all blocked operations from the role chain (with inheritance).
    /// Protects against circular inheritance with a visited set.
    fn collect_blocked_operations(&self, role_name: &str) -> HashSet<String> {
        let mut blocked = HashSet::new();
        let mut visited = HashSet::new();
        self.collect_blocked_recursive(role_name, &mut blocked, &mut visited);
        blocked
    }

    fn collect_blocked_recursive(
        &self,
        role_name: &str,
        blocked: &mut HashSet<String>,
        visited: &mut HashSet<String>,
    ) {
        if !visited.insert(role_name.to_string()) {
            return; // circular inheritance guard
        }

        if let Some(role) = self.roles.get(role_name) {
            for op in &role.blocked_operations {
                blocked.insert(op.clone());
            }
            // Recurse into parent role
            if let Some(ref parent) = role.inherits {
                self.collect_blocked_recursive(parent, blocked, visited);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::config::*;
    use super::super::types::TotpStatus;

    fn test_config() -> TotpConfig {
        let mut config = TotpConfig::default();
        config.enabled = true;

        // Roles with inheritance
        config.roles.insert("base".to_string(), RoleConfig {
            inherits: None,
            blocked_operations: vec!["system.shutdown".to_string()],
        });
        config.roles.insert("attorney".to_string(), RoleConfig {
            inherits: Some("base".to_string()),
            blocked_operations: vec![], // inherits base's blocks
        });
        config.roles.insert("paralegal".to_string(), RoleConfig {
            inherits: Some("base".to_string()),
            blocked_operations: vec![
                "akte.loeschen".to_string(),
                "gericht.einreichen".to_string(),
            ],
        });

        // Users
        config.users = vec![
            UserConfig {
                id: "mueller".to_string(),
                name: "RA Dr. Mueller".to_string(),
                role: "attorney".to_string(),
                identity: "telegram:123456789".to_string(),
                totp_status: TotpStatus::Active,
            },
            UserConfig {
                id: "bauer".to_string(),
                name: "Fr. Bauer".to_string(),
                role: "paralegal".to_string(),
                identity: "os_user:bauer".to_string(),
                totp_status: TotpStatus::Active,
            },
        ];

        config
    }

    #[test]
    fn resolve_by_identity() {
        let config = test_config();
        let registry = UserRegistry::from_config(&config);
        let user = registry.resolve_by_identity("telegram:123456789").unwrap();
        assert_eq!(user.id, "mueller");
        assert_eq!(user.role, "attorney");
    }

    #[test]
    fn resolve_by_id() {
        let config = test_config();
        let registry = UserRegistry::from_config(&config);
        let user = registry.resolve_by_id("bauer").unwrap();
        assert_eq!(user.name, "Fr. Bauer");
        assert_eq!(user.role, "paralegal");
    }

    #[test]
    fn role_inheritance_collects_parent_blocks() {
        let config = test_config();
        let registry = UserRegistry::from_config(&config);

        // Paralegal: has own blocks + inherits base's system.shutdown
        let user = registry.resolve_by_id("bauer").unwrap();
        assert!(user.blocked_operations.contains("akte.loeschen"));
        assert!(user.blocked_operations.contains("gericht.einreichen"));
        assert!(user.blocked_operations.contains("system.shutdown")); // inherited
    }

    #[test]
    fn attorney_inherits_base_blocks_only() {
        let config = test_config();
        let registry = UserRegistry::from_config(&config);

        let user = registry.resolve_by_id("mueller").unwrap();
        assert!(user.blocked_operations.contains("system.shutdown")); // from base
        assert!(!user.blocked_operations.contains("akte.loeschen")); // not blocked for attorney
    }

    #[test]
    fn blocked_operation_check() {
        let config = test_config();
        let registry = UserRegistry::from_config(&config);

        assert!(registry.is_operation_blocked("bauer", "akte.loeschen"));
        assert!(!registry.is_operation_blocked("mueller", "akte.loeschen"));
        assert!(registry.is_operation_blocked("bauer", "system.shutdown"));
    }

    #[test]
    fn unknown_user_is_blocked() {
        let config = test_config();
        let registry = UserRegistry::from_config(&config);
        assert!(registry.is_operation_blocked("unknown", "anything"));
    }

    #[test]
    fn unknown_identity_returns_none() {
        let config = test_config();
        let registry = UserRegistry::from_config(&config);
        assert!(registry.resolve_by_identity("telegram:999999").is_none());
    }
}
