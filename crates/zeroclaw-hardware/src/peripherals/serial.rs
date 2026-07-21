//! Serial peripheral — STM32 and similar boards over USB CDC/serial.

use crate::peripherals::Peripheral;
#[cfg(unix)]
use crate::util::should_open_serial_nonexclusive;
use crate::util::{is_serial_path_allowed, serial_open_baud, serial_path_allowlist_hint};
use async_trait::async_trait;
use portable_atomic::{AtomicU64, Ordering};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::Mutex;
use tokio_serial::{SerialPortBuilderExt, SerialStream};
use zeroclaw_api::attribution::ToolKind;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_api::tool_attribution;
use zeroclaw_config::schema::PeripheralBoardConfig;

tool_attribution!(GpioReadTool, ToolKind::Plugin);
tool_attribution!(GpioWriteTool, ToolKind::Plugin);

/// Timeout for serial request/response (seconds).
const SERIAL_TIMEOUT_SECS: u64 = 5;

/// Maximum malformed or non-matching response frames skipped per request.
const MAX_SKIPPED_RESPONSE_FRAMES: usize = 16;

/// JSON request/response over serial.
async fn send_request<S>(port: &mut S, cmd: &str, args: Value) -> anyhow::Result<Value>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    static ID: AtomicU64 = AtomicU64::new(0);
    let id = ID.fetch_add(1, Ordering::Relaxed);
    let id_str = id.to_string();

    let req = json!({
        "id": id_str,
        "cmd": cmd,
        "args": args
    });
    let line = format!("{}\n", req);

    port.write_all(line.as_bytes()).await?;
    port.flush().await?;

    let mut skipped_frames = 0;
    loop {
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            port.read_exact(&mut byte).await?;
            if byte[0] == b'\n' {
                break;
            }
            buf.push(byte[0]);
        }

        if let Ok(resp) = serde_json::from_slice::<Value>(&buf)
            && resp.get("id").and_then(Value::as_str) == Some(id_str.as_str())
        {
            return Ok(resp);
        }

        if skipped_frames == MAX_SKIPPED_RESPONSE_FRAMES {
            anyhow::bail!(
                "Serial response skip limit exceeded (maximum {MAX_SKIPPED_RESPONSE_FRAMES} frames)"
            );
        }
        skipped_frames += 1;
    }
}

/// Shared serial transport for tools. Pub(crate) for capabilities tool.
pub struct SerialTransport {
    port: Mutex<SerialStream>,
}

impl SerialTransport {
    pub(crate) async fn request(&self, cmd: &str, args: Value) -> anyhow::Result<ToolResult> {
        let mut port = self.port.lock().await;
        // One timeout covers the request and every skipped frame, so stale or
        // malformed input cannot restart the deadline.
        let resp = tokio::time::timeout(
            std::time::Duration::from_secs(SERIAL_TIMEOUT_SECS),
            send_request(&mut *port, cmd, args),
        )
        .await
        .map_err(|_| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Timeout)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "command": cmd,
                        "timeout_secs": SERIAL_TIMEOUT_SECS,
                    })),
                "serial peripheral request timed out"
            );
            anyhow::Error::msg(format!(
                "Serial request timed out after {SERIAL_TIMEOUT_SECS}s"
            ))
        })??;

        let ok = resp["ok"].as_bool().unwrap_or(false);
        let result = resp["result"]
            .as_str()
            .map(String::from)
            .unwrap_or_else(|| resp["result"].to_string());
        let error = resp["error"].as_str().map(String::from);

        Ok(ToolResult {
            success: ok,
            output: result.into(),
            error,
        })
    }

    /// Phase C: fetch capabilities from device (gpio pins, led_pin).
    pub async fn capabilities(&self) -> anyhow::Result<ToolResult> {
        self.request("capabilities", json!({})).await
    }
}

/// Serial peripheral for STM32, Arduino, etc. over USB CDC.
pub struct SerialPeripheral {
    name: String,
    board_type: String,
    transport: Arc<SerialTransport>,
}

impl SerialPeripheral {
    /// Create and connect to a serial peripheral.
    #[allow(clippy::unused_async)]
    pub async fn connect(config: &PeripheralBoardConfig) -> anyhow::Result<Self> {
        let path = config.path.as_deref().ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"board": config.board})),
                "serial peripheral connect refused: config missing 'path'"
            );
            anyhow::Error::msg("Serial peripheral requires path")
        })?;

        if !is_serial_path_allowed(path) {
            anyhow::bail!(
                "Serial path not allowed: {}. Allowed: {}",
                path,
                serial_path_allowlist_hint()
            );
        }

        let builder = tokio_serial::new(path, serial_open_baud(path, config.baud));
        #[cfg(unix)]
        let builder = if should_open_serial_nonexclusive(path) {
            builder.exclusive(false)
        } else {
            builder
        };
        let port = builder.open_native_async().map_err(|e| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "path": path,
                        "baud": config.baud,
                        "error": format!("{}", e),
                    })),
                "serial peripheral open failed"
            );
            anyhow::Error::msg(format!("Failed to open {path}: {e}"))
        })?;

        let name = format!("{}-{}", config.board, path.replace('/', "_"));
        let transport = Arc::new(SerialTransport {
            port: Mutex::new(port),
        });

        Ok(Self {
            name: name.clone(),
            board_type: config.board.clone(),
            transport,
        })
    }
}

#[async_trait]
impl Peripheral for SerialPeripheral {
    fn name(&self) -> &str {
        &self.name
    }

    fn board_type(&self) -> &str {
        &self.board_type
    }

    async fn connect(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn disconnect(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn health_check(&self) -> bool {
        self.transport
            .request("ping", json!({}))
            .await
            .map(|r| r.success)
            .unwrap_or(false)
    }

    fn tools(&self) -> Vec<Box<dyn Tool>> {
        vec![
            Box::new(GpioReadTool {
                transport: self.transport.clone(),
            }),
            Box::new(GpioWriteTool {
                transport: self.transport.clone(),
            }),
        ]
    }
}

impl SerialPeripheral {
    /// Expose transport for capabilities tool (Phase C).
    pub fn transport(&self) -> Arc<SerialTransport> {
        self.transport.clone()
    }
}

/// Tool: read GPIO pin value.
struct GpioReadTool {
    transport: Arc<SerialTransport>,
}

#[async_trait]
impl Tool for GpioReadTool {
    fn name(&self) -> &str {
        "gpio_read"
    }

    fn description(&self) -> &str {
        "Read the value (0 or 1) of a GPIO pin on a connected peripheral (e.g. STM32 Nucleo)"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pin": {
                    "type": "integer",
                    "description": "GPIO pin number (e.g. 13 for LED on Nucleo)"
                }
            },
            "required": ["pin"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let pin = args.get("pin").and_then(|v| v.as_u64()).ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"tool": "gpio_read", "param": "pin"})),
                "tool argument validation failed: missing parameter"
            );
            anyhow::Error::msg("Missing 'pin' parameter")
        })?;
        self.transport
            .request("gpio_read", json!({ "pin": pin }))
            .await
    }
}

/// Tool: write GPIO pin value.
struct GpioWriteTool {
    transport: Arc<SerialTransport>,
}

#[async_trait]
impl Tool for GpioWriteTool {
    fn name(&self) -> &str {
        "gpio_write"
    }

    fn description(&self) -> &str {
        "Set a GPIO pin high (1) or low (0) on a connected peripheral (e.g. turn on/off LED)"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pin": {
                    "type": "integer",
                    "description": "GPIO pin number"
                },
                "value": {
                    "type": "integer",
                    "description": "0 for low, 1 for high"
                }
            },
            "required": ["pin", "value"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let pin = args.get("pin").and_then(|v| v.as_u64()).ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"tool": "gpio_write", "param": "pin"})),
                "tool argument validation failed: missing parameter"
            );
            anyhow::Error::msg("Missing 'pin' parameter")
        })?;
        let value = args.get("value").and_then(|v| v.as_u64()).ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"tool": "gpio_write", "param": "value"})),
                "tool argument validation failed: missing parameter"
            );
            anyhow::Error::msg("Missing 'value' parameter")
        })?;
        self.transport
            .request("gpio_write", json!({ "pin": pin, "value": value }))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::DuplexStream;
    use tokio::time::{Duration, sleep, timeout};

    async fn read_frame(port: &mut DuplexStream) -> Vec<u8> {
        let mut frame = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            port.read_exact(&mut byte)
                .await
                .expect("device should receive a complete frame");
            if byte[0] == b'\n' {
                return frame;
            }
            frame.push(byte[0]);
        }
    }

    async fn read_request_id(port: &mut DuplexStream) -> String {
        let frame = read_frame(port).await;
        let request: Value =
            serde_json::from_slice(&frame).expect("host request should be valid JSON");
        request
            .get("id")
            .and_then(Value::as_str)
            .expect("host request should contain a string id")
            .to_owned()
    }

    async fn write_frame(port: &mut DuplexStream, frame: &[u8]) {
        port.write_all(frame)
            .await
            .expect("device should write response frame");
        port.write_all(b"\n")
            .await
            .expect("device should terminate response frame");
        port.flush()
            .await
            .expect("device should flush response frame");
    }

    async fn write_json_frame(port: &mut DuplexStream, frame: Value) {
        let frame = serde_json::to_vec(&frame).expect("response should serialize");
        write_frame(port, &frame).await;
    }

    #[tokio::test]
    async fn wrong_id_then_matching_response_succeeds() {
        let (mut host, mut device) = tokio::io::duplex(4096);

        let host_request = send_request(&mut host, "ping", json!({}));
        let device_response = async {
            let request_id = read_request_id(&mut device).await;
            write_json_frame(
                &mut device,
                json!({"id": "unexpected", "ok": true, "result": "stale"}),
            )
            .await;
            write_json_frame(
                &mut device,
                json!({"id": request_id, "ok": true, "result": "current"}),
            )
            .await;
        };

        let (response, ()) = tokio::join!(host_request, device_response);
        let response = response.expect("matching response should succeed");
        assert_eq!(response["result"], "current");
    }

    #[tokio::test]
    async fn malformed_frame_then_matching_response_succeeds() {
        let (mut host, mut device) = tokio::io::duplex(4096);

        let host_request = send_request(&mut host, "ping", json!({}));
        let device_response = async {
            let request_id = read_request_id(&mut device).await;
            write_frame(&mut device, b"not-json").await;
            write_json_frame(
                &mut device,
                json!({"id": request_id, "ok": true, "result": "current"}),
            )
            .await;
        };

        let (response, ()) = tokio::join!(host_request, device_response);
        let response = response.expect("matching response should succeed");
        assert_eq!(response["result"], "current");
    }

    #[tokio::test]
    async fn unsolicited_frame_flood_does_not_extend_deadline_and_stream_recovers() {
        let (mut host, mut device) = tokio::io::duplex(4096);

        let host_requests = async {
            let first = timeout(
                Duration::from_millis(200),
                send_request(&mut host, "first", json!({})),
            )
            .await;
            assert!(
                first.is_err(),
                "unsolicited frames must not extend the original request deadline"
            );

            let second = timeout(
                Duration::from_secs(2),
                send_request(&mut host, "second", json!({})),
            )
            .await
            .expect("subsequent request should finish")
            .expect("subsequent request should recover on the same stream");
            assert_eq!(second["result"], "recovered");
        };

        let device_responses = async {
            let _first_request_id = read_request_id(&mut device).await;
            for sequence in 0..5 {
                write_json_frame(
                    &mut device,
                    json!({
                        "id": format!("unsolicited-{sequence}"),
                        "ok": true,
                        "result": "stale"
                    }),
                )
                .await;
                sleep(Duration::from_millis(60)).await;
            }

            let second_request_id = read_request_id(&mut device).await;
            write_json_frame(
                &mut device,
                json!({"id": second_request_id, "ok": true, "result": "recovered"}),
            )
            .await;
        };

        tokio::join!(host_requests, device_responses);
    }

    #[tokio::test]
    async fn unsolicited_frame_skip_limit_is_bounded() {
        let (mut host, mut device) = tokio::io::duplex(4096);

        let host_request = send_request(&mut host, "ping", json!({}));
        let device_response = async {
            let _request_id = read_request_id(&mut device).await;
            for sequence in 0..=MAX_SKIPPED_RESPONSE_FRAMES {
                write_json_frame(
                    &mut device,
                    json!({"id": format!("unsolicited-{sequence}"), "ok": true}),
                )
                .await;
            }
        };

        let (response, ()) = tokio::join!(host_request, device_response);
        let error = response.expect_err("skip limit should reject an unsolicited-frame flood");
        assert!(
            error.to_string().contains("skip limit exceeded"),
            "unexpected error: {error:#}"
        );
    }
}
