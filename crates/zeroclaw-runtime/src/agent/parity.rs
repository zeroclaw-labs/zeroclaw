//! Agent-policy parity harness (Epic H) - the cross-path test matrix described
//! in `docs/book/src/contributing/agent-policy-parity-harness.md`.
//!
//! A `#[cfg(test)]` sibling of the `#7415` `safety_net.rs` turn-engine oracle,
//! reusing its fixtures. Two layers:
//!
//! - **L1 (engine enforcement):** "when setting S reaches `run_tool_call_loop`,
//!   the engine honors it." Path-independent locks; a refactor of the engine
//!   cannot silently stop honoring a field.
//! - **L2 (path resolution parity):** "every construction path, given one agent
//!   config, hands the engine the same resolved value for S." This is the layer
//!   that exposes divergence. Where a surface already resolves through one seam,
//!   its L2 test is a positive parity assertion. Where a confirmed divergence has
//!   no single seam yet, it ships as an always-running CHARACTERIZATION test that
//!   pins the divergence as it exists (an `assert_ne!` on the two paths' output);
//!   when the owning epic unifies the semantic, that assertion fails in the same
//!   PR, forcing it to be rewritten into the positive parity assertion. No
//!   `#[ignore]`d specs: a known-failing ignored test never runs in CI and
//!   protects nothing, so the goal is expressed as a live assertion of the
//!   current state instead.
//!
//! The `MATRIX` below is an INDEX of parity rows, not a grid of hand-written
//! per-path verdicts. Deliberately so: a static verdict grid would be exactly
//! the "data that goes stale with nothing noticing" this program exists to end:
//! a cell asserting a path's state could rot the moment another PR changes that
//! path, and no test would catch it. So the enforceable claims live ONLY
//! in the L1/L2 tests below (which fail loudly when the behavior changes); each
//! matrix row just records its owner epic, a public tracking reference, and its
//! `evidence` - the in-file test that backs it, or, for a divergence with no
//! single resolution seam yet, a tracked record pointing at where it is
//! characterized. The meta-test enforces that bookkeeping (owner + tracking +
//! evidence present), nothing more. Rows accrete one surface epic at a time;
//! this scaffold ships the framework plus the tool-surface (Epic A) rows.
//! Surfaces still under private review accrete their rows when the owning epic
//! lands. The human-readable (setting x path) grid lives in the docs page.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::mpsc;
use zeroclaw_api::ingress::IngressContext;
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_providers::ChatMessage;

use super::safety_net::{CountingTool, ScriptedProvider, text_response, tool_call, tool_response};
use crate::agent::loop_::{
    LoopKnobs, ResolvedAgentExecution, ResolvedIo, ResolvedModelAccess, ResolvedRuntimeKnobs,
    ToolLoop, apply_policy_tool_filter, filter_channel_builtin_tools, run_tool_call_loop,
};
use crate::observability;
use crate::tools::Tool;

// ── The matrix as an index of parity rows ───────────────────────────────────

/// How a row is currently backed - bookkeeping only; the enforceable claim is
/// the test named in `evidence`, never this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowStatus {
    /// An in-file L1/L2 test pins this row's behavior; it fails loudly if the
    /// behavior changes. `evidence` names it.
    Tested,
    /// A confirmed divergence with no single resolution seam to assert against
    /// in this crate yet (e.g. it spans a cross-crate boot path). Carried as a
    /// tracked record until the owning epic lands the seam; `evidence` says
    /// where it is characterized. No cell here claims a per-path verdict.
    TrackedDivergence,
}

struct ParityRow {
    surface: &'static str,
    setting: &'static str,
    /// Epic that owns turning this row into a single-seam, fully-tested row.
    owner_epic: &'static str,
    /// Public tracking reference (a merged or open PR, an issue, or a ledger
    /// row named in a public artifact).
    tracking: &'static str,
    status: RowStatus,
    /// For `Tested`: the test fn(s) that back this row. For `TrackedDivergence`:
    /// where the divergence is characterized / tracked. Never empty - the
    /// meta-test enforces it, so a row cannot ship as an unbacked assertion.
    evidence: &'static str,
}

/// Rows ship for the tool surface (Epic A). Each names its backing test or its
/// tracked-divergence record; none encodes a per-path verdict that could rot.
const MATRIX: &[ParityRow] = &[
    ParityRow {
        surface: "A",
        setting: "built-in filter minted through the one assemble() seam on every path",
        owner_epic: "A",
        tracking: "#8640 merged (gateway); remaining sites cut over one PR at a time",
        // Every construction path DOES apply the built-in filter today - the
        // former gateway-listings gap (ledger A1) was closed by #8640, now
        // merged. What is not yet uniform is that only the gateway mints its
        // registry through the `assemble()` seam; loop_ run / process_message,
        // Agent::from_config, the orchestrator, and the delegate independent-
        // target builder still hand-roll the same filter call (verified present,
        // by convention). Until those cut-overs land - and the engine field
        // seals to ScopedToolRegistry - cross-path uniformity is enforced by
        // discipline, not by a single seam, so there is nothing to assert green
        // here yet. A cross-crate boot path with no in-file seam; each cut-over
        // PR carries its own site test.
        status: RowStatus::TrackedDivergence,
        evidence: "gateway routes through assemble() as of #8640 (merged); \
                   loop_/from_config/orchestrator/delegate still hand-roll the same \
                   filter, tracked by the remaining Epic A cut-overs + the seal",
    },
    ParityRow {
        surface: "A",
        setting: "built-in filter semantic uniform (safe-defaults admit)",
        owner_epic: "A",
        tracking: "#8640 (merged; residual-divergence note) + ledger A4",
        // process_message admits the canonical read-only defaults past
        // allowed_tools at non-Full autonomy; the plain policy filter does not.
        // This IS backed by an in-file test pair, so it changes only loudly.
        status: RowStatus::Tested,
        evidence: "parity_l2_builtin_filter_semantic_divergence_characterized \
                   (an always-running loud-change guard; no ignored spec)",
    },
];

/// Meta-test: bookkeeping only. Every row must name a surface, a setting, an
/// owner epic, a public tracking reference, and its `evidence` (an in-file test
/// or a tracked-divergence record). It intentionally does NOT judge any per-path
/// verdict - there are none to judge; the enforceable claims are the L1/L2 tests
/// below, which fail loudly when the behavior they pin changes.
#[test]
fn parity_matrix_rows_are_owned_tracked_and_evidenced() {
    for row in MATRIX {
        assert!(
            !row.surface.is_empty() && !row.setting.is_empty(),
            "matrix row missing surface/setting"
        );
        assert!(
            !row.owner_epic.is_empty(),
            "row '{}' has no owner epic",
            row.setting
        );
        assert!(
            !row.tracking.is_empty(),
            "row '{}' has no public tracking reference",
            row.setting
        );
        assert!(
            !row.evidence.is_empty(),
            "row '{}' has no evidence (name the backing test or the tracked-divergence record)",
            row.setting
        );
        // A `Tested` row must point at something test-shaped; a
        // `TrackedDivergence` row must not masquerade as tested.
        if row.status == RowStatus::Tested {
            assert!(
                row.evidence.contains("parity_"),
                "Tested row '{}' must name its backing parity_* test in evidence",
                row.setting
            );
        }
    }
}

// ── L1 exemplar: the engine honors excluded_tools ───────────────────────────

/// L1 engine lock: a tool named in `excluded_tools` is never executed, even if
/// the model calls it anyway. Constructed through `ResolvedAgentExecution::
/// resolve` - the seam every production path uses (#8179).
#[tokio::test]
async fn parity_l1_engine_honors_excluded_tools() {
    let exec_count = Arc::new(AtomicUsize::new(0));
    let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool {
        name: "echo",
        calls: Arc::clone(&exec_count),
    })];
    let provider = ScriptedProvider::new(vec![
        tool_response(vec![tool_call("tc1", "echo")]),
        text_response("done"),
    ]);
    let excluded = vec!["echo".to_string()];
    let mut history = vec![ChatMessage::user("hi")];
    let (dtx, _drx) = mpsc::channel(256);
    let turn_id = uuid::Uuid::new_v4().to_string();
    let result = run_tool_call_loop(ToolLoop {
        exec: ResolvedAgentExecution::resolve(
            ResolvedModelAccess {
                model_provider: &provider,
                provider_name: "mock",
                model: "mock-model",
                temperature: None,
            },
            ResolvedIo {
                tools_registry: &tools_registry,
                observer: &observability::NoopObserver {},
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                hooks: None,
                activated_tools: None,
                model_switch_callback: None,
                receipt_generator: None,
            },
            ResolvedRuntimeKnobs {
                max_tool_iterations: 5,
                excluded_tools: &excluded,
                dedup_exempt_tools: &[],
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 30_000,
                context_token_budget: 100_000,
                knobs: &LoopKnobs::default(),
            },
        ),
        history: &mut history,
        channel_name: "cli",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: Some(dtx),
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        ingress: IngressContext::internal(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await;
    assert_eq!(
        exec_count.load(Ordering::SeqCst),
        0,
        "an excluded tool must never execute, even when the model calls it"
    );
    // The loop itself must not crash on the refused call; it surfaces the
    // refusal to the model and continues the scripted conversation.
    result.expect("loop must complete after refusing the excluded tool");
}

// ── L2 exemplar: built-in filter semantic parity (ledger row A4) ────────────

fn named_tools(names: &[&'static str]) -> Vec<Box<dyn Tool>> {
    names
        .iter()
        .map(|n| {
            Box::new(CountingTool {
                name: n,
                calls: Arc::new(AtomicUsize::new(0)),
            }) as Box<dyn Tool>
        })
        .collect()
}

fn retained_names(tools: &[Box<dyn Tool>]) -> Vec<String> {
    tools.iter().map(|t| t.name().to_string()).collect()
}

/// The A4 registry shape: a canonical read-only default (`web_fetch`, in
/// `default_auto_approve()`), the allowlisted tool, and a tool on neither list.
const A4_NAMES: &[&str] = &["web_fetch", "only_this", "other_tool"];

/// A policy that allowlists one bespoke tool, at default (Supervised, non-Full)
/// autonomy.
fn a4_policy() -> SecurityPolicy {
    SecurityPolicy {
        allowed_tools: Some(vec!["only_this".to_string()]),
        workspace_dir: std::env::temp_dir(),
        ..SecurityPolicy::default()
    }
}

/// L2 characterization of ledger row A4: a single, always-running test that
/// pins the built-in-filter divergence as it exists today. `process_message`'s
/// `filter_channel_builtin_tools` admits the canonical read-only default
/// (`web_fetch`) past `allowed_tools` at non-Full autonomy; the plain
/// `apply_policy_tool_filter` (every other path) drops it. Both agree on the
/// allowlisted tool and on a tool that is neither.
///
/// This test is the loud-change guard, and it is a PASSING test, not an
/// `#[ignore]`d one: a known-failing ignored "spec" would never run in CI and
/// could not protect anything. The parity GOAL is instead expressed as an
/// assertion of the current divergence - so when Epic A unifies the semantic
/// into the seam, the `assert_ne!` below fails in that same PR, forcing the
/// author to rewrite this into the positive parity assertion
/// (`retained_names(channel) == retained_names(plain)`). The divergence cannot
/// change silently. Tracked: #8640 residual-divergence note, ledger A4.
#[test]
fn parity_l2_builtin_filter_semantic_divergence_characterized() {
    let policy = a4_policy();
    let mut via_channel = named_tools(A4_NAMES);
    let mut via_plain = named_tools(A4_NAMES);
    filter_channel_builtin_tools(&mut via_channel, &policy);
    apply_policy_tool_filter(&mut via_plain, Some(&policy), None);
    let channel_names = retained_names(&via_channel);
    let plain_names = retained_names(&via_plain);
    // The loud-change guard: today the two paths diverge. When Epic A unifies
    // the semantic, this assertion fails and this test must become the positive
    // parity assertion (`assert_eq!`) in the same PR.
    assert_ne!(
        channel_names, plain_names,
        "A4 divergence unified? rewrite this into the parity assertion (see the doc comment)"
    );
    assert!(
        channel_names.contains(&"web_fetch".to_string()),
        "channel filter admits the canonical read-only default: {channel_names:?}"
    );
    assert!(
        !plain_names.contains(&"web_fetch".to_string()),
        "plain policy filter drops it (not in allowed_tools): {plain_names:?}"
    );
    assert!(
        channel_names.contains(&"only_this".to_string())
            && plain_names.contains(&"only_this".to_string()),
        "both admit the allowlisted tool"
    );
    assert!(
        !channel_names.contains(&"other_tool".to_string())
            && !plain_names.contains(&"other_tool".to_string()),
        "both drop a tool that is neither allowlisted nor a canonical default"
    );
}
