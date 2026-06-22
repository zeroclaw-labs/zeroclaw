//! Per-execution store state and the host-side implementations of the WIT
//! `host` and `logging` imports. Ported from `ironclaw_wasm::store`, adapted to
//! zeroclaw's WIT (logging is a separate interface; HTTP/usage use crate-local
//! types).

use std::time::{Duration, Instant};

use wasmtime_wasi::{ResourceTable, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::bindings;
use crate::limiter::WasmResourceLimiter;
use crate::usage::ResourceUsage;
use crate::wit_config::{DEFAULT_HTTP_TIMEOUT_MS, MAX_LOG_MESSAGE_BYTES, MAX_LOGS_PER_EXECUTION};
use crate::wit_host::{WasmHttpRequest, WitToolHost};
use crate::wit_types::{WasmLogLevel, WasmLogRecord};

pub(crate) struct StoreData {
    host: WitToolHost,
    pub(crate) limiter: WasmResourceLimiter,
    wasi: WasiCtx,
    table: ResourceTable,
    pub(crate) usage: ResourceUsage,
    pub(crate) logs: Vec<WasmLogRecord>,
    deadline: Option<Instant>,
}

impl StoreData {
    pub(crate) fn new(host: WitToolHost, memory_limit: u64, timeout: Duration) -> Self {
        Self {
            host,
            limiter: WasmResourceLimiter::new(memory_limit),
            // SECURITY: the full WASI p2 surface is linked (real wit-bindgen
            // guests import it via std), but this WasiCtx is intentionally empty
            // — no preopened dirs, no inherited env, no network. That empty
            // context is the deny boundary for filesystem/sockets/env; do NOT
            // call `inherit_*`/`preopened_dir`/network builders here without a
            // capability gate, or plugins gain ambient access outside the
            // permission model.
            wasi: WasiCtxBuilder::new().build(),
            table: ResourceTable::new(),
            usage: ResourceUsage::default(),
            logs: Vec::new(),
            deadline: Instant::now().checked_add(timeout),
        }
    }

    pub(crate) fn deadline_exceeded(&self) -> bool {
        self.deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
    }

    fn deadline_error(&self) -> Option<String> {
        self.deadline_exceeded()
            .then(|| "WASM execution deadline exceeded during host import".to_string())
    }

    /// Cap the guest-requested HTTP timeout to the smaller of its own value (or
    /// the WIT default) and the time remaining before the execution deadline.
    fn remaining_timeout_ms(&self, requested_timeout_ms: Option<u32>) -> Option<u32> {
        let requested_timeout_ms = requested_timeout_ms.unwrap_or(DEFAULT_HTTP_TIMEOUT_MS);
        let deadline_timeout_ms = self.deadline.map(|deadline| {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let remaining_ms = remaining.as_millis();
            if remaining_ms == 0 {
                1
            } else {
                remaining_ms.min(u128::from(u32::MAX)) as u32
            }
        });

        Some(match deadline_timeout_ms {
            Some(deadline) => requested_timeout_ms.min(deadline),
            None => requested_timeout_ms,
        })
    }

    fn record_network_egress(&mut self, request_body_bytes: u64) {
        self.usage.network_egress_bytes = self
            .usage
            .network_egress_bytes
            .saturating_add(request_body_bytes);
    }
}

impl WasiView for StoreData {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

// The `types` interface only defines the `json-string` alias, but bindgen still
// generates an (empty) Host trait that the linker requires.
impl bindings::zeroclaw::plugin::types::Host for StoreData {}

impl bindings::zeroclaw::plugin::host::Host for StoreData {
    fn now_millis(&mut self) -> u64 {
        self.host.clock.now_millis()
    }

    fn workspace_read(&mut self, path: String) -> Option<String> {
        if self.deadline_exceeded() {
            return None;
        }
        let result = self.host.workspace.read(&path);
        if self.deadline_exceeded() {
            return None;
        }
        result
    }

    fn http_request(
        &mut self,
        method: String,
        url: String,
        headers_json: String,
        body: Option<Vec<u8>>,
        timeout_ms: Option<u32>,
    ) -> Result<bindings::zeroclaw::plugin::host::HttpResponse, String> {
        if let Some(error) = self.deadline_error() {
            return Err(error);
        }

        let request_body_bytes = body.as_ref().map(|body| body.len() as u64).unwrap_or(0);
        let response = self.host.http.request(WasmHttpRequest {
            method,
            url,
            headers_json,
            body,
            timeout_ms: self.remaining_timeout_ms(timeout_ms),
        });
        match response {
            Ok(response) => {
                self.record_network_egress(request_body_bytes);
                if let Some(error) = self.deadline_error() {
                    return Err(error);
                }
                Ok(bindings::zeroclaw::plugin::host::HttpResponse {
                    status: response.status,
                    headers_json: response.headers_json,
                    body: response.body,
                })
            }
            Err(error) => {
                if error.request_was_sent() {
                    self.record_network_egress(request_body_bytes);
                }
                Err(error.to_string())
            }
        }
    }

    fn tool_invoke(&mut self, alias: String, params_json: String) -> Result<String, String> {
        if let Some(error) = self.deadline_error() {
            return Err(error);
        }
        let result = self
            .host
            .tools
            .invoke(&alias, &params_json)
            .map_err(|error| error.to_string());
        if let Some(error) = self.deadline_error() {
            return Err(error);
        }
        result
    }

    fn secret_exists(&mut self, name: String) -> bool {
        if self.deadline_exceeded() {
            return false;
        }
        let exists = self.host.secrets.exists(&name);
        if self.deadline_exceeded() {
            return false;
        }
        exists
    }
}

impl bindings::zeroclaw::plugin::logging::Host for StoreData {
    fn log_record(
        &mut self,
        level: bindings::zeroclaw::plugin::logging::LogLevel,
        event: bindings::zeroclaw::plugin::logging::PluginEvent,
    ) {
        if self.logs.len() >= MAX_LOGS_PER_EXECUTION {
            return;
        }
        let level = match level {
            bindings::zeroclaw::plugin::logging::LogLevel::Trace => WasmLogLevel::Trace,
            bindings::zeroclaw::plugin::logging::LogLevel::Debug => WasmLogLevel::Debug,
            bindings::zeroclaw::plugin::logging::LogLevel::Info => WasmLogLevel::Info,
            bindings::zeroclaw::plugin::logging::LogLevel::Warn => WasmLogLevel::Warn,
            bindings::zeroclaw::plugin::logging::LogLevel::Error => WasmLogLevel::Error,
        };
        let message = truncate_log_message(event.message);
        self.logs.push(WasmLogRecord { level, message });
    }
}

fn truncate_log_message(message: String) -> String {
    if message.len() <= MAX_LOG_MESSAGE_BYTES {
        return message;
    }

    let mut end = MAX_LOG_MESSAGE_BYTES;
    while !message.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    message[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::{MAX_LOG_MESSAGE_BYTES, truncate_log_message};

    #[test]
    fn truncate_log_message_respects_utf8_boundaries() {
        let message = "é".repeat(MAX_LOG_MESSAGE_BYTES);
        let truncated = truncate_log_message(message);
        assert!(truncated.len() <= MAX_LOG_MESSAGE_BYTES);
        assert!(truncated.is_char_boundary(truncated.len()));
    }
}
