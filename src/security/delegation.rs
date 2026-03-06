//! Delegation token system for multi-agent coordination security.
//!
//! This module provides cryptographically-verifiable delegation tokens that:
//! - Prevent message spoofing by verifying delegation chain integrity
//! - Enforce depth limits immutably through HMAC signing
//! - Track delegation provenance for audit purposes
//!
//! ## Usage
//!
//! ```rust
//! use crate::security::delegation::{DelegationToken, DelegationStore};
//!
//! // Root delegation (from system/user)
//! let root = DelegationToken::new_root("agent_a", "agent_b");
//!
//! // Child delegation
//! let child = DelegationToken::new_child(&root, "agent_b", "agent_c");
//!
//! // Verify chain
//! assert!(child.verify_chain(&root));
//! ```

use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use tokio::sync::RwLock;

type HmacSha256 = Hmac<Sha256>;

/// Maximum delegation depth (prevents infinite loops)
pub const MAX_DELEGATION_DEPTH: u32 = 10;

/// Delegation token lifetime (seconds)
pub const TOKEN_LIFETIME_SECS: i64 = 3600;

/// Cryptographic delegation token
///
/// Each token contains:
/// - A chain of hashes linking to the root delegation
/// - The delegating and target agent IDs
/// - The current depth in the delegation chain
/// - An HMAC signature for verification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationToken {
    /// Hash of parent token (None for root tokens)
    pub parent_hash: Option<String>,
    /// Agent that is delegating
    pub delegating_agent: String,
    /// Agent being delegated to
    pub target_agent: String,
    /// Depth in delegation chain (0 for root)
    pub depth: u32,
    /// When this token was issued
    pub issued_at: DateTime<Utc>,
    /// When this token expires
    pub expires_at: DateTime<Utc>,
    /// Unique ID for this delegation
    pub delegation_id: String,
    /// HMAC signature (hex-encoded)
    pub signature: String,
}

impl DelegationToken {
    /// Create a new root delegation token (from system/user)
    ///
    /// Root tokens have no parent and depth=0.
    pub fn new_root(
        delegating_agent: &str,
        target_agent: &str,
        secret_key: &[u8; 32],
    ) -> Self {
        let delegation_id = Self::generate_delegation_id();
        let issued_at = Utc::now();
        let expires_at = issued_at + chrono::Duration::seconds(TOKEN_LIFETIME_SECS);

        let mut token = Self {
            parent_hash: None,
            delegating_agent: delegating_agent.to_string(),
            target_agent: target_agent.to_string(),
            depth: 0,
            issued_at,
            expires_at,
            delegation_id,
            signature: String::new(),
        };

        token.sign(secret_key);
        token
    }

    /// Create a child delegation token from a parent
    ///
    /// Child tokens reference their parent's hash and increment depth.
    pub fn new_child(
        parent: &DelegationToken,
        delegating_agent: &str,
        target_agent: &str,
        secret_key: &[u8; 32],
    ) -> Result<Self> {
        // Check depth limit
        if parent.depth >= MAX_DELEGATION_DEPTH {
            bail!(
                "Maximum delegation depth reached: {}",
                MAX_DELEGATION_DEPTH
            );
        }

        // Check parent hasn't expired
        if parent.expires_at < Utc::now() {
            bail!("Parent delegation token has expired");
        }

        let delegation_id = Self::generate_delegation_id();
        let issued_at = Utc::now();
        let expires_at = (issued_at + chrono::Duration::seconds(TOKEN_LIFETIME_SECS))
            .min(parent.expires_at);

        let parent_hash = parent.compute_hash();

        let mut token = Self {
            parent_hash: Some(parent_hash),
            delegating_agent: delegating_agent.to_string(),
            target_agent: target_agent.to_string(),
            depth: parent.depth + 1,
            issued_at,
            expires_at,
            delegation_id,
            signature: String::new(),
        };

        token.sign(secret_key);
        Ok(token)
    }

    /// Verify the token signature
    pub fn verify(&self, secret_key: &[u8; 32]) -> bool {
        // First check expiration
        if self.expires_at < Utc::now() {
            return false;
        }

        // Compute expected signature
        let expected = self.compute_signature(secret_key);

        // Constant-time comparison to prevent timing attacks
        if self.signature.len() != expected.len() {
            return false;
        }

        let mut result = 0u8;
        for (a, b) in self.signature.bytes().zip(expected.bytes()) {
            result |= a ^ b;
        }
        result == 0
    }

    /// Verify the delegation chain integrity
    ///
    /// Checks that the parent_hash correctly links to the parent token.
    pub fn verify_chain(&self, parent: &DelegationToken, secret_key: &[u8; 32]) -> bool {
        // Verify this token
        if !self.verify(secret_key) {
            return false;
        }

        // Verify parent
        if !parent.verify(secret_key) {
            return false;
        }

        // Verify depth is monotonic
        if self.depth != parent.depth + 1 {
            return false;
        }

        // Verify parent hash matches
        let expected_parent_hash = parent.compute_hash();
        match &self.parent_hash {
            Some(hash) if hash == &expected_parent_hash => true,
            _ => false,
        }
    }

    /// Generate a unique delegation ID
    fn generate_delegation_id() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("del-{}-{}", nonce, uuid::Uuid::new_v4())
    }

    /// Compute the hash of this token (for parent linking)
    fn compute_hash(&self) -> String {
        let data = format!(
            "{}|{}|{}|{}|{}|{}",
            self.parent_hash.as_deref().unwrap_or(""),
            self.delegating_agent,
            self.target_agent,
            self.depth,
            self.issued_at.timestamp(),
            self.delegation_id
        );
        let mut hasher = Sha256::new();
        hasher.update(data.as_bytes());
        hex::encode(hasher.finalize())
    }

    /// Compute HMAC signature
    fn compute_signature(&self, secret_key: &[u8; 32]) -> String {
        let data = format!(
            "{}|{}|{}|{}|{}|{}|{}",
            self.parent_hash.as_deref().unwrap_or(""),
            self.delegating_agent,
            self.target_agent,
            self.depth,
            self.issued_at.timestamp(),
            self.expires_at.timestamp(),
            self.delegation_id
        );

        let mut mac = HmacSha256::new_from_slice(secret_key).expect("HMAC key size");
        mac.update(data.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    /// Sign the token with the secret key
    fn sign(&mut self, secret_key: &[u8; 32]) {
        self.signature = self.compute_signature(secret_key);
    }

    /// Check if this token is expired
    pub fn is_expired(&self) -> bool {
        self.expires_at < Utc::now()
    }

    /// Get the remaining lifetime in seconds
    pub fn remaining_lifetime(&self) -> i64 {
        (self.expires_at - Utc::now()).num_seconds().max(0)
    }
}

/// Store for active delegation tokens
///
/// Provides tracking and revocation of delegation tokens.
pub struct DelegationStore {
    /// Active tokens by delegation ID
    tokens: RwLock<HashMap<String, DelegationToken>>,
    /// Per-agent active delegation count
    agent_delegations: RwLock<HashMap<String, usize>>,
    /// Secret key for signing tokens
    secret_key: [u8; 32],
    /// Maximum active delegations per agent
    max_per_agent: usize,
}

impl DelegationStore {
    /// Create a new delegation store with a random secret key
    pub fn new() -> Self {
        Self::with_config(10)
    }

    /// Create a new delegation store with specific limits
    pub fn with_config(max_per_agent: usize) -> Self {
        // Generate secret key from system time and process ID
        let secret_key = Self::generate_secret_key();

        Self {
            tokens: RwLock::new(HashMap::new()),
            agent_delegations: RwLock::new(HashMap::new()),
            secret_key,
            max_per_agent,
        }
    }

    /// Generate a secret key from system entropy
    fn generate_secret_key() -> [u8; 32] {
        use std::time::{SystemTime, UNIX_EPOCH};
        let mut key = [0u8; 32];

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0) as u64;

        // Mix in various sources of entropy
        let mut hasher = Sha256::new();
        hasher.update(&nonce.to_le_bytes());
        hasher.update(&std::process::id().to_le_bytes());
        hasher.update(&std::env::var("USER").unwrap_or_default().as_bytes());
        hasher.update(b"zeroclaw-delegation-secret");

        let result = hasher.finalize();
        key.copy_from_slice(&result[..32]);
        key
    }

    /// Create with a specific secret key (for testing)
    pub fn with_secret_key(secret_key: [u8; 32], max_per_agent: usize) -> Self {
        Self {
            tokens: RwLock::new(HashMap::new()),
            agent_delegations: RwLock::new(HashMap::new()),
            secret_key,
            max_per_agent,
        }
    }

    /// Create and register a root delegation token
    pub async fn create_root(
        &self,
        delegating_agent: &str,
        target_agent: &str,
    ) -> Result<DelegationToken> {
        self.check_agent_limit(delegating_agent).await?;

        let token = DelegationToken::new_root(delegating_agent, target_agent, &self.secret_key);
        self.register_token(&token).await?;
        Ok(token)
    }

    /// Create and register a child delegation token
    pub async fn create_child(
        &self,
        parent: &DelegationToken,
        delegating_agent: &str,
        target_agent: &str,
    ) -> Result<DelegationToken> {
        self.check_agent_limit(delegating_agent).await?;

        let token =
            DelegationToken::new_child(parent, delegating_agent, target_agent, &self.secret_key)?;
        self.register_token(&token).await?;
        Ok(token)
    }

    /// Verify and retrieve a token
    pub async fn verify(&self, delegation_id: &str) -> Option<DelegationToken> {
        let tokens = self.tokens.read().await;
        let token = tokens.get(delegation_id)?;

        // Verify signature and expiration
        if token.verify(&self.secret_key) {
            Some(token.clone())
        } else {
            None
        }
    }

    /// Revoke a delegation token
    pub async fn revoke(&self, delegation_id: &str) -> Result<()> {
        // Clone the token if it exists
        let token = {
            let tokens = self.tokens.read().await;
            tokens.get(delegation_id).cloned()
        };

        if let Some(token) = token {
            self.unregister_token(&token).await?;
            Ok(())
        } else {
            bail!("Delegation token not found: {}", delegation_id);
        }
    }

    /// Revoke all delegations for an agent
    pub async fn revoke_agent(&self, agent_id: &str) -> Result<()> {
        let mut tokens = self.tokens.write().await;
        let mut to_remove = Vec::new();

        for (id, token) in tokens.iter() {
            if &token.delegating_agent == agent_id || &token.target_agent == agent_id {
                to_remove.push(id.clone());
            }
        }

        for id in to_remove {
            let token = tokens.remove(&id).unwrap();
            self.decrement_agent_count(&token.delegating_agent).await;
        }

        Ok(())
    }

    /// Clean up expired tokens
    pub async fn cleanup_expired(&self) -> usize {
        let mut tokens = self.tokens.write().await;
        let mut to_remove = Vec::new();

        for (id, token) in tokens.iter() {
            if token.is_expired() {
                to_remove.push(id.clone());
            }
        }

        let count = to_remove.len();
        for id in to_remove {
            if let Some(token) = tokens.remove(&id) {
                self.decrement_agent_count(&token.delegating_agent).await;
            }
        }

        count
    }

    /// Get statistics about active delegations
    pub async fn stats(&self) -> DelegationStats {
        let tokens = self.tokens.read().await;
        let agent_counts = self.agent_delegations.read().await;

        let total = tokens.len();
        let expired = tokens.values().filter(|t| t.is_expired()).count();
        let by_depth = {
            let mut counts = [0usize; 11];
            for token in tokens.values() {
                let idx = (token.depth as usize).min(10);
                counts[idx] += 1;
            }
            counts.to_vec()
        };

        DelegationStats {
            total,
            active: total - expired,
            expired,
            by_depth,
            by_agent: agent_counts.clone(),
        }
    }

    /// Register a token in the store
    async fn register_token(&self, token: &DelegationToken) -> Result<()> {
        let mut tokens = self.tokens.write().await;
        tokens.insert(token.delegation_id.clone(), token.clone());

        let mut counts = self.agent_delegations.write().await;
        *counts.entry(token.delegating_agent.clone()).or_insert(0) += 1;

        Ok(())
    }

    /// Unregister a token from the store
    async fn unregister_token(&self, token: &DelegationToken) -> Result<()> {
        let mut tokens = self.tokens.write().await;
        tokens.remove(&token.delegation_id);
        self.decrement_agent_count(&token.delegating_agent).await;
        Ok(())
    }

    /// Check if agent can create more delegations
    async fn check_agent_limit(&self, agent_id: &str) -> Result<()> {
        let counts = self.agent_delegations.read().await;
        let current = *counts.get(agent_id).unwrap_or(&0);
        if current >= self.max_per_agent {
            bail!(
                "Agent '{}' has reached maximum active delegations: {}",
                agent_id,
                self.max_per_agent
            );
        }
        Ok(())
    }

    /// Decrement the delegation count for an agent
    async fn decrement_agent_count(&self, agent_id: &str) {
        let mut counts = self.agent_delegations.write().await;
        if let Some(count) = counts.get_mut(agent_id) {
            if *count > 0 {
                *count -= 1;
            }
            if *count == 0 {
                counts.remove(agent_id);
            }
        }
    }
}

impl Default for DelegationStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Delegation statistics
#[derive(Debug, Clone)]
pub struct DelegationStats {
    pub total: usize,
    pub active: usize,
    pub expired: usize,
    pub by_depth: Vec<usize>,
    pub by_agent: HashMap<String, usize>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_root_token_creation() {
        let secret_key = [0u8; 32];
        let token = DelegationToken::new_root("agent_a", "agent_b", &secret_key);

        assert_eq!(token.delegating_agent, "agent_a");
        assert_eq!(token.target_agent, "agent_b");
        assert_eq!(token.depth, 0);
        assert!(token.parent_hash.is_none());
        assert!(!token.signature.is_empty());
    }

    #[test]
    fn test_child_token_creation() {
        let secret_key = [0u8; 32];
        let root = DelegationToken::new_root("agent_a", "agent_b", &secret_key);
        let child =
            DelegationToken::new_child(&root, "agent_b", "agent_c", &secret_key).unwrap();

        assert_eq!(child.delegating_agent, "agent_b");
        assert_eq!(child.target_agent, "agent_c");
        assert_eq!(child.depth, 1);
        assert!(child.parent_hash.is_some());
    }

    #[test]
    fn test_token_verification() {
        let secret_key = [0u8; 32];
        let token = DelegationToken::new_root("agent_a", "agent_b", &secret_key);

        assert!(token.verify(&secret_key));

        // Wrong key should fail
        let wrong_key = [1u8; 32];
        assert!(!token.verify(&wrong_key));
    }

    #[test]
    fn test_chain_verification() {
        let secret_key = [0u8; 32];
        let root = DelegationToken::new_root("agent_a", "agent_b", &secret_key);
        let child =
            DelegationToken::new_child(&root, "agent_b", "agent_c", &secret_key).unwrap();

        assert!(child.verify_chain(&root, &secret_key));

        // Tampered parent hash should fail
        let mut tampered = child.clone();
        tampered.parent_hash = Some("tampered".to_string());
        assert!(!tampered.verify_chain(&root, &secret_key));
    }

    #[test]
    fn test_depth_limit_enforcement() {
        let secret_key = [0u8; 32];
        let mut token = DelegationToken::new_root("agent_a", "agent_b", &secret_key);

        // Build chain to max depth
        for i in 0..MAX_DELEGATION_DEPTH {
            token = DelegationToken::new_child(
                &token,
                &format!("agent_{}", i),
                &format!("agent_{}", i + 1),
                &secret_key,
            )
            .unwrap();
        }

        assert_eq!(token.depth, MAX_DELEGATION_DEPTH);

        // Next delegation should fail
        let result = DelegationToken::new_child(
            &token,
            "agent_last",
            "agent_beyond",
            &secret_key,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_delegation_store() {
        let store = DelegationStore::with_secret_key([0u8; 32], 5);

        let root = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(store.create_root("agent_a", "agent_b"))
            .unwrap();

        assert_eq!(root.delegating_agent, "agent_a");

        // Verify the token can be retrieved
        let retrieved = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(store.verify(&root.delegation_id))
            .unwrap();

        assert_eq!(retrieved.delegation_id, root.delegation_id);
    }

    #[test]
    fn test_agent_limit_enforcement() {
        let store = DelegationStore::with_secret_key([0u8; 32], 2);
        let rt = tokio::runtime::Runtime::new().unwrap();

        // First two should succeed
        rt.block_on(store.create_root("agent_a", "agent_x"))
            .unwrap();
        rt.block_on(store.create_root("agent_a", "agent_y"))
            .unwrap();

        // Third should fail
        let result = rt.block_on(store.create_root("agent_a", "agent_z"));
        assert!(result.is_err());
    }

    #[test]
    fn test_token_expiration() {
        let secret_key = [0u8; 32];
        let mut token = DelegationToken::new_root("agent_a", "agent_b", &secret_key);

        // Set expiration to past
        token.expires_at = Utc::now() - chrono::Duration::seconds(1);

        assert!(token.is_expired());
        assert!(!token.verify(&secret_key));
    }
}
