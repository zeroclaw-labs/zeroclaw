//! UNCOMMITTED live probe. Validation evidence only; never staged.
//! Run: cargo test -p zeroclaw-runtime --test live_duplicate_narration_probe -- --ignored --nocapture
//!
//! Drives the REAL anthropic.personal_code provider through `run_tool_call_loop`
//! with an `on_delta` sink wired exactly like the channel orchestrator
//! (StreamDelta::Text accumulated into a single draft buffer). The duplicate
//! manifests when narration text streamed live before a tool call is re-sent
//! post-tool-call on the same sink, doubling it in the accumulated draft.

use std::sync::Arc;
use std::sync::Mutex;

use zeroclaw_config::schema::Config;
use zeroclaw_config::schema::MultimodalConfig;
use zeroclaw_config::schema::PacingConfig;
use zeroclaw_runtime::agent::agent::build_session_model_provider;
use zeroclaw_runtime::agent::loop_::{
    DraftEvent, LoopKnobs, StreamDelta, ToolLoop, run_tool_call_loop,
};
use zeroclaw_runtime::observability::NoopObserver;
use zeroclaw_runtime::platform::NativeRuntime;
use zeroclaw_runtime::security::{AutonomyLevel, SecurityPolicy};
use zeroclaw_runtime::tools::ShellTool;
use zeroclaw_runtime::tools::Tool;

use zeroclaw_providers::ChatMessage;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn live_anthropic_narration_then_shell_no_duplicate() {
    let config = Config::load_or_init()
        .await
        .expect("load ~/.zeroclaw/config.toml");

    let (model_provider, provider_name, model) =
        build_session_model_provider(&config, "anthropic.personal_code", None)
            .expect("build anthropic.personal_code provider");

    let security = Arc::new(SecurityPolicy {
        autonomy: AutonomyLevel::Full,
        ..SecurityPolicy::default()
    });
    let runtime = Arc::new(NativeRuntime::new());
    let shell: Box<dyn Tool> = Box::new(ShellTool::new(security, runtime));
    let tools_registry: Vec<Box<dyn Tool>> = vec![shell];

    let mut history = vec![
        ChatMessage::system(
            "You are a terse assistant. When asked to check something with a shell command, \
             first say one short sentence about what you are about to do, then call the shell tool.",
        ),
        ChatMessage::user(
            "Say one short sentence telling me you are about to check the time, then call the \
             shell tool to run `date +%s`, then report the number.",
        ),
    ];

    let observer = NoopObserver;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(1024);

    // Mirror the orchestrator draft updater: accumulate StreamDelta::Text into
    // ONE buffer, exactly as orchestrator/mod.rs does. Record every Text delta
    // and the final accumulated draft.
    let deltas: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let accumulated: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let deltas_c = Arc::clone(&deltas);
    let acc_c = Arc::clone(&accumulated);
    #[allow(clippy::disallowed_methods)]
    let drain = tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            if let StreamDelta::Text(text) = ev {
                deltas_c.lock().unwrap().push(text.clone());
                acc_c.lock().unwrap().push_str(&text);
            }
        }
    });

    let knobs = LoopKnobs::default();
    let multimodal = MultimodalConfig::default();
    let pacing = PacingConfig::default();

    let result = run_tool_call_loop(ToolLoop {
        model_provider: model_provider.as_ref(),
        history: &mut history,
        tools_registry: &tools_registry,
        observer: &observer,
        provider_name: &provider_name,
        model: &model,
        temperature: None,
        silent: true,
        approval: None,
        channel_name: "matrix",
        channel_reply_target: None,
        multimodal_config: &multimodal,
        max_tool_iterations: 6,
        cancellation_token: None,
        on_delta: Some(tx),
        hooks: None,
        excluded_tools: &[],
        dedup_exempt_tools: &[],
        activated_tools: None,
        model_switch_callback: None,
        pacing: &pacing,
        strict_tool_parsing: false,
        parallel_tools: false,
        max_tool_result_chars: 0,
        context_token_budget: 0,
        shared_budget: None,
        channel: None,
        receipt_generator: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        knobs: &knobs,
        image_cache: None,
    })
    .await
    .expect("loop should succeed");

    drain.await.ok();

    let all_deltas = deltas.lock().unwrap().clone();
    let draft = accumulated.lock().unwrap().clone();

    eprintln!("\n===== LIVE PROBE EVIDENCE (on_delta / draft sink) =====");
    eprintln!("provider={provider_name} model={model}");
    eprintln!("StreamDelta::Text count: {}", all_deltas.len());
    for (i, d) in all_deltas.iter().enumerate() {
        eprintln!("  delta[{i}] ({} bytes): {:?}", d.len(), d);
    }
    eprintln!("--- accumulated draft ({} bytes) ---\n{draft}", draft.len());
    eprintln!("--- returned final ({} bytes) ---\n{result}", result.len());

    // Duplication signal (primary): the live-streamed narration is re-sent
    // verbatim post-tool-call on the SAME sink, so the concatenation of the
    // pre-tool deltas appears a second time as a single later delta. Detect by
    // checking whether any single delta equals the concatenation of earlier
    // deltas (modulo trailing newline) — the mod.rs re-send fingerprint.
    let mut resend_detected: Option<String> = None;
    for i in 0..all_deltas.len() {
        let prefix: String = all_deltas[..i].concat();
        let candidate = all_deltas[i].trim_end_matches('\n');
        if candidate.len() >= 12 && !prefix.is_empty() && prefix.trim_end() == candidate.trim_end()
        {
            resend_detected = Some(candidate.to_string());
            break;
        }
    }

    // Duplication signal (secondary): any non-trivial line appearing more than
    // once in the accumulated draft (the buffer the channel renders).
    let mut dup_lines: Vec<String> = Vec::new();
    for line in draft.lines() {
        let t = line.trim();
        if t.len() < 12 {
            continue;
        }
        let count = draft.matches(t).count();
        if count > 1 {
            dup_lines.push(format!("[{count}x] {t}"));
        }
    }
    dup_lines.sort();
    dup_lines.dedup();

    eprintln!("resend_detected: {resend_detected:?}");
    eprintln!("dup_lines: {dup_lines:?}");
    eprintln!("===== END EVIDENCE =====\n");

    assert!(
        resend_detected.is_none() && dup_lines.is_empty(),
        "RED: duplicate narration on draft sink. resend={resend_detected:?} dup_lines={dup_lines:?}"
    );
}
