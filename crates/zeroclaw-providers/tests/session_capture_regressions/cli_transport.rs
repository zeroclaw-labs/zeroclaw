use super::*;
use serde_json::Value;
use std::os::unix::fs::PermissionsExt;

fn fixture(kind: &str, mode: &str) -> (tempfile::TempDir, Box<dyn ModelProvider>) {
    let dir = tempfile::TempDir::new().expect("CLI fixture");
    let binary = dir.path().join("provider.sh");
    std::fs::write(&binary, include_str!("fixtures/plain-cli-capture.sh")).expect("script");
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).expect("executable");
    std::fs::write(dir.path().join("mode"), format!("{mode}\n")).expect("mode");
    let provider: Box<dyn ModelProvider> = if kind == "gemini" {
        Box::new(
            zeroclaw_providers::gemini_cli::GeminiCliModelProvider::builder("audit")
                .binary_path(binary.to_str())
                .build(),
        )
    } else {
        Box::new(
            zeroclaw_providers::kilocli::KiloCliModelProvider::builder("audit")
                .binary_path(binary.to_str())
                .build(),
        )
    };
    (dir, provider)
}

async fn chat(
    h: &Harness,
    provider: &dyn ModelProvider,
    message: &str,
    legacy: bool,
) -> anyhow::Result<String> {
    scope_provider_capture(h.admission.clone(), async {
        let dispatch = ProviderDispatch::from_ref(provider);
        if legacy {
            dispatch
                .chat_with_system(None, message, "default", None)
                .await
        } else {
            let messages = [ChatMessage::user(message)];
            let response = dispatch
                .chat(
                    ChatRequest {
                        messages: &messages,
                        tools: None,
                        thinking: None,
                    },
                    "default",
                    None,
                )
                .await?;
            assert!(
                response.usage.is_none(),
                "plain CLI reports no token counts"
            );
            Ok(response.text.unwrap_or_default())
        }
    })
    .await
}

fn read_bytes(store: &dyn SessionEventStore, reference: &StoredPayloadRef) -> Vec<u8> {
    match store.read_payload(&reference.payload_id).expect("payload") {
        PayloadRead::Present(record) => record.body,
        _ => panic!("present payload"),
    }
}

fn response_text(h: &Harness, backend: &str) -> (String, Option<ProviderDiagnostic>) {
    let store = open_event_store(h._dir.path(), backend).expect("reopen");
    let events = h.events();
    let (response_ref, diagnostic) = match &events.last().expect("terminal").body.data {
        EventData::ProviderAttemptCompleted { response_ref, .. } => (response_ref, None),
        EventData::InvocationOutcomeUnknown {
            observed_partial_ref: Some(reference),
            error_ref: Some(error_ref),
            ..
        } => (
            reference,
            Some(
                serde_json::from_slice::<ProviderDiagnostic>(&read_bytes(&*store, error_ref))
                    .expect("diagnostic"),
            ),
        ),
        other => panic!("unexpected terminal {other:?}"),
    };
    let response: ProviderResponsePayload =
        serde_json::from_slice(&read_bytes(&*store, response_ref)).expect("response");
    let text = response
        .blocks
        .iter()
        .filter_map(|block| match block {
            ResponseBlock::Text { text_ref } => {
                Some(String::from_utf8(read_bytes(&*store, text_ref)).expect("text"))
            }
            _ => None,
        })
        .collect::<String>();
    (text, diagnostic)
}

#[tokio::test]
async fn cli_success_and_abnormal_exits_preserve_received_output_both_backends() {
    for backend in ["sqlite", "jsonl"] {
        for kind in ["gemini", "kilo"] {
            for mode in [
                "success",
                "fail",
                "signal",
                "invalid_utf8",
                "truncated_utf8",
                "large_stderr",
                "blocked_stdin",
                "early_exit",
            ] {
                for legacy in [false, true] {
                    let (_dir, provider) = fixture(kind, mode);
                    let h = Harness::new(backend);
                    let message = if matches!(mode, "blocked_stdin" | "early_exit") {
                        "y".repeat(262144)
                    } else {
                        "fixture".into()
                    };
                    let result = tokio::time::timeout(
                        std::time::Duration::from_secs(10),
                        chat(&h, &*provider, &message, legacy),
                    )
                    .await
                    .expect("bounded CLI execution");
                    let (text, diagnostic) = response_text(&h, backend);
                    if matches!(mode, "success" | "blocked_stdin") {
                        assert_eq!(result.expect("success"), text.trim());
                        assert_eq!(text.matches("CLI_PARTIAL").count(), 1);
                        assert!(text.ends_with(" \u{4e2d}\u{6587} CLI_FINAL\n"));
                        assert!(diagnostic.is_none());
                        let response_ref = match &h.events().last().unwrap().body.data {
                            EventData::ProviderAttemptCompleted { response_ref, .. } => {
                                response_ref.clone()
                            }
                            _ => unreachable!(),
                        };
                        let response: ProviderResponsePayload = h.payload(&response_ref);
                        let finish: Value = h.payload(
                            response
                                .finish_evidence_ref
                                .as_ref()
                                .expect("actual finish"),
                        );
                        assert_eq!(finish["exit_code"], 0);
                        assert_eq!(finish["success"], true);
                    } else {
                        let error = result.expect_err("abnormal exit");
                        assert!(!error.to_string().is_empty());
                        assert_eq!(text, "CLI_PARTIAL");
                        let diagnostic = diagnostic.expect("diagnostic");
                        let key = if mode.ends_with("utf8") {
                            "provider_decode_error"
                        } else {
                            "provider_process_exit"
                        };
                        assert_eq!(diagnostic.error_key, key, "{backend}/{kind}/{mode}");
                        let DiagnosticDetail::Available { detail_ref } = diagnostic.detail else {
                            panic!("detail")
                        };
                        let detail = String::from_utf8(read_bytes(&*h.store, &detail_ref))
                            .expect("detail text");
                        if mode == "fail" {
                            assert!(detail.contains("fixture-secret"));
                            assert!(detail.contains("fixture process error"));
                        }
                    }
                }
            }
        }
    }
}

#[tokio::test]
async fn cli_stdout_is_durable_before_child_exit_both_backends() {
    for backend in ["sqlite", "jsonl"] {
        for kind in ["gemini", "kilo"] {
            let (dir, provider) = fixture(kind, "live");
            let h = Harness::new(backend);
            let mut call = Box::pin(chat(&h, &*provider, "fixture", false));
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    tokio::select! {
                        result = &mut call => panic!("child completed before release: {result:?}"),
                        _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {},
                    }
                    let mut received = String::new();
                    let store =
                        open_event_store(h._dir.path(), backend).expect("independent reader");
                    for event in h.events() {
                        if let EventData::ProviderResponseProgressObserved { observation_ref } =
                            event.body.data
                        {
                            let observation: ProviderObservationPayload =
                                h.payload(&observation_ref);
                            for operation in observation.operations {
                                if let ProviderObservation::BytesAppended { bytes_ref, .. } =
                                    operation
                                {
                                    received.push_str(
                                        &String::from_utf8(read_bytes(&*store, &bytes_ref))
                                            .expect("durable text"),
                                    );
                                }
                            }
                        }
                    }
                    if received.contains("CLI_PARTIAL") {
                        break;
                    }
                }
            })
            .await
            .expect("progress must precede child exit");
            std::fs::write(dir.path().join("release"), b"release").expect("release");
            assert!(call.await.expect("success").contains("CLI_FINAL"));
        }
    }
}

#[tokio::test]
async fn cli_split_credentials_preserve_durable_content_both_backends() {
    for backend in ["sqlite", "jsonl"] {
        for kind in ["gemini", "kilo"] {
            let (_dir, provider) = fixture(kind, "split_credentials");
            let h = Harness::new(backend);
            chat(&h, &*provider, "fixture", false)
                .await
                .expect("CLI completion");
            let (text, _) = response_text(&h, backend);
            assert!(
                text.contains("api_key=\"fixture-secret\""),
                "raw content in {text:?}"
            );
            assert!(
                text.contains("sk-fixture-secret"),
                "raw content in {text:?}"
            );
            assert!(text.contains("CLI_PARTIAL"));
            assert!(text.contains("CLI_FINAL"));
            assert!(text.contains("example.invalid/path"));
        }
    }
}

struct FailProgress(Arc<dyn zeroclaw_api::session_events::capture::ProviderAdmissionPort>);

#[async_trait::async_trait]
impl zeroclaw_api::session_events::capture::ProviderAdmissionPort for FailProgress {
    async fn admit(
        &self,
        intent: zeroclaw_api::session_events::capture::ProviderDispatchIntent,
    ) -> anyhow::Result<Arc<dyn zeroclaw_api::session_events::capture::ProviderObservationPort>>
    {
        struct Port(Arc<dyn zeroclaw_api::session_events::capture::ProviderObservationPort>);
        #[async_trait::async_trait]
        impl zeroclaw_api::session_events::capture::ProviderObservationPort for Port {
            fn started_event_id(&self) -> EventId {
                self.0.started_event_id()
            }
            async fn observe(
                &self,
                data: EventData,
                payloads: Vec<PayloadRecord>,
            ) -> anyhow::Result<AppendReceipt> {
                if matches!(data, EventData::ProviderResponseProgressObserved { .. }) {
                    anyhow::bail!("fixture_cli_capture_failure");
                }
                self.0.observe(data, payloads).await
            }
        }
        Ok(Arc::new(Port(self.0.admit(intent).await?)))
    }
}

#[tokio::test]
async fn cli_capture_failure_stops_and_reaps_owned_child_both_backends() {
    for backend in ["sqlite", "jsonl"] {
        for kind in ["gemini", "kilo"] {
            let (dir, provider) = fixture(kind, "live");
            let mut h = Harness::new(backend);
            h.admission = Arc::new(FailProgress(h.admission.clone()));
            let error = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                chat(&h, &*provider, "fixture", false),
            )
            .await
            .expect("capture failure must stop child")
            .expect_err("capture error");
            assert!(zeroclaw_api::session_events::capture::is_capture_stopped(
                &error
            ));
            assert!(format!("{error:#}").contains("fixture_cli_capture_failure"));
            let pid: i32 = std::fs::read_to_string(dir.path().join("pid"))
                .expect("PID")
                .trim()
                .parse()
                .expect("numeric PID");
            assert_eq!(
                nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None),
                Err(nix::errno::Errno::ESRCH)
            );
            assert!(
                !h.events()
                    .iter()
                    .any(|event| event.body.kind() == EventKind::ProviderAttemptCompleted)
            );
        }
    }
}
