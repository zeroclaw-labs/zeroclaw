//! Exercise timeout selection through the real gateway and agent dispatch.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::response::IntoResponse;
use axum::{Json, Router, routing::post};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use zeroclaw_config::multi_agent::{AgentMemoryConfig, AgentWorkspaceConfig, MemoryBackendKind};
use zeroclaw_config::schema::{
    AliasedAgentConfig, Config, CustomModelProviderConfig, ModelProviderConfig, RiskProfileConfig,
};

struct Fixture {
    addr: SocketAddr,
    requests: Arc<AtomicUsize>,
    gateway: tokio::task::JoinHandle<anyhow::Result<()>>,
    provider: tokio::task::JoinHandle<()>,
    _root: tempfile::TempDir,
}

impl Fixture {
    async fn start(ordinary: u64, long_running: u64) -> Self {
        Self::configured(ordinary, long_running, |_| {}).await
    }

    async fn configured(
        ordinary: u64,
        long_running: u64,
        configure: impl FnOnce(&mut Config),
    ) -> Self {
        Self::with_response_gate(ordinary, long_running, configure, None).await
    }

    async fn with_response_gate(
        ordinary: u64,
        long_running: u64,
        configure: impl FnOnce(&mut Config),
        response_gate: Option<Arc<tokio::sync::Semaphore>>,
    ) -> Self {
        let requests = Arc::new(AtomicUsize::new(0));
        let counter = requests.clone();
        let app = Router::new().route(
            "/v1/chat/completions",
            post(move |Json(request): Json<Value>| {
                let counter = counter.clone();
                let response_gate = response_gate.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    if let Some(gate) = response_gate {
                        gate.acquire().await.unwrap().forget();
                    } else {
                        tokio::time::sleep(Duration::from_secs(3)).await;
                    }
                    if request["stream"] == true {
                        let chunk = json!({"id": "timeout-fixture", "model": "fixture-model",
                            "choices": [{"index": 0, "delta": {"content": "completed"}}]});
                        let end = json!({"id": "timeout-fixture", "model": "fixture-model",
                            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]});
                        return (
                            [("content-type", "text/event-stream")],
                            format!("data: {chunk}\n\ndata: {end}\n\ndata: [DONE]\n\n"),
                        )
                            .into_response();
                    }
                    Json(json!({
                        "id": "timeout-fixture", "model": "fixture-model",
                        "choices": [{"index": 0, "message": {
                            "role": "assistant", "content": "completed"
                        }, "finish_reason": "stop"}]
                    }))
                    .into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let provider_addr = listener.local_addr().unwrap();
        let provider = zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let mut config = Config {
            data_dir: workspace.clone(),
            config_path: root.path().join("config.toml"),
            ..Config::default()
        };
        config.gateway.require_pairing = false;
        config.gateway.request_timeout_secs = ordinary;
        config.gateway.long_running_request_timeout_secs = long_running;
        config.gateway.session_persistence = false;
        config.memory.backend = "none".into();
        config.memory.auto_save = false;
        config.memory.response_cache_enabled = false;
        config.reliability.provider_retries = 0;
        config.providers.models.custom.insert(
            "fixture".into(),
            CustomModelProviderConfig {
                base: ModelProviderConfig {
                    uri: Some(format!("http://{provider_addr}/v1")),
                    model: Some("fixture-model".into()),
                    ..ModelProviderConfig::default()
                },
            },
        );
        config.risk_profiles.insert(
            "fixture".into(),
            RiskProfileConfig {
                allowed_tools: vec!["__fixture_no_tools__".into()],
                ..RiskProfileConfig::default()
            },
        );
        config.agents.insert(
            "fixture".into(),
            AliasedAgentConfig {
                model_provider: "custom.fixture".into(),
                risk_profile: "fixture".into(),
                memory: AgentMemoryConfig {
                    backend: MemoryBackendKind::None,
                },
                workspace: AgentWorkspaceConfig {
                    path: Some(workspace),
                    ..AgentWorkspaceConfig::default()
                },
                ..AliasedAgentConfig::default()
            },
        );
        configure(&mut config);
        let (sop_engine, sop_audit) = if config.sop.sops_dir.is_some() {
            let (engine, audit) = zeroclaw_runtime::sop::build_sop_engine(
                config.sop.clone(),
                &config.decision_models,
                &config.data_dir,
                &config.install_root_dir(),
                Arc::new(zeroclaw_memory::none::NoneMemory::new("fixture")),
                Default::default(),
            );
            (Some(engine), Some(audit))
        } else {
            (None, None)
        };
        let (ready_tx, mut ready_rx) = tokio::sync::watch::channel(None);
        let readiness = zeroclaw_runtime::daemon::GatewayReadinessReporter::new(move |addr| {
            ready_tx.send_replace(Some(addr));
        });
        let gateway = zeroclaw_spawn::spawn!(async move {
            zeroclaw_gateway::run_gateway(
                "127.0.0.1",
                0,
                config,
                None,
                None,
                None,
                None,
                sop_engine,
                sop_audit,
                None,
                None,
                Some(readiness),
            )
            .await
        });
        let addr = tokio::time::timeout(Duration::from_secs(20), async {
            ready_rx.wait_for(Option::is_some).await.unwrap();
            ready_rx.borrow().unwrap()
        })
        .await
        .expect("gateway readiness");
        Self {
            addr,
            requests,
            gateway,
            provider,
            _root: root,
        }
    }

    async fn post(&self, path: &str, body: &str, extra_headers: &str) -> (u16, String) {
        let mut stream = tokio::net::TcpStream::connect(self.addr).await.unwrap();
        let request = format!(
            "POST {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{extra_headers}\r\n{body}",
            self.addr,
            body.len()
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        Self::read_response(&mut stream).await
    }

    async fn read_response(stream: &mut tokio::net::TcpStream) -> (u16, String) {
        let mut response = String::new();
        let result = tokio::time::timeout(
            Duration::from_secs(20),
            stream.read_to_string(&mut response),
        )
        .await
        .expect("HTTP response deadline");
        // A body-limit rejection may close with unread request bytes. Preserve
        // the received HTTP response when the peer resets that connection.
        if let Err(error) = result {
            assert_eq!(error.kind(), std::io::ErrorKind::ConnectionReset);
            assert!(response.starts_with("HTTP/1.1 "), "{error}");
        }
        let status = response.split_whitespace().nth(1).unwrap().parse().unwrap();
        (status, response)
    }
}

#[test]
fn webhook_preserves_admission_and_idempotency() {
    run(async || {
        let secret = format!("{:032x}", rand::random::<u128>());
        let fixture = Fixture::configured(1, 10, |config| {
            config.gateway.webhook_secret = Some(secret.clone());
        })
        .await;
        let path = "/webhook?agent=fixture";
        let body = r#"{"message":"reply"}"#;
        assert_eq!(fixture.post(path, body, "").await.0, 401);
        let auth = format!("X-Webhook-Secret: {secret}\r\n");
        // A declared oversized body is rejected before upload. Sending only
        // headers avoids a TCP reset from closing with unread payload bytes.
        let mut stream = tokio::net::TcpStream::connect(fixture.addr).await.unwrap();
        let oversized = format!(
            "POST {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{auth}\r\n",
            fixture.addr,
            zeroclaw_gateway::MAX_BODY_SIZE + 1
        );
        stream.write_all(oversized.as_bytes()).await.unwrap();
        assert_eq!(Fixture::read_response(&mut stream).await.0, 413);
        assert_eq!(fixture.requests.load(Ordering::SeqCst), 0);
        let headers = format!("{auth}X-Idempotency-Key: fixture-attempt\r\n");
        assert_eq!(fixture.post(path, body, &headers).await.0, 200);
        let (status, duplicate) = fixture.post(path, body, &headers).await;
        assert_eq!(status, 200);
        assert!(duplicate.contains("duplicate"), "{duplicate}");
        assert_eq!(fixture.requests.load(Ordering::SeqCst), 1);
    });
}

#[test]
fn webhook_body_receipt_uses_the_long_running_deadline() {
    run(async || {
        let fixture = Fixture::start(1, 4).await;
        let body = r#"{"message":"reply"}"#;
        let mut stream = tokio::net::TcpStream::connect(fixture.addr).await.unwrap();
        let headers = format!(
            "POST /webhook?agent=missing HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            fixture.addr,
            body.len()
        );
        stream.write_all(headers.as_bytes()).await.unwrap();
        tokio::time::sleep(Duration::from_secs(2)).await;
        stream.write_all(body.as_bytes()).await.unwrap();
        // Completing the body after the ordinary deadline still reaches the
        // handler. This deliberately characterizes the whole-route policy.
        assert_eq!(Fixture::read_response(&mut stream).await.0, 400);

        let mut stream = tokio::net::TcpStream::connect(fixture.addr).await.unwrap();
        stream.write_all(headers.as_bytes()).await.unwrap();
        assert_eq!(Fixture::read_response(&mut stream).await.0, 408);
        assert_eq!(fixture.requests.load(Ordering::SeqCst), 0);
    });
}

#[test]
fn webhook_retry_after_marker_expiry_can_overlap_an_active_turn() {
    run(async || {
        let response_gate = Arc::new(tokio::sync::Semaphore::new(0));
        let fixture = Fixture::with_response_gate(
            1,
            20,
            |config| config.gateway.idempotency_ttl_secs = 1,
            Some(response_gate.clone()),
        )
        .await;
        let request = || {
            fixture.post(
                "/webhook?agent=fixture",
                r#"{"message":"reply"}"#,
                "X-Idempotency-Key: expiring-attempt\r\n",
            )
        };
        let first = request();
        tokio::pin!(first);
        let wait_for_calls = async |expected| {
            tokio::time::timeout(Duration::from_secs(5), async {
                while fixture.requests.load(Ordering::SeqCst) < expected {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("provider request admission");
        };
        let expired = async {
            wait_for_calls(1).await;
            tokio::time::sleep(Duration::from_millis(1200)).await;
        };
        tokio::select! {
            result = &mut first => panic!("first turn ended before marker expiry: {result:?}"),
            () = expired => {}
        }
        let retry = request();
        tokio::pin!(retry);
        // Prove that the second provider call starts before the original HTTP
        // request finishes, rather than merely counting eventual calls.
        tokio::select! {
            result = &mut first => panic!("first turn ended before overlapping retry: {result:?}"),
            result = &mut retry => panic!("retry ended before overlapping provider call: {result:?}"),
            () = wait_for_calls(2) => {}
        }
        // Release responses only after both calls have reached the provider.
        // Scheduler delays cannot complete the first turn before overlap.
        response_gate.add_permits(2);
        let (first, retry) = tokio::join!(first, retry);
        assert_eq!(first.0, 200, "{}", first.1);
        assert_eq!(retry.0, 200, "{}", retry.1);
        assert!(retry.1.contains("completed"), "{}", retry.1);
    });
}

#[test]
fn ordinary_and_sop_routes_keep_the_short_body_deadline() {
    run(async || {
        let fixture = Fixture::start(1, 10).await;
        for path in ["/api/memory", "/sop/fixture"] {
            let mut stream = tokio::net::TcpStream::connect(fixture.addr).await.unwrap();
            // Leave the body unfinished so the real extractor must wait.
            let headers = format!(
                "POST {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: 10\r\n\r\n",
                fixture.addr
            );
            stream.write_all(headers.as_bytes()).await.unwrap();
            let (status, response) = Fixture::read_response(&mut stream).await;
            assert_eq!(status, 408, "{response}");
        }
        assert_eq!(fixture.requests.load(Ordering::SeqCst), 0);
    });
}

#[test]
fn webhook_keeps_its_rate_limit() {
    run(async || {
        let fixture = Fixture::configured(1, 10, |config| {
            config.gateway.webhook_rate_limit_per_minute = 1;
        })
        .await;
        let body = r#"{"message":"reply"}"#;
        assert_eq!(
            fixture.post("/webhook?agent=missing", body, "").await.0,
            400
        );
        assert_eq!(
            fixture.post("/webhook?agent=fixture", body, "").await.0,
            429
        );
        assert_eq!(fixture.requests.load(Ordering::SeqCst), 0);
    });
}

#[test]
fn webhook_still_dispatches_sop_before_chat_routing() {
    run(async || {
        let secret = format!("{:032x}", rand::random::<u128>());
        let fixture = Fixture::configured(1, 10, |config| {
            config.gateway.webhook_secret = Some(secret.clone());
            let sops_dir = config.data_dir.join("sops");
            let definition = sops_dir.join("fixture");
            std::fs::create_dir_all(&definition).unwrap();
            std::fs::write(
                definition.join("SOP.toml"),
                "[sop]\nname = \"fixture\"\ndescription = \"Webhook fixture\"\nexecution_mode = \"auto\"\n\n[[triggers]]\ntype = \"webhook\"\npath = \"/webhook\"\n",
            )
            .unwrap();
            std::fs::write(
                definition.join("SOP.md"),
                "## Steps\n\n1. **Inspect** - Inspect the fixture.\n",
            )
            .unwrap();
            config.sop.sops_dir = Some(sops_dir.to_string_lossy().into_owned());
            config.sop.persist_runs = false;
        })
        .await;
        let headers = format!("X-Webhook-Secret: {secret}\r\n");
        // A matching SOP must take precedence even over an invalid chat alias.
        let (status, response) = fixture
            .post(
                "/webhook?agent=missing",
                r#"{"message":"inspect"}"#,
                &headers,
            )
            .await;
        assert_eq!(status, 200, "{response}");
        assert!(response.contains("accepted"), "{response}");
        assert_eq!(fixture.requests.load(Ordering::SeqCst), 0);
    });
}

#[test]
fn webhook_sse_can_finish_after_the_ordinary_deadline() {
    run(async || {
        // Neither request budget imposes a total SSE body lifetime once the
        // handler has returned its response headers.
        for long_running in [1, 10] {
            let fixture = Fixture::start(1, long_running).await;
            let (status, response) = fixture
                .post(
                    "/webhook?agent=fixture",
                    r#"{"message":"reply","stream":true}"#,
                    "Accept: text/event-stream\r\n",
                )
                .await;
            assert_eq!(status, 200, "{response}");
            assert!(response.contains("event: done"), "{response}");
            assert!(response.contains("completed"), "{response}");
            assert_eq!(fixture.requests.load(Ordering::SeqCst), 1);
        }
    });
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.gateway.abort();
        self.provider.abort();
    }
}

// Building a real agent needs more than the default test thread stack on Linux.
fn run(test: impl AsyncFnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .unwrap()
                .block_on(test());
        })
        .unwrap()
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
}

#[test]
fn webhook_uses_long_running_budget() {
    run(async || {
        // The same slow provider succeeds above its execution time and times
        // out below it, even when the ordinary budget would allow completion.
        for (ordinary, long_running, expected) in [(1, 10, 200), (10, 1, 408)] {
            let fixture = Fixture::start(ordinary, long_running).await;
            let (status, response) = fixture
                .post("/webhook?agent=fixture", r#"{"message":"reply"}"#, "")
                .await;
            assert_eq!(status, expected, "{response}");
            assert_eq!(fixture.requests.load(Ordering::SeqCst), 1);
            if expected == 200 {
                assert!(response.contains("completed"), "{response}");
            }
        }
    });
}
