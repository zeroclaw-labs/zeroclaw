use anyhow::Context;
use async_trait::async_trait;
use serde_json::json;
use std::future::Future;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult, with_ephemeral_workspace_warning};
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::policy::ToolOperation;

use crate::helpers::domain_guard;

const TOOL_DESCRIPTION_KEY: &str = "tool-image-gen";
static TOOL_DESCRIPTION: OnceLock<String> = OnceLock::new();

/// Maximum redirect hops the download loop follows before giving up.
const MAX_REDIRECT_HOPS: usize = 10;

/// Production DNS resolver for the download pipeline. Exists as a free
/// function so the injectable-resolver seam can take `&F` and the
/// production path passes `&resolve_host_for_download` — the same shape
/// `http_request.rs` uses for `resolve_host_for_request`.
async fn resolve_host_for_download(
    host: String,
    port: u16,
) -> anyhow::Result<Vec<std::net::SocketAddr>> {
    let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host.as_str(), port))
        .await
        .context(ImageGenTool::tool_msg(
            "tool-image-gen-error-resolved-url-resolve-failed",
        ))?
        .collect();
    if addrs.is_empty() {
        anyhow::bail!(ImageGenTool::tool_msg_with_args(
            "tool-image-gen-error-resolved-url-resolve-empty",
            &[("host", &host)],
        ));
    }
    Ok(addrs)
}

/// A download target that has passed both SSRF gates: the validated URL
/// string, its bare hostname, and the resolved-and-validated socket
/// addresses. Produced only by `ImageGenTool::validate_image_target` and
/// consumed as a single value by `ImageGenTool::build_download_client`, so
/// the validated URL, the `resolve_to_addrs` lookup key, and the pinned
/// address set cannot drift apart at the call site. `resolve_to_addrs`
/// keys by bare hostname (lower-cased): keying the override by the full
/// URL would never match the request hostname and the validated address
/// set would be silently ignored at connect time, reopening the
/// DNS-rebinding window — the struct makes that mistake a type error.
struct ValidatedImageTarget {
    url: String,
    host: String,
    addrs: Vec<std::net::SocketAddr>,
}

fn resolve_image_filename(filename_arg: Option<&str>, nanos: u128) -> String {
    filename_arg
        .filter(|s| !s.trim().is_empty())
        .map(|s| {
            PathBuf::from(s).file_name().map_or_else(
                || "generated_image".to_string(),
                |n| n.to_string_lossy().to_string(),
            )
        })
        .unwrap_or_else(|| format!("generated_image_{nanos}"))
}

fn format_image_tool_output(
    path_display: &str,
    size_kb: usize,
    model: &str,
    prompt: &str,
) -> String {
    format!(
        "Image generated successfully.\n\
         File: {path_display}\n\
         Size: {size_kb} KB\n\
         Model: {model}\n\
         Prompt: {prompt}\n\
         [IMAGE:{path_display}]",
    )
}

pub struct ImageGenTool {
    security: Arc<SecurityPolicy>,
    workspace_dir: PathBuf,
    default_model: String,
    api_key_env: String,
    allowed_private_hosts: Vec<String>,
    persistent_writes: bool,
}

impl ImageGenTool {
    pub fn new(
        security: Arc<SecurityPolicy>,
        workspace_dir: PathBuf,
        default_model: String,
        api_key_env: String,
    ) -> Self {
        Self {
            security,
            workspace_dir,
            default_model,
            api_key_env,
            allowed_private_hosts: Vec::new(),
            persistent_writes: true,
        }
    }

    /// Construct with an explicit persistence flag derived from the active
    /// runtime adapter's `has_filesystem_access()`. Mirrors
    /// [`super::file_write::FileWriteTool::new_with_persistence`].
    pub fn new_with_persistence(
        security: Arc<SecurityPolicy>,
        workspace_dir: PathBuf,
        default_model: String,
        api_key_env: String,
        persistent_writes: bool,
    ) -> Self {
        Self {
            security,
            workspace_dir,
            default_model,
            api_key_env,
            allowed_private_hosts: Vec::new(),
            persistent_writes,
        }
    }

    /// Construct with the full config (including `allowed_private_hosts`).
    /// The host allowlist is normalized via `domain_guard::normalize_allowed_domains`
    /// at construction time so per-request validation is a constant-time
    /// allowlist match (no per-call parsing).
    pub fn new_with_config(
        security: Arc<SecurityPolicy>,
        workspace_dir: PathBuf,
        default_model: String,
        api_key_env: String,
        persistent_writes: bool,
        allowed_private_hosts: Vec<String>,
    ) -> anyhow::Result<Self> {
        let normalized = domain_guard::normalize_allowed_domains(
            allowed_private_hosts,
            "image_gen.allowed_private_hosts",
        )?;
        Ok(Self {
            security,
            workspace_dir,
            default_model,
            api_key_env,
            allowed_private_hosts: normalized,
            persistent_writes,
        })
    }

    /// Resolve a tool-localized Fluent key to its current-locale string.
    /// Mirrors `file_download.rs:99-101`.
    fn tool_msg(key: &str) -> String {
        crate::i18n::get_required_tool_string(key)
    }

    /// Resolve a tool-localized Fluent key with named arguments (Fluent
    /// external args, e.g. `{ $host }`). Mirrors `file_download.rs:103-105`.
    fn tool_msg_with_args(key: &str, args: &[(&str, &str)]) -> String {
        crate::i18n::get_required_tool_string_with_args(key, args)
    }

    /// Validate the URL of an image to be downloaded from a (server-supplied)
    /// fal.ai response. Mirrors `http_request::validate_url_policy` but for
    /// the image-download stage: no `allowed_domains` (we trust fal.ai's
    /// hostname choice), only a private-host gate lifted by
    /// `allowed_private_hosts`. Cloud-metadata IP literals are rejected even
    /// when `allowed_private_hosts` would otherwise lift the gate; the
    /// metadata check covers the endpoints known to the shared
    /// `domain_guard::is_cloud_metadata_ip` predicate (169.254.169.254 and
    /// fd00:ec2::254), not every cloud vendor's metadata address.
    fn validate_image_url(&self, raw_url: &str) -> anyhow::Result<String> {
        let url = raw_url.trim();
        if url.is_empty() {
            anyhow::bail!(Self::tool_msg("tool-image-gen-error-url-empty"));
        }
        if url.chars().any(char::is_whitespace) {
            anyhow::bail!(Self::tool_msg("tool-image-gen-error-url-whitespace"));
        }
        if !url.starts_with("http://") && !url.starts_with("https://") {
            anyhow::bail!(Self::tool_msg("tool-image-gen-error-url-scheme"));
        }

        let parsed = reqwest::Url::parse(url).map_err(|e| {
            anyhow::Error::msg(Self::tool_msg_with_args(
                "tool-image-gen-error-url-parse",
                &[("err", &e.to_string())],
            ))
        })?;

        // Reject userinfo-bearing URLs at parse time — the same shape used
        // by the http_request SSRF gate (`http_request.rs` extract_host).
        // A `user:pass@host` form is never legitimate for an image CDN.
        if !parsed.username().is_empty() || parsed.password().is_some() {
            anyhow::bail!(Self::tool_msg("tool-image-gen-error-url-userinfo"));
        }

        let host = parsed
            .host_str()
            .ok_or_else(|| anyhow::Error::msg(Self::tool_msg("tool-image-gen-error-url-no-host")))?
            .to_string();

        // Cloud-metadata IP literals recognized by the shared
        // `domain_guard::is_cloud_metadata_ip` predicate (169.254.169.254,
        // fd00:ec2::254) are rejected unconditionally — never a legitimate
        // image source.
        if host
            .parse::<IpAddr>()
            .is_ok_and(domain_guard::is_cloud_metadata_ip)
        {
            anyhow::bail!(Self::tool_msg_with_args(
                "tool-image-gen-error-url-metadata-host",
                &[("host", &host)],
            ));
        }

        let host_is_private_or_local = domain_guard::is_private_or_local_host(&host);
        let private_match = if host_is_private_or_local {
            domain_guard::host_matches_allowlist(&host, &self.allowed_private_hosts)
        } else {
            false
        };

        if host_is_private_or_local && !private_match {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"tool": "image_gen", "host": host})),
                "image_gen: rejecting private/local image host"
            );
            anyhow::bail!(Self::tool_msg_with_args(
                "tool-image-gen-error-url-private-host",
                &[("host", &host)],
            ));
        }

        // Warn loudly when an explicit carve-out lifts the SSRF gate — same
        // signal as web_fetch's redirect-path warn, so operators can detect
        // accidental "trusted internal CDN" misconfigurations in audit.
        if host_is_private_or_local && private_match {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"tool": "image_gen", "host": host})),
                "image_gen: allowing private/local image host via allowed_private_hosts"
            );
        }

        Ok(url.to_string())
    }

    /// After the host-string gate passes, resolve the validated image URL
    /// to its IP addresses and reject any that are private/local or cloud
    /// metadata — catches the SSRF class where a public-looking hostname
    /// (e.g., `attacker.example`) resolves to `10.0.0.5`, `127.0.0.1`, or
    /// `169.254.169.254`. When the host is covered by
    /// `allowed_private_hosts`, the non-global check is lifted and only the
    /// metadata addresses known to `domain_guard::is_cloud_metadata_ip`
    /// (169.254.169.254, fd00:ec2::254) remain rejected (consistent with
    /// the allowlist semantics in the other tools).
    ///
    /// Returns the parsed hostname + validated socket addresses so the
    /// caller can bind the download connection to them via
    /// `resolve_to_addrs` (keyed by the hostname, NOT the full URL),
    /// closing the TOCTOU window between DNS check and transport connect.
    async fn validate_image_url_resolved_with_resolver<F, Fut>(
        &self,
        validated_url: &str,
        resolve_host: &F,
    ) -> anyhow::Result<(String, Vec<std::net::SocketAddr>)>
    where
        F: Fn(String, u16) -> Fut,
        Fut: Future<Output = anyhow::Result<Vec<std::net::SocketAddr>>>,
    {
        let parsed = reqwest::Url::parse(validated_url).map_err(|e| {
            anyhow::Error::msg(Self::tool_msg_with_args(
                "tool-image-gen-error-resolved-url-parse",
                &[("err", &e.to_string())],
            ))
        })?;
        let host = parsed.host_str().ok_or_else(|| {
            anyhow::Error::msg(Self::tool_msg("tool-image-gen-error-resolved-url-no-host"))
        })?;
        let port = parsed.port_or_known_default().ok_or_else(|| {
            anyhow::Error::msg(Self::tool_msg("tool-image-gen-error-resolved-url-no-port"))
        })?;

        let addrs = resolve_host(host.to_string(), port).await?;

        if addrs.is_empty() {
            anyhow::bail!(Self::tool_msg_with_args(
                "tool-image-gen-error-resolved-url-resolve-empty",
                &[("host", host)],
            ));
        }

        let ips: Vec<std::net::IpAddr> = addrs.iter().map(|sa| sa.ip()).collect();

        let private_resolution_allowed =
            domain_guard::host_matches_allowlist(host, &self.allowed_private_hosts);

        for ip in &ips {
            // The metadata check is unconditional — never lifted by the
            // allowlist. Its scope is exactly the endpoints known to the
            // shared `domain_guard::is_cloud_metadata_ip` predicate
            // (169.254.169.254 and fd00:ec2::254), not every cloud
            // vendor's metadata address.
            if domain_guard::is_cloud_metadata_ip(*ip) {
                anyhow::bail!(Self::tool_msg_with_args(
                    "tool-image-gen-error-resolved-ip-metadata",
                    &[("host", host), ("ip", &ip.to_string())],
                ));
            }
            if !private_resolution_allowed {
                let non_global = match ip {
                    std::net::IpAddr::V4(v4) => domain_guard::is_non_global_v4(*v4),
                    std::net::IpAddr::V6(v6) => domain_guard::is_non_global_v6(*v6),
                };
                if non_global {
                    anyhow::bail!(Self::tool_msg_with_args(
                        "tool-image-gen-error-resolved-ip-non-global",
                        &[("host", host), ("ip", &ip.to_string())],
                    ));
                }
            }
        }

        Ok((host.to_string(), addrs))
    }

    /// Public entry point: same as `validate_image_url_resolved_with_resolver`
    /// but always resolves via `tokio::net::lookup_host` (the production path).
    #[cfg_attr(not(test), allow(dead_code))]
    async fn validate_image_url_resolved(
        &self,
        validated_url: &str,
    ) -> anyhow::Result<(String, Vec<std::net::SocketAddr>)> {
        self.validate_image_url_resolved_with_resolver(validated_url, &resolve_host_for_download)
            .await
    }

    /// Run both SSRF gates for one download URL — the host-string gate
    /// (`validate_image_url`) followed by the resolved-IP gate
    /// (`validate_image_url_resolved`) — and bundle the outcome into a
    /// single `ValidatedImageTarget`. The download loop calls this for the
    /// initial URL and again for every redirect target, so each hop is
    /// validated, resolved, and pinned before any connection to it.
    async fn validate_image_target_with_resolver<F, Fut>(
        &self,
        raw_url: &str,
        resolve_host: &F,
    ) -> anyhow::Result<ValidatedImageTarget>
    where
        F: Fn(String, u16) -> Fut,
        Fut: Future<Output = anyhow::Result<Vec<std::net::SocketAddr>>>,
    {
        let url = self.validate_image_url(raw_url)?;
        let (host, addrs) = self
            .validate_image_url_resolved_with_resolver(&url, resolve_host)
            .await?;
        Ok(ValidatedImageTarget { url, host, addrs })
    }

    /// Public entry point: same as `validate_image_target_with_resolver`
    /// but always resolves via `tokio::net::lookup_host` (the production path).
    #[cfg_attr(not(test), allow(dead_code))]
    async fn validate_image_target(&self, raw_url: &str) -> anyhow::Result<ValidatedImageTarget> {
        self.validate_image_target_with_resolver(raw_url, &resolve_host_for_download)
            .await
    }

    /// Build the download client for one validated hop: the connection is
    /// dialed only to the hop's validated address set (`resolve_to_addrs`
    /// keyed by `target.host`, the bare hostname — see the struct docs),
    /// proxies are disabled so this process always dials the validated
    /// addresses itself, and reqwest's automatic redirect following is
    /// off. Redirects are handled by `download_image`, which re-runs the
    /// full validate-resolve-pin pipeline for each hop.
    fn build_download_client(
        &self,
        target: &ValidatedImageTarget,
    ) -> anyhow::Result<reqwest::Client> {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .connect_timeout(std::time::Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            // The pinning above is only meaningful if THIS process dials
            // the validated addresses. reqwest honors proxy env vars by
            // default, and a proxy resolves and connects on our behalf —
            // the override would never apply and the proxy's DNS view
            // (which may include internal hosts) decides the destination,
            // reopening the rebinding window. Disable proxies so the
            // connection is always dialed directly to the validated set.
            .no_proxy()
            .resolve_to_addrs(&target.host, &target.addrs)
            .build()
            .map_err(|e| {
                anyhow::Error::msg(Self::tool_msg_with_args(
                    "tool-image-gen-error-client-build",
                    &[("err", &e.to_string())],
                ))
            })
    }

    /// Download an image URL, following redirects hop by hop. The initial
    /// URL and every redirect target go through the full
    /// validate-resolve-pin pipeline (`validate_image_target`), and each
    /// hop is fetched with a client bound to that hop's validated
    /// addresses — a public-looking redirect target that resolves to a
    /// private, local, or metadata address is rejected before any
    /// connection to it is attempted. Relative `Location` values are
    /// resolved against the current hop's URL.
    async fn download_image_with_resolver<F, Fut>(
        &self,
        image_url: &str,
        resolve_host: &F,
    ) -> anyhow::Result<reqwest::Response>
    where
        F: Fn(String, u16) -> Fut,
        Fut: Future<Output = anyhow::Result<Vec<std::net::SocketAddr>>>,
    {
        let mut target = self
            .validate_image_target_with_resolver(image_url, resolve_host)
            .await?;
        let mut redirects_followed = 0usize;

        loop {
            let client = self.build_download_client(&target)?;
            let resp = client
                .get(&target.url)
                .send()
                .await
                .context("Failed to download generated image")?;

            if !resp.status().is_redirection() {
                return Ok(resp);
            }

            redirects_followed += 1;
            if redirects_followed > MAX_REDIRECT_HOPS {
                anyhow::bail!(Self::tool_msg_with_args(
                    "tool-image-gen-error-redirect-limit",
                    &[("max", &MAX_REDIRECT_HOPS.to_string())],
                ));
            }

            let location = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| {
                    anyhow::Error::msg(Self::tool_msg(
                        "tool-image-gen-error-redirect-location-missing",
                    ))
                })?;
            let next = reqwest::Url::parse(&target.url)
                .and_then(|base| base.join(location))
                .map_err(|e| {
                    anyhow::Error::msg(Self::tool_msg_with_args(
                        "tool-image-gen-error-redirect-location-invalid",
                        &[("err", &e.to_string())],
                    ))
                })?;

            target = self
                .validate_image_target_with_resolver(next.as_str(), resolve_host)
                .await?;
        }
    }

    /// Public entry point: same as `download_image_with_resolver` but
    /// always resolves via `tokio::net::lookup_host` (the production path).
    async fn download_image(&self, image_url: &str) -> anyhow::Result<reqwest::Response> {
        self.download_image_with_resolver(image_url, &resolve_host_for_download)
            .await
    }

    /// Build a reusable HTTP client with reasonable timeouts.
    fn http_client() -> reqwest::Client {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .unwrap_or_default()
    }

    /// Read an API key from the environment.
    fn read_api_key(env_var: &str) -> Result<String, String> {
        std::env::var(env_var)
            .map(|v| v.trim().to_string())
            .ok()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| format!("Missing API key: set the {env_var} environment variable"))
    }

    /// Core generation logic: call fal.ai, download image, save to disk.
    async fn generate(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        // ── Parse parameters ───────────────────────────────────────
        let prompt = match args.get("prompt").and_then(|v| v.as_str()) {
            Some(p) if !p.trim().is_empty() => p.trim().to_string(),
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some("Missing required parameter: 'prompt'".into()),
                });
            }
        };

        // Sanitize filename — strip path components to prevent traversal.
        // When the caller doesn't provide one, generate a unique default so
        // successive calls without an explicit name never clobber each other.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let safe_name =
            resolve_image_filename(args.get("filename").and_then(|v| v.as_str()), nanos);

        let size = args
            .get("size")
            .and_then(|v| v.as_str())
            .unwrap_or("square_hd");

        // Validate size enum.
        const VALID_SIZES: &[&str] = &[
            "square_hd",
            "landscape_4_3",
            "portrait_4_3",
            "landscape_16_9",
            "portrait_16_9",
        ];
        if !VALID_SIZES.contains(&size) {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!(
                    "Invalid size '{size}'. Valid values: {}",
                    VALID_SIZES.join(", ")
                )),
            });
        }

        let model = args
            .get("model")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(&self.default_model);

        // Validate model identifier: must look like a fal.ai model path
        // (e.g. "fal-ai/flux/schnell"). Reject values with "..", query
        // strings, or fragments that could redirect the HTTP request.
        if model.contains("..")
            || model.contains('?')
            || model.contains('#')
            || model.contains('\\')
            || model.starts_with('/')
        {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!(
                    "Invalid model identifier '{model}'. \
                     Must be a fal.ai model path (e.g. 'fal-ai/flux/schnell')."
                )),
            });
        }

        // ── Read API key ───────────────────────────────────────────
        let api_key = match Self::read_api_key(&self.api_key_env) {
            Ok(k) => k,
            Err(msg) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(msg),
                });
            }
        };

        // ── Call fal.ai ────────────────────────────────────────────
        let client = Self::http_client();
        let url = format!("https://fal.run/{model}");

        let body = json!({
            "prompt": prompt,
            "image_size": size,
            "num_images": 1
        });

        let resp = client
            .post(&url)
            .header("Authorization", format!("Key {api_key}"))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .context("fal.ai request failed")?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("fal.ai API error ({status}): {body_text}")),
            });
        }

        let resp_json: serde_json::Value = resp
            .json()
            .await
            .context("Failed to parse fal.ai response as JSON")?;

        let image_url = resp_json
            .pointer("/images/0/url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                    "image_gen: fal.ai response missing image URL"
                );
                anyhow::Error::msg("No image URL in fal.ai response")
            })?;

        // ── Download image (SSRF-gated, redirects re-validated per hop) ──
        // The image URL is server-supplied by fal.ai (and therefore not
        // trustable in the same way as the operator-configured fal.run
        // endpoint above). `download_image` validates, resolves, and pins
        // the initial URL and every redirect target before connecting —
        // closes both the direct and the redirect-to-internal SSRF gap.
        let img_resp = self.download_image(image_url).await?;

        if !img_resp.status().is_success() {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!(
                    "Failed to download image from {image_url} ({})",
                    img_resp.status()
                )),
            });
        }

        let bytes = img_resp
            .bytes()
            .await
            .context("Failed to read image bytes")?;

        // ── Save to disk ───────────────────────────────────────────
        let images_dir = self.workspace_dir.join("images");
        tokio::fs::create_dir_all(&images_dir)
            .await
            .context("Failed to create images directory")?;

        let output_path = images_dir.join(format!("{safe_name}.png"));
        tokio::fs::write(&output_path, &bytes)
            .await
            .context("Failed to write image file")?;

        let size_kb = bytes.len() / 1024;

        let path_display = output_path.display().to_string();
        let output = format_image_tool_output(&path_display, size_kb, model, &prompt);

        Ok(ToolResult {
            success: true,
            output: output.into(),
            error: None,
        })
    }
}

#[async_trait]
impl Tool for ImageGenTool {
    fn name(&self) -> &str {
        "image_gen"
    }

    fn description(&self) -> &str {
        TOOL_DESCRIPTION.get_or_init(|| crate::i18n::get_required_tool_string(TOOL_DESCRIPTION_KEY))
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "required": ["prompt"],
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": Self::tool_msg("tool-image-gen-param-prompt")
                },
                "filename": {
                    "type": "string",
                    "description": Self::tool_msg("tool-image-gen-param-filename")
                },
                "size": {
                    "type": "string",
                    "enum": ["square_hd", "landscape_4_3", "portrait_4_3", "landscape_16_9", "portrait_16_9"],
                    "description": Self::tool_msg("tool-image-gen-param-size")
                },
                "model": {
                    "type": "string",
                    "description": Self::tool_msg("tool-image-gen-param-model")
                }
            }
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        // Security: image generation is a side-effecting action (HTTP + file write).
        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "image_gen")
        {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(error),
            });
        }

        let mut result = self.generate(args).await?;
        // A generated image saved to an ephemeral workspace never reaches the
        // host and is lost at session end; warn loudly on success
        if !self.persistent_writes && result.success {
            result.output = with_ephemeral_workspace_warning(&result.output).into();
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::autonomy::AutonomyLevel;
    use zeroclaw_config::policy::SecurityPolicy;

    fn test_security() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        })
    }

    fn test_tool() -> ImageGenTool {
        ImageGenTool::new(
            test_security(),
            std::env::temp_dir(),
            "fal-ai/flux/schnell".into(),
            "FAL_API_KEY".into(),
        )
    }

    #[test]
    fn tool_name() {
        let tool = test_tool();
        assert_eq!(tool.name(), "image_gen");
    }

    #[test]
    fn tool_description_is_nonempty() {
        let tool = test_tool();
        assert!(!tool.description().is_empty());
        assert!(tool.description().contains("image"));
    }

    #[test]
    fn tool_schema_has_required_prompt() {
        let tool = test_tool();
        let schema = tool.parameters_schema();
        assert_eq!(schema["required"], json!(["prompt"]));
        assert!(schema["properties"]["prompt"].is_object());
    }

    #[test]
    fn tool_schema_has_optional_params() {
        let tool = test_tool();
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["filename"].is_object());
        assert!(schema["properties"]["size"].is_object());
        assert!(schema["properties"]["model"].is_object());
    }

    #[test]
    fn tool_spec_roundtrip() {
        let tool = test_tool();
        let spec = tool.spec();
        assert_eq!(spec.name, "image_gen");
        assert!(spec.parameters.is_object());
    }

    #[tokio::test]
    async fn missing_prompt_returns_error() {
        let tool = test_tool();
        let result = tool.execute(json!({})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("prompt"));
    }

    #[tokio::test]
    async fn empty_prompt_returns_error() {
        let tool = test_tool();
        let result = tool.execute(json!({"prompt": "   "})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("prompt"));
    }

    #[tokio::test]
    async fn missing_api_key_returns_error() {
        // Temporarily ensure the env var is unset.
        let original = std::env::var("FAL_API_KEY_TEST_IMAGE_GEN").ok();
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("FAL_API_KEY_TEST_IMAGE_GEN") };

        let tool = ImageGenTool::new(
            test_security(),
            std::env::temp_dir(),
            "fal-ai/flux/schnell".into(),
            "FAL_API_KEY_TEST_IMAGE_GEN".into(),
        );
        let result = tool
            .execute(json!({"prompt": "a sunset over the ocean"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap()
                .contains("FAL_API_KEY_TEST_IMAGE_GEN")
        );

        // Restore if it was set.
        if let Some(val) = original {
            // SAFETY: test-only, single-threaded test runner.
            unsafe { std::env::set_var("FAL_API_KEY_TEST_IMAGE_GEN", val) };
        }
    }

    #[tokio::test]
    async fn invalid_size_returns_error() {
        // Set a dummy key so we get past the key check.
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("FAL_API_KEY_TEST_SIZE", "dummy_key") };

        let tool = ImageGenTool::new(
            test_security(),
            std::env::temp_dir(),
            "fal-ai/flux/schnell".into(),
            "FAL_API_KEY_TEST_SIZE".into(),
        );
        let result = tool
            .execute(json!({"prompt": "test", "size": "invalid_size"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("Invalid size"));

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("FAL_API_KEY_TEST_SIZE") };
    }

    #[tokio::test]
    async fn read_only_autonomy_blocks_execution() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        });
        let tool = ImageGenTool::new(
            security,
            std::env::temp_dir(),
            "fal-ai/flux/schnell".into(),
            "FAL_API_KEY".into(),
        );
        let result = tool.execute(json!({"prompt": "test image"})).await.unwrap();
        assert!(!result.success);
        let err = result.error.as_deref().unwrap();
        assert!(
            err.contains("read-only") || err.contains("image_gen"),
            "expected read-only or image_gen in error, got: {err}"
        );
    }

    #[tokio::test]
    async fn invalid_model_with_traversal_returns_error() {
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("FAL_API_KEY_TEST_MODEL", "dummy_key") };

        let tool = ImageGenTool::new(
            test_security(),
            std::env::temp_dir(),
            "fal-ai/flux/schnell".into(),
            "FAL_API_KEY_TEST_MODEL".into(),
        );
        let result = tool
            .execute(json!({"prompt": "test", "model": "../../evil-endpoint"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap()
                .contains("Invalid model identifier")
        );

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("FAL_API_KEY_TEST_MODEL") };
    }

    #[test]
    fn read_api_key_missing() {
        let result = ImageGenTool::read_api_key("DEFINITELY_NOT_SET_ZC_TEST_12345");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .contains("DEFINITELY_NOT_SET_ZC_TEST_12345")
        );
    }

    #[test]
    fn filename_traversal_is_sanitized() {
        // Verify that path traversal in filenames is stripped to just the final component.
        let sanitized = PathBuf::from("../../etc/passwd").file_name().map_or_else(
            || "generated_image".to_string(),
            |n| n.to_string_lossy().to_string(),
        );
        assert_eq!(sanitized, "passwd");

        // ".." alone has no file_name, falls back to default.
        let sanitized = PathBuf::from("..").file_name().map_or_else(
            || "generated_image".to_string(),
            |n| n.to_string_lossy().to_string(),
        );
        assert_eq!(sanitized, "generated_image");
    }

    #[test]
    fn resolve_image_filename_default_is_non_clobbering_and_unique() {
        // Exercises the PRODUCTION filename-selection helper an omitted
        // filename must yield a unique timestamped name, never the bare
        // `generated_image` that would clobber prior generations, and two
        // default calls must differ. Fails if the code reverts to a fixed name.
        let a = resolve_image_filename(None, 1_000);
        let b = resolve_image_filename(None, 2_000);
        assert_eq!(a, "generated_image_1000");
        assert_ne!(
            a, "generated_image",
            "default must not clobber the bare name"
        );
        assert_ne!(a, b, "successive default names must differ");
        // An explicit filename is used verbatim, with path components stripped.
        assert_eq!(resolve_image_filename(Some("my_pic"), 1_000), "my_pic");
        assert_eq!(
            resolve_image_filename(Some("../../etc/passwd"), 1_000),
            "passwd"
        );
        // Blank/whitespace filename falls back to the timestamped default.
        assert_eq!(
            resolve_image_filename(Some("   "), 1_000),
            "generated_image_1000"
        );
    }

    #[test]
    fn image_output_emits_matching_file_line_and_image_marker() {
        let path = "/ws/images/generated_image_42.png";
        let out = format_image_tool_output(path, 12, "fal-ai/flux", "a cat");
        assert!(
            out.contains(&format!("File: {path}")),
            "output must carry a durable File: line: {out}"
        );
        assert!(
            out.contains(&format!("[IMAGE:{path}]")),
            "output must carry a matching [IMAGE:<path>] marker: {out}"
        );
    }

    #[test]
    fn read_api_key_present() {
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZC_IMAGE_GEN_TEST_KEY", "test_value_123") };
        let result = ImageGenTool::read_api_key("ZC_IMAGE_GEN_TEST_KEY");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "test_value_123");
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZC_IMAGE_GEN_TEST_KEY") };
    }

    // ── SSRF gate tests for the image-download stage ──────────────
    //
    // These exercise `ImageGenTool::validate_image_url` directly — the same
    // gate `validate_image_target` applies to the initial URL and to every
    // redirect target. Hermetic — no network, no reqwest, no `fal.run`
    // POST. A regression that drops the SSRF gate (or relaxes the
    // private-host check) would surface here.

    fn test_tool_with_private_hosts(allowed_private_hosts: Vec<&str>) -> ImageGenTool {
        ImageGenTool::new_with_config(
            test_security(),
            std::env::temp_dir(),
            "fal-ai/flux/schnell".into(),
            "FAL_API_KEY".into(),
            true,
            allowed_private_hosts
                .into_iter()
                .map(String::from)
                .collect(),
        )
        .expect("test tool construction should succeed")
    }

    #[test]
    fn validate_image_url_rejects_empty() {
        let tool = test_tool_with_private_hosts(vec![]);
        let err = tool.validate_image_url("").unwrap_err().to_string();
        assert!(err.contains("empty"), "got: {err}");
    }

    #[test]
    fn validate_image_url_rejects_whitespace() {
        let tool = test_tool_with_private_hosts(vec![]);
        let err = tool
            .validate_image_url("http://example .com/x.png")
            .unwrap_err()
            .to_string();
        assert!(err.contains("whitespace"), "got: {err}");
    }

    #[test]
    fn validate_image_url_rejects_non_http_scheme() {
        let tool = test_tool_with_private_hosts(vec![]);
        let err = tool
            .validate_image_url("ftp://example.com/x.png")
            .unwrap_err()
            .to_string();
        assert!(err.contains("http://"), "got: {err}");
    }

    #[test]
    fn validate_image_url_rejects_userinfo() {
        let tool = test_tool_with_private_hosts(vec![]);
        let err = tool
            .validate_image_url("https://attacker:pwn@cdn.example.com/x.png")
            .unwrap_err()
            .to_string();
        assert!(err.contains("userinfo"), "got: {err}");
    }

    #[test]
    fn validate_image_url_rejects_localhost() {
        let tool = test_tool_with_private_hosts(vec![]);
        let err = tool
            .validate_image_url("http://localhost/x.png")
            .unwrap_err()
            .to_string();
        assert!(err.contains("local/private"), "got: {err}");
    }

    #[test]
    fn validate_image_url_rejects_private_ipv4() {
        let tool = test_tool_with_private_hosts(vec![]);
        let err = tool
            .validate_image_url("http://10.0.0.5/x.png")
            .unwrap_err()
            .to_string();
        assert!(err.contains("local/private"), "got: {err}");
    }

    #[test]
    fn validate_image_url_rejects_cloud_metadata_ipv4() {
        // Even with `allowed_private_hosts = ["*"]`, cloud-metadata IPs
        // must still be rejected — the gate is unconditional for the
        // metadata service.
        let wildcard = test_tool_with_private_hosts(vec!["*"]);
        let err = wildcard
            .validate_image_url("http://169.254.169.254/latest/meta-data/")
            .unwrap_err()
            .to_string();
        assert!(err.contains("metadata"), "got: {err}");
    }

    #[test]
    fn validate_image_url_accepts_public_https() {
        let tool = test_tool_with_private_hosts(vec![]);
        let url = tool
            .validate_image_url("https://cdn.fal.ai/files/abc123.png")
            .expect("public HTTPS host must be accepted");
        assert_eq!(url, "https://cdn.fal.ai/files/abc123.png");
    }

    #[test]
    fn validate_image_url_accepts_allowed_private_host_explicit() {
        // Operator opted-in to a specific internal CDN via the allowlist.
        let tool = test_tool_with_private_hosts(vec!["cdn.internal.example"]);
        let url = tool
            .validate_image_url("https://cdn.internal.example/x.png")
            .expect("explicit allowed_private_hosts entry must lift the block");
        assert_eq!(url, "https://cdn.internal.example/x.png");
    }

    #[test]
    fn validate_image_url_accepts_allowed_private_host_wildcard() {
        // `*` blanket-tolerates any private/local host (dev/CI use case).
        let tool = test_tool_with_private_hosts(vec!["*"]);
        let url = tool
            .validate_image_url("http://localhost:8080/x.png")
            .expect("wildcard allowed_private_hosts must lift the block");
        assert_eq!(url, "http://localhost:8080/x.png");
    }

    // ── Resolved-IP gate tests ──────────────────────────────────────
    //
    // These exercise `validate_image_url_resolved` (instance method).
    // `localhost` resolution works via /etc/hosts even in network-free
    // environments; `example.com` requires real DNS.

    #[tokio::test]
    async fn validate_image_url_resolved_rejects_localhost() {
        let tool = test_tool_with_private_hosts(vec![]);
        let err = tool
            .validate_image_url_resolved("http://localhost/test.png")
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("non-global") || err.contains("Failed to resolve"),
            "expected non-global error, got: {err}"
        );
    }

    #[tokio::test]
    async fn validate_image_url_resolved_rejects_cloud_metadata_ipv4() {
        let tool = test_tool_with_private_hosts(vec!["*"]);
        let err = tool
            .validate_image_url_resolved("http://169.254.169.254/latest/meta-data/")
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("metadata"), "got: {err}");
    }

    #[tokio::test]
    async fn validate_image_url_resolved_accepts_public_host() {
        let tool = test_tool_with_private_hosts(vec![]);
        // example.com is a well-known public test domain (RFC 2606).
        tool.validate_image_url_resolved("https://example.com/image.png")
            .await
            .expect("public host must resolve to global IPs and pass");
    }

    #[tokio::test]
    async fn validate_image_url_resolved_accepts_private_host_with_wildcard_allowlist() {
        let tool = test_tool_with_private_hosts(vec!["*"]);
        tool.validate_image_url_resolved("http://localhost:8080/test.png")
            .await
            .expect("wildcard allowlist must lift the non-global check");
    }

    /// The resolved-IP validator returns the parsed hostname alongside the
    /// validated addresses. The caller passes the hostname (NOT the full
    /// URL) to `reqwest::Client::resolve_to_addrs` — reqwest lower-cases
    /// the lookup key, so a full URL would never match the request
    /// hostname and the validated address set would be silently ignored
    /// at connect time. This test pins the return-type contract so a
    /// future refactor can't accidentally widen the key back to the URL.
    #[tokio::test]
    async fn validate_image_url_resolved_returns_hostname_not_url() {
        let tool = test_tool_with_private_hosts(vec!["*"]);
        let (host, _addrs) = tool
            .validate_image_url_resolved("http://localhost:8080/test.png")
            .await
            .expect("wildcard allowlist must lift the non-global check");
        assert_eq!(
            host, "localhost",
            "host must be the bare hostname, not the full URL — \
             reqwest::Client::resolve_to_addrs keys by hostname"
        );
    }

    /// The download path passes one `ValidatedImageTarget` from the
    /// validation pipeline into the client builder, so the validated URL,
    /// the `resolve_to_addrs` lookup key, and the pinned addresses cannot
    /// be mixed at the call site. This exercises the real
    /// validation-to-bundle path and pins its contract: the URL stays
    /// whole, the host is the bare hostname reqwest keys by, and the
    /// addresses are the resolved set for that host.
    #[tokio::test]
    async fn validated_image_target_bundles_url_host_and_addrs() {
        let tool = test_tool_with_private_hosts(vec!["*"]);
        let target = tool
            .validate_image_target("http://localhost:8080/test.png")
            .await
            .expect("wildcard allowlist must lift the non-global check");
        assert_eq!(target.url, "http://localhost:8080/test.png");
        assert_eq!(
            target.host, "localhost",
            "host must be the bare hostname, not the full URL — \
             reqwest::Client::resolve_to_addrs keys by hostname"
        );
        assert!(
            !target.addrs.is_empty(),
            "validated target must carry the resolved address set"
        );
        assert!(
            target
                .addrs
                .iter()
                .all(|sa| sa.port() == 8080 && sa.ip().is_loopback()),
            "addrs must be the resolved set for the target host, got {:?}",
            target.addrs
        );
    }

    // ──────────────────────────────────────────────────────────────────
    // Transport-level tests: the download client's address pinning.
    //
    // These drive the same client construction the download path uses
    // (`ImageGenTool::build_download_client`) against a local listener.
    // The synthetic `.invalid` hostname (RFC 2606) cannot resolve via
    // ordinary DNS, so a request can only reach the listener when the
    // validated address override is present and keyed by the request
    // hostname. If the override is dropped or mis-keyed in the builder,
    // the connection never lands on the listener and the tests fail.
    // The assertions are on the listener's hit count, so the outcome
    // does not depend on how the environment resolves (or fails to
    // resolve) the synthetic hostname.
    // ──────────────────────────────────────────────────────────────────

    /// With the validated address override keyed by the request hostname,
    /// the connection lands on the validated `SocketAddr`: the synthetic
    /// hostname is unresolvable and the URL carries no port, so the
    /// override is the only path to the listener.
    #[tokio::test]
    async fn resolve_to_addrs_pins_connection_to_validated_socket_addr() {
        use std::net::SocketAddr;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let port = server.address().port();
        Mock::given(method("GET"))
            .and(path("/img.png"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"ok".to_vec()))
            .expect(1)
            .mount(&server)
            .await;

        let tool = test_tool_with_private_hosts(vec![]);
        let target = ValidatedImageTarget {
            url: "http://image-gen-resolve-test.invalid/img.png".to_string(),
            host: "image-gen-resolve-test.invalid".to_string(),
            addrs: vec![SocketAddr::from(([127, 0, 0, 1], port))],
        };
        let client = tool
            .build_download_client(&target)
            .expect("production client build must succeed");

        // No port in the URL: dialing must be driven entirely by the
        // validated address set, not by URL components or DNS.
        let resp = client.get(&target.url).send().await.expect(
            "the validated address override must make the \
                 unresolvable hostname reachable",
        );
        assert!(
            resp.status().is_success(),
            "expected 200 from the local listener, got {}",
            resp.status()
        );

        let received = server.received_requests().await.expect("received_requests");
        assert_eq!(received.len(), 1, "expected exactly one listener hit");
    }

    /// Control: the same production builder, but with the override keyed
    /// by a hostname that does NOT match the request URL — the mis-keyed
    /// regression class. The listener must see no request: the request
    /// hostname has no validated address set bound to it, so reaching
    /// `127.0.0.1:port` through this client is impossible regardless of
    /// how the environment resolves the synthetic name.
    #[tokio::test]
    async fn resolve_to_addrs_miskeyed_override_cannot_reach_listener() {
        use std::net::SocketAddr;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let port = server.address().port();
        Mock::given(method("GET"))
            .and(path("/img.png"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"ok".to_vec()))
            .expect(0)
            .mount(&server)
            .await;

        let tool = test_tool_with_private_hosts(vec![]);
        let target = ValidatedImageTarget {
            url: "http://image-gen-resolve-test.invalid/img.png".to_string(),
            host: "some-other-host.invalid".to_string(),
            addrs: vec![SocketAddr::from(([127, 0, 0, 1], port))],
        };
        let client = tool
            .build_download_client(&target)
            .expect("production client build must succeed");

        // The outcome of the send itself is environment-dependent (the
        // hostname may fail DNS or hit a captive portal); the authoritative
        // check is the listener's hit count.
        let _ = client.get(&target.url).send().await;

        let received = server.received_requests().await.expect("received_requests");
        assert_eq!(
            received.len(),
            0,
            "a mis-keyed override must not pin the connection: the request \
             hostname has no validated address set, so the listener must be \
             unreachable; got {received_len} hit(s)",
            received_len = received.len(),
        );
    }

    // ──────────────────────────────────────────────────────────────────
    // Redirect-loop tests: `download_image` follows redirects hop by hop,
    // re-running the full validate-resolve-pin pipeline for each target.
    //
    // Both listeners bind 127.0.0.1, so the tool is constructed with
    // `allowed_private_hosts = ["127.0.0.1"]` to lift the private-host
    // gate for the literal loopback address. The rejection cases use
    // redirect targets whose host string (`localhost`) is NOT covered by
    // that allowlist entry, so the per-hop gate must stop the loop before
    // any connection to the second listener.
    // ──────────────────────────────────────────────────────────────────

    /// A redirect to an allowed target is followed: the response bytes
    /// come from the second listener, and both listeners see exactly one
    /// request (the initial hit and the followed hop).
    #[tokio::test]
    async fn download_image_follows_redirect_to_allowed_target() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let target_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/img.png"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"image-bytes".to_vec()))
            .expect(1)
            .mount(&target_server)
            .await;

        let redirect_server = MockServer::start().await;
        let location = format!("{}/img.png", target_server.uri());
        Mock::given(method("GET"))
            .and(path("/start.png"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", location))
            .expect(1)
            .mount(&redirect_server)
            .await;

        let tool = test_tool_with_private_hosts(vec!["127.0.0.1"]);
        let resp = tool
            .download_image(&format!("{}/start.png", redirect_server.uri()))
            .await
            .expect("redirect to an allowlisted loopback target must succeed");
        assert!(resp.status().is_success(), "got {}", resp.status());
        let bytes = resp.bytes().await.expect("response body must read");
        assert_eq!(&bytes[..], b"image-bytes");

        let initial = redirect_server
            .received_requests()
            .await
            .expect("received_requests");
        let followed = target_server
            .received_requests()
            .await
            .expect("received_requests");
        assert_eq!(initial.len(), 1, "initial listener must see one request");
        assert_eq!(followed.len(), 1, "redirect target must see one request");
    }

    /// A relative `Location` is resolved against the current hop's URL and
    /// followed within the same listener.
    #[tokio::test]
    async fn download_image_resolves_relative_redirect_location() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/start.png"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/img.png"))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/img.png"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"image-bytes".to_vec()))
            .expect(1)
            .mount(&server)
            .await;

        let tool = test_tool_with_private_hosts(vec!["127.0.0.1"]);
        let resp = tool
            .download_image(&format!("{}/start.png", server.uri()))
            .await
            .expect("relative redirect on the same host must succeed");
        assert!(resp.status().is_success(), "got {}", resp.status());
        let bytes = resp.bytes().await.expect("response body must read");
        assert_eq!(&bytes[..], b"image-bytes");
    }

    /// The per-hop regression pin: a redirect target whose host string is
    /// private/local and NOT covered by the allowlist must be rejected
    /// before any connection to it. If the loop ever follows a redirect
    /// without re-validating the target, the second listener gets hit and
    /// this test fails.
    #[tokio::test]
    async fn download_image_rejects_redirect_to_non_allowlisted_private_host() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let target_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/img.png"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"image-bytes".to_vec()))
            .expect(0)
            .mount(&target_server)
            .await;

        let redirect_server = MockServer::start().await;
        // `localhost` resolves to the same loopback the allowlisted
        // `127.0.0.1` literal does, but the host STRING is not covered by
        // the allowlist — only per-hop re-validation catches it.
        let location = format!(
            "http://localhost:{}/img.png",
            target_server.address().port()
        );
        Mock::given(method("GET"))
            .and(path("/start.png"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", location))
            .expect(1)
            .mount(&redirect_server)
            .await;

        let tool = test_tool_with_private_hosts(vec!["127.0.0.1"]);
        let err = tool
            .download_image(&format!("{}/start.png", redirect_server.uri()))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("local/private"),
            "expected private-host rejection, got: {err}"
        );

        let followed = target_server
            .received_requests()
            .await
            .expect("received_requests");
        assert_eq!(
            followed.len(),
            0,
            "the rejected redirect target must never be connected to"
        );
    }

    /// A redirect chain that never terminates is cut off at the hop limit
    /// with the localized error.
    #[tokio::test]
    async fn download_image_enforces_redirect_hop_limit() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/loop.png"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/loop.png"))
            .mount(&server)
            .await;

        let tool = test_tool_with_private_hosts(vec!["127.0.0.1"]);
        let err = tool
            .download_image(&format!("{}/loop.png", server.uri()))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("Too many"),
            "expected hop-limit error, got: {err}"
        );

        let received = server.received_requests().await.expect("received_requests");
        assert_eq!(
            received.len(),
            MAX_REDIRECT_HOPS + 1,
            "expected the initial request plus {MAX_REDIRECT_HOPS} followed hops"
        );
    }

    /// A redirect response without a `Location` header is rejected with
    /// the localized error instead of being treated as a final response.
    #[tokio::test]
    async fn download_image_rejects_redirect_without_location() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/start.png"))
            .respond_with(ResponseTemplate::new(302))
            .expect(1)
            .mount(&server)
            .await;

        let tool = test_tool_with_private_hosts(vec!["127.0.0.1"]);
        let err = tool
            .download_image(&format!("{}/start.png", server.uri()))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("Location"),
            "expected missing-Location error, got: {err}"
        );
    }

    // ──────────────────────────────────────────────────────────────────
    // Resolver-injection tests: `download_image_with_resolver` exercises
    // the production download pipeline with controlled DNS resolution.
    //
    // Each test injects a closure that maps synthetic hostnames to
    // controlled `SocketAddr`s — no external DNS is performed. The
    // assertions anchor on wiremock listener hit counts, so the outcome
    // does not depend on how the environment resolves (or fails to
    // resolve) any hostname.
    // ──────────────────────────────────────────────────────────────────

    /// An unresolvable synthetic hostname reaches ONLY the validated
    /// listener when the injected resolver returns the listener's
    /// address.  If `download_image` ever bypasses `build_download_client`
    /// (or the `resolve_to_addrs` call is dropped), the unresolvable
    /// hostname has no other path to the listener and the request fails.
    #[tokio::test]
    async fn download_image_with_resolver_pins_initial_connection_to_validated_listener() {
        use std::net::SocketAddr;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let port = server.address().port();
        Mock::given(method("GET"))
            .and(path("/img.png"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"pinned-body".to_vec()))
            .expect(1)
            .mount(&server)
            .await;

        // Wildcard lifts the resolved-IP gate for the injected loopback
        // address so the pinning test can exercise the full pipeline.
        let tool = test_tool_with_private_hosts(vec!["*"]);
        let resp = tool
            .download_image_with_resolver(
                "http://download-test-synthetic.invalid/img.png",
                &|host, p| {
                    assert_eq!(host, "download-test-synthetic.invalid");
                    assert_eq!(p, 80);
                    async move { Ok(vec![SocketAddr::from(([127, 0, 0, 1], port))]) }
                },
            )
            .await
            .expect("download_image must succeed with injected resolver");

        assert!(resp.status().is_success());
        let body = resp.bytes().await.unwrap();
        assert_eq!(&body[..], b"pinned-body");

        let received = server.received_requests().await.unwrap();
        assert_eq!(
            received.len(),
            1,
            "exactly one request must reach the validated listener"
        );
    }

    /// A redirect target with a different synthetic hostname reaches ONLY
    /// its validated listener — the per-hop resolver is called for each
    /// target independently.  If the loop reuses the initial hop's address
    /// set for the redirect, the second synthetic hostname cannot reach
    /// its listener.
    #[tokio::test]
    async fn download_image_with_resolver_pins_redirected_connection_to_validated_listener() {
        use std::net::SocketAddr;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let redirect_server = MockServer::start().await;
        let redirect_port = redirect_server.address().port();
        let target_server = MockServer::start().await;
        let target_port = target_server.address().port();

        Mock::given(method("GET"))
            .and(path("/img.png"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"redirect-body".to_vec()))
            .expect(1)
            .mount(&target_server)
            .await;

        Mock::given(method("GET"))
            .and(path("/start.png"))
            .respond_with(ResponseTemplate::new(302).insert_header(
                "location",
                "http://download-test-redirect-target.invalid/img.png",
            ))
            .expect(1)
            .mount(&redirect_server)
            .await;

        let tool = test_tool_with_private_hosts(vec!["*"]);
        let resp = tool
            .download_image_with_resolver(
                "http://download-test-redirect.invalid/start.png",
                &|host, _p| async move {
                    match host.as_str() {
                        "download-test-redirect.invalid" => {
                            Ok(vec![SocketAddr::from(([127, 0, 0, 1], redirect_port))])
                        }
                        "download-test-redirect-target.invalid" => {
                            Ok(vec![SocketAddr::from(([127, 0, 0, 1], target_port))])
                        }
                        other => panic!("unexpected resolver host: {other}"),
                    }
                },
            )
            .await
            .expect("redirect with per-hop resolver must succeed");

        assert!(resp.status().is_success());
        let body = resp.bytes().await.unwrap();
        assert_eq!(&body[..], b"redirect-body");

        assert_eq!(redirect_server.received_requests().await.unwrap().len(), 1);
        assert_eq!(target_server.received_requests().await.unwrap().len(), 1);
    }

    /// A public-looking redirect hostname that the injected resolver maps
    /// to a loopback address is rejected by the resolved-IP gate with
    /// ZERO target-listener requests.  The host string looks legitimate
    /// (passes the host-string gate), but the resolved-IP classification
    /// catches the non-global address.  If resolved-IP classification is
    /// dropped, the redirect is followed onto the target listener and the
    /// `.expect(0)` assertion fails — this is the exact DNS-rebinding
    /// regression the prior `localhost` redirect test could not detect
    /// because literal `localhost` is stopped by the host-string gate.
    #[tokio::test]
    async fn download_image_rejects_redirect_to_public_hostname_resolving_to_private_address() {
        use std::net::SocketAddr;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let redirect_server = MockServer::start().await;
        let redirect_port = redirect_server.address().port();
        let target_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/img.png"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"must-not-reach".to_vec()))
            .expect(0) // ZERO requests — resolved-IP gate must block before connect
            .mount(&target_server)
            .await;

        Mock::given(method("GET"))
            .and(path("/start.png"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", "http://public-looking-cdn.example/img.png"),
            )
            .expect(1)
            .mount(&redirect_server)
            .await;

        // The initial synthetic host is the only allowed one.
        // "public-looking-cdn.example" is NOT in the allowlist — its host
        // string passes the host-string gate (it looks like a normal CDN
        // hostname), but the injected resolver returns a loopback address
        // that the resolved-IP gate must reject before any connection.
        let tool = test_tool_with_private_hosts(vec!["download-test-start.invalid"]);

        let err = tool
            .download_image_with_resolver(
                "http://download-test-start.invalid/start.png",
                &|host, _p| {
                    let target_port = target_server.address().port();
                    async move {
                        match host.as_str() {
                            "download-test-start.invalid" => {
                                Ok(vec![SocketAddr::from(([127, 0, 0, 1], redirect_port))])
                            }
                            "public-looking-cdn.example" => Ok(vec![SocketAddr::new(
                                std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)),
                                target_port,
                            )]),
                            other => panic!("unexpected resolver host: {other}"),
                        }
                    }
                },
            )
            .await
            .unwrap_err()
            .to_string();

        assert!(
            err.contains("non-global"),
            "expected resolved-IP 'non-global' rejection, got: {err}"
        );

        let followed = target_server.received_requests().await.unwrap();
        assert_eq!(
            followed.len(),
            0,
            "rejected redirect target must never be connected to"
        );
    }
}
