use crate::helpers::domain_guard;
use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::json;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult, with_ephemeral_workspace_warning};
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::schema::{FileDownloadConfig, ProxyConfig, ProxyScope};

const RESPONSE_BODY_LIMIT_BYTES: usize = 4 * 1024;
const TOOL_DESCRIPTION_KEY: &str = "tool-file-download";
static TOOL_DESCRIPTION: OnceLock<String> = OnceLock::new();
const FILE_DOWNLOAD_PROXY_PINNING_ERROR: &str = "file_download requires direct transport so validated DNS answers remain pinned; set proxy.scope = \"services\" and omit tool.file_download and tool.* from proxy.services, or disable the proxy; proxy.scope = \"environment\" is incompatible with pinned HTTP requests";

type ResolveResult = Result<Vec<SocketAddr>, String>;
type EndpointResolver =
    Arc<dyn Fn(String, u16) -> Pin<Box<dyn Future<Output = ResolveResult> + Send>> + Send + Sync>;

fn default_endpoint_resolver() -> EndpointResolver {
    Arc::new(
        |host: String, port: u16| -> Pin<Box<dyn Future<Output = ResolveResult> + Send>> {
            Box::pin(async move { resolve_endpoint_ips(&host, port).await })
        },
    )
}

#[derive(Clone, Default)]
pub struct FileDownloadSsrfPolicy {
    pub allowed_private_hosts: Vec<String>,
    pub nat64_prefixes: Vec<String>,
}

pub struct FileDownloadTool {
    security: Arc<SecurityPolicy>,
    config: FileDownloadConfig,
    policy_resolver: Arc<dyn Fn() -> FileDownloadSsrfPolicy + Send + Sync>,
    endpoint_resolver: EndpointResolver,
    persistent_writes: bool,
}

impl FileDownloadTool {
    pub fn new(security: Arc<SecurityPolicy>, config: FileDownloadConfig) -> Self {
        Self::new_with_persistence(security, config, true)
    }

    /// Construct with an explicit persistence flag derived from the active
    /// runtime adapter's `has_filesystem_access()`. Mirrors
    /// [`super::file_write::FileWriteTool::new_with_persistence`].
    pub fn new_with_persistence(
        security: Arc<SecurityPolicy>,
        config: FileDownloadConfig,
        persistent_writes: bool,
    ) -> Self {
        let snapshot = FileDownloadSsrfPolicy {
            allowed_private_hosts: config.allowed_private_hosts.clone(),
            nat64_prefixes: Vec::new(),
        };
        Self::new_with_persistence_and_resolver(security, config, persistent_writes, move || {
            snapshot.clone()
        })
    }

    pub fn new_with_persistence_and_resolver<F>(
        security: Arc<SecurityPolicy>,
        config: FileDownloadConfig,
        persistent_writes: bool,
        policy_resolver: F,
    ) -> Self
    where
        F: Fn() -> FileDownloadSsrfPolicy + Send + Sync + 'static,
    {
        Self {
            security,
            config,
            policy_resolver: Arc::new(policy_resolver),
            endpoint_resolver: default_endpoint_resolver(),
            persistent_writes,
        }
    }

    #[cfg(test)]
    fn new_with_endpoint_resolver<F>(
        security: Arc<SecurityPolicy>,
        config: FileDownloadConfig,
        persistent_writes: bool,
        policy_resolver: F,
        endpoint_resolver: EndpointResolver,
    ) -> Self
    where
        F: Fn() -> FileDownloadSsrfPolicy + Send + Sync + 'static,
    {
        Self {
            security,
            config,
            policy_resolver: Arc::new(policy_resolver),
            endpoint_resolver,
            persistent_writes,
        }
    }

    async fn validate_endpoint_host(
        &self,
        raw_url: &str,
    ) -> Result<(String, Vec<SocketAddr>), String> {
        let (transport_host, policy_host, port) = parse_endpoint_url(raw_url)?;
        let policy = (self.policy_resolver)();
        let allowed = normalize_allowed_private_hosts(&policy.allowed_private_hosts);
        let nat64_prefixes = normalize_nat64_prefixes(&policy.nat64_prefixes)?;
        let resolved_addrs = (self.endpoint_resolver)(transport_host.clone(), port).await?;
        ssrf_check_endpoint(&policy_host, &resolved_addrs, &allowed, &nat64_prefixes)?;
        Ok((transport_host, resolved_addrs))
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
            Self::tool_msg_with_args(
                "tool-file-download-error-temp-create",
                &[("err", &e.to_string())],
            )
        })?;

        let mut stream = response.bytes_stream();
        let mut written: u64 = 0;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| {
                Self::tool_msg_with_args(
                    "tool-file-download-error-read-body",
                    &[("err", &e.to_string())],
                )
            })?;
            written = written.saturating_add(chunk.len() as u64);
            if written > max_bytes {
                let limit = max_bytes.to_string();
                return Err(Self::tool_msg_with_args(
                    "tool-file-download-error-too-large-stream",
                    &[("limit", &limit)],
                ));
            }
            file.write_all(&chunk).await.map_err(|e| {
                Self::tool_msg_with_args(
                    "tool-file-download-error-write-body",
                    &[("err", &e.to_string())],
                )
            })?;
        }

        file.flush().await.map_err(|e| {
            Self::tool_msg_with_args("tool-file-download-error-flush", &[("err", &e.to_string())])
        })?;
        Ok(written)
    }

    fn tool_msg(key: &str) -> String {
        crate::i18n::get_required_tool_string(key)
    }

    fn tool_msg_with_args(key: &str, args: &[(&str, &str)]) -> String {
        crate::i18n::get_required_tool_string_with_args(key, args)
    }
}

fn proxy_conflicts_with_dns_pinning(config: &ProxyConfig) -> bool {
    (config.enabled && config.scope == ProxyScope::Environment)
        || (config.has_any_proxy_url() && config.should_apply_to_service("tool.file_download"))
}

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

    let host = parsed
        .host_str()
        .filter(|host| !host.is_empty())
        .ok_or_else(|| anyhow::Error::msg("URL must include a valid host"))?;
    if host.contains(':') {
        anyhow::bail!("IPv6 hosts are not supported in file_download endpoint URLs");
    }

    Ok(host.to_ascii_lowercase())
}

fn parse_endpoint_url(raw_url: &str) -> Result<(String, String, u16), String> {
    let url = raw_url.trim();
    if url.is_empty() {
        return Err(FileDownloadTool::tool_msg(
            "tool-file-download-error-disabled",
        ));
    }

    let parsed = reqwest::Url::parse(url).map_err(|e| {
        FileDownloadTool::tool_msg_with_args(
            "tool-file-download-error-invalid-url",
            &[("err", &e.to_string())],
        )
    })?;

    match parsed.scheme() {
        "http" | "https" => {}
        _ => {
            return Err(FileDownloadTool::tool_msg_with_args(
                "tool-file-download-error-bad-scheme",
                &[("scheme", parsed.scheme())],
            ));
        }
    }

    let port = parsed.port_or_known_default().ok_or_else(|| {
        FileDownloadTool::tool_msg_with_args(
            "tool-file-download-error-invalid-url",
            &[("err", "URL must include a valid port")],
        )
    })?;
    let transport_host = extract_download_url_host(url).map_err(|e| {
        FileDownloadTool::tool_msg_with_args(
            "tool-file-download-error-invalid-url",
            &[("err", &e.to_string())],
        )
    })?;
    let policy_host = transport_host.trim_end_matches('.').to_string();

    Ok((transport_host, policy_host, port))
}

async fn resolve_endpoint_ips(host: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(vec![SocketAddr::new(ip, port)]);
    }

    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| {
            FileDownloadTool::tool_msg_with_args(
                "tool-file-download-error-invalid-url",
                &[("err", &format!("Failed to resolve host '{host}': {e}"))],
            )
        })?
        .collect();
    if addrs.is_empty() {
        return Err(FileDownloadTool::tool_msg_with_args(
            "tool-file-download-error-invalid-url",
            &[("err", &format!("Failed to resolve host '{host}'"))],
        ));
    }
    Ok(addrs)
}

fn normalize_allowed_private_hosts(allowed: &[String]) -> Vec<String> {
    match domain_guard::normalize_allowed_domains(
        allowed.to_vec(),
        "file_download.allowed_private_hosts",
    ) {
        Ok(allowed) => allowed,
        Err(error) => {
            NORMALIZE_ALLOWED_PRIVATE_HOSTS_WARNING.get_or_init(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": error.to_string()})),
                    "file_download: failed to normalize allowed_private_hosts; using empty list"
                );
            });
            Vec::new()
        }
    }
}

fn normalize_nat64_prefixes(raw: &[String]) -> Result<Vec<domain_guard::Nat64Prefix>, String> {
    domain_guard::parse_nat64_prefixes(raw, "security.nat64_prefixes").map_err(|error| {
        FileDownloadTool::tool_msg_with_args(
            "tool-file-download-error-invalid-nat64-prefix",
            &[
                ("prefix", &error.to_string()),
                ("config_key", "security.nat64_prefixes"),
            ],
        )
    })
}

fn declared_nat64_metadata(
    ip: IpAddr,
    nat64_prefixes: &[domain_guard::Nat64Prefix],
) -> Option<std::net::Ipv4Addr> {
    let IpAddr::V6(v6) = ip else {
        return None;
    };
    nat64_prefixes
        .iter()
        .filter_map(|prefix| prefix.embedded_ipv4(v6))
        .find(|embedded| domain_guard::is_cloud_metadata_ip(IpAddr::V4(*embedded)))
}

fn ssrf_check_endpoint(
    policy_host: &str,
    resolved_addrs: &[SocketAddr],
    allowed_hosts: &[String],
    nat64_prefixes: &[domain_guard::Nat64Prefix],
) -> Result<(), String> {
    let ips: Vec<IpAddr> = resolved_addrs.iter().map(SocketAddr::ip).collect();
    let private_allowed = domain_guard::host_matches_allowlist(policy_host, allowed_hosts);

    if let Some((raw_ip, metadata_ip)) = ips.iter().find_map(|ip| {
        if domain_guard::is_cloud_metadata_ip(*ip) {
            Some((*ip, *ip))
        } else {
            declared_nat64_metadata(*ip, nat64_prefixes).map(|v4| (*ip, IpAddr::V4(v4)))
        }
    }) {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "tool": "file_download",
                    "host": policy_host,
                    "ip": raw_ip.to_string(),
                })),
            "file_download: rejected cloud metadata/credential endpoint host"
        );
        return Err(FileDownloadTool::tool_msg_with_args(
            "tool-file-download-error-metadata-endpoint",
            &[("host", policy_host), ("ip", &metadata_ip.to_string())],
        ));
    }

    let validation = if private_allowed {
        domain_guard::validate_resolved_ips_exclude_metadata(policy_host, &ips, nat64_prefixes)
    } else {
        domain_guard::validate_resolved_ips_are_public(policy_host, &ips, nat64_prefixes)
    };
    if let Err(error) = validation {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "tool": "file_download",
                    "host": policy_host,
                })),
            "file_download: rejected private/local endpoint host"
        );
        return Err(FileDownloadTool::tool_msg_with_args(
            "tool-file-download-error-private-host",
            &[
                ("host", policy_host),
                ("config_key", "file_download.allowed_private_hosts"),
                ("err", &error.to_string()),
            ],
        ));
    }

    if private_allowed
        && domain_guard::validate_resolved_ips_are_public(policy_host, &ips, nat64_prefixes)
            .is_err()
    {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({
                    "tool": "file_download",
                    "host": policy_host,
                })),
            "file_download: allowing private host via allowed_private_hosts"
        );
    }

    Ok(())
}

async fn build_secure_download_client(
    transport_host: &str,
    resolved_addrs: &[SocketAddr],
    timeout_secs: u64,
) -> Result<reqwest::Client, String> {
    let builder = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(timeout_secs))
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none());
    let builder = if transport_host.parse::<IpAddr>().is_ok() {
        builder
    } else {
        builder.resolve_to_addrs(transport_host, resolved_addrs)
    };
    builder.build().map_err(|e| {
        FileDownloadTool::tool_msg_with_args(
            "tool-file-download-error-client-build",
            &[("err", &e.to_string())],
        )
    })
}

static NORMALIZE_ALLOWED_PRIVATE_HOSTS_WARNING: OnceLock<()> = OnceLock::new();

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
                    "description": Self::tool_msg("tool-file-download-param-document-id")
                },
                "dest_path": {
                    "type": "string",
                    "description": Self::tool_msg("tool-file-download-param-dest-path")
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
                error: Some(Self::tool_msg("tool-file-download-error-disabled")),
            });
        };

        if !self.security.can_act() {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(Self::tool_msg("tool-file-download-error-read-only")),
            });
        }

        if self.security.is_rate_limited() {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(Self::tool_msg("tool-file-download-error-rate-limited-hour")),
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
                anyhow::Error::msg(Self::tool_msg(
                    "tool-file-download-error-missing-document-id",
                ))
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
                anyhow::Error::msg(Self::tool_msg("tool-file-download-error-missing-dest-path"))
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
                    error: Some(Self::tool_msg_with_args(
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
                error: Some(Self::tool_msg_with_args(
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
                    error: Some(Self::tool_msg_with_args(
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

        let proxy_config = zeroclaw_config::schema::runtime_proxy_config();
        if proxy_conflicts_with_dns_pinning(&proxy_config) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"service": "tool.file_download"})),
                "file_download: configured runtime proxy rejected to preserve validated DNS pin"
            );
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(FILE_DOWNLOAD_PROXY_PINNING_ERROR.into()),
            });
        }

        if let Some(variable) = zeroclaw_config::schema::environment_proxy_for_url(url) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"proxy_variable": variable})),
                "file_download: environment proxy ignored to preserve validated DNS pin"
            );
        }

        let (transport_host, resolved_addrs) = match self.validate_endpoint_host(url).await {
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
                error: Some(Self::tool_msg(
                    "tool-file-download-error-rate-limited-budget",
                )),
            });
        }

        let client = match build_secure_download_client(
            &transport_host,
            &resolved_addrs,
            self.config.timeout_secs,
        )
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
                let error = e.without_url().to_string();
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(Self::tool_msg_with_args(
                        "tool-file-download-error-request",
                        &[("err", &error)],
                    )),
                });
            }
        };

        let status = response.status();

        if !status.is_success() {
            let raw_body = response.text().await.unwrap_or_default();
            let truncated = if raw_body.len() > RESPONSE_BODY_LIMIT_BYTES {
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
                error: Some(Self::tool_msg_with_args(
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
                error: Some(Self::tool_msg_with_args(
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
                    let output = Self::tool_msg_with_args(
                        "tool-file-download-success",
                        &[
                            ("written", &written.to_string()),
                            ("dest_path", dest_path),
                            ("status", &status.to_string()),
                        ],
                    );
                    // The download landed in an ephemeral workspace and will not
                    // reach the host — warn loudly rather than report a bare
                    // success
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
                        error: Some(Self::tool_msg_with_args(
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
    use std::sync::atomic::{AtomicUsize, Ordering};
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

    fn cfg_for_local_server(server: &MockServer) -> FileDownloadConfig {
        FileDownloadConfig {
            url: Some(format!("{}/download", server.uri())),
            allowed_private_hosts: vec!["127.0.0.1".into()],
            ..FileDownloadConfig::default()
        }
    }

    fn tool_with_resolver(
        config: FileDownloadConfig,
        resolved_addrs: Vec<SocketAddr>,
        nat64_prefixes: Vec<String>,
    ) -> FileDownloadTool {
        let tmp = TempDir::new().unwrap();
        let snapshot = FileDownloadSsrfPolicy {
            allowed_private_hosts: config.allowed_private_hosts.clone(),
            nat64_prefixes,
        };
        let endpoint_resolver: EndpointResolver = Arc::new(move |_host: String, _port: u16| {
            let resolved_addrs = resolved_addrs.clone();
            Box::pin(async move { Ok(resolved_addrs) })
        });
        FileDownloadTool::new_with_endpoint_resolver(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            config,
            true,
            move || snapshot.clone(),
            endpoint_resolver,
        )
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

    #[test]
    fn proxy_conflict_detection_matches_dns_pinned_service_scope() {
        assert!(proxy_conflicts_with_dns_pinning(&ProxyConfig {
            enabled: true,
            http_proxy: Some("http://127.0.0.1:8080".into()),
            scope: ProxyScope::Environment,
            ..ProxyConfig::default()
        }));
        assert!(proxy_conflicts_with_dns_pinning(&ProxyConfig {
            enabled: true,
            http_proxy: Some("http://127.0.0.1:8080".into()),
            scope: ProxyScope::Services,
            services: vec!["tool.file_download".into()],
            ..ProxyConfig::default()
        }));
        assert!(!proxy_conflicts_with_dns_pinning(&ProxyConfig {
            enabled: true,
            http_proxy: Some("http://127.0.0.1:8080".into()),
            scope: ProxyScope::Services,
            services: vec!["tool.http_request".into()],
            ..ProxyConfig::default()
        }));
    }

    #[test]
    fn parse_endpoint_url_bad_scheme_does_not_echo_secret_url() {
        let err =
            parse_endpoint_url("ftp://user:secret@example.com/download?token=abc").unwrap_err();

        assert!(err.contains("ftp"));
        assert!(!err.contains("secret"));
        assert!(!err.contains("token=abc"));
    }

    #[tokio::test]
    async fn validate_endpoint_host_rejects_private_literal_by_default() {
        let tmp = TempDir::new().unwrap();
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            cfg(Some("http://127.0.0.1:1/download".into())),
        );

        let err = tool
            .validate_endpoint_host("http://127.0.0.1:1/download")
            .await
            .unwrap_err();

        assert!(err.contains("127.0.0.1"));
        assert!(err.contains("file_download.allowed_private_hosts"));
    }

    #[tokio::test]
    async fn validate_endpoint_host_allows_explicit_private_host() {
        let tmp = TempDir::new().unwrap();
        let mut config = cfg(Some("http://127.0.0.1:1/download".into()));
        config.allowed_private_hosts = vec!["127.0.0.1".into()];
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            config,
        );

        assert!(
            tool.validate_endpoint_host("http://127.0.0.1:1/download")
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn validate_endpoint_host_rejects_metadata_even_when_allowed() {
        let tmp = TempDir::new().unwrap();
        let mut config = cfg(Some("http://169.254.169.254:80/latest".into()));
        config.allowed_private_hosts = vec!["169.254.169.254".into()];
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            config,
        );

        let err = tool
            .validate_endpoint_host("http://169.254.169.254:80/latest")
            .await
            .unwrap_err();

        assert!(err.contains("metadata"));
        assert!(!err.contains("To allow this host"));
    }

    #[tokio::test]
    async fn validate_endpoint_host_rejects_declared_nat64_metadata() {
        let resolved = vec![SocketAddr::new(
            "2001:4860:64:ff9b::a9fe:a9fe".parse().unwrap(),
            80,
        )];
        let mut config = cfg(Some("http://files.example.test/download".into()));
        config.allowed_private_hosts = vec!["files.example.test".into()];
        let tool = tool_with_resolver(config, resolved, vec!["2001:4860:64:ff9b::/96".into()]);

        let err = tool
            .validate_endpoint_host("http://files.example.test/download")
            .await
            .unwrap_err();

        assert!(err.contains("metadata"));
        assert!(err.contains("169.254.169.254"));
    }

    #[tokio::test]
    async fn validate_endpoint_host_fails_closed_on_malformed_nat64_before_dns() {
        let tmp = TempDir::new().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let endpoint_resolver: EndpointResolver = Arc::new(move |_host: String, port: u16| {
            counter.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                Ok(vec![SocketAddr::new(
                    IpAddr::from([93, 184, 216, 34]),
                    port,
                )])
            })
        });
        let config = cfg(Some("http://files.example.test/download".into()));
        let policy = FileDownloadSsrfPolicy {
            allowed_private_hosts: Vec::new(),
            nat64_prefixes: vec!["2606:4700:4700::1/48".into()],
        };
        let tool = FileDownloadTool::new_with_endpoint_resolver(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            config,
            true,
            move || policy.clone(),
            endpoint_resolver,
        );

        let err = tool
            .validate_endpoint_host("http://files.example.test/download")
            .await
            .unwrap_err();

        assert!(err.contains("security.nat64_prefixes"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn build_secure_download_client_binds_hostname_to_validated_addrs() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/download"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"ok".to_vec()))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            build_secure_download_client("files.example.invalid", &[*server.address()], 30)
                .await
                .unwrap();
        let result = client
            .get(format!(
                "http://files.example.invalid:{}/download",
                server.address().port()
            ))
            .send()
            .await;

        assert!(result.is_ok(), "request must use the pinned address");
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
        let config = cfg_for_local_server(&server);
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

        let config = cfg_for_local_server(&server);
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

        let config = cfg_for_local_server(&server);
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
        let mut config = cfg_for_local_server(&server);
        config.headers = headers;
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
    async fn execute_request_error_redacts_endpoint_url_and_document_id() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let tmp = TempDir::new().unwrap();
        let endpoint_secret = "secret-query-token-should-not-leak";
        let document_id = "secret-document-id-should-not-leak";
        let mut config = cfg(Some(format!(
            "http://127.0.0.1:{port}/download?api_key={endpoint_secret}"
        )));
        config.allowed_private_hosts = vec!["127.0.0.1".into()];
        config.timeout_secs = 1;
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            config,
        );

        let result = tool
            .execute(json!({ "document_id": document_id, "dest_path": "out.bin" }))
            .await
            .unwrap();

        assert!(!result.success);
        let error = result.error.as_deref().unwrap_or("");
        assert!(
            !error.contains(endpoint_secret),
            "request errors must not echo endpoint query secrets: {error}"
        );
        assert!(
            !error.contains(document_id),
            "request errors must not echo document_id query values: {error}"
        );
        assert!(
            !error.contains("api_key"),
            "request errors must not echo configured query keys: {error}"
        );
        assert!(!tmp.path().join("out.bin").exists());
        drop(listener);
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

        let config = cfg_for_local_server(&server);
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

        let mut config = cfg_for_local_server(&server);
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

        let mut config = cfg_for_local_server(&server);
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

        let config = cfg_for_local_server(&server);
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

        let mut body = "x".repeat(4094);
        body.push_str("世界世界世界世界世界世界");
        assert!(!body.is_char_boundary(4096));

        Mock::given(method("GET"))
            .and(path("/download"))
            .respond_with(ResponseTemplate::new(500).set_body_string(body.clone()))
            .expect(1)
            .mount(&server)
            .await;

        let config = cfg_for_local_server(&server);
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
}
