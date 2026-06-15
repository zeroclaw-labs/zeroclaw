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
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use zeroclaw_config::schema::Config;
use zeroclaw_config::x0_extensions::{ConscienceConfig, NormConfigSerde};
use zeroclaw_runtime::hooks::{HookHandler, HookResult};

use zeroclaw_conscience::gate::evaluate_tool_call;
use zeroclaw_conscience::ledger::IntegrityLedger;
use zeroclaw_conscience::types::{GateVerdict, Norm, NormAction, Thresholds};

/// Filename of the persisted integrity ledger under `<data_dir>/conscience/`.
const LEDGER_FILENAME: &str = "ledger.json";

/// Per-Agent hook that wraps the conscience module's gate evaluation.
///
/// Cloned out of the loaded `ConscienceConfig` at Agent build time so the
/// hot path doesn't lock against config-reload writers. Carries an
/// `Arc<Mutex<IntegrityLedger>>` so the `SelfState` driving the gate
/// reflects accumulated violation history — without it, every call sees
/// `integrity_score: 1.0, recent_violations: 0`, which makes the
/// threshold-aware scoring degenerate to "purely tool-dependent".
///
/// A fresh ledger is constructed per hook instance (i.e. per Agent
/// build); restoring the ledger across process restarts is future work
/// tracked alongside the rest of Plan v2 Slice A finish.
pub struct ConscienceHook {
    thresholds: Thresholds,
    norms: Vec<Norm>,
    ledger: Arc<Mutex<IntegrityLedger>>,
    /// File path the ledger autosaves to after each recorded violation.
    /// `None` for in-memory-only operation (tests and the convenience
    /// `ConscienceHook::new` constructor).
    ledger_path: Option<PathBuf>,
}

impl ConscienceHook {
    /// In-memory hook with no disk persistence. Convenient for tests
    /// and one-off usage; production code in `register_hook_factory`
    /// goes through [`Self::with_persistence`] instead.
    pub fn new(cfg: &ConscienceConfig) -> Self {
        Self::build(cfg, None, IntegrityLedger::new())
    }

    /// Hook that loads its starting ledger from `<data_dir>/conscience/ledger.json`
    /// and autosaves back to it after every recorded violation. A missing
    /// or corrupt file falls back to a fresh healthy ledger so a first
    /// boot — or a deliberate ledger wipe — doesn't make the gate refuse
    /// to load.
    pub fn with_persistence(cfg: &ConscienceConfig, data_dir: &std::path::Path) -> Self {
        let path = data_dir.join("conscience").join(LEDGER_FILENAME);
        let ledger = IntegrityLedger::load_or_default(&path);
        Self::build(cfg, Some(path), ledger)
    }

    fn build(
        cfg: &ConscienceConfig,
        ledger_path: Option<PathBuf>,
        ledger: IntegrityLedger,
    ) -> Self {
        Self {
            thresholds: Thresholds {
                allow_above: cfg.allow_threshold,
                ask_above: cfg.ask_threshold,
                block_below: cfg.block_threshold,
            },
            norms: cfg.default_norms.iter().map(convert_norm).collect(),
            ledger: Arc::new(Mutex::new(ledger)),
            ledger_path,
        }
    }

    /// Borrow the ledger for diagnostics or testing. Holding the lock
    /// stalls the gate, so callers should drop the guard quickly.
    #[cfg(test)]
    pub(super) fn ledger(&self) -> Arc<Mutex<IntegrityLedger>> {
        Arc::clone(&self.ledger)
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
        // Read the live SelfState from the ledger so accumulated
        // violations drag the integrity score down across turns. The
        // lock is held only for the to_self_state() snapshot, not
        // through evaluate_tool_call.
        let self_state = match self.ledger.lock() {
            Ok(guard) => guard.to_self_state(),
            // Poisoned mutex: another thread panicked while holding the
            // ledger. Fall through with healthy defaults so the gate
            // still runs and the agent can make progress; the next
            // successful record_violation will recover.
            Err(poisoned) => poisoned.into_inner().to_self_state(),
        };

        let (verdict, score) = evaluate_tool_call(
            &name,
            &self.thresholds,
            &self_state,
            &self.norms,
            /* llm_risk_override */ None,
            /* tool_affinity */ None,
        );

        // Emit a structured verdict event for every gate decision (not just
        // violations) so operators can observe the gate's behaviour through
        // the unified log surface, /api/logs, and the broadcast hook that
        // bridges to registered Observers. Without this the gate is a black
        // box: Allow decisions left no trace and blocks only surfaced as a
        // tool-call cancellation with no score attribution.
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(if matches!(verdict, GateVerdict::Allow) {
                    ::zeroclaw_log::EventOutcome::Success
                } else {
                    ::zeroclaw_log::EventOutcome::Failure
                })
                .with_attrs(::serde_json::json!({
                    "tool": name,
                    "verdict": verdict_label(verdict),
                    "score": score,
                })),
            "conscience gate verdict"
        );

        // Non-Allow verdicts record a violation so the score erodes for
        // subsequent calls (Plan v2 Slice A step 8). The harm magnitude
        // is approximated as `1 - score` — the gate's score is "how
        // pro-act this call looks"; its complement is the residual harm
        // the gate is rejecting on.
        if !matches!(verdict, GateVerdict::Allow) {
            let harm = (1.0 - score).clamp(0.0, 1.0);
            // Take a clone of the path so we can write to disk
            // *outside* the mutex critical section — file I/O on the
            // hot path would otherwise stall every subsequent gate
            // call queued on the lock.
            let path = self.ledger_path.clone();
            let snapshot: Option<IntegrityLedger> = if let Ok(mut guard) = self.ledger.lock() {
                guard.record_violation(&name, harm);
                path.is_some().then(|| guard.clone())
            } else {
                None
            };
            if let (Some(path), Some(snap)) = (path, snapshot)
                && let Err(err) = snap.save(&path)
            {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "path": path.display().to_string(),
                            "error": err.to_string(),
                        })),
                    "IntegrityLedger autosave failed; in-memory state still authoritative"
                );
            }
        }

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

/// Stable, log-friendly label for a gate verdict. Kept in English with a
/// fixed set of values (RFC #5653 §4.6 — log fields are never translated)
/// so downstream observers and dashboards can match on it.
fn verdict_label(verdict: GateVerdict) -> &'static str {
    match verdict {
        GateVerdict::Allow => "allow",
        GateVerdict::Block => "block",
        GateVerdict::Ask => "ask",
        GateVerdict::Revise => "revise",
    }
}

/// Translate the serde-friendly mirror that lives in `zeroclaw-config`
/// straight into the runtime `Norm` the gate evaluates against.
/// `zeroclaw-config` cannot depend on the `zeroclaw-conscience` crate, so
/// it carries its own `NormConfigSerde`; this adapter is the single hop
/// that maps it onto the conscience crate's `Norm`.
fn convert_norm(serde_norm: &NormConfigSerde) -> Norm {
    Norm {
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

/// Install the conscience hook factory on the runtime's global registry.
///
/// Called once from the binary's startup (gated on `x0-extended`). The
/// factory inspects `[conscience].gate_enabled` at Agent-build time, so
/// flipping the config flag at reload picks up the new value the next
/// time any Agent spawns.
pub fn register_hook_factory() {
    zeroclaw_runtime::hooks::registry::register_factory(Box::new(|cfg: &Config| {
        if cfg.conscience.gate_enabled {
            // Production path: persist to <data_dir>/conscience/ledger.json
            // so violations survive restarts. Tests construct the hook
            // via ConscienceHook::new directly to keep state in memory.
            vec![Box::new(ConscienceHook::with_persistence(
                &cfg.conscience,
                &cfg.data_dir,
            ))]
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
        // file_read is a low-harm tool name (scores 0.74); at the shipped
        // defaults (allow_above=0.70) it falls in Allow.
        let result = hook.before_tool_call("file_read".into(), Value::Null).await;
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
        let result = hook.before_tool_call("shell".into(), Value::Null).await;
        match result {
            HookResult::Cancel(reason) => {
                assert!(reason.starts_with("conscience:"), "reason: {reason}");
            }
            HookResult::Continue(_) => panic!("shell must NOT pass the gate at defaults"),
        }
    }

    #[tokio::test]
    async fn blocked_call_records_a_violation_in_the_ledger() {
        let cfg = ConscienceConfig::default();
        let hook = ConscienceHook::new(&cfg);
        let ledger_handle = hook.ledger();

        assert_eq!(
            ledger_handle
                .lock()
                .unwrap()
                .to_self_state()
                .recent_violations,
            0,
            "fresh ledger reports no violations"
        );

        // Drive a non-Allow verdict.
        let _ = hook.before_tool_call("shell".into(), Value::Null).await;

        let state_after = ledger_handle.lock().unwrap().to_self_state();
        assert!(
            state_after.recent_violations >= 1,
            "blocked/ask/revise call must persist a ledger entry; got {} violations",
            state_after.recent_violations
        );
    }

    #[tokio::test]
    async fn allowed_call_does_not_record_a_violation() {
        let cfg = ConscienceConfig::default();
        let hook = ConscienceHook::new(&cfg);
        let ledger_handle = hook.ledger();

        let _ = hook.before_tool_call("file_read".into(), Value::Null).await;

        assert_eq!(
            ledger_handle
                .lock()
                .unwrap()
                .to_self_state()
                .recent_violations,
            0,
            "Allow path must not write to the ledger"
        );
    }

    #[tokio::test]
    async fn ledger_state_persists_across_consecutive_calls() {
        let cfg = ConscienceConfig::default();
        let hook = ConscienceHook::new(&cfg);
        let ledger_handle = hook.ledger();

        for _ in 0..3 {
            let _ = hook.before_tool_call("shell".into(), Value::Null).await;
        }

        let state = ledger_handle.lock().unwrap().to_self_state();
        assert!(
            state.recent_violations >= 3,
            "three blocked calls should leave at least three violations; got {}",
            state.recent_violations
        );
    }

    #[tokio::test]
    async fn ledger_autosaves_to_disk_on_violation() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = ConscienceConfig::default();
        let hook = ConscienceHook::with_persistence(&cfg, tmp.path());
        let path = tmp.path().join("conscience").join("ledger.json");

        // No file yet — first violation should create it.
        let _ = hook.before_tool_call("shell".into(), Value::Null).await;

        assert!(
            path.exists(),
            "violation must autosave the ledger to disk at {}",
            path.display()
        );
        let bytes = std::fs::read(&path).unwrap();
        assert!(!bytes.is_empty(), "saved file must not be empty");
        let reloaded: IntegrityLedger = serde_json::from_slice(&bytes).unwrap();
        assert!(
            !reloaded.violations.is_empty(),
            "reloaded ledger should carry the violation we just produced"
        );
    }

    #[tokio::test]
    async fn ledger_state_survives_a_simulated_restart() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = ConscienceConfig::default();

        // First "process": land two violations.
        {
            let hook = ConscienceHook::with_persistence(&cfg, tmp.path());
            for _ in 0..2 {
                let _ = hook.before_tool_call("shell".into(), Value::Null).await;
            }
        }

        // Second "process": a fresh hook should pick up the saved ledger.
        let hook2 = ConscienceHook::with_persistence(&cfg, tmp.path());
        let state = hook2.ledger().lock().unwrap().to_self_state();
        assert!(
            state.recent_violations >= 2,
            "ledger must reload from disk; saw {} recent_violations",
            state.recent_violations,
        );
        assert!(
            state.integrity_score < 1.0,
            "integrity_score must reflect the persisted violations; got {}",
            state.integrity_score,
        );
    }

    #[tokio::test]
    async fn ledger_load_falls_back_when_file_is_corrupt() {
        let tmp = tempfile::tempdir().unwrap();
        // Plant a junk file at the expected path.
        std::fs::create_dir_all(tmp.path().join("conscience")).unwrap();
        std::fs::write(
            tmp.path().join("conscience").join("ledger.json"),
            b"not valid json",
        )
        .unwrap();

        let cfg = ConscienceConfig::default();
        let hook = ConscienceHook::with_persistence(&cfg, tmp.path());
        let state = hook.ledger().lock().unwrap().to_self_state();
        assert_eq!(
            state.recent_violations, 0,
            "corrupt persisted ledger must fall back to a fresh healthy state"
        );
        assert_eq!(
            state.integrity_score, 1.0,
            "corrupt file → fresh ledger → score 1.0"
        );
    }

    #[test]
    fn verdict_label_is_stable_and_lowercase() {
        // Observers and dashboards match on these exact strings; they are
        // part of the log contract and must not drift.
        assert_eq!(verdict_label(GateVerdict::Allow), "allow");
        assert_eq!(verdict_label(GateVerdict::Block), "block");
        assert_eq!(verdict_label(GateVerdict::Ask), "ask");
        assert_eq!(verdict_label(GateVerdict::Revise), "revise");
    }
}
