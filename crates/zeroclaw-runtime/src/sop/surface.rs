//! SOP operations shared by the gateway HTTP routes and the daemon RPC
//! methods, so both surfaces answer from one implementation rather than two
//! copies that drift.

use std::sync::{Arc, Mutex};

use serde::Serialize;
use zeroclaw_config::schema::{Config, SopDecisionProvider};

use super::audit::SopAuditLogger;
use super::dispatch::{DispatchResult, dispatch_untrusted_fan_in};
use super::engine::SopEngine;
use super::executor::SopDriverHandles;
use super::types::{SopRunAction, SopTriggerSource};

/// One `[decision_models.<alias>]` entry an SOP's `[decision] model` can
/// select. Never carries the API key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecisionModelOption {
    pub alias: String,
    pub provider: SopDecisionProvider,
    pub model: String,
    pub base_url: String,
}

/// The selectable decision models, sorted by alias. Entries with no known
/// endpoint (a `custom` provider without a base URL) are omitted because an
/// SOP could not call them.
pub fn decision_model_options(config: &Config) -> Vec<DecisionModelOption> {
    let mut models: Vec<DecisionModelOption> = config
        .decision_models
        .iter()
        .filter_map(|(alias, entry)| {
            let (base_url, model) = entry.endpoint()?;
            Some(DecisionModelOption {
                alias: alias.clone(),
                provider: entry.provider,
                model,
                base_url,
            })
        })
        .collect();
    models.sort_by(|a, b| a.alias.cmp(&b.alias));
    models
}

/// How a webhook-path event fared against the loaded SOPs.
#[derive(Debug, Clone, PartialEq)]
pub enum WebhookDispatch {
    /// The engine or its audit logger is not available.
    Unavailable,
    /// No loaded SOP has a webhook trigger for this path.
    NoMatch,
    /// At least one SOP matched. `blocked` is true when every match was
    /// refused as unsafe, which the HTTP route reports as 422.
    Dispatched {
        blocked: bool,
        results: Vec<serde_json::Value>,
    },
}

/// Fan a webhook event for `path` out to every SOP whose webhook trigger
/// matches it, and drive each run that starts on an agent step.
///
/// Deterministic steps are executed by dispatch itself and parked runs are
/// driven when approved, so only an `ExecuteStep` start needs a driver here.
/// With driver handles the driver is admitted into the daemon generation, so
/// a reload drains it; without them (a context with no generation) it is
/// detached. The caller authenticates the delivery and applies idempotency
/// before calling this.
pub async fn dispatch_webhook_event(
    engine: &Arc<Mutex<SopEngine>>,
    audit: &Arc<SopAuditLogger>,
    driver_handles: Option<&SopDriverHandles>,
    config: &Config,
    path: &str,
    payload: Option<&str>,
) -> WebhookDispatch {
    let results = dispatch_untrusted_fan_in(
        engine,
        audit,
        SopTriggerSource::Webhook,
        Some(path),
        payload,
        None,
    )
    .await;
    if results.is_empty() {
        return WebhookDispatch::Unavailable;
    }
    if results
        .iter()
        .all(|result| matches!(result, DispatchResult::NoMatch))
    {
        return WebhookDispatch::NoMatch;
    }

    for result in &results {
        if let DispatchResult::Started { action, .. } = result
            && matches!(action.as_ref(), SopRunAction::ExecuteStep { .. })
        {
            match driver_handles {
                Some(handles) => {
                    super::spawn_and_register_sop_driver(
                        handles,
                        config.clone(),
                        Arc::clone(engine),
                        Some(Arc::clone(audit)),
                        action.as_ref().clone(),
                    );
                }
                None => drop(super::spawn_headless_run_driver(
                    config.clone(),
                    Arc::clone(engine),
                    Some(Arc::clone(audit)),
                    action.as_ref().clone(),
                )),
            }
        }
    }

    let blocked = results.iter().all(|result| {
        matches!(
            result,
            DispatchResult::BlockedUnsafe { .. } | DispatchResult::NoMatch
        )
    });
    let results = results
        .into_iter()
        .filter_map(|result| match result {
            DispatchResult::Started {
                run_id, sop_name, ..
            } => Some(serde_json::json!({
                "status": "started",
                "sop": sop_name,
                "run_id": run_id,
            })),
            DispatchResult::Skipped { sop_name, reason } => Some(serde_json::json!({
                "status": "skipped",
                "sop": sop_name,
                "reason": reason,
            })),
            DispatchResult::Deferred { sop_name, reason } => Some(serde_json::json!({
                "status": "deferred",
                "sop": sop_name,
                "reason": reason,
            })),
            DispatchResult::Coalesced {
                sop_name,
                existing_run_id,
            } => Some(serde_json::json!({
                "status": "coalesced",
                "sop": sop_name,
                "run_id": existing_run_id,
            })),
            DispatchResult::BlockedUnsafe { sop_name, reason } => Some(serde_json::json!({
                "status": "blocked_unsafe",
                "sop": sop_name,
                "reason": reason,
            })),
            DispatchResult::NoMatch => None,
        })
        .collect();
    WebhookDispatch::Dispatched { blocked, results }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::schema::SopDecisionModelConfig;

    #[test]
    fn decision_models_are_sorted_and_carry_no_key() {
        let mut config = Config::default();
        for alias in ["zeta", "alpha"] {
            config.decision_models.insert(
                alias.to_string(),
                SopDecisionModelConfig {
                    provider: SopDecisionProvider::default(),
                    base_url: Some("http://127.0.0.1:9/v1".into()),
                    model: Some("m".into()),
                    ..SopDecisionModelConfig::default()
                },
            );
        }
        let options = decision_model_options(&config);
        let aliases: Vec<_> = options.iter().map(|o| o.alias.as_str()).collect();
        assert_eq!(aliases, ["alpha", "zeta"]);
        let wire = serde_json::to_value(&options[0]).unwrap();
        let mut keys: Vec<_> = wire.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["alias", "base_url", "model", "provider"]);
    }
}
