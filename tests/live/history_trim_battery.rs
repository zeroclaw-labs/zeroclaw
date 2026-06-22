//! Expansion battery for the history-trim redo. These go beyond the original
//! two RED->GREEN proofs and stress the parts most likely to regress:
//!
//!   3. The `history_trimmed` event carries REAL numbers, not a fired-but-empty
//!      signal. `dropped_messages > 0` and `kept_turns >= 1` must hold whenever
//!      the event appears, so a UI rendering it shows truthful counts.
//!   4. Tool-pair atomicity over the wire: a heavy multi-tool conversation under
//!      a tiny budget never produces a provider 400. If trim split a
//!      tool_use/tool_result pair, Anthropic rejects the next request and the
//!      turn surfaces an error event. Absence of error across many trims proves
//!      whole-turn atomicity holds against the real API.
//!   5. Recall-after-trim honesty: when the event fires, the model is allowed to
//!      say it doesn't know; when it does NOT fire, the model must still recall
//!      its own recent tool result. No silent amnesia.
//!   6. Trim is idempotent and bounded: across a long run the most recent turn
//!      always survives (kept_turns never drops to 0), so the agent always has
//!      its current task in context.
//!
//! Run: `cargo test --test live live_history_trim_ -- --ignored --nocapture`

use std::sync::Arc;

use serde_json::{Value, json};
use tokio::sync::mpsc;

use zeroclaw_config::scattered_types::HistoryPrunerConfig;
use zeroclaw_config::schema::{
    AliasedAgentConfig, Config, RiskProfileConfig, RuntimeProfileConfig,
};
use zeroclaw_config::secrets::SecretStore;
use zeroclaw_runtime::rpc::context::RpcContext;
use zeroclaw_runtime::rpc::dispatch::RpcDispatcher;
use zeroclaw_runtime::rpc::session::SessionStore;

const TINY_PROFILE: &str = "tiny";
const AGENT_ALIAS: &str = "trim_probe";

fn decrypt_personal_code_key() -> String {
    let home = std::env::var("HOME").expect("HOME not set");
    let zc_dir = std::path::Path::new(&home).join(".zeroclaw");
    let cfg_path = zc_dir.join("config.toml");
    let raw = std::fs::read_to_string(&cfg_path).expect("read ~/.zeroclaw/config.toml");
    let doc: toml::Value = toml::from_str(&raw).expect("parse config.toml");
    let enc = doc
        .get("providers")
        .and_then(|v| v.get("models"))
        .and_then(|v| v.get("anthropic"))
        .and_then(|v| v.get("personal_code"))
        .and_then(|v| v.get("api_key"))
        .and_then(|v| v.as_str())
        .expect("personal_code anthropic api_key present");
    let store = SecretStore::new(&zc_dir, true);
    store.decrypt(enc).expect("decrypt enc2: api_key")
}

fn throwaway_config(tmp: &std::path::Path, api_key: String, budget: usize) -> Config {
    use std::collections::HashMap;

    let workspace_dir = tmp.join("workspace");
    std::fs::create_dir_all(&workspace_dir).unwrap();

    let mut providers = zeroclaw_config::providers::Providers::default();
    {
        let base = providers
            .models
            .ensure("anthropic", "personal_code")
            .expect("anthropic slot");
        base.api_key = Some(api_key);
        base.model = Some("claude-opus-4-8".into());
        base.max_tokens = Some(2000);
    }

    let mut runtime_profiles = HashMap::new();
    runtime_profiles.insert(
        TINY_PROFILE.to_string(),
        RuntimeProfileConfig {
            agentic: true,
            max_tool_iterations: 8,
            max_context_tokens: Some(budget),
            history_pruning: HistoryPrunerConfig {
                enabled: true,
                max_tokens: budget,
                keep_recent: 1,
                collapse_tool_results: true,
            },
            ..Default::default()
        },
    );

    let mut agents = HashMap::new();
    agents.insert(
        AGENT_ALIAS.to_string(),
        AliasedAgentConfig {
            enabled: true,
            model_provider: "anthropic.personal_code".into(),
            risk_profile: "probe".into(),
            runtime_profile: TINY_PROFILE.into(),
            ..Default::default()
        },
    );

    let mut risk_profiles = HashMap::new();
    risk_profiles.insert(
        "probe".to_string(),
        RiskProfileConfig {
            level: zeroclaw_config::autonomy::AutonomyLevel::Full,
            workspace_only: false,
            require_approval_for_medium_risk: false,
            block_high_risk_commands: false,
            auto_approve: vec!["file_write".into(), "file_read".into()],
            ..RiskProfileConfig::default()
        },
    );

    Config {
        data_dir: workspace_dir,
        config_path: tmp.join("config.toml"),
        providers,
        agents,
        risk_profiles,
        runtime_profiles,
        ..Config::default()
    }
}

fn build_dispatcher(config: Config) -> (RpcDispatcher, mpsc::Receiver<String>) {
    use zeroclaw_infra::session_queue::SessionActorQueue;
    let queue = Arc::new(SessionActorQueue::new(4, 10, 60));
    let sessions = Arc::new(SessionStore::new(16, queue));
    let ctx = RpcContext::for_live_test(config, sessions);
    let (tx, rx) = mpsc::channel(256);
    let dispatcher = RpcDispatcher::new(ctx, tx, "live-trim-probe".into());
    (dispatcher, rx)
}

fn drain(rx: &mut mpsc::Receiver<String>) -> Vec<Value> {
    let mut out = Vec::new();
    while let Ok(line) = rx.try_recv() {
        if let Ok(v) = serde_json::from_str::<Value>(&line) {
            out.push(v);
        }
    }
    out
}

fn event_type(v: &Value) -> Option<&str> {
    v.get("params")
        .and_then(|p| p.get("type"))
        .and_then(|t| t.as_str())
}

fn is_history_trimmed(v: &Value) -> bool {
    event_type(v) == Some("history_trimmed")
}

fn is_turn_complete(v: &Value) -> bool {
    event_type(v) == Some("turn_complete")
}

fn is_error(v: &Value) -> bool {
    event_type(v) == Some("error") || v.get("error").is_some()
}

fn u64_field(v: &Value, key: &str) -> Option<u64> {
    v.get("params").and_then(|p| p.get(key)).and_then(Value::as_u64)
}

async fn await_turn(rx: &mut mpsc::Receiver<String>, secs: u64) -> Vec<Value> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    let mut out = Vec::new();
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await {
            Ok(Some(line)) => {
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    let done = is_turn_complete(&v);
                    out.push(v);
                    if done {
                        break;
                    }
                }
            }
            Ok(None) => break,
            Err(_) => continue,
        }
    }
    out
}

async fn initialize(dispatcher: &mut RpcDispatcher) {
    dispatcher
        .process_line_for_test(
            &json!({
                "jsonrpc": "2.0", "id": 0, "method": "initialize",
                "params": {"protocol_version": 1}
            })
            .to_string(),
        )
        .await;
}

async fn new_session(dispatcher: &mut RpcDispatcher, rx: &mut mpsc::Receiver<String>, sid: &str) {
    dispatcher
        .process_line_for_test(
            &json!({
                "jsonrpc": "2.0", "id": 1, "method": "session/new",
                "params": {"agent_alias": AGENT_ALIAS, "chat_mode": "acp", "session_id": sid}
            })
            .to_string(),
        )
        .await;
    let _ = drain(rx);
}

async fn prompt(
    dispatcher: &mut RpcDispatcher,
    rx: &mut mpsc::Receiver<String>,
    sid: &str,
    id: i64,
    text: &str,
) -> Vec<Value> {
    dispatcher
        .process_line_for_test(
            &json!({
                "jsonrpc": "2.0", "id": id, "method": "session/prompt",
                "params": {"session_id": sid, "prompt": text}
            })
            .to_string(),
        )
        .await;
    await_turn(rx, 150).await
}

/// (3) When the event fires it must carry truthful, non-zero numbers and (6)
/// kept_turns must never collapse to zero — the current task always survives.
#[tokio::test]
#[ignore = "requires live anthropic personal_code credentials in ~/.zeroclaw"]
async fn live_history_trim_event_carries_real_numbers() {
    let api_key = decrypt_personal_code_key();
    let tmp = tempfile::TempDir::new().unwrap();
    let config = throwaway_config(tmp.path(), api_key, 600);
    let (mut dispatcher, mut rx) = build_dispatcher(config);

    initialize(&mut dispatcher).await;
    let _ = drain(&mut rx);
    let sid = "live-trim-numbers";
    new_session(&mut dispatcher, &mut rx, sid).await;

    let mut events_seen = 0usize;
    for turn in 0..8 {
        let evs = prompt(
            &mut dispatcher,
            &mut rx,
            sid,
            100 + turn,
            &format!(
                "Turn {turn}: write a one-paragraph note about subject {turn} into \
                 tmp/note-{turn}.md with file_write, then state the path."
            ),
        )
        .await;
        for n in &evs {
            eprintln!("[turn {turn}] {n}");
            if is_history_trimmed(n) {
                events_seen += 1;
                let dropped = u64_field(n, "dropped_messages");
                let kept = u64_field(n, "kept_turns");
                assert!(
                    dropped.is_some_and(|d| d > 0),
                    "history_trimmed fired with dropped_messages={dropped:?}; a \
                     fired-but-empty event lies to the UI"
                );
                assert!(
                    kept.is_some_and(|k| k >= 1),
                    "history_trimmed fired with kept_turns={kept:?}; the current \
                     turn must always survive (option-a invariant)"
                );
            }
        }
    }

    assert!(
        events_seen > 0,
        "8 budget-600 tool turns produced no history_trimmed event at all"
    );
}

/// (4) Tool-pair atomicity against the real Anthropic API. Heavy multi-tool
/// turns under a tiny budget force repeated trims. If any trim split a
/// tool_use/tool_result pair, the very next request 400s and an error event
/// surfaces. Zero errors across the run proves whole-turn atomicity holds.
#[tokio::test]
#[ignore = "requires live anthropic personal_code credentials in ~/.zeroclaw"]
async fn live_history_trim_never_splits_tool_pairs() {
    let api_key = decrypt_personal_code_key();
    let tmp = tempfile::TempDir::new().unwrap();
    let config = throwaway_config(tmp.path(), api_key, 700);
    let (mut dispatcher, mut rx) = build_dispatcher(config);

    initialize(&mut dispatcher).await;
    let _ = drain(&mut rx);
    let sid = "live-trim-pairs";
    new_session(&mut dispatcher, &mut rx, sid).await;

    let mut trims = 0usize;
    let mut errors: Vec<String> = Vec::new();
    for turn in 0..10 {
        let evs = prompt(
            &mut dispatcher,
            &mut rx,
            sid,
            200 + turn,
            &format!(
                "Turn {turn}: first write the word alpha-{turn} into tmp/a-{turn}.md, \
                 then read it back with file_read, then write the word omega-{turn} into \
                 tmp/o-{turn}.md. Use the tools. Report both paths."
            ),
        )
        .await;
        for n in &evs {
            eprintln!("[turn {turn}] {n}");
            if is_history_trimmed(n) {
                trims += 1;
            }
            if is_error(n) {
                errors.push(n.to_string());
            }
        }
    }

    eprintln!("total trims observed: {trims}");
    assert!(
        errors.is_empty(),
        "provider/dispatch errors during a heavy multi-tool trimmed run \
         (a split tool_use/tool_result pair 400s Anthropic): {errors:?}"
    );
    assert!(
        trims > 0,
        "10 multi-tool budget-700 turns never triggered a trim; budget is not \
         exercising the path"
    );
}

/// (5) Recall-after-trim honesty: if the event fired the model may say it does
/// not know; if it did NOT fire the model must still recall the recent word.
#[tokio::test]
#[ignore = "requires live anthropic personal_code credentials in ~/.zeroclaw"]
async fn live_history_trim_recall_is_honest_or_signalled() {
    let api_key = decrypt_personal_code_key();
    let tmp = tempfile::TempDir::new().unwrap();
    // Looser budget so the FIRST recall after one write usually survives,
    // making the no-trim recall branch meaningful.
    let config = throwaway_config(tmp.path(), api_key, 1200);
    let (mut dispatcher, mut rx) = build_dispatcher(config);

    initialize(&mut dispatcher).await;
    let _ = drain(&mut rx);
    let sid = "live-trim-recall";
    new_session(&mut dispatcher, &mut rx, sid).await;

    let mut trimmed = false;
    let w1 = prompt(
        &mut dispatcher,
        &mut rx,
        sid,
        300,
        "Write the single word marigold into tmp/flower.md with file_write. Confirm the path.",
    )
    .await;
    for n in &w1 {
        if is_history_trimmed(n) {
            trimmed = true;
        }
    }

    let recall = prompt(
        &mut dispatcher,
        &mut rx,
        sid,
        301,
        "Without using any tool, what exact word did you just write into tmp/flower.md?",
    )
    .await;
    for n in &recall {
        eprintln!("[recall] {n}");
        if is_history_trimmed(n) {
            trimmed = true;
        }
    }

    let text = recall
        .iter()
        .filter(|v| is_turn_complete(v))
        .filter_map(|v| {
            v.get("params")
                .and_then(|p| p.get("content"))
                .and_then(|c| c.as_str())
                .map(str::to_string)
        })
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();

    if trimmed {
        return;
    }
    assert!(
        text.contains("marigold"),
        "no trim event fired yet the model could not recall its own just-written \
         word (got: {text:?}); silent context loss"
    );
}
