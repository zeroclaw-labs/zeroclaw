//! Compatibility bridge to the extracted per-call approval gate.
use super::context::TurnCtx;
use super::events::StreamDelta;
use super::redact::scrub_credentials;
pub(crate) use zeroclaw_kernel::approval_gate::ApprovalGateOutcome;
use zeroclaw_kernel::approval_gate::{ApprovalContext, ApprovalProgressPort};

struct Progress<'a>(&'a tokio::sync::mpsc::Sender<StreamDelta>);
#[async_trait::async_trait]
impl ApprovalProgressPort for Progress<'_> {
    async fn flush_narration(&self) {
        let (barrier, ack) = StreamDelta::flush_barrier();
        if self.0.send(barrier).await.is_ok() {
            let _ = ack.await;
        }
    }
    async fn status(&self, status: String) {
        let _ = self.0.send(StreamDelta::Status(status)).await;
    }
}

pub(crate) async fn gate_tool_approval(
    ctx: &TurnCtx<'_>,
    tool_name: &str,
    tool_args: &serde_json::Value,
    iteration: usize,
    position: zeroclaw_api::channel::ApprovalPosition,
) -> ApprovalGateOutcome {
    let progress = ctx.on_delta.map(Progress);
    let prompt_cli = |request: &crate::approval::ApprovalRequest| {
        ctx.approval
            .ok_or_else(|| std::io::Error::other("approval manager absent"))?
            .prompt_cli(request)
    };
    let render_arguments_summary = |arguments: &serde_json::Value| {
        let summary = crate::approval::summarize_args(arguments);
        match (ctx.channel_name, ctx.agent_alias, ctx.parent_agent_alias) {
            ("delegate", Some(target), Some(caller)) => {
                let attribution = crate::i18n::get_required_cli_string_with_args(
                    "channel-approval-independent-delegate",
                    &[
                        ("target", &format!("{target:?}")),
                        ("caller", &format!("{caller:?}")),
                    ],
                );
                format!("{attribution}\n{summary}")
            }
            _ => summary,
        }
    };
    let context = ApprovalContext {
        approval: ctx.approval.map(|manager| &**manager),
        channel: ctx.channel,
        channel_name: ctx.channel_name,
        channel_reply_target: ctx.channel_reply_target,
        cancellation_token: ctx.cancellation_token,
        progress: progress
            .as_ref()
            .map(|port| port as &dyn ApprovalProgressPort),
        model: ctx.model,
        turn_id: ctx.turn_id,
        prompt_cli: &prompt_cli,
        render_arguments_summary: &render_arguments_summary,
        scrub_credentials,
    };
    zeroclaw_kernel::approval_gate::gate_tool_approval(
        &context, tool_name, tool_args, iteration, position,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::{ApprovalGateOutcome, gate_tool_approval};
    use crate::agent::turn::context::TurnCtx;
    use crate::approval::ApprovalManager;
    use crate::observability::NoopObserver;
    use crate::rpc::approval_channel::RpcApprovalChannel;
    use crate::rpc::context::ApprovalPendingMap;
    use crate::security::AutonomyLevel;
    use async_trait::async_trait;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;
    use zeroclaw_api::attribution::{Attributable, ChannelKind, Role};
    use zeroclaw_api::channel::{
        Channel, ChannelApprovalRequest, ChannelApprovalResponse, ChannelMessage, SendMessage,
    };
    use zeroclaw_api::jsonrpc::RpcOutbound;
    use zeroclaw_config::schema::{PacingConfig, RiskProfileConfig, StreamReasoningMode};

    fn full_always_ask_profile() -> RiskProfileConfig {
        RiskProfileConfig {
            level: AutonomyLevel::Full,
            always_ask: vec![" shell ".into()],
            ..RiskProfileConfig::default()
        }
    }

    fn test_ctx<'a>(
        observer: &'a NoopObserver,
        pacing: &'a PacingConfig,
        approval: Option<&'a ApprovalManager>,
        channel: Option<&'a dyn Channel>,
    ) -> TurnCtx<'a> {
        TurnCtx {
            parent_agent_alias: None,
            observer,
            provider_name: "stub",
            model: "stub-model",
            context_limits: zeroclaw_config::schema::ResolvedContextLimits::legacy_fallback(0),
            temperature: None,
            approval,
            channel_name: "test",
            channel_reply_target: Some("operator"),
            cancellation_token: None,
            on_delta: None,
            event_tx: None,
            hooks: None,
            dedup_exempt_tools: &[],
            pacing,
            strict_tool_parsing: false,
            channel,
            agent_alias: None,
            draft_reasoning: zeroclaw_config::schema::StreamReasoningMode::Status,
            turn_id: "trace-approval-gate",
            serving_provider_name: None,
            serving_model: None,
        }
    }

    // Re-execute this test in a new session so /dev/tty is genuinely unavailable.
    // Only the child owns stdin and the process-global log subscriber.
    #[cfg(unix)]
    #[tokio::test]
    async fn cli_approval_input_provenance_regression_11335() {
        use std::io::Write;
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};

        const CHILD_MODE: &str = "ZEROCLAW_TEST_11335_APPROVAL_CHILD";
        const TEST_NAME: &str =
            "agent::turn::approval_gate::tests::cli_approval_input_provenance_regression_11335";

        let Ok(mode) = std::env::var(CHILD_MODE) else {
            let mut failures = Vec::new();
            for (mode, input) in [
                ("no", Some("n\n")),
                ("blank", Some("\n")),
                ("unknown", Some("maybe\n")),
                ("yes", Some("y\n")),
                ("always", Some("always\n")),
                ("eof", None),
                ("read_error", None),
            ] {
                let mut command = Command::new(std::env::current_exe().unwrap());
                command
                    .args(["--exact", TEST_NAME, "--nocapture", "--test-threads=1"])
                    .env(CHILD_MODE, mode)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped());
                // SAFETY: setsid is async-signal-safe; no Rust locks or allocations
                // are used in the post-fork hook before exec.
                unsafe {
                    command.pre_exec(|| {
                        if libc::setsid() == -1 {
                            Err(std::io::Error::last_os_error())
                        } else {
                            Ok(())
                        }
                    });
                }
                if mode == "read_error" {
                    // Reading a directory as stdin produces a real I/O error.
                    command.stdin(Stdio::from(std::fs::File::open("/").unwrap()));
                } else if input.is_some() {
                    command.stdin(Stdio::piped());
                } else {
                    command.stdin(Stdio::null());
                }
                let mut child = command.spawn().unwrap();
                if let Some(input) = input {
                    child
                        .stdin
                        .take()
                        .unwrap()
                        .write_all(input.as_bytes())
                        .unwrap();
                }
                let output = child.wait_with_output().unwrap();
                if !output.status.success() {
                    failures.push(format!(
                        "{mode}: {}\n{}\n{}",
                        output.status,
                        String::from_utf8_lossy(&output.stdout),
                        String::from_utf8_lossy(&output.stderr)
                    ));
                } else {
                    print!("{}", String::from_utf8_lossy(&output.stdout));
                    println!("CLI approval control {mode}: passed");
                }
            }
            assert!(failures.is_empty(), "{}", failures.join("\n"));
            return;
        };

        assert!(std::fs::File::open("/dev/tty").is_err());
        zeroclaw_log::try_install_capture_subscriber();
        let mut log_events = zeroclaw_log::subscribe_or_install();
        let observer = NoopObserver;
        let pacing = PacingConfig::default();
        let profile = RiskProfileConfig {
            level: AutonomyLevel::Supervised,
            auto_approve: Vec::new(),
            always_ask: Vec::new(),
            ..RiskProfileConfig::default()
        };
        let approval = ApprovalManager::from_risk_profile(&profile);
        assert!(approval.needs_approval("shell"));
        let ctx = TurnCtx {
            channel_name: "cli",
            ..test_ctx(&observer, &pacing, Some(&approval), None)
        };
        let outcome = gate_tool_approval(
            &ctx,
            "shell",
            &serde_json::json!({"command": "true"}),
            0,
            zeroclaw_api::channel::ApprovalPosition { index: 1, total: 1 },
        )
        .await;
        let unavailable = mode == "eof" || mode == "read_error";
        if mode == "yes" || mode == "always" {
            assert!(matches!(
                outcome,
                ApprovalGateOutcome::Proceed { approved: true }
            ));
        } else {
            let ApprovalGateOutcome::Deny(outcome) = outcome else {
                panic!("{mode} must remain fail-closed");
            };
            assert!(!outcome.success);
            if unavailable {
                assert!(
                    outcome
                        .output
                        .contains("no operator decision was available"),
                    "{mode} was incorrectly attributed: {}",
                    outcome.output
                );
                assert!(outcome.output.contains("This was not a user's decision."));
                assert!(!outcome.output.contains("Denied by user"));
            } else {
                assert!(outcome.output.starts_with("Denied by user."));
            }
            let mut rejection = None;
            while let Ok(event) = log_events.try_recv() {
                if event["attributes"]["tool"] == "shell"
                    && event["attributes"].get("denied_by_runtime").is_some()
                {
                    rejection = Some(event);
                }
            }
            let rejection = rejection.expect("real approval rejection audit event");
            assert_eq!(rejection["attributes"]["denied_by_runtime"], unavailable);
            assert_eq!(
                rejection["attributes"]["operator_hint"].is_string(),
                unavailable
            );
            assert_eq!(rejection["attributes"]["result"], outcome.output);
            println!("CLI approval observed {mode}: {}", rejection["attributes"]);
        }
        let expected_decision = match mode.as_str() {
            "yes" => crate::approval::ApprovalResponse::Yes,
            "always" => crate::approval::ApprovalResponse::Always,
            _ => crate::approval::ApprovalResponse::No,
        };
        let decisions = approval.audit_log();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].decision, expected_decision);
        assert_eq!(
            approval.session_allowlist().contains("shell"),
            mode == "always"
        );
    }

    struct ApprovingChannel {
        approval_requests: Arc<AtomicUsize>,
    }

    impl Attributable for ApprovingChannel {
        fn role(&self) -> Role {
            Role::Channel(ChannelKind::AcpChannel)
        }
        fn alias(&self) -> &str {
            "approving-test"
        }
    }

    #[async_trait]
    impl Channel for ApprovingChannel {
        fn name(&self) -> &str {
            "approving-test"
        }

        async fn send(&self, _message: &SendMessage) -> anyhow::Result<()> {
            Ok(())
        }

        async fn listen(
            &self,
            _tx: tokio::sync::mpsc::Sender<ChannelMessage>,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn request_approval(
            &self,
            _recipient: &str,
            _request: &ChannelApprovalRequest,
        ) -> anyhow::Result<Option<ChannelApprovalResponse>> {
            self.approval_requests.fetch_add(1, Ordering::SeqCst);
            Ok(Some(ChannelApprovalResponse::Approve))
        }
    }

    #[tokio::test]
    async fn full_always_ask_fail_closed_without_channel_does_not_execute() {
        let observer = NoopObserver;
        let pacing = PacingConfig::default();
        let profile = full_always_ask_profile();
        let approval = ApprovalManager::for_non_interactive(&profile);
        let ctx = test_ctx(&observer, &pacing, Some(&approval), None);

        match gate_tool_approval(
            &ctx,
            "shell",
            &serde_json::json!({"command": "ls"}),
            0,
            zeroclaw_api::channel::ApprovalPosition { index: 1, total: 1 },
        )
        .await
        {
            ApprovalGateOutcome::Deny(outcome) => {
                assert!(!outcome.success);
                assert!(
                    outcome.output.contains("requires approval"),
                    "plain non-interactive Full+always_ask must fail closed, got {}",
                    outcome.output
                );
            }
            ApprovalGateOutcome::Proceed { approved } => {
                panic!("listed Full tool must not silently execute (approved={approved})")
            }
            ApprovalGateOutcome::Replace(_) => panic!("listed Full tool must not be replaced"),
            ApprovalGateOutcome::Cancelled => panic!("unexpected cancellation"),
            ApprovalGateOutcome::Error(error) => panic!("unexpected capture failure: {error}"),
        }

        match gate_tool_approval(
            &ctx,
            "file_write",
            &serde_json::json!({"path": "x"}),
            0,
            zeroclaw_api::channel::ApprovalPosition { index: 1, total: 1 },
        )
        .await
        {
            ApprovalGateOutcome::Proceed { approved: true } => {}
            ApprovalGateOutcome::Proceed { approved: false } => {
                panic!("uncovered Full tool must still auto-approve")
            }
            ApprovalGateOutcome::Deny(_)
            | ApprovalGateOutcome::Replace(_)
            | ApprovalGateOutcome::Cancelled
            | ApprovalGateOutcome::Error(_) => {
                panic!("uncovered Full tool must still auto-approve")
            }
        }
    }

    #[tokio::test]
    async fn full_always_ask_backchannel_requests_approval_and_uncovered_still_executes() {
        let observer = NoopObserver;
        let pacing = PacingConfig::default();
        let profile = full_always_ask_profile();
        let approval = ApprovalManager::for_non_interactive_backchannel(&profile);
        let requests = Arc::new(AtomicUsize::new(0));
        let channel = ApprovingChannel {
            approval_requests: Arc::clone(&requests),
        };
        let ctx = test_ctx(&observer, &pacing, Some(&approval), Some(&channel));

        match gate_tool_approval(
            &ctx,
            "shell",
            &serde_json::json!({"command": "ls"}),
            0,
            zeroclaw_api::channel::ApprovalPosition { index: 1, total: 1 },
        )
        .await
        {
            ApprovalGateOutcome::Proceed { approved: true } => {}
            ApprovalGateOutcome::Proceed { approved: false } => {
                panic!("back-channel approval must mark the listed tool approved")
            }
            ApprovalGateOutcome::Deny(outcome) => {
                panic!(
                    "back-channel approval must permit the listed tool, got {}",
                    outcome.output
                )
            }
            ApprovalGateOutcome::Replace(_) => panic!("unexpected replace"),
            ApprovalGateOutcome::Cancelled => panic!("unexpected cancellation"),
            ApprovalGateOutcome::Error(error) => panic!("unexpected capture failure: {error}"),
        }
        assert_eq!(
            requests.load(Ordering::SeqCst),
            1,
            "listed Full tool must go through the real back-channel request path"
        );

        match gate_tool_approval(
            &ctx,
            "file_write",
            &serde_json::json!({"path": "x"}),
            0,
            zeroclaw_api::channel::ApprovalPosition { index: 1, total: 1 },
        )
        .await
        {
            ApprovalGateOutcome::Proceed { approved: true } => {}
            ApprovalGateOutcome::Proceed { approved: false }
            | ApprovalGateOutcome::Deny(_)
            | ApprovalGateOutcome::Replace(_)
            | ApprovalGateOutcome::Cancelled
            | ApprovalGateOutcome::Error(_) => {
                panic!("uncovered Full tool must still auto-approve")
            }
        }
        assert_eq!(
            requests.load(Ordering::SeqCst),
            1,
            "uncovered Full tool must not prompt the back-channel"
        );
    }

    #[tokio::test]
    async fn cancelling_turn_drops_pending_channel_approval() {
        let (writer_tx, mut writer_rx) = mpsc::channel::<String>(4);
        let rpc = Arc::new(RpcOutbound::new(writer_tx));
        let pending = Arc::new(ApprovalPendingMap::default());
        let channel = RpcApprovalChannel::new(
            "rpc",
            "session-approval",
            rpc,
            Arc::clone(&pending),
            Default::default(),
        );
        let approval =
            ApprovalManager::for_non_interactive_backchannel(&RiskProfileConfig::default());
        let observer = NoopObserver;
        let pacing = PacingConfig::default();
        let cancel = CancellationToken::new();
        let ctx = TurnCtx {
            observer: &observer,
            provider_name: "test",
            model: "test-model",
            context_limits: zeroclaw_config::schema::ResolvedContextLimits::legacy_fallback(0),
            temperature: None,
            approval: Some(&approval),
            channel_name: "rpc",
            channel_reply_target: Some("operator"),
            cancellation_token: Some(&cancel),
            on_delta: None,
            event_tx: None,
            hooks: None,
            dedup_exempt_tools: &[],
            pacing: &pacing,
            strict_tool_parsing: false,
            channel: Some(&channel),
            draft_reasoning: StreamReasoningMode::Status,
            turn_id: "turn-approval",
            agent_alias: Some("default"),
            parent_agent_alias: None,
            serving_provider_name: None,
            serving_model: None,
        };

        let arguments = serde_json::json!({"command": "sleep 60"});
        let approval_wait = gate_tool_approval(
            &ctx,
            "shell",
            &arguments,
            0,
            zeroclaw_api::channel::ApprovalPosition { index: 1, total: 1 },
        );
        tokio::pin!(approval_wait);
        let line = tokio::select! {
            outcome = &mut approval_wait => panic!("approval completed before cancellation: {}", matches!(outcome, ApprovalGateOutcome::Cancelled)),
            line = writer_rx.recv() => line.expect("approval request notification"),
        };
        let frame: serde_json::Value = serde_json::from_str(&line).expect("valid notification");
        let request_id = frame["params"]["request_id"]
            .as_str()
            .expect("approval request id")
            .to_string();
        assert!(pending.contains(&request_id));

        cancel.cancel();
        assert!(matches!(
            approval_wait.await,
            ApprovalGateOutcome::Cancelled
        ));
        assert!(
            !pending.contains(&request_id),
            "cancelling the turn must drop the stale approval responder"
        );
    }
}
