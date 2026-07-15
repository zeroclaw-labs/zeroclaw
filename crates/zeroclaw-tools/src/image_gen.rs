use anyhow::Context;
use async_trait::async_trait;
use serde_json::json;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult, with_ephemeral_workspace_warning};
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::policy::ToolOperation;

use crate::helpers::domain_guard;

/// Fluent lookup key for the `image_gen` tool's `description()` string.
/// Mirrors `TOOL_DESCRIPTION_KEY` in `file_download.rs:13`.
const TOOL_DESCRIPTION_KEY: &str = "tool-image-gen";
/// Cached localized description; initialized once on first `description()` call
/// so the `&'static str` return type is satisfied without re-running the
/// Fluent lookup on every invocation. Mirrors `TOOL_DESCRIPTION` in
/// `file_download.rs:14`.
static TOOL_DESCRIPTION: OnceLock<String> = OnceLock::new();

/// Resolve the output filename stem (no extension) for a generated image.
///
/// A caller-supplied `filename` is used verbatim with path components stripped
/// (traversal-safe). When none is given, a unique timestamped default
/// (`generated_image_<nanos>`) is returned so successive default generations
/// never clobber each other. `nanos` is injected so the selection is testable.
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

/// Format the tool output for a saved image.
///
/// Emits the saved path in BOTH a durable `File:` line (survives marker
/// stripping in older turns) and an explicit `[IMAGE:<path>]` marker the
/// multimodal pipeline inlines. Both carry the same path so the runtime
/// canonicalizer dedups them.
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

/// Standalone image generation tool using fal.ai (Flux / Nano Banana models).
///
/// Reads the API key from an environment variable (default: `FAL_API_KEY`),
/// calls the fal.ai synchronous endpoint, downloads the resulting image,
/// and saves it to `{workspace}/images/{filename}.png`.
pub struct ImageGenTool {
    security: Arc<SecurityPolicy>,
    workspace_dir: PathBuf,
    default_model: String,
    api_key_env: String,
    /// Normalized host allowlist (entries from `ImageGenConfig::allowed_private_hosts`).
    /// Empty by default. A bare `"*"` blanket-tolerates any private/local host;
    /// otherwise each entry is a host or suffix checked against the image-download
    /// host. Mirrors the same field on `[file_download]`, `[http_request]`,
    /// `[web_fetch]`, and the browser tools.
    allowed_private_hosts: Vec<String>,
    /// Whether the saved image persists on the host filesystem. `false` on an
    /// ephemeral runtime (Docker tmpfs / no volume mount), where the PNG is
    /// written inside the container but invisible on the host and discarded at
    /// session end. When `false`, a successful generation carries a loud
    /// ephemeral-workspace warning. Mirrors
    /// [`super::file_write::FileWriteTool`]. See issue #4627.
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
    /// `allowed_private_hosts`. Always rejects cloud-metadata IP literals
    /// even if `allowed_private_hosts` would otherwise lift the gate — that
    /// matches the matrix-textbrower-browser-file_download pattern (see
    /// `domain_guard::validate_resolved_ips_exclude_metadata`).
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

        // Cloud-metadata IP literals (169.254.169.254, fd00:ec2::254, etc.)
        // are rejected unconditionally — never a legitimate image source.
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
    /// `allowed_private_hosts`, only cloud metadata IPs are rejected
    /// (consistent with the allowlist semantics in the other tools).
    ///
    /// Resolve the validated image URL host to its socket addresses and reject
    /// private/local or cloud-metadata IPs. Returns the parsed hostname +
    /// validated socket addresses so the caller can bind the download
    /// connection to them via `resolve_to_addrs` (keyed by the hostname,
    /// NOT the full URL), closing the TOCTOU window between DNS check and
    /// transport connect.
    async fn validate_image_url_resolved(
        &self,
        validated_url: &str,
    ) -> anyhow::Result<(String, Vec<std::net::SocketAddr>)> {
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

        let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host, port))
            .await
            .context(Self::tool_msg(
                "tool-image-gen-error-resolved-url-resolve-failed",
            ))?
            .collect();

        if addrs.is_empty() {
            anyhow::bail!(Self::tool_msg_with_args(
                "tool-image-gen-error-resolved-url-resolve-empty",
                &[("host", host)],
            ));
        }

        let ips: Vec<std::net::IpAddr> = addrs.iter().map(|sa| sa.ip()).collect();

        let private_resolution_allowed =
            domain_guard::host_matches_allowlist(host, &self.allowed_private_hosts);

        if private_resolution_allowed {
            domain_guard::validate_resolved_ips_exclude_metadata(host, &ips)
        } else {
            domain_guard::validate_resolved_ips_are_public(host, &ips)
        }?;

        Ok((host.to_string(), addrs))
    }

    /// Build the image-download client: bind the validated address set to
    /// the connection via `resolve_to_addrs`, and re-validate each redirect
    /// target with the synchronous host-string gate.
    ///
    /// `resolve_to_addrs` keys by domain name (lower-cased), NOT the full
    /// URL — passing `https://cdn.example/image.png` would never match the
    /// request hostname `cdn.example` and the validated address set would
    /// be silently ignored at connect time, reopening the DNS-rebinding
    /// window. `resolved_host` must therefore be the bare hostname from
    /// `validate_image_url_resolved`. Mirrors http_request's reference
    /// impl at http_request.rs:363.
    ///
    /// Redirect resolved-IP validation is explicitly deferred: the reqwest
    /// redirect callback runs synchronously inside the async runtime, and
    /// nesting `Handle::block_on` there risks a panic. Redirect targets
    /// are still gated by the synchronous host-string check; cross-host
    /// DNS rebinding on redirect hops remains a documented residual risk.
    fn build_download_client(
        &self,
        resolved_host: &str,
        validated_addrs: &[std::net::SocketAddr],
    ) -> anyhow::Result<reqwest::Client> {
        // The closure captures a clone of `self.allowed_private_hosts` so
        // the per-redirect check uses the exact operator-configured
        // allowlist (no re-parse, no IO).
        let allowed_private_hosts = self.allowed_private_hosts.clone();
        let redirect_policy = reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= 10 {
                return attempt.error(std::io::Error::other(Self::tool_msg_with_args(
                    "tool-image-gen-error-redirect-limit",
                    &[("max", "10")],
                )));
            }
            // Host-string gate (sync). The helper's anyhow::Error Display
            // is the localized Fluent string verbatim; pass it through
            // unwrapped so the operator sees the translator's message.
            // `PermissionDenied` is preserved so any caller that
            // classifies the error by kind still works.
            if let Err(err) =
                validate_redirect_image_url(attempt.url().as_str(), &allowed_private_hosts)
            {
                return attempt.error(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    err.to_string(),
                ));
            }
            attempt.follow()
        });

        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .connect_timeout(std::time::Duration::from_secs(10))
            .redirect(redirect_policy)
            // The pinning above is only meaningful if THIS process dials
            // the validated addresses. reqwest honors proxy env vars by
            // default, and a proxy resolves and connects on our behalf —
            // the override would never apply and the proxy's DNS view
            // (which may include internal hosts) decides the destination,
            // reopening the rebinding window. Disable proxies so the
            // connection is always dialed directly to the validated set.
            .no_proxy()
            .resolve_to_addrs(resolved_host, validated_addrs)
            .build()
            .map_err(|e| {
                anyhow::Error::msg(Self::tool_msg_with_args(
                    "tool-image-gen-error-client-build",
                    &[("err", &e.to_string())],
                ))
            })
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

        // ── Validate image URL against the SSRF gate ──────────────
        // The image URL is server-supplied by fal.ai (and therefore not
        // trustable in the same way as the operator-configured fal.run
        // endpoint above). Validate the host string before any bytes hit
        // the network. The redirect policy on the download client below
        // re-validates each redirect target with the same gate — closes
        // the redirect-to-internal gap that `Policy::default()` would
        // leave open.
        let validated_image_url = self.validate_image_url(image_url)?;

        // Layer-2: resolved-IP check — the host string looks public, but
        // resolve it now to verify none of its addresses are private/local
        // or cloud metadata. A host covered by `allowed_private_hosts`
        // skips the non-global check but still rejects cloud metadata.
        // Returns the parsed hostname + validated socket addresses so they
        // can be bound to the download connection via `resolve_to_addrs`
        // (keyed by hostname, NOT the full URL), closing the TOCTOU window.
        let (resolved_host, validated_addrs) = self
            .validate_image_url_resolved(&validated_image_url)
            .await?;

        // ── Build image-download client ────────────────────────────
        // Binds the validated address set keyed by the parsed hostname
        // (NOT the full URL) and re-validates each redirect target with
        // the synchronous host-string gate — closes the redirect-to-
        // internal gap that `Policy::default()` would leave open.
        let download_client = self.build_download_client(&resolved_host, &validated_addrs)?;

        // ── Download image ─────────────────────────────────────────
        let img_resp = download_client
            .get(&validated_image_url)
            .send()
            .await
            .context("Failed to download generated image")?;

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

        // Emit a durable `File:` line (survives marker-stripping in older turns)
        // plus an explicit `[IMAGE:…]` marker the multimodal pipeline inlines.
        // Both carry the same path string so the promoter
        // (`canonicalize_tool_result_media_markers`) dedups the bare path
        // against the already-wrapped marker and does not double-count.
        let path_display = output_path.display().to_string();
        let output = format_image_tool_output(&path_display, size_kb, model, &prompt);

        Ok(ToolResult {
            success: true,
            output: output.into(),
            error: None,
        })
    }
}

/// Free-function companion to `ImageGenTool::validate_image_url`, used by the
/// reqwest redirect policy closure (which can't borrow `self`). Performs the
/// same gate — http(s)-only, no userinfo, no private/local host unless covered
/// by `allowed_private_hosts`, cloud-metadata IP literals always rejected.
fn validate_redirect_image_url(
    raw_url: &str,
    allowed_private_hosts: &[String],
) -> anyhow::Result<()> {
    let url = raw_url.trim();
    if url.is_empty() {
        anyhow::bail!(crate::i18n::get_required_tool_string(
            "tool-image-gen-error-redirect-url-empty"
        ));
    }
    if url.chars().any(char::is_whitespace) {
        anyhow::bail!(crate::i18n::get_required_tool_string(
            "tool-image-gen-error-redirect-url-whitespace"
        ));
    }
    if !url.starts_with("http://") && !url.starts_with("https://") {
        anyhow::bail!(crate::i18n::get_required_tool_string(
            "tool-image-gen-error-redirect-url-scheme"
        ));
    }

    let parsed = reqwest::Url::parse(url).map_err(|e| {
        anyhow::Error::msg(crate::i18n::get_required_tool_string_with_args(
            "tool-image-gen-error-redirect-url-parse",
            &[("err", &e.to_string())],
        ))
    })?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        anyhow::bail!(crate::i18n::get_required_tool_string(
            "tool-image-gen-error-redirect-url-userinfo"
        ));
    }

    let host = parsed
        .host_str()
        .ok_or_else(|| {
            anyhow::Error::msg(crate::i18n::get_required_tool_string(
                "tool-image-gen-error-redirect-url-no-host",
            ))
        })?
        .to_string();

    if host
        .parse::<IpAddr>()
        .is_ok_and(domain_guard::is_cloud_metadata_ip)
    {
        anyhow::bail!(crate::i18n::get_required_tool_string_with_args(
            "tool-image-gen-error-redirect-url-metadata-host",
            &[("host", &host)],
        ));
    }

    let host_is_private_or_local = domain_guard::is_private_or_local_host(&host);
    let private_match = host_is_private_or_local
        && domain_guard::host_matches_allowlist(&host, allowed_private_hosts);

    if host_is_private_or_local && !private_match {
        anyhow::bail!(crate::i18n::get_required_tool_string_with_args(
            "tool-image-gen-error-redirect-url-private-host",
            &[("host", &host)],
        ));
    }

    Ok(())
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
        // host and is lost at session end; warn loudly on success (issue #4627).
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
        // Exercises the PRODUCTION filename-selection helper (#7874): an omitted
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
        // Exercises the PRODUCTION output formatter (#7874): the saved path must
        // appear in BOTH the durable `File:` line and the `[IMAGE:<path>]`
        // marker, with the same concrete path, so the multimodal pipeline can
        // inline the attachment and the canonicalizer dedups them. Fails if the
        // marker (or the matching path) is dropped.
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
    // These exercise `ImageGenTool::validate_image_url` and the free-function
    // companion `validate_redirect_image_url` directly. Hermetic — no
    // network, no reqwest, no `fal.run` POST. A regression that drops the
    // SSRF gate (or relaxes the private-host check) would surface here.

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

    // ── Redirect-gate tests ───────────────────────────────────────
    //
    // The reqwest `Policy::custom` closure that re-validates each redirect
    // target uses `validate_redirect_image_url` (free function) — these tests
    // pin the redirect-path contract directly.

    #[test]
    fn validate_redirect_image_url_rejects_localhost() {
        let allowed = vec!["cdn.internal.example".to_string()];
        let err = validate_redirect_image_url("http://localhost/x.png", &allowed)
            .unwrap_err()
            .to_string();
        assert!(err.contains("local/private"), "got: {err}");
    }

    #[test]
    fn validate_redirect_image_url_rejects_cloud_metadata() {
        let allowed = vec!["*".to_string()];
        let err = validate_redirect_image_url("http://169.254.169.254/latest/meta-data/", &allowed)
            .unwrap_err()
            .to_string();
        assert!(err.contains("metadata"), "got: {err}");
    }

    #[test]
    fn validate_redirect_image_url_accepts_public_https() {
        let allowed: Vec<String> = vec![];
        validate_redirect_image_url("https://cdn.fal.ai/files/abc.png", &allowed)
            .expect("public HTTPS host must be accepted by redirect gate");
    }

    #[test]
    fn validate_redirect_image_url_accepts_allowed_private_host() {
        let allowed = vec!["cdn.internal.example".to_string()];
        validate_redirect_image_url("https://cdn.internal.example/x.png", &allowed)
            .expect("allowed_private_hosts must lift the redirect gate");
    }

    // ── Resolved-IP gate tests ──────────────────────────────────────
    //
    // These exercise `validate_image_url_resolved` (instance method).
    // `localhost` resolution works via /etc/hosts even in network-free
    // environments; `example.com` requires real DNS. Redirect resolved-IP
    // validation is currently deferred (the reqwest redirect callback runs
    // synchronously and cannot await DNS).

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

    /// Deterministic seam test: `validate_image_url_resolved` returns
    /// the parsed hostname alongside the validated addresses, and the
    /// download path passes the tuple's `.0` (hostname, not URL) to
    /// `reqwest::Client::resolve_to_addrs`. Together these two
    /// contracts guarantee that reqwest's lookup key matches the
    /// request hostname and the validated address set is selected at
    /// connect time, so a second unbound DNS lookup cannot bypass the
    /// SSRF gate. The transport-level behavior of the override itself
    /// is pinned by the listener-backed tests below.
    #[test]
    fn resolve_to_addrs_seam_uses_hostname_not_url() {
        let url = "http://localhost:8080/test.png";
        let host = reqwest::Url::parse(url)
            .expect("test URL must parse")
            .host_str()
            .expect("test URL must have a host")
            .to_string();
        assert_eq!(host, "localhost");
        assert!(
            !host.contains('/') && !host.contains(':'),
            "host must be the bare hostname (no scheme, no port, no path) — \
             reqwest::Client::resolve_to_addrs keys by hostname, and a full \
             URL or a host:port string would never match the request hostname"
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
        let validated_addrs = vec![SocketAddr::from(([127, 0, 0, 1], port))];
        let client = tool
            .build_download_client("image-gen-resolve-test.invalid", &validated_addrs)
            .expect("production client build must succeed");

        // No port in the URL: dialing must be driven entirely by the
        // validated address set, not by URL components or DNS.
        let resp = client
            .get("http://image-gen-resolve-test.invalid/img.png")
            .send()
            .await
            .expect(
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
        let validated_addrs = vec![SocketAddr::from(([127, 0, 0, 1], port))];
        let client = tool
            .build_download_client("some-other-host.invalid", &validated_addrs)
            .expect("production client build must succeed");

        // The outcome of the send itself is environment-dependent (the
        // hostname may fail DNS or hit a captive portal); the authoritative
        // check is the listener's hit count.
        let _ = client
            .get("http://image-gen-resolve-test.invalid/img.png")
            .send()
            .await;

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
}
