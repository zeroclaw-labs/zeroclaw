use super::*;
use zeroclaw_api::attribution::{Attributable, Role};
use zeroclaw_api::session_events::capture::scope_provider_capture;
use zeroclaw_api::session_events::payloads::{StoredPayload, content_leaf, nested_refs};
use zeroclaw_infra::session_events::open_event_store;
use zeroclaw_kernel::session::{
    EffectIntent, PermissionResolver, SessionCoordinator, SessionError,
};

struct Allow;

#[tokio::test]
async fn provider_content_metadata_survives_complete_and_interrupted_capture() {
    for backend in ["sqlite", "jsonl"] {
        for streamed in [false, true] {
            let h = Harness::new(backend);
            let provider = Leaf {
                store: h.store.clone(),
                stream: h.stream.clone(),
                error: false,
            };
            let mut recorder = scope_provider_capture(
                h.admission.clone(),
                Recorder::begin(
                    &provider,
                    intent(&provider, "fixture", &[], None, None, None),
                ),
            )
            .await
            .unwrap()
            .unwrap();
            let blocks = crate::output::responses_blocks(&[
                serde_json::json!({"type":"message","id":"msg_fixture","status":"completed","phase":"final_answer","content":[{"type":"output_text","text":"answer","annotations":[{"url":"https://fixture.invalid"}],"future":{"token":"fixture-secret"}}]}),
                serde_json::json!({"type":"function_call","id":"fc_fixture","call_id":"call_fixture","name":"lookup","namespace":"search","arguments":"{}"}),
            ]);
            if streamed {
                for block in &blocks {
                    recorder
                        .event(&StreamEvent::OutputBlock(block.clone()))
                        .await
                        .unwrap();
                }
                recorder
                    .error(&anyhow::Error::msg("fixture interrupted"))
                    .await
                    .unwrap();
            } else {
                recorder
                    .response(&ChatResponse {
                        output_blocks: blocks,
                        text: None,
                        tool_calls: vec![],
                        usage: None,
                        reasoning_content: None,
                    })
                    .await
                    .unwrap();
            }
            let events = h.events();
            let reference = match &events.last().unwrap().body.data {
                EventData::ProviderAttemptCompleted { response_ref, .. } => response_ref,
                EventData::ProviderAttemptFailed {
                    partial_response_ref: Some(reference),
                    ..
                } => reference,
                EventData::InvocationOutcomeUnknown {
                    observed_partial_ref: Some(reference),
                    ..
                } => reference,
                other => panic!("unexpected terminal {other:?}"),
            };
            let response: ProviderResponsePayload = h.payload(reference);
            let metadata: Vec<serde_json::Value> = response
                .blocks
                .iter()
                .filter_map(|block| match block {
                    ResponseBlock::Other {
                        type_name,
                        classified_body_ref,
                    } if type_name == "provider.content_metadata" => {
                        Some(h.payload(classified_body_ref))
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(metadata.len(), 3);
            assert_eq!(metadata[0]["fields"]["id"], "msg_fixture");
            assert_eq!(metadata[0]["fields"]["phase"], "final_answer");
            assert_eq!(
                metadata[1]["position"],
                serde_json::json!({"output_index":0,"content_index":0})
            );
            assert_eq!(metadata[1]["fields"]["future"]["token"], "fixture-secret");
            assert_eq!(metadata[2]["fields"]["id"], "fc_fixture");
            assert_eq!(metadata[2]["fields"]["namespace"], "search");
            assert_eq!(
                response.completion,
                if streamed {
                    ContentCompletion::Partial
                } else {
                    ContentCompletion::Complete
                }
            );
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn cli_deadline_cannot_cancel_an_inflight_durable_checkpoint() {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct SlowPort {
        inner: Arc<dyn ProviderObservationPort>,
        completed: Arc<AtomicBool>,
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
            if matches!(data, EventData::ProviderResponseProgressObserved { .. }) {
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                let receipt = self.inner.observe(data, payloads).await?;
                self.completed.store(true, Ordering::SeqCst);
                return Ok(receipt);
            }
            self.inner.observe(data, payloads).await
        }
    }
    for backend in ["sqlite", "jsonl"] {
        let h = Harness::new(backend);
        let provider = Leaf {
            store: h.store.clone(),
            stream: h.stream.clone(),
            error: false,
        };
        let mut recorder = scope_provider_capture(
            h.admission.clone(),
            Recorder::begin(
                &provider,
                intent(&provider, "fixture", &[], None, None, None),
            ),
        )
        .await
        .expect("admission");
        let completed = Arc::new(AtomicBool::new(false));
        let recording = recorder.as_mut().expect("native recorder");
        recording.port = Arc::new(SlowPort {
            inner: recording.port.clone(),
            completed: completed.clone(),
        });
        let child = tokio::process::Command::new("sh")
            .args(["-c", "printf partial; exec sleep 60"])
            .kill_on_drop(true)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("fixture child");
        let error = scope_adapter_capture(
            &mut recorder,
            crate::cli_output::run(
                child,
                "",
                "fixture_cli",
                std::time::Duration::from_millis(200),
            ),
        )
        .await
        .expect_err("deadline");
        assert!(
            completed.load(Ordering::SeqCst),
            "inflight observation was cancelled"
        );
        assert!(error.chain().any(|cause| matches!(
            cause.downcast_ref::<zeroclaw_api::model_provider::StreamError>(),
            Some(zeroclaw_api::model_provider::StreamError::ResponseTimeout(
                _
            ))
        )));
        let recording = recorder.as_mut().expect("recorder restored");
        recording.error(&error).await.expect("terminal event");
        let events = h.events();
        let EventData::InvocationOutcomeUnknown {
            observed_partial_ref: Some(reference),
            error_ref: Some(diagnostic_ref),
            ..
        } = &events.last().expect("terminal").body.data
        else {
            panic!("unknown partial")
        };
        let response: ProviderResponsePayload = h.payload(reference);
        assert_eq!(
            response
                .blocks
                .iter()
                .filter_map(|block| match block {
                    ResponseBlock::Text { text_ref } => Some(h.bytes(text_ref)),
                    _ => None,
                })
                .flatten()
                .collect::<Vec<_>>(),
            b"partial"
        );
        let diagnostic: ProviderDiagnostic = h.payload(diagnostic_ref);
        assert_eq!(diagnostic.error_key, "provider_response_timeout");
    }
}

#[tokio::test]
async fn native_anthropic_signed_reasoning_preserves_partial_final_and_request_replay() {
    let _proxy_guard = crate::RuntimeProxyTestGuard::acquire().await;
    for backend in ["sqlite", "jsonl"] {
        for (completed, forced_nonstream) in [(false, false), (true, false), (true, true)] {
            let h = Harness::new(backend);
            let requests = Arc::new(tokio::sync::Mutex::new(Vec::<serde_json::Value>::new()));
            let recorded = requests.clone();
            let signature = "sk-fixture-signature";
            let thinking = "weigh the options token=fixture-secret";
            let signed = serde_json::json!({"thinking":thinking,"signature":signature}).to_string();
            let opaque =
                serde_json::json!({"type":"redacted_thinking","data":"sk-opaque-continuation"});
            let native_response = serde_json::json!({
                "model":"claude-sonnet-4-6",
                "content":[
                    {"type":"thinking","thinking":thinking,"signature":signature},
                    opaque.clone(),
                    {"type":"text","text":"answer"},
                    {"type":"tool_use","id":"call_fixture","name":"fixture","input":{"query":"hello"}},
                    {"type":"image","source":{"type":"base64","media_type":"image/png","data":"AAEC"}},
                    {"type":"future_content","result":"retained"},
                ],
                "stop_reason":"end_turn",
            });
            let mut frames = vec![
                serde_json::json!({"type":"message_start","message":{"id":"fixture","model":"claude-sonnet-4-6"}}),
                serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}),
                serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":thinking}}),
                serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":signature,"api_key":"fixture-secret"}}),
            ];
            if completed {
                frames.extend([
                    serde_json::json!({"type":"content_block_stop","index":0}),
                    serde_json::json!({"type":"content_block_start","index":1,"content_block":opaque}),
                    serde_json::json!({"type":"content_block_stop","index":1}),
                    serde_json::json!({"type":"content_block_start","index":2,"content_block":{"type":"text","text":""}}),
                    serde_json::json!({"type":"content_block_delta","index":2,"delta":{"type":"text_delta","text":"answer"}}),
                    serde_json::json!({"type":"content_block_stop","index":2}),
                    serde_json::json!({"type":"message_delta","delta":{"stop_reason":"end_turn"}}),
                    serde_json::json!({"type":"message_stop"}),
                ]);
            }
            let wire = frames
                .iter()
                .map(|event| format!("data: {event}\n\n"))
                .collect::<String>();
            let app = axum::Router::new().route(
                "/v1/messages",
                axum::routing::post(move |axum::Json(body): axum::Json<serde_json::Value>| {
                    let recorded = recorded.clone();
                    let wire = wire.clone();
                    let native_response = native_response.clone();
                    async move {
                        let mut requests = recorded.lock().await;
                        requests.push(body);
                        if requests.len() == 1 && !forced_nonstream {
                            axum::response::Response::builder()
                                .header("content-type", "text/event-stream")
                                .body(axum::body::Body::from(wire))
                                .unwrap()
                        } else if requests.len() == 1 {
                            axum::response::Response::builder()
                                .header("content-type", "application/json")
                                .body(axum::body::Body::from(native_response.to_string()))
                                .unwrap()
                        } else {
                            axum::response::Response::builder()
                                .header("content-type", "application/json")
                                .body(axum::body::Body::from(
                                    serde_json::json!({
                                        "content":[{"type":"text","text":"continued"}],
                                        "stop_reason":"end_turn",
                                    })
                                    .to_string(),
                                ))
                                .unwrap()
                        }
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = ::zeroclaw_spawn::spawn!(async move {
                axum::serve(listener, app).await.unwrap();
            });
            let provider = crate::anthropic::AnthropicModelProvider::builder("fixture")
                .credential(Some("fixture-key"))
                .base_url(&format!("http://{address}"))
                .build();
            let messages = [ChatMessage::user("fixture")];
            let events = scope_provider_capture(h.admission.clone(), async {
                ProviderDispatch::from_ref(&provider)
                    .stream_chat(
                        ChatRequest {
                            messages: &messages,
                            tools: None,
                            thinking: Some(zeroclaw_api::model_provider::NativeThinkingParams {
                                budget_tokens: 2_048,
                                display: if forced_nonstream {
                                    None
                                } else {
                                    Some(zeroclaw_api::model_provider::ThinkingDisplay::Updates)
                                },
                            }),
                        },
                        "claude-sonnet-4-6",
                        None,
                        StreamOptions::new(true),
                    )
                    .collect::<Vec<_>>()
                    .await
            })
            .await;
            assert_eq!(events.iter().all(Result::is_ok), completed);
            let history = h.events();
            let response: ProviderResponsePayload = history
                .iter()
                .find_map(|event| match &event.body.data {
                    EventData::ProviderAttemptCompleted { response_ref, .. } => {
                        Some(h.payload(response_ref))
                    }
                    EventData::InvocationOutcomeUnknown {
                        observed_partial_ref: Some(response_ref),
                        ..
                    } => Some(h.payload(response_ref)),
                    _ => None,
                })
                .expect("durable response");
            let partial = response.blocks.iter().find_map(|block| match block {
                ResponseBlock::Reasoning {
                    representation_kind,
                    payload_ref,
                    completion: ContentCompletion::Partial,
                    replay_requirement,
                } if representation_kind == "signature_delta"
                    && replay_requirement == "display_only" =>
                {
                    Some(h.payload::<serde_json::Value>(payload_ref))
                }
                _ => None,
            });
            if forced_nonstream {
                assert!(partial.is_none());
            } else {
                let partial = partial.expect("durable partial signature");
                assert_eq!(partial["activity"]["delta"]["signature"], signature);
                assert_eq!(partial["activity"]["delta"]["api_key"], "fixture-secret");
            }
            let readable = response
                .blocks
                .iter()
                .find_map(|block| match block {
                    ResponseBlock::Reasoning {
                        representation_kind,
                        payload_ref,
                        ..
                    } if representation_kind == "human_readable_delta" => {
                        Some(h.bytes(payload_ref))
                    }
                    _ => None,
                })
                .expect("original readable thinking");
            assert_eq!(readable, thinking.as_bytes());
            let finalized = response
                .blocks
                .iter()
                .filter_map(|block| match block {
                    ResponseBlock::Reasoning {
                        representation_kind,
                        payload_ref,
                        completion: ContentCompletion::Complete,
                        ..
                    } if representation_kind == crate::anthropic::SIGNED_REPLAY_REPRESENTATION => {
                        Some(h.bytes(payload_ref))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(
                finalized,
                if completed {
                    vec![signed.as_bytes().to_vec()]
                } else {
                    vec![]
                }
            );
            assert!(!response.blocks.iter().any(|block| matches!(block,
                ResponseBlock::Reasoning { representation_kind, .. } if representation_kind == "provider_replay"
            )), "compatibility projection must not create a second canonical copy");
            if completed {
                let retained_opaque = response
                    .blocks
                    .iter()
                    .find_map(|block| match block {
                        ResponseBlock::Reasoning {
                            representation_kind,
                            payload_ref,
                            ..
                        } if representation_kind == "redacted_thinking" => {
                            Some(h.bytes(payload_ref))
                        }
                        _ => None,
                    })
                    .expect("opaque continuation");
                assert_eq!(retained_opaque, opaque.to_string().as_bytes());
                if forced_nonstream {
                    let block_kinds: Vec<_> = response
                        .blocks
                        .iter()
                        .filter(|block| !matches!(block, ResponseBlock::Other { type_name, .. } if type_name == "provider.content_metadata"))
                        .map(|block| match block {
                            ResponseBlock::Reasoning { .. } => "reasoning",
                            ResponseBlock::Text { .. } => "text",
                            ResponseBlock::ToolCall { .. } => "tool",
                            ResponseBlock::Media { .. } => "media",
                            ResponseBlock::Other { .. } => "other",
                        })
                        .collect();
                    assert_eq!(
                        block_kinds,
                        [
                            "reasoning",
                            "reasoning",
                            "reasoning",
                            "text",
                            "tool",
                            "media",
                            "other"
                        ]
                    );
                    assert_eq!(
                        events
                            .iter()
                            .filter(|event| matches!(event, Ok(StreamEvent::TextDelta(_))))
                            .count(),
                        1
                    );
                    assert_eq!(
                        events
                            .iter()
                            .filter(|event| matches!(event, Ok(StreamEvent::ToolCall(_))))
                            .count(),
                        1
                    );
                    let media = response
                        .blocks
                        .iter()
                        .find_map(|block| match block {
                            ResponseBlock::Media {
                                received: MediaReceived::Captured { representation_ref },
                                ..
                            } => Some(h.bytes(representation_ref)),
                            _ => None,
                        })
                        .expect("forced nonstream retains received image bytes");
                    assert_eq!(media, [0, 1, 2]);
                    assert!(response.blocks.iter().any(|block| matches!(block,
                        ResponseBlock::Other { type_name, .. } if type_name == "future_content"
                    )));
                    let served = history
                        .iter()
                        .find_map(|event| match &event.body.data {
                            EventData::ProviderAttemptCompleted {
                                served_attribution_ref: Some(reference),
                                ..
                            } => Some(h.payload::<serde_json::Value>(reference)),
                            _ => None,
                        })
                        .expect("served model retains protocol attribution");
                    assert_eq!(served["model"], "claude-sonnet-4-6");
                    let finish: serde_json::Value = h.payload(
                        response
                            .finish_evidence_ref
                            .as_ref()
                            .expect("protocol finish evidence"),
                    );
                    assert_eq!(finish["stop_reason"], "end_turn");
                }
                let continuation = events
                    .into_iter()
                    .filter_map(|event| match event.unwrap() {
                        StreamEvent::ReasoningFinalized(value) => Some(value),
                        _ => None,
                    })
                    .collect::<String>();
                assert_eq!(continuation, format!("{signed}\n{opaque}"));
                let replay = [
                    messages[0].clone(),
                    ChatMessage::assistant_tool_calls(
                        ("answer").to_string(),
                        serde_json::from_value(serde_json::json!([])).unwrap(),
                        Some((continuation).to_string()),
                    ),
                    ChatMessage::user("continue"),
                ];
                ProviderDispatch::from_ref(&provider)
                    .chat(
                        ChatRequest {
                            messages: &replay,
                            tools: None,
                            thinking: None,
                        },
                        "claude-sonnet-4-6",
                        None,
                    )
                    .await
                    .expect("legacy continuation consumer still sends signed replay");
                let requests = requests.lock().await;
                assert_eq!(requests.len(), 2);
                let replayed = requests[1]["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|message| message["role"] == "assistant")
                    .flat_map(|message| message["content"].as_array().unwrap())
                    .find(|block| block["type"] == "thinking")
                    .unwrap();
                assert_eq!(replayed["thinking"], thinking);
                assert_eq!(replayed["signature"], signature);
                let replayed_opaque = requests[1]["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|message| message["role"] == "assistant")
                    .flat_map(|message| message["content"].as_array().unwrap())
                    .find(|block| block["type"] == "redacted_thinking")
                    .expect("opaque continuation returns to the provider");
                assert_eq!(replayed_opaque, &opaque);
            }
            server.abort();
        }
    }
}

#[tokio::test]
async fn ordinary_reasoning_preserves_original_content_without_signed_adapter_evidence() {
    for backend in ["sqlite", "jsonl"] {
        let h = Harness::new(backend);
        let provider = Leaf {
            store: h.store.clone(),
            stream: h.stream.clone(),
            error: false,
        };
        let mut recorder = scope_provider_capture(
            h.admission.clone(),
            Recorder::begin(
                &provider,
                intent(&provider, "fixture", &[], None, None, None),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        let plain = "ordinary token=fixture-secret";
        let structured = r#"{"thinking":"ordinary","signature":"sk-fixture-signature"}"#;
        for event in [
            StreamEvent::ThinkingDelta(plain.into()),
            StreamEvent::ReasoningFinalized(plain.into()),
            StreamEvent::ReasoningFinalized(structured.into()),
        ] {
            recorder.event(&event).await.unwrap();
        }
        recorder.event(&StreamEvent::Final).await.unwrap();
        assert_eq!(recorder.blocks.len(), 3);
        for (block, expected) in recorder
            .blocks
            .iter()
            .map(|segment| &segment.block)
            .zip([plain, plain, structured])
        {
            let ResponseBlock::Reasoning { payload_ref, .. } = block else {
                panic!("reasoning")
            };
            let text = String::from_utf8(h.bytes(payload_ref)).unwrap();
            assert_eq!(text, expected);
        }
    }
}

#[tokio::test]
async fn signed_reasoning_projection_is_consumed_once_and_only_when_adjacent() {
    use zeroclaw_api::model_provider::ProviderOutputBlock;
    for backend in ["sqlite", "jsonl"] {
        let h = Harness::new(backend);
        let provider = Leaf {
            store: h.store.clone(),
            stream: h.stream.clone(),
            error: false,
        };
        let mut recorder = scope_provider_capture(
            h.admission.clone(),
            Recorder::begin(
                &provider,
                intent(&provider, "fixture", &[], None, None, None),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        let signed = r#"{"thinking":"ordinary","signature":"sk-fixture-signature"}"#;
        let typed = StreamEvent::OutputBlock(ProviderOutputBlock::Reasoning {
            representation: crate::anthropic::SIGNED_REPLAY_REPRESENTATION.into(),
            bytes: signed.as_bytes().to_vec(),
            replay_requirement: "provider_defined".into(),
        });
        for event in [
            typed.clone(),
            StreamEvent::ReasoningFinalized(signed.into()),
            StreamEvent::ReasoningFinalized(signed.into()),
            typed,
            StreamEvent::ProtocolMetadata(serde_json::json!({"kind":"fixture_boundary"})),
            StreamEvent::ReasoningFinalized(signed.into()),
            StreamEvent::Final,
        ] {
            recorder.event(&event).await.unwrap();
        }
        assert_eq!(recorder.blocks.len(), 4);
        for (index, block) in recorder
            .blocks
            .iter()
            .map(|segment| &segment.block)
            .enumerate()
        {
            let ResponseBlock::Reasoning {
                representation_kind,
                payload_ref,
                ..
            } = block
            else {
                panic!("reasoning block")
            };
            let bytes = h.bytes(payload_ref);
            if index % 2 == 0 {
                assert_eq!(
                    representation_kind,
                    crate::anthropic::SIGNED_REPLAY_REPRESENTATION
                );
                assert_eq!(bytes, signed.as_bytes());
            } else {
                assert_eq!(representation_kind, "provider_replay");
                assert_eq!(bytes, signed.as_bytes());
            }
        }
    }
}

#[tokio::test]
async fn native_codex_buffered_api_checkpoints_wire_reasoning_and_never_retries_uncertainty() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let _proxy_guard = crate::RuntimeProxyTestGuard::acquire().await;
    for backend in ["sqlite", "jsonl"] {
        for mode in [
            "completed",
            "truncated",
            "json",
            "malformed_sse",
            "invalid_utf8",
        ] {
            for string_api in [false, true] {
                let h = Harness::new(backend);
                let calls = Arc::new(AtomicUsize::new(0));
                let seen = calls.clone();
                let store = h.store.clone();
                let stream_id = h.stream.clone();
                let app = axum::Router::new().route("/responses", axum::routing::post(move || {
                    let seen = seen.clone();
                    let store = store.clone();
                    let stream_id = stream_id.clone();
                    async move {
                        seen.fetch_add(1, Ordering::SeqCst);
                        let output = serde_json::json!([
                            {"type":"reasoning","id":"reason-1","encrypted_content":"opaque-continuation","summary":[{"type":"summary_text","text":"thinking fixture"}]},
                            {"type":"message","id":"message-1","content":[{"type":"output_text","text":"answer fixture"}]}
                        ]);
                        if mode == "json" {
                            return axum::response::Response::new(axum::body::Body::from(serde_json::json!({
                                "status":"completed", "output":output, "usage":{"input_tokens":7,"output_tokens":9}
                            }).to_string()));
                        }
                        let first = "data: {\"type\":\"response.output_audio.delta\",\"delta\":\"AAEC\"}\n\ndata: {\"type\":\"response.reasoning_summary_text.delta\",\"item_id\":\"reason-1\",\"delta\":\"thinking fixture\"}\n\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"answer fixture\"}\n\n".to_string();
                        let second = format!("data: {}\n\ndata: {}\n\n", serde_json::json!({"type":"response.output_item.done","item":output[0]}), serde_json::json!({"type":"response.completed","response":{"status":"completed","output":output,"usage":{"input_tokens":7,"output_tokens":9}}}));
                        let chunks = futures_util::stream::unfold(0, move |phase| {
                            let first=first.clone(); let second=second.clone(); let store=store.clone(); let stream_id=stream_id.clone();
                            async move {
                                if phase == 0 {
                                    return Some((Ok::<_, std::io::Error>(first.into_bytes()), 1));
                                }
                                if phase == 1 {
                                    // This is a real transport backpressure boundary: the
                                    // server withholds the terminal frame until both
                                    // earlier deltas are durable, while chat() is pending.
                                    tokio::time::timeout(std::time::Duration::from_secs(10), async {
                                        loop {
                                            let page=store.read_page(&stream_id,&EventCursor::beginning(stream_id.clone()),None,100).expect("history");
                                            if page.events.iter().filter(|e|e.body.kind()==EventKind::ProviderResponseProgressObserved).count() >= 3 { break; }
                                            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                                        }
                                    }).await.expect("wire deltas must be durable before chat returns");
                                    if mode == "completed" { return Some((Ok(second.into_bytes()), 2)); }
                                    if mode == "malformed_sse" { return Some((Ok(b"data: not-json\n\n".to_vec()), 2)); }
                                    if mode == "invalid_utf8" { return Some((Ok(vec![0xff]), 2)); }
                                }
                                None
                            }
                        });
                        axum::response::Response::new(axum::body::Body::from_stream(chunks))
                    }
                }));
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                    .await
                    .expect("listener");
                let address = listener.local_addr().expect("address");
                let server = ::zeroclaw_spawn::spawn!(async move {
                    axum::serve(listener, app).await.expect("server");
                });
                let options = crate::ModelProviderRuntimeOptions {
                    provider_api_url: Some(format!("http://{address}")),
                    zeroclaw_dir: Some(h._dir.path().to_path_buf()),
                    secrets_encrypt: false,
                    ..Default::default()
                };
                let provider = crate::openai_codex::OpenAiCodexModelProvider::new(
                    "fixture",
                    &options,
                    Some("fixture-key"),
                )
                .expect("provider");
                let messages = [ChatMessage::user("fixture")];
                let result = scope_provider_capture(h.admission.clone(), async {
                    if string_api {
                        ProviderDispatch::from_ref(&provider)
                            .chat_with_history(&messages, "fixture", None)
                            .await
                    } else {
                        ProviderDispatch::from_ref(&provider)
                            .chat(
                                ChatRequest {
                                    messages: &messages,
                                    tools: None,
                                    thinking: None,
                                },
                                "fixture",
                                None,
                            )
                            .await
                            .map(|r| r.text.expect("text"))
                    }
                })
                .await;
                assert_eq!(calls.load(Ordering::SeqCst), 1, "no hidden physical retry");
                let events = h.events();
                let response: ProviderResponsePayload =
                    if matches!(mode, "truncated" | "malformed_sse" | "invalid_utf8") {
                        assert!(zeroclaw_api::session_events::capture::is_capture_stopped(
                            &result.expect_err("uncertain EOF")
                        ));
                        let EventData::InvocationOutcomeUnknown {
                            observed_partial_ref: Some(reference),
                            error_ref: Some(error),
                            ..
                        } = &events.last().expect("unknown").body.data
                        else {
                            panic!("typed unknown")
                        };
                        let diagnostic: ProviderDiagnostic = h.payload(error);
                        assert_eq!(
                            diagnostic.error_key,
                            if mode == "truncated" {
                                "provider_missing_final"
                            } else {
                                "provider_decode_error"
                            }
                        );
                        if mode != "truncated" {
                            assert_eq!(diagnostic.stage, DiagnosticStage::Decode);
                        }
                        h.payload(reference)
                    } else {
                        assert_eq!(result.expect("completed"), "answer fixture");
                        let EventData::ProviderAttemptCompleted { response_ref, .. } =
                            &events.last().expect("complete").body.data
                        else {
                            panic!("one terminal completion")
                        };
                        assert_eq!(
                            events
                                .iter()
                                .filter(|e| e.body.kind() == EventKind::ProviderAttemptCompleted)
                                .count(),
                            1
                        );
                        h.payload(response_ref)
                    };
                let text = response
                    .blocks
                    .iter()
                    .filter_map(|b| match b {
                        ResponseBlock::Text { text_ref } => {
                            Some(String::from_utf8(h.bytes(text_ref)).expect("utf8"))
                        }
                        _ => None,
                    })
                    .collect::<String>();
                assert_eq!(text, "answer fixture", "no doubled accumulated text");
                let reasoning = response
                    .blocks
                    .iter()
                    .filter_map(|b| match b {
                        ResponseBlock::Reasoning { payload_ref, .. } => {
                            Some(String::from_utf8(h.bytes(payload_ref)).expect("utf8"))
                        }
                        _ => None,
                    })
                    .collect::<String>();
                assert!(reasoning.contains("thinking fixture"));
                if mode != "json" {
                    assert!(response.blocks.iter().any(|block| matches!(block, ResponseBlock::Other {type_name,classified_body_ref} if type_name == "response.output_audio.delta" && String::from_utf8(h.bytes(classified_body_ref)).expect("media evidence").contains("AAEC"))), "unsupported media delta remains observable");
                }
                if matches!(mode, "completed" | "json") {
                    assert!(
                        reasoning.contains("opaque-continuation"),
                        "actual opaque replay content"
                    );
                    assert!(response.usage_ref.is_some());
                } else {
                    assert_eq!(response.completion, ContentCompletion::Partial);
                }
                server.abort();
            }
        }
    }
}

#[tokio::test]
async fn native_provider_tool_activity_preserves_real_ids_and_absence() {
    for backend in ["sqlite", "jsonl"] {
        let h = Harness::new(backend);
        let port = h
            .admission
            .admit(ProviderDispatchIntent {
                requested_route: None,
                previous_attempt_event_id: None,
                tool_round_policy: zeroclaw_api::model_provider::ToolRoundPolicy::Batch,
                thinking: None,
                provider_ref: "fixture".into(),
                model: "fixture".into(),
                messages: vec![],
                tools: None,
                temperature: None,
            })
            .await
            .expect("admit");
        let mut recorder = Recorder {
            port,
            blocks: vec![],
            last_progress: None,
            captured_bytes: 0,
            usage_ref: None,
            finish_ref: None,
            served_ref: None,
            adapter_response_observed: false,
            pending_signed_projection: None,
            pending_position: None,
        };
        recorder
            .event(&StreamEvent::ToolCall(
                zeroclaw_api::model_provider::ToolCall {
                    id: "invalid-args".into(),
                    name: "fixture".into(),
                    arguments: "{invalid JSON".into(),
                    extra_content: None,
                },
            ))
            .await
            .expect("malformed argument evidence must survive");
        let ResponseBlock::ToolCall { tool_call_ref } = &recorder.blocks[0].block else {
            panic!("call evidence")
        };
        let call: serde_json::Value = h.payload(tool_call_ref);
        let arguments: StoredPayloadRef =
            serde_json::from_value(call["arguments_ref"].clone()).expect("argument ref");
        assert_eq!(h.bytes(&arguments), b"{invalid JSON");
        for id in [Some("call-1"), Some("call-2"), None] {
            recorder
                .event(&StreamEvent::PreExecutedToolCall {
                    provider_call_id: id.map(str::to_owned),
                    name: "same-tool".into(),
                    args: "{}".into(),
                })
                .await
                .expect("call");
        }
        recorder.event(&StreamEvent::ProtocolMetadata(serde_json::json!({"kind":"provider_tool_activity","activity":{"type":"response.web_search_call.searching","item_id":"call-2","payload_id":"external-provider-id"}}))).await.expect("activity");
        let ids = h
            .events()
            .iter()
            .filter_map(|e| match &e.body.data {
                EventData::ProviderResponseProgressObserved {
                    observation_ref, ..
                } => Some(h.payload::<ProviderObservationPayload>(observation_ref)),
                _ => None,
            })
            .flat_map(|p| p.operations)
            .filter_map(|o| match o {
                ProviderObservation::ProviderToolActivityObserved {
                    provider_call_id, ..
                } => Some(provider_call_id),
                _ => None,
            })
            .collect::<Vec<_>>();
        let observation = h
            .events()
            .iter()
            .find_map(|event| match &event.body.data {
                EventData::ProviderResponseProgressObserved { observation_ref } => {
                    let value: ProviderObservationPayload = h.payload(observation_ref);
                    value
                        .operations
                        .iter()
                        .find_map(|operation| match operation {
                            ProviderObservation::ProviderToolActivityObserved {
                                provider_call_id: Some(id),
                                activity_ref,
                            } if id == "call-2"
                                && h.bytes(activity_ref)
                                    .windows(b"external-provider-id".len())
                                    .any(|part| part == b"external-provider-id") =>
                            {
                                Some(activity_ref.clone())
                            }
                            _ => None,
                        })
                }
                _ => None,
            })
            .expect("opaque external identity survives");
        assert_eq!(observation.codec, "session.bytes/v1");
        assert!(content_leaf(&observation.codec));
        assert_eq!(
            nested_refs(&StoredPayload {
                reference: observation.clone(),
                body: Some(h.bytes(&observation))
            })
            .expect("content has no internal edges"),
            Vec::<StoredPayloadRef>::new()
        );
        assert_eq!(
            ids,
            vec![
                Some("call-1".into()),
                Some("call-2".into()),
                None,
                Some("call-2".into())
            ]
        );
    }
}
impl PermissionResolver for Allow {
    fn authorize(&self, _intent: &EffectIntent) -> Result<(), SessionError> {
        Ok(())
    }
}
struct Harness {
    _dir: tempfile::TempDir,
    store: Arc<dyn SessionEventStore>,
    stream: SessionStreamId,
    admission: Arc<dyn zeroclaw_api::session_events::capture::ProviderAdmissionPort>,
}
impl Harness {
    fn new(backend: &str) -> Self {
        let dir = tempfile::TempDir::new().expect("dir");
        let store = open_event_store(dir.path(), backend).expect("store");
        let stream = SessionStreamId::new();
        let branch = BranchId::new();
        let turn = TurnId::new();
        let ingress = PayloadRecord::json(
            "session.ingress/v1",
            &serde_json::json!({"source":"embedded_test"}),
        )
        .expect("ingress");
        let binding = PayloadRecord::json(
            "session.execution-binding/v1",
            &serde_json::json!({"execution":"fixture"}),
        )
        .expect("binding");
        let content = PayloadRecord::json(
            "session.content/v1",
            &ContentPayload {
                message_id: MessageId::new(),
                parts: vec![],
            },
        )
        .expect("content");
        let context = PayloadRecord::json(
            "session.context/v1",
            &serde_json::json!({"input_ref":StoredPayloadRef::from(&content)}),
        )
        .expect("context");
        let make = |data: EventData, is_turn: bool| {
            let mut event =
                SessionEvent::typed(stream.clone(), data, EventActor::default()).expect("event");
            event.branch_id = Some(branch.clone());
            if is_turn {
                event.turn_id = Some(turn.clone());
            }
            event
        };
        let root = make(
            EventData::SessionStarted {
                session_id: SessionId::new(),
                owning_agent: "fixture".into(),
                capture_mode: CaptureMode::Native,
            },
            false,
        );
        let accepted = make(
            EventData::TurnAccepted {
                input_ref: StoredPayloadRef::from(&content),
                ingress_ref: StoredPayloadRef::from(&ingress),
                reply_route_ref: None,
            },
            true,
        );
        let started = make(
            EventData::TurnStarted {
                accepted_event_id: accepted.event_id.clone(),
                execution_binding_ref: StoredPayloadRef::from(&binding),
            },
            true,
        );
        let projection = make(
            EventData::ContextProjectionCreated {
                source_ranges: vec![],
                summary_ref: None,
                budget: 4096,
                reason_key: "fixture_context".into(),
                algorithm_version: "fixture/v1".into(),
                result_ref: StoredPayloadRef::from(&context),
                summary_source_refs: vec![],
                transform_started_event_id: None,
            },
            true,
        );
        let context_id = projection.event_id.clone();
        store
            .append_batch(AppendRequest {
                stream_id: stream.clone(),
                expected_head: EventCursor::beginning(stream.clone()),
                idempotency_key: "fixture".into(),
                events: vec![root, accepted, started, projection],
                payloads: vec![ingress, binding, content, context],
                erasure_reference_commitment: None,
            })
            .expect("fixture commit");
        let coordinator = Arc::new(SessionCoordinator::new(Arc::clone(&store)));
        let admission =
            coordinator.provider_capture(stream.clone(), branch, turn, context_id, Arc::new(Allow));
        Self {
            _dir: dir,
            store,
            stream,
            admission,
        }
    }
    fn events(&self) -> Vec<SessionEvent> {
        self.store
            .read_page(
                &self.stream,
                &EventCursor::beginning(self.stream.clone()),
                None,
                1000,
            )
            .expect("events")
            .events
    }
    fn payload<T: serde::de::DeserializeOwned>(&self, reference: &StoredPayloadRef) -> T {
        let PayloadRead::Present(record) = self
            .store
            .read_payload(&reference.payload_id)
            .expect("payload")
        else {
            panic!("present")
        };
        serde_json::from_slice(&record.body).expect("decode")
    }
    fn bytes(&self, reference: &StoredPayloadRef) -> Vec<u8> {
        let PayloadRead::Present(record) = self
            .store
            .read_payload(&reference.payload_id)
            .expect("payload")
        else {
            panic!("present")
        };
        record.body
    }
}

struct Leaf {
    store: Arc<dyn SessionEventStore>,
    stream: SessionStreamId,
    error: bool,
}
impl Attributable for Leaf {
    fn role(&self) -> Role {
        Role::System
    }
    fn alias(&self) -> &str {
        "capture-fixture"
    }
}
#[async_trait::async_trait]
impl ModelProvider for Leaf {
    async fn chat_with_system(
        &self,
        _system: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        Ok("answer".into())
    }
    async fn chat(
        &self,
        _request: ChatRequest<'_>,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<ChatResponse> {
        let events = self.store.read_page(
            &self.stream,
            &EventCursor::beginning(self.stream.clone()),
            None,
            1000,
        )?;
        assert_eq!(
            events
                .events
                .last()
                .expect("started before network")
                .body
                .kind(),
            EventKind::ProviderAttemptStarted
        );
        if self.error {
            return Err(crate::api_error_from_parts(
                "fixture",
                reqwest::StatusCode::UNAUTHORIZED,
                r#"{"error":{"message":"invalid sk-fixture-secret"}}"#,
            ));
        }
        Ok(ChatResponse {
            output_blocks: Vec::new(),
            text: Some("answer".into()),
            tool_calls: vec![],
            usage: None,
            reasoning_content: Some("opaque signed reasoning fixture".into()),
        })
    }
    fn stream_chat(
        &self,
        _request: ChatRequest<'_>,
        _model: &str,
        _temperature: Option<f64>,
        _options: StreamOptions,
    ) -> stream::BoxStream<'static, StreamResult<StreamEvent>> {
        let events = vec![
            Ok(StreamEvent::ThinkingDelta("visible thought".into())),
            Ok(StreamEvent::ReasoningFinalized("signed reasoning".into())),
            Ok(StreamEvent::TextDelta(
                zeroclaw_api::model_provider::StreamChunk::delta("answer"),
            )),
            Ok(StreamEvent::Final),
        ];
        stream::iter(events).boxed()
    }
}

#[tokio::test]
async fn session_capture_forwards_and_freezes_tool_round_policy() {
    use zeroclaw_api::model_provider::ToolRoundPolicy;

    struct PolicyLeaf(Leaf);
    impl Attributable for PolicyLeaf {
        fn role(&self) -> Role {
            Role::System
        }
        fn alias(&self) -> &str {
            "policy-fixture"
        }
    }
    #[async_trait::async_trait]
    impl ModelProvider for PolicyLeaf {
        async fn chat_with_system(
            &self,
            _: Option<&str>,
            _: &str,
            _: &str,
            _: Option<f64>,
        ) -> anyhow::Result<String> {
            panic!("policy dispatch must retain its structured entrypoint")
        }
        async fn chat_with_tool_round_policy(
            &self,
            request: ChatRequest<'_>,
            model: &str,
            temperature: Option<f64>,
            policy: ToolRoundPolicy,
        ) -> anyhow::Result<ChatResponse> {
            assert_eq!(policy, ToolRoundPolicy::Single);
            self.0.chat(request, model, temperature).await
        }
        fn stream_chat_with_tool_round_policy(
            &self,
            request: ChatRequest<'_>,
            model: &str,
            temperature: Option<f64>,
            options: StreamOptions,
            policy: ToolRoundPolicy,
        ) -> stream::BoxStream<'static, StreamResult<StreamEvent>> {
            assert_eq!(policy, ToolRoundPolicy::Single);
            self.0.stream_chat(request, model, temperature, options)
        }
    }

    for backend in ["sqlite", "jsonl"] {
        for owned in [false, true] {
            for streaming in [false, true] {
                let h = Harness::new(backend);
                let leaf = Arc::new(PolicyLeaf(Leaf {
                    store: h.store.clone(),
                    stream: h.stream.clone(),
                    error: false,
                }));
                let messages = [ChatMessage::user("single tool fixture")];
                let request = ChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                };
                scope_provider_capture(h.admission.clone(), async {
                    if owned {
                        let dispatch = ProviderDispatch::new(leaf.clone());
                        if streaming {
                            let mut events = dispatch.stream_chat_with_tool_round_policy(
                                request,
                                "fixture",
                                None,
                                StreamOptions::default(),
                                ToolRoundPolicy::Single,
                            );
                            while let Some(event) = events.next().await {
                                event.expect("stream event");
                            }
                        } else {
                            dispatch
                                .chat_with_tool_round_policy(
                                    request,
                                    "fixture",
                                    None,
                                    ToolRoundPolicy::Single,
                                )
                                .await
                                .expect("response");
                        }
                    } else {
                        // Pass the Arc itself through the blanket implementation.
                        let dispatch = ProviderDispatch::from_ref(&leaf);
                        if streaming {
                            let mut events = dispatch.stream_chat_with_tool_round_policy(
                                request,
                                "fixture",
                                None,
                                StreamOptions::default(),
                                ToolRoundPolicy::Single,
                            );
                            while let Some(event) = events.next().await {
                                event.expect("stream event");
                            }
                        } else {
                            dispatch
                                .chat_with_tool_round_policy(
                                    request,
                                    "fixture",
                                    None,
                                    ToolRoundPolicy::Single,
                                )
                                .await
                                .expect("response");
                        }
                    }
                })
                .await;
                let events = h.events();
                let requests: Vec<_> = events
                    .iter()
                    .filter_map(|event| match &event.body.data {
                        EventData::ProviderRequestPrepared { request_ref, .. } => {
                            Some(h.payload::<serde_json::Value>(request_ref))
                        }
                        _ => None,
                    })
                    .collect();
                assert_eq!(requests.len(), 1);
                assert_eq!(requests[0]["tool_round_policy"], "single");
                assert_eq!(
                    events
                        .iter()
                        .filter(|event| event.body.kind() == EventKind::ProviderAttemptCompleted)
                        .count(),
                    1
                );
            }
        }
    }
}

#[tokio::test]
async fn session_capture_preserves_per_request_native_thinking_parameters() {
    use zeroclaw_api::model_provider::{NativeThinkingParams, ThinkingDisplay};
    for backend in ["sqlite", "jsonl"] {
        for owned in [false, true] {
            for streaming in [false, true] {
                for thinking in [
                    None,
                    Some(NativeThinkingParams {
                        budget_tokens: 4096,
                        display: None,
                    }),
                    Some(NativeThinkingParams {
                        budget_tokens: 8192,
                        display: Some(ThinkingDisplay::Updates),
                    }),
                ] {
                    let h = Harness::new(backend);
                    let leaf = Arc::new(Leaf {
                        store: Arc::clone(&h.store),
                        stream: h.stream.clone(),
                        error: false,
                    });
                    let messages = [ChatMessage::user("fixture")];
                    let request = ChatRequest {
                        messages: &messages,
                        tools: None,
                        thinking,
                    };
                    scope_provider_capture(h.admission.clone(), async {
                        if owned {
                            let dispatch = ProviderDispatch::new(leaf.clone());
                            if streaming {
                                let mut stream = dispatch.stream_chat(
                                    request,
                                    "fixture",
                                    Some(0.25),
                                    StreamOptions::default(),
                                );
                                while let Some(event) = stream.next().await {
                                    event.expect("event");
                                }
                            } else {
                                dispatch
                                    .chat(request, "fixture", Some(0.25))
                                    .await
                                    .expect("response");
                            }
                        } else {
                            let dispatch = ProviderDispatch::from_ref(leaf.as_ref());
                            if streaming {
                                let mut stream = dispatch.stream_chat(
                                    request,
                                    "fixture",
                                    Some(0.25),
                                    StreamOptions::default(),
                                );
                                while let Some(event) = stream.next().await {
                                    event.expect("event");
                                }
                            } else {
                                dispatch
                                    .chat(request, "fixture", Some(0.25))
                                    .await
                                    .expect("response");
                            }
                        }
                    })
                    .await;
                    let events = h.events();
                    let prepared = events
                        .iter()
                        .find(|event| event.body.kind() == EventKind::ProviderRequestPrepared)
                        .expect("prepared");
                    let EventData::ProviderRequestPrepared { request_ref, .. } =
                        &prepared.body.data
                    else {
                        panic!("request");
                    };
                    let payload: serde_json::Value = h.payload(request_ref);
                    assert!(
                        payload.get("thinking").is_some(),
                        "absence must be an explicit dispatch fact"
                    );
                    assert_eq!(
                        payload["thinking"],
                        serde_json::to_value(thinking).expect("thinking")
                    );
                    assert_eq!(payload["temperature"], 0.25);
                    assert_eq!(payload["tool_round_policy"], "batch");
                }
            }
        }
    }
}

#[tokio::test]
async fn session_capture_commits_start_and_preserves_callless_reasoning() {
    for backend in ["sqlite", "jsonl"] {
        let h = Harness::new(backend);
        let leaf = Leaf {
            store: Arc::clone(&h.store),
            stream: h.stream.clone(),
            error: false,
        };
        let messages = [ChatMessage::user("fixture")];
        scope_provider_capture(
            Arc::clone(&h.admission),
            ProviderDispatch::from_ref(&leaf).chat(
                ChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "fixture-model",
                None,
            ),
        )
        .await
        .expect("response");
        let events = h.events();
        let EventData::ProviderAttemptCompleted { response_ref, .. } =
            &events.last().expect("completed").body.data
        else {
            panic!("completed")
        };
        let response: ProviderResponsePayload = h.payload(response_ref);
        let ResponseBlock::Reasoning { payload_ref, .. } = &response.blocks[0] else {
            panic!("reasoning block")
        };
        assert_eq!(h.bytes(payload_ref), b"opaque signed reasoning fixture");
        assert_eq!(
            events
                .iter()
                .filter(|event| event.body.kind() == EventKind::ProviderAttemptStarted)
                .count(),
            1
        );
        assert_eq!(response.completion, ContentCompletion::Complete);
        let evidence: serde_json::Value = h.payload(
            response
                .finish_evidence_ref
                .as_ref()
                .expect("observed response return"),
        );
        assert_eq!(evidence["observed"], "adapter_response_returned");
    }
}

#[tokio::test]
async fn session_capture_http_failure_is_definite_and_secrets_are_scrubbed() {
    let h = Harness::new("sqlite");
    let leaf = Leaf {
        store: Arc::clone(&h.store),
        stream: h.stream.clone(),
        error: true,
    };
    let messages = [ChatMessage::user("fixture")];
    let result = scope_provider_capture(
        Arc::clone(&h.admission),
        ProviderDispatch::from_ref(&leaf).chat(
            ChatRequest {
                messages: &messages,
                tools: None,
                thinking: None,
            },
            "fixture-model",
            None,
        ),
    )
    .await;
    assert!(result.is_err());
    let events = h.events();
    let EventData::ProviderAttemptFailed { error_ref, .. } =
        &events.last().expect("failed").body.data
    else {
        panic!("definite failed")
    };
    let diagnostic: ProviderDiagnostic = h.payload(error_ref);
    assert!(matches!(
        diagnostic.protocol_evidence.as_slice(),
        [ProtocolEvidence::Http {
            status_code: 401,
            ..
        }]
    ));
    let DiagnosticDetail::Available { detail_ref } = diagnostic.detail else {
        panic!("error text")
    };
    let detail = String::from_utf8(h.bytes(&detail_ref)).expect("text");
    assert!(detail.contains("[REDACTED]"));
    assert!(!detail.contains("sk-fixture-secret"));
}

#[tokio::test]
async fn session_capture_stream_checkpoints_precede_display() {
    for backend in ["sqlite", "jsonl"] {
        let h = Harness::new(backend);
        let leaf = Leaf {
            store: Arc::clone(&h.store),
            stream: h.stream.clone(),
            error: false,
        };
        let messages = [ChatMessage::user("fixture")];
        scope_provider_capture(Arc::clone(&h.admission), async {
            let mut output = ProviderDispatch::from_ref(&leaf).stream_chat(
                ChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "fixture-model",
                None,
                StreamOptions::default(),
            );
            for _ in 0..3 {
                output.next().await.expect("chunk").expect("success");
                assert_eq!(
                    h.events().last().expect("durable progress").body.kind(),
                    EventKind::ProviderResponseProgressObserved
                );
            }
            assert!(matches!(output.next().await, Some(Ok(StreamEvent::Final))));
            assert!(output.next().await.is_none());
        })
        .await;
        let events = h.events();
        let EventData::ProviderAttemptCompleted { response_ref, .. } =
            &events.last().expect("completed").body.data
        else {
            panic!("completed")
        };
        let response: ProviderResponsePayload = h.payload(response_ref);
        assert_eq!(response.blocks.len(), 3);
        let ResponseBlock::Reasoning {
            payload_ref,
            completion: ContentCompletion::Complete,
            ..
        } = &response.blocks[1]
        else {
            panic!("signed reasoning")
        };
        assert_eq!(h.bytes(payload_ref), b"signed reasoning");
    }
}

#[tokio::test]
async fn classified_nested_failures_and_uncertain_streams_retain_distinct_diagnostics() {
    for backend in ["sqlite", "jsonl"] {
        for (status, code) in [
            (401, "invalid_api_key"),
            (403, "permission_denied"),
            (429, "rate_limit"),
            (500, "server_error"),
            (400, "context_length_exceeded"),
        ] {
            let h = Harness::new(backend);
            let port = h
                .admission
                .admit(ProviderDispatchIntent {
                    requested_route: None,
                    previous_attempt_event_id: None,
                    tool_round_policy: zeroclaw_api::model_provider::ToolRoundPolicy::Batch,
                    thinking: None,
                    provider_ref: "fixture".into(),
                    model: "fixture-model".into(),
                    messages: vec![ChatMessage::user("fixture")],
                    tools: None,
                    temperature: None,
                })
                .await
                .expect("admit");
            let mut recorder = Recorder {
                port,
                blocks: Vec::new(),
                last_progress: None,
                captured_bytes: 0,
                usage_ref: None,
                finish_ref: None,
                served_ref: None,
                adapter_response_observed: false,
                pending_signed_projection: None,
                pending_position: None,
            };
            let typed = crate::api_error_from_parts(
                "fixture",
                reqwest::StatusCode::from_u16(status).expect("status"),
                &serde_json::json!({"error":{"code":code,"message":"fixture failure"}}).to_string(),
            );
            let nested =
                anyhow::Error::new(zeroclaw_api::model_provider::StreamError::ProtocolFailure(
                    typed.reallocate_into_boxed_dyn_error_without_backtrace(),
                ))
                .context("stream adapter");
            assert!(recorder.error(&nested).await.expect("capture"));
            let events = h.events();
            let EventData::ProviderAttemptFailed { error_ref, .. } =
                &events.last().expect("terminal").body.data
            else {
                panic!("definite failure")
            };
            let diagnostic: ProviderDiagnostic = h.payload(error_ref);
            assert_eq!(
                diagnostic.error_key,
                if status == 400 {
                    "provider_context_limit"
                } else {
                    "provider_http_error"
                }
            );
            assert!(
                matches!(&diagnostic.protocol_evidence[..], [ProtocolEvidence::Http {status_code,provider_error_code:Some(observed),..}] if *status_code==status && observed==code)
            );
            assert!(!events.iter().any(|event| matches!(
                event.body.data,
                EventData::InvocationOutcomeUnknown { .. }
            )));
        }
        for body in [
            serde_json::json!({"type":"response.failed","response":{"error":{"code":"context_length_exceeded","message":"fixture limit"}}}),
            serde_json::json!({"type":"error","error":{"type":"overloaded_error","message":"fixture overloaded"}}),
        ] {
            let h = Harness::new(backend);
            let port = h
                .admission
                .admit(ProviderDispatchIntent {
                    requested_route: None,
                    previous_attempt_event_id: None,
                    tool_round_policy: zeroclaw_api::model_provider::ToolRoundPolicy::Batch,
                    thinking: None,
                    provider_ref: "fixture".into(),
                    model: "fixture-model".into(),
                    messages: vec![ChatMessage::user("fixture")],
                    tools: None,
                    temperature: None,
                })
                .await
                .expect("admit");
            let mut recorder = Recorder {
                port,
                blocks: Vec::new(),
                last_progress: None,
                captured_bytes: 0,
                usage_ref: None,
                finish_ref: None,
                served_ref: None,
                adapter_response_observed: false,
                pending_signed_projection: None,
                pending_position: None,
            };
            let typed = crate::dispatch::ProviderProtocolFailure::new(body);
            let code = typed.code().expect("actual code");
            let error = anyhow::Error::new(
                zeroclaw_api::model_provider::StreamError::ProtocolFailure(Box::new(typed)),
            );
            assert!(
                recorder
                    .error(&error)
                    .await
                    .expect("capture explicit failure")
            );
            let events = h.events();
            let EventData::ProviderAttemptFailed { error_ref, .. } =
                &events.last().expect("terminal").body.data
            else {
                panic!("definite protocol failure")
            };
            let diagnostic: ProviderDiagnostic = h.payload(error_ref);
            assert_eq!(
                diagnostic.error_key,
                if code == "context_length_exceeded" {
                    "provider_context_limit"
                } else {
                    "provider_protocol_error"
                }
            );
            assert_eq!(
                diagnostic.protocol_evidence,
                vec![ProtocolEvidence::StreamTermination {
                    evidence: format!("provider_error_code={code}")
                }]
            );
        }
        for (error, key) in [
            (
                anyhow::Error::new(zeroclaw_api::model_provider::StreamError::MissingFinal(
                    "message_stop".into(),
                )),
                "provider_missing_final",
            ),
            (
                anyhow::Error::msg("stream_eof_without_final"),
                "provider_missing_final",
            ),
            (
                anyhow::Error::new(super::super::ProviderAdapterPanic),
                "provider_adapter_panicked",
            ),
            (
                anyhow::Error::new(zeroclaw_api::model_provider::StreamError::InvalidSse(
                    "fixture partial frame".into(),
                )),
                "provider_decode_error",
            ),
            (
                anyhow::Error::new(zeroclaw_api::model_provider::StreamError::ResponseTimeout(
                    "fixture response deadline".into(),
                )),
                "provider_response_timeout",
            ),
            (
                anyhow::Error::new(zeroclaw_api::model_provider::StreamError::ConnectFailed(
                    "fixture connect failure".into(),
                )),
                "provider_connect_error",
            ),
        ] {
            let h = Harness::new(backend);
            let port = h
                .admission
                .admit(ProviderDispatchIntent {
                    requested_route: None,
                    previous_attempt_event_id: None,
                    tool_round_policy: zeroclaw_api::model_provider::ToolRoundPolicy::Batch,
                    thinking: None,
                    provider_ref: "fixture".into(),
                    model: "fixture-model".into(),
                    messages: vec![ChatMessage::user("fixture")],
                    tools: None,
                    temperature: None,
                })
                .await
                .expect("admit");
            let mut recorder = Recorder {
                port,
                blocks: Vec::new(),
                last_progress: None,
                captured_bytes: 0,
                usage_ref: None,
                finish_ref: None,
                served_ref: None,
                adapter_response_observed: false,
                pending_signed_projection: None,
                pending_position: None,
            };
            recorder
                .event(&StreamEvent::TextDelta(
                    zeroclaw_api::model_provider::StreamChunk::delta("partial"),
                ))
                .await
                .expect("durable partial");
            assert!(!recorder.error(&error).await.expect("capture"));
            let events = h.events();
            let EventData::InvocationOutcomeUnknown {
                error_ref: Some(error_ref),
                observed_partial_ref: Some(partial),
                ..
            } = &events.last().expect("terminal").body.data
            else {
                panic!("uncertain partial")
            };
            let diagnostic: ProviderDiagnostic = h.payload(error_ref);
            assert_eq!(diagnostic.error_key, key);
            let response: ProviderResponsePayload = h.payload(partial);
            assert_eq!(response.completion, ContentCompletion::Partial);
            assert!(!response.blocks.is_empty());
            if key == "provider_adapter_panicked" {
                assert!(matches!(
                    diagnostic.detail,
                    DiagnosticDetail::Unavailable { .. }
                ));
            }
        }
    }
}

#[tokio::test]
async fn native_http_header_and_error_frame_evidence_survives_protocol_adapters() {
    use axum::{Router, routing::post};
    let app = Router::new()
        .route("/http", post(|| async {
            (axum::http::StatusCode::TOO_MANY_REQUESTS,
             [("x-request-id", "fixture-request"), ("retry-after", "7")],
             r#"{"error":{"code":"rate_limit_exceeded","message":"fixture failure"}}"#)
        }))
        .route("/stream", post(|| async {
            "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"code\":\"context_length_exceeded\",\"message\":\"fixture limit\"}}}\n\n"
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let address = listener.local_addr().expect("address");
    let server = ::zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    for backend in ["sqlite", "jsonl"] {
        for endpoint in ["http", "stream"] {
            let h = Harness::new(backend);
            let port = h
                .admission
                .admit(ProviderDispatchIntent {
                    requested_route: None,
                    previous_attempt_event_id: None,
                    tool_round_policy: zeroclaw_api::model_provider::ToolRoundPolicy::Batch,
                    thinking: None,
                    provider_ref: "fixture".into(),
                    model: "fixture-model".into(),
                    messages: vec![ChatMessage::user("fixture")],
                    tools: None,
                    temperature: None,
                })
                .await
                .expect("admit");
            let mut recorder = Recorder {
                port,
                blocks: Vec::new(),
                last_progress: None,
                captured_bytes: 0,
                usage_ref: None,
                finish_ref: None,
                served_ref: None,
                adapter_response_observed: false,
                pending_signed_projection: None,
                pending_position: None,
            };
            let request = reqwest::Client::new().post(format!("http://{address}/{endpoint}"));
            let error = if endpoint == "http" {
                crate::api_error("fixture", request.send().await.expect("response")).await
            } else {
                let (tx, mut rx) = tokio::sync::mpsc::channel(8);
                crate::openai::run_responses_sse(
                    request,
                    &tx,
                    false,
                    crate::StreamIdleBound::Fixed(std::time::Duration::from_secs(5)),
                )
                .await;
                anyhow::Error::new(
                    rx.recv()
                        .await
                        .expect("error frame")
                        .expect_err("explicit service error"),
                )
            };
            assert!(recorder.error(&error).await.expect("durable failure"));
            let events = h.events();
            let EventData::ProviderAttemptFailed { error_ref, .. } =
                &events.last().expect("terminal").body.data
            else {
                panic!("explicit failure must not become unknown")
            };
            let diagnostic: ProviderDiagnostic = h.payload(error_ref);
            if endpoint == "http" {
                assert!(
                    matches!(&diagnostic.protocol_evidence[..], [ProtocolEvidence::Http {
                    status_code:429,provider_error_code:Some(code),provider_request_id:Some(id),retry_after:Some(retry),
                }] if code=="rate_limit_exceeded" && id=="fixture-request" && retry=="7")
                );
            } else {
                assert_eq!(diagnostic.error_key, "provider_context_limit");
                assert_eq!(
                    diagnostic.protocol_evidence,
                    vec![ProtocolEvidence::StreamTermination {
                        evidence: "provider_error_code=context_length_exceeded".into()
                    }]
                );
            }
        }
    }
    server.abort();
}

#[tokio::test]
async fn native_capture_limits_stop_before_publishing_incomplete_response() {
    use zeroclaw_api::model_provider::ProviderOutputBlock;
    for backend in ["sqlite", "jsonl"] {
        for blocks in [
            vec![ProviderOutputBlock::Text {
                text: "x".repeat(32 * 1024 * 1024 + 1),
            }],
            vec![ProviderOutputBlock::Text { text: "x".into() }; 16_385],
        ] {
            let h = Harness::new(backend);
            let port = h
                .admission
                .admit(ProviderDispatchIntent {
                    requested_route: None,
                    previous_attempt_event_id: None,
                    tool_round_policy: zeroclaw_api::model_provider::ToolRoundPolicy::Batch,
                    thinking: None,
                    provider_ref: "fixture".into(),
                    model: "fixture".into(),
                    messages: vec![ChatMessage::user("fixture")],
                    tools: None,
                    temperature: None,
                })
                .await
                .expect("admission");
            let mut recorder = Recorder {
                port,
                blocks: vec![],
                last_progress: None,
                captured_bytes: 0,
                usage_ref: None,
                finish_ref: None,
                served_ref: None,
                adapter_response_observed: false,
                pending_signed_projection: None,
                pending_position: None,
            };
            recorder
                .event(&StreamEvent::ThinkingDelta("durable reasoning".into()))
                .await
                .expect("checkpoint");
            let response = ChatResponse {
                text: None,
                tool_calls: vec![],
                reasoning_content: None,
                usage: None,
                output_blocks: blocks,
            };
            let error = recorder
                .response(&response)
                .await
                .expect_err("bounded capture");
            assert!(
                !recorder
                    .error(&error)
                    .await
                    .expect("bounded failure diagnostic")
            );
            let events = h.events();
            let EventData::InvocationOutcomeUnknown {
                observed_partial_ref: Some(partial),
                error_ref: Some(diagnostic),
                ..
            } = &events.last().expect("unknown").body.data
            else {
                panic!("typed capture uncertainty")
            };
            let diagnostic: ProviderDiagnostic = h.payload(diagnostic);
            assert_eq!(diagnostic.error_key, "provider_capture_limit");
            let partial: ProviderResponsePayload = h.payload(partial);
            assert_eq!(
                partial.blocks.len(),
                1,
                "discard undurable response staging"
            );
            assert!(zeroclaw_api::session_events::capture::is_capture_stopped(
                &stop(error)
            ));
            let events = h.events();
            assert!(events.iter().any(|event| matches!(
                event.body.data,
                EventData::ProviderResponseProgressObserved { .. }
            )));
            assert!(!events.iter().any(|event| matches!(
                event.body.data,
                EventData::ProviderAttemptCompleted { .. }
            )));
        }
    }
}

struct OversizedStreamLeaf;
impl Attributable for OversizedStreamLeaf {
    fn role(&self) -> Role {
        Role::System
    }
    fn alias(&self) -> &str {
        "oversized-stream-fixture"
    }
}
#[async_trait::async_trait]
impl ModelProvider for OversizedStreamLeaf {
    async fn chat_with_system(
        &self,
        _system: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        anyhow::bail!("unexpected non-streaming fallback")
    }
    fn stream_chat_with_system(
        &self,
        _system: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
        _options: StreamOptions,
    ) -> stream::BoxStream<'static, StreamResult<zeroclaw_api::model_provider::StreamChunk>> {
        use zeroclaw_api::model_provider::StreamChunk;
        stream::iter([
            Ok(StreamChunk::reasoning("durable reasoning")),
            Ok(StreamChunk::delta("x".repeat(32 * 1024 * 1024 + 1))),
            Ok(StreamChunk::final_chunk()),
        ])
        .boxed()
    }
}

#[tokio::test]
async fn streaming_capture_limit_retains_diagnostic_and_partial_before_returning_error() {
    for backend in ["sqlite", "jsonl"] {
        for structured in [true, false] {
            let h = Harness::new(backend);
            let leaf = OversizedStreamLeaf;
            let messages = [ChatMessage::user("fixture")];
            scope_provider_capture(h.admission.clone(), async {
                let dispatch = ProviderDispatch::from_ref(&leaf);
                let mut output = if structured {
                    dispatch.stream_chat(
                        ChatRequest {
                            messages: &messages,
                            tools: None,
                            thinking: None,
                        },
                        "fixture-model",
                        None,
                        StreamOptions::default(),
                    )
                } else {
                    dispatch
                        .stream_chat_with_history(
                            &messages,
                            "fixture-model",
                            None,
                            StreamOptions::default(),
                        )
                        .map(|item| item.map(StreamEvent::TextDelta))
                        .boxed()
                };
                output.next().await.expect("reasoning").expect("checkpoint");
                let error = output
                    .next()
                    .await
                    .expect("error")
                    .expect_err("capture limit");
                assert!(zeroclaw_api::session_events::capture::is_capture_stopped(
                    &anyhow::Error::new(error)
                ));
                assert!(
                    output.next().await.is_none(),
                    "no final after capture stopped"
                );
                let events = h.events();
                let EventData::InvocationOutcomeUnknown {
                    error_ref: Some(error),
                    observed_partial_ref: Some(partial),
                    ..
                } = &events.last().expect("terminal before returning").body.data
                else {
                    panic!("capture limit must settle with diagnostic and partial")
                };
                let diagnostic: ProviderDiagnostic = h.payload(error);
                assert_eq!(diagnostic.error_key, "provider_capture_limit");
                assert_eq!(diagnostic.stage, DiagnosticStage::Capture);
                let DiagnosticDetail::Available { detail_ref } = diagnostic.detail else {
                    panic!("capture limit detail")
                };
                assert_eq!(h.bytes(&detail_ref), b"session_capture_limit_exceeded");
                let partial: ProviderResponsePayload = h.payload(partial);
                assert_eq!(partial.completion, ContentCompletion::Partial);
                assert_eq!(partial.blocks.len(), 1);
                let ResponseBlock::Reasoning { payload_ref, .. } = &partial.blocks[0] else {
                    panic!("durable reasoning")
                };
                assert_eq!(h.bytes(payload_ref), b"durable reasoning");
                assert_eq!(
                    events
                        .iter()
                        .filter(|event| rules::terminal(&event.body.kind()))
                        .count(),
                    1
                );
            })
            .await;
        }
    }
}

#[tokio::test]
async fn native_partial_response_preserves_observed_finish_evidence() {
    for backend in ["sqlite", "jsonl"] {
        let h = Harness::new(backend);
        let port = h
            .admission
            .admit(ProviderDispatchIntent {
                requested_route: None,
                previous_attempt_event_id: None,
                tool_round_policy: zeroclaw_api::model_provider::ToolRoundPolicy::Batch,
                thinking: None,
                provider_ref: "fixture".into(),
                model: "fixture".into(),
                messages: vec![ChatMessage::user("fixture")],
                tools: None,
                temperature: None,
            })
            .await
            .expect("admission");
        let mut recorder = Recorder {
            port,
            blocks: vec![],
            last_progress: None,
            captured_bytes: 0,
            usage_ref: None,
            finish_ref: None,
            served_ref: None,
            adapter_response_observed: false,
            pending_signed_projection: None,
            pending_position: None,
        };
        recorder
            .event(&StreamEvent::ThinkingDelta("partial reasoning".into()))
            .await
            .expect("checkpoint");
        let evidence = serde_json::json!({"kind":"finish","finish_reason":"length"});
        recorder
            .event(&StreamEvent::ProtocolMetadata(evidence.clone()))
            .await
            .expect("evidence");
        recorder
            .error(&anyhow::Error::new(
                zeroclaw_api::model_provider::StreamError::MissingFinal("[DONE]".into()),
            ))
            .await
            .expect("unknown");
        let events = h.events();
        let EventData::InvocationOutcomeUnknown {
            observed_partial_ref: Some(reference),
            ..
        } = &events.last().expect("terminal").body.data
        else {
            panic!("unknown partial")
        };
        let response: ProviderResponsePayload = h.payload(reference);
        assert_eq!(response.completion, ContentCompletion::Partial);
        let actual: serde_json::Value = h.payload(
            response
                .finish_evidence_ref
                .as_ref()
                .expect("observed evidence"),
        );
        assert_eq!(actual, evidence);
    }
}

#[tokio::test]
async fn native_finish_metadata_is_separate_from_content_and_checkpoint_bytes_are_reused() {
    for backend in ["sqlite", "jsonl"] {
        let h = Harness::new(backend);
        let port = h
            .admission
            .admit(ProviderDispatchIntent {
                requested_route: None,
                previous_attempt_event_id: None,
                tool_round_policy: zeroclaw_api::model_provider::ToolRoundPolicy::Batch,
                thinking: None,
                provider_ref: "fixture".into(),
                model: "fixture".into(),
                messages: vec![ChatMessage::user("fixture")],
                tools: None,
                temperature: None,
            })
            .await
            .expect("admission");
        let mut recorder = Recorder {
            port,
            blocks: vec![],
            last_progress: None,
            captured_bytes: 0,
            usage_ref: None,
            finish_ref: None,
            served_ref: None,
            adapter_response_observed: false,
            pending_signed_projection: None,
            pending_position: None,
        };
        recorder
            .event(&StreamEvent::TextDelta(
                zeroclaw_api::model_provider::StreamChunk::delta("字节片段"),
            ))
            .await
            .expect("checkpoint");
        let event = h.events().last().expect("progress").clone();
        let EventData::ProviderResponseProgressObserved { observation_ref } = event.body.data
        else {
            panic!("progress");
        };
        let observation: ProviderObservationPayload = h.payload(&observation_ref);
        let bytes_ref = observation
            .operations
            .iter()
            .find_map(|operation| match operation {
                ProviderObservation::BytesAppended {
                    block_index: 0,
                    byte_offset: 0,
                    component,
                    bytes_ref,
                } if component == "text" => Some(bytes_ref.clone()),
                _ => None,
            })
            .expect("byte coordinates");
        assert_eq!(h.bytes(&bytes_ref), "字节片段".as_bytes());
        let evidence = serde_json::json!({"kind":"finish", "finish_reason":"length"});
        recorder
            .event(&StreamEvent::ProtocolMetadata(evidence.clone()))
            .await
            .expect("actual finish");
        recorder
            .event(&StreamEvent::ProtocolMetadata(
                serde_json::json!({"kind":"completion_signal", "signal":"[DONE]"}),
            ))
            .await
            .expect("boundary");
        recorder
            .event(&StreamEvent::Final)
            .await
            .expect("completion");
        let event = h.events().last().expect("completed").clone();
        let EventData::ProviderAttemptCompleted { response_ref, .. } = event.body.data else {
            panic!("completed");
        };
        let response: ProviderResponsePayload = h.payload(&response_ref);
        assert_eq!(response.blocks.len(), 1);
        assert!(
            matches!(&response.blocks[0], ResponseBlock::Text { text_ref } if text_ref == &bytes_ref)
        );
        assert_eq!(
            h.payload::<serde_json::Value>(
                response.finish_evidence_ref.as_ref().expect("evidence")
            ),
            evidence
        );

        let h = Harness::new(backend);
        let port = h
            .admission
            .admit(ProviderDispatchIntent {
                requested_route: None,
                previous_attempt_event_id: None,
                tool_round_policy: zeroclaw_api::model_provider::ToolRoundPolicy::Batch,
                thinking: None,
                provider_ref: "fixture".into(),
                model: "fixture".into(),
                messages: vec![ChatMessage::user("fixture")],
                tools: None,
                temperature: None,
            })
            .await
            .expect("admission");
        let mut recorder = Recorder {
            port,
            blocks: vec![],
            last_progress: None,
            captured_bytes: 0,
            usage_ref: None,
            finish_ref: None,
            served_ref: None,
            adapter_response_observed: false,
            pending_signed_projection: None,
            pending_position: None,
        };
        recorder
            .response(&ChatResponse {
                output_blocks: vec![
                    zeroclaw_api::model_provider::ProviderOutputBlock::ProtocolMetadata {
                        metadata: evidence.clone(),
                    },
                ],
                text: Some("legacy content".into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: Some("retained reasoning".into()),
            })
            .await
            .expect("response");
        let event = h.events().last().expect("completed").clone();
        let EventData::ProviderAttemptCompleted { response_ref, .. } = event.body.data else {
            panic!("completed");
        };
        let response: ProviderResponsePayload = h.payload(&response_ref);
        assert_eq!(response.blocks.len(), 2);
        assert_eq!(
            h.payload::<serde_json::Value>(
                response.finish_evidence_ref.as_ref().expect("evidence")
            ),
            evidence
        );
    }
}

#[tokio::test]
async fn native_stream_does_not_dispatch_before_durable_admission_or_when_unpolled() {
    use zeroclaw_api::session_events::capture::ProviderAdmissionPort;
    struct Gate {
        admitted: Arc<dyn ProviderAdmissionPort>,
        ready: tokio::sync::Semaphore,
    }
    #[async_trait::async_trait]
    impl ProviderAdmissionPort for Gate {
        async fn admit(
            &self,
            intent: ProviderDispatchIntent,
        ) -> anyhow::Result<Arc<dyn ProviderObservationPort>> {
            self.ready.acquire().await?.forget();
            self.admitted.admit(intent).await
        }
    }
    let _proxy_guard = crate::RuntimeProxyTestGuard::acquire().await;
    for backend in ["sqlite", "jsonl"] {
        let h = Harness::new(backend);
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = calls.clone();
        let store = h.store.clone();
        let stream = h.stream.clone();
        let app = axum::Router::new().route("/chat/completions", axum::routing::post(move || {
            let calls=seen.clone(); let store=store.clone(); let stream=stream.clone();
            async move {
                let events=store.read_page(&stream,&EventCursor::beginning(stream.clone()),None,100).expect("history");
                assert!(events.events.iter().any(|e| e.body.kind()==EventKind::ProviderAttemptStarted), "request must follow durable start");
                calls.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
                "data: {\"choices\":[{\"delta\":{\"content\":\"fixture\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");
        let server = ::zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app).await.expect("server");
        });
        let provider = crate::compatible::OpenAiCompatibleModelProvider::builder("fixture")
            .display_name("fixture")
            .auth_style(crate::compatible::AuthStyle::Bearer)
            .base_url(&format!("http://{address}"))
            .credential(None)
            .build();
        let gate = Arc::new(Gate {
            admitted: h.admission.clone(),
            ready: tokio::sync::Semaphore::new(0),
        });
        let messages = [ChatMessage::user("fixture")];
        scope_provider_capture(gate.clone(), async {
            let make = || {
                ProviderDispatch::from_ref(&provider).stream_chat(
                    ChatRequest {
                        messages: &messages,
                        tools: None,
                        thinking: None,
                    },
                    "fixture",
                    None,
                    StreamOptions {
                        enabled: true,
                        ..StreamOptions::default()
                    },
                )
            };
            drop(make());
            tokio::task::yield_now().await;
            assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
            let mut output = make();
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(100), output.next())
                    .await
                    .is_err(),
                "blocked admission"
            );
            assert_eq!(
                calls.load(std::sync::atomic::Ordering::SeqCst),
                0,
                "no request while waiting for durable admission"
            );
            gate.ready.add_permits(1);
            let mut completed = false;
            while let Some(event) = output.next().await {
                completed |= matches!(event.expect("event"), StreamEvent::Final);
            }
            assert!(completed);
            assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        })
        .await;
        server.abort();
    }
}
