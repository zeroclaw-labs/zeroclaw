use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::mpsc;
use zeroclaw_api::ingress::IngressContext;
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::schema::Config;
use zeroclaw_providers::ChatMessage;

use super::safety_net::{CountingTool, ScriptedProvider, text_response, tool_call, tool_response};
use crate::agent::loop_::{
    LoopKnobs, ResolvedAgentExecution, ResolvedIo, ResolvedModelAccess, ResolvedRuntimeKnobs,
    ToolLoop, apply_policy_tool_filter, run_tool_call_loop,
};
use crate::observability;
use crate::tools::scoped::{ScopedAssembly, ScopedToolRegistry};
use crate::tools::{AllToolsResult, Tool};

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
        tracking: "gateway + loop_::run + process_message route through assemble(); \
                   remaining sites cut over one PR at a time",
        status: RowStatus::TrackedDivergence,
        evidence: "gateway/loop_::run/process_message route through assemble(); \
                   from_config/orchestrator/delegate still hand-roll the same filter, \
                   tracked by the remaining Epic A cut-overs + the seal",
    },
    ParityRow {
        surface: "A",
        setting: "built-in filter semantic uniform (safe-defaults admit)",
        owner_epic: "A",
        tracking: "filter_channel_builtin_tools retired, closing ledger A4",
        status: RowStatus::Tested,
        evidence: "parity_l2_builtin_filter_semantic_parity \
                   (positive parity assertion; the divergence this row tracked is closed)",
    },
];

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
        image_cache: None,
        ingress: IngressContext::sub_turn(),
        memory: None,
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
fn a4_policy() -> Arc<SecurityPolicy> {
    Arc::new(SecurityPolicy {
        allowed_tools: Some(vec!["only_this".to_string()]),
        workspace_dir: std::env::temp_dir(),
        ..SecurityPolicy::default()
    })
}

/// Minimal `AllToolsResult` around a bare tool list, matching the fixture
/// `scoped::tests::built_with` uses to drive `assemble()` in isolation.
fn built_with(tools: Vec<Box<dyn Tool>>) -> AllToolsResult {
    AllToolsResult {
        tools,
        delegate_handle: None,
        ask_user_handle: None,
        reaction_handle: Arc::new(parking_lot::RwLock::new(std::collections::HashMap::new())),
        poll_handle: None,
        escalate_handle: None,
        channel_room_handle: None,
        unfiltered_tool_arcs: Vec::new(),
    }
}

#[tokio::test]
async fn parity_l2_builtin_filter_semantic_parity() {
    let security = a4_policy();

    let assembled = ScopedToolRegistry::assemble(ScopedAssembly {
        config: &Config::default(),
        agent_alias: "default",
        security: &security,
        built: built_with(named_tools(A4_NAMES)),
        skills: &[],
        runtime: Arc::new(crate::platform::NativeRuntime::new()),
        caller_allowed: None,
        connect_mcp: false,
        connect_peripherals: false,
        exclude_memory: false,
        list_deferred_mcp_specs: false,
        emit_assembly_logs: false,
    })
    .await;
    let seam_names = retained_names(&assembled.registry.into_inner());

    let mut via_hand_rolled = named_tools(A4_NAMES);
    apply_policy_tool_filter(&mut via_hand_rolled, Some(security.as_ref()), None);
    let hand_rolled_names = retained_names(&via_hand_rolled);

    assert_eq!(
        seam_names, hand_rolled_names,
        "the assemble() seam and the still-uncut hand-rolled call must resolve \
         the built-in filter identically"
    );
    assert!(
        !seam_names.contains(&"web_fetch".to_string()),
        "the canonical read-only default is no longer admitted past allowed_tools: {seam_names:?}"
    );
    assert!(
        seam_names.contains(&"only_this".to_string()),
        "the allowlisted tool is retained: {seam_names:?}"
    );
    assert!(
        !seam_names.contains(&"other_tool".to_string()),
        "a tool that is neither allowlisted nor a canonical default is dropped: {seam_names:?}"
    );
}
