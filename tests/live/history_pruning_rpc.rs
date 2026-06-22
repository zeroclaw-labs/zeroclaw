//! Live RPC test proving history pruning is observable and never silently
//! drops the model's own tool context mid-conversation.
//!
//! Run: `cargo test --test live live_history_pruning -- --ignored --nocapture`
//! Requires the local `~/.zeroclaw/config.toml` with an encrypted
//! `providers.models.anthropic.personal_code.api_key` plus `~/.zeroclaw/.secret_key`.
//!
//! These assert the POST-REDO contract and are RED on current master:
//!   1. When history is trimmed, an RPC `session/update` event with kind
//!      `history_trimmed` is emitted so the end user sees why context changed.
//!   2. The trim never silently removes the prior turn's tool result without
//!      that event firing (the f84c05d confabulation root cause).

use std::sync::Arc;

use serde_json::{Value, json};
use tokio::sync::mpsc;

use zeroclaw_config::schema::{
    AliasedAgentConfig, Config, RiskProfileConfig, RuntimeProfileConfig,
};
use zeroclaw_config::scattered_types::HistoryPrunerConfig;
use zeroclaw_config::secrets::SecretStore;
use zeroclaw_runtime::rpc::context::RpcContext;
use zeroclaw_runtime::rpc::dispatch::RpcDispatcher;
use zeroclaw_runtime::rpc::session::SessionStore;

const TINY_PROFILE: &str = "tiny";
const AGENT_ALIAS: &str = "pruning_probe";

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

fn throwaway_config(tmp: &std::path::Path, api_key: String) -> Config {
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

    // History pruning lives on the runtime profile only. Set it LOW so the
    // trim path fires within one or two turns.
    let mut runtime_profiles = HashMap::new();
    runtime_profiles.insert(
        TINY_PROFILE.to_string(),
        RuntimeProfileConfig {
            agentic: true,
            max_tool_iterations: 8,
            max_context_tokens: Some(600),
            history_pruning: HistoryPrunerConfig {
                enabled: true,
                max_tokens: 600,
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
    let dispatcher = RpcDispatcher::new(ctx, tx, "live-pruning-probe".into());
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
    v.get("params").and_then(|p| p.get("type")).and_then(|t| t.as_str())
}

fn is_history_trimmed(v: &Value) -> bool {
    event_type(v) == Some("history_trimmed")
}

fn is_turn_complete(v: &Value) -> bool {
    event_type(v) == Some("turn_complete")
}

/// Await turn completion: session/prompt spawns the turn and signals done via a
/// `turn_complete` notification. Collect every notification until it arrives or
/// the timeout elapses.
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

#[tokio::test]
#[ignore = "requires live anthropic personal_code credentials in ~/.zeroclaw"]
async fn live_history_pruning_emits_rpc_event() {
    let api_key = decrypt_personal_code_key();
    let tmp = tempfile::TempDir::new().unwrap();
    let config = throwaway_config(tmp.path(), api_key);
    let (mut dispatcher, mut rx) = build_dispatcher(config);

    initialize(&mut dispatcher).await;
    let _ = drain(&mut rx);

    let sid = "live-pruning-001";
    dispatcher
        .process_line_for_test(
            &json!({
                "jsonrpc": "2.0", "id": 1, "method": "session/new",
                "params": {"agent_alias": AGENT_ALIAS, "chat_mode": "acp", "session_id": sid}
            })
            .to_string(),
        )
        .await;
    for n in drain(&mut rx) {
        eprintln!("[after session/new] {n}");
    }

    let mut saw_event = false;
    'outer: for turn in 0..6 {
        dispatcher
            .process_line_for_test(
                &json!({
                    "jsonrpc": "2.0", "id": 100 + turn, "method": "session/prompt",
                    "params": {
                        "session_id": sid,
                        "prompt": format!(
                            "Turn {turn}: write a one-paragraph note about topic number {turn} \
                             into the file tmp/note-{turn}.md using the file_write tool, then \
                             tell me the exact path you wrote."
                        )
                    }
                })
                .to_string(),
            )
            .await;

        for n in await_turn(&mut rx, 120).await {
            eprintln!("[turn {turn}] {n}");
            if is_history_trimmed(&n) {
                saw_event = true;
                break 'outer;
            }
        }
    }

    assert!(
        saw_event,
        "history was pruned across {AGENT_ALIAS} turns but no session/update \
         history_trimmed event was ever emitted. The end user gets no signal \
         that context was cut, which is the f84c05d confabulation root cause."
    );
}

#[tokio::test]
#[ignore = "requires live anthropic personal_code credentials in ~/.zeroclaw"]
async fn live_history_pruning_never_silently_drops_tool_result() {
    let api_key = decrypt_personal_code_key();
    let tmp = tempfile::TempDir::new().unwrap();
    let config = throwaway_config(tmp.path(), api_key);
    let (mut dispatcher, mut rx) = build_dispatcher(config);

    initialize(&mut dispatcher).await;
    let _ = drain(&mut rx);

    let sid = "live-pruning-002";
    dispatcher
        .process_line_for_test(
            &json!({
                "jsonrpc": "2.0", "id": 1, "method": "session/new",
                "params": {"agent_alias": AGENT_ALIAS, "chat_mode": "acp", "session_id": sid}
            })
            .to_string(),
        )
        .await;
    let _ = drain(&mut rx);

    let mut trimmed = false;

    // Turn 1: real tool work.
    dispatcher
        .process_line_for_test(
            &json!({
                "jsonrpc": "2.0", "id": 10, "method": "session/prompt",
                "params": {
                    "session_id": sid,
                    "prompt": "Write the single word zephyr into tmp/secret.md using the \
                               file_write tool. Confirm the path."
                }
            })
            .to_string(),
        )
        .await;
    for n in await_turn(&mut rx, 120).await {
        eprintln!("[turn1] {n}");
        if is_history_trimmed(&n) {
            trimmed = true;
        }
    }

    // Filler turns to push the earlier tool result toward the budget edge.
    for turn in 0..4 {
        dispatcher
            .process_line_for_test(
                &json!({
                    "jsonrpc": "2.0", "id": 20 + turn, "method": "session/prompt",
                    "params": {"session_id": sid, "prompt": format!("Filler turn {turn}: reply with one sentence.")}
                })
                .to_string(),
            )
            .await;
        for n in await_turn(&mut rx, 120).await {
            eprintln!("[filler {turn}] {n}");
            if is_history_trimmed(&n) {
                trimmed = true;
            }
        }
    }

    // Final turn: ask the model to recall the tool result.
    dispatcher
        .process_line_for_test(
            &json!({
                "jsonrpc": "2.0", "id": 99, "method": "session/prompt",
                "params": {"session_id": sid, "prompt": "Without using any tool, what exact word did you write into tmp/secret.md earlier?"}
            })
            .to_string(),
        )
        .await;
    let final_events = await_turn(&mut rx, 120).await;
    for n in &final_events {
        eprintln!("[final] {n}");
        if is_history_trimmed(n) {
            trimmed = true;
        }
    }

    let final_text = final_events
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
        // Budget forced a trim and the user WAS told (event fired). Recall
        // failing is then explainable and acceptable. Pass.
        return;
    }

    assert!(
        final_text.contains("zephyr"),
        "no history_trimmed event fired, yet the model could not recall the \
         tool result it wrote earlier (got: {final_text:?}). The tool context \
         was dropped silently."
    );
}
