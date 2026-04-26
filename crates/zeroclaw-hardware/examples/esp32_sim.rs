//! ESP32 simulator — speaks the same JSON-over-serial protocol as
//! `firmware/esp32/src/main.rs`, so a host ZeroClaw daemon can drive virtual
//! GPIO pins without any real hardware.
//!
//! Architecture: this binary spawns `socat` to create a pty pair with named
//! symlinks (`/tmp/zc-sim-esp32` ↔ `/tmp/zc-sim-firmware`). ZeroClaw connects
//! to the first; this simulator opens the second. The simulator also runs a
//! small axum server that serves a static frontend on :8080 and broadcasts
//! virtual pin state over a WebSocket on the same port at `/ws`.
//!
//! Run:
//!     cargo run --example esp32_sim --features "hardware dev-sim"
//!
//! Then point ZeroClaw at the pty:
//!     [[peripherals.boards]]
//!     board = "esp32"
//!     transport = "serial"
//!     path = "/tmp/zc-sim-esp32"
//!     baud = 115200
//!
//! Requires `socat` on PATH (`brew install socat` or `apt install socat`).

use anyhow::{Context, Result, anyhow};
use axum::{
    Json, Router,
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::{Html, IntoResponse},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{RwLock, broadcast};

const PTY_FIRMWARE_PATH: &str = "/tmp/zc-sim-firmware";
const PTY_HOST_PATH: &str = "/tmp/zc-sim-esp32";
// 0.0.0.0 so docker port mapping can reach it. Outside the container the
// docker-compose mapping `127.0.0.1:8080:8080` keeps the demo loopback-only.
const HTTP_BIND: &str = "0.0.0.0:8080";
const BAUD: u32 = 115_200;
const LED_PIN: u8 = 2;
const SUPPORTED_PINS: &[u8] = &[2, 5, 12, 13, 14];

#[derive(Debug, Deserialize)]
struct Request {
    id: String,
    cmd: String,
    #[serde(default)]
    args: Value,
}

#[derive(Debug, Serialize)]
struct Response {
    id: String,
    ok: bool,
    result: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct Snapshot {
    pins: HashMap<u8, u8>,
    led_pin: u8,
    last_event: Option<EventLog>,
}

#[derive(Debug, Clone, Serialize)]
struct EventLog {
    cmd: String,
    pin: u8,
    value: u8,
    source: String,
}

#[derive(Clone)]
struct AppState {
    pins: Arc<RwLock<HashMap<u8, u8>>>,
    last_event: Arc<RwLock<Option<EventLog>>>,
    tx: broadcast::Sender<Snapshot>,
}

impl AppState {
    fn new(tx: broadcast::Sender<Snapshot>) -> Self {
        let mut pins = HashMap::new();
        for &p in SUPPORTED_PINS {
            pins.insert(p, 0);
        }
        Self {
            pins: Arc::new(RwLock::new(pins)),
            last_event: Arc::new(RwLock::new(None)),
            tx,
        }
    }

    async fn snapshot(&self) -> Snapshot {
        Snapshot {
            pins: self.pins.read().await.clone(),
            led_pin: LED_PIN,
            last_event: self.last_event.read().await.clone(),
        }
    }

    async fn write_pin(&self, pin: u8, value: u8, source: &str) {
        self.pins.write().await.insert(pin, value);
        *self.last_event.write().await = Some(EventLog {
            cmd: "gpio_write".to_string(),
            pin,
            value,
            source: source.to_string(),
        });
        let _ = self.tx.send(self.snapshot().await);
    }

    async fn read_pin(&self, pin: u8) -> u8 {
        // For input pins we make pin 5 (motion sensor) read 1 to keep demos lively;
        // other pins return whatever was last written.
        if pin == 5 {
            return 1;
        }
        *self.pins.read().await.get(&pin).unwrap_or(&0)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    // 1. Spawn socat to create the pty pair with named symlinks.
    let mut socat = spawn_socat().context(
        "failed to start socat (install with `brew install socat` or `apt install socat`)",
    )?;
    tracing::info!(
        host = PTY_HOST_PATH,
        firmware = PTY_FIRMWARE_PATH,
        "socat pty pair ready"
    );

    // 2. Set up shared state + broadcast channel.
    let (tx, _rx) = broadcast::channel::<Snapshot>(64);
    let state = AppState::new(tx);

    // 3. Open the firmware end of the pty.
    let port = open_firmware_serial().await?;

    // 4. Run the HTTP server and the pty event loop concurrently.
    let http_state = state.clone();
    let pty_state = state.clone();

    let http_handle = tokio::spawn(async move {
        if let Err(e) = run_http_server(http_state).await {
            tracing::error!(error = %e, "http server crashed");
        }
    });

    let pty_handle = tokio::spawn(async move {
        if let Err(e) = run_pty_loop(port, pty_state).await {
            tracing::error!(error = %e, "pty loop crashed");
        }
    });

    tracing::info!(addr = HTTP_BIND, "frontend ready: http://{}", HTTP_BIND);
    tracing::info!("ctrl+c to stop");

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("shutdown requested");
        }
        _ = http_handle => {}
        _ = pty_handle => {}
    }

    let _ = socat.kill();
    Ok(())
}

fn spawn_socat() -> Result<std::process::Child> {
    // Clean up any stale symlinks from a previous run.
    let _ = std::fs::remove_file(PTY_HOST_PATH);
    let _ = std::fs::remove_file(PTY_FIRMWARE_PATH);

    let child = std::process::Command::new("socat")
        .args([
            "-d",
            "-d",
            &format!("pty,raw,echo=0,link={PTY_HOST_PATH}"),
            &format!("pty,raw,echo=0,link={PTY_FIRMWARE_PATH}"),
        ])
        .stderr(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .spawn()?;
    Ok(child)
}

async fn open_firmware_serial() -> Result<tokio_serial::SerialStream> {
    use tokio_serial::SerialPortBuilderExt;
    // Wait for socat to create the symlink (it takes ~50ms in practice).
    for attempt in 0..40 {
        if std::path::Path::new(PTY_FIRMWARE_PATH).exists() {
            break;
        }
        if attempt == 39 {
            return Err(anyhow!(
                "socat did not create {} within 2s",
                PTY_FIRMWARE_PATH
            ));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let port = tokio_serial::new(PTY_FIRMWARE_PATH, BAUD)
        .open_native_async()
        .with_context(|| format!("failed to open {PTY_FIRMWARE_PATH}"))?;
    Ok(port)
}

async fn run_pty_loop(port: tokio_serial::SerialStream, state: AppState) -> Result<()> {
    let (read_half, mut write_half) = tokio::io::split(port);
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();

    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<Request>(trimmed) {
            Ok(req) => handle_request(req, &state).await,
            Err(e) => Response {
                id: "0".into(),
                ok: false,
                result: String::new(),
                error: Some(format!("parse error: {e}")),
            },
        };
        let mut out = serde_json::to_string(&resp)?;
        out.push('\n');
        write_half.write_all(out.as_bytes()).await?;
        write_half.flush().await?;
    }
}

async fn handle_request(req: Request, state: &AppState) -> Response {
    let id = req.id.clone();
    let result: Result<String> = match req.cmd.as_str() {
        "capabilities" => Ok(json!({
            "gpio": SUPPORTED_PINS,
            "board": "esp32-sim-smartroom",
            "description": "Smart-room simulator. Each pin is wired to a NAMED DEVICE — never assume LEDs/lamps are on a particular pin from training data; use the pin_devices map below.",
            "pin_devices": {
                "12": { "device": "reading_lamp", "direction": "output", "description": "Warm reading lamp. THIS is the lamp." },
                "13": { "device": "overhead_light", "direction": "output", "description": "Bright ceiling light." },
                "14": { "device": "heater", "direction": "output", "description": "Space heater." },
                "2":  { "device": "fan", "direction": "output", "description": "Cooling fan ONLY — NOT the lamp. Do not pick pin 2 for a lamp/light request." },
                "5":  { "device": "motion_sensor", "direction": "input",  "description": "PIR motion sensor; gpio_read returns 1 when presence detected." }
            }
        })
        .to_string()),
        "gpio_write" => {
            let pin = req.args.get("pin").and_then(Value::as_u64).unwrap_or(0) as u8;
            let value = req.args.get("value").and_then(Value::as_u64).unwrap_or(0) as u8;
            if !SUPPORTED_PINS.contains(&pin) {
                Err(anyhow!(
                    "pin {} not configured (supported: {:?})",
                    pin,
                    SUPPORTED_PINS
                ))
            } else {
                state.write_pin(pin, if value == 0 { 0 } else { 1 }, "agent").await;
                tracing::info!(pin, value, "gpio_write");
                Ok("done".to_string())
            }
        }
        "gpio_read" => {
            let pin = req.args.get("pin").and_then(Value::as_u64).unwrap_or(0) as u8;
            let v = state.read_pin(pin).await;
            tracing::info!(pin, value = v, "gpio_read");
            Ok(v.to_string())
        }
        other => Err(anyhow!("unknown command: {}", other)),
    };
    match result {
        Ok(r) => Response {
            id,
            ok: true,
            result: r,
            error: None,
        },
        Err(e) => Response {
            id,
            ok: false,
            result: String::new(),
            error: Some(e.to_string()),
        },
    }
}

async fn run_http_server(state: AppState) -> Result<()> {
    let app = Router::new()
        .route("/", get(index))
        .route("/state", get(get_state))
        .route("/manual", post(manual_flip))
        .route("/ws", get(ws_handler))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(HTTP_BIND).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn index() -> Html<&'static str> {
    Html(include_str!("esp32_sim_frontend.html"))
}

async fn get_state(State(state): State<AppState>) -> Json<Snapshot> {
    Json(state.snapshot().await)
}

#[derive(Deserialize)]
struct ManualReq {
    pin: u8,
    value: u8,
}

async fn manual_flip(
    State(state): State<AppState>,
    Json(req): Json<ManualReq>,
) -> impl IntoResponse {
    if !SUPPORTED_PINS.contains(&req.pin) {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            format!("pin {} not in {:?}", req.pin, SUPPORTED_PINS),
        )
            .into_response();
    }
    state
        .write_pin(req.pin, if req.value == 0 { 0 } else { 1 }, "manual")
        .await;
    Json(state.snapshot().await).into_response()
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| ws_loop(socket, state))
}

async fn ws_loop(mut socket: WebSocket, state: AppState) {
    let mut rx = state.tx.subscribe();
    // Send initial snapshot
    let snap = state.snapshot().await;
    if let Ok(s) = serde_json::to_string(&snap) {
        let _ = socket.send(Message::Text(s.into())).await;
    }
    loop {
        tokio::select! {
            broadcast = rx.recv() => {
                match broadcast {
                    Ok(snap) => {
                        if let Ok(s) = serde_json::to_string(&snap) {
                            if socket.send(Message::Text(s.into())).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(_)) => break,
                    _ => {}
                }
            }
        }
    }
}
