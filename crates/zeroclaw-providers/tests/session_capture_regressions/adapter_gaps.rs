use super::*;
use serde_json::{Value, json};
use zeroclaw_providers::{hailo_ollama::HailoOllamaModelProvider, ollama::OllamaTuning};

async fn server(path: &str, wire: Value, status: u16) -> (String, tokio::task::JoinHandle<()>) {
    let app = axum::Router::new().route(
        path,
        axum::routing::post(move || {
            let wire = wire.clone();
            async move {
                (
                    axum::http::StatusCode::from_u16(status).expect("status"),
                    [("x-request-id", "fixture-request"), ("retry-after", "7")],
                    axum::Json(wire),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let url = format!("http://{}", listener.local_addr().expect("address"));
    let handle = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app).await.expect("fixture server");
    });
    (url, handle)
}

async fn chat(h: &Harness, provider: &dyn ModelProvider) -> anyhow::Result<ChatResponse> {
    let messages = [ChatMessage::user("fixture")];
    scope_provider_capture(
        h.admission.clone(),
        ProviderDispatch::from_ref(provider).chat(
            ChatRequest {
                messages: &messages,
                tools: None,
                thinking: None,
            },
            "fixture",
            None,
        ),
    )
    .await
}

fn bytes(store: &dyn SessionEventStore, reference: &StoredPayloadRef) -> Vec<u8> {
    let PayloadRead::Present(payload) = store.read_payload(&reference.payload_id).expect("read")
    else {
        panic!("present payload");
    };
    payload.body
}

fn reopened(h: &Harness, backend: &str) -> Arc<dyn SessionEventStore> {
    open_event_store(h._dir.path(), backend).expect("reopen durable store")
}

fn completed(h: &Harness, backend: &str) -> ProviderResponsePayload {
    let events = h.events();
    let EventData::ProviderAttemptCompleted { response_ref, .. } =
        &events.last().expect("outcome").body.data
    else {
        panic!("completed attempt: {:?}", events.last());
    };
    serde_json::from_slice(&bytes(&*reopened(h, backend), response_ref)).expect("durable response")
}

async fn assert_reasoning_requires_disclosure(h: &Harness) {
    use zeroclaw_api::session_events::payloads::ResponseReadReference;
    use zeroclaw_api::session_events::payloads::response_references_for_read;
    use zeroclaw_kernel::session::query::PayloadSelector;
    let events = h.events();
    let completed = events.last().expect("outcome");
    let EventData::ProviderAttemptCompleted { response_ref, .. } = &completed.body.data else {
        panic!("completed attempt");
    };
    let references =
        response_references_for_read(&bytes(&*h.store, response_ref)).expect("references");
    let nested_index = references
        .references
        .iter()
        .position(|reference| matches!(reference, ResponseReadReference::Reasoning(_)))
        .expect("reasoning reference");
    let root_index = completed
        .body
        .data
        .stored_refs()
        .expect("root refs")
        .iter()
        .position(|reference| reference == response_ref)
        .expect("response index");
    let owner = Arc::new(SessionCoordinator::new(h.store.clone()));
    let query = owner.query(
        zeroclaw_api::principal::PrincipalId::shared_operator(),
        Arc::new(ReadAllow),
    );
    assert!(matches!(
        query
            .read_payload(
                h.stream.clone(),
                PayloadSelector {
                    event_sequence: completed.sequence,
                    reference_path: vec![root_index, nested_index],
                }
            )
            .await,
        Err(SessionError::PermissionDenied(_))
    ));
}

fn contents(
    store: &dyn SessionEventStore,
    response: &ProviderResponsePayload,
) -> Vec<(String, Vec<u8>)> {
    response
        .blocks
        .iter()
        .map(|block| match block {
            ResponseBlock::Text { text_ref } => ("text".into(), bytes(store, text_ref)),
            ResponseBlock::Reasoning { payload_ref, .. } => {
                ("reasoning".into(), bytes(store, payload_ref))
            }
            ResponseBlock::Other {
                type_name,
                classified_body_ref,
            } => (type_name.clone(), bytes(store, classified_body_ref)),
            ResponseBlock::Media {
                received: MediaReceived::Captured { representation_ref },
                ..
            } => ("media".into(), bytes(store, representation_ref)),
            other => panic!("unexpected block: {other:?}"),
        })
        .collect()
}

#[tokio::test]
async fn hailo_retains_reasoning_and_inline_order_both_backends() {
    for backend in ["sqlite", "jsonl"] {
        for (content, thinking, expected) in [
            (
                "answer",
                Some("FIELD_THOUGHT"),
                vec![("reasoning", "FIELD_THOUGHT"), ("text", "answer")],
            ),
            (
                "before<think>TAG_THOUGHT</think>answer",
                None,
                vec![
                    ("text", "before"),
                    ("reasoning", "TAG_THOUGHT"),
                    ("text", "answer"),
                ],
            ),
            (
                "answer<think>PARTIAL_THOUGHT",
                None,
                vec![
                    ("text", "answer"),
                    ("reasoning", "PARTIAL_THOUGHT"),
                    ("text", ""),
                ],
            ),
            (
                "",
                Some("FALLBACK_THOUGHT"),
                vec![("reasoning", "FALLBACK_THOUGHT"), ("text", "")],
            ),
        ] {
            let wire = json!({"model":"served-fixture","done":true,"done_reason":"stop","message":{"role":"assistant","content":content,"thinking":thinking}});
            let (url, server) = server("/api/chat", wire, 200).await;
            let provider =
                HailoOllamaModelProvider::new("audit", Some(&url), 5, 5, OllamaTuning::default())
                    .expect("provider");
            let h = Harness::new(backend);
            let response = chat(&h, &provider).await.expect("response");
            let expected_text = match content {
                "before<think>TAG_THOUGHT</think>answer" => "beforeanswer",
                "answer<think>PARTIAL_THOUGHT" => "answer",
                "" => "FALLBACK_THOUGHT",
                _ => "answer",
            };
            assert_eq!(response.text.as_deref(), Some(expected_text));
            let stored = completed(&h, backend);
            assert_reasoning_requires_disclosure(&h).await;
            assert!(stored.reasoning_absence.is_none());
            let received = contents(&*reopened(&h, backend), &stored);
            assert_eq!(
                received,
                expected
                    .into_iter()
                    .map(|(kind, text)| (kind.to_owned(), text.as_bytes().to_vec()))
                    .collect::<Vec<_>>()
            );
            server.abort();
            let _ = server.await;
        }
    }
}

#[tokio::test]
async fn hailo_http_rejections_keep_exact_evidence_both_backends() {
    for backend in ["sqlite", "jsonl"] {
        for status in [401, 429] {
            let wire = json!({"error":{"code":"fixture_rejection","message":"request rejected","api_key":"fixture-secret"}});
            let (url, server) = server("/api/chat", wire, status).await;
            let provider =
                HailoOllamaModelProvider::new("audit", Some(&url), 5, 5, OllamaTuning::default())
                    .expect("provider");
            let h = Harness::new(backend);
            let error = chat(&h, &provider).await.expect_err("HTTP rejection");
            assert!(format!("{error:#}").contains("fixture-secret"));
            let events = h.events();
            let EventData::ProviderAttemptFailed { error_ref, .. } =
                &events.last().expect("outcome").body.data
            else {
                panic!("definite HTTP failure");
            };
            let store = reopened(&h, backend);
            let diagnostic: ProviderDiagnostic =
                serde_json::from_slice(&bytes(&*store, error_ref)).expect("diagnostic");
            assert_eq!(diagnostic.error_key, "provider_http_error");
            assert!(diagnostic.protocol_evidence.iter().any(|evidence| matches!(evidence,
                ProtocolEvidence::Http { status_code, provider_error_code: Some(code), provider_request_id: Some(id), retry_after: Some(retry) }
                if *status_code == status && code == "fixture_rejection" && id == "fixture-request" && retry == "7"
            )), "{:?}", diagnostic.protocol_evidence);
            server.abort();
            let _ = server.await;
        }
    }
}

#[tokio::test]
async fn hailo_rejection_with_truncated_body_remains_definite_both_backends() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    for backend in ["sqlite", "jsonl"] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let url = format!("http://{}", listener.local_addr().expect("address"));
        let server = zeroclaw_spawn::spawn!(async move {
            let (mut socket, _) = listener.accept().await.expect("request");
            let mut request = [0_u8; 4096];
            assert!(socket.read(&mut request).await.expect("read request") > 0);
            socket.write_all(b"HTTP/1.1 429 Too Many Requests\r\nContent-Length: 200\r\nConnection: close\r\n\r\n{\"error\":").await.expect("partial response");
            socket.shutdown().await.expect("close peer");
        });
        let provider =
            HailoOllamaModelProvider::new("audit", Some(&url), 5, 5, OllamaTuning::default())
                .expect("provider");
        let h = Harness::new(backend);
        chat(&h, &provider).await.expect_err("HTTP rejection");
        let events = h.events();
        let EventData::ProviderAttemptFailed { error_ref, .. } =
            &events.last().expect("outcome").body.data
        else {
            panic!("definite rejection");
        };
        let diagnostic: ProviderDiagnostic =
            serde_json::from_slice(&bytes(&*reopened(&h, backend), error_ref)).expect("diagnostic");
        assert_eq!(diagnostic.error_key, "provider_http_error");
        assert!(diagnostic.protocol_evidence.iter().any(|evidence| matches!(
            evidence,
            ProtocolEvidence::Http {
                status_code: 429,
                ..
            }
        )));
        server.await.expect("peer exit");
    }
}

// ProjectDirs reads process environment. Isolate the real factory's cache path
// in a child test process instead of mutating global env or opening user files.
#[cfg(target_os = "linux")]
#[test]
fn copilot_capture_uses_isolated_credential_cache() {
    let config = tempfile::TempDir::new().expect("isolated config");
    let result = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args([
            "--exact",
            "adapter_gaps::copilot_capture_child",
            "--ignored",
            "--nocapture",
        ])
        .env("XDG_CONFIG_HOME", config.path())
        .env("ZEROCLAW_CAPTURE_FIXTURE_CONFIG", config.path())
        .output()
        .expect("child test");
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stdout).contains("1 passed"));
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "runs through the isolated-cache parent test"]
async fn copilot_capture_child() {
    use std::os::unix::fs::PermissionsExt;
    use zeroclaw_providers::copilot::CopilotModelProvider;
    let config =
        std::env::var_os("ZEROCLAW_CAPTURE_FIXTURE_CONFIG").expect("isolated child fixture");
    assert_eq!(std::env::var_os("XDG_CONFIG_HOME"), Some(config.clone()));
    let cache = std::path::PathBuf::from(config).join("zeroclaw/copilot");
    std::fs::create_dir_all(&cache).expect("cache");
    std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o700))
        .expect("private cache");
    for backend in ["sqlite", "jsonl"] {
        for field in ["reasoning_content", "reasoning"] {
            let mut message = json!({"role":"assistant","content":[{"type":"text","text":"answer"},{"type":"image_url","image_url":{"url":"data:image/png;base64,cG5n"}}],"audio":{"data":"YXVkaW8=","format":"wav"},"future":{"token":"fixture-secret","payload_id":"received-value"}});
            message[field] = json!("COPILOT_THOUGHT");
            let (url, server) = server("/chat/completions", json!({"model":"served-fixture","choices":[{"message":message,"finish_reason":"stop"}]}), 200).await;
            let file = cache.join("api-key.json");
            std::fs::write(&file, json!({"token":"synthetic-fixture-credential","expires_at":4102444800_i64,"endpoints":{"api":url}}).to_string()).expect("synthetic cache");
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600))
                .expect("private fixture");
            let provider = CopilotModelProvider::builder("audit").build();
            let h = Harness::new(backend);
            let response = chat(&h, &provider).await.expect("Copilot response");
            assert_eq!(response.text.as_deref(), Some("answer"));
            assert_eq!(
                response.reasoning_content.as_deref(),
                Some("COPILOT_THOUGHT")
            );
            let stored = completed(&h, backend);
            assert_reasoning_requires_disclosure(&h).await;
            let finish: Value = h.payload(
                stored
                    .finish_evidence_ref
                    .as_ref()
                    .expect("finish evidence"),
            );
            assert_eq!(finish["finish_reason"], "stop");
            assert!(h.events().iter().any(|event| {
                if let EventData::ProviderAttemptCompleted {
                    served_attribution_ref: Some(reference),
                    ..
                } = &event.body.data
                {
                    let served: Value = h.payload(reference);
                    served["model"] == "served-fixture"
                } else {
                    false
                }
            }));
            let store = reopened(&h, backend);
            let content = contents(&*store, &stored);
            assert_eq!(
                content
                    .iter()
                    .filter(|(kind, _)| kind == "reasoning")
                    .count(),
                1
            );
            assert_eq!(content.iter().filter(|(kind, _)| kind == "text").count(), 1);
            assert!(content.contains(&("media".into(), b"png".to_vec())));
            assert!(content.contains(&("media".into(), b"audio".to_vec())));
            let (_, opaque) = content
                .iter()
                .find(|(kind, _)| kind == "assistant.future")
                .expect("future extension");
            let opaque: Value = serde_json::from_slice(opaque).expect("opaque value");
            assert_eq!(opaque["token"], "fixture-secret");
            assert_eq!(opaque["payload_id"], "received-value");
            server.abort();
            let _ = server.await;
        }
    }
}

#[cfg(unix)]
fn grok_fixture(
    mode: &str,
) -> (
    tempfile::TempDir,
    zeroclaw_providers::grok_cli::GrokCliModelProvider,
) {
    use std::os::unix::fs::PermissionsExt;
    let work = tempfile::TempDir::new().expect("child working directory");
    let binary = work.path().join("grok-fixture.sh");
    std::fs::write(&binary, include_str!("fixtures/grok-capture.sh")).expect("peer fixture");
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).expect("executable");
    std::fs::write(work.path().join("mode"), format!("{mode}\n")).expect("fixture mode");
    let extra_args = if mode == "allow" {
        vec!["--permission-mode=bypassPermissions".into()]
    } else {
        vec![]
    };
    let provider = zeroclaw_providers::grok_cli::GrokCliModelProvider::builder("audit")
        .binary_path(binary.to_str())
        .extra_args(extra_args)
        .working_directory(work.path().to_str().expect("path"))
        .timeout_secs(Some(if mode == "slow_capture" { 1 } else { 5 }))
        .build()
        .expect("Grok provider");
    (work, provider)
}

#[cfg(unix)]
#[tokio::test]
async fn grok_captures_native_activity_and_interrupted_thoughts_both_backends() {
    for backend in ["sqlite", "jsonl"] {
        for mode in ["success", "allow", "crash"] {
            let (work, provider) = grok_fixture(mode);
            let h = Harness::new(backend);
            let result = chat(&h, &provider).await;
            let stored = if mode != "crash" {
                assert_eq!(result.expect("response").text.as_deref(), Some("ACP_FINAL"));
                let reply: Value = serde_json::from_slice(
                    &std::fs::read(work.path().join("permission-reply.json"))
                        .expect("permission response"),
                )
                .expect("permission JSON");
                assert_eq!(
                    reply["result"]["outcome"]["optionId"],
                    if mode == "allow" { "allow" } else { "reject" }
                );
                assert_reasoning_requires_disclosure(&h).await;
                completed(&h, backend)
            } else {
                assert!(result.is_err());
                let events = h.events();
                let EventData::InvocationOutcomeUnknown {
                    observed_partial_ref: Some(reference),
                    error_ref: Some(error_ref),
                    ..
                } = &events.last().expect("outcome").body.data
                else {
                    panic!("interrupted outcome with partial content");
                };
                let store = reopened(&h, backend);
                let diagnostic: ProviderDiagnostic =
                    serde_json::from_slice(&bytes(&*store, error_ref))
                        .expect("interruption diagnostic");
                assert_eq!(diagnostic.error_key, "provider_outcome_unresolved");
                let DiagnosticDetail::Available { detail_ref } = &diagnostic.detail else {
                    panic!("error detail");
                };
                assert!(
                    String::from_utf8(bytes(&*store, detail_ref))
                        .expect("error text")
                        .contains("closed before session/prompt completed")
                );
                assert_eq!(
                    events
                        .iter()
                        .filter(|event| event.body.kind() == EventKind::InvocationOutcomeUnknown)
                        .count(),
                    1
                );
                serde_json::from_slice(&bytes(&*store, reference))
                    .expect("durable partial response")
            };
            let store = reopened(&h, backend);
            let content = contents(&*store, &stored);
            assert_eq!(
                content
                    .iter()
                    .filter(|(kind, _)| kind == "reasoning")
                    .collect::<Vec<_>>(),
                vec![&("reasoning".into(), b"ACP_THOUGHT".to_vec())]
            );
            assert_eq!(
                content
                    .iter()
                    .filter(|(kind, _)| kind == "text")
                    .map(|(_, data)| data.as_slice())
                    .collect::<Vec<_>>(),
                if mode != "crash" {
                    vec![b"progress".as_slice(), b"ACP_FINAL".as_slice()]
                } else {
                    vec![b"progress".as_slice()]
                }
            );
            assert_eq!(
                content
                    .iter()
                    .filter(|(kind, _)| kind == "provider_tool_activity")
                    .count(),
                if mode != "crash" { 2 } else { 1 }
            );
            assert!(!format!("{content:?}").contains("FOREIGN_THOUGHT"));
            let (_, plan) = content
                .iter()
                .find(|(kind, _)| kind == "grok_acp.plan")
                .expect("plan");
            assert_eq!(
                serde_json::from_slice::<Value>(plan).expect("plan JSON")["token"],
                "fixture-secret"
            );
            if mode != "crash" {
                assert!(content.contains(&("media".into(), b"png".to_vec())));
                let media = stored
                    .blocks
                    .iter()
                    .find_map(|block| match block {
                        ResponseBlock::Media {
                            media_descriptor_ref,
                            ..
                        } => Some(media_descriptor_ref),
                        _ => None,
                    })
                    .expect("media descriptor");
                assert_eq!(h.payload::<Value>(media)["format"], "image/png");
                let metadata = h
                    .events()
                    .iter()
                    .filter_map(|event| {
                        let EventData::ProviderResponseProgressObserved { observation_ref } =
                            &event.body.data
                        else {
                            return None;
                        };
                        let observation: ProviderObservationPayload = h.payload(observation_ref);
                        Some(observation.operations)
                    })
                    .flatten()
                    .filter_map(|operation| {
                        if let ProviderObservation::MetadataObserved { metadata_ref } = operation {
                            Some(h.payload::<Value>(&metadata_ref))
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>();
                assert!(
                    metadata
                        .iter()
                        .any(|value| value["kind"] == "provider_permission_request"
                            && value["request_id"] == 50)
                );
                assert!(
                    metadata
                        .iter()
                        .any(|value| value["kind"] == "provider_permission_response"
                            && value["outcome"]["optionId"]
                                == if mode == "allow" { "allow" } else { "reject" })
                );
                assert!(
                    metadata
                        .iter()
                        .any(|value| value["kind"] == "provider_permission_response"
                            && value["phase"] == "sent"
                            && value["request_id"] == 50)
                );
            }
            assert!(
                !h.events()
                    .iter()
                    .any(|event| event.body.kind() == EventKind::ToolEffectStarted)
            );
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn grok_explicit_error_retains_diagnostic_and_partial_both_backends() {
    for backend in ["sqlite", "jsonl"] {
        let (_work, provider) = grok_fixture("remote_error");
        let h = Harness::new(backend);
        let error = chat(&h, &provider).await.expect_err("remote error");
        assert!(!format!("{error:#}").contains("fixture-secret"));
        assert!(!format!("{error:#}").contains("REMOTE_ERROR_DETAIL"));
        let events = h.events();
        let EventData::ProviderAttemptFailed {
            error_ref,
            partial_response_ref: Some(partial),
            ..
        } = &events.last().expect("terminal").body.data
        else {
            panic!("explicit failure with partial: {:?}", events.last());
        };
        assert!(
            !events
                .iter()
                .any(|event| event.body.kind() == EventKind::InvocationOutcomeUnknown)
        );
        let store = reopened(&h, backend);
        let diagnostic: ProviderDiagnostic =
            serde_json::from_slice(&bytes(&*store, error_ref)).expect("diagnostic");
        assert_eq!(diagnostic.error_key, "provider_protocol_error");
        assert!(
            diagnostic
                .protocol_evidence
                .iter()
                .any(|evidence| matches!(evidence,
            ProtocolEvidence::StreamTermination { evidence } if evidence.contains("-32042")))
        );
        let DiagnosticDetail::Available { detail_ref } = diagnostic.detail else {
            panic!("detail")
        };
        let detail: Value =
            serde_json::from_slice(&bytes(&*store, &detail_ref)).expect("error object");
        assert_eq!(detail["code"], -32042);
        assert_eq!(detail["message"], "REMOTE_ERROR_DETAIL");
        assert_eq!(detail["data"]["request_id"], "audit-request-42");
        assert_eq!(detail["data"]["api_key"], "fixture-secret");
        let partial: ProviderResponsePayload =
            serde_json::from_slice(&bytes(&*store, partial)).expect("partial");
        let content = contents(&*store, &partial);
        assert!(content.contains(&("reasoning".into(), b"ACP_THOUGHT".to_vec())));
        assert!(content.contains(&("text".into(), b"progress".to_vec())));
    }
}

#[cfg(unix)]
#[tokio::test]
async fn grok_deadline_preserves_inflight_durable_observation_both_backends() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use zeroclaw_api::session_events::capture::{
        ProviderAdmissionPort, ProviderDispatchIntent, ProviderObservationPort,
    };
    struct SlowAdmission {
        inner: Arc<dyn ProviderAdmissionPort>,
        completed: Arc<AtomicBool>,
    }
    struct SlowPort {
        inner: Arc<dyn ProviderObservationPort>,
        completed: Arc<AtomicBool>,
    }
    #[async_trait::async_trait]
    impl ProviderAdmissionPort for SlowAdmission {
        async fn admit(
            &self,
            intent: ProviderDispatchIntent,
        ) -> anyhow::Result<Arc<dyn ProviderObservationPort>> {
            Ok(Arc::new(SlowPort {
                inner: self.inner.admit(intent).await?,
                completed: self.completed.clone(),
            }))
        }
    }
    #[async_trait::async_trait]
    impl ProviderObservationPort for SlowPort {
        fn started_event_id(&self) -> EventId {
            self.inner.started_event_id()
        }
        async fn observe(
            &self,
            data: EventData,
            payloads: Vec<PayloadRecord>,
        ) -> anyhow::Result<AppendReceipt> {
            if matches!(data, EventData::ProviderResponseProgressObserved { .. })
                && !self.completed.load(Ordering::SeqCst)
            {
                tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
                let receipt = self.inner.observe(data, payloads).await?;
                self.completed.store(true, Ordering::SeqCst);
                return Ok(receipt);
            }
            self.inner.observe(data, payloads).await
        }
    }
    for backend in ["sqlite", "jsonl"] {
        let (work, provider) = grok_fixture("slow_capture");
        let mut h = Harness::new(backend);
        let completed = Arc::new(AtomicBool::new(false));
        h.admission = Arc::new(SlowAdmission {
            inner: h.admission.clone(),
            completed: completed.clone(),
        });
        let error = tokio::time::timeout(std::time::Duration::from_secs(5), chat(&h, &provider))
            .await
            .expect("bounded protocol deadline")
            .expect_err("deadline");
        assert!(
            completed.load(Ordering::SeqCst),
            "inflight observation cancelled"
        );
        assert!(format!("{error:#}").contains("timed out after 1s"));
        assert!(!work.path().join("permission-reply.json").exists());
        let events = h.events();
        let EventData::InvocationOutcomeUnknown {
            observed_partial_ref: Some(reference),
            error_ref: Some(error_ref),
            ..
        } = &events.last().expect("outcome").body.data
        else {
            panic!("unknown partial")
        };
        let store = reopened(&h, backend);
        let diagnostic: ProviderDiagnostic =
            serde_json::from_slice(&bytes(&*store, error_ref)).expect("diagnostic");
        assert_eq!(diagnostic.error_key, "provider_response_timeout");
        let partial: ProviderResponsePayload =
            serde_json::from_slice(&bytes(&*store, reference)).expect("partial");
        assert!(contents(&*store, &partial).contains(&("text".into(), b"progress".to_vec())));
    }
}

#[cfg(unix)]
struct FailObservation {
    inner: Arc<dyn zeroclaw_api::session_events::capture::ProviderAdmissionPort>,
    kind: &'static str,
}

#[cfg(unix)]
#[async_trait::async_trait]
impl zeroclaw_api::session_events::capture::ProviderAdmissionPort for FailObservation {
    async fn admit(
        &self,
        intent: zeroclaw_api::session_events::capture::ProviderDispatchIntent,
    ) -> anyhow::Result<Arc<dyn zeroclaw_api::session_events::capture::ProviderObservationPort>>
    {
        struct Port {
            inner: Arc<dyn zeroclaw_api::session_events::capture::ProviderObservationPort>,
            kind: &'static str,
        }
        #[async_trait::async_trait]
        impl zeroclaw_api::session_events::capture::ProviderObservationPort for Port {
            fn started_event_id(&self) -> EventId {
                self.inner.started_event_id()
            }
            async fn observe(
                &self,
                data: EventData,
                payloads: Vec<PayloadRecord>,
            ) -> anyhow::Result<AppendReceipt> {
                if payloads.iter().any(|payload| {
                    serde_json::from_slice::<Value>(&payload.body)
                        .ok()
                        .is_some_and(|value| value["kind"] == self.kind)
                }) {
                    anyhow::bail!("fixture_observation_failure");
                }
                self.inner.observe(data, payloads).await
            }
        }
        Ok(Arc::new(Port {
            inner: self.inner.admit(intent).await?,
            kind: self.kind,
        }))
    }
}

#[cfg(unix)]
#[tokio::test]
async fn grok_capture_failure_stops_before_permission_reply_both_backends() {
    for backend in ["sqlite", "jsonl"] {
        for kind in [
            "provider_tool_activity",
            "provider_permission_request",
            "provider_permission_response",
        ] {
            let (work, provider) = grok_fixture("allow");
            let mut h = Harness::new(backend);
            h.admission = Arc::new(FailObservation {
                inner: h.admission.clone(),
                kind,
            });
            let error = chat(&h, &provider).await.expect_err("capture failure");
            assert!(
                zeroclaw_api::session_events::capture::is_capture_stopped(&error),
                "{error:#}"
            );
            assert!(format!("{error:#}").contains("fixture_observation_failure"));
            assert!(!work.path().join("permission-reply.json").exists());
            assert!(
                !h.events()
                    .iter()
                    .any(|event| event.body.kind() == EventKind::ProviderAttemptCompleted)
            );
        }
    }
}
