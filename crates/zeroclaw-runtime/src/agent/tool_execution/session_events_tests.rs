use super::tests::{FailingToolReturningErr, test_turn_meta};
use super::*;
use zeroclaw_api::session_events::capture::*;
use zeroclaw_api::session_events::*;

#[derive(Default)]
struct CaptureFixture(std::sync::Mutex<Vec<(EventData, Vec<PayloadRecord>)>>);
#[async_trait::async_trait]
impl ToolObservationPort for CaptureFixture {
    fn started_event_id(&self) -> EventId {
        EventId::from("fixture-start")
    }
    async fn observe(
        &self,
        data: EventData,
        payloads: Vec<PayloadRecord>,
    ) -> anyhow::Result<AppendReceipt> {
        self.0.lock().expect("observations").push((data, payloads));
        let stream = SessionStreamId::from("fixture-stream");
        Ok(AppendReceipt {
            first: EventCursor {
                stream_id: stream.clone(),
                sequence: 1,
            },
            last: EventCursor {
                stream_id: stream,
                sequence: 1,
            },
            event_ids: vec![EventId::new()],
            head_hash: Digest::zero(),
        })
    }
}
struct Preparation(Arc<CaptureFixture>);
#[async_trait::async_trait]
impl PreparedToolPort for Preparation {
    async fn verify_action(&self, _: &str, _: &serde_json::Value) -> anyhow::Result<()> {
        Ok(())
    }
    async fn request_approval(
        &self,
        _: zeroclaw_api::session_events::capture::ApprovalRequestIntent,
    ) -> anyhow::Result<()> {
        anyhow::bail!("unexpected fixture approval")
    }
    async fn resolve_approval(
        &self,
        _: zeroclaw_api::channel::ChannelApprovalResponse,
        _: &str,
        _: Option<zeroclaw_api::session_events::EventId>,
        _: serde_json::Value,
    ) -> anyhow::Result<()> {
        anyhow::bail!("unexpected fixture approval")
    }
    async fn invalidate(&self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn start(&self) -> anyhow::Result<Arc<dyn ToolObservationPort>> {
        Ok(self.0.clone())
    }
}
struct Admission;
#[async_trait::async_trait]
impl ToolAdmissionPort for Admission {
    async fn prepare(&self, _: ToolPreparationIntent) -> anyhow::Result<Arc<dyn PreparedToolPort>> {
        anyhow::bail!("fixture already prepared")
    }
}

#[tokio::test]
async fn uncertain_adapter_error_stops_dispatch_after_recording_diagnostic() {
    let captured = Arc::new(CaptureFixture::default());
    let prepared: Arc<dyn PreparedToolPort> = Arc::new(Preparation(captured.clone()));
    let tools: Vec<Box<dyn Tool>> = vec![Box::new(FailingToolReturningErr {
        message: "fixture connection lost token=sk-abcdefgh1234567890",
    })];
    let meta = test_turn_meta();
    let observer = crate::observability::NoopObserver;
    let result = scope_tool_capture(
        Arc::new(Admission),
        scope_tool_batch(
            vec![("fixture-call".into(), prepared)],
            execute_one_tool(
                "failing_tool_returning_err",
                serde_json::json!({}),
                Some("fixture-call"),
                ToolDispatchContext {
                    tools_registry: &tools,
                    activated_tools: None,
                    excluded_tools: &[],
                    model_switch_callback: None,
                },
                &meta,
                &observer,
                None,
                None,
                None,
            ),
        ),
    )
    .await;
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("uncertainty cannot become a recoverable model-visible failure"),
    };
    assert!(is_capture_stopped(&error));
    let observations = captured.0.lock().expect("observations");
    assert_eq!(observations.len(), 1);
    let (
        EventData::InvocationOutcomeUnknown {
            error_ref: Some(error_ref),
            ..
        },
        payloads,
    ) = &observations[0]
    else {
        panic!("unknown with diagnostic");
    };
    let diagnostic = payloads
        .iter()
        .find(|p| p.payload_id == error_ref.payload_id)
        .expect("diagnostic");
    let text = std::str::from_utf8(&diagnostic.body).expect("text");
    assert!(text.contains("fixture connection lost"));
    assert!(!text.contains("sk-abcdefgh1234567890"));
}

struct ContextCancellationProbe(Arc<std::sync::atomic::AtomicBool>);
zeroclaw_api::mock_tool_attribution!(ContextCancellationProbe);
#[async_trait::async_trait]
impl Tool for ContextCancellationProbe {
    fn name(&self) -> &str {
        "context_cancellation_probe"
    }
    fn description(&self) -> &str {
        "Checks the cancellation context at the captured tool boundary"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    async fn execute(&self, _: serde_json::Value) -> Result<crate::tools::ToolResult> {
        anyhow::bail!("context-aware entrypoint required")
    }
    async fn execute_with_context(
        &self,
        _: serde_json::Value,
        context: &ToolExecutionContext,
    ) -> Result<crate::tools::ToolResult> {
        assert!(ToolExecutionContext::current().is_some());
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        context.cancellation_token().expect("turn token").cancel();
        std::future::pending().await
    }
}

#[tokio::test]
async fn captured_tool_context_cancellation_preserves_unknown_outcome() {
    for pre_cancelled in [false, true] {
        let captured = Arc::new(CaptureFixture::default());
        let prepared: Arc<dyn PreparedToolPort> = Arc::new(Preparation(captured.clone()));
        let executed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(ContextCancellationProbe(executed.clone()))];
        let token = CancellationToken::new();
        if pre_cancelled {
            token.cancel();
        }
        let meta = test_turn_meta();
        let observer = crate::observability::NoopObserver;
        let result = scope_tool_capture(
            Arc::new(Admission),
            scope_tool_batch(
                vec![("fixture-call".into(), prepared)],
                execute_one_tool(
                    "context_cancellation_probe",
                    serde_json::json!({}),
                    Some("fixture-call"),
                    ToolDispatchContext {
                        tools_registry: &tools,
                        activated_tools: None,
                        excluded_tools: &[],
                        model_switch_callback: None,
                    },
                    &meta,
                    &observer,
                    Some(&token),
                    None,
                    None,
                ),
            ),
        )
        .await;
        let error = result.err().expect("captured call cancelled");
        assert!(is_tool_loop_cancelled(&error));
        assert_eq!(
            executed.load(std::sync::atomic::Ordering::SeqCst),
            !pre_cancelled,
        );
        let observations = captured.0.lock().expect("observations");
        assert_eq!(observations.len(), 1);
        assert!(matches!(
            &observations[0].0,
            EventData::InvocationOutcomeUnknown { reason_key, .. }
                if reason_key == "tool_wait_cancelled"
        ));
        assert!(ToolExecutionContext::current().is_none());
    }
}
