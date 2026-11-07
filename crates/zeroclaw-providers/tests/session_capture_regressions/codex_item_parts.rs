use super::*;
use futures_util::StreamExt;
use serde_json::{Value, json};

fn bytes(store: &dyn SessionEventStore, reference: &StoredPayloadRef) -> Vec<u8> {
    let PayloadRead::Present(record) = store.read_payload(&reference.payload_id).unwrap() else {
        panic!("fixture payload must remain present");
    };
    record.body
}

async fn capture(
    backend: &str,
    streaming: bool,
    mut frames: Vec<Value>,
    output: Vec<Value>,
    ending: &str,
) -> (Harness, anyhow::Result<String>) {
    let h = Harness::new(backend);
    match ending {
        "completed" => frames.push(json!({
            "type":"response.completed", "response":{"status":"completed","output":output}
        })),
        "error" => frames.push(json!({
            "type":"error","code":"fixture_interrupted_parts","message":"fixture parts interrupted"
        })),
        "eof" => {}
        _ => panic!("unknown fixture ending"),
    }
    let body = frames
        .into_iter()
        .map(|frame| format!("data: {frame}\n\n"))
        .collect::<String>();
    let app = axum::Router::new().route(
        "/responses",
        axum::routing::post(move || {
            let body = body.clone();
            async move {
                axum::response::Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(axum::body::Body::from(body))
                    .unwrap()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let provider = zeroclaw_providers::openai_codex::OpenAiCodexModelProvider::new(
        "parts-fixture",
        &zeroclaw_providers::ModelProviderRuntimeOptions {
            provider_api_url: Some(format!("http://{address}")),
            zeroclaw_dir: Some(h._dir.path().to_path_buf()),
            secrets_encrypt: false,
            ..Default::default()
        },
        Some("fixture-key"),
    )
    .unwrap();
    let result = scope_provider_capture(h.admission.clone(), async {
        let dispatch = ProviderDispatch::from_ref(&provider);
        let messages = [ChatMessage::user("fixture")];
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
            Ok(text)
        } else {
            Ok(dispatch
                .chat(request, "fixture", None)
                .await?
                .text
                .unwrap_or_default())
        }
    })
    .await;
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
    (h, result)
}

fn response<'a>(
    h: &Harness,
    events: &'a [SessionEvent],
    ending: &str,
) -> (&'a StoredPayloadRef, ProviderResponsePayload) {
    let terminals: Vec<_> = events
        .iter()
        .filter(|event| {
            matches!(
                event.body.data,
                EventData::ProviderAttemptCompleted { .. }
                    | EventData::ProviderAttemptFailed { .. }
                    | EventData::InvocationOutcomeUnknown { .. }
            )
        })
        .collect();
    assert_eq!(terminals.len(), 1, "one terminal outcome");
    let reference = match &terminals[0].body.data {
        EventData::ProviderAttemptCompleted { response_ref, .. } if ending == "completed" => {
            response_ref
        }
        EventData::ProviderAttemptFailed {
            partial_response_ref: Some(reference),
            ..
        } if ending == "error" => reference,
        EventData::InvocationOutcomeUnknown {
            observed_partial_ref: Some(reference),
            ..
        } if ending == "eof" => reference,
        other => panic!("unexpected terminal for {ending}: {other:?}"),
    };
    let response: ProviderResponsePayload = h.payload(reference);
    assert_eq!(
        response.completion,
        if ending == "completed" {
            ContentCompletion::Complete
        } else {
            ContentCompletion::Partial
        }
    );
    (reference, response)
}

fn assert_reopened(
    h: &Harness,
    backend: &str,
    events: &[SessionEvent],
    reference: &StoredPayloadRef,
    response: &ProviderResponsePayload,
) {
    let reopened = open_event_store(h._dir.path(), backend).unwrap();
    let page = reopened
        .read_page(
            &h.stream,
            &EventCursor::beginning(h.stream.clone()),
            None,
            1000,
        )
        .unwrap();
    assert_eq!(page.events, events);
    let restored: ProviderResponsePayload =
        serde_json::from_slice(&bytes(&*reopened, reference)).unwrap();
    assert_eq!(&restored, response);
    for block in &restored.blocks {
        let (_, reference) = block.component();
        assert_eq!(bytes(&*reopened, reference), bytes(&*h.store, reference));
    }
}

#[tokio::test]
async fn codex_late_coordinates_preserve_observed_order_both_backends() {
    for backend in ["sqlite", "jsonl"] {
        for streaming in [false, true] {
            for identity in ["item_id", "unattributed", "interleaved"] {
                for ending in ["completed", "eof", "error"] {
                    let mut delta = json!({"type":"response.output_text.delta","delta":"A"});
                    if identity != "unattributed" {
                        delta["item_id"] = json!("message-main");
                    }
                    let mut frames = vec![delta];
                    if identity == "interleaved" {
                        frames.push(json!({
                            "type":"response.output_text.delta","item_id":"message-other",
                            "output_index":1,"content_index":0,"delta":"Z"
                        }));
                    }
                    let done = json!({
                        "type":"response.output_text.done","item_id":"message-main",
                        "output_index":0,"content_index":0,"text":"A\u{4e2d}"
                    });
                    frames.extend([done.clone(), done]);
                    let item = json!({
                        "type":"message","id":"message-main",
                        "content":[{"type":"output_text","text":"A\u{4e2d}"}]
                    });
                    frames.push(json!({
                        "type":"response.output_item.done","output_index":0,"item":item
                    }));
                    let mut output = vec![item];
                    if identity == "interleaved" {
                        output.push(json!({
                            "type":"message","id":"message-other",
                            "content":[{"type":"output_text","text":"Z"}]
                        }));
                    }
                    let (h, result) = capture(backend, streaming, frames, output, ending).await;
                    assert_eq!(result.is_ok(), ending == "completed");
                    if let Ok(text) = result {
                        assert_eq!(
                            text,
                            if identity == "interleaved" {
                                "AZ\u{4e2d}"
                            } else {
                                "A\u{4e2d}"
                            }
                        );
                    }
                    let events = h.events();
                    let (reference, response) = response(&h, &events, ending);
                    let text: Vec<_> = semantic_blocks(h.store.as_ref(), &response.blocks)
                        .map(|block| match block {
                            ResponseBlock::Text { text_ref } => {
                                String::from_utf8(bytes(&*h.store, text_ref)).unwrap()
                            }
                            other => panic!("unexpected late-coordinate block: {other:?}"),
                        })
                        .collect();
                    assert_eq!(
                        text,
                        if identity == "interleaved" {
                            vec!["A", "Z", "\u{4e2d}"]
                        } else {
                            vec!["A", "\u{4e2d}"]
                        },
                        "{backend} {identity} {ending} streaming={streaming}"
                    );
                    assert_reopened(&h, backend, &events, reference, &response);
                }
            }
        }
    }
}

#[tokio::test]
async fn codex_later_item_parts_preserve_media_and_unknown_updates_both_backends() {
    for backend in ["sqlite", "jsonl"] {
        for streaming in [false, true] {
            for delivery in ["terminal", "item_then_terminal", "changed_parts"] {
                let first = json!({
                    "type":"message","id":"message-main",
                    "content":[{"type":"output_text","text":"A"}]
                });
                let mut expanded = first.clone();
                expanded["content"].as_array_mut().unwrap().extend([
                    json!({"type":"output_image","result":"AQID","format":"png"}),
                    json!({"type":"output_audio","data":"BAUG","format":"wav"}),
                    json!({"type":"fixture.future_part","value":"initial","token":"fixture-secret"}),
                ]);
                let mut changed = expanded.clone();
                changed["content"][1]["result"] = json!("BwgJ");
                changed["content"][3]["value"] = json!("updated");
                let mut frames = vec![json!({
                    "type":"response.output_item.done","output_index":0,"item":first
                })];
                if delivery != "terminal" {
                    let done = json!({
                        "type":"response.output_item.done","output_index":0,"item":expanded
                    });
                    frames.extend([done.clone(), done]);
                }
                let output = if delivery == "changed_parts" {
                    let done = json!({
                        "type":"response.output_item.done","output_index":0,"item":changed
                    });
                    frames.extend([done.clone(), done]);
                    vec![changed]
                } else {
                    vec![expanded]
                };
                let (h, result) = capture(backend, streaming, frames, output, "completed").await;
                assert_eq!(result.unwrap(), "A");
                let events = h.events();
                let (reference, response) = response(&h, &events, "completed");
                let mut projected = Vec::new();
                for block in semantic_blocks(h.store.as_ref(), &response.blocks) {
                    match block {
                        ResponseBlock::Text { text_ref } => {
                            projected.push(String::from_utf8(bytes(&*h.store, text_ref)).unwrap());
                        }
                        ResponseBlock::Media {
                            media_descriptor_ref,
                            received: MediaReceived::Captured { representation_ref },
                        } => {
                            let descriptor: Value = h.payload(media_descriptor_ref);
                            let payload = bytes(&*h.store, representation_ref);
                            projected.push(match payload.as_slice() {
                                [1, 2, 3] => {
                                    assert_eq!(descriptor["media_type"], "image");
                                    "image initial".into()
                                }
                                [4, 5, 6] => {
                                    assert_eq!(descriptor["media_type"], "audio");
                                    "audio".into()
                                }
                                [7, 8, 9] => "image updated".into(),
                                other => panic!("unexpected media bytes: {other:?}"),
                            });
                        }
                        ResponseBlock::Other {
                            type_name,
                            classified_body_ref,
                        } => {
                            assert_eq!(type_name, "fixture.future_part");
                            let value: Value = h.payload(classified_body_ref);
                            assert_eq!(value["token"], "fixture-secret");
                            projected.push(format!("unknown {}", value["value"].as_str().unwrap()));
                        }
                        other => panic!("unexpected late-part block: {other:?}"),
                    }
                }
                assert_eq!(
                    projected,
                    if delivery == "changed_parts" {
                        vec![
                            "A",
                            "image initial",
                            "audio",
                            "unknown initial",
                            "image updated",
                            "unknown updated",
                        ]
                    } else {
                        vec!["A", "image initial", "audio", "unknown initial"]
                    },
                    "{backend} {delivery} streaming={streaming}"
                );
                let mut offsets = std::collections::HashMap::new();
                for event in &events {
                    if let EventData::ProviderResponseProgressObserved { observation_ref } =
                        &event.body.data
                    {
                        let observed: ProviderObservationPayload = h.payload(observation_ref);
                        for operation in observed.operations {
                            if let ProviderObservation::ProtocolSegmentObserved {
                                position,
                                component,
                                byte_offset,
                                segment_ref,
                            } = operation
                            {
                                let expected = offsets.entry((position, component)).or_insert(0);
                                assert_eq!(byte_offset, *expected);
                                let block: ResponseBlock = h.payload(&segment_ref);
                                *expected += h.payload_len(block.component().1);
                            }
                        }
                    }
                }
                assert_reopened(&h, backend, &events, reference, &response);
            }
        }
    }
}
