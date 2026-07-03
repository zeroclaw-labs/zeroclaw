//! Stdio JSON protocol between the main binary and the `zeroclaw-plugin-host`
//! sidecar.
//!
//! The sidecar carries the wasmtime/Cranelift weight so the dist binary does
//! not. One request per line in, one response per line out; the sidecar is
//! spawned per call, mirroring the fresh-store-per-call model tool plugins
//! already use in-process, so there is no session state to manage.
//!
//! Sidecar resolution order:
//! 1. `ZEROCLAW_PLUGIN_HOST` env var (explicit path)
//! 2. `zeroclaw-plugin-host` next to the current executable
//! 3. `zeroclaw-plugin-host` on `PATH`

use crate::PluginPermission;
use crate::execution::PluginLimits;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Protocol version spoken by this build. The sidecar echoes its own version
/// in every response envelope; a mismatch is a hard error naming both sides.
pub const PROTOCOL_VERSION: u32 = 1;

/// Name of the sidecar binary (per-target suffix handled by the OS).
pub const HOST_BINARY: &str = "zeroclaw-plugin-host";

/// Environment variable overriding sidecar discovery with an explicit path.
pub const HOST_ENV: &str = "ZEROCLAW_PLUGIN_HOST";

/// A single operation forwarded to the sidecar.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    ToolMetadata {
        wasm_path: PathBuf,
        permissions: Vec<PluginPermission>,
        limits: PluginLimits,
    },
    ToolExecute {
        wasm_path: PathBuf,
        permissions: Vec<PluginPermission>,
        limits: PluginLimits,
        args: serde_json::Value,
        config: HashMap<String, String>,
    },
}

/// Response envelope emitted by the sidecar: exactly one of `result`/`error`.
#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub protocol_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `ToolResult` as it crosses the wire (mirror of `zeroclaw_api::tool::ToolResult`,
/// duplicated so the protocol shape is pinned independently of the API crate).
#[derive(Debug, Serialize, Deserialize)]
pub struct WireToolResult {
    pub success: bool,
    pub output: String,
    pub error: Option<String>,
}

/// Locate the sidecar binary. Env override, then sibling of the current exe,
/// then PATH.
pub fn resolve_host_binary() -> Result<PathBuf> {
    if let Ok(explicit) = std::env::var(HOST_ENV) {
        let p = PathBuf::from(explicit);
        anyhow::ensure!(
            p.is_file(),
            "{HOST_ENV} points at {} which does not exist",
            p.display()
        );
        return Ok(p);
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let sibling = dir.join(host_file_name());
        if sibling.is_file() {
            return Ok(sibling);
        }
    }
    which_on_path(host_file_name().as_ref()).context(
        "zeroclaw-plugin-host not found. Plugin execution in this build runs in a separate \
         sidecar process. Install it next to the zeroclaw binary (it ships as a release \
         asset), or set ZEROCLAW_PLUGIN_HOST to its path.",
    )
}

fn host_file_name() -> String {
    format!("{HOST_BINARY}{}", std::env::consts::EXE_SUFFIX)
}

fn which_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// Spawn the sidecar, send one request line, read one response line.
pub async fn call(request: &Request) -> Result<serde_json::Value> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let host = resolve_host_binary()?;
    let mut child = tokio::process::Command::new(&host)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .with_context(|| format!("failed to spawn {}", host.display()))?;

    let mut line = serde_json::to_string(request)?;
    line.push('\n');
    child
        .stdin
        .take()
        .context("sidecar stdin unavailable")?
        .write_all(line.as_bytes())
        .await
        .context("failed to write request to sidecar")?;

    let stdout = child.stdout.take().context("sidecar stdout unavailable")?;
    let mut reply = String::new();
    BufReader::new(stdout)
        .read_line(&mut reply)
        .await
        .context("failed to read response from sidecar")?;

    let status = child.wait().await.context("sidecar did not exit")?;
    anyhow::ensure!(
        !reply.trim().is_empty(),
        "sidecar exited ({status}) without a response"
    );

    let response: Response =
        serde_json::from_str(reply.trim()).context("sidecar response is not valid JSON")?;
    anyhow::ensure!(
        response.protocol_version == PROTOCOL_VERSION,
        "plugin host protocol mismatch: main binary speaks v{PROTOCOL_VERSION}, sidecar speaks \
         v{}; update the older side",
        response.protocol_version
    );
    if let Some(err) = response.error {
        anyhow::bail!("plugin host error: {err}");
    }
    response
        .result
        .context("sidecar response carried neither result nor error")
}

/// Handle one already-parsed request. Only meaningful inside the sidecar
/// binary, which builds this crate WITH `plugins-wasmtime`.
#[cfg(feature = "plugins-wasmtime")]
pub async fn handle(request: Request) -> Response {
    let outcome = match request {
        Request::ToolMetadata {
            wasm_path,
            permissions,
            limits,
        } => crate::execution::tool_metadata(&wasm_path, &permissions, limits)
            .await
            .and_then(|meta| Ok(serde_json::to_value(meta)?)),
        Request::ToolExecute {
            wasm_path,
            permissions,
            limits,
            args,
            config,
        } => crate::execution::tool_execute(&wasm_path, &permissions, limits, args, &config)
            .await
            .and_then(|r| Ok(serde_json::to_value(r)?)),
    };
    match outcome {
        Ok(result) => Response {
            protocol_version: PROTOCOL_VERSION,
            result: Some(result),
            error: None,
        },
        Err(e) => Response {
            protocol_version: PROTOCOL_VERSION,
            result: None,
            error: Some(format!("{e:#}")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trips_through_json() {
        let req = Request::ToolExecute {
            wasm_path: PathBuf::from("/p/t.wasm"),
            permissions: vec![PluginPermission::ConfigRead],
            limits: PluginLimits {
                call_fuel: 1,
                max_memory_bytes: 2,
                max_table_elements: 3,
                max_instances: 4,
            },
            args: serde_json::json!({"input": "x"}),
            config: HashMap::from([("k".into(), "v".into())]),
        };
        let s = serde_json::to_string(&req).unwrap();
        let back: Request = serde_json::from_str(&s).unwrap();
        match back {
            Request::ToolExecute { wasm_path, .. } => {
                assert_eq!(wasm_path, PathBuf::from("/p/t.wasm"));
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn version_mismatch_is_detected() {
        let raw = r#"{"protocol_version": 999, "result": {}}"#;
        let r: Response = serde_json::from_str(raw).unwrap();
        assert_ne!(r.protocol_version, PROTOCOL_VERSION);
    }

    #[test]
    fn env_override_missing_file_errors() {
        // SAFETY: test-local env mutation, removed before return.
        unsafe { std::env::set_var(HOST_ENV, "/nonexistent/zeroclaw-plugin-host") };
        let err = resolve_host_binary().unwrap_err().to_string();
        unsafe { std::env::remove_var(HOST_ENV) };
        assert!(err.contains("does not exist"), "{err}");
    }
}
