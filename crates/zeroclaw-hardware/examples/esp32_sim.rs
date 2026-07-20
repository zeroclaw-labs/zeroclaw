//! ESP32 simulator — speaks the same JSON-over-serial protocol as
//! `firmware/esp32/src/main.rs`, so a host ZeroClaw daemon can drive virtual
//! GPIO pins without any real hardware.

use anyhow::{Context, Result};
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
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{RwLock, broadcast};
use zeroclaw_hardware::util::serial_open_baud;
#[cfg(unix)]
use zeroclaw_hardware::util::should_open_serial_nonexclusive;

const PTY_FIRMWARE_PATH: &str = "/tmp/zc-sim-firmware";
const PTY_HOST_PATH: &str = "/tmp/zc-sim-esp32";
// 0.0.0.0 so docker port mapping can reach it. Outside the container the
// docker-compose mapping `127.0.0.1:8080:8080` keeps the demo loopback-only.
const HTTP_BIND: &str = "0.0.0.0:8080";
const BAUD: u32 = 115_200;
const PTY_OPEN_ATTEMPTS: usize = 50;
const PTY_OPEN_MAX_RETRY_DELAY_MS: u64 = 250;
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

    // Optional first CLI argument to override the bind address (addresses review suggestion).
    // Default is still 0.0.0.0:8080 for Docker demo convenience.
    // Usage: cargo run --example esp32_sim --features "hardware dev-sim" -- 127.0.0.1:8080
    let bind_addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| HTTP_BIND.to_string());

    // 1. Spawn socat to create the pty pair with named symlinks.
    let mut socat = spawn_socat().context(
        "failed to start socat (install with `brew install socat` or `apt install socat`)",
    )?;

    // 2. Set up shared state + broadcast channel.
    let (tx, _rx) = broadcast::channel::<Snapshot>(64);
    let state = AppState::new(tx);

    // 3. Open the firmware end of the pty.
    let port = match open_firmware_serial().await {
        Ok(port) => port,
        Err(e) => {
            let _ = socat.kill();
            let _ = socat.wait();
            return Err(e);
        }
    };
    eprintln!(
        "socat pty pair ready (host={}, firmware={})",
        PTY_HOST_PATH, PTY_FIRMWARE_PATH
    );

    // 4. Run the HTTP server and the pty event loop concurrently.
    let http_state = state.clone();
    let pty_state = state.clone();

    let bind_for_http = bind_addr.clone();
    let http_handle = zeroclaw_spawn::spawn!(async move {
        if let Err(e) = run_http_server(http_state, bind_for_http).await {
            eprintln!("http server crashed: {}", e);
        }
    });

    let pty_handle = zeroclaw_spawn::spawn!(async move {
        if let Err(e) = run_pty_loop(port, pty_state).await {
            eprintln!("pty loop crashed: {}", e);
        }
    });

    eprintln!("frontend ready: http://{}", bind_addr);
    eprintln!("ctrl+c to stop");

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            eprintln!("shutdown requested");
        }
        _ = http_handle => {}
        _ = pty_handle => {}
    }

    let _ = socat.kill();
    let _ = socat.wait();
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
        // Let socat's diagnostic output (PTY device names, errors) go to the terminal.
        // This helps debug pty creation issues on macOS.
        .stderr(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .spawn()?;
    Ok(child)
}

async fn open_firmware_serial() -> Result<tokio_serial::SerialStream> {
    use tokio_serial::SerialPortBuilderExt;

    let mut announced_paths = false;
    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 1..=PTY_OPEN_ATTEMPTS {
        let host_path = match resolve_pty_slave_path(PTY_HOST_PATH) {
            Ok(Some(path)) => path,
            Ok(None) => {
                last_err = Some(anyhow::Error::msg(format!(
                    "socat has not created {PTY_HOST_PATH} yet"
                )));
                sleep_before_pty_open_retry(attempt).await;
                continue;
            }
            Err(e) => {
                last_err = Some(e);
                sleep_before_pty_open_retry(attempt).await;
                continue;
            }
        };

        let firmware_path = match resolve_pty_slave_path(PTY_FIRMWARE_PATH) {
            Ok(Some(path)) => path,
            Ok(None) => {
                last_err = Some(anyhow::Error::msg(format!(
                    "socat has not created {PTY_FIRMWARE_PATH} yet"
                )));
                sleep_before_pty_open_retry(attempt).await;
                continue;
            }
            Err(e) => {
                last_err = Some(e);
                sleep_before_pty_open_retry(attempt).await;
                continue;
            }
        };

        if !announced_paths {
            eprintln!(
                "socat pty pair resolved (host={} -> {}, firmware={} -> {})",
                PTY_HOST_PATH,
                host_path.display(),
                PTY_FIRMWARE_PATH,
                firmware_path.display()
            );
            announced_paths = true;
        }

        let builder =
            tokio_serial::new(PTY_FIRMWARE_PATH, serial_open_baud(PTY_FIRMWARE_PATH, BAUD));
        #[cfg(unix)]
        let builder = if should_open_serial_nonexclusive(PTY_FIRMWARE_PATH) {
            builder.exclusive(false)
        } else {
            builder
        };

        match builder.open_native_async() {
            Ok(port) => return Ok(port),
            Err(e) => {
                last_err = Some(anyhow::Error::new(e).context(format!(
                    "open attempt {attempt} for {PTY_FIRMWARE_PATH} -> {}",
                    firmware_path.display()
                )));
                sleep_before_pty_open_retry(attempt).await;
            }
        }
    }

    Err(last_err
        .unwrap_or_else(|| anyhow::Error::msg("unknown open error after retries"))
        .context(format!(
            "failed to open firmware pty after {PTY_OPEN_ATTEMPTS} attempts"
        )))
}

fn resolve_pty_slave_path(link: &str) -> Result<Option<PathBuf>> {
    let link_path = Path::new(link);
    let metadata = match std::fs::symlink_metadata(link_path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("failed to inspect {link}")),
    };
    if !metadata.file_type().is_symlink() {
        anyhow::bail!("{link} exists but is not a symlink to a pty slave");
    }

    let target = std::fs::canonicalize(link_path)
        .with_context(|| format!("failed to resolve pty symlink {link}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;

        let target_metadata = std::fs::metadata(&target)
            .with_context(|| format!("failed to inspect pty target {}", target.display()))?;
        if !target_metadata.file_type().is_char_device() {
            anyhow::bail!(
                "{link} resolves to {}, which is not a character device",
                target.display()
            );
        }
    }

    Ok(Some(target))
}

async fn sleep_before_pty_open_retry(attempt: usize) {
    if attempt < PTY_OPEN_ATTEMPTS {
        tokio::time::sleep(pty_open_retry_delay(attempt)).await;
    }
}

fn pty_open_retry_delay(attempt: usize) -> Duration {
    let attempt = u64::try_from(attempt).unwrap_or(u64::MAX);
    let delay_ms = (25 * attempt).clamp(50, PTY_OPEN_MAX_RETRY_DELAY_MS);
    Duration::from_millis(delay_ms)
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
                Err(anyhow::Error::msg(format!(
                    "pin {} not configured (supported: {:?})",
                    pin,
                    SUPPORTED_PINS
                )))
            } else {
                state.write_pin(pin, if value == 0 { 0 } else { 1 }, "agent").await;
                eprintln!("gpio_write pin={} value={}", pin, value);
                Ok("done".to_string())
            }
        }
        "gpio_read" => {
            let pin = req.args.get("pin").and_then(Value::as_u64).unwrap_or(0) as u8;
            let v = state.read_pin(pin).await;
            eprintln!("gpio_read pin={} value={}", pin, v);
            Ok(v.to_string())
        }
        other => Err(anyhow::Error::msg(format!("unknown command: {}", other))),
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

async fn run_http_server(state: AppState, bind_addr: String) -> Result<()> {
    let app = Router::new()
        .route("/", get(index))
        .route("/state", get(get_state))
        .route("/manual", post(manual_flip))
        .route("/ws", get(ws_handler))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
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
    // Demo-only: this mutates only the simulator's in-memory pin state for the
    // visualizer/manual demo page. It is not forwarded to the pty side.
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
                        if let Ok(s) = serde_json::to_string(&snap)
                            && socket.send(Message::Text(s.into())).await.is_err()
                        {
                            break;
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
