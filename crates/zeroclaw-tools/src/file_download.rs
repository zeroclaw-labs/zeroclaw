use crate::helpers::domain_guard;
use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::json;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult, with_ephemeral_workspace_warning};
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::schema::FileDownloadConfig;
const RESPONSE_BODY_LIMIT_BYTES: usize = 4 * 1024;
const TOOL_DESCRIPTION_KEY: &str = "tool-file-download";
static TOOL_DESCRIPTION: OnceLock<String> = OnceLock::new();

// Set to `true` at the top of [`resolve_endpoint_ips`] so the ordering
// tests can independently prove the resolver was never entered. Thread-local
// storage means concurrent tests on other threads cannot interfere — each
// thread's flag is private.
#[cfg(test)]
std::thread_local! {
    static DNS_ENTERED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub struct FileDownloadTool {
    security: Arc<SecurityPolicy>,
    config: FileDownloadConfig,
    /// Whether the downloaded file persists on the host filesystem. `false` on
    /// an ephemeral runtime (Docker tmpfs / no volume mount), where the file is
    /// written inside the container but invisible on the host and discarded at
    /// session end. When `false`, a successful download carries a loud
    /// ephemeral-workspace warning. Mirrors
    /// [`super::file_write::FileWriteTool`].
    persistent_writes: bool,
}

impl FileDownloadTool {
    pub fn new(security: Arc<SecurityPolicy>, config: FileDownloadConfig) -> Self {
        Self::new_with_persistence(security, config, true)
    }

    /// Construct with an explicit persistence flag derived from the active
    /// runtime adapter's `has_filesystem_access()`. Mirrors
    /// [`super::file_write::FileWriteTool::new_with_persistence`].
    ///
    /// `allowed_private_hosts` lives on `config` and is normalized on demand
    /// by [`normalize_allowed_private_hosts`] per dispatch — the canonical
    /// state is the config field.
    pub fn new_with_persistence(
        security: Arc<SecurityPolicy>,
        config: FileDownloadConfig,
        persistent_writes: bool,
    ) -> Self {
        Self {
            security,
            config,
            persistent_writes,
        }
    }

    /// Gate the configured download URL against the SSRF policy. The endpoint
    /// URL is operator-configured, but a typo or copy-paste (e.g.
    /// `http://127.0.0.1`, `http://169.254.169.254/...`) must surface as a
    /// clear rejection before any network call. Returns the validated
    /// canonical host + its resolved `SocketAddr` set so the caller can
    /// bind them via `resolve_to_addrs`, closing the TOCTOU window
    /// between validation and connect.
    ///
    /// Thin dispatch over three module-level helpers:
    ///
    /// - [`parse_endpoint_url`] — pure URL parsing + canonical-host extract.
    /// - [`resolve_endpoint_ips`] — DNS resolution (short-circuits on IP literals).
    /// - [`ssrf_check_endpoint`] — applies the private-host / metadata policy
    ///   and emits the operator-audit WARN/INFO log signals.
    ///
    /// Mirrors the `ValidatedHttpRequestTarget` pattern from
    /// `http_request.rs:172-191` / `:363`.
    async fn validate_endpoint_host(
        &self,
        raw_url: &str,
    ) -> Result<(String, Vec<std::net::SocketAddr>), String> {
        let (host, port) = parse_endpoint_url(raw_url)?;
        let resolved_addrs = resolve_endpoint_ips(&host, port).await?;
        let allowed = normalize_allowed_private_hosts(&self.config.allowed_private_hosts);
        ssrf_check_endpoint(&host, &resolved_addrs, &allowed)?;
        Ok((host, resolved_addrs))
    }

    /// Stream a response body into `temp_path`, treating `max_bytes` as a hard
    /// ceiling so an unbounded or oversized body never fully buffers in memory.
    /// Returns the number of bytes written, or an error message. The caller is
    /// responsible for removing `temp_path` on any error.
    async fn stream_to_temp(
        response: reqwest::Response,
        temp_path: &Path,
        max_bytes: u64,
    ) -> Result<u64, String> {
        let mut file = tokio::fs::File::create(temp_path).await.map_err(|e| {
            tool_msg_with_args(
                "tool-file-download-error-temp-create",
                &[("err", &e.to_string())],
            )
        })?;

        let mut stream = response.bytes_stream();
        let mut written: u64 = 0;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| {
                tool_msg_with_args(
                    "tool-file-download-error-read-body",
                    &[("err", &e.to_string())],
                )
            })?;
            written = written.saturating_add(chunk.len() as u64);
            if written > max_bytes {
                let limit = max_bytes.to_string();
                return Err(tool_msg_with_args(
                    "tool-file-download-error-too-large-stream",
                    &[("limit", &limit)],
                ));
            }
            file.write_all(&chunk).await.map_err(|e| {
                tool_msg_with_args(
                    "tool-file-download-error-write-body",
                    &[("err", &e.to_string())],
                )
            })?;
        }

        file.flush().await.map_err(|e| {
            tool_msg_with_args("tool-file-download-error-flush", &[("err", &e.to_string())])
        })?;
        Ok(written)
    }
}

/// Look up a required tool string from the Fluent catalogue. Thin wrapper
/// around [`crate::i18n::get_required_tool_string`] kept as a module-level
/// free function so the URL-resolution seam (`parse_endpoint_url` /
/// `resolve_endpoint_ips` / `ssrf_check_endpoint`) and the `Tool` impl both
/// call into the same lookup without reaching into the impl block.
fn tool_msg(key: &str) -> String {
    crate::i18n::get_required_tool_string(key)
}

/// Variant of [`tool_msg`] that interpolates named arguments into the
/// localized string. Mirrors [`crate::i18n::get_required_tool_string_with_args`].
fn tool_msg_with_args(key: &str, args: &[(&str, &str)]) -> String {
    crate::i18n::get_required_tool_string_with_args(key, args)
}

/// Extract the canonical host from an `http://` or `https://` URL. The
/// endpoint is parsed through `reqwest::Url` so alternate and percent-encoded
/// IPv4 representations are canonicalised the same way reqwest's transport will
/// (e.g. `http://2130706433/` → `127.0.0.1`). Userinfo, non-http(s) schemes,
/// IPv6 hosts, and empty hosts are all rejected.
fn extract_download_url_host(url: &str) -> anyhow::Result<String> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|e| anyhow::Error::msg(format!("Invalid download URL: {e}")))?;

    match parsed.scheme() {
        "http" | "https" => {}
        _ => anyhow::bail!("Only http:// and https:// URLs are allowed"),
    }

    if !parsed.username().is_empty() || parsed.password().is_some() {
        anyhow::bail!("URL userinfo is not allowed");
    }

    let host_str = parsed
        .host_str()
        .filter(|h| !h.is_empty())
        .ok_or_else(|| anyhow::Error::msg("URL must include a valid host"))?;

    // IPv6 hosts appear as e.g. "::1" in host_str(); reject them.
    if host_str.contains(':') {
        anyhow::bail!("IPv6 hosts are not supported in file_download endpoint URLs");
    }

    // Preserve trailing dot (FQDN root label) because reqwest's
    // `resolve_to_addrs` override lookup requires an exact hostname match.
    // While reqwest's transport treats "example.com." and "example.com" as
    // equivalent, the override keyed by the hostname will not bind if the
    // request hostname differs from the override key. Keeping the dot ensures
    // the validated address set binds to the exact transport-canonical form.
    Ok(host_str.to_ascii_lowercase())
}

/// Parse the configured `[file_download].url` into a canonical
/// `(host, port)` pair. Pure: no network I/O. Rejects empty URLs,
/// non-http(s) schemes, URLs without an explicit port, userinfo, and IPv6
/// hosts. The host is taken through [`extract_download_url_host`] so
/// alternate IPv4 / percent-encoded loopback forms canonicalise the same
/// way reqwest's transport will classify them.
fn parse_endpoint_url(raw_url: &str) -> Result<(String, u16), String> {
    let url = raw_url.trim();
    if url.is_empty() {
        return Err(tool_msg("tool-file-download-error-disabled"));
    }
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(tool_msg_with_args(
            "tool-file-download-error-bad-scheme",
            &[("url", url)],
        ));
    }
    let parsed = reqwest::Url::parse(url).map_err(|e| {
        tool_msg_with_args(
            "tool-file-download-error-invalid-url",
            &[("err", &e.to_string())],
        )
    })?;
    let port = parsed.port_or_known_default().ok_or_else(|| {
        tool_msg_with_args(
            "tool-file-download-error-invalid-url",
            &[("err", "URL must include a valid port")],
        )
    })?;
    let host = extract_download_url_host(url).map_err(|e| {
        tool_msg_with_args(
            "tool-file-download-error-invalid-url",
            &[("err", &e.to_string())],
        )
    })?;
    Ok((host, port))
}

/// DNS resolution for a single `(host, port)`. IP literals short-circuit
/// to a one-element vector (no I/O) — `ssrf_check_endpoint` then
/// classifies the literal directly. Hostnames go through
/// `tokio::net::lookup_host` and the address set is collected.
async fn resolve_endpoint_ips(host: &str, port: u16) -> Result<Vec<std::net::SocketAddr>, String> {
    #[cfg(test)]
    DNS_ENTERED.with(|c| c.set(true));
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return Ok(vec![std::net::SocketAddr::new(ip, port)]);
    }
    let addrs: Vec<std::net::SocketAddr> = match tokio::net::lookup_host((host, port)).await {
        Ok(s) => s.collect(),
        Err(e) => {
            return Err(tool_msg_with_args(
                "tool-file-download-error-invalid-url",
                &[("err", &format!("Failed to resolve host '{host}': {e}"))],
            ));
        }
    };
    if addrs.is_empty() {
        return Err(tool_msg_with_args(
            "tool-file-download-error-invalid-url",
            &[("err", &format!("Failed to resolve host '{host}'"))],
        ));
    }
    Ok(addrs)
}

/// Apply the shared SSRF policy over a `(host, resolved-IP set, allowlist)`
/// triple. `allowed_hosts` must already be normalized via
/// [`normalize_allowed_private_hosts`].
///
/// Mirrors `http_request::validate_resolved_ips_for_ssrf`
/// (`http_request.rs:707-717`):
///
/// - A hostname covered by `allowed_hosts` lifts the *non-global* check
///   but never lifts the metadata-IP check.
/// - If the hostname is not allowlisted and resolves to a public IP, it
///   passes through; if it resolves to a private / loopback / link-local
///   IP, it is rejected with `tool-file-download-error-private-host`.
/// - The structured WARN log event fires on every rejection path (both
///   resolved-IP and literal-host rejections) so the operator-audit
///   signal is always preserved.
fn ssrf_check_endpoint(
    host: &str,
    resolved_addrs: &[std::net::SocketAddr],
    allowed_hosts: &[String],
) -> Result<(), String> {
    let ips: Vec<std::net::IpAddr> = resolved_addrs.iter().map(|sa| sa.ip()).collect();
    let private_allowed = domain_guard::host_matches_allowlist(host, allowed_hosts);
    let policy_result = if private_allowed {
        domain_guard::validate_resolved_ips_exclude_metadata(host, &ips)
    } else {
        domain_guard::validate_resolved_ips_are_public(host, &ips)
    };

    if let Err(e) = policy_result {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "tool": "file_download",
                    "host": host,
                })),
            "file_download: rejected private/local endpoint host"
        );
        return Err(tool_msg_with_args(
            "tool-file-download-error-private-host",
            &[
                ("host", host),
                ("config_key", "file_download.allowed_private_hosts"),
                ("err", &e.to_string()),
            ],
        ));
    }

    if private_allowed {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({
                    "tool": "file_download",
                    "host": host,
                })),
            "file_download: allowing private host via allowed_private_hosts"
        );
    }

    Ok(())
}

/// Per-dispatch normalization of the canonical `config.allowed_private_hosts`
/// Vec. Returns the filtered list on `Ok`. On `Err`, emits a
/// once-per-process WARN (so spam doesn't flood the logs on every
/// dispatch) and falls back to an empty allowlist — the SSRF gate still
/// functions and any future config-layer regression surfaces in logs
/// instead of silently disabling the gate.
fn normalize_allowed_private_hosts(allowed: &[String]) -> Vec<String> {
    match domain_guard::normalize_allowed_domains(
        allowed.to_vec(),
        "file_download.allowed_private_hosts",
    ) {
        Ok(v) => v,
        Err(e) => {
            NORMALIZE_WARNING_EMITTED.get_or_init(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "file_download: failed to normalize allowed_private_hosts; using empty list"
                );
            });
            Vec::new()
        }
    }
}

/// Set by [`normalize_allowed_private_hosts`] the first time the
/// configured allowlist fails to normalize, so the WARN fires at most
/// once per process. Drops the per-dispatch noise that would otherwise
/// flood logs for a permanently-misconfigured entry.
static NORMALIZE_WARNING_EMITTED: OnceLock<()> = OnceLock::new();

/// Build the reqwest client used to fetch the configured endpoint. The
/// validated `(host, resolved_addrs)` pair from
/// [`FileDownloadTool::validate_endpoint_host`] is bound into the client
/// via `resolve_to_addrs`, so the connection cannot perform a second
/// unbound DNS lookup at connect time. Redirect-following is disabled
/// because the configured `[file_download].url` is operator-approved and
/// a 3xx must surface as a status, not silently rehome the request.
///
/// **Proxy disabled**: The runtime proxy is explicitly NOT applied here.
/// With a proxy enabled, reqwest connects to the proxy and sends
/// `CONNECT <host>:<port>` for HTTPS, allowing the proxy to resolve the
/// target independently. That would bypass the validated address set.
/// Failing closed on proxy support is the only SSRF-safe choice.
///
/// This helper is intentionally a free function (not an instance method)
/// so the wire-up contract can be unit-tested directly: callers can
/// supply a hand-crafted `(host, resolved_addrs)` and assert that a real
/// reqwest request lands on the validated address set rather than on a
/// second DNS lookup. See `execute_does_not_perform_unbound_dns_when_resolve_to_addrs_is_wired`.
async fn build_secure_download_client(
    host: &str,
    resolved_addrs: &[std::net::SocketAddr],
    timeout_secs: u64,
) -> Result<reqwest::Client, String> {
    // Fail closed: disable proxy to prevent target re-resolution at the
    // proxy side. The validated address set must be the only connection
    // target - a proxy could otherwise resolve the hostname to a private
    // address even though local DNS validated a public IP.
    let builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .resolve_to_addrs(host, resolved_addrs)
        .no_proxy();

    // Explicitly do NOT call `apply_runtime_proxy_to_builder` - that would
    // re-enable proxy and reopen the SSRF window via proxy-side DNS.

    builder.build().map_err(|e| {
        tool_msg_with_args(
            "tool-file-download-error-client-build",
            &[("err", &e.to_string())],
        )
    })
}

#[async_trait]
impl Tool for FileDownloadTool {
    fn name(&self) -> &str {
        "file_download"
    }

    fn description(&self) -> &str {
        TOOL_DESCRIPTION
            .get_or_init(|| crate::i18n::get_required_tool_string(TOOL_DESCRIPTION_KEY))
            .as_str()
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "document_id": {
                    "type": "string",
                    "description": tool_msg("tool-file-download-param-document-id")
                },
                "dest_path": {
                    "type": "string",
                    "description": tool_msg("tool-file-download-param-dest-path")
                }
            },
            "required": ["document_id", "dest_path"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let Some(url) = self
            .config
            .url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(tool_msg("tool-file-download-error-disabled")),
            });
        };

        // SSRF gate + DNS resolution are intentionally deferred until AFTER
        // the local authorization / input / destination checks below: a
        // read-only, rate-limited, missing-arg, or traversal-rejected call
        // must NOT reach the resolver.
        if !self.security.can_act() {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(tool_msg("tool-file-download-error-read-only")),
            });
        }

        if self.security.is_rate_limited() {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(tool_msg("tool-file-download-error-rate-limited-hour")),
            });
        }

        let document_id = args
            .get("document_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"param": "document_id"})),
                    "file_download: missing document_id parameter"
                );
                anyhow::Error::msg(tool_msg("tool-file-download-error-missing-document-id"))
            })?;

        let dest_path = args
            .get("dest_path")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"param": "dest_path"})),
                    "file_download: missing dest_path parameter"
                );
                anyhow::Error::msg(tool_msg("tool-file-download-error-missing-dest-path"))
            })?;

        // The downloaded bytes are attacker-influenceable, so the write target
        // must resolve inside the workspace allowlist before any network call.
        let full = self.security.resolve_tool_path(dest_path);

        let file_name = match full.file_name().and_then(|s| s.to_str()) {
            Some(name) if name != "." && name != ".." => name.to_string(),
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(tool_msg_with_args(
                        "tool-file-download-error-invalid-file-name",
                        &[("dest_path", dest_path)],
                    )),
                });
            }
        };

        let Some(parent) = full.parent() else {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(tool_msg_with_args(
                    "tool-file-download-error-no-parent",
                    &[("dest_path", dest_path)],
                )),
            });
        };

        // Canonicalize the parent (which must already exist) so a symlinked
        // parent cannot redirect the write outside the workspace. `full` itself
        // does not exist yet, so it is never canonicalized.
        let canonical_parent = match tokio::fs::canonicalize(parent).await {
            Ok(p) => p,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(tool_msg_with_args(
                        "tool-file-download-error-resolve-dir",
                        &[("dest_path", dest_path), ("err", &e.to_string())],
                    )),
                });
            }
        };

        if !self.security.is_resolved_path_allowed(&canonical_parent) {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(
                    self.security
                        .resolved_path_violation_message(&canonical_parent),
                ),
            });
        }

        let dest = canonical_parent.join(&file_name);
        if !self.security.is_resolved_path_allowed(&dest) {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(self.security.resolved_path_violation_message(&dest)),
            });
        }

        // SSRF gate: the configured URL must point at a non-private host
        // (or an explicitly allowlisted one) AND its resolved IPs must
        // satisfy the same policy. Catches typos / copy-paste mistakes
        // (e.g. `http://127.0.0.1` or `http://169.254.169.254/...`) at
        // dispatch time before any network call, and binds the validated
        // address set into the reqwest client so a second unbound DNS
        // lookup at connect time cannot bypass this gate. Runs AFTER
        // local authorization / arg / destination validation and BEFORE
        // the action-budget debit so a request that fails the SSRF gate
        // never burns budget.
        let (host, resolved_addrs) = match self.validate_endpoint_host(url).await {
            Ok(target) => target,
            Err(msg) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(msg),
                });
            }
        };

        // Debit the action budget only once the request is validated, mirroring
        // file_upload — right before the network call.
        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(tool_msg("tool-file-download-error-rate-limited-budget")),
            });
        }

        // Disable redirect-following: the configured `[file_download].url` is
        // the operator-approved endpoint, so a 3xx response from it must surface
        // as a non-success status rather than silently rehome the request.
        // Bind the validated address set into the reqwest client keyed by
        // the parsed hostname (NOT the full URL), so a second unbound DNS
        // lookup at connect time cannot bypass the SSRF gate.
        let client =
            match build_secure_download_client(&host, &resolved_addrs, self.config.timeout_secs)
                .await
            {
                Ok(c) => c,
                Err(msg) => {
                    return Ok(ToolResult {
                        success: false,
                        output: ToolOutput::default(),
                        error: Some(msg),
                    });
                }
            };

        let mut request = client.get(url).query(&[("document_id", document_id)]);
        for (k, v) in &self.config.headers {
            request = request.header(k.as_str(), v.as_str());
        }

        let response = match request.send().await {
            Ok(r) => r,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(tool_msg_with_args(
                        "tool-file-download-error-request",
                        &[("err", &e.to_string())],
                    )),
                });
            }
        };

        let status = response.status();

        if !status.is_success() {
            let raw_body = response.text().await.unwrap_or_default();
            let truncated = if raw_body.len() > RESPONSE_BODY_LIMIT_BYTES {
                // The body is attacker-influenceable, so split on a char boundary
                // to avoid panicking when the byte cutoff lands inside a
                // multi-byte UTF-8 sequence. floor_char_boundary is unstable, so
                // walk down at most three bytes — a UTF-8 code point is at most
                // four bytes wide, so a boundary is always within reach.
                let mut cut = RESPONSE_BODY_LIMIT_BYTES;
                while cut > 0 && !raw_body.is_char_boundary(cut) {
                    cut -= 1;
                }
                format!(
                    "{}... [truncated {} bytes]",
                    &raw_body[..cut],
                    raw_body.len() - cut
                )
            } else {
                raw_body
            };
            return Ok(ToolResult {
                success: false,
                output: truncated.into(),
                error: Some(tool_msg_with_args(
                    "tool-file-download-error-status",
                    &[("status", &status.to_string())],
                )),
            });
        }

        // Fast-reject when the endpoint advertises an oversized body, before
        // opening the destination file at all.
        if let Some(len) = response.content_length()
            && len > self.config.max_file_size_bytes
        {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(tool_msg_with_args(
                    "tool-file-download-error-too-large-reported",
                    &[
                        ("len", &len.to_string()),
                        ("limit", &self.config.max_file_size_bytes.to_string()),
                    ],
                )),
            });
        }

        // Stream into a temp file in the destination directory so a failed or
        // oversized transfer never leaves a partial artifact at `dest`; on
        // success the rename is atomic within the same directory.
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let temp_path = canonical_parent.join(format!(".{file_name}.part-{nanos}"));

        match Self::stream_to_temp(response, &temp_path, self.config.max_file_size_bytes).await {
            Ok(written) => match tokio::fs::rename(&temp_path, &dest).await {
                Ok(()) => {
                    let output = tool_msg_with_args(
                        "tool-file-download-success",
                        &[
                            ("written", &written.to_string()),
                            ("dest_path", dest_path),
                            ("status", &status.to_string()),
                        ],
                    );
                    // The download landed in an ephemeral workspace and will not
                    // reach the host — warn loudly rather than report a bare
                    // success (issue 4627).
                    let output = if self.persistent_writes {
                        output
                    } else {
                        with_ephemeral_workspace_warning(&output)
                    };
                    Ok(ToolResult {
                        success: true,
                        output: output.into(),
                        error: None,
                    })
                }
                Err(e) => {
                    let _ = tokio::fs::remove_file(&temp_path).await;
                    Ok(ToolResult {
                        success: false,
                        output: ToolOutput::default(),
                        error: Some(tool_msg_with_args(
                            "tool-file-download-error-move",
                            &[("err", &e.to_string())],
                        )),
                    })
                }
            },
            Err(msg) => {
                let _ = tokio::fs::remove_file(&temp_path).await;
                Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(msg),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use zeroclaw_config::autonomy::AutonomyLevel;

    fn test_security(workspace: PathBuf, level: AutonomyLevel) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: level,
            max_actions_per_hour: 100,
            workspace_dir: workspace,
            ..SecurityPolicy::default()
        })
    }

    fn cfg(url: Option<String>) -> FileDownloadConfig {
        FileDownloadConfig {
            url,
            ..FileDownloadConfig::default()
        }
    }

    /// Count files in `dir` whose name marks an in-progress download temp file.
    fn part_files(dir: &Path) -> Vec<PathBuf> {
        fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|n| n.contains(".part-"))
            })
            .collect()
    }

    #[test]
    fn tool_name_and_description() {
        let tmp = TempDir::new().unwrap();
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            cfg(Some("https://example.com/download".into())),
        );
        assert_eq!(tool.name(), "file_download");
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn schema_requires_document_id_and_dest_path() {
        let tmp = TempDir::new().unwrap();
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            cfg(Some("https://example.com/download".into())),
        );
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&serde_json::Value::String("document_id".into())));
        assert!(required.contains(&serde_json::Value::String("dest_path".into())));
        assert_eq!(
            schema["properties"]["document_id"]["description"],
            crate::i18n::get_required_tool_string("tool-file-download-param-document-id")
        );
    }

    #[tokio::test]
    async fn execute_fails_when_url_unset() {
        let tmp = TempDir::new().unwrap();
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            cfg(None),
        );

        let result = tool
            .execute(json!({ "document_id": "doc-1", "dest_path": "out.bin" }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("disabled"));
        assert!(!tmp.path().join("out.bin").exists());
    }

    #[tokio::test]
    async fn execute_blocks_readonly_autonomy() {
        let tmp = TempDir::new().unwrap();
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::ReadOnly),
            cfg(Some("https://example.com/download".into())),
        );

        let result = tool
            .execute(json!({ "document_id": "doc-1", "dest_path": "out.bin" }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("read-only"));
        assert!(!tmp.path().join("out.bin").exists());
    }

    #[tokio::test]
    async fn execute_errors_on_missing_arguments() {
        let tmp = TempDir::new().unwrap();
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            cfg(Some("https://example.com/download".into())),
        );

        assert!(
            tool.execute(json!({ "dest_path": "out.bin" }))
                .await
                .is_err()
        );
        assert!(
            tool.execute(json!({ "document_id": "doc-1" }))
                .await
                .is_err()
        );
        // Present-but-empty values are treated the same as missing.
        assert!(
            tool.execute(json!({ "document_id": "  ", "dest_path": "out.bin" }))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn execute_rejects_traversal_dest_path() {
        let tmp = TempDir::new().unwrap();
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            cfg(Some("https://example.com/download".into())),
        );

        // A dest_path that terminates in `..` has no concrete file name.
        let result = tool
            .execute(json!({ "document_id": "doc-1", "dest_path": "nested/.." }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("concrete file name"));
    }

    #[tokio::test]
    async fn execute_rejects_dest_outside_workspace() {
        let server = MockServer::start().await;
        let workspace = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();

        // The endpoint must never be contacted when the destination is rejected.
        Mock::given(method("GET"))
            .and(path("/download"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"should-not-arrive".to_vec()))
            .expect(0)
            .mount(&server)
            .await;

        let dest_abs = outside.path().join("escape.bin");
        let config = FileDownloadConfig {
            url: Some(format!("{}/download", server.uri())),
            allowed_private_hosts: vec!["127.0.0.1".into()],
            ..FileDownloadConfig::default()
        };
        let tool = FileDownloadTool::new(
            test_security(workspace.path().to_path_buf(), AutonomyLevel::Full),
            config,
        );

        let result = tool
            .execute(json!({
                "document_id": "doc-1",
                "dest_path": dest_abs.to_string_lossy(),
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(
            !dest_abs.exists(),
            "no file should be written outside workspace"
        );
    }

    #[tokio::test]
    async fn execute_downloads_file_to_dest() {
        let server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();
        let body = b"the-downloaded-bytes-\x00\x01\x02".to_vec();

        Mock::given(method("GET"))
            .and(path("/download"))
            .and(query_param("document_id", "doc-123"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
            .expect(1)
            .mount(&server)
            .await;

        let config = FileDownloadConfig {
            url: Some(format!("{}/download", server.uri())),
            allowed_private_hosts: vec!["127.0.0.1".into()],
            ..FileDownloadConfig::default()
        };
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            config,
        );

        let result = tool
            .execute(json!({ "document_id": "doc-123", "dest_path": "out.bin" }))
            .await
            .unwrap();

        assert!(result.success, "expected success, got {result:?}");
        let written = fs::read(tmp.path().join("out.bin")).unwrap();
        assert_eq!(written, body);
        assert!(result.output.contains("out.bin"));
        assert!(
            part_files(tmp.path()).is_empty(),
            "temp file must be cleaned up"
        );
    }

    /// On an ephemeral runtime a successful download lands in a workspace that
    /// won't persist; the output must carry the loud warning while preserving
    /// the original status, and the bytes must still be written (issue 4627).
    #[tokio::test]
    async fn execute_warns_on_ephemeral_workspace() {
        let server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();
        let body = b"downloaded-bytes".to_vec();

        Mock::given(method("GET"))
            .and(path("/download"))
            .and(query_param("document_id", "doc-eph"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
            .expect(1)
            .mount(&server)
            .await;

        let config = FileDownloadConfig {
            url: Some(format!("{}/download", server.uri())),
            allowed_private_hosts: vec!["127.0.0.1".into()],
            ..FileDownloadConfig::default()
        };
        let tool = FileDownloadTool::new_with_persistence(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            config,
            false,
        );

        let result = tool
            .execute(json!({ "document_id": "doc-eph", "dest_path": "out.bin" }))
            .await
            .unwrap();

        assert!(result.success, "expected success, got {result:?}");
        assert!(
            result.output.contains("EPHEMERAL WORKSPACE"),
            "ephemeral warning must be present, got: {}",
            result.output
        );
        assert!(result.output.contains("mount_workspace"));
        assert!(
            result.output.contains("out.bin"),
            "original download status must be preserved, got: {}",
            result.output
        );
        assert_eq!(fs::read(tmp.path().join("out.bin")).unwrap(), body);
    }

    #[tokio::test]
    async fn execute_sends_configured_bearer_header() {
        let server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();

        Mock::given(method("GET"))
            .and(path("/download"))
            .and(header("Authorization", "Bearer secret-token"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"ok".to_vec()))
            .expect(1)
            .mount(&server)
            .await;

        let mut headers = HashMap::new();
        headers.insert("Authorization".into(), "Bearer secret-token".into());
        let config = FileDownloadConfig {
            url: Some(format!("{}/download", server.uri())),
            headers,
            allowed_private_hosts: vec!["127.0.0.1".into()],
            ..FileDownloadConfig::default()
        };
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            config,
        );

        let result = tool
            .execute(json!({ "document_id": "doc-1", "dest_path": "out.bin" }))
            .await
            .unwrap();

        // The mock only matches when the Bearer header is present, so success
        // proves the configured header was attached to the request.
        assert!(result.success, "expected success, got {result:?}");
        assert_eq!(fs::read(tmp.path().join("out.bin")).unwrap(), b"ok");
    }

    #[tokio::test]
    async fn execute_reports_non_2xx_without_writing() {
        let server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();

        Mock::given(method("GET"))
            .and(path("/download"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not_found"))
            .expect(1)
            .mount(&server)
            .await;

        let config = FileDownloadConfig {
            url: Some(format!("{}/download", server.uri())),
            allowed_private_hosts: vec!["127.0.0.1".into()],
            ..FileDownloadConfig::default()
        };
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            config,
        );

        let result = tool
            .execute(json!({ "document_id": "missing", "dest_path": "out.bin" }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("404"));
        assert!(!tmp.path().join("out.bin").exists());
        assert!(part_files(tmp.path()).is_empty());
    }

    #[tokio::test]
    async fn execute_rejects_oversized_via_content_length() {
        let server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();

        // Body of 2048 bytes; wiremock serves it with a Content-Length header.
        Mock::given(method("GET"))
            .and(path("/download"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0u8; 2048]))
            .mount(&server)
            .await;

        let mut config = FileDownloadConfig {
            url: Some(format!("{}/download", server.uri())),
            allowed_private_hosts: vec!["127.0.0.1".into()],
            ..FileDownloadConfig::default()
        };
        config.max_file_size_bytes = 1024;
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            config,
        );

        let result = tool
            .execute(json!({ "document_id": "big", "dest_path": "out.bin" }))
            .await
            .unwrap();

        assert!(!result.success);
        // The advertised Content-Length must trigger the fast pre-stream reject.
        assert!(
            result.error.unwrap().contains("endpoint reports"),
            "expected the Content-Length fast-reject path"
        );
        assert!(!tmp.path().join("out.bin").exists());
        assert!(
            part_files(tmp.path()).is_empty(),
            "no partial file may remain"
        );
    }

    #[tokio::test]
    async fn execute_rejects_oversized_while_streaming_without_content_length() {
        let server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();

        // `Transfer-Encoding: chunked` makes the served response omit
        // Content-Length, so the size ceiling can only be enforced by the
        // streaming accumulator rather than the fast Content-Length check.
        Mock::given(method("GET"))
            .and(path("/download"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Transfer-Encoding", "chunked")
                    .set_body_bytes(vec![0u8; 4096]),
            )
            .mount(&server)
            .await;

        let mut config = FileDownloadConfig {
            url: Some(format!("{}/download", server.uri())),
            allowed_private_hosts: vec!["127.0.0.1".into()],
            ..FileDownloadConfig::default()
        };
        config.max_file_size_bytes = 1024;
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            config,
        );

        let result = tool
            .execute(json!({ "document_id": "big", "dest_path": "out.bin" }))
            .await
            .unwrap();

        assert!(!result.success);
        // With no Content-Length, only the streaming accumulator can catch the
        // overage, which emits this distinct message.
        assert!(
            result.error.unwrap().contains("exceeded limit"),
            "expected the streaming size-cap path"
        );
        assert!(!tmp.path().join("out.bin").exists());
        assert!(
            part_files(tmp.path()).is_empty(),
            "no partial file may remain"
        );
    }

    #[tokio::test]
    async fn execute_does_not_follow_redirects_from_configured_endpoint() {
        let server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();

        // The configured endpoint returns a 302 pointing at a sibling path.
        // With redirects disabled, the tool must surface the 302 itself as a
        // non-success status and must never contact the redirect target.
        Mock::given(method("GET"))
            .and(path("/download"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", format!("{}/elsewhere", server.uri())),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/elsewhere"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"redirected-bytes".to_vec()))
            .expect(0)
            .mount(&server)
            .await;

        let config = FileDownloadConfig {
            url: Some(format!("{}/download", server.uri())),
            allowed_private_hosts: vec!["127.0.0.1".into()],
            ..FileDownloadConfig::default()
        };
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            config,
        );

        let result = tool
            .execute(json!({ "document_id": "doc-1", "dest_path": "out.bin" }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(
            result.error.as_deref().unwrap_or("").contains("302"),
            "expected the 302 status to surface; got {result:?}"
        );
        assert!(
            !tmp.path().join("out.bin").exists(),
            "no file may be written when the configured endpoint returns 3xx"
        );
        assert!(
            part_files(tmp.path()).is_empty(),
            "no partial file may remain after a 3xx response"
        );
    }

    #[tokio::test]
    async fn execute_truncates_non_ascii_error_body_safely() {
        let server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();

        // Build a non-2xx body that is longer than RESPONSE_BODY_LIMIT_BYTES
        // (4096) and where the byte at offset 4096 lands inside a multi-byte
        // UTF-8 sequence. Pre-truncation pad — 4094 ASCII bytes — places the
        // first byte of the next 3-byte character ("界") at offset 4094, so
        // offset 4096 lies in the middle of that code point.
        let mut body = "x".repeat(4094);
        body.push_str("世界世界世界世界世界世界");
        assert!(!body.is_char_boundary(4096));

        Mock::given(method("GET"))
            .and(path("/download"))
            .respond_with(ResponseTemplate::new(500).set_body_string(body.clone()))
            .expect(1)
            .mount(&server)
            .await;

        let config = FileDownloadConfig {
            url: Some(format!("{}/download", server.uri())),
            allowed_private_hosts: vec!["127.0.0.1".into()],
            ..FileDownloadConfig::default()
        };
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            config,
        );

        // Must not panic when slicing the body at a non-char-boundary byte
        // index. The truncated output must still be valid UTF-8 and must
        // include the "[truncated ...]" marker.
        let result = tool
            .execute(json!({ "document_id": "doc-1", "dest_path": "out.bin" }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("500"));
        assert!(result.output.contains("[truncated"));
        assert!(
            result.output.len() < body.len(),
            "expected the body to be shortened"
        );
        assert!(!tmp.path().join("out.bin").exists());
    }

    // ── SSRF gate tests ────────────────────────────────────────────────
    //
    // The configured `[file_download].url` is operator-only, but a typo or
    // copy-paste (e.g. `http://127.0.0.1`, `http://169.254.169.254/...`,
    // `http://10.0.0.5/...`) must surface as a clear rejection before any
    // network call. Redirects are already disabled by the production code,
    // so the gate only needs to inspect the initial URL host.

    #[tokio::test]
    async fn execute_rejects_loopback_endpoint_without_opt_in() {
        // No `allowed_private_hosts` and no mock — the rejection must happen
        // before any HTTP call. The endpoint is operator-configured here to a
        // loopback URL; the only way that URL is contacted is if the gate
        // fails, so the test is over-determined (no real server is bound).
        let tmp = TempDir::new().unwrap();
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            cfg(Some("http://127.0.0.1:9999/download".into())),
        );

        let result = tool
            .execute(json!({ "document_id": "doc-1", "dest_path": "out.bin" }))
            .await
            .unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(
            err.contains("loopback") || err.contains("private"),
            "SSRF rejection should mention private/loopback; got: {err}"
        );
        assert!(err.contains("allowed_private_hosts"));
        assert!(!tmp.path().join("out.bin").exists());
    }

    #[tokio::test]
    async fn execute_rejects_metadata_endpoint_without_opt_in() {
        // AWS / GCP / Azure instance metadata services — the canonical
        // SSRF target. `169.254.169.254` is a link-local address; without
        // opt-in the gate must reject it.
        let tmp = TempDir::new().unwrap();
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            cfg(Some("http://169.254.169.254/latest/meta-data/".into())),
        );

        let result = tool
            .execute(json!({ "document_id": "doc-1", "dest_path": "out.bin" }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("private"));
    }

    #[tokio::test]
    async fn execute_rejects_rfc1918_endpoint_without_opt_in() {
        // 10.0.0.0/8 private range. No mock — the rejection is a string
        // comparison and must happen before any TCP connect.
        let tmp = TempDir::new().unwrap();
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            cfg(Some("http://10.0.0.5/internal/file".into())),
        );

        let result = tool
            .execute(json!({ "document_id": "doc-1", "dest_path": "out.bin" }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("private"));
    }

    #[tokio::test]
    async fn execute_rejects_localhost_name_without_opt_in() {
        let tmp = TempDir::new().unwrap();
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            cfg(Some("http://localhost:8080/file".into())),
        );

        let result = tool
            .execute(json!({ "document_id": "doc-1", "dest_path": "out.bin" }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("private"));
    }

    #[tokio::test]
    async fn execute_rejects_userinfo_in_endpoint_url() {
        // `user@host` form is a separate SSRF vector (userinfo can sneak
        // through naive host parsers). The extractor rejects it before the
        // private-host check.
        let tmp = TempDir::new().unwrap();
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            cfg(Some("http://attacker@example.com/file".into())),
        );

        let result = tool
            .execute(json!({ "document_id": "doc-1", "dest_path": "out.bin" }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("userinfo"));
    }

    #[tokio::test]
    async fn execute_allows_loopback_endpoint_with_explicit_opt_in() {
        // The legitimate internal-document-service case: operator opts the
        // loopback IP into `allowed_private_hosts` and the gate lets it
        // through. The endpoint still has to actually serve the file —
        // we wiremock it and verify the success path.
        let server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();
        let body = b"internal-bytes".to_vec();

        Mock::given(method("GET"))
            .and(path("/download"))
            .and(query_param("document_id", "doc-int"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
            .expect(1)
            .mount(&server)
            .await;

        // `server.uri()` is `http://127.0.0.1:port`; allowlist covers it.
        let config = FileDownloadConfig {
            url: Some(format!("{}/download", server.uri())),
            allowed_private_hosts: vec!["127.0.0.1".into()],
            ..FileDownloadConfig::default()
        };
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            config,
        );

        let result = tool
            .execute(json!({ "document_id": "doc-int", "dest_path": "out.bin" }))
            .await
            .unwrap();
        assert!(result.success, "expected success, got {result:?}");
        assert_eq!(fs::read(tmp.path().join("out.bin")).unwrap(), body);
    }

    #[tokio::test]
    async fn validate_endpoint_host_wildcard_lifts_literal_private_host_block() {
        // Wildcard semantics: `"*"` in `allowed_private_hosts` lifts the
        // literal private-host block for the host classifier — but the
        // classifier is *only* the host classifier. The wildcard does not
        // widen the tool to non-private hosts, it does not bypass
        // redirect validation (redirects are still disabled), and it does
        // not turn the gate into a blanket bypass of classification.
        //
        // This test pins that contract by calling `validate_endpoint_host`
        // directly (no network I/O, no metadata-service request) for both
        // sides of the contract:
        //
        // - With `"*"` in the allowlist, a non-metadata private IP
        //   (10.0.0.1) is admitted at the gate (returns `Ok`); a future
        //   refactor that re-tightens the wildcard back to a literal-only
        //   check would surface as `Err` here.
        // - With `"*"` removed, the same URL is rejected with the
        //   `private-host` error so the wildcard is the *only* reason
        //   the URL would have been admitted.
        let tmp = TempDir::new().unwrap();

        // Side 1: wildcard set → gate admits a non-metadata private IP.
        let tool_with_wildcard = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            FileDownloadConfig {
                url: Some("http://10.0.0.1/test.bin".into()),
                allowed_private_hosts: vec!["*".into()],
                ..FileDownloadConfig::default()
            },
        );
        let (admitted_host, _) = tool_with_wildcard
            .validate_endpoint_host("http://10.0.0.1/test.bin")
            .await
            .expect("wildcard must lift the literal private-host block for a non-metadata IP");
        assert_eq!(admitted_host, "10.0.0.1");

        // Side 2: no wildcard → same host is rejected (proves the
        // wildcard is the only reason the URL is admitted above).
        let tool_without_wildcard = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            FileDownloadConfig {
                url: Some("http://10.0.0.1/test.bin".into()),
                allowed_private_hosts: Vec::new(),
                ..FileDownloadConfig::default()
            },
        );
        let rejected = tool_without_wildcard
            .validate_endpoint_host("http://10.0.0.1/test.bin")
            .await
            .expect_err("without wildcard the private host must be rejected");
        assert!(
            rejected.contains("10.0.0.1"),
            "expected the SSRF rejection string, got: {rejected}"
        );
    }

    /// Cloud metadata IPs (e.g. 169.254.169.254) are rejected even with
    /// the wildcard opt-in, because `validate_resolved_ips_exclude_metadata`
    /// applies unconditionally. This pins the metadata-IP carve-out from
    /// the shared SSRF policy.
    #[tokio::test]
    async fn validate_endpoint_host_wildcard_does_not_lift_metadata_block() {
        let tmp = TempDir::new().unwrap();
        let tool_with_wildcard = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            FileDownloadConfig {
                url: Some("http://169.254.169.254/latest/meta-data/".into()),
                allowed_private_hosts: vec!["*".into()],
                ..FileDownloadConfig::default()
            },
        );
        let rejected = tool_with_wildcard
            .validate_endpoint_host("http://169.254.169.254/latest/meta-data/")
            .await
            .expect_err("wildcard must NOT lift the metadata-IP block");
        assert!(
            rejected.contains("169.254.169.254") || rejected.contains("metadata"),
            "expected the metadata rejection string, got: {rejected}"
        );
    }

    #[tokio::test]
    async fn execute_rejects_non_http_scheme() {
        // `file://` / `gopher://` / etc. must surface as a clear scheme
        // error before any I/O. The endpoint is operator-configured, but a
        // hand-edited TOML can still smuggle a non-HTTP scheme.
        let tmp = TempDir::new().unwrap();
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            cfg(Some("file:///etc/passwd".into())),
        );

        let result = tool
            .execute(json!({ "document_id": "doc-1", "dest_path": "out.bin" }))
            .await
            .unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(
            err.contains("http://") || err.contains("scheme"),
            "got: {err}"
        );
    }

    #[test]
    fn extract_download_url_host_handles_canonical_forms() {
        // Pins the helper that the SSRF gate sits on top of. The canonical host
        // is obtained through reqwest::Url so it matches what the transport will
        // actually contact.
        assert_eq!(
            extract_download_url_host("https://Example.com:8443/path").unwrap(),
            "example.com"
        );
        assert_eq!(
            extract_download_url_host("http://10.0.0.5/").unwrap(),
            "10.0.0.5"
        );
        assert_eq!(
            extract_download_url_host("https://example.com").unwrap(),
            "example.com"
        );
        // userinfo rejected.
        assert!(extract_download_url_host("https://user@example.com").is_err());
        assert!(extract_download_url_host("https://user:pass@example.com").is_err());
        // IPv6 unsupported (file_download doesn't speak v6).
        assert!(extract_download_url_host("https://[::1]/p").is_err());
        // Wrong scheme.
        assert!(extract_download_url_host("ftp://example.com/").is_err());
        // Garbage URL — the parser rejects non-URL input.
        assert!(extract_download_url_host("not-a-url").is_err());
    }

    /// Alternate and percent-encoded IPv4 loopback forms must classify as
    /// `127.0.0.1` — the same canonical host that reqwest's transport contacts.
    /// This pins the SSRF-bypass fix: the gate no longer does manual string
    /// splitting that sees a bare integer and lets it through as non-private.
    #[test]
    fn extract_download_url_host_canonicalises_alternate_ipv4_loopback() {
        // Decimal IPv4: http://2130706433/ → 127.0.0.1
        assert_eq!(
            extract_download_url_host("http://2130706433/path").unwrap(),
            "127.0.0.1"
        );
        // Hex IPv4: http://0x7f000001/ → 127.0.0.1
        assert_eq!(
            extract_download_url_host("http://0x7f000001/path").unwrap(),
            "127.0.0.1"
        );
        // Octal IPv4: http://0177.0.0.1/ → 127.0.0.1
        assert_eq!(
            extract_download_url_host("http://0177.0.0.1/path").unwrap(),
            "127.0.0.1"
        );
        // Dotted-quad with leading zeros (some parsers normalise these).
        assert_eq!(
            extract_download_url_host("http://127.0.0.1/path").unwrap(),
            "127.0.0.1"
        );
    }

    /// Percent-encoded loopback host: the URL parser percent-decodes the
    /// authority, so a percent-encoded `127.0.0.1` becomes canonical
    /// `127.0.0.1` and is blockable by the private-host check. This test
    /// pins that the gate sees the canonical form rather than the encoded
    /// wrapper.
    #[test]
    fn extract_download_url_host_canonicalises_percent_encoded_loopback() {
        let host = extract_download_url_host("http://%31%32%37%2e%30%2e%30%2e%31/").unwrap();
        assert_eq!(host, "127.0.0.1");
    }

    #[tokio::test]
    async fn validate_endpoint_host_surfaces_loopback_audit_signal() {
        // The rejection path emits a structured audit log event. We don't
        // capture logs here — this test just pins the gating decision so
        // future refactors can't silently drop the SSRF check.
        let tmp = TempDir::new().unwrap();
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            cfg(Some("http://127.0.0.1:9000/".into())),
        );
        let result = tool.validate_endpoint_host("http://127.0.0.1:9000/").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("private"));
    }

    // ── Transport-level regression tests ───────────────────────────────
    //
    // - Transport tests drive a real hostname through the full gate and
    //   prove the reqwest client connects only to the validated
    //   address set.
    // - DNS resolution runs AFTER local authorization / arg / destination
    //   validation, so read-only / missing-arg / traversal-rejected calls
    //   never reach the resolver. Each ordering test resets the
    //   `DNS_CALLED` flag and asserts the resolver was never entered.

    /// Full round-trip with a real hostname (`localhost`) allowlisted.
    /// The wiremock is mounted on `127.0.0.1:<port>`; the configured URL
    /// uses `localhost:<port>`. With `localhost` in `allowed_private_hosts`,
    /// the SSRF gate admits the request and the wiremock receives exactly
    /// one GET — proving the private-host path is reachable when the
    /// operator has explicitly opted in. The counterpart test
    /// `execute_rejects_localhost_name_without_opt_in` proves the gate
    /// rejects the same hostname when the allowlist is empty.
    #[tokio::test]
    async fn execute_allows_private_hostname_via_local_mock_when_allowlisted() {
        let server = MockServer::start().await;
        let mock_port = server.address().port();
        let tmp = TempDir::new().unwrap();
        let body = b"private-hostname-bytes".to_vec();

        Mock::given(method("GET"))
            .and(path("/x"))
            .and(query_param("document_id", "doc-priv"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
            .expect(1)
            .mount(&server)
            .await;

        // Use `localhost` as the URL hostname (resolved by /etc/hosts to
        // 127.0.0.1) so this test exercises the real DNS path through the
        // SSRF gate, not an IP-literal shortcut. The wiremock listens on
        // 127.0.0.1:<mock_port>; the URL is `http://localhost:<mock_port>/x`.
        let config = FileDownloadConfig {
            url: Some(format!("http://localhost:{mock_port}/x")),
            allowed_private_hosts: vec!["localhost".into()],
            ..FileDownloadConfig::default()
        };
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            config,
        );

        let result = tool
            .execute(json!({ "document_id": "doc-priv", "dest_path": "out.bin" }))
            .await
            .unwrap();

        assert!(result.success, "expected success, got {result:?}");
        let on_disk = fs::read(tmp.path().join("out.bin")).unwrap();
        assert_eq!(on_disk, body);
        // Sanity: wiremock must have observed exactly the one GET.
        let received = server.received_requests().await.expect("infallible");
        assert_eq!(received.len(), 1);
    }

    /// Direct unit test on `ssrf_check_endpoint` over a hand-crafted IP
    /// set. Deterministic without controlling real DNS: the synthetic
    /// hostname `public-looking.example.com` is paired with both private
    /// and public IP sets so we can prove the rejection is private-driven,
    /// not hostname-driven, and that the allowlist correctly lifts the
    /// non-global check (but never the metadata carve-out — covered by
    /// `validate_endpoint_host_wildcard_does_not_lift_metadata_block`).
    #[test]
    fn ssrf_check_endpoint_rejects_hostname_resolving_to_private_ip_without_opt_in() {
        // Side A: public-looking hostname + private IP + no allowlist →
        // rejected. The user-facing message names the host (the IP is
        // captured in the structured `$err` arg but not interpolated in
        // the catalogue — the policy fires, but only `host` is shown to
        // the operator). The shape of the rejection — host-named,
        // private-driven — proves the policy fired correctly. The IP-
        // set classifier is the trigger here (10.0.0.5 is RFC1918, the
        // host classifier on `public-looking.example.com` returns false).
        let err = ssrf_check_endpoint(
            "public-looking.example.com",
            &[std::net::SocketAddr::from(([10, 0, 0, 5], 80))],
            &[],
        )
        .expect_err("private IP without opt-in must be rejected");
        assert!(
            err.contains("public-looking.example.com"),
            "rejection should name the host; got: {err}"
        );
        assert!(
            err.contains("private"),
            "rejection should mention private; got: {err}"
        );

        // Side B: same hostname + same private IP + allowlisted hostname
        // → admitted. Proves the rejection above is driven by the policy,
        // not by the synthetic hostname shape or the IP literal.
        ssrf_check_endpoint(
            "public-looking.example.com",
            &[std::net::SocketAddr::from(([10, 0, 0, 5], 80))],
            &["public-looking.example.com".into()],
        )
        .expect("allowlisted hostname with non-metadata private IP must pass");

        // Side C: same hostname + public IP → always admitted (proves
        // policy is private-driven, not hostname-driven).
        ssrf_check_endpoint(
            "public-looking.example.com",
            &[std::net::SocketAddr::from(([8, 8, 8, 8], 443))],
            &[],
        )
        .expect("public-IP hostname must pass without opt-in");
    }

    /// Wire-up contract for the `resolve_to_addrs` binding the production
    /// code applies via [`build_secure_download_client`]: a hostname whose
    /// real-DNS resolution would land on the wiremock must NOT reach the
    /// wiremock when the override IP points elsewhere. Detects regressions
    /// that drop or miskey the `resolve_to_addrs(host, addrs)` call.
    ///
    /// reqwest's `resolve_to_addrs(host, addrs)` overrides only the IP;
    /// the port always comes from the URL (reqwest 0.12 client-builder
    /// docs). So the binding's IP half is what this test pins:
    ///
    /// - `build_secure_download_client` wired to a bogus IP: reqwest
    ///   connects to the bogus IP + URL port → ECONNREFUSED → mock NOT hit.
    /// - regression drops `resolve_to_addrs`: reqwest does real DNS for
    ///   `localhost` (via /etc/hosts → 127.0.0.1) + URL port → mock
    ///   hit → `expect(0)` violated → test fails.
    ///
    /// `localhost` is used because it is covered by the test
    /// environment's `no_proxy`, so reqwest does not divert the
    /// request through the HTTP proxy (the proxy intercepts and
    /// returns 302 for unknown hostnames, masking the wire-up).
    #[tokio::test]
    async fn resolve_to_addrs_binds_resolved_addrs_not_real_dns() {
        let server = MockServer::start().await;
        let mock_port = server.address().port();

        Mock::given(method("GET"))
            .and(path("/probe"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"hit".to_vec()))
            .expect(0) // MUST not be hit when the override IP is bogus
            .mount(&server)
            .await;

        // Override pins "localhost" to a bogus IP (RFC 5737 documentation
        // range, unrouted). The URL port comes from MOCK_PORT via
        // reqwest's documented behavior; real DNS would point at
        // 127.0.0.1 (which IS the wiremock), so without the override the
        // request would hit the mock.
        let bogus_addrs = [std::net::SocketAddr::from(([192, 0, 2, 1], mock_port))];

        // Exercise the production helper directly. If
        // `build_secure_download_client` drops the `resolve_to_addrs` call,
        // real DNS for `localhost` lands on the wiremock and `expect(0)`
        // fails.
        let client = build_secure_download_client("localhost", &bogus_addrs, 30)
            .await
            .expect("client build must succeed");

        let url = format!("http://localhost:{mock_port}/probe");
        let result = client.get(&url).send().await;

        // Drop the result without inspecting it — the wiremock side is
        // the authoritative regression-detector: zero hits means
        // resolve_to_addrs was honored, one hit means it was dropped
        // and reqwest fell through to real DNS.
        let _ = result;
        let received = server.received_requests().await.expect("infallible");
        assert!(
            received.is_empty(),
            "wiremock must NOT be hit when resolve_to_addrs binds localhost to a bogus IP; \
             if it is, resolve_to_addrs was dropped or miskeyed; saw {} request(s)",
            received.len()
        );
    }

    /// DNS resolution must defer until after the can_act() check.
    /// Read-only mode must surface as a read-only error, NOT as a
    /// private-host error (which would prove the SSRF gate ran first).
    /// The thread-local `DNS_ENTERED` flag independently proves that
    /// [`resolve_endpoint_ips`] was never entered on this thread.
    #[tokio::test]
    async fn execute_defers_dns_until_after_readonly_check() {
        DNS_ENTERED.with(|c| c.set(false));
        let tmp = TempDir::new().unwrap();
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::ReadOnly),
            cfg(Some("http://127.0.0.1:1/x".into())),
        );

        let result = tool
            .execute(json!({ "document_id": "doc-1", "dest_path": "out.bin" }))
            .await
            .unwrap();

        assert!(!result.success);
        let err = result.error.unwrap().to_lowercase();
        assert!(
            err.contains("read-only") || err.contains("readonly"),
            "read-only check must fire before DNS; got: {err}"
        );
        // Must NOT be a private/loopback error — that would mean DNS ran first.
        assert!(
            !err.contains("private") && !err.contains("loopback"),
            "DNS check must come AFTER the read-only check; got: {err}"
        );
        DNS_ENTERED.with(|c| {
            assert!(
                !c.get(),
                "DNS resolver must not be invoked for a read-only call"
            )
        });
        assert!(!tmp.path().join("out.bin").exists());
    }

    /// DNS resolution must defer until after required-arg validation.
    /// A missing `dest_path` must surface as a missing-arg error, NOT as a
    /// private-host error. The thread-local `DNS_ENTERED` flag independently
    /// proves that [`resolve_endpoint_ips`] was never entered on this thread.
    #[tokio::test]
    async fn execute_defers_dns_until_after_missing_arg_check() {
        DNS_ENTERED.with(|c| c.set(false));
        let tmp = TempDir::new().unwrap();
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            cfg(Some("http://127.0.0.1:1/x".into())),
        );

        // The missing-arg path bubbles up as `anyhow::Err` with a
        // localized message — `execute()` returns `Err`, not `Ok` with
        // `error` set. That's the established shape (see
        // `execute_errors_on_missing_arguments`).
        let err = tool
            .execute(json!({ "document_id": "doc-1" }))
            .await
            .expect_err("missing dest_path must surface as Err");

        // Use the Display chain (`: #`) so the source text bubbles up.
        let msg = format!("{err:#}").to_lowercase();
        assert!(
            msg.contains("dest_path"),
            "missing-arg check must fire before DNS; got: {msg}"
        );
        assert!(
            !msg.contains("private") && !msg.contains("loopback"),
            "DNS check must come AFTER arg validation; got: {msg}"
        );
        DNS_ENTERED.with(|c| {
            assert!(
                !c.get(),
                "DNS resolver must not be invoked for a missing-arg call"
            )
        });
    }

    /// DNS resolution must defer until after destination validation. A
    /// dest_path that has no concrete file name must surface as a
    /// destination error, NOT as a private-host error. The thread-local
    /// `DNS_ENTERED` flag independently proves that [`resolve_endpoint_ips`]
    /// was never entered on this thread.
    #[tokio::test]
    async fn execute_defers_dns_until_after_destination_check() {
        DNS_ENTERED.with(|c| c.set(false));
        let tmp = TempDir::new().unwrap();
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            cfg(Some("http://127.0.0.1:1/x".into())),
        );

        // `nested/..` terminates in `..` → "no concrete file name"
        // (see `execute_rejects_traversal_dest_path`).
        let result = tool
            .execute(json!({
                "document_id": "doc-1",
                "dest_path": "nested/.."
            }))
            .await
            .unwrap();

        assert!(!result.success);
        let err = result.error.unwrap().to_lowercase();
        assert!(
            err.contains("file name") || err.contains("concrete") || err.contains("invalid"),
            "destination check must fire before DNS; got: {err}"
        );
        assert!(
            !err.contains("private") && !err.contains("loopback"),
            "DNS check must come AFTER destination validation; got: {err}"
        );
        DNS_ENTERED.with(|c| {
            assert!(
                !c.get(),
                "DNS resolver must not be invoked for a destination-rejected call"
            )
        });
    }
}
