use super::*;
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};

fn fixture(mode: &str) -> (Vec<Value>, Vec<Value>, Vec<&'static str>, &'static str) {
    let done = |output_index, content_index, text| json!({"type":"response.output_text.done","item_id":"same-message-id","output_index":output_index,"content_index":content_index,"text":text});
    match mode {
        "done_only" => (
            vec![
                done(1, 0, "A"),
                done(0, 1, "A"),
                done(0, 0, "A"),
                done(0, 0, "A"),
            ],
            vec![
                json!({"type":"message","id":"same-message-id","content":[{"type":"output_text","text":"A"},{"type":"output_text","text":"A"}]}),
                json!({"type":"message","id":"same-message-id","content":[{"type":"output_text","text":"A"}]}),
            ],
            vec!["A", "A", "A"],
            "AAA",
        ),
        "mixed_suffix" => (
            vec![
                json!({"type":"response.output_text.delta","item_id":"same-message-id","output_index":0,"content_index":0,"delta":"A"}),
                json!({"type":"response.output_text.delta","item_id":"same-message-id","output_index":0,"content_index":1,"delta":"B"}),
                json!({"type":"response.output_text.delta","item_id":"same-message-id","output_index":0,"content_index":0,"delta":"\u{4e2d}"}),
                done(0, 0, "A\u{4e2d}\u{1f642}"),
                done(0, 0, "A\u{4e2d}\u{1f642}"),
                done(1, 0, "A"),
            ],
            vec![
                json!({"type":"message","id":"same-message-id","content":[{"type":"output_text","text":"A\u{4e2d}\u{1f642}"},{"type":"output_text","text":"B"}]}),
                json!({"type":"message","id":"same-message-id","content":[{"type":"output_text","text":"A"}]}),
            ],
            vec!["A", "\u{4e2d}", "\u{1f642}", "B", "A"],
            "AB\u{4e2d}\u{1f642}A",
        ),
        "legacy" => (
            vec![
                json!({"type":"response.output_text.done","text":"legacy"}),
                json!({"type":"response.output_text.done","text":"legacy"}),
            ],
            vec![json!({"type":"message","content":[{"type":"output_text","text":"legacy"}]})],
            vec!["legacy"],
            "legacy",
        ),
        "item_suffix" => (
            vec![
                json!({"type":"response.output_text.delta","item_id":"same-message-id","output_index":0,"content_index":0,"delta":"A"}),
                json!({"type":"response.output_item.done","output_index":0,"item":{"type":"message","id":"same-message-id","content":[{"type":"output_text","text":"A\u{4e2d}"},{"type":"output_text","text":"B"}]}}),
                json!({"type":"response.output_item.done","output_index":0,"item":{"type":"message","id":"same-message-id","content":[{"type":"output_text","text":"A\u{4e2d}\u{1f642}"},{"type":"output_text","text":"B"}]}}),
            ],
            vec![
                json!({"type":"message","id":"same-message-id","content":[{"type":"output_text","text":"A\u{4e2d}\u{1f642} tail"},{"type":"output_text","text":"B"}]}),
            ],
            vec!["A", "\u{4e2d}", "B", "\u{1f642}"],
            "A\u{4e2d}B\u{1f642}",
        ),
        _ => panic!("unknown fixture"),
    }
}

fn bytes(store: &dyn SessionEventStore, reference: &StoredPayloadRef) -> Vec<u8> {
    let PayloadRead::Present(record) = store
        .read_payload(&reference.payload_id)
        .expect("stored payload")
    else {
        panic!("payload must remain present");
    };
    record.body
}

#[tokio::test]
async fn codex_text_done_interruption_preserves_parts_both_backends() {
    for backend in ["sqlite", "jsonl"] {
        for mode in [
            "done_only",
            "mixed_suffix",
            "legacy",
            "item_suffix",
            "mismatch",
            "large_mismatch",
        ] {
            let endings: &[&str] = if mode.ends_with("mismatch") {
                &["mismatch"]
            } else {
                &[
                    "eof",
                    "transport",
                    "failed",
                    "error",
                    "completed",
                    "invalid_utf8",
                    "terminal_invalid_utf8",
                    "terminal_incomplete_utf8",
                    "terminal_unterminated",
                    "terminal_unterminated_one_newline",
                    "terminal_unterminated_crlf",
                    "terminal_unterminated_crlf_one_newline",
                ]
            };
            for &ending in endings {
                for streaming in [false, true] {
                    audit(backend, mode, ending, streaming).await;
                }
            }
        }
    }
}

async fn wait_for_text(h: &Harness, text: &str) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            for event in h.events() {
                let EventData::ProviderResponseProgressObserved { observation_ref } =
                    event.body.data
                else {
                    continue;
                };
                let observation: ProviderObservationPayload = h.payload(&observation_ref);
                for operation in observation.operations {
                    if let ProviderObservation::ProtocolSegmentObserved { segment_ref, .. } =
                        operation
                    {
                        let block: ResponseBlock = h.payload(&segment_ref);
                        if let ResponseBlock::Text { text_ref } = block
                            && bytes(h.store.as_ref(), &text_ref) == text.as_bytes()
                        {
                            return;
                        }
                    }
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("complete CRLF frame must be durable before the next frame is sent");
}

#[tokio::test]
async fn codex_crlf_frames_are_durable_before_connection_close_both_backends() {
    for backend in ["sqlite", "jsonl"] {
        for streaming in [false, true] {
            for split_crlf in [false, true] {
                let h = Harness::new(backend);
                let (tail_tx, tail_rx) = tokio::sync::oneshot::channel::<()>();
                let (terminal_tx, terminal_rx) = tokio::sync::oneshot::channel::<()>();
                let gates = Arc::new(tokio::sync::Mutex::new(Some((tail_rx, terminal_rx))));
                let app = axum::Router::new().route(
                    "/responses",
                    axum::routing::post(move || {
                        let gates = gates.clone();
                        async move {
                            let (tail_rx, terminal_rx) = gates.lock().await.take().expect("one request");
                            let mut prefix = format!(
                                ": keepalive\r\n\r\ndata: {}\r\n\r\ndata: {}\r\n\r\n",
                                json!({"type":"response.output_text.delta","item_id":"message","output_index":0,"content_index":0,"delta":"A"}),
                                json!({"type":"response.output_text.done","item_id":"message","output_index":0,"content_index":1,"text":"B"}),
                            );
                            let tail = if split_crlf {
                                assert_eq!(prefix.pop(), Some('\n'));
                                "\n"
                            } else {
                                ""
                            };
                            let terminal = format!(
                                "data: {}\r\n\r\n: keepalive\r\n\r\n",
                                json!({"type":"response.completed","response":{"status":"completed","output":[{"type":"message","id":"message","content":[{"type":"output_text","text":"A"},{"type":"output_text","text":"B"}]}]}}),
                            );
                            let body = futures_util::stream::once(async move {
                                Ok::<_, std::io::Error>(axum::body::Bytes::from(prefix))
                            })
                            .chain(futures_util::stream::once(async move {
                                tail_rx.await.expect("release CRLF tail");
                                Ok(axum::body::Bytes::from_static(tail.as_bytes()))
                            }))
                            .chain(futures_util::stream::once(async move {
                                terminal_rx.await.expect("release terminal frame");
                                Ok(axum::body::Bytes::from(terminal))
                            }))
                            .chain(futures_util::stream::pending::<Result<axum::body::Bytes, std::io::Error>>());
                            axum::response::Response::builder()
                                .header("content-type", "text/event-stream; charset=utf-8")
                                .body(axum::body::Body::from_stream(body))
                                .expect("response")
                        }
                    }),
                );
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                    .await
                    .expect("listener");
                let address = listener.local_addr().expect("address");
                let server = zeroclaw_spawn::spawn!(async move {
                    axum::serve(listener, app).await.expect("server");
                });
                let provider = zeroclaw_providers::openai_codex::OpenAiCodexModelProvider::new(
                    "fixture",
                    &zeroclaw_providers::ModelProviderRuntimeOptions {
                        provider_api_url: Some(format!("http://{address}")),
                        zeroclaw_dir: Some(h._dir.path().to_path_buf()),
                        secrets_encrypt: false,
                        ..Default::default()
                    },
                    Some("fixture-key"),
                )
                .expect("provider");
                let admission = h.admission.clone();
                let mut request = zeroclaw_spawn::spawn!(async move {
                    scope_provider_capture(admission, async {
                        let messages = [ChatMessage::user("fixture")];
                        let dispatch = ProviderDispatch::from_ref(&provider);
                        let request = ChatRequest {
                            messages: &messages,
                            tools: None,
                            thinking: None,
                        };
                        if streaming {
                            let mut response = dispatch.stream_chat(
                                request,
                                "fixture",
                                None,
                                StreamOptions::new(true),
                            );
                            let mut text = String::new();
                            while let Some(event) = response.next().await {
                                if let StreamEvent::TextDelta(chunk) = event? {
                                    text.push_str(&chunk.delta);
                                }
                            }
                            Ok::<_, anyhow::Error>(text)
                        } else {
                            Ok(dispatch
                                .chat(request, "fixture", None)
                                .await?
                                .text
                                .expect("text"))
                        }
                    })
                    .await
                });
                wait_for_text(&h, "A").await;
                if split_crlf {
                    assert_eq!(
                        h.events()
                            .iter()
                            .filter(|event| event.body.kind()
                                == EventKind::ProviderResponseProgressObserved)
                            .count(),
                        1,
                        "incomplete second frame must not be guessed",
                    );
                }
                tail_tx.send(()).expect("release tail");
                wait_for_text(&h, "B").await;
                assert!(!request.is_finished(), "terminal not sent yet");
                terminal_tx.send(()).expect("release terminal");
                let result =
                    tokio::time::timeout(std::time::Duration::from_secs(2), &mut request).await;
                if result.is_err() {
                    request.abort();
                    let _ = request.await;
                    server.abort();
                    let _ = server.await;
                    panic!("complete CRLF terminal must finish while the HTTP body remains open");
                }
                assert_eq!(
                    result
                        .unwrap()
                        .expect("request task")
                        .expect("provider result"),
                    "AB"
                );
                let events = h.events();
                assert_eq!(
                    events
                        .iter()
                        .filter(|event| event.body.kind() == EventKind::ProviderAttemptCompleted)
                        .count(),
                    1
                );
                assert_eq!(
                    events
                        .iter()
                        .filter(|event| event.body.kind() == EventKind::InvocationOutcomeUnknown)
                        .count(),
                    0
                );
                let reopened = open_event_store(h._dir.path(), backend).expect("reopen capture");
                assert_eq!(
                    reopened.head(&h.stream).unwrap(),
                    h.store.head(&h.stream).unwrap()
                );
                let replay = reopened
                    .read_page(
                        &h.stream,
                        &EventCursor::beginning(h.stream.clone()),
                        None,
                        1000,
                        4 * 1024 * 1024,
                    )
                    .expect("reopened events");
                assert_eq!(replay.events, events);
                server.abort();
                assert!(server.await.expect_err("server aborted").is_cancelled());
            }
        }
    }
}

async fn audit(backend: &str, mode: &'static str, ending: &str, streaming: bool) {
    let h = Harness::new(backend);
    let mismatch = mode.ends_with("mismatch");
    let (mut frames, output, mut expected, mut expected_text) = if mismatch {
        let text = if mode == "large_mismatch" {
            format!(
                "wrong token=fixture-secretMismatch {}",
                "z".repeat(80 * 1024)
            )
        } else {
            "wrong token=fixture-secretMismatch".into()
        };
        (
            vec![
                json!({"type":"response.output_text.delta","item_id":"same-message-id","output_index":0,"content_index":0,"delta":"\u{4e2d}"}),
                json!({"type":"response.output_text.done","item_id":"same-message-id","output_index":0,"content_index":0,"text":text,"api_key":"fixture-secretApiKey","token":{"value":"fixture-secretToken"}}),
            ],
            vec![],
            vec!["\u{4e2d}"],
            "\u{4e2d}",
        )
    } else {
        fixture(mode)
    };
    let terminal_frame = ending == "completed" || ending.starts_with("terminal_");
    let invalid_utf8 = ending.contains("utf8");
    let unfinished_frame = ending.contains("unterminated");
    if mode == "item_suffix" && terminal_frame && !unfinished_frame {
        expected.push(" tail");
        expected_text = "A\u{4e2d}B\u{1f642} tail";
    }
    if mode == "mixed_suffix" && terminal_frame && !unfinished_frame {
        // A parsed terminal adds item-level metadata, making the mixed
        // coordinates ambiguous. Capture then retains observation order.
        expected = vec!["A", "B", "\u{4e2d}", "\u{1f642}", "A"];
    }
    match ending {
        "failed" => frames.push(json!({"type":"response.failed","response":{"error":{"code":"fixture_failed","message":"fixture failed after done"}}})),
        "error" => frames.push(json!({"type":"error","code":"fixture_error","message":"fixture error after done"})),
        _ if terminal_frame => frames.push(json!({"type":"response.completed","response":{"status":"completed","output":output}})),
        _ => {}
    }
    let mut body = frames
        .into_iter()
        .map(|frame| format!("data: {frame}\n\n"))
        .collect::<String>();
    if unfinished_frame {
        body.truncate(
            body.len()
                - if ending.ends_with("one_newline") {
                    1
                } else {
                    2
                },
        );
    }
    if ending.contains("crlf") {
        body = body.replace('\n', "\r\n");
    }
    let mut body = body.into_bytes();
    if ending == "terminal_incomplete_utf8" {
        body.extend_from_slice(&[0xf0, 0x9f]);
    } else if invalid_utf8 {
        body.push(0xff);
    }
    let transport = ending == "transport";
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let app = axum::Router::new().route(
        "/responses",
        axum::routing::post(move || {
            seen.fetch_add(1, Ordering::SeqCst);
            let body = body.clone();
            async move {
                let body = if transport {
                    axum::body::Body::from_stream(
                        futures_util::stream::once(async move {
                            Ok::<_, std::io::Error>(axum::body::Bytes::from(body))
                        })
                        .chain(futures_util::stream::once(async {
                            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                            Err(std::io::Error::other("fixture interrupted transport"))
                        })),
                    )
                } else {
                    axum::body::Body::from(body)
                };
                axum::response::Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(body)
                    .expect("response")
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let address = listener.local_addr().expect("address");
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app).await.expect("server");
    });
    let provider = zeroclaw_providers::openai_codex::OpenAiCodexModelProvider::new(
        "fixture",
        &zeroclaw_providers::ModelProviderRuntimeOptions {
            provider_api_url: Some(format!("http://{address}")),
            zeroclaw_dir: Some(h._dir.path().to_path_buf()),
            secrets_encrypt: false,
            ..Default::default()
        },
        Some("fixture-key"),
    )
    .expect("provider");
    let messages = [ChatMessage::user("fixture")];
    let result = scope_provider_capture(h.admission.clone(), async {
        let dispatch = ProviderDispatch::from_ref(&provider);
        let request = ChatRequest {
            messages: &messages,
            tools: None,
            thinking: None,
        };
        if streaming {
            let mut stream =
                dispatch.stream_chat(request, "fixture", None, StreamOptions::new(true));
            let mut text = String::new();
            while let Some(event) = stream.next().await {
                if let StreamEvent::TextDelta(chunk) = event? {
                    text.push_str(&chunk.delta);
                }
            }
            Ok::<_, anyhow::Error>(text)
        } else {
            Ok(dispatch
                .chat(request, "fixture", None)
                .await?
                .text
                .expect("text"))
        }
    })
    .await;
    assert_eq!(
        result.is_ok(),
        ending == "completed",
        "{backend} {mode} {ending} streaming={streaming}: {result:?}"
    );
    if let Ok(text) = result {
        assert_eq!(text, expected_text);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1, "one physical attempt");
    let events = h.events();
    let terminals: Vec<_> = events
        .iter()
        .filter(|event| {
            matches!(
                event.body.kind(),
                EventKind::ProviderAttemptCompleted
                    | EventKind::ProviderAttemptFailed
                    | EventKind::InvocationOutcomeUnknown
            )
        })
        .collect();
    assert_eq!(terminals.len(), 1, "one terminal outcome");
    let (response_ref, error_ref) = match &terminals[0].body.data {
        EventData::ProviderAttemptCompleted { response_ref, .. } => {
            assert_eq!(ending, "completed");
            (response_ref, None)
        }
        EventData::ProviderAttemptFailed {
            partial_response_ref,
            error_ref,
            ..
        } => {
            assert!(matches!(ending, "failed" | "error"));
            (
                partial_response_ref.as_ref().expect("partial response"),
                Some(error_ref),
            )
        }
        EventData::InvocationOutcomeUnknown {
            observed_partial_ref,
            error_ref,
            ..
        } => {
            assert!(
                matches!(ending, "eof" | "transport" | "mismatch")
                    || invalid_utf8
                    || unfinished_frame
            );
            (
                observed_partial_ref.as_ref().expect("partial response"),
                error_ref.as_ref(),
            )
        }
        _ => unreachable!(),
    };
    let response: ProviderResponsePayload = h.payload(response_ref);
    assert_eq!(
        response.completion,
        if ending == "completed" {
            ContentCompletion::Complete
        } else {
            ContentCompletion::Partial
        }
    );
    let texts: Vec<_> = semantic_blocks(h.store.as_ref(), &response.blocks)
        .map(|block| match block {
            ResponseBlock::Text { text_ref } => {
                String::from_utf8(bytes(h.store.as_ref(), text_ref)).expect("text bytes")
            }
            other => panic!("unexpected block: {other:?}"),
        })
        .collect();
    assert_eq!(
        texts, expected,
        "{backend} {mode} {ending} streaming={streaming}"
    );
    let mut offsets = std::collections::HashMap::new();
    let mut segments = 0;
    for event in &events {
        if let EventData::ProviderResponseProgressObserved { observation_ref } = &event.body.data {
            let observation: ProviderObservationPayload = h.payload(observation_ref);
            for operation in observation.operations {
                if let ProviderObservation::ProtocolSegmentObserved {
                    position,
                    component,
                    byte_offset,
                    segment_ref,
                } = operation
                {
                    let block: ResponseBlock = h.payload(&segment_ref);
                    assert_eq!(component, block.component().0);
                    let next = offsets.entry((position, component)).or_insert(0);
                    assert_eq!(byte_offset, *next);
                    *next += block.component().1.byte_len;
                    if matches!(block, ResponseBlock::Text { .. }) {
                        segments += 1;
                    }
                }
            }
        }
    }
    assert_eq!(segments, if mode == "legacy" { 0 } else { expected.len() });
    if let Some(error_ref) = error_ref {
        let diagnostic: ProviderDiagnostic = h.payload(error_ref);
        assert_eq!(
            diagnostic.error_key,
            match ending {
                "eof" => "provider_missing_final",
                "failed" | "error" => "provider_protocol_error",
                _ if mismatch || invalid_utf8 || unfinished_frame => "provider_decode_error",
                _ => "provider_outcome_unresolved",
            }
        );
        assert_eq!(
            diagnostic.stage,
            if mismatch || invalid_utf8 || unfinished_frame {
                DiagnosticStage::Decode
            } else {
                DiagnosticStage::ConsumeStream
            }
        );
        let DiagnosticDetail::Available { detail_ref } = &diagnostic.detail else {
            panic!("diagnostic detail retained");
        };
        let detail = String::from_utf8(bytes(h.store.as_ref(), detail_ref)).expect("detail bytes");
        assert!(detail.len() <= 64 * 1024 + b" [truncated]".len());
        if mode == "large_mismatch" {
            assert!(detail.ends_with(" [truncated]"));
        }
        if mismatch {
            assert!(detail.contains("response.output_text.done"));
            assert!(detail.contains("fixture-secretMismatch"));
            assert!(detail.contains("fixture-secretApiKey"));
            if mode != "large_mismatch" {
                assert!(detail.contains("fixture-secretToken"));
            }
        } else if matches!(ending, "failed" | "error") {
            assert!(detail.contains(&format!("fixture_{ending}")));
        } else if unfinished_frame {
            assert!(detail.contains("response.completed"));
        }
    } else {
        assert_eq!(ending, "completed");
    }
    let reopened = open_event_store(h._dir.path(), backend).expect("reopen capture");
    assert_eq!(
        reopened.head(&h.stream).unwrap(),
        h.store.head(&h.stream).unwrap()
    );
    let replay = reopened
        .read_page(
            &h.stream,
            &EventCursor::beginning(h.stream.clone()),
            None,
            1000,
            4 * 1024 * 1024,
        )
        .expect("reopened events");
    assert_eq!(replay.events, events);
    let restored: ProviderResponsePayload =
        serde_json::from_slice(&bytes(reopened.as_ref(), response_ref)).expect("reopened response");
    assert_eq!(restored, response);
    for block in &restored.blocks {
        let (_, reference) = block.component();
        assert!(!bytes(reopened.as_ref(), reference).is_empty());
    }
    if let Some(error_ref) = error_ref {
        let diagnostic: ProviderDiagnostic =
            serde_json::from_slice(&bytes(reopened.as_ref(), error_ref))
                .expect("reopened diagnostic");
        let DiagnosticDetail::Available { detail_ref } = diagnostic.detail else {
            panic!("reopened diagnostic detail retained");
        };
        assert!(!bytes(reopened.as_ref(), &detail_ref).is_empty());
    }
    server.abort();
    assert!(server.await.expect_err("server aborted").is_cancelled());
}
