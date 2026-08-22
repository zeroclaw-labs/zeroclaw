//! Unix-domain-socket client for the Android UI bridge.
//!
//! Wire contract: newline-delimited JSON over a filesystem-namespace Unix
//! socket owned by the Android APK (see
//! `docs/book/src/tools/android-bridge-protocol.md`). One request line, one
//! response line, then the client drops the connection — the server is
//! documented not to assume connection reuse.
//!
//! Kernel UID isolation is the only trust boundary; the protocol carries no
//! token or auth field by design, because the socket lives inside the app's
//! private files directory where no other app UID can open it.

use anyhow::Result;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use super::{tool_msg, tool_msg_with_args};

/// Read timeout for one bridge round-trip. The contract gives the server a
/// 10 s budget; the client allows 15 s so a server-side `timeout` error is
/// reported as such rather than being masked by a client-side cutoff.
const READ_TIMEOUT: Duration = Duration::from_secs(15);

/// Connect timeout. Connecting to a live Unix socket is essentially instant,
/// so a short bound keeps a wedged bridge from stalling the whole turn.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Maximum request line the server accepts (contract: 64 KiB).
const MAX_REQUEST_BYTES: usize = 64 * 1024;

/// Maximum response line the client will buffer (contract: 4 MiB).
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// Client for the Android APK's UI-control socket.
#[derive(Debug, Clone)]
pub struct AndroidBridgeClient {
    socket_path: PathBuf,
}

impl AndroidBridgeClient {
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
        }
    }

    #[must_use]
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Confirm which app is on screen, returning the foreground package name.
    pub async fn foreground_package(&self) -> Result<String> {
        let data = self.call("foreground", json!({})).await?;
        Ok(data
            .get("package")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string())
    }

    /// Fail unless `expected` is the app currently on screen.
    ///
    /// The screen is shared mutable state owned by the user and the system, not by the agent: a
    /// notification, a launcher gesture, or the agent's own stray tap can swap the foreground
    /// between two calls. Without this check a tool call aimed at one app silently lands in
    /// another, and the model has no way to notice.
    pub async fn assert_foreground(&self, expected: &str) -> Result<()> {
        let actual = self.foreground_package().await?;
        if actual == expected {
            return Ok(());
        }
        anyhow::bail!(tool_msg_with_args(
            "tool-android-error-wrong-foreground",
            &[("expected", expected), ("actual", &actual)],
        ))
    }

    /// Send one `op` with `args` and return the response `data` object.
    ///
    /// Every failure mode — missing socket, refused connection, timeout,
    /// malformed response, or a structured bridge error — surfaces as an
    /// `Err`. This path never panics: it is reached whenever the phone-side
    /// APK is not installed or its accessibility service is off, which is a
    /// routine condition rather than a bug.
    pub async fn call(&self, op: &str, args: Value) -> Result<Value> {
        let request = json!({ "op": op, "args": args });
        let mut line = serde_json::to_string(&request)?;
        if line.len() > MAX_REQUEST_BYTES {
            anyhow::bail!(tool_msg_with_args(
                "tool-android-error-request-too-large",
                &[
                    ("bytes", &line.len().to_string()),
                    ("max", &MAX_REQUEST_BYTES.to_string()),
                ],
            ));
        }
        line.push('\n');

        let stream =
            match tokio::time::timeout(CONNECT_TIMEOUT, UnixStream::connect(&self.socket_path))
                .await
            {
                Ok(Ok(stream)) => stream,
                Ok(Err(e)) => {
                    anyhow::bail!(tool_msg_with_args(
                        "tool-android-error-connect",
                        &[
                            ("path", &self.socket_path.display().to_string()),
                            ("error", &e.to_string()),
                        ],
                    ));
                }
                Err(_) => {
                    anyhow::bail!(tool_msg_with_args(
                        "tool-android-error-connect-timeout",
                        &[("path", &self.socket_path.display().to_string())],
                    ));
                }
            };

        let (read_half, mut write_half) = stream.into_split();
        write_half.write_all(line.as_bytes()).await?;
        write_half.flush().await?;

        // Cap the reader so a runaway server cannot exhaust memory: the
        // contract bounds a response line at 4 MiB.
        let mut reader = BufReader::new(read_half.take(MAX_RESPONSE_BYTES as u64));
        let mut buf = Vec::new();
        let read = tokio::time::timeout(READ_TIMEOUT, reader.read_until(b'\n', &mut buf)).await;
        let byte_count = match read {
            Ok(Ok(n)) => n,
            Ok(Err(e)) => {
                anyhow::bail!(tool_msg_with_args(
                    "tool-android-error-read",
                    &[("error", &e.to_string())],
                ));
            }
            Err(_) => {
                anyhow::bail!(tool_msg_with_args(
                    "tool-android-error-timeout",
                    &[("secs", &READ_TIMEOUT.as_secs().to_string())],
                ));
            }
        };

        if byte_count == 0 {
            anyhow::bail!(tool_msg("tool-android-error-empty-response"));
        }
        if byte_count >= MAX_RESPONSE_BYTES && !buf.ends_with(b"\n") {
            anyhow::bail!(tool_msg_with_args(
                "tool-android-error-response-too-large",
                &[("max", &MAX_RESPONSE_BYTES.to_string())],
            ));
        }

        parse_response(&buf)
    }
}

/// Parse one response line into its `data` payload, mapping a structured
/// `{ ok: false, error: { code, message } }` envelope onto an `Err`.
///
/// Split out from the socket path so the envelope handling is testable
/// without a live bridge.
pub(crate) fn parse_response(buf: &[u8]) -> Result<Value> {
    let response: Value = serde_json::from_slice(buf).map_err(|e| {
        anyhow::Error::msg(tool_msg_with_args(
            "tool-android-error-bad-response",
            &[("error", &e.to_string())],
        ))
    })?;

    if response.get("ok").and_then(Value::as_bool) == Some(true) {
        return Ok(response.get("data").cloned().unwrap_or(Value::Null));
    }

    let code = response
        .pointer("/error/code")
        .and_then(Value::as_str)
        .unwrap_or("internal");
    let message = response
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    Err(anyhow::Error::msg(tool_msg_with_args(
        "tool-android-error-bridge",
        &[("code", code), ("message", message)],
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_response_returns_data_on_ok() {
        let line = br#"{"ok":true,"data":{"launched":true}}"#;
        let data = parse_response(line).expect("ok envelope parses");
        assert_eq!(data, json!({ "launched": true }));
    }

    #[test]
    fn parse_response_defaults_missing_data_to_null() {
        let line = br#"{"ok":true}"#;
        let data = parse_response(line).expect("ok envelope without data parses");
        assert_eq!(data, Value::Null);
    }

    #[test]
    fn parse_response_maps_error_envelope_to_err() {
        let line = br#"{"ok":false,"error":{"code":"no_focus","message":"no input field"}}"#;
        let err = parse_response(line).expect_err("error envelope is an Err");
        let rendered = err.to_string();
        assert!(rendered.contains("no_focus"), "got: {rendered}");
        assert!(rendered.contains("no input field"), "got: {rendered}");
    }

    #[test]
    fn parse_response_rejects_malformed_json() {
        let err = parse_response(b"not json\n").expect_err("malformed line is an Err");
        assert!(!err.to_string().is_empty());
    }

    /// The bridge is absent on any host that is not the phone, so the
    /// unreachable-socket path must be a graceful error rather than a panic.
    #[tokio::test]
    async fn call_on_missing_socket_errors_without_panicking() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("definitely-absent.sock");
        let client = AndroidBridgeClient::new(&missing);

        let result = client.call("ping", json!({})).await;

        let err = result.expect_err("missing socket must be an error");
        let rendered = err.to_string();
        assert!(
            rendered.contains("definitely-absent.sock"),
            "error should name the socket path, got: {rendered}"
        );
    }
}
