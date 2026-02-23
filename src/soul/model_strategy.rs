//! Tier-based model selection — maps survival tiers to provider+model combos.
//!
//! Consults configured `tier_models` to override the default model when the
//! agent's survival tier changes. Enforces per-session and per-call budgets.

use crate::config::ModelStrategyConfig;
use crate::soul::survival::SurvivalTier;

#[derive(Debug, Clone)]
pub struct TierModelOverride {
    pub provider: String,
    pub model: String,
}

pub struct ModelStrategy {
    tier_map: Vec<(SurvivalTier, TierModelOverride)>,
    per_session_budget_cents: Option<i64>,
    per_call_budget_cents: Option<i64>,
    session_spent_cents: i64,
}

impl ModelStrategy {
    pub fn from_config(config: &ModelStrategyConfig) -> Self {
        let tier_map = config
            .tier_models
            .iter()
            .filter_map(|tm| {
                let tier = parse_tier(&tm.tier)?;
                Some((
                    tier,
                    TierModelOverride {
                        provider: tm.provider.clone(),
                        model: tm.model.clone(),
                    },
                ))
            })
            .collect();

        Self {
            tier_map,
            per_session_budget_cents: config.per_session_budget_usd.map(usd_to_cents),
            per_call_budget_cents: config.per_call_budget_usd.map(usd_to_cents),
            session_spent_cents: 0,
        }
    }

    pub fn model_for_tier(&self, tier: SurvivalTier) -> Option<&TierModelOverride> {
        self.tier_map
            .iter()
            .find(|(t, _)| *t == tier)
            .map(|(_, m)| m)
    }

    pub fn record_spend(&mut self, cost_cents: i64) {
        self.session_spent_cents = self.session_spent_cents.saturating_add(cost_cents);
    }

    pub fn session_budget_exceeded(&self) -> bool {
        self.per_session_budget_cents
            .is_some_and(|cap| self.session_spent_cents >= cap)
    }

    pub fn call_budget_exceeded(&self, estimated_cost_cents: i64) -> bool {
        self.per_call_budget_cents
            .is_some_and(|cap| estimated_cost_cents > cap)
    }

    pub fn session_spent_cents(&self) -> i64 {
        self.session_spent_cents
    }
}

fn parse_tier(s: &str) -> Option<SurvivalTier> {
    match s.to_lowercase().as_str() {
        "dead" => Some(SurvivalTier::Dead),
        "critical" => Some(SurvivalTier::Critical),
        "low_compute" | "lowcompute" => Some(SurvivalTier::LowCompute),
        "normal" => Some(SurvivalTier::Normal),
        "high" => Some(SurvivalTier::High),
        _ => None,
    }
}

#[allow(clippy::cast_possible_truncation)]
fn usd_to_cents(usd: f64) -> i64 {
    (usd * 100.0) as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TierModelConfig;

    fn test_config() -> ModelStrategyConfig {
        ModelStrategyConfig {
            enabled: true,
            tier_models: vec![
                TierModelConfig {
                    tier: "high".into(),
                    provider: "openrouter".into(),
                    model: "anthropic/claude-opus-4".into(),
                },
                TierModelConfig {
                    tier: "normal".into(),
                    provider: "openrouter".into(),
                    model: "anthropic/claude-sonnet-4.6".into(),
                },
                TierModelConfig {
                    tier: "low_compute".into(),
                    provider: "groq".into(),
                    model: "llama-3.3-70b-versatile".into(),
                },
                TierModelConfig {
                    tier: "critical".into(),
                    provider: "groq".into(),
                    model: "llama-3.2-3b".into(),
                },
            ],
            per_session_budget_usd: Some(5.0),
            per_call_budget_usd: Some(0.50),
        }
    }

    #[test]
    fn model_for_tier_returns_correct_override() {
        let strategy = ModelStrategy::from_config(&test_config());

        let high = strategy.model_for_tier(SurvivalTier::High).unwrap();
        assert_eq!(high.model, "anthropic/claude-opus-4");

        let normal = strategy.model_for_tier(SurvivalTier::Normal).unwrap();
        assert_eq!(normal.model, "anthropic/claude-sonnet-4.6");

        let low = strategy.model_for_tier(SurvivalTier::LowCompute).unwrap();
        assert_eq!(low.model, "llama-3.3-70b-versatile");
    }

    #[test]
    fn model_for_dead_tier_returns_none() {
        let strategy = ModelStrategy::from_config(&test_config());
        assert!(strategy.model_for_tier(SurvivalTier::Dead).is_none());
    }

    #[test]
    fn session_budget_tracking() {
        let mut strategy = ModelStrategy::from_config(&test_config());
        assert!(!strategy.session_budget_exceeded());

        strategy.record_spend(400);
        assert!(!strategy.session_budget_exceeded());

        strategy.record_spend(100);
        assert!(strategy.session_budget_exceeded());
    }

    #[test]
    fn call_budget_enforcement() {
        let strategy = ModelStrategy::from_config(&test_config());
        assert!(!strategy.call_budget_exceeded(49));
        assert!(strategy.call_budget_exceeded(51));
    }

    #[test]
    fn empty_config_returns_no_overrides() {
        let strategy = ModelStrategy::from_config(&ModelStrategyConfig::default());
        assert!(strategy.model_for_tier(SurvivalTier::High).is_none());
        assert!(!strategy.session_budget_exceeded());
        assert!(!strategy.call_budget_exceeded(1000));
    }

    #[test]
    fn invalid_tier_name_is_skipped() {
        let config = ModelStrategyConfig {
            enabled: true,
            tier_models: vec![TierModelConfig {
                tier: "invalid_tier".into(),
                provider: "test".into(),
                model: "test".into(),
            }],
            per_session_budget_usd: None,
            per_call_budget_usd: None,
        };
        let strategy = ModelStrategy::from_config(&config);
        assert!(strategy.model_for_tier(SurvivalTier::High).is_none());
    }

    #[test]
    fn usd_to_cents_conversion() {
        assert_eq!(usd_to_cents(1.0), 100);
        assert_eq!(usd_to_cents(0.50), 50);
        assert_eq!(usd_to_cents(5.0), 500);
    }
}
