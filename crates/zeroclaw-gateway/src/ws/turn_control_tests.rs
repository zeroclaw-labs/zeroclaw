use super::*;

// Preserve 4 MiB of real TCP pressure without making the fixture a benchmark
// of cumulative-prefix sanitization. Each projected chunk stays below 64 KiB.
const LARGE_STREAM_CHUNKS: usize = 128;
const LARGE_STREAM_CHUNK_BYTES: usize = 32 * 1024;
const LARGE_STREAM_PROVIDER_SECS: u64 = 30;
// Provider timeout and runtime settlement are distinct phases. Keep one finite
// recovery deadline with room for draining/sanitizing after the provider ends.
const LARGE_STREAM_SETTLE: Duration = Duration::from_secs(2 * LARGE_STREAM_PROVIDER_SECS);

/// Marker-only behavior in the real HTTP provider. No Conversation injection,
/// no fake gateway output: all frames pass through the runtime streaming path.
pub(super) async fn provider_response(request: &Value) -> Option<axum::response::Response> {
    let text = request["messages"].as_array()?.last()?["content"].to_string();
    if text.contains("s2-delete-marker") {
        let message = json!({"role":"assistant","content":"s2-deleted-output-marker "});
        return Some(if request["stream"] == true {
            ([("content-type", "text/event-stream")], format!("data: {}\n\ndata: [DONE]\n\n", json!({"id":"deletion-response","choices":[{"index":0,"delta":message,"finish_reason":"stop"}]}))).into_response()
        } else {
            axum::Json(json!({"id":"deletion-response","choices":[{"index":0,"message":message,"finish_reason":"stop"}]})).into_response()
        });
    }
    if text.contains("s2-held-provider") {
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    if text.contains("s2-held-provider-error") {
        return Some((
            axum::http::StatusCode::BAD_REQUEST,
            axum::Json(json!({"error":{"message":"synthetic provider rejected input","type":"invalid_request_error"}})),
        ).into_response());
    }
    if !text.contains("s2-large-stream") || request["stream"] != true {
        return None;
    }
    let stream = futures_util::stream::unfold(0usize, |index| async move {
        if index > LARGE_STREAM_CHUNKS {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
        let bytes = if index == LARGE_STREAM_CHUNKS {
            "data: [DONE]\n\n".to_string()
        } else {
            format!(
                "data: {}\n\n",
                json!({"id":"s2-local-stream","choices":[{"index":0,"delta":{"content":format!("{} ", "x".repeat(LARGE_STREAM_CHUNK_BYTES - 1))},"finish_reason":null}]})
            )
        };
        Some((Ok::<_, std::io::Error>(bytes), index + 1))
    });
    Some(
        (
            [("content-type", "text/event-stream")],
            axum::body::Body::from_stream(stream),
        )
            .into_response(),
    )
}

async fn negotiate(client: &mut DeviceWire) -> String {
    client
        .send(json!({"type":"turn_control","version":1,"op":"negotiate"}))
        .await;
    let ready = client.until("turn_ready").await;
    assert_eq!(ready["version"], 1);
    let epoch = ready["epoch"].as_str().unwrap().to_owned();
    assert!(uuid::Uuid::parse_str(&epoch).is_ok());
    epoch
}

fn request(epoch: &str, id: &str, content: &str) -> Value {
    json!({"type":"turn_control","version":1,"op":"message","epoch":epoch,"request_id":id,"content":content})
}

fn steer(epoch: &str, request: &str, parent: &str, turn: &str, content: &str) -> Value {
    json!({"type":"turn_control","version":1,"op":"steer","epoch":epoch,
        "request_id":request,"parent_request_id":parent,"turn_id":turn,"content":content})
}

async fn negotiate_steering(client: &mut DeviceWire) -> String {
    client
        .send(json!({"type":"turn_control","version":1,"op":"negotiate","capabilities":["steer"]}))
        .await;
    let ready = client.until("turn_ready").await;
    assert_eq!(ready["capabilities"], json!(["steer"]));
    assert_eq!(ready["busy"], "reject");
    ready["epoch"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn s2_explicit_steering_requires_opt_in_scopes_and_deduplicates_receipts() {
    let fixture = DeviceFixture::new(&["chat", "cancel"]).await;
    let mut client = fixture.socket(DEVICE_TOKEN).await;
    let epoch = negotiate(&mut client).await;
    let parent = uuid::Uuid::new_v4().to_string();
    client
        .send(request(&epoch, &parent, "s2-held-provider"))
        .await;
    let admitted = client.until("turn_ack").await;
    let turn = admitted["turn_id"].as_str().unwrap();
    wait_provider(&fixture, 1).await;
    let id = uuid::Uuid::new_v4().to_string();
    let correction = steer(&epoch, &id, &parent, turn, "use the corrected destination");
    client.send(correction.clone()).await;
    assert_eq!(
        client.until("turn_error").await["code"],
        "STEERING_NOT_NEGOTIATED"
    );
    assert_eq!(negotiate_steering(&mut client).await, epoch);

    let mut wrong = correction.clone();
    wrong["epoch"] = json!(uuid::Uuid::new_v4().to_string());
    client.send(wrong).await;
    assert_eq!(client.until("turn_error").await["code"], "EPOCH_MISMATCH");
    let mut wrong = correction.clone();
    wrong["turn_id"] = json!(uuid::Uuid::new_v4().to_string());
    client.send(wrong).await;
    assert_eq!(client.until("turn_steer_ack").await["status"], "stale");
    let mut wrong = correction.clone();
    wrong["parent_request_id"] = json!(uuid::Uuid::new_v4().to_string());
    client.send(wrong).await;
    assert_eq!(client.until("turn_steer_ack").await["status"], "unknown");
    let mut forged = correction.clone();
    forged["source"] = json!("owner");
    client.send(forged).await;
    assert_eq!(
        client.until("turn_error").await["code"],
        "UNSUPPORTED_FIELD"
    );

    // Another authenticated subject in the same conversation cannot steer an
    // admission that belongs to the device, even when it knows all public IDs.
    let mut other = fixture.socket(OWNER_TOKEN).await;
    negotiate_steering(&mut other).await;
    other.send(correction.clone()).await;
    assert_eq!(other.until("turn_steer_ack").await["status"], "unknown");

    client.send(correction.clone()).await;
    let accepted = client.until("turn_steer_ack").await;
    assert_eq!(accepted["status"], "accepted");
    assert_eq!(accepted["parent_request_id"], parent);
    client.send(correction.clone()).await;
    assert_eq!(client.until("turn_steer_ack").await["status"], "duplicate");
    let mut conflict = correction.clone();
    conflict["content"] = json!("different correction");
    client.send(conflict).await;
    assert_eq!(client.until("turn_steer_ack").await["status"], "conflict");
    client
        .send(request(
            &epoch,
            &uuid::Uuid::new_v4().to_string(),
            "ordinary busy message",
        ))
        .await;
    assert_eq!(client.until("turn_ack").await["status"], "busy");

    terminal(&mut client, &parent, turn).await;
    assert_eq!(
        fixture.seen.lock().len(),
        2,
        "one accepted correction creates one continuation"
    );
    client.send(correction).await;
    assert_eq!(client.until("turn_steer_ack").await["status"], "duplicate");
    client
        .send(steer(
            &epoch,
            &uuid::Uuid::new_v4().to_string(),
            &parent,
            turn,
            "too late",
        ))
        .await;
    assert_eq!(client.until("turn_steer_ack").await["status"], "terminal");
}

#[tokio::test]
async fn s2_explicit_steering_preserves_bridge_attachment_gate() {
    let fixture = DeviceFixture::new(&["chat"]).await;
    let mut client = fixture.socket(DEVICE_TOKEN).await;
    let epoch = negotiate_steering(&mut client).await;
    let mut correction = steer(
        &epoch,
        &uuid::Uuid::new_v4().to_string(),
        &uuid::Uuid::new_v4().to_string(),
        &uuid::Uuid::new_v4().to_string(),
        "caption",
    );
    correction["attachments"] = json!([uuid::Uuid::new_v4().to_string()]);
    client.send(correction).await;
    assert_eq!(
        client.until("error").await["code"],
        "BRIDGE_OPERATION_DENIED"
    );
    assert!(fixture.seen.lock().is_empty());
}

fn assert_failed_parent_fallback(attempts: &[Value]) {
    // call_provider retries a failed, output-free stream as ordinary chat.
    // Both attempts belong to the SAME parent input; neither is a new turn.
    assert_eq!(
        attempts.len(),
        2,
        "one stream attempt and its non-streaming fallback: {attempts:?}"
    );
    assert_eq!(attempts[0]["stream"], true);
    assert_ne!(attempts[1]["stream"], true);
    assert_eq!(
        attempts[0]["messages"], attempts[1]["messages"],
        "fallback must retain the exact parent transcript"
    );
    let messages = attempts[1]["messages"].as_array().unwrap();
    let last = messages.last().unwrap();
    assert_eq!(last["role"], "user");
    assert!(
        last["content"]
            .as_str()
            .unwrap()
            .ends_with("s2-held-provider-error"),
        "both attempts must still address the failed parent: {last}"
    );
}

#[tokio::test]
async fn s2_explicit_steering_is_not_promoted_after_provider_error() {
    let fixture = DeviceFixture::new(&["chat"]).await;
    let mut client = fixture.socket(DEVICE_TOKEN).await;
    let epoch = negotiate_steering(&mut client).await;
    let parent = uuid::Uuid::new_v4().to_string();
    client
        .send(request(&epoch, &parent, "s2-held-provider-error"))
        .await;
    let admitted = client.until("turn_ack").await;
    let turn = admitted["turn_id"].as_str().unwrap();
    wait_provider(&fixture, 1).await;
    client
        .send(steer(
            &epoch,
            &uuid::Uuid::new_v4().to_string(),
            &parent,
            turn,
            "must stay on the failed turn",
        ))
        .await;
    assert_eq!(client.until("turn_steer_ack").await["status"], "accepted");
    let events = terminal(&mut client, &parent, turn).await;
    assert_eq!(events.last().unwrap()["event"]["outcome"], "error");
    let failed_attempts = fixture.seen.lock().clone();
    assert_failed_parent_fallback(&failed_attempts);
    assert!(
        tokio::time::timeout(
            Duration::from_millis(500),
            wait_provider(&fixture, failed_attempts.len() + 1)
        )
        .await
        .is_err(),
        "unread explicit steering must not start a successor or inject into another turn"
    );
    assert_eq!(
        *fixture.seen.lock(),
        failed_attempts,
        "no request may follow the failed parent's fallback"
    );

    // A separate explicit admission remains possible; the scoped fence only
    // discards unread steering from the failed target.
    let next = uuid::Uuid::new_v4().to_string();
    client
        .send(request(&epoch, &next, "new explicit task"))
        .await;
    let admitted = client.until("turn_ack").await;
    assert_eq!(admitted["status"], "accepted");
    terminal(&mut client, &next, admitted["turn_id"].as_str().unwrap()).await;
    let seen = fixture.seen.lock();
    assert_eq!(seen.len(), failed_attempts.len() + 1);
    let next_messages = seen.last().unwrap()["messages"].as_array().unwrap();
    assert!(
        next_messages.last().unwrap()["content"]
            .as_str()
            .unwrap()
            .ends_with("new explicit task")
    );
    assert!(
        !next_messages.iter().any(|message| message["content"]
            .to_string()
            .contains("must stay on the failed turn")),
        "discarded steering must not leak into the next explicit turn"
    );
}

#[tokio::test]
async fn legacy_late_steering_still_follows_up_after_provider_error() {
    let fixture = DeviceFixture::new(&["chat"]).await;
    let mut client = fixture.socket(DEVICE_TOKEN).await;
    client.send(json!({"type":"message","id":"legacy-failed-parent","content":"s2-held-provider-error"})).await;
    assert_eq!(client.until("ack").await["turn"], "started");
    wait_provider(&fixture, 1).await;
    client
        .send(json!({"type":"message","id":"legacy-follow-up","content":"legacy correction"}))
        .await;
    assert_eq!(client.until("ack").await["turn"], "steered");
    client.until("error").await;
    client.until("done").await;
    let seen = fixture.seen.lock();
    assert_eq!(
        seen.len(),
        3,
        "two parent transport attempts and one legacy follow-up"
    );
    assert_failed_parent_fallback(&seen[..2]);
    let follow_up = seen[2]["messages"].as_array().unwrap().last().unwrap();
    assert_eq!(follow_up["role"], "user");
    assert!(
        follow_up["content"]
            .as_str()
            .unwrap()
            .ends_with("legacy correction"),
        "the third request must be the legacy follow-up, not another attempt of the parent: {follow_up}"
    );
}

async fn terminal(client: &mut DeviceWire, id: &str, turn: &str) -> Vec<Value> {
    let mut events = Vec::new();
    tokio::time::timeout(LIMIT, async {
        loop {
            let frame = client.next_with_limit(32 * 1024 * 1024).await.unwrap();
            if frame["type"] != "turn_event" {
                assert!(
                    !matches!(frame["type"].as_str(), Some("turn_error" | "turn_gap")),
                    "{frame}"
                );
                continue;
            }
            assert_eq!(frame["request_id"], id);
            assert_eq!(frame["turn_id"], turn);
            assert_eq!(frame["seq"].as_u64(), Some(events.len() as u64 + 1));
            let finished = frame["terminal"] == true;
            events.push(frame);
            if finished {
                break;
            }
        }
    })
    .await
    .expect("turn terminal must be bounded");
    events
}

async fn wait_provider(fixture: &DeviceFixture, count: usize) {
    tokio::time::timeout(LIMIT, async {
        while fixture.seen.lock().len() < count {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn s2_wire_normal_turn_has_correlated_chunks_and_terminal() {
    let fixture = DeviceFixture::new(&["chat"]).await;
    let mut client = fixture.socket(DEVICE_TOKEN).await;
    let epoch = negotiate(&mut client).await;
    let id = uuid::Uuid::new_v4().to_string();
    client.send(request(&epoch, &id, "hello S2")).await;
    let ack = client.until("turn_ack").await;
    assert_eq!(ack["status"], "accepted");
    assert_eq!(ack["request_id"], id);
    assert_eq!(ack["epoch"], epoch);
    let turn = ack["turn_id"].as_str().unwrap();
    let events = terminal(&mut client, &id, turn).await;
    assert!(events.iter().any(|event| event["event"]["type"] == "chunk"));
    assert!(events.iter().any(|event| event["event"]["type"] == "done"));
    assert_eq!(events.last().unwrap()["event"]["outcome"], "done");
    assert_eq!(events.last().unwrap()["stream_complete"], true);
    assert_eq!(fixture.seen.lock().len(), 1);
}

#[tokio::test]
async fn s2_wire_busy_refuses_without_steering_and_late_cancel_is_scoped() {
    let fixture = DeviceFixture::new(&["chat", "cancel"]).await;
    let mut client = fixture.socket(DEVICE_TOKEN).await;
    let epoch = negotiate(&mut client).await;
    let first = uuid::Uuid::new_v4().to_string();
    client.send(request(&epoch, &first, "first")).await;
    let first_ack = client.until("turn_ack").await;
    let old_turn = first_ack["turn_id"].as_str().unwrap();
    terminal(&mut client, &first, old_turn).await;
    let second = uuid::Uuid::new_v4().to_string();
    client
        .send(request(&epoch, &second, "s2-held-provider"))
        .await;
    let second_ack = client.until("turn_ack").await;
    let new_turn = second_ack["turn_id"].as_str().unwrap();
    assert_ne!(old_turn, new_turn);
    wait_provider(&fixture, 2).await;
    let refused = uuid::Uuid::new_v4().to_string();
    client
        .send(request(&epoch, &refused, "must not steer"))
        .await;
    let busy = tokio::time::timeout(Duration::from_secs(1), client.until("turn_ack"))
        .await
        .unwrap();
    assert_eq!(busy["status"], "busy");
    assert_eq!(busy["request_id"], refused);
    client.send(json!({"type":"turn_control","version":1,"op":"cancel","epoch":epoch,"request_id":first,"turn_id":old_turn})).await;
    let cancel = client.until("turn_cancel_ack").await;
    assert_eq!(
        cancel["status"], "stale",
        "old turn must not be cancelled as current"
    );
    let events = terminal(&mut client, &second, new_turn).await;
    assert_eq!(events.last().unwrap()["event"]["outcome"], "done");
    assert_eq!(
        fixture.seen.lock().len(),
        2,
        "busy request must not become steering or queued input"
    );
}

#[tokio::test]
async fn s2_wire_lost_ack_reconnect_recovers_mapping_without_reexecution() {
    let fixture = DeviceFixture::new(&["chat"]).await;
    let mut lost = fixture.socket(DEVICE_TOKEN).await;
    let epoch = negotiate(&mut lost).await;
    let id = uuid::Uuid::new_v4().to_string();
    lost.send(request(&epoch, &id, "s2-held-provider")).await;
    wait_provider(&fixture, 1).await;
    // Deliberately never consume the ACK. Disconnect is not cancellation.
    drop(lost);
    let mut recovered = fixture.socket(DEVICE_TOKEN).await;
    assert_eq!(negotiate(&mut recovered).await, epoch);
    recovered
        .send(request(&epoch, &id, "s2-held-provider"))
        .await;
    let duplicate = recovered.until("turn_ack").await;
    assert_eq!(duplicate["status"], "duplicate");
    let turn = duplicate["turn_id"].as_str().unwrap().to_string();
    recovered.send(json!({"type":"turn_control","version":1,"op":"resume","epoch":epoch,"request_id":id,"after_seq":0})).await;
    let replay = recovered.until("turn_replay").await;
    assert_eq!(replay["turn_id"], turn);
    assert_eq!(replay["gap"], false);
    assert_eq!(fixture.seen.lock().len(), 1);
    // A second reconnect after settlement also retains the same identity.
    tokio::time::sleep(Duration::from_secs(3)).await;
    drop(recovered);
    let mut settled = fixture.socket(DEVICE_TOKEN).await;
    assert_eq!(negotiate(&mut settled).await, epoch);
    settled.send(json!({"type":"turn_control","version":1,"op":"resume","epoch":epoch,"request_id":id,"after_seq":0})).await;
    let replay = settled.until("turn_replay").await;
    assert_eq!(replay["request_id"], id);
    assert_eq!(replay["turn_id"], turn);
    assert_eq!(replay["terminal"], true);
    assert_eq!(replay["gap"], false);
    let events = replay["events"].as_array().unwrap();
    assert!(!events.is_empty());
    for (index, event) in events.iter().enumerate() {
        assert_eq!(event["request_id"], id);
        assert_eq!(event["turn_id"], turn);
        assert_eq!(event["seq"], index as u64 + 1);
    }
    assert_eq!(events.last().unwrap()["terminal"], true);
    assert_eq!(fixture.seen.lock().len(), 1);
    settled
        .send(request(&epoch, &id, "different payload must not run"))
        .await;
    assert_eq!(settled.until("turn_ack").await["status"], "conflict");
    assert_eq!(fixture.seen.lock().len(), 1);
}

#[tokio::test]
async fn s2_wire_unknown_version_epoch_and_unnegotiated_controls_are_inert() {
    let fixture = DeviceFixture::new(&["chat"]).await;
    let mut client = fixture.socket(DEVICE_TOKEN).await;
    let id = uuid::Uuid::new_v4().to_string();
    client
        .send(request(
            &uuid::Uuid::new_v4().to_string(),
            &id,
            "not negotiated",
        ))
        .await;
    assert_eq!(
        client.until("turn_error").await["code"],
        "NEGOTIATION_REQUIRED"
    );
    client
        .send(json!({"type":"turn_control","version":99,"op":"negotiate"}))
        .await;
    assert_eq!(
        client.until("turn_error").await["code"],
        "UNSUPPORTED_VERSION"
    );
    let epoch = negotiate(&mut client).await;
    for _ in 0..2 {
        client.send(json!({"type":"turn_control","version":1,"op":"future_action","epoch":epoch,"request_id":id,"content":"no side effects"})).await;
        assert_eq!(
            client.until("turn_error").await["code"],
            "UNKNOWN_OPERATION"
        );
    }
    client
        .send(request(
            &uuid::Uuid::new_v4().to_string(),
            &id,
            "old process must not revive",
        ))
        .await;
    assert_eq!(client.until("turn_error").await["code"], "EPOCH_MISMATCH");
    assert!(fixture.seen.lock().is_empty());
    client
        .send(request(&epoch, &id, "valid after rejected frames"))
        .await;
    let ack = client.until("turn_ack").await;
    assert_eq!(ack["status"], "accepted");
    terminal(&mut client, &id, ack["turn_id"].as_str().unwrap()).await;
    assert_eq!(
        fixture.seen.lock().len(),
        1,
        "unknown frames must not reserve or poison the request ID"
    );
}

#[tokio::test]
async fn s2_wire_control_authority_and_live_revoke_preserve_s1() {
    let fixture = DeviceFixture::new(&["chat"]).await;
    let mut client = fixture.socket(DEVICE_TOKEN).await;
    let epoch = negotiate(&mut client).await;
    let id = uuid::Uuid::new_v4().to_string();
    client.send(request(&epoch, &id, "s2-held-provider")).await;
    let ack = client.until("turn_ack").await;
    client.send(json!({"type":"turn_control","version":1,"op":"cancel","epoch":epoch,"request_id":id,"turn_id":ack["turn_id"]})).await;
    let denied = client.next().await.unwrap();
    assert_eq!(denied["code"], "BRIDGE_OPERATION_DENIED");
    terminal(&mut client, &id, ack["turn_id"].as_str().unwrap()).await;
    fixture
        .state
        .config
        .write()
        .gateway
        .bridges
        .remove("device");
    client
        .send(request(
            &epoch,
            &uuid::Uuid::new_v4().to_string(),
            "revoked",
        ))
        .await;
    client.denied().await;
    assert_eq!(fixture.seen.lock().len(), 1);
}

#[tokio::test]
async fn s2_wire_negotiation_does_not_change_legacy_subscriber_contract() {
    let fixture = DeviceFixture::new(&["chat"]).await;
    let mut controlled = fixture.socket(DEVICE_TOKEN).await;
    let mut legacy = fixture.socket(OWNER_TOKEN).await;
    let epoch = negotiate(&mut controlled).await;
    let id = uuid::Uuid::new_v4().to_string();
    controlled
        .send(request(&epoch, &id, "coexisting subscribers"))
        .await;
    let ack = controlled.until("turn_ack").await;
    let mut chunk_seen = false;
    let mut done_seen = false;
    for _ in 0..64 {
        let frame = legacy.next().await.unwrap();
        assert!(
            !matches!(
                frame["type"].as_str(),
                Some("turn_ready" | "turn_event" | "turn_ack" | "turn_replay" | "turn_terminal")
            ),
            "extension leaked to unnegotiated subscriber: {frame}"
        );
        if frame["type"] == "chunk" {
            chunk_seen = true;
            assert!(frame.get("turn_id").is_none());
            assert!(frame.get("seq").is_none());
        }
        if frame["type"] == "done" {
            done_seen = true;
            break;
        }
    }
    assert!(
        chunk_seen && done_seen,
        "legacy output must contain both chunk and done"
    );
    terminal(&mut controlled, &id, ack["turn_id"].as_str().unwrap()).await;
    fixture
        .send_turn(
            &mut legacy,
            "legacy-after-negotiation",
            "legacy still works",
        )
        .await;
    assert_eq!(fixture.seen.lock().len(), 2);
}

#[tokio::test]
async fn s2_wire_slow_reader_has_explicit_recovery_gap_without_blocking_other_session() {
    let fixture = DeviceFixture::new(&["chat"]).await;
    fixture
        .state
        .config
        .write()
        .providers
        .models
        .custom
        .get_mut("test")
        .unwrap()
        .base
        .timeout_secs = Some(LARGE_STREAM_PROVIDER_SECS);
    let mut slow = fixture.socket(DEVICE_TOKEN).await;
    let epoch = negotiate(&mut slow).await;
    let id = uuid::Uuid::new_v4().to_string();
    let socket = socket2::SockRef::from(&slow.0);
    socket.set_recv_buffer_size(1024).unwrap();
    assert!(
        socket.recv_buffer_size().unwrap() <= 65_536,
        "test requires a genuinely small receive buffer"
    );
    slow.send(request(&epoch, &id, "s2-large-stream")).await;
    wait_provider(&fixture, 1).await;
    // No more reads on this real TCP socket while 4 MiB of whitespace-ended
    // deltas traverse runtime/Conversation. The verified small receive buffer
    // ensures transport backpressure; whitespace makes sanitized chunks visible.
    let (status, mut independent) = DeviceWire::upgrade(
        fixture.address,
        "agent=device-agent&session_id=s2-independent-session",
        OWNER_TOKEN,
    )
    .await;
    assert_eq!(status, 101);
    assert_eq!(
        independent.until("session_start").await["session_id"],
        "s2-independent-session"
    );
    independent.send(json!({"type":"connect"})).await;
    independent.until("connected").await;
    tokio::time::timeout(Duration::from_secs(3), async {
        independent
            .send(json!({"type":"message","id":"independent-turn","content":"unrelated fast turn"}))
            .await;
        assert_eq!(independent.until("ack").await["status"], "accepted");
        assert_eq!(
            independent.until("done").await["full_response"],
            "device response"
        );
    })
    .await
    .expect("slow reader must not block unrelated Conversation");
    let mut recovery = fixture.socket(DEVICE_TOKEN).await;
    assert_eq!(negotiate(&mut recovery).await, epoch);
    assert_eq!(
        LARGE_STREAM_CHUNKS * LARGE_STREAM_CHUNK_BYTES,
        4 * 1024 * 1024
    );
    let mut snapshot = Value::Null;
    let mut live_events = 0usize;
    let mut last_sequence = 0u64;
    let settled = tokio::time::timeout(LARGE_STREAM_SETTLE, async {
        loop {
            recovery.send(json!({"type":"turn_control","version":1,"op":"resume","epoch":epoch,"request_id":id,"after_seq":0})).await;
            // Live output can race the replay response. Bound each frame while
            // looking for the authoritative cache snapshot.
            loop {
                let frame = recovery.next_with_limit(32 * 1024 * 1024).await.unwrap();
                if frame["type"] == "turn_replay" { snapshot = frame; break; }
            }
            if snapshot["terminal"] == true { break; }
            // Keep this recovery reader draining while the intentionally slow
            // socket remains unread. Do not create an artificial second lagger.
            loop {
                let frame = recovery.next_with_limit(32 * 1024 * 1024).await.unwrap();
                assert_ne!(frame["type"], "turn_gap", "draining recovery client fell behind");
                if frame["type"] == "turn_event" {
                    live_events += 1;
                    last_sequence = frame["seq"].as_u64().unwrap();
                    if frame["terminal"] == true { break; }
                }
            }
        }
    }).await;
    assert!(
        settled.is_ok(),
        "bounded streamed turn must settle within {LARGE_STREAM_SETTLE:?}; live_events={live_events}, last_sequence={last_sequence}, snapshot_terminal={}, snapshot_last_seq={}",
        snapshot["terminal"],
        snapshot["last_seq"]
    );
    assert_eq!(snapshot["request_id"], id);
    assert_eq!(
        snapshot["gap"], true,
        "overflow must not claim complete replay"
    );
    assert_eq!(snapshot["terminal"], true);
    assert_eq!(
        snapshot["events"].as_array().unwrap().last().unwrap()["event"]["outcome"],
        "done",
        "a provider timeout must not masquerade as successful stream settlement"
    );
    assert_eq!(
        snapshot["events"].as_array().unwrap().last().unwrap()["stream_complete"],
        false,
        "terminal must not describe a truncated replay as complete"
    );
    assert!(
        snapshot["last_seq"].as_u64().unwrap()
            > snapshot["events"].as_array().unwrap().len() as u64
    );
    assert_eq!(
        fixture.seen.lock().len(),
        2,
        "recovery must never execute again"
    );
    drop(slow);
}

#[tokio::test]
async fn s2_wire_targeted_cancel_and_negotiated_legacy_bypass_are_bounded() {
    let fixture = DeviceFixture::new(&["chat", "cancel"]).await;
    let mut client = fixture.socket(DEVICE_TOKEN).await;
    let epoch = negotiate(&mut client).await;
    let id = uuid::Uuid::new_v4().to_string();
    client.send(request(&epoch, &id, "s2-held-provider")).await;
    let ack = client.until("turn_ack").await;
    wait_provider(&fixture, 1).await;
    client.send(json!({"type":"cancel"})).await;
    assert_eq!(
        client.until("turn_error").await["code"],
        "UNKNOWN_OPERATION"
    );
    client
        .send(json!({"type":"message","content":"legacy steering bypass"}))
        .await;
    assert_eq!(
        client.until("turn_error").await["code"],
        "UNKNOWN_OPERATION"
    );
    client.send(json!({"type":"turn_control","version":1,"op":"cancel","epoch":epoch,"request_id":id,"turn_id":ack["turn_id"]})).await;
    assert_eq!(client.until("turn_cancel_ack").await["status"], "requested");
    let events = terminal(&mut client, &id, ack["turn_id"].as_str().unwrap()).await;
    assert_eq!(events.last().unwrap()["event"]["outcome"], "aborted");
    assert_eq!(fixture.seen.lock().len(), 1);
}

#[tokio::test]
async fn s2_wire_request_recovery_is_bound_to_authenticated_principal() {
    let fixture = DeviceFixture::new(&["chat"]).await;
    let mut device = fixture.socket(DEVICE_TOKEN).await;
    let epoch = negotiate(&mut device).await;
    let id = uuid::Uuid::new_v4().to_string();
    device
        .send(request(&epoch, &id, "private correlation"))
        .await;
    let ack = device.until("turn_ack").await;
    terminal(&mut device, &id, ack["turn_id"].as_str().unwrap()).await;
    let mut owner = fixture.socket(OWNER_TOKEN).await;
    assert_eq!(negotiate(&mut owner).await, epoch);
    for _ in 0..2 {
        owner.send(json!({"type":"turn_control","version":1,"op":"resume","epoch":epoch,"request_id":id,"after_seq":0})).await;
        let reply = owner.until("turn_ack").await;
        assert_eq!(reply["status"], "unknown");
        assert!(reply["turn_id"].is_null());
    }
    assert_eq!(
        fixture.seen.lock().len(),
        1,
        "unknown recovery cannot repeat execution"
    );
}

#[tokio::test]
async fn s2_wire_concurrent_duplicate_has_one_admission_and_one_turn() {
    let fixture = DeviceFixture::new(&["chat"]).await;
    let mut first = fixture.socket(DEVICE_TOKEN).await;
    let mut second = fixture.socket(DEVICE_TOKEN).await;
    let epoch = negotiate(&mut first).await;
    assert_eq!(negotiate(&mut second).await, epoch);
    let id = uuid::Uuid::new_v4().to_string();
    tokio::join!(
        first.send(request(&epoch, &id, "s2-held-provider")),
        second.send(request(&epoch, &id, "s2-held-provider"))
    );
    let (a, b) = tokio::join!(first.until("turn_ack"), second.until("turn_ack"));
    let mut statuses = [a["status"].as_str().unwrap(), b["status"].as_str().unwrap()];
    statuses.sort_unstable();
    assert_eq!(statuses, ["accepted", "duplicate"]);
    assert_eq!(a["turn_id"], b["turn_id"]);
    assert_eq!(a["request_id"], id);
    assert_eq!(b["request_id"], id);
    let turn = a["turn_id"].as_str().unwrap();
    let (a_events, b_events) = tokio::join!(
        terminal(&mut first, &id, turn),
        terminal(&mut second, &id, turn)
    );
    assert_eq!(a_events, b_events);
    assert_eq!(fixture.seen.lock().len(), 1);
}

#[tokio::test]
async fn s2_wire_session_delete_resets_shared_history_and_preserves_other_sessions() {
    let fixture = DeviceFixture::new(&["chat"]).await;
    let mut first = fixture.socket(DEVICE_TOKEN).await;
    let mut second = fixture.socket(DEVICE_TOKEN).await;
    let epoch = negotiate(&mut first).await;
    assert_eq!(negotiate(&mut second).await, epoch);
    let old_id = uuid::Uuid::new_v4().to_string();
    first
        .send(request(&epoch, &old_id, "s2-delete-marker"))
        .await;
    let ack = first.until("turn_ack").await;
    let turn = ack["turn_id"].as_str().unwrap();
    terminal(&mut first, &old_id, turn).await;
    terminal(&mut second, &old_id, turn).await;

    let (status, mut other) = DeviceWire::upgrade(
        fixture.address,
        "agent=device-agent&session_id=unaffected-session",
        OWNER_TOKEN,
    )
    .await;
    assert_eq!(status, 101);
    assert_eq!(
        other.until("session_start").await["session_id"],
        "unaffected-session"
    );
    other.send(json!({"type":"connect"})).await;
    other.until("connected").await;
    assert_eq!(negotiate(&mut other).await, epoch);
    let other_id = uuid::Uuid::new_v4().to_string();
    other
        .send(request(&epoch, &other_id, "unaffected-context-marker"))
        .await;
    let ack = other.until("turn_ack").await;
    terminal(&mut other, &other_id, ack["turn_id"].as_str().unwrap()).await;

    let (status, _) = device_http(
        fixture.address,
        "DELETE",
        "/api/sessions/device-session",
        "synthetic-unpaired-token",
        "",
    )
    .await;
    assert_eq!(status, 401, "deletion retains owner authentication");
    let (status, _) = device_http(
        fixture.address,
        "DELETE",
        "/api/sessions/device-session",
        OWNER_TOKEN,
        "",
    )
    .await;
    assert_eq!(status, 200);
    assert!(
        !fixture
            .state
            .session_backend
            .as_ref()
            .unwrap()
            .session_exists("gw_device-session")
    );

    // Keep B attached throughout A's clear/reconnect. Its subscription used to
    // retain the old Agent, so an empty database alone did not reset context.
    drop(first);
    let mut reopened = fixture.socket(DEVICE_TOKEN).await;
    assert_eq!(negotiate(&mut reopened).await, epoch);
    second.send(json!({"type":"turn_control","version":1,"op":"resume","epoch":epoch,"request_id":old_id,"after_seq":0})).await;
    assert_eq!(second.until("turn_ack").await["status"], "unknown");
    reopened
        .send(request(&epoch, &old_id, "s2-delete-marker"))
        .await;
    assert_eq!(reopened.until("turn_ack").await["status"], "unknown");
    assert_eq!(
        fixture.seen.lock().len(),
        2,
        "deleted request cannot replay effects"
    );

    let fresh_id = uuid::Uuid::new_v4().to_string();
    reopened
        .send(request(&epoch, &fresh_id, "fresh-after-delete"))
        .await;
    let ack = reopened.until("turn_ack").await;
    terminal(&mut reopened, &fresh_id, ack["turn_id"].as_str().unwrap()).await;
    {
        let seen = fixture.seen.lock();
        assert_eq!(seen.len(), 3);
        let input = seen[2]["messages"].to_string();
        assert!(input.contains("fresh-after-delete"));
        assert!(!input.contains("s2-delete-marker"));
        assert!(!input.contains("s2-deleted-output-marker"));
    }

    let second_id = uuid::Uuid::new_v4().to_string();
    second
        .send(request(&epoch, &second_id, "still-connected-after-delete"))
        .await;
    let ack = second.until("turn_ack").await;
    terminal(&mut second, &second_id, ack["turn_id"].as_str().unwrap()).await;
    {
        let seen = fixture.seen.lock();
        assert_eq!(seen.len(), 4);
        let input = seen[3]["messages"].to_string();
        assert!(
            input.contains("fresh-after-delete"),
            "both sockets still share one Agent"
        );
        assert!(!input.contains("s2-delete-marker"));
        assert!(!input.contains("s2-deleted-output-marker"));
    }

    let followup = uuid::Uuid::new_v4().to_string();
    other
        .send(request(&epoch, &followup, "unaffected-followup"))
        .await;
    let ack = other.until("turn_ack").await;
    assert_eq!(ack["status"], "accepted", "another session keeps its epoch");
    terminal(&mut other, &followup, ack["turn_id"].as_str().unwrap()).await;
    let seen = fixture.seen.lock();
    assert_eq!(seen.len(), 5);
    assert!(
        seen[4]["messages"]
            .to_string()
            .contains("unaffected-context-marker")
    );
}

#[tokio::test]
async fn s2_wire_session_delete_during_connect_cannot_restore_old_snapshot() {
    let fixture = DeviceFixture::new(&["chat"]).await;
    let mut first = fixture.socket(DEVICE_TOKEN).await;
    let epoch = negotiate(&mut first).await;
    let id = uuid::Uuid::new_v4().to_string();
    first.send(request(&epoch, &id, "s2-delete-marker")).await;
    let ack = first.until("turn_ack").await;
    terminal(&mut first, &id, ack["turn_id"].as_str().unwrap()).await;

    let (status, mut connecting) = DeviceWire::upgrade(
        fixture.address,
        &format!("agent=device-agent&session_id={SESSION}"),
        DEVICE_TOKEN,
    )
    .await;
    assert_eq!(status, 101);
    assert_eq!(connecting.until("session_start").await["resumed"], true);
    drop(first);
    tokio::time::timeout(LIMIT, async {
        while fixture.state.ws_conversations.len() != 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    // This socket has read session_start but has not sent its connect frame.
    // It must neither block DELETE nor later seed an Agent with that snapshot.
    let (status, _) = device_http(
        fixture.address,
        "DELETE",
        "/api/sessions/device-session",
        OWNER_TOKEN,
        "",
    )
    .await;
    assert_eq!(status, 200);
    // session_start was consumed before DELETE; complete that handshake rather
    // than waiting for another session_start that the server does not send.
    connecting.send(json!({"type":"connect"})).await;
    connecting.until("connected").await;
    assert_eq!(negotiate(&mut connecting).await, epoch);
    let fresh_id = uuid::Uuid::new_v4().to_string();
    connecting
        .send(request(&epoch, &fresh_id, "fresh-delayed-connect"))
        .await;
    let ack = connecting.until("turn_ack").await;
    terminal(&mut connecting, &fresh_id, ack["turn_id"].as_str().unwrap()).await;
    let seen = fixture.seen.lock();
    assert_eq!(seen.len(), 2);
    let input = seen[1]["messages"].to_string();
    assert!(!input.contains("s2-delete-marker"));
    assert!(!input.contains("s2-deleted-output-marker"));
}

#[tokio::test]
async fn s2_wire_session_delete_without_persistence_keeps_404_and_live_history() {
    let fixture = DeviceFixture::new(&["chat"]).await;
    let mut client = fixture.socket(DEVICE_TOKEN).await;
    let epoch = negotiate(&mut client).await;
    let id = uuid::Uuid::new_v4().to_string();
    client.send(request(&epoch, &id, "s2-delete-marker")).await;
    let ack = client.until("turn_ack").await;
    terminal(&mut client, &id, ack["turn_id"].as_str().unwrap()).await;

    // Exercise the same handler with persistence disabled. DELETE never
    // silently becomes a different in-memory reset operation in that mode.
    let mut state = fixture.state.clone();
    state.session_backend = None;
    let mut headers = HeaderMap::new();
    headers.insert(
        axum::http::header::AUTHORIZATION,
        format!("Bearer {OWNER_TOKEN}").parse().unwrap(),
    );
    let response = crate::api::handle_api_session_delete(
        State(state),
        headers,
        axum::extract::Path(SESSION.into()),
    )
    .await
    .into_response();
    assert_eq!(response.status(), axum::http::StatusCode::NOT_FOUND);
    let followup = uuid::Uuid::new_v4().to_string();
    client
        .send(request(&epoch, &followup, "history-still-present"))
        .await;
    let ack = client.until("turn_ack").await;
    terminal(&mut client, &followup, ack["turn_id"].as_str().unwrap()).await;
    let seen = fixture.seen.lock();
    assert_eq!(seen.len(), 2);
    assert!(seen[1]["messages"].to_string().contains("s2-delete-marker"));
}

#[tokio::test]
async fn s2_wire_http_session_delete_erases_replay_without_reexecution() {
    let fixture = DeviceFixture::new(&["chat"]).await;
    let mut device = fixture.socket(DEVICE_TOKEN).await;
    let epoch = negotiate(&mut device).await;
    let id = uuid::Uuid::new_v4().to_string();
    device.send(request(&epoch, &id, "s2-delete-marker")).await;
    let ack = device.until("turn_ack").await;
    let events = terminal(&mut device, &id, ack["turn_id"].as_str().unwrap()).await;
    assert!(
        serde_json::to_string(&events)
            .unwrap()
            .contains("s2-deleted-output-marker")
    );
    drop(device);
    tokio::time::timeout(LIMIT, async {
        while fixture.state.ws_conversations.len() != 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("socket teardown must release idle Agent");
    let (status, body) = device_http(
        fixture.address,
        "DELETE",
        "/api/sessions/device-session",
        OWNER_TOKEN,
        "",
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["deleted"],
        true
    );
    assert!(
        !fixture
            .state
            .session_backend
            .as_ref()
            .unwrap()
            .session_exists("gw_device-session")
    );
    let mut reopened = fixture.socket(DEVICE_TOKEN).await;
    assert_eq!(negotiate(&mut reopened).await, epoch);
    reopened.send(json!({"type":"turn_control","version":1,"op":"resume","epoch":epoch,"request_id":id,"after_seq":0})).await;
    let resumed = reopened.next().await.unwrap();
    assert_eq!(
        resumed["type"], "turn_ack",
        "deleted data must not be replayed"
    );
    assert_eq!(resumed["status"], "unknown");
    assert!(resumed["turn_id"].is_null());
    assert!(!resumed.to_string().contains("s2-deleted-output-marker"));
    reopened
        .send(request(&epoch, &id, "s2-delete-marker"))
        .await;
    let repeated = reopened.until("turn_ack").await;
    assert_eq!(
        repeated["status"], "unknown",
        "deleting history must not authorize effect replay"
    );
    assert_eq!(fixture.seen.lock().len(), 1);
}

#[tokio::test]
async fn s2_wire_negotiated_device_cannot_approve_owner_can_complete_same_prompt() {
    let fixture = DeviceFixture::new(&["chat", "answer", "cancel"]).await;
    let mut device = fixture.socket(DEVICE_TOKEN).await;
    let mut owner = fixture.socket(OWNER_TOKEN).await;
    let epoch = negotiate(&mut device).await;
    let id = uuid::Uuid::new_v4().to_string();
    device.send(request(&epoch, &id, "approval-probe")).await;
    let ack = device.until("turn_ack").await;
    assert_eq!(ack["status"], "accepted");
    let approval = device.until("approval_request").await;
    assert_eq!(
        owner.until("approval_request").await["request_id"],
        approval["request_id"]
    );
    for decision in ["approve", "always", "deny"] {
        device.send(json!({"type":"approval_response","request_id":approval["request_id"],"decision":decision})).await;
        assert_eq!(
            device.until("error").await["code"],
            "BRIDGE_OPERATION_DENIED"
        );
        assert!(!fixture.workspace.join("approved.txt").exists());
    }
    owner.send(json!({"type":"approval_response","request_id":approval["request_id"],"decision":"approve"})).await;
    owner.until("done").await;
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("approved.txt")).unwrap(),
        "owner-approved"
    );
    device.send(json!({"type":"turn_control","version":1,"op":"resume","epoch":epoch,"request_id":id,"after_seq":0})).await;
    let replay = device.until("turn_replay").await;
    assert_eq!(replay["turn_id"], ack["turn_id"]);
    assert_eq!(replay["gap"], false);
    assert_eq!(fixture.seen.lock().len(), 2);
}

#[tokio::test]
async fn s2_wire_delete_active_turn_suppresses_late_events_and_allows_fresh_request() {
    let fixture = DeviceFixture::new(&["chat"]).await;
    let mut active = fixture.socket(DEVICE_TOKEN).await;
    let mut second = fixture.socket(DEVICE_TOKEN).await;
    let mut legacy = fixture.socket(OWNER_TOKEN).await;
    let epoch = negotiate(&mut active).await;
    assert_eq!(negotiate(&mut second).await, epoch);
    let erased = uuid::Uuid::new_v4().to_string();
    active
        .send(request(
            &epoch,
            &erased,
            "s2-held-provider s2-active-private-marker",
        ))
        .await;
    let ack = active.until("turn_ack").await;
    assert_eq!(ack["status"], "accepted");
    wait_provider(&fixture, 1).await;
    let (status, _) = device_http(
        fixture.address,
        "DELETE",
        "/api/sessions/device-session",
        OWNER_TOKEN,
        "",
    )
    .await;
    assert_eq!(status, 200);
    // The unnegotiated subscriber has no S2 journal visibility filter. Its
    // queued cancellation/output frames must be invalidated by reset too.
    legacy.send(json!({"type":"reset-visibility-probe"})).await;
    tokio::time::timeout(LIMIT, async {
        loop {
            let frame = legacy.next().await.unwrap();
            assert!(
                !matches!(frame["type"].as_str(), Some("chunk" | "done" | "aborted")),
                "legacy subscriber received a deleted turn frame: {frame}"
            );
            if frame["type"] == "error" && frame["code"] == "UNKNOWN_MESSAGE_TYPE" {
                break;
            }
        }
    })
    .await
    .unwrap();
    // The provider was held before emitting any content. After a successful
    // deletion neither cancellation output nor late completion may leak through
    // this already-attached negotiated subscriber.
    for attempt in 0..2 {
        if attempt == 1 {
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
        active.send(json!({"type":"turn_control","version":1,"op":"resume","epoch":epoch,"request_id":erased,"after_seq":0})).await;
        let reply = tokio::time::timeout(LIMIT, async {
            loop {
                let frame = active.next().await.unwrap();
                assert!(
                    !(frame["type"] == "turn_event" && frame["request_id"] == erased),
                    "deleted active turn emitted late content: {frame}"
                );
                if frame["type"] == "turn_ack" {
                    break frame;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(reply["status"], "unknown");
        assert!(reply["turn_id"].is_null());
    }
    drop(active);
    assert_eq!(
        fixture.state.ws_conversations.len(),
        1,
        "second subscriber stays attached"
    );
    assert!(
        !fixture
            .state
            .session_backend
            .as_ref()
            .unwrap()
            .session_exists("gw_device-session"),
        "a deleted active turn cannot recreate durable history"
    );
    let mut fresh = fixture.socket(DEVICE_TOKEN).await;
    assert_eq!(negotiate(&mut fresh).await, epoch);
    fresh
        .send(request(
            &epoch,
            &erased,
            "s2-held-provider s2-active-private-marker",
        ))
        .await;
    assert_eq!(fresh.until("turn_ack").await["status"], "unknown");
    let fresh_id = uuid::Uuid::new_v4().to_string();
    fresh
        .send(request(
            &epoch,
            &fresh_id,
            "new explicit request after deletion",
        ))
        .await;
    let fresh_ack = fresh.until("turn_ack").await;
    assert_eq!(fresh_ack["status"], "accepted");
    let events = terminal(
        &mut fresh,
        &fresh_id,
        fresh_ack["turn_id"].as_str().unwrap(),
    )
    .await;
    assert_eq!(events.last().unwrap()["event"]["outcome"], "done");
    assert_eq!(
        fixture.seen.lock().len(),
        2,
        "only the new explicit ID may run after deletion"
    );
    assert!(
        !fixture.seen.lock()[1]["messages"]
            .to_string()
            .contains("s2-active-private-marker"),
        "cancellation settlement must not retain the deleted turn in the shared Agent"
    );
}

fn attachment_request(epoch: &str, id: &str, content: &str, attachments: &[String]) -> Value {
    let mut frame = request(epoch, id, content);
    frame["attachments"] = json!(attachments);
    frame
}

fn s2_attachment_scope(token: &str) -> crate::api_attachments::Scope {
    crate::api_attachments::Scope {
        subject: zeroclaw_config::tokens::hash_token(token),
        session: SESSION.into(),
        agent: "device-agent".into(),
    }
}

/// #491 scoped handles through the S2 admission path: same materialization
/// owner, same denials, journal identity extended to the handle list, and
/// bridge-operation parity with the legacy classifier (restricted bridges
/// cannot carry attachments at all).
#[tokio::test]
async fn s2_wire_attachments_reuse_scoped_handles_with_legacy_denials() {
    let fixture = DeviceFixture::new(&["chat"]).await;

    // ── Owner (pairing) socket: the attachment-capable path ────────────────
    let mut owner = fixture.socket(OWNER_TOKEN).await;
    let epoch = negotiate(&mut owner).await;
    let scope = s2_attachment_scope(OWNER_TOKEN);
    let handle = fixture
        .state
        .ws_conversations
        .attachments
        .insert(
            scope.clone(),
            "note.txt".into(),
            "text/plain".into(),
            axum::body::Bytes::from_static(b"s2 attachment contents"),
        )
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let second_handle = fixture
        .state
        .ws_conversations
        .attachments
        .insert(
            scope,
            "second.txt".into(),
            "text/plain".into(),
            axum::body::Bytes::from_static(b"second attachment"),
        )
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Accepted: materialized text reaches the provider inside one real turn.
    let id = uuid::Uuid::new_v4().to_string();
    let frame = attachment_request(&epoch, &id, "read the note", std::slice::from_ref(&handle));
    owner.send(frame.clone()).await;
    let ack = owner.until("turn_ack").await;
    assert_eq!(ack["status"], "accepted");
    let events = terminal(&mut owner, &id, ack["turn_id"].as_str().unwrap()).await;
    assert_eq!(events.last().unwrap()["event"]["outcome"], "done");
    assert!(
        fixture.seen.lock()[0]
            .to_string()
            .contains("s2 attachment contents"),
        "attachment must materialize through the #491 path"
    );

    // Duplicate: identical content and handles never re-run.
    owner.send(frame).await;
    assert_eq!(owner.until("turn_ack").await["status"], "duplicate");
    assert_eq!(fixture.seen.lock().len(), 1);

    // Conflict: same request identity, different handle list.
    owner
        .send(attachment_request(
            &epoch,
            &id,
            "read the note",
            std::slice::from_ref(&second_handle),
        ))
        .await;
    assert_eq!(owner.until("turn_ack").await["status"], "conflict");
    assert_eq!(fixture.seen.lock().len(), 1);

    // Unknown handle refuses before the turn starts; the error names the op.
    let unknown = uuid::Uuid::new_v4().to_string();
    owner
        .send(attachment_request(&epoch, &unknown, "read", std::slice::from_ref(&uuid::Uuid::new_v4().to_string())))
        .await;
    let unknown_error = owner.until("turn_error").await;
    assert_eq!(unknown_error["code"], "ATTACHMENT_UNAVAILABLE");
    assert_eq!(unknown_error["op"], "message");

    // Shape denials: non-UUID, duplicated ids, and over the #491 item limit.
    for ids in [
        vec!["not-a-uuid".to_string()],
        vec![handle.clone(), handle.clone()],
        vec![
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
        ],
    ] {
        let shape = uuid::Uuid::new_v4().to_string();
        owner
            .send(attachment_request(&epoch, &shape, "read", &ids))
            .await;
        assert_eq!(
            owner.until("turn_error").await["code"],
            "INVALID_ATTACHMENTS"
        );
    }

    // Attachment-only turn (legacy #491 parity): empty text is admissible.
    let attachment_only = uuid::Uuid::new_v4().to_string();
    owner
        .send(attachment_request(&epoch, &attachment_only, "", std::slice::from_ref(&handle)))
        .await;
    let only_ack = owner.until("turn_ack").await;
    assert_eq!(only_ack["status"], "accepted");
    let events = terminal(
        &mut owner,
        &attachment_only,
        only_ack["turn_id"].as_str().unwrap(),
    )
    .await;
    assert_eq!(events.last().unwrap()["event"]["outcome"], "done");

    // The admitted-field list grew by exactly one name.
    let smuggled = uuid::Uuid::new_v4().to_string();
    let mut smuggle = request(&epoch, &smuggled, "read");
    smuggle["sources"] = json!(["unvetted-source"]);
    owner.send(smuggle).await;
    assert_eq!(
        owner.until("turn_error").await["code"],
        "UNSUPPORTED_FIELD"
    );

    // ── Device bridge socket: legacy operation parity ──────────────────────
    // The legacy classifier routes attachment-bearing messages to
    // "unsupported"; a chat-only restricted bridge must be refused identically
    // through S2, while its plain chat keeps working.
    let mut device = fixture.socket(DEVICE_TOKEN).await;
    let device_epoch = negotiate(&mut device).await;
    let denied = uuid::Uuid::new_v4().to_string();
    device
        .send(attachment_request(
            &device_epoch,
            &denied,
            "read",
            std::slice::from_ref(&second_handle),
        ))
        .await;
    device.denied().await;
    let plain = uuid::Uuid::new_v4().to_string();
    device.send(request(&device_epoch, &plain, "plain device chat")).await;
    let plain_ack = device.until("turn_ack").await;
    assert_eq!(plain_ack["status"], "accepted");
    let events = terminal(
        &mut device,
        &plain,
        plain_ack["turn_id"].as_str().unwrap(),
    )
    .await;
    assert_eq!(events.last().unwrap()["event"]["outcome"], "done");

    // ── Defense in depth: revoking the pairing credential keeps attachments
    // refused (the operation gate denies them for the restricted bridge
    // regardless of pairing state, and the materialize-time scope re-check
    // remains as the second layer).
    assert!(fixture.state.pairing.revoke_token(DEVICE_TOKEN));
    let revoked = uuid::Uuid::new_v4().to_string();
    device
        .send(attachment_request(
            &device_epoch,
            &revoked,
            "read",
            std::slice::from_ref(&second_handle),
        ))
        .await;
    device.denied().await;
    assert!(
        fixture.seen.lock().len() >= 2,
        "owner and device plain turns ran; attachment turns did not"
    );
}

/// #564 brain monitor: S2 wire events land in the structured log with full
/// identities and NO content (canary privacy proof). The in-process log
/// broadcast is the capture seam — the same feed `/api/logs` persists.
/// Holding the gateway-start lock across the test's awaits is the point —
/// it serializes this whole test against any full-gateway start's hook swap.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn s2_wire_events_land_in_structured_log_without_content() {
    // Serialize against the global log plumbing: other tests in this binary
    // take the same guards and one of them clears the broadcast hook, which
    // would Close this receiver mid-collection. The gateway-start lock also
    // covers full `run_gateway` tests, whose hook swap takes no log locks.
    let _writer_guard = zeroclaw_log::__private_test_writer_lock();
    let _hook_guard = zeroclaw_log::__private_test_hook_lock();
    let _gateway_start_guard = crate::TEST_GATEWAY_START_LOCK.lock();
    // record! fans out through the global tracing subscriber; tests must
    // install the capture layer (idempotent across parallel tests) before
    // subscribing the broadcast, or every event drops into the no-op default.
    zeroclaw_log::try_install_capture_subscriber();
    let mut rx = zeroclaw_log::subscribe_or_install();
    let fixture = DeviceFixture::new(&["chat", "cancel"]).await;
    let mut owner = fixture.socket(OWNER_TOKEN).await;
    let epoch = negotiate(&mut owner).await;

    let scope = s2_attachment_scope(OWNER_TOKEN);
    let handle = fixture
        .state
        .ws_conversations
        .attachments
        .insert(
            scope,
            "canary.txt".into(),
            "text/plain".into(),
            axum::body::Bytes::from_static(b"canary-attachment-bytes"),
        )
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Canary-bearing admission with an attachment.
    let first = uuid::Uuid::new_v4().to_string();
    let content = "monitor-canary-content";
    owner
        .send(attachment_request(
            &epoch,
            &first,
            content,
            std::slice::from_ref(&handle),
        ))
        .await;
    let ack = owner.until("turn_ack").await;
    assert_eq!(ack["status"], "accepted");
    let first_turn = ack["turn_id"].as_str().unwrap().to_string();
    let events = terminal(&mut owner, &first, &first_turn).await;
    assert_eq!(events.last().unwrap()["event"]["outcome"], "done");

    // A second, cancelled turn exercises the cancel + aborted-terminal path.
    let second = uuid::Uuid::new_v4().to_string();
    owner.send(request(&epoch, &second, "s2-held-provider")).await;
    let second_ack = owner.until("turn_ack").await;
    let second_turn = second_ack["turn_id"].as_str().unwrap().to_string();
    wait_provider(&fixture, 2).await;
    owner
        .send(json!({"type":"turn_control","version":1,"op":"cancel","epoch":epoch,
            "request_id":second,"turn_id":second_turn}))
        .await;
    assert_eq!(
        owner.until("turn_cancel_ack").await["status"],
        "requested"
    );
    let events = terminal(&mut owner, &second, &second_turn).await;
    assert_eq!(events.last().unwrap()["event"]["outcome"], "aborted");

    // A LEGACY turn (unnegotiated socket, caller-chosen id) must terminal-log
    // too — finish() records it when no S2 journal entry exists. Its id
    // deliberately equals the FIRST S2 admission's journal receipt key,
    // computable by any caller (all hash inputs are client-known): ownership
    // must key on the TURN match, so this different turn keeps its id.
    let receipt_key = super::super::super::turn_control::key(
        &format!(
            "{}{SESSION}\u{1f}device-agent",
            super::super::super::GW_SESSION_PREFIX
        ),
        &zeroclaw_config::tokens::hash_token(OWNER_TOKEN),
        &epoch,
        &first,
    );
    let legacy_id = receipt_key;
    // Different session: the S2 admission recorded a session-scoped receipt
    // under this key in SESSION, which would otherwise answer "duplicate".
    // The owner (pairing) token is not bridge-session-bound, so a second
    // session is admissible.
    let (status, mut legacy_socket) = DeviceWire::upgrade(
        fixture.address,
        "agent=device-agent&session_id=monitor-cross-session",
        OWNER_TOKEN,
    )
    .await;
    assert_eq!(status, 101);
    legacy_socket
        .send(json!({"type":"connect"}))
        .await;
    legacy_socket.until("connected").await;
    legacy_socket
        .send(json!({"type":"message", "id": legacy_id, "content":"legacy-canary-content"}))
        .await;
    assert_eq!(legacy_socket.until("ack").await["status"], "accepted");
    legacy_socket.until("done").await;

    // Drain the broadcast until both S2 terminals AND the legacy terminal are
    // seen for our session.
    let mut collected: Vec<Value> = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            panic!("timed out collecting log events; got {} frames", collected.len());
        }
        match tokio::time::timeout(Duration::from_millis(50).min(remaining), rx.recv()).await {
            Ok(Ok(event)) => collected.push(event),
            Err(_elapsed) => {}
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                // The hook locks cover the api.rs broadcast tests, but
                // `run_gateway`-based tests install the hook without them.
                // Resubscribe and keep draining; events buffered on the old
                // sender are lost, which the deadline treats as a failure.
                rx = zeroclaw_log::subscribe_or_install();
            }
        }
        let terminals = collected
            .iter()
            .filter(|event| {
                event["event"]["action"] == "complete"
                    && event["attributes"]["request_id"] == json!(second)
            })
            .count();
        let legacy_terminal = collected
            .iter()
            .any(|event| {
                event["event"]["action"] == "complete"
                    && event["attributes"]["request_id"] == json!(legacy_id)
            });
        if terminals >= 1
            && legacy_terminal
            && collected.iter().any(|event| {
                event["event"]["action"] == "cancel"
                    && event["attributes"]["request_id"] == json!(second)
            })
        {
            break;
        }
    }

    let admission = collected
        .iter()
        .find(|event| {
            event["event"]["action"] == "inbound"
                && event["attributes"]["request_id"] == json!(first)
        })
        .expect("admission event with the client request id must be logged");
    assert_eq!(admission["event"]["category"], "session");
    assert_eq!(admission["attributes"]["status"], "accepted");
    assert_eq!(admission["attributes"]["session_id"], SESSION);
    assert_eq!(admission["attributes"]["agent"], "device-agent");
    assert_eq!(admission["attributes"]["turn_id"], first_turn.as_str());
    assert_eq!(
        admission["attributes"]["content_bytes"],
        json!(content.len()),
        "size only — never the content itself"
    );
    assert_eq!(admission["attributes"]["attachments"], 1);

    let cancel = collected
        .iter()
        .find(|event| {
            event["event"]["action"] == "cancel"
                && event["attributes"]["request_id"] == json!(second)
        })
        .expect("cancel event must be logged");
    assert_eq!(cancel["attributes"]["status"], "requested");
    assert_eq!(cancel["attributes"]["turn_id"], second_turn.as_str());

    let terminal = collected
        .iter()
        .find(|event| {
            event["event"]["action"] == "complete"
                && event["attributes"]["request_id"] == json!(second)
        })
        .expect("terminal event must be logged");
    assert_eq!(terminal["attributes"]["outcome"], "aborted");
    assert_eq!(terminal["attributes"]["turn_id"], second_turn.as_str());
    // A cancelled turn is a failure outcome, never a silent success.
    assert_eq!(terminal["event"]["outcome"], "failure");
    // Exactly one terminal per turn — no double logging on either path.
    for request in [&first, &second, &legacy_id] {
        let count = collected
            .iter()
            .filter(|event| {
                event["event"]["action"] == "complete"
                    && event["attributes"]["request_id"] == json!(request)
            })
            .count();
        assert_eq!(count, 1, "exactly one terminal event for {request}");
    }

    // Session identity is the SAME field across admission and terminal (the
    // conversation key's gw_ prefix stripped), so one turn's rows correlate.
    let first_terminal = collected
        .iter()
        .find(|event| {
            event["event"]["action"] == "complete"
                && event["attributes"]["request_id"] == json!(first)
        })
        .expect("first turn terminal event must be logged");
    assert_eq!(first_terminal["attributes"]["outcome"], "done");
    assert_eq!(first_terminal["attributes"]["session_id"], SESSION);
    assert_eq!(first_terminal["attributes"]["turn_id"], first_turn.as_str());

    // The legacy turn terminal carries the caller-chosen id and session.
    let legacy_terminal = collected
        .iter()
        .find(|event| {
            event["event"]["action"] == "complete"
                && event["attributes"]["request_id"] == json!(legacy_id)
        })
        .expect("legacy terminal event must be logged");
    assert_eq!(legacy_terminal["attributes"]["outcome"], "done");
    // The cross-session legacy turn keeps its caller-chosen id — even though
    // that id is byte-identical to the FIRST admission's journal receipt key
    // — because ownership is turn-scoped, and logs its own session id.
    assert_eq!(
        legacy_terminal["attributes"]["request_id"],
        legacy_id.as_str()
    );
    assert_eq!(
        legacy_terminal["attributes"]["session_id"],
        "monitor-cross-session"
    );
    assert_eq!(legacy_terminal["attributes"]["agent"], "device-agent");

    let negotiated = collected
        .iter()
        .any(|event| {
            event["event"]["action"] == "connect"
                && event["attributes"]["session_id"] == SESSION
                && event["attributes"]["epoch"] == epoch.as_str()
        });
    assert!(negotiated, "negotiation event must be logged");

    // Privacy with teeth: no canary appears in ANY captured event — including
    // the legacy turn's content.
    for event in &collected {
        let text = event.to_string();
        assert!(
            !text.contains("monitor-canary-content"),
            "message content leaked into the log: {text}"
        );
        assert!(
            !text.contains("canary-attachment-bytes"),
            "attachment bytes leaked into the log: {text}"
        );
        assert!(
            !text.contains("legacy-canary-content"),
            "legacy message content leaked into the log: {text}"
        );
    }
}

#[tokio::test]
async fn offline_approval_reconnect_keeps_request_and_requires_operator() {
    let fixture = DeviceFixture::new(&["chat", "answer", "cancel"]).await;
    let mut owner = fixture.socket(OWNER_TOKEN).await;
    let id = uuid::Uuid::new_v4().to_string();
    owner
        .send(json!({"type":"message", "id":id, "content":"approval-probe"}))
        .await;
    assert_eq!(owner.until("ack").await["status"], "accepted");
    let prompt = owner.until("approval_request").await;
    let conversation = fixture
        .state
        .ws_conversations
        .session_conversations(&format!("gw_{SESSION}"))[0]
        .clone();
    drop(owner);
    tokio::time::timeout(LIMIT, async {
        while conversation.subscriber_count() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(conversation.is_running());
    assert!(!fixture.workspace.join("approved.txt").exists());

    let mut device = fixture.socket(DEVICE_TOKEN).await;
    let restored = device.until("approval_request").await;
    assert_eq!(restored["request_id"], prompt["request_id"]);
    assert!(restored["timeout_secs"].as_u64().unwrap() <= prompt["timeout_secs"].as_u64().unwrap());
    device.send(json!({"type":"approval_response", "request_id":prompt["request_id"], "decision":"approve"})).await;
    assert_eq!(
        device.until("error").await["code"],
        "BRIDGE_OPERATION_DENIED"
    );
    assert!(!fixture.workspace.join("approved.txt").exists());
    let mut reopened = fixture.socket(OWNER_TOKEN).await;
    assert_eq!(
        reopened.until("approval_request").await["request_id"],
        prompt["request_id"]
    );
    reopened.send(json!({"type":"approval_response", "request_id":prompt["request_id"], "decision":"approve"})).await;
    reopened.until("done").await;
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("approved.txt")).unwrap(),
        "owner-approved"
    );
    assert!(!conversation.resolve_approval(
        prompt["request_id"].as_str().unwrap(),
        ChannelApprovalResponse::Approve
    ));
    assert_eq!(
        fixture.seen.lock().len(),
        2,
        "reconnect must not run the model/tool again"
    );
}

#[tokio::test]
async fn approval_created_offline_waits_for_reconnect_instead_of_becoming_unreachable() {
    let fixture = DeviceFixture::new(&["chat"]).await;
    let mut owner = fixture.socket(OWNER_TOKEN).await;
    let id = uuid::Uuid::new_v4().to_string();
    owner
        .send(json!({"type":"message", "id":id, "content":"approval-probe s2-held-provider"}))
        .await;
    assert_eq!(owner.until("ack").await["status"], "accepted");
    let conversation = fixture
        .state
        .ws_conversations
        .session_conversations(&format!("gw_{SESSION}"))[0]
        .clone();
    drop(owner);
    tokio::time::timeout(LIMIT, async {
        while conversation.subscriber_count() != 0
            || conversation.pending_approvals.lock().is_empty()
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(!fixture.workspace.join("approved.txt").exists());
    let mut reopened = fixture.socket(OWNER_TOKEN).await;
    let prompt = reopened.until("approval_request").await;
    reopened.send(json!({"type":"approval_response", "request_id":prompt["request_id"], "decision":"deny"})).await;
    reopened.until("done").await;
    assert!(
        !fixture.workspace.join("approved.txt").exists(),
        "reconnect is not permission to execute"
    );
}

#[tokio::test]
async fn production_gateway_shutdown_retires_idle_and_handshake_sockets() {
    let fixture = DeviceFixture::new(&["chat"]).await;
    let mut config = fixture.state.config.read().clone();
    config.gateway.paired_tokens = vec![zeroclaw_config::tokens::hash_token(OWNER_TOKEN)];
    let data_dir = config.data_dir.clone();
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = probe.local_addr().unwrap();
    drop(probe);
    let (shutdown_tx, _) = tokio::sync::watch::channel(false);
    let (reload_tx, _) = tokio::sync::watch::channel(false);
    let controls = zeroclaw_runtime::daemon::GatewayReloadControls {
        shutdown_tx: shutdown_tx.clone(), reload_tx,
    };
    let server = zeroclaw_spawn::spawn!(async move {
        crate::run_gateway("127.0.0.1", address.port(), config, None, Some(controls), None).await
    });
    tokio::time::timeout(LIMIT, async {
        loop {
            if tokio::net::TcpStream::connect(address).await.is_ok() { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    let query = format!("agent=device-agent&session_id={SESSION}");
    let (status, mut idle) = DeviceWire::upgrade(address, &query, OWNER_TOKEN).await;
    assert_eq!(status, 101);
    idle.connected(json!({"type":"connect"})).await;
    // Ensure the connection finished attaching rather than merely writing ACK.
    idle.send(json!({"type":"turn_control","version":1,"op":"negotiate"})).await;
    idle.until("turn_ready").await;
    let (status, mut handshake) = DeviceWire::upgrade(address, &query, OWNER_TOKEN).await;
    assert_eq!(status, 101);
    handshake.until("session_start").await;
    assert!(zeroclaw_infra::gateway_session_owner::GatewaySessionOwner::acquire(&data_dir).is_err());
    shutdown_tx.send(true).unwrap();
    tokio::time::timeout(LIMIT, server).await.unwrap().unwrap().unwrap();
    assert!(idle.next().await.is_none(), "retired socket must close without another client message");
    assert!(handshake.next().await.is_none(), "uncompleted handshake must also release old state");
    // EOF can become visible just before the handler drops its final state.
    let replacement = tokio::time::timeout(LIMIT, async {
        loop {
            match zeroclaw_infra::gateway_session_owner::GatewaySessionOwner::acquire(&data_dir) {
                Ok(owner) => break owner,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => tokio::task::yield_now().await,
                Err(error) => panic!("replacement failed: {error}"),
            }
        }
    }).await.expect("idle sockets must not pin the retired gateway owner");
    assert_eq!(replacement.recovered_turns(), 0);
    assert!(fixture.seen.lock().is_empty(), "idle shutdown must not start model work");
}
