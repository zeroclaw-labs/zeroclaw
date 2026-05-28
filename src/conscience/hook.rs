//! HookHandler glue that lets the agent runtime invoke the conscience
//! gate before every tool call without taking a direct dependency on
//! the X0 fork's conscience module.
//!
//! The runtime crate exposes a process-global factory registry
//! (`zeroclaw_runtime::hooks::registry`). At startup the binary
//! registers a closure that, given the current `Config`, returns a
//! `ConscienceHook` when `[conscience].gate_enabled = true`. Every
//! Agent constructed afterwards picks it up alongside the built-in
//! command-logger and webhook-audit hooks.
//!
//! Wiring path:
//!     main.rs → conscience::hook::register_hook_factory()
//!         → zeroclaw_runtime::hooks::registry::register_factory(...)
//!     ↓ later, per Agent build:
//!     Agent::run → HookRunner.register(ConscienceHook { … })
//!     ↓ per tool call:
//!     before_tool_call → evaluate_tool_call(…) → HookResult::{Continue|Cancel}
//!
//! Verdict mapping (Block / Ask / Revise all cancel; Allow continues):
//!     - Block  → HookResult::Cancel("conscience: blocked (score=…)")
//!     - Ask    → HookResult::Cancel("conscience: ask (score=…)")
//!     - Revise → HookResult::Cancel("conscience: revise (score=…)")
//!     - Allow  → HookResult::Continue((name, args))
//!
//! Cancel is the only existing surface to stop a tool call from the
//! pre-hook chain; for Ask/Revise the cancellation reason carries the
//! verdict label so the calling model sees why. A richer protocol
//! (e.g. surface Ask back to the user) is future work.

use async_trait::async_trait;
use serde_json::Value;
use zeroclaw_config::schema::Config;
use zeroclaw_config::x0_extensions::{ConscienceConfig, NormConfigSerde};
use zeroclaw_runtime::hooks::{HookHandler, HookResult};

use super::gate::evaluate_tool_call;
use super::types::{GateVerdict, Norm, NormAction, NormConfig, SelfState, Thresholds};

/// Per-Agent hook that wraps the conscience module's gate evaluation.
///
/// Cloned out of the loaded `ConscienceConfig` at Agent build time so the
/// hot path doesn't lock against config-reload writers.
pub struct ConscienceHook {
    thresholds: Thresholds,
    norms: Vec<Norm>,
}

impl ConscienceHook {
    pub fn new(cfg: &ConscienceConfig) -> Self {
        Self {
            thresholds: Thresholds {
                allow_above: cfg.allow_threshold,
                ask_above: cfg.ask_threshold,
                block_below: cfg.block_threshold,
            },
            norms: cfg
                .default_norms
                .iter()
                .map(|s| convert_norm(s).into_runtime_norm())
                .collect(),
        }
    }
}

#[async_trait]
impl HookHandler for ConscienceHook {
    fn name(&self) -> &str {
        "conscience-gate"
    }

    fn priority(&self) -> i32 {
        // Run after the command-logger (default 0) and webhook-audit
        // (default 0) so the logger captures the *intent* even when the
        // gate blocks. A higher priority value runs later in the chain
        // per `HookRunner`'s sort.
        10
    }

    async fn before_tool_call(&self, name: String, args: Value) -> HookResult<(String, Value)> {
        // Self-state defaults are intentionally "healthy" — IntegrityLedger
        // wiring (Plan v2 Slice A step 8) is a follow-up.
        let self_state = SelfState {
            integrity_score: 1.0,
            recent_violations: 0,
            active_repairs: 0,
            arousal: None,
            confidence: None,
            risk_level: None,
            free_energy: None,
        };

        let (verdict, score) = evaluate_tool_call(
            &name,
            &self.thresholds,
            &self_state,
            &self.norms,
            /* llm_risk_override */ None,
            /* tool_affinity */ None,
        );

        match verdict {
            GateVerdict::Allow => HookResult::Continue((name, args)),
            GateVerdict::Block => HookResult::Cancel(format!(
                "conscience: blocked (tool={name}, score={score:.2})"
            )),
            GateVerdict::Ask => HookResult::Cancel(format!(
                "conscience: ask (tool={name}, score={score:.2}) — operator review required"
            )),
            GateVerdict::Revise => HookResult::Cancel(format!(
                "conscience: revise (tool={name}, score={score:.2}) — adjust arguments and retry"
            )),
        }
    }
}

/// Translate the serde-friendly mirror that lives in `zeroclaw-config`
/// into the binary-local `NormConfig` shape, then into a runtime `Norm`.
/// The intermediate hop is necessary because `zeroclaw-config` cannot
/// depend on this binary-local module.
fn convert_norm(serde_norm: &NormConfigSerde) -> NormConfig {
    NormConfig {
        name: serde_norm.name.clone(),
        action: match serde_norm.action {
            zeroclaw_config::x0_extensions::NormActionSerde::Allow => NormAction::Prefer,
            zeroclaw_config::x0_extensions::NormActionSerde::Forbid => NormAction::Forbid,
            zeroclaw_config::x0_extensions::NormActionSerde::Require => NormAction::Require,
        },
        condition: serde_norm.condition.clone(),
        severity: serde_norm.severity,
    }
}

impl NormConfig {
    /// Shim that funnels the serialised + decoded `NormConfig` into the
    /// runtime `Norm` the gate evaluates against.
    fn into_runtime_norm(self) -> Norm {
        Norm {
            name: self.name,
            action: self.action,
            condition: self.condition,
            severity: self.severity,
        }
    }
}

/// Install the conscience hook factory on the runtime's global registry.
///
/// Called once from the binary's startup (gated on `x0-extended`). The
/// factory inspects `[conscience].gate_enabled` at Agent-build time, so
/// flipping the config flag at reload picks up the new value the next
/// time any Agent spawns.
pub fn register_hook_factory() {
    zeroclaw_runtime::hooks::registry::register_factory(Box::new(|cfg: &Config| {
        if cfg.conscience.gate_enabled {
            vec![Box::new(ConscienceHook::new(&cfg.conscience))]
        } else {
            Vec::new()
        }
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn allow_path_passes_args_through() {
        let cfg = ConscienceConfig::default();
        let hook = ConscienceHook::new(&cfg);
        // file_read is a low-harm tool name; with default thresholds the
        // gate should let it through.
        let result = hook
            .before_tool_call("file_read".into(), Value::Null)
            .await;
        assert!(
            matches!(result, HookResult::Continue(_)),
            "low-harm tool must Allow, got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn dangerous_shell_call_is_cancelled() {
        let cfg = ConscienceConfig::default();
        let hook = ConscienceHook::new(&cfg);
        // shell calls map to (harm=0.6, reversibility=0.3); with default
        // thresholds the gate should produce something other than Allow.
        let result = hook
            .before_tool_call("shell".into(), Value::Null)
            .await;
        match result {
            HookResult::Cancel(reason) => {
                assert!(reason.starts_with("conscience:"), "reason: {reason}");
            }
            HookResult::Continue(_) => panic!("shell must NOT pass the gate at defaults"),
        }
    }
}
