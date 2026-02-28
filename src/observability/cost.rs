//! Cost-tracking observer that wires provider token usage to the cost tracker.
//!
//! Intercepts `LlmResponse` events and records usage to the `CostTracker`,
//! calculating costs based on model pricing configuration.

use super::traits::{Observer, ObserverEvent, ObserverMetric};
use crate::config::schema::ModelPricing;
use crate::cost::{CostTracker, TokenUsage};
use std::collections::HashMap;
use std::sync::Arc;

/// Observer that records token usage to a CostTracker.
///
/// Listens for `LlmResponse` events and calculates costs using model pricing.
pub struct CostObserver {
    tracker: Arc<CostTracker>,
    prices: HashMap<String, ModelPricing>,
    /// Default pricing for unknown models (USD per 1M tokens)
    default_input_price: f64,
    default_output_price: f64,
}

impl CostObserver {
    /// Create a new cost observer with the given tracker and pricing config.
    pub fn new(tracker: Arc<CostTracker>, prices: HashMap<String, ModelPricing>) -> Self {
        Self {
            tracker,
            prices,
            // Conservative defaults for unknown models
            default_input_price: 3.0,
            default_output_price: 15.0,
        }
    }

    /// Look up pricing for a model, trying various name formats.
    fn get_pricing(&self, provider: &str, model: &str) -> (f64, f64) {
        // Try exact match first: "provider/model"
        let full_name = format!("{provider}/{model}");
        if let Some(pricing) = self.prices.get(&full_name) {
            return (pricing.input, pricing.output);
        }

        // Try just the model name
        if let Some(pricing) = self.prices.get(model) {
            return (pricing.input, pricing.output);
        }

        // Try model family matching (e.g., "claude-sonnet-4" matches any claude-sonnet-4-*)
        for (key, pricing) in &self.prices {
            // Strip provider prefix if present
            let key_model = key.split('/').last().unwrap_or(key);

            // Check if model starts with the key (family match)
            if model.starts_with(key_model) || key_model.starts_with(model) {
                return (pricing.input, pricing.output);
            }

            // Check for common model name patterns
            // e.g., "claude-3-5-sonnet-20241022" should match "claude-3.5-sonnet"
            let normalized_model = model.replace('-', ".");
            let normalized_key = key_model.replace('-', ".");
            if normalized_model.contains(&normalized_key)
                || normalized_key.contains(&normalized_model)
            {
                return (pricing.input, pricing.output);
            }
        }

        // Fall back to defaults
        tracing::debug!(
            "No pricing found for {}/{}, using defaults (${}/{} per 1M tokens)",
            provider,
            model,
            self.default_input_price,
            self.default_output_price
        );
        (self.default_input_price, self.default_output_price)
    }
}

impl Observer for CostObserver {
    fn record_event(&self, event: &ObserverEvent) {
        if let ObserverEvent::LlmResponse {
            provider,
            model,
            success: true,
            input_tokens,
            output_tokens,
            ..
        } = event
        {
            // Only record if we have token counts
            let input = input_tokens.unwrap_or(0);
            let output = output_tokens.unwrap_or(0);

            if input == 0 && output == 0 {
                return;
            }

            let (input_price, output_price) = self.get_pricing(provider, model);
            let full_model_name = format!("{provider}/{model}");

            let usage = TokenUsage::new(full_model_name, input, output, input_price, output_price);

            if let Err(e) = self.tracker.record_usage(usage) {
                tracing::warn!("Failed to record cost usage: {e}");
            }
        }
    }

    fn record_metric(&self, _metric: &ObserverMetric) {
        // Cost observer doesn't handle metrics
    }

    fn name(&self) -> &str {
        "cost"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::CostConfig;
    use std::time::Duration;
    use tempfile::TempDir;

    fn create_test_tracker() -> (TempDir, Arc<CostTracker>) {
        let tmp = TempDir::new().unwrap();
        let config = CostConfig {
            enabled: true,
            ..Default::default()
        };
        let tracker = Arc::new(CostTracker::new(config, tmp.path()).unwrap());
        (tmp, tracker)
    }

    #[test]
    fn cost_observer_records_llm_response_usage() {
        let (_tmp, tracker) = create_test_tracker();
        let mut prices = HashMap::new();
        prices.insert(
            "anthropic/claude-sonnet-4-20250514".into(),
            ModelPricing {
                input: 3.0,
                output: 15.0,
            },
        );

        let observer = CostObserver::new(tracker.clone(), prices);

        observer.record_event(&ObserverEvent::LlmResponse {
            provider: "anthropic".into(),
            model: "claude-sonnet-4-20250514".into(),
            duration: Duration::from_millis(100),
            success: true,
            error_message: None,
            input_tokens: Some(1000),
            output_tokens: Some(500),
        });

        let summary = tracker.get_summary().unwrap();
        assert_eq!(summary.request_count, 1);
        // Cost: (1000/1M)*3 + (500/1M)*15 = 0.003 + 0.0075 = 0.0105
        assert!((summary.session_cost_usd - 0.0105).abs() < 0.0001);
    }

    #[test]
    fn cost_observer_ignores_failed_responses() {
        let (_tmp, tracker) = create_test_tracker();
        let observer = CostObserver::new(tracker.clone(), HashMap::new());

        observer.record_event(&ObserverEvent::LlmResponse {
            provider: "anthropic".into(),
            model: "claude-sonnet-4".into(),
            duration: Duration::from_millis(100),
            success: false,
            error_message: Some("API error".into()),
            input_tokens: Some(1000),
            output_tokens: Some(500),
        });

        let summary = tracker.get_summary().unwrap();
        assert_eq!(summary.request_count, 0);
    }

    #[test]
    fn cost_observer_ignores_zero_token_responses() {
        let (_tmp, tracker) = create_test_tracker();
        let observer = CostObserver::new(tracker.clone(), HashMap::new());

        observer.record_event(&ObserverEvent::LlmResponse {
            provider: "anthropic".into(),
            model: "claude-sonnet-4".into(),
            duration: Duration::from_millis(100),
            success: true,
            error_message: None,
            input_tokens: None,
            output_tokens: None,
        });

        let summary = tracker.get_summary().unwrap();
        assert_eq!(summary.request_count, 0);
    }

    #[test]
    fn cost_observer_uses_default_pricing_for_unknown_models() {
        let (_tmp, tracker) = create_test_tracker();
        let observer = CostObserver::new(tracker.clone(), HashMap::new());

        observer.record_event(&ObserverEvent::LlmResponse {
            provider: "unknown".into(),
            model: "mystery-model".into(),
            duration: Duration::from_millis(100),
            success: true,
            error_message: None,
            input_tokens: Some(1_000_000), // 1M tokens
            output_tokens: Some(1_000_000),
        });

        let summary = tracker.get_summary().unwrap();
        assert_eq!(summary.request_count, 1);
        // Default: $3 input + $15 output = $18 for 1M each
        assert!((summary.session_cost_usd - 18.0).abs() < 0.01);
    }

    #[test]
    fn cost_observer_matches_model_family() {
        let (_tmp, tracker) = create_test_tracker();
        let mut prices = HashMap::new();
        prices.insert(
            "openai/gpt-4o".into(),
            ModelPricing {
                input: 5.0,
                output: 15.0,
            },
        );

        let observer = CostObserver::new(tracker.clone(), prices);

        // Model name with version suffix should still match
        observer.record_event(&ObserverEvent::LlmResponse {
            provider: "openai".into(),
            model: "gpt-4o-2024-05-13".into(),
            duration: Duration::from_millis(100),
            success: true,
            error_message: None,
            input_tokens: Some(1_000_000),
            output_tokens: Some(0),
        });

        let summary = tracker.get_summary().unwrap();
        // Should use $5 input price, not default $3
        assert!((summary.session_cost_usd - 5.0).abs() < 0.01);
    }
}
