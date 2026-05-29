#![cfg(feature = "x0-broken-legacy")]

//! Conscience gate integration test — PR-2.
//!
//! These tests validate the contract that `agent/loop_.rs` depends on
//! when `[bot.conscience] gate_enabled = true`. The wiring there
//! consults `evaluate_tool_call` with default thresholds, an empty norm
//! set, and a neutral SelfState, then blocks any verdict other than
//! Allow. If these tests pass, the gate behaves as the dispatch site
//! expects: high-harm tools block; benign tools are not auto-blocked
//! by the safety constraint; explicit Forbid norms always win.

use zeroclaw::config::schema::ConscienceConfig;
use zeroclaw::conscience::{
    GateVerdict, Norm, NormAction, SelfState, Thresholds, evaluate_tool_call,
};

fn neutral_self_state() -> SelfState {
    SelfState {
        integrity_score: 1.0,
        recent_violations: 0,
        active_repairs: 0,
        arousal: None,
        confidence: None,
        risk_level: None,
        free_energy: None,
    }
}

fn config_thresholds(cfg: &ConscienceConfig) -> Thresholds {
    Thresholds {
        allow_above: cfg.allow_threshold,
        ask_above: cfg.ask_threshold,
        block_below: cfg.block_threshold,
    }
}

#[test]
fn default_config_disables_gate() {
    // The gate must be opt-in. A fresh ConscienceConfig MUST NOT enable
    // the gate — that is the contract the dispatch site relies on to
    // preserve current behavior for users who haven't read about the
    // feature.
    let cfg = ConscienceConfig::default();
    assert!(
        !cfg.gate_enabled,
        "ConscienceConfig::default() must disable the gate; got gate_enabled = true"
    );
}

#[test]
fn default_thresholds_match_conscience_module() {
    // ConscienceConfig::default() exposes thresholds that mirror
    // crate::conscience::types::Thresholds::default(). If they ever
    // drift, the dispatch-site behavior also drifts silently.
    let cfg = ConscienceConfig::default();
    let from_cfg = config_thresholds(&cfg);
    let canonical = Thresholds::default();
    assert_eq!(from_cfg.allow_above, canonical.allow_above);
    assert_eq!(from_cfg.ask_above, canonical.ask_above);
    assert_eq!(from_cfg.block_below, canonical.block_below);
}

#[test]
fn high_harm_tool_is_blocked_by_safety_constraint() {
    // wallet_send carries harm=0.7 in evaluate_tool_call's tool taxonomy;
    // the built-in safety constraint (weight 0.8) blocks any harm above
    // 1.0 - 0.8 = 0.2, so the verdict MUST be Block regardless of
    // thresholds, norms, or self-state. This is the load-bearing
    // assertion: the dispatch site expects a deterministic Block here.
    let cfg = ConscienceConfig::default();
    let thresholds = config_thresholds(&cfg);
    let self_state = neutral_self_state();

    let (verdict, _score) =
        evaluate_tool_call("wallet_send", &thresholds, &self_state, &[], None, None);
    assert_eq!(verdict, GateVerdict::Block);
}

#[test]
fn benign_read_only_tool_is_not_blocked() {
    // A read-only tool ("memory_recall") falls into the catch-all bucket
    // (harm=0.1, reversibility=0.9). It MUST NOT trigger the safety
    // constraint, since 0.1 < 0.2. The verdict can be Allow or Ask
    // depending on score; what matters is that we never auto-Block it.
    let cfg = ConscienceConfig::default();
    let thresholds = config_thresholds(&cfg);
    let self_state = neutral_self_state();

    let (verdict, _score) =
        evaluate_tool_call("memory_recall", &thresholds, &self_state, &[], None, None);
    assert_ne!(
        verdict,
        GateVerdict::Block,
        "benign read-only tool should not be auto-blocked"
    );
}

#[test]
fn explicit_forbid_norm_blocks_matching_tool() {
    // Even a benign-looking tool name MUST be blocked when an explicit
    // high-severity Forbid norm matches its name. This is the path the
    // soul/constitution wiring will take in a follow-up PR to express
    // hard ethical red lines.
    let cfg = ConscienceConfig::default();
    let thresholds = config_thresholds(&cfg);
    let self_state = neutral_self_state();

    let norms = vec![Norm {
        name: "no-payments-during-soak".into(),
        action: NormAction::Forbid,
        condition: "memory_recall".into(),
        severity: 0.95,
    }];

    let (verdict, _score) = evaluate_tool_call(
        "memory_recall",
        &thresholds,
        &self_state,
        &norms,
        None,
        None,
    );
    assert_eq!(verdict, GateVerdict::Block);
}

#[test]
fn forbid_norm_below_severity_threshold_does_not_force_block() {
    // Norms with severity < 0.9 do not unconditionally block; they
    // contribute only via the wider scoring path. A Discourage or
    // low-severity Forbid against a benign tool MUST allow the
    // tool to be evaluated normally (verdict NOT necessarily Block).
    let cfg = ConscienceConfig::default();
    let thresholds = config_thresholds(&cfg);
    let self_state = neutral_self_state();

    let norms = vec![Norm {
        name: "discourage-soft".into(),
        action: NormAction::Forbid,
        condition: "memory_recall".into(),
        severity: 0.5, // below the 0.9 hard-block threshold
    }];

    let (verdict, _) = evaluate_tool_call(
        "memory_recall",
        &thresholds,
        &self_state,
        &norms,
        None,
        None,
    );
    assert_ne!(
        verdict,
        GateVerdict::Block,
        "low-severity norm should not unconditionally block"
    );
}
