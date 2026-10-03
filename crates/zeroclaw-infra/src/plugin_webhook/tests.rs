use super::*;

use std::collections::HashMap;
use std::time::Duration;

use tokio::sync::mpsc;
use zeroclaw_api::webhook::{
    PLUGIN_WEBHOOK_QUEUE_DEPTH, PluginWebhookRegistryLease, PluginWebhookRoute, RawWebhook,
    WebhookReservation, WebhookReservationStatus, WebhookReservationToken,
};

use crate::IDEMPOTENCY_MAX_KEYS_DEFAULT;

fn owner(plugin: &str, channel_alias: &str) -> PluginWebhookOwner {
    PluginWebhookOwner::new(plugin, channel_alias)
}

fn post(path: &str, body: &[u8]) -> PluginWebhookRequest {
    PluginWebhookRequest::new(path, "POST", "", Vec::new(), body.to_vec())
        .expect("test request is within the ingress bounds")
}

fn test_ingress() -> PluginWebhookIngress {
    PluginWebhookIngress::new(300, 8)
}

/// Publish one generation of routes and hand back each route's queue, in
/// order.
fn publish(
    ingress: &PluginWebhookIngress,
    routes: &[(&str, PluginWebhookOwner, usize)],
) -> (PluginWebhookRegistryLease, Vec<mpsc::Receiver<RawWebhook>>) {
    let mut published = HashMap::new();
    let mut receivers = Vec::new();
    for (path, owner, capacity) in routes {
        let (sink, receiver) = mpsc::channel(*capacity);
        published.insert(
            (*path).to_string(),
            PluginWebhookRoute::new(owner.clone(), sink),
        );
        receivers.push(receiver);
    }
    let lease = ingress.registry().start_generation();
    assert!(lease.replace(published), "the test generation is current");
    (lease, receivers)
}

/// Let woken tasks run without moving a paused clock.
async fn run_ready_tasks() {
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}

fn owned(reservation: WebhookReservation) -> WebhookReservationToken {
    match reservation {
        WebhookReservation::Owner(token) => token,
        _ => panic!("expected to own the reservation"),
    }
}

#[tokio::test]
async fn reservation_waits_for_owner_outcome_and_fences_stale_tokens() {
    let store = Arc::new(WebhookReservationStore::new(Duration::from_secs(300), 8));
    let idempotency = idempotency_bridge(&store, &owner("p", "a"), "fixture");
    let first = owned(idempotency.begin("stable-id"));
    let mut duplicate = match idempotency.begin("stable-id") {
        WebhookReservation::InFlight(waiter) => waiter,
        _ => panic!("duplicate must observe an in-flight owner"),
    };

    assert!(idempotency.rollback(&first));
    assert_eq!(duplicate.wait().await, WebhookReservationStatus::RolledBack);
    let replacement = owned(idempotency.begin("stable-id"));
    assert_ne!(first.generation(), replacement.generation());
    assert!(!idempotency.rollback(&first));
    assert!(!idempotency.commit(&first));

    let mut committed_duplicate = match idempotency.begin("stable-id") {
        WebhookReservation::InFlight(waiter) => waiter,
        _ => panic!("later duplicate must wait for replacement owner"),
    };
    assert!(idempotency.commit(&replacement));
    assert_eq!(
        committed_duplicate.wait().await,
        WebhookReservationStatus::Committed
    );
    assert!(matches!(
        idempotency.begin("stable-id"),
        WebhookReservation::Committed
    ));
}

#[test]
fn dedup_key_separates_every_identity_component() {
    let base = dedup_key(&owner("p", "a"), "path", "m");
    assert_eq!(base.len(), 64);
    assert!(
        base.bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    );
    assert_eq!(base, dedup_key(&owner("p", "a"), "path", "m"));

    for changed in [
        dedup_key(&owner("q", "a"), "path", "m"),
        dedup_key(&owner("p", "b"), "path", "m"),
        dedup_key(&owner("p", "a"), "other", "m"),
        dedup_key(&owner("p", "a"), "path", "n"),
    ] {
        assert_ne!(changed, base);
    }
    assert_ne!(
        dedup_key(&owner("ab", "c"), "p", "m"),
        dedup_key(&owner("a", "bc"), "p", "m")
    );
    assert_ne!(
        dedup_key(&owner("a\0b", "c"), "p", "m"),
        dedup_key(&owner("a", "b\0c"), "p", "m")
    );
    assert_ne!(
        dedup_key(&owner("p", "a"), "", "xm"),
        dedup_key(&owner("p", "a"), "x", "m")
    );
}

/// The ingress bounds in-flight deliveries by `gateway.idempotency_max_keys`,
/// with zero selecting the default. The TTL floor is covered where
/// `effective_idempotency_ttl` is defined.
#[test]
fn new_applies_the_configured_or_default_key_bound() {
    fn in_flight_capacity(ingress: &PluginWebhookIngress) -> usize {
        let idempotency = idempotency_bridge(&ingress.reservations, &owner("p", "a"), "fixture");
        (0..)
            .take_while(|n| {
                matches!(
                    idempotency.begin(&format!("m{n}")),
                    WebhookReservation::Owner(_)
                )
            })
            .count()
    }
    assert_eq!(
        in_flight_capacity(&PluginWebhookIngress::new(0, 0)),
        IDEMPOTENCY_MAX_KEYS_DEFAULT
    );
    assert_eq!(in_flight_capacity(&PluginWebhookIngress::new(7, 3)), 3);
}

#[test]
fn routes_delegates_to_the_registry() {
    let ingress = test_ingress();
    let (_lease, _receivers) = publish(
        &ingress,
        &[
            ("second", owner("p", "b"), 1),
            ("first", owner("p", "a"), 1),
        ],
    );

    let listed = ingress.routes();
    assert_eq!(listed, ingress.registry().routes());
    assert_eq!(listed.generation(), 1);
    assert_eq!(
        listed.routes(),
        [
            ("first".to_string(), owner("p", "a")),
            ("second".to_string(), owner("p", "b")),
        ]
    );
}

/// Admission sees the owner the request would be queued to, and a refusal
/// queues and reserves nothing. A path no route owns is decided before
/// admission is consulted.
#[tokio::test]
async fn a_refused_admission_queues_nothing_and_sees_the_resolved_owner() {
    let ingress = test_ingress();
    let (_lease, mut receivers) = publish(&ingress, &[("fixture", owner("p", "a"), 1)]);
    let mut seen = None;
    let refused = ingress
        .dispatch_admitted(
            post("fixture", b"body"),
            &WebhookCancellation::new(),
            |owner| {
                seen = Some(owner.clone());
                Err("refused")
            },
        )
        .await;
    assert_eq!(refused, Err("refused"));
    assert_eq!(seen, Some(owner("p", "a")));
    assert!(
        receivers[0].try_recv().is_err(),
        "a refused request is never queued"
    );

    let unknown = ingress
        .dispatch_admitted(
            post("missing", b"body"),
            &WebhookCancellation::new(),
            |_| -> Result<(), &str> { panic!("admission runs only for a resolved route") },
        )
        .await;
    assert_eq!(unknown, Ok(PluginWebhookOutcome::NotFound));
}

#[tokio::test]
async fn dispatch_forwards_the_exact_request_to_the_route_worker() {
    let ingress = test_ingress();
    let (_lease, mut receivers) = publish(&ingress, &[("fixture", owner("p", "a"), 1)]);
    let mut receiver = receivers.remove(0);
    let (seen, observed) = oneshot::channel();
    zeroclaw_spawn::spawn!(async move {
        let request = receiver.recv().await.expect("route forwards the request");
        let _ = seen.send((
            request.method.clone(),
            request.query.clone(),
            request.headers.clone(),
            request.body.clone(),
            request.idempotency.is_some(),
        ));
        let _ = request.reply.send(Ok(WebhookOutcome::Ack));
    });

    let request = PluginWebhookRequest::new(
        "fixture",
        "POST",
        "a=1&a=2",
        vec![
            ("X-A".to_string(), "1".to_string()),
            ("x-a".to_string(), "2".to_string()),
        ],
        b"\x00\xffraw".to_vec(),
    )
    .expect("valid request");
    assert_eq!(
        ingress.dispatch(request, &WebhookCancellation::new()).await,
        PluginWebhookOutcome::Ack
    );
    let (method, query, headers, body, has_idempotency) =
        observed.await.expect("the worker saw the request");
    assert_eq!(method, "POST");
    assert_eq!(query, "a=1&a=2");
    assert_eq!(
        headers,
        [
            ("x-a".to_string(), "1".to_string()),
            ("x-a".to_string(), "2".to_string()),
        ]
    );
    assert_eq!(body, b"\x00\xffraw");
    assert!(has_idempotency);
}

#[tokio::test]
async fn dispatch_maps_every_worker_answer() {
    let ingress = test_ingress();
    let (_lease, mut receivers) = publish(&ingress, &[("fixture", owner("p", "a"), 1)]);
    let mut receiver = receivers.remove(0);
    zeroclaw_spawn::spawn!(async move {
        while let Some(request) = receiver.recv().await {
            let answer = match request.body.as_slice() {
                b"ack" => Ok(WebhookOutcome::Ack),
                b"reply" => Ok(WebhookOutcome::Body("x".to_string())),
                b"empty" => Ok(WebhookOutcome::Body(String::new())),
                b"at-limit" => Ok(WebhookOutcome::Body(
                    "x".repeat(MAX_WEBHOOK_RESPONSE_BODY_BYTES),
                )),
                b"over-limit" => Ok(WebhookOutcome::Body(
                    "λ".repeat(MAX_WEBHOOK_RESPONSE_BODY_BYTES / 2 + 1),
                )),
                b"unauthorized" => Err(WebhookReject::Unauthorized("private".to_string())),
                b"bad-request" => Err(WebhookReject::BadRequest("private".to_string())),
                b"unavailable" => Err(WebhookReject::Unavailable("private".to_string())),
                b"invalid-response" => Err(WebhookReject::InvalidResponse),
                b"timeout" => Err(WebhookReject::Timeout),
                _ => continue,
            };
            let _ = request.reply.send(answer);
        }
    });

    for (body, expected) in [
        ("ack", PluginWebhookOutcome::Ack),
        ("reply", PluginWebhookOutcome::Reply("x".to_string())),
        ("empty", PluginWebhookOutcome::Reply(String::new())),
        (
            "at-limit",
            PluginWebhookOutcome::Reply("x".repeat(MAX_WEBHOOK_RESPONSE_BODY_BYTES)),
        ),
        ("over-limit", PluginWebhookOutcome::InvalidResponse),
        ("unauthorized", PluginWebhookOutcome::Unauthorized),
        ("bad-request", PluginWebhookOutcome::BadRequest),
        ("unavailable", PluginWebhookOutcome::Unavailable),
        ("invalid-response", PluginWebhookOutcome::InvalidResponse),
        ("timeout", PluginWebhookOutcome::Timeout),
        ("drop-reply", PluginWebhookOutcome::Unavailable),
    ] {
        assert_eq!(
            ingress
                .dispatch(
                    post("fixture", body.as_bytes()),
                    &WebhookCancellation::new()
                )
                .await,
            expected,
            "worker answer {body}"
        );
    }
}

#[tokio::test]
async fn dispatch_answers_not_found_for_unknown_and_malformed_paths() {
    let ingress = test_ingress();
    let oversized = "x".repeat(zeroclaw_api::webhook::MAX_PLUGIN_WEBHOOK_PATH_BYTES + 1);
    let (_lease, mut receivers) = publish(
        &ingress,
        &[
            ("fixture", owner("p", "a"), 1),
            ("not.a.route", owner("p", "b"), 1),
            (oversized.as_str(), owner("p", "c"), 1),
        ],
    );

    for path in ["missing", "", "not.a.route", "a/b", oversized.as_str()] {
        assert_eq!(
            ingress
                .dispatch(post(path, b"body"), &WebhookCancellation::new())
                .await,
            PluginWebhookOutcome::NotFound,
            "path {path:?}"
        );
    }
    for receiver in &mut receivers {
        assert!(receiver.try_recv().is_err());
    }
}

#[tokio::test]
async fn dispatch_reports_full_and_closed_route_queues() {
    let ingress = test_ingress();
    let (_lease, mut receivers) = publish(
        &ingress,
        &[
            ("full", owner("p", "full"), 1),
            ("closed", owner("p", "closed"), 1),
        ],
    );
    let full_route = ingress
        .registry()
        .get("full")
        .expect("full route is published");
    let (unanswered, _) = oneshot::channel();
    full_route
        .sink()
        .try_send(post("full", b"prefill").into_raw_webhook(
            WebhookCancellation::new(),
            None,
            unanswered,
        ))
        .unwrap_or_else(|_| panic!("prefill the full route's queue"));
    drop(full_route);
    drop(receivers.remove(1));

    assert_eq!(
        ingress
            .dispatch(post("full", b"body"), &WebhookCancellation::new())
            .await,
        PluginWebhookOutcome::QueueFull
    );
    assert_eq!(
        ingress
            .dispatch(post("closed", b"body"), &WebhookCancellation::new())
            .await,
        PluginWebhookOutcome::Unavailable
    );
    let prefill = receivers[0]
        .try_recv()
        .expect("the prefill is still queued");
    assert_eq!(prefill.body, b"prefill");
    assert!(receivers[0].try_recv().is_err());
}

#[tokio::test(start_paused = true)]
async fn dispatch_times_out_at_the_deadline_and_cancels_the_worker_request() {
    let ingress = test_ingress();
    let (_lease, mut receivers) = publish(&ingress, &[("fixture", owner("p", "a"), 1)]);
    let mut receiver = receivers.remove(0);
    let (cancelled, observed) = oneshot::channel();
    zeroclaw_spawn::spawn!(async move {
        let request = receiver.recv().await.expect("route forwards the request");
        request.cancellation.cancelled().await;
        let _ = cancelled.send(());
    });

    let started = tokio::time::Instant::now();
    assert_eq!(
        ingress
            .dispatch(post("fixture", b"slow"), &WebhookCancellation::new())
            .await,
        PluginWebhookOutcome::Timeout
    );
    assert!(started.elapsed() >= PLUGIN_WEBHOOK_DEADLINE);
    observed
        .await
        .expect("the deadline cancels the worker's request");
}

#[tokio::test(start_paused = true)]
async fn dispatch_times_out_at_the_deadline_and_not_later() {
    let ingress = Arc::new(test_ingress());
    let (_lease, mut receivers) = publish(&ingress, &[("fixture", owner("p", "a"), 1)]);
    let mut receiver = receivers.remove(0);
    let dispatching = Arc::clone(&ingress);
    let dispatch = zeroclaw_spawn::spawn!(async move {
        dispatching
            .dispatch(post("fixture", b"slow"), &WebhookCancellation::new())
            .await
    });
    // Held unanswered so only the deadline can end the dispatch.
    let _request = receiver.recv().await.expect("route forwards the request");

    // Time moves only by `advance`: awaiting the dispatch before it finishes
    // would let the paused clock jump to whatever deadline the ingress set.
    tokio::time::advance(PLUGIN_WEBHOOK_DEADLINE - Duration::from_millis(1)).await;
    run_ready_tasks().await;
    assert!(
        !dispatch.is_finished(),
        "dispatch ended before the deadline"
    );

    tokio::time::advance(Duration::from_millis(2)).await;
    run_ready_tasks().await;
    assert!(dispatch.is_finished(), "dispatch outlived the deadline");
    assert_eq!(
        dispatch.await.expect("dispatch joins"),
        PluginWebhookOutcome::Timeout
    );
}

#[tokio::test]
async fn dispatch_with_a_cancelled_caller_does_not_enqueue() {
    let ingress = test_ingress();
    let (_lease, mut receivers) = publish(&ingress, &[("fixture", owner("p", "a"), 1)]);
    let cancel = WebhookCancellation::new();
    cancel.cancel();

    assert_eq!(
        ingress.dispatch(post("fixture", b"body"), &cancel).await,
        PluginWebhookOutcome::Cancelled
    );
    assert!(matches!(
        receivers[0].try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn caller_cancellation_while_waiting_cancels_the_worker_request() {
    let ingress = Arc::new(test_ingress());
    let (_lease, mut receivers) = publish(&ingress, &[("fixture", owner("p", "a"), 1)]);
    let mut receiver = receivers.remove(0);
    let cancel = WebhookCancellation::new();
    let dispatching = Arc::clone(&ingress);
    let caller = cancel.clone();
    let dispatch = zeroclaw_spawn::spawn!(async move {
        dispatching
            .dispatch(post("fixture", b"slow"), &caller)
            .await
    });

    let request = receiver.recv().await.expect("route forwards the request");
    cancel.cancel();
    assert_eq!(
        dispatch.await.expect("dispatch joins"),
        PluginWebhookOutcome::Cancelled
    );
    assert!(request.cancellation.is_cancelled());
}

/// With the answer and the caller's cancellation ready together, the worker's
/// real outcome wins.
#[tokio::test]
async fn a_ready_worker_outcome_wins_over_a_simultaneous_cancellation() {
    let ingress = Arc::new(test_ingress());
    let (_lease, mut receivers) = publish(&ingress, &[("fixture", owner("p", "a"), 1)]);
    let mut receiver = receivers.remove(0);
    let cancel = WebhookCancellation::new();
    let caller = cancel.clone();
    zeroclaw_spawn::spawn!(async move {
        let request = receiver.recv().await.expect("route forwards the request");
        caller.cancel();
        let _ = request.reply.send(Ok(WebhookOutcome::Ack));
    });

    assert_eq!(
        ingress.dispatch(post("fixture", b"body"), &cancel).await,
        PluginWebhookOutcome::Ack
    );
}

#[tokio::test]
async fn caller_cancellation_wins_over_the_worker_timeout_it_causes() {
    let ingress = Arc::new(test_ingress());
    let (_lease, mut receivers) = publish(&ingress, &[("fixture", owner("p", "a"), 1)]);
    let mut receiver = receivers.remove(0);
    let cancel = WebhookCancellation::new();
    let caller = cancel.clone();
    zeroclaw_spawn::spawn!(async move {
        let request = receiver.recv().await.expect("route forwards the request");
        caller.cancel();
        request.cancellation.cancelled().await;
        let _ = request.reply.send(Err(WebhookReject::Timeout));
    });

    assert_eq!(
        ingress.dispatch(post("fixture", b"body"), &cancel).await,
        PluginWebhookOutcome::Cancelled
    );
}

#[tokio::test(start_paused = true)]
async fn dropping_dispatch_cancels_the_worker_request() {
    let ingress = test_ingress();
    let (_lease, mut receivers) = publish(&ingress, &[("fixture", owner("p", "a"), 1)]);
    let mut receiver = receivers.remove(0);
    let (cancelled, observed) = oneshot::channel();
    zeroclaw_spawn::spawn!(async move {
        let request = receiver.recv().await.expect("route forwards the request");
        request.cancellation.cancelled().await;
        let _ = cancelled.send(());
    });

    let abandoned = tokio::time::timeout(
        Duration::from_millis(50),
        ingress.dispatch(post("fixture", b"slow"), &WebhookCancellation::new()),
    )
    .await;
    assert!(abandoned.is_err());
    observed
        .await
        .expect("dropping dispatch cancels the worker's request");
}

#[tokio::test]
async fn a_waiting_dispatch_does_not_keep_a_retired_route_open() {
    let ingress = Arc::new(test_ingress());
    let (lease, mut receivers) = publish(&ingress, &[("fixture", owner("p", "a"), 1)]);
    let mut receiver = receivers.remove(0);
    let dispatching = Arc::clone(&ingress);
    let dispatch = zeroclaw_spawn::spawn!(async move {
        dispatching
            .dispatch(post("fixture", b"body"), &WebhookCancellation::new())
            .await
    });

    let request = receiver.recv().await.expect("route forwards the request");
    drop(lease);
    let closed = tokio::time::timeout(Duration::from_secs(5), receiver.recv())
        .await
        .expect("retiring the route closes its queue while dispatch waits");
    assert!(closed.is_none());
    request
        .reply
        .send(Ok(WebhookOutcome::Ack))
        .unwrap_or_else(|_| panic!("dispatch still waits for the outcome"));
    assert_eq!(
        dispatch.await.expect("dispatch joins"),
        PluginWebhookOutcome::Ack
    );
}

#[tokio::test]
async fn dedup_is_keyed_by_route_owner_and_survives_republication() {
    async fn reserve_once(
        ingress: &PluginWebhookIngress,
        route_owner: PluginWebhookOwner,
    ) -> &'static str {
        let (_lease, mut receivers) = publish(
            ingress,
            &[("fixture", route_owner, PLUGIN_WEBHOOK_QUEUE_DEPTH)],
        );
        let mut receiver = receivers.remove(0);
        let worker = zeroclaw_spawn::spawn!(async move {
            let request = receiver.recv().await.expect("route forwards the request");
            let reservation = match &request.idempotency {
                None => "no idempotency bridge",
                Some(idempotency) => match idempotency.begin("m") {
                    WebhookReservation::Owner(token) if idempotency.commit(&token) => "owner",
                    WebhookReservation::Owner(_) => "owner, commit refused",
                    WebhookReservation::Committed => "committed",
                    WebhookReservation::InFlight(_) => "in flight",
                    WebhookReservation::Unavailable => "unavailable",
                },
            };
            let _ = request.reply.send(Ok(WebhookOutcome::Ack));
            reservation
        });
        assert_eq!(
            ingress
                .dispatch(post("fixture", b"{}"), &WebhookCancellation::new())
                .await,
            PluginWebhookOutcome::Ack
        );
        worker.await.expect("worker joins")
    }

    let ingress = test_ingress();
    assert_eq!(reserve_once(&ingress, owner("p", "a")).await, "owner");
    assert_eq!(reserve_once(&ingress, owner("p", "a")).await, "committed");
    assert_eq!(reserve_once(&ingress, owner("p", "b")).await, "owner");
    assert_eq!(reserve_once(&ingress, owner("q", "a")).await, "owner");
}

fn attr<'a>(record: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    record["attributes"][key].as_str()
}

#[tokio::test]
async fn dispatch_logs_route_and_worker_failures_with_the_route_owner() {
    let _hook_guard = zeroclaw_log::__private_test_hook_lock();
    zeroclaw_log::try_install_capture_subscriber();
    let mut records = zeroclaw_log::subscribe_or_install();
    while records.try_recv().is_ok() {}

    let ingress = test_ingress();
    let (_lease, mut receivers) = publish(
        &ingress,
        &[
            ("logged", owner("log-plugin", "log-alias"), 1),
            ("logged-closed", owner("log-plugin", "log-closed-alias"), 1),
        ],
    );
    drop(receivers.remove(1));
    let mut receiver = receivers.remove(0);
    zeroclaw_spawn::spawn!(async move {
        while let Some(request) = receiver.recv().await {
            let answer = match request.body.as_slice() {
                b"unauthorized" => Err(WebhookReject::Unauthorized("private".to_string())),
                b"bad-request" => Err(WebhookReject::BadRequest("private".to_string())),
                b"unavailable" => Err(WebhookReject::Unavailable("private".to_string())),
                _ => continue,
            };
            let _ = request.reply.send(answer);
        }
    });

    for (path, channel_alias, body, outcome, level, error_key) in [
        (
            "logged",
            "log-alias",
            "unauthorized",
            PluginWebhookOutcome::Unauthorized,
            "WARN",
            "plugin_webhook_unauthorized",
        ),
        (
            "logged",
            "log-alias",
            "bad-request",
            PluginWebhookOutcome::BadRequest,
            "WARN",
            "plugin_webhook_invalid",
        ),
        (
            "logged",
            "log-alias",
            "unavailable",
            PluginWebhookOutcome::Unavailable,
            "ERROR",
            "plugin_webhook_unavailable",
        ),
        (
            "logged",
            "log-alias",
            "drop-reply",
            PluginWebhookOutcome::Unavailable,
            "WARN",
            "plugin_webhook_reply_dropped",
        ),
        (
            "logged-closed",
            "log-closed-alias",
            "body",
            PluginWebhookOutcome::Unavailable,
            "WARN",
            "plugin_webhook_route_closed",
        ),
    ] {
        assert_eq!(
            ingress
                .dispatch(post(path, body.as_bytes()), &WebhookCancellation::new())
                .await,
            outcome,
            "{error_key}"
        );
        let mut logged = Vec::new();
        while let Ok(record) = records.try_recv() {
            if attr(&record, "plugin") == Some("log-plugin") {
                logged.push(record);
            }
        }
        let [record] = logged.as_slice() else {
            panic!("expected one {error_key} record, got {logged:#?}");
        };
        assert_eq!(record["severity_text"], level, "{record:#?}");
        assert_eq!(attr(record, "error_key"), Some(error_key), "{record:#?}");
        assert_eq!(
            attr(record, "channel_alias"),
            Some(channel_alias),
            "{record:#?}"
        );
        assert_eq!(attr(record, "path"), Some(path), "{record:#?}");
    }
    zeroclaw_log::clear_broadcast_hook();
}

/// Dispatch to a worker that drops its reply sender without answering,
/// cancelling the caller first when `caller_cancels`. Returns the outcome and
/// the log records that name the route's plugin.
async fn dispatch_to_a_dropped_reply(
    caller_cancels: bool,
) -> (PluginWebhookOutcome, Vec<serde_json::Value>) {
    let _hook_guard = zeroclaw_log::__private_test_hook_lock();
    zeroclaw_log::try_install_capture_subscriber();
    let mut records = zeroclaw_log::subscribe_or_install();
    while records.try_recv().is_ok() {}

    let ingress = test_ingress();
    let (_lease, mut receivers) = publish(
        &ingress,
        &[("dropped", owner("drop-plugin", "drop-alias"), 1)],
    );
    let mut receiver = receivers.remove(0);
    let cancel = WebhookCancellation::new();
    let caller = cancel.clone();
    zeroclaw_spawn::spawn!(async move {
        let request = receiver.recv().await.expect("route forwards the request");
        if caller_cancels {
            caller.cancel();
            request.cancellation.cancelled().await;
        }
        drop(request);
    });
    let outcome = ingress.dispatch(post("dropped", b"body"), &cancel).await;

    let mut logged = Vec::new();
    while let Ok(record) = records.try_recv() {
        if attr(&record, "plugin") == Some("drop-plugin") {
            logged.push(record);
        }
    }
    zeroclaw_log::clear_broadcast_hook();
    (outcome, logged)
}

#[tokio::test]
async fn a_reply_dropped_after_caller_cancellation_is_cancelled_and_not_logged() {
    let (outcome, logged) = dispatch_to_a_dropped_reply(true).await;
    assert_eq!(outcome, PluginWebhookOutcome::Cancelled);
    assert!(logged.is_empty(), "{logged:#?}");
}

#[tokio::test]
async fn a_reply_dropped_without_caller_cancellation_is_unavailable_and_logged() {
    let (outcome, logged) = dispatch_to_a_dropped_reply(false).await;
    assert_eq!(outcome, PluginWebhookOutcome::Unavailable);
    let [record] = logged.as_slice() else {
        panic!("expected one plugin_webhook_reply_dropped record, got {logged:#?}");
    };
    assert_eq!(record["severity_text"], "WARN", "{record:#?}");
    assert_eq!(
        attr(record, "error_key"),
        Some("plugin_webhook_reply_dropped"),
        "{record:#?}"
    );
}
