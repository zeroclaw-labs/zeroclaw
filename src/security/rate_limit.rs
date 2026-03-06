//! Rate limiting for agent operations.
//!
//! This module provides token-bucket rate limiting for:
//! - Delegation operations (to prevent resource exhaustion)
//! - Agent registry operations (to prevent abuse)
//! - Per-agent and global rate limits
//!
//! ## Usage
//!
//! ```rust
//! use crate::security::rate_limit::{AgentRateLimiter, RateLimitConfig};
//!
//! let config = RateLimitConfig::default();
//! let limiter = AgentRateLimiter::new(config);
//!
//! // Check if operation is allowed
//! match limiter.check("agent_a", "delegate", 1).await {
//!     Ok(permit) => {
//!         // Execute operation
//!         limiter.record("agent_a", "delegate", 1, true).await;
//!     },
//!     Err(retry_after) => {
//!         // Rate limited, retry after specified duration
//!     },
//! }
//! ```

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

/// Default rate limit configuration
pub const DEFAULT_RATE_LIMIT: u32 = 100;
pub const DEFAULT_BURST: u32 = 10;
pub const DEFAULT_AGENT_LIMIT: u32 = 20;

/// Rate limit error with retry information
#[derive(Debug, Clone, PartialEq)]
pub struct RateLimitError {
    /// Estimated time until next token is available
    pub retry_after: Duration,
    /// Current token count
    pub current_tokens: f64,
    /// Token capacity
    pub capacity: u32,
}

impl std::fmt::Display for RateLimitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Rate limited. Retry after {}ms. Current: {}/{}",
            self.retry_after.as_millis(),
            self.current_tokens as u32,
            self.capacity
        )
    }
}

impl std::error::Error for RateLimitError {}

/// Rate limit configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitConfig {
    /// Global rate limit (operations per second)
    pub global_rate: f64,
    /// Global burst capacity
    pub global_burst: u32,
    /// Per-agent rate limit
    pub per_agent_rate: f64,
    /// Per-agent burst capacity
    pub per_agent_burst: u32,
    /// Cost of each operation type
    pub operation_costs: HashMap<String, u32>,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        let mut operation_costs = HashMap::new();
        operation_costs.insert("delegate".to_string(), 1);
        operation_costs.insert("delegate_agentic".to_string(), 5);
        operation_costs.insert("agent_register".to_string(), 10);
        operation_costs.insert("agent_unregister".to_string(), 5);
        operation_costs.insert("team_create".to_string(), 10);
        operation_costs.insert("team_modify".to_string(), 5);

        Self {
            global_rate: DEFAULT_RATE_LIMIT as f64,
            global_burst: DEFAULT_BURST,
            per_agent_rate: DEFAULT_AGENT_LIMIT as f64,
            per_agent_burst: DEFAULT_AGENT_LIMIT,
            operation_costs,
        }
    }
}

impl RateLimitConfig {
    /// Get the cost for an operation type
    pub fn operation_cost(&self, operation: &str) -> u32 {
        self.operation_costs
            .get(operation)
            .copied()
            .unwrap_or(1)
    }

    /// Set the cost for an operation type
    pub fn set_operation_cost(&mut self, operation: &str, cost: u32) {
        self.operation_costs.insert(operation.to_string(), cost);
    }
}

/// Token bucket for rate limiting
#[derive(Debug, Clone)]
struct TokenBucket {
    /// Current token count
    tokens: f64,
    /// Maximum token capacity
    capacity: u32,
    /// Token refill rate (tokens per second)
    rate: f64,
    /// Last refill timestamp
    last_refill: DateTime<Utc>,
}

impl TokenBucket {
    /// Create a new token bucket
    fn new(capacity: u32, rate: f64) -> Self {
        Self {
            tokens: capacity as f64,
            capacity,
            rate,
            last_refill: Utc::now(),
        }
    }

    /// Attempt to consume tokens
    ///
    /// Returns Ok(remaining) if successful, Err(retry_after) if rate limited
    fn try_consume(&mut self, cost: u32) -> Result<f64, RateLimitError> {
        self.refill();

        if self.tokens >= cost as f64 {
            self.tokens -= cost as f64;
            Ok(self.tokens)
        } else {
            // Calculate retry time
            let deficit = cost as f64 - self.tokens;
            let retry_after_secs = deficit / self.rate;
            let retry_after = Duration::from_millis((retry_after_secs * 1000.0).ceil() as u64);

            Err(RateLimitError {
                retry_after,
                current_tokens: self.tokens,
                capacity: self.capacity,
            })
        }
    }

    /// Refund tokens (for failed operations)
    fn refund(&mut self, cost: u32) {
        self.tokens = (self.tokens + cost as f64).min(self.capacity as f64);
    }

    /// Refill tokens based on elapsed time
    fn refill(&mut self) {
        let now = Utc::now();
        let elapsed = (now - self.last_refill).num_seconds() as f64;
        self.last_refill = now;

        let new_tokens = elapsed * self.rate;
        self.tokens = (self.tokens + new_tokens).min(self.capacity as f64);
    }

    /// Get current token count
    fn current(&self) -> f64 {
        self.tokens
    }
}

/// Rate limiter for agent operations
pub struct AgentRateLimiter {
    /// Configuration
    config: Arc<RateLimitConfig>,
    /// Global token bucket
    global: RwLock<TokenBucket>,
    /// Per-agent token buckets
    agents: RwLock<HashMap<String, TokenBucket>>,
    /// Per-operation statistics
    stats: RwLock<HashMap<String, OperationStats>>,
}

/// Statistics for an operation type
#[derive(Debug, Clone, Default)]
pub struct OperationStats {
    pub total: u64,
    pub allowed: u64,
    pub denied: u64,
    pub last_denied: Option<DateTime<Utc>>,
}

impl AgentRateLimiter {
    /// Create a new rate limiter
    pub fn new(config: RateLimitConfig) -> Self {
        let global = TokenBucket::new(config.global_burst, config.global_rate);

        Self {
            config: Arc::new(config),
            global: RwLock::new(global),
            agents: RwLock::new(HashMap::new()),
            stats: RwLock::new(HashMap::new()),
        }
    }

    /// Create with default configuration
    pub fn default() -> Self {
        Self::new(RateLimitConfig::default())
    }

    /// Check if an operation is allowed
    ///
    /// Returns Ok(permit) if allowed, Err(retry_after) if rate limited
    pub async fn check(
        &self,
        agent: &str,
        operation: &str,
    ) -> Result<(), RateLimitError> {
        let cost = self.config.operation_cost(operation);

        // Check global limit first
        {
            let mut global = self.global.write().await;
            global.try_consume(cost)?;
        }

        // Check per-agent limit
        {
            let mut agents = self.agents.write().await;
            let bucket = agents.entry(agent.to_string()).or_insert_with(|| {
                TokenBucket::new(self.config.per_agent_burst, self.config.per_agent_rate)
            });

            // Clone bucket to avoid holding write lock during error
            let result = bucket.try_consume(cost);
            if result.is_err() {
                // Refund global tokens
                self.global.write().await.refund(cost);
            }
            result?;
        }

        Ok(())
    }

    /// Record a completed operation (for statistics and potential refunds)
    pub async fn record(&self, _agent: &str, operation: &str, success: bool) {
        let mut stats = self.stats.write().await;
        let entry = stats.entry(operation.to_string()).or_default();
        entry.total += 1;

        if success {
            entry.allowed += 1;
        } else {
            entry.denied += 1;
            entry.last_denied = Some(Utc::now());
        }
    }

    /// Get statistics for an operation type
    pub async fn stats(&self, operation: &str) -> OperationStats {
        let stats = self.stats.read().await;
        stats.get(operation).cloned().unwrap_or_default()
    }

    /// Get all statistics
    pub async fn all_stats(&self) -> HashMap<String, OperationStats> {
        let stats = self.stats.read().await;
        stats.clone()
    }

    /// Get current token counts
    pub async fn token_counts(&self, agent: &str) -> TokenCounts {
        let global = self.global.read().await;
        let agents = self.agents.read().await;

        TokenCounts {
            global: global.current(),
            agent: agents.get(agent).map(|b| b.current()).unwrap_or(0.0),
            global_capacity: self.config.global_burst,
            agent_capacity: self.config.per_agent_burst,
        }
    }

    /// Reset rate limits for an agent
    pub async fn reset_agent(&self, agent: &str) {
        let mut agents = self.agents.write().await;
        agents.remove(agent);
    }

    /// Reset all rate limits
    pub async fn reset_all(&self) {
        let mut global = self.global.write().await;
        *global = TokenBucket::new(self.config.global_burst, self.config.global_rate);

        let mut agents = self.agents.write().await;
        agents.clear();
    }

    /// Prune inactive agent buckets
    pub async fn prune_inactive(&self, idle_secs: i64) -> usize {
        let mut agents = self.agents.write().await;
        let threshold = Utc::now() - chrono::Duration::seconds(idle_secs);

        let initial_count = agents.len();
        agents.retain(|_, bucket| bucket.last_refill > threshold);
        initial_count - agents.len()
    }
}

/// Current token counts
#[derive(Debug, Clone)]
pub struct TokenCounts {
    pub global: f64,
    pub agent: f64,
    pub global_capacity: u32,
    pub agent_capacity: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration as StdDuration;

    #[test]
    fn test_token_bucket_refill() {
        let mut bucket = TokenBucket::new(10, 10.0); // 10 tokens, refills at 10/sec

        // Consume all tokens
        assert!(bucket.try_consume(10).is_ok());

        // Should be empty
        assert!(bucket.try_consume(1).is_err());

        // Wait for refill (0.2 seconds = 2 tokens)
        thread::sleep(StdDuration::from_millis(200));
        bucket.refill();

        // Now we should have ~2 tokens
        assert!(bucket.current() >= 1.5);
        assert!(bucket.current() <= 2.5);
    }

    #[test]
    fn test_token_bucket_burst() {
        let mut bucket = TokenBucket::new(10, 1.0); // 10 burst, 1 token/sec

        // Can burst up to capacity
        assert!(bucket.try_consume(10).is_ok());

        // Then need to wait
        assert!(bucket.try_consume(1).is_err());

        // Wait 1 second for 1 token
        thread::sleep(StdDuration::from_millis(1100));
        bucket.refill();

        // Should have 1 token
        assert!(bucket.try_consume(1).is_ok());
    }

    #[test]
    fn test_rate_limiter_global_limit() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut config = RateLimitConfig::default();
        config.global_rate = 10.0;
        config.global_burst = 10;
        config.per_agent_rate = 100.0;
        config.per_agent_burst = 100;

        let limiter = AgentRateLimiter::new(config);

        // First 10 should succeed
        for _ in 0..10 {
            rt.block_on(limiter.check("agent_a", "delegate"))
                .unwrap();
        }

        // 11th should fail (global limit)
        let result = rt.block_on(limiter.check("agent_a", "delegate"));
        assert!(result.is_err());
    }

    #[test]
    fn test_rate_limiter_per_agent_limit() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut config = RateLimitConfig::default();
        config.global_rate = 1000.0;
        config.global_burst = 1000;
        config.per_agent_rate = 5.0;
        config.per_agent_burst = 5;

        let limiter = AgentRateLimiter::new(config);

        // First 5 for agent_a should succeed
        for _ in 0..5 {
            rt.block_on(limiter.check("agent_a", "delegate"))
                .unwrap();
        }

        // 6th should fail (per-agent limit)
        let result = rt.block_on(limiter.check("agent_a", "delegate"));
        assert!(result.is_err());

        // But agent_b should still work
        rt.block_on(limiter.check("agent_b", "delegate"))
            .unwrap();
    }

    #[test]
    fn test_rate_limiter_different_costs() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut config = RateLimitConfig::default();
        config.global_rate = 10.0;
        config.global_burst = 10;
        config.set_operation_cost("expensive", 5);

        let limiter = AgentRateLimiter::new(config);

        // 2 cheap operations
        rt.block_on(limiter.check("agent_a", "delegate"))
            .unwrap();
        rt.block_on(limiter.check("agent_a", "delegate"))
            .unwrap();

        // 1 expensive should fail (2 + 5 = 7, but next expensive = 5 more = 12 > 10)
        rt.block_on(limiter.check("agent_a", "expensive"))
            .unwrap();

        // Second expensive should fail
        let result = rt.block_on(limiter.check("agent_a", "expensive"));
        assert!(result.is_err());
    }

    #[test]
    fn test_rate_limiter_stats() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let limiter = AgentRateLimiter::default();

        // Record some operations
        rt.block_on(limiter.check("agent_a", "delegate"))
            .unwrap();
        rt.block_on(limiter.record("agent_a", "delegate", true));

        rt.block_on(limiter.check("agent_a", "delegate"))
            .unwrap();
        rt.block_on(limiter.record("agent_a", "delegate", false));

        let stats = rt.block_on(limiter.stats("delegate"));
        assert_eq!(stats.total, 2);
        assert_eq!(stats.allowed, 1);
        assert_eq!(stats.denied, 1);
    }

    #[test]
    fn test_rate_limiter_reset_agent() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut config = RateLimitConfig::default();
        config.per_agent_rate = 5.0;
        config.per_agent_burst = 5;

        let limiter = AgentRateLimiter::new(config);

        // Exhaust agent quota
        for _ in 0..5 {
            rt.block_on(limiter.check("agent_a", "delegate"))
                .unwrap();
        }
        assert!(rt.block_on(limiter.check("agent_a", "delegate")).is_err());

        // Reset agent
        rt.block_on(limiter.reset_agent("agent_a"));

        // Should work again
        rt.block_on(limiter.check("agent_a", "delegate"))
            .unwrap();
    }

    #[test]
    fn test_rate_limiter_token_counts() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let limiter = AgentRateLimiter::default();

        // Consume some tokens
        rt.block_on(limiter.check("agent_a", "delegate"))
            .unwrap();
        rt.block_on(limiter.check("agent_a", "delegate"))
            .unwrap();

        let counts = rt.block_on(limiter.token_counts("agent_a"));
        assert!(counts.global < counts.global_capacity as f64);
        assert!(counts.agent < counts.agent_capacity as f64);
    }
}
