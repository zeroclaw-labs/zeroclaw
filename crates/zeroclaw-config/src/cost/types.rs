use serde::{Deserialize, Serialize};

/// Token usage information from a single API call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Model identifier (e.g., "anthropic/claude-sonnet-4-20250514")
    pub model: String,
    /// Input/prompt tokens
    pub input_tokens: u64,
    /// Output/completion tokens
    pub output_tokens: u64,
    /// Cached input tokens (Anthropic `cache_read_input_tokens`, OpenAI
    /// `prompt_tokens_details.cached_tokens`). Subset of `input_tokens`
    /// when reported by the provider; the rate sheet's
    /// `cached_input_per_mtok` applies to these.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub cached_input_tokens: u64,
    /// Total tokens (input + output, ignoring the cached subset).
    pub total_tokens: u64,
    /// Calculated cost in USD
    pub cost_usd: f64,
    /// Timestamp of the request
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

fn is_zero_u64(v: &u64) -> bool {
    *v == 0
}

impl TokenUsage {
    fn sanitize_price(value: f64) -> f64 {
        if value.is_finite() && value > 0.0 {
            value
        } else {
            0.0
        }
    }

    /// Create a new token usage record. Cached input tokens are billed at
    /// `cached_input_price_per_million`; the rest of `input_tokens` at the
    /// standard `input_price_per_million`. When `cached_input_price` is 0
    /// the cached subset bills at the standard rate (no discount), so
    /// providers that don't surface a cached rate still produce a sane
    /// total.
    pub fn new(
        model: impl Into<String>,
        input_tokens: u64,
        output_tokens: u64,
        cached_input_tokens: u64,
        input_price_per_million: f64,
        output_price_per_million: f64,
        cached_input_price_per_million: f64,
    ) -> Self {
        let model = model.into();
        let input_price_per_million = Self::sanitize_price(input_price_per_million);
        let output_price_per_million = Self::sanitize_price(output_price_per_million);
        let cached_input_price_per_million =
            Self::sanitize_price(cached_input_price_per_million);
        let cached_input_tokens = cached_input_tokens.min(input_tokens);
        let billable_uncached_input = input_tokens.saturating_sub(cached_input_tokens);
        let total_tokens = input_tokens.saturating_add(output_tokens);

        // Calculate cost: (tokens / 1M) * price_per_million for each band.
        // Cached subset uses its own rate when set, else falls back to the
        // standard input rate so providers without a cache-rate aren't
        // charged $0 for the cached portion.
        let cached_rate = if cached_input_price_per_million > 0.0 {
            cached_input_price_per_million
        } else {
            input_price_per_million
        };
        let input_cost =
            (billable_uncached_input as f64 / 1_000_000.0) * input_price_per_million;
        let cached_cost = (cached_input_tokens as f64 / 1_000_000.0) * cached_rate;
        let output_cost = (output_tokens as f64 / 1_000_000.0) * output_price_per_million;
        let cost_usd = input_cost + cached_cost + output_cost;

        Self {
            model,
            input_tokens,
            output_tokens,
            cached_input_tokens,
            total_tokens,
            cost_usd,
            timestamp: chrono::Utc::now(),
        }
    }

    /// Get the total cost.
    pub fn cost(&self) -> f64 {
        self.cost_usd
    }
}

/// Time period for cost aggregation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UsagePeriod {
    Session,
    Day,
    Month,
}

macro_rules! emit_cost_range {
    ($(($variant:ident, $primary:literal $(, $alias:literal)*)),+ $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum CostRange {
            $($variant,)+
        }

        impl CostRange {
            pub fn from_wire(s: &str) -> Option<Self> {
                match s {
                    $($primary $(| $alias)* => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

#[macro_export]
macro_rules! for_each_cost_range_slot {
    ($mac:ident) => {
        $mac! {
            (Today, "today"),
            (Last7Days, "last_7_days", "7d"),
            (Last30Days, "last_30_days", "30d"),
            (CurrentMonth, "current_month", "month"),
            (AllTime, "all_time", "all"),
        }
    };
}

for_each_cost_range_slot!(emit_cost_range);

/// A single cost record for persistent storage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostRecord {
    /// Unique identifier
    pub id: String,
    /// Token usage details
    pub usage: TokenUsage,
    /// Session identifier (for grouping)
    pub session_id: String,
    /// Alias of the agent that incurred this cost (HashMap key in
    /// `config.agents`). `None` for records persisted before per-agent
    /// attribution, or when `[cost].track_per_agent = false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_alias: Option<String>,
}

impl CostRecord {
    /// Create a new cost record without agent attribution.
    pub fn new(session_id: impl Into<String>, usage: TokenUsage) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            usage,
            session_id: session_id.into(),
            agent_alias: None,
        }
    }

    /// Create a new cost record attributed to an agent.
    pub fn with_agent(
        session_id: impl Into<String>,
        agent_alias: Option<String>,
        usage: TokenUsage,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            usage,
            session_id: session_id.into(),
            agent_alias,
        }
    }
}

/// Budget enforcement result.
#[derive(Debug, Clone)]
pub enum BudgetCheck {
    /// Within budget, request can proceed
    Allowed,
    /// Warning threshold exceeded but request can proceed
    Warning {
        current_usd: f64,
        limit_usd: f64,
        period: UsagePeriod,
    },
    /// Budget exceeded, request blocked
    Exceeded {
        current_usd: f64,
        limit_usd: f64,
        period: UsagePeriod,
    },
}

/// Cost summary for reporting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostSummary {
    /// Total cost for the session
    pub session_cost_usd: f64,
    /// Total cost for the day
    pub daily_cost_usd: f64,
    /// Total cost for the month
    pub monthly_cost_usd: f64,
    /// Total tokens used
    pub total_tokens: u64,
    /// Number of requests
    pub request_count: usize,
    /// Breakdown by model
    pub by_model: std::collections::HashMap<String, ModelStats>,
    /// Breakdown by agent alias. Empty when `[cost].track_per_agent =
    /// false` or when no records carry an agent_alias.
    #[serde(default)]
    pub by_agent: std::collections::HashMap<String, AgentCostStats>,
}

/// Statistics for a specific agent alias.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCostStats {
    /// Agent alias (HashMap key in `config.agents`).
    pub agent_alias: String,
    /// Total cost attributed to this agent for the period.
    pub cost_usd: f64,
    /// Total tokens attributed to this agent for the period (input + output).
    pub total_tokens: u64,
    /// Input tokens (uncached + cached). Matches each record's
    /// `input_tokens` field.
    #[serde(default)]
    pub input_tokens: u64,
    /// Output tokens.
    #[serde(default)]
    pub output_tokens: u64,
    /// Cached input tokens (subset of `input_tokens` served from the
    /// provider's prompt cache).
    #[serde(default)]
    pub cached_input_tokens: u64,
    /// Number of LLM responses attributed to this agent for the period.
    pub request_count: usize,
}

/// Statistics for a specific model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelStats {
    /// Model name (upstream resource id from usage telemetry).
    pub model: String,
    /// Total cost for this model.
    pub cost_usd: f64,
    /// Total tokens for this model (input + output).
    pub total_tokens: u64,
    /// Input tokens (uncached + cached).
    #[serde(default)]
    pub input_tokens: u64,
    /// Output tokens.
    #[serde(default)]
    pub output_tokens: u64,
    /// Cached input tokens served from the prompt cache.
    #[serde(default)]
    pub cached_input_tokens: u64,
    /// Number of LLM responses for this model.
    pub request_count: usize,
}

impl Default for CostSummary {
    fn default() -> Self {
        Self {
            session_cost_usd: 0.0,
            daily_cost_usd: 0.0,
            monthly_cost_usd: 0.0,
            total_tokens: 0,
            request_count: 0,
            by_model: std::collections::HashMap::new(),
            by_agent: std::collections::HashMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_usage_calculation() {
        let usage = TokenUsage::new("test/model", 1000, 500, 0, 3.0, 15.0, 0.0);

        // Expected: (1000/1M)*3 + (500/1M)*15 = 0.003 + 0.0075 = 0.0105
        assert!((usage.cost_usd - 0.0105).abs() < 0.0001);
        assert_eq!(usage.input_tokens, 1000);
        assert_eq!(usage.output_tokens, 500);
        assert_eq!(usage.total_tokens, 1500);
        assert_eq!(usage.cached_input_tokens, 0);
    }

    #[test]
    fn token_usage_cached_input_billed_at_cached_rate() {
        // 200 cached input @ 0.5/Mtok + 800 uncached input @ 3/Mtok + 500 output @ 15/Mtok
        // = (200/1e6)*0.5 + (800/1e6)*3 + (500/1e6)*15
        // = 0.0001 + 0.0024 + 0.0075 = 0.01
        let usage = TokenUsage::new("test/model", 1000, 500, 200, 3.0, 15.0, 0.5);
        assert!((usage.cost_usd - 0.01).abs() < 1e-6, "{}", usage.cost_usd);
        assert_eq!(usage.cached_input_tokens, 200);
    }

    #[test]
    fn token_usage_zero_cached_rate_falls_back_to_input_rate() {
        // Cached rate 0 means "no discount" — bill cached subset at the
        // standard input rate so providers without a published cache rate
        // still produce a sane total.
        let with_cache = TokenUsage::new("test/model", 1000, 500, 200, 3.0, 15.0, 0.0);
        let without_cache = TokenUsage::new("test/model", 1000, 500, 0, 3.0, 15.0, 0.0);
        assert!((with_cache.cost_usd - without_cache.cost_usd).abs() < 1e-9);
    }

    #[test]
    fn token_usage_zero_tokens() {
        let usage = TokenUsage::new("test/model", 0, 0, 0, 3.0, 15.0, 0.0);
        assert!(usage.cost_usd.abs() < f64::EPSILON);
        assert_eq!(usage.total_tokens, 0);
    }

    #[test]
    fn token_usage_negative_or_non_finite_prices_are_clamped() {
        let usage = TokenUsage::new("test/model", 1000, 1000, 0, -3.0, f64::NAN, f64::INFINITY);
        assert!(usage.cost_usd.abs() < f64::EPSILON);
        assert_eq!(usage.total_tokens, 2000);
    }

    #[test]
    fn cost_record_creation() {
        let usage = TokenUsage::new("test/model", 100, 50, 0, 1.0, 2.0, 0.0);
        let record = CostRecord::new("session-123", usage);

        assert_eq!(record.session_id, "session-123");
        assert!(!record.id.is_empty());
        assert_eq!(record.usage.model, "test/model");
    }
}
