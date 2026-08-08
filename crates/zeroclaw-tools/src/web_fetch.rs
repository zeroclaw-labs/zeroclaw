use crate::helpers::domain_guard;
use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::schema::FirecrawlConfig;

/// Minimum body length to consider a standard fetch successful.
/// Bodies shorter than this are treated as JS-only pages that need Firecrawl.
const FIRECRAWL_MIN_BODY_LEN: usize = 100;

/// Size of *converted* text (bytes) above which a response is written to a
/// file in the agent workspace instead of being returned inline.
///
/// At or below this, behaviour is unchanged: the body comes back in the tool
/// result. Above it, half a megabyte of markdown in one tool result would
/// flood the model's context and the old hard-truncation dropped the tail
/// outright, so the full text is spilled to disk and the model is handed a
/// path it can read or search with the workspace file tools.
const SPILL_THRESHOLD_BYTES: usize = 50_000;

/// Directory, relative to the workspace root, that oversized responses are
/// spilled into. Kept as components rather than a `"tmp/web_fetch"` literal
/// so the join is separator-correct on every platform.
const SPILL_DIR_COMPONENTS: [&str; 2] = ["tmp", "web_fetch"];

/// Longest single line, in bytes, a spilled file may contain.
///
/// `file_read` pages by line and `content_search` reports by line, so a body
/// that is one enormous line — minified JSON, or an HTML-to-text conversion
/// that never emitted a break — is unpageable: the first page is the whole
/// file, which is exactly what spilling was meant to avoid. Any line longer
/// than this is split at a UTF-8 character boundary before the write. The
/// budget is a compromise: small enough that a page of lines is a useful slice
/// of the document, large enough that ordinary prose paragraphs and formatted
/// JSON lines are never touched.
const SPILL_MAX_LINE_BYTES: usize = 4_000;

/// The tool whose permission decides whether `web_fetch` may spill to disk.
///
/// Spilling is a durable filesystem write, but `web_fetch` is auto-approved by
/// default and is not classified as a writing tool, so nothing upstream asks
/// the operator about it. Rather than mint a second answer to "may this agent
/// write files" — the drift AGENTS.md bans — the spill defers to the policy's
/// existing answer for `file_write` via `SecurityPolicy::is_tool_allowed`,
/// which resolves the profile's `allowed_tools`/`excluded_tools` exactly as
/// the agent loop does when it decides whether to offer `file_write` at all.
/// A profile that denies file writes gets the inline truncation instead.
const SPILL_WRITE_TOOL: &str = "file_write";

/// Web fetch tool: fetches a web page and converts HTML to plain text for LLM consumption.
///
/// Unlike `http_request` (an API client returning raw responses), this tool:
/// - Only supports GET
/// - Follows redirects (up to 10)
/// - Converts HTML to clean plain text via `nanohtml2text`
/// - Passes through text/plain, text/markdown, and application/json as-is
/// - Sets a descriptive User-Agent
/// - Falls back to Firecrawl API when standard fetch fails (if enabled)
pub struct WebFetchTool {
    security: Arc<SecurityPolicy>,
    allowed_domains: Vec<String>,
    blocked_domains: Vec<String>,
    allowed_private_hosts: Vec<String>,
    max_response_size: usize,
    timeout_secs: u64,
    firecrawl: FirecrawlConfig,
}

impl WebFetchTool {
    pub fn new(
        security: Arc<SecurityPolicy>,
        allowed_domains: Vec<String>,
        blocked_domains: Vec<String>,
        max_response_size: usize,
        timeout_secs: u64,
        firecrawl: FirecrawlConfig,
        allowed_private_hosts: Vec<String>,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            security,
            allowed_domains: domain_guard::normalize_allowed_domains(
                allowed_domains,
                "web_fetch.allowed_domains",
            )?,
            blocked_domains: domain_guard::normalize_allowed_domains(
                blocked_domains,
                "web_fetch.blocked_domains",
            )?,
            allowed_private_hosts: domain_guard::normalize_allowed_domains(
                allowed_private_hosts,
                "web_fetch.allowed_private_hosts",
            )?,
            max_response_size,
            timeout_secs,
            firecrawl,
        })
    }

    fn validate_url(&self, raw_url: &str) -> anyhow::Result<String> {
        validate_target_url(
            raw_url,
            &self.allowed_domains,
            &self.blocked_domains,
            &self.allowed_private_hosts,
            "web_fetch",
        )
    }

    fn truncate_response(&self, text: &str) -> String {
        if self.max_response_size == 0 {
            return text.to_string();
        }
        if text.len() > self.max_response_size {
            let mut truncated = text
                .chars()
                .take(self.max_response_size)
                .collect::<String>();
            truncated.push_str("\n\n... [Response truncated due to size limit] ...");
            truncated
        } else {
            text.to_string()
        }
    }

    async fn read_response_text_limited(
        &self,
        response: reqwest::Response,
    ) -> anyhow::Result<CappedBody> {
        let mut bytes_stream = response.bytes_stream();
        let hard_cap = if self.max_response_size == 0 {
            usize::MAX
        } else {
            self.max_response_size.saturating_add(1)
        };
        let mut bytes = Vec::new();
        let mut cap_hit = false;

        while let Some(chunk_result) = bytes_stream.next().await {
            let chunk = chunk_result?;
            if append_chunk_with_cap(&mut bytes, &chunk, hard_cap) {
                cap_hit = true;
                break;
            }
        }

        Ok(CappedBody {
            text: String::from_utf8_lossy(&bytes).into_owned(),
            cap_hit,
        })
    }

    /// Turn fetched content into the tool result, whatever fetched it.
    ///
    /// This is the single delivery point for both the standard fetch and the
    /// Firecrawl fallback, so an oversized body spills from either source
    /// under identical rules. Below the threshold, or when spilling is not
    /// permitted or does not succeed, the pre-existing inline behaviour —
    /// `truncate_response` over the raw converted text — applies unchanged.
    async fn deliver(&self, url: &str, content: FetchedContent) -> ToolResult {
        if content.text.len() > SPILL_THRESHOLD_BYTES
            && let Some(message) = self.spill_to_workspace(url, &content).await
        {
            return ToolResult {
                success: true,
                output: message.into(),
                error: None,
            };
        }

        ToolResult {
            success: true,
            output: self.truncate_response(&content.text).into(),
            error: None,
        }
    }

    /// Write an oversized converted body to a file inside the agent workspace
    /// and return the short message that replaces it in the tool result.
    ///
    /// `None` means "no spill happened" — writes not permitted by policy, an
    /// unresolvable workspace, a destination that is not ours to write, or an
    /// I/O error. Callers fall back to the pre-existing inline truncation,
    /// which is degraded but never wrong.
    async fn spill_to_workspace(&self, url: &str, content: &FetchedContent) -> Option<String> {
        // A spill is a durable filesystem write performed by a tool the
        // operator auto-approved as a fetch. Ask the policy the same question
        // the agent loop asks before offering `file_write`, and stay inline
        // when the answer is no.
        if !self.security.is_tool_allowed(SPILL_WRITE_TOOL) {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({
                        "phase": "spill_to_workspace",
                        "gate": SPILL_WRITE_TOOL,
                    })),
                "web_fetch: security policy does not permit file writes, returning the \
                 oversized response inline instead of spilling it"
            );
            return None;
        }

        match self.try_spill_to_workspace(url, content).await {
            Ok((relative_path, format)) => Some(spill_message(
                url,
                content.title.as_deref(),
                content.text.len(),
                &relative_path,
                content.cap_hit,
                &format,
            )),
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({
                            "phase": "spill_to_workspace",
                            "error": format!("{}", e),
                        })),
                    "web_fetch: could not spill oversized response to the workspace, \
                     falling back to inline truncation"
                );
                None
            }
        }
    }

    /// Reshape the body, name it after its own bytes, and write it.
    ///
    /// Returns the workspace-*relative* path (what `file_read` and
    /// `content_search` expect) plus how the body was reshaped.
    async fn try_spill_to_workspace(
        &self,
        url: &str,
        content: &FetchedContent,
    ) -> anyhow::Result<(PathBuf, SpillFormat)> {
        // The workspace root has exactly one source of truth in this
        // codebase: `SecurityPolicy::workspace_dir`
        // (crates/zeroclaw-config/src/policy.rs). It is the same field
        // `SecurityPolicy::resolve_tool_path` and
        // `SecurityPolicy::is_resolved_path_allowed` use to root `file_read`
        // and `file_write`, and `WebFetchTool` already holds the policy. Do
        // not introduce a second resolution path here (no env var, no
        // `std::env::temp_dir()`, no constructor-threaded copy) — a divergent
        // second answer is exactly the drift bug AGENTS.md bans.
        let workspace = self.security.workspace_dir.clone();
        if workspace.as_os_str().is_empty() {
            anyhow::bail!("workspace_dir is empty");
        }

        let (body, format) = prepare_spill_body(&content.text, content.extension);
        let file_name = spill_file_name(url, &body, content.extension);

        // `spill_file_name` sanitizes to a single component, but prove it
        // rather than trust it. The write below is confined to a directory
        // handle regardless, so this is the second line of defence, not the
        // first.
        let mut components = Path::new(&file_name).components();
        let single_normal_component = matches!(components.next(), Some(Component::Normal(_)))
            && components.next().is_none()
            && !file_name.contains(['/', '\\']);
        if !single_normal_component {
            anyhow::bail!("spill file name is not a single path component");
        }

        let mut relative = PathBuf::new();
        for component in SPILL_DIR_COMPONENTS {
            relative.push(component);
        }
        relative.push(&file_name);

        // Blocking filesystem work off the async runtime. The handle-bound
        // write is synchronous by nature: its safety comes from never
        // re-resolving a pathname, which an async open/write pair cannot offer.
        let workspace_for_write = workspace.clone();
        let body_for_write = body;
        tokio::task::spawn_blocking(move || {
            write_spill_file(&workspace_for_write, &file_name, &body_for_write)
        })
        .await
        .map_err(|e| anyhow::Error::msg(format!("spill write task did not complete: {e}")))??;

        Ok((relative, format))
    }

    /// Whether a fetch attempt should trigger a Firecrawl fallback.
    ///
    /// Judged on the converted text as fetched, before any spill or
    /// truncation, so how a body is *delivered* can never be mistaken for how
    /// much of it there was.
    fn should_fallback_to_firecrawl(&self, attempt: &FetchAttempt) -> bool {
        if !self.firecrawl.enabled {
            return false;
        }
        match attempt {
            // Fallback on failure (HTTP error, network error, etc.)
            FetchAttempt::Failure(_) => true,
            // Fallback on empty or very short body (JS-only pages)
            FetchAttempt::Content(content) => content.text.trim().len() < FIRECRAWL_MIN_BODY_LEN,
        }
    }

    /// Fetch content via the Firecrawl API.
    async fn fetch_via_firecrawl(&self, url: &str) -> anyhow::Result<FetchAttempt> {
        let api_key = std::env::var(&self.firecrawl.api_key_env).map_err(|_| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "env_var": &self.firecrawl.api_key_env,
                    })),
                "web_fetch: Firecrawl API key missing from env"
            );
            anyhow::Error::msg(format!(
                "Firecrawl API key not found in environment variable '{}'",
                self.firecrawl.api_key_env
            ))
        })?;

        let endpoint = format!("{}/scrape", self.firecrawl.api_url.trim_end_matches('/'));

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "web_fetch: failed to build Firecrawl HTTP client"
                );
                anyhow::Error::msg(format!("Failed to build Firecrawl HTTP client: {e}"))
            })?;

        let body = json!({
            "url": url,
            "formats": ["markdown"]
        });

        let response = client
            .post(&endpoint)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "phase": "firecrawl_request",
                            "error": format!("{}", e),
                        })),
                    "web_fetch: Firecrawl request failed"
                );
                anyhow::Error::msg(format!("Firecrawl request failed: {e}"))
            })?;

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            return Ok(FetchAttempt::failure(format!(
                "Firecrawl API error: HTTP {} - {}",
                status.as_u16(),
                error_body
            )));
        }

        let resp_json: serde_json::Value = response.json().await.map_err(|e| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "phase": "firecrawl_response_parse",
                        "error": format!("{}", e),
                    })),
                "web_fetch: failed to parse Firecrawl response"
            );
            anyhow::Error::msg(format!("Failed to parse Firecrawl response: {e}"))
        })?;

        let markdown = resp_json
            .get("data")
            .and_then(|d| d.get("markdown"))
            .and_then(|m| m.as_str())
            .unwrap_or("");

        if markdown.is_empty() {
            return Ok(FetchAttempt::failure(
                "Firecrawl returned empty markdown content".to_string(),
            ));
        }

        // Handed back unshaped: delivery decides inline-vs-spill for Firecrawl
        // markdown on exactly the same terms as a standard fetch. Firecrawl
        // reads a parsed JSON field rather than the size-capped stream, so
        // `max_response_size` never cut it short.
        Ok(FetchAttempt::Content(FetchedContent {
            text: markdown.to_string(),
            extension: "md",
            title: None,
            cap_hit: false,
        }))
    }

    /// Perform the standard HTTP GET fetch and convert to text.
    async fn standard_fetch(&self, client: &reqwest::Client, url: &str) -> FetchAttempt {
        let response = match client.get(url).send().await {
            Ok(r) => r,
            Err(e) => {
                return FetchAttempt::failure(format!("HTTP request failed: {e}"));
            }
        };

        let status = response.status();
        if !status.is_success() {
            return FetchAttempt::failure(format!(
                "HTTP {} {}",
                status.as_u16(),
                status.canonical_reason().unwrap_or("Unknown")
            ));
        }

        // Determine content type for processing strategy
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_lowercase();

        let body_mode = if content_type.contains("text/html") || content_type.is_empty() {
            "html"
        } else if content_type.contains("text/plain")
            || content_type.contains("text/markdown")
            || content_type.contains("application/json")
        {
            "plain"
        } else {
            return FetchAttempt::failure(format!(
                "Unsupported content type: {content_type}. \
                 web_fetch supports text/html, text/plain, text/markdown, and application/json."
            ));
        };

        let CappedBody {
            text: body,
            cap_hit,
        } = match self.read_response_text_limited(response).await {
            Ok(t) => t,
            Err(e) => {
                return FetchAttempt::failure(format!("Failed to read response body: {e}"));
            }
        };

        // Keep the raw HTML alive alongside the converted text so a `<title>`
        // can be lifted from it *only* when the body is large enough to spill —
        // sub-threshold fetches pay nothing for it, and the raw markup is
        // dropped here rather than carried through fallback orchestration.
        let (text, raw_html) = if body_mode == "html" {
            (nanohtml2text::html2text(&body), Some(body))
        } else {
            (body, None)
        };
        let title = if text.len() > SPILL_THRESHOLD_BYTES {
            raw_html.as_deref().and_then(extract_html_title)
        } else {
            None
        };

        // Handed back unshaped. The stream cap above remains the absolute
        // guard on how much was read; whether this comes back inline or as a
        // spilled file is decided once, in `deliver`, after any Firecrawl
        // fallback has had its say.
        FetchAttempt::Content(FetchedContent {
            text,
            extension: spill_extension(&content_type, body_mode),
            title,
            cap_hit,
        })
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        "Fetch a web page and return its content as clean plain text. \
         HTML pages are automatically converted to readable text. \
         JSON and plain text responses are returned as-is. \
         Only GET requests; follows redirects. \
         Falls back to Firecrawl for JS-heavy/bot-blocked sites (if enabled). \
         Oversized responses are saved to a file in the agent workspace and the \
         path is returned instead of the text, when the security policy permits \
         file writes. \
         Security: allowlist-only domains, no local/private hosts."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The HTTP or HTTPS URL to fetch"
                }
            },
            "required": ["url"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let url = args.get("url").and_then(|v| v.as_str()).ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"param": "url"})),
                "web_fetch: missing url parameter"
            );
            anyhow::Error::msg("Missing 'url' parameter")
        })?;

        if !self.security.can_act() {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some("Action blocked: autonomy is read-only".into()),
            });
        }

        // Rate limiting is applied by the RateLimitedTool wrapper at
        // registration time (see zeroclaw-runtime::tools::mod).

        let url = match self.validate_url(url) {
            Ok(v) => v,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(e.to_string()),
                });
            }
        };

        // Build client: follow redirects, set timeout, set User-Agent
        let timeout_secs = if self.timeout_secs == 0 {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "web_fetch: timeout_secs is 0, using safe default of 30s"
            );
            30
        } else {
            self.timeout_secs
        };

        let allowed_domains = self.allowed_domains.clone();
        let blocked_domains = self.blocked_domains.clone();
        let allowed_private_hosts = self.allowed_private_hosts.clone();
        let redirect_policy = reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= 10 {
                return attempt.error(std::io::Error::other("Too many redirects (max 10)"));
            }

            if let Err(err) = validate_target_url(
                attempt.url().as_str(),
                &allowed_domains,
                &blocked_domains,
                &allowed_private_hosts,
                "web_fetch",
            ) {
                return attempt.error(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!("Blocked redirect target: {err}"),
                ));
            }

            attempt.follow()
        });

        let builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .connect_timeout(Duration::from_secs(10))
            .redirect(redirect_policy)
            .user_agent("ZeroClaw/0.1 (web_fetch)");
        let builder =
            zeroclaw_config::schema::apply_runtime_proxy_to_builder(builder, "tool.web_fetch");
        let client = match builder.build() {
            Ok(c) => c,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(format!("Failed to build HTTP client: {e}")),
                });
            }
        };

        let standard_attempt = self.standard_fetch(&client, &url).await;

        // If standard fetch succeeded well enough, use it directly.
        // Otherwise, try Firecrawl fallback if enabled.
        let attempt = if self.should_fallback_to_firecrawl(&standard_attempt) {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"url": url})),
                "web_fetch: standard fetch insufficient, attempting Firecrawl fallback"
            );
            match Box::pin(self.fetch_via_firecrawl(&url)).await {
                Ok(content @ FetchAttempt::Content(_)) => content,
                Ok(FetchAttempt::Failure(firecrawl_failure)) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                        &format!(
                            "web_fetch: Firecrawl fallback also failed: {:?}",
                            firecrawl_failure.error
                        )
                    );
                    // Return original standard result if Firecrawl also failed
                    standard_attempt
                }
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                        "web_fetch: Firecrawl fallback error"
                    );
                    standard_attempt
                }
            }
        } else {
            standard_attempt
        };

        // One delivery point for both sources: whichever attempt won, an
        // oversized body spills under the same rules and a small one comes
        // back inline.
        match attempt {
            FetchAttempt::Content(content) => Ok(self.deliver(&url, content).await),
            FetchAttempt::Failure(failure) => Ok(failure),
        }
    }
}

// ── Helper functions (independent from http_request.rs per DRY rule-of-three) ──

/// Result of the size-capped streamed body read.
///
/// Per-call value, not stored state: `cap_hit` is knowable only here, at the
/// moment the read stops, and nothing else in the codebase records it.
struct CappedBody {
    /// The bytes that were read, lossily decoded as UTF-8.
    text: String,
    /// True when the read stopped because `max_response_size` was reached,
    /// i.e. the source body was larger than the cap and its tail was dropped.
    cap_hit: bool,
}

/// Content one fetch attempt produced, before the inline-vs-spill decision.
///
/// Per-attempt value, not stored state: it lives only between a fetch and the
/// single delivery step, and exists so the standard fetch and the Firecrawl
/// fallback hand back the same shape and are delivered by the same code.
#[derive(Debug)]
struct FetchedContent {
    /// Converted text exactly as fetched — neither truncated nor reshaped.
    text: String,
    /// File extension a spilled copy would carry.
    extension: &'static str,
    /// Page title, when one could be lifted from the source markup. Only
    /// populated when the body is large enough that a spill could use it.
    title: Option<String>,
    /// True when `max_response_size` cut the read short, so `text` is the
    /// head of the source rather than all of it.
    cap_hit: bool,
}

/// Outcome of one fetch attempt.
#[derive(Debug)]
enum FetchAttempt {
    /// Content to deliver.
    Content(FetchedContent),
    /// A failed attempt, already shaped as the result to hand back.
    Failure(ToolResult),
}

impl FetchAttempt {
    fn failure(error: String) -> Self {
        FetchAttempt::Failure(ToolResult {
            success: false,
            output: ToolOutput::default(),
            error: Some(error),
        })
    }
}

/// How a spilled body was reshaped before it was written.
///
/// Reported to the model so it knows the saved file is not byte-for-byte what
/// the server served — line numbers and offsets refer to the reshaped form.
#[derive(Default)]
struct SpillFormat {
    /// JSON that parsed and was re-emitted indented.
    pretty_printed: bool,
    /// At least one line exceeded [`SPILL_MAX_LINE_BYTES`] and was split.
    wrapped: bool,
}

/// Reshape a body for spilling so the workspace file tools can navigate it.
///
/// Two transforms, both aimed at the same problem — `file_read` pages by line,
/// so a one-line file cannot be paged. JSON that parses is re-emitted indented;
/// then any line still over [`SPILL_MAX_LINE_BYTES`] is hard-wrapped. JSON that
/// does not parse is left alone rather than guessed at.
///
/// This runs only on the spill path. The inline result is never reshaped.
fn prepare_spill_body(text: &str, extension: &str) -> (String, SpillFormat) {
    let mut format = SpillFormat::default();

    let pretty = if extension == "json" {
        serde_json::from_str::<serde_json::Value>(text)
            .ok()
            .and_then(|value| serde_json::to_string_pretty(&value).ok())
    } else {
        None
    };
    let mut body = match pretty {
        Some(pretty) => {
            format.pretty_printed = true;
            pretty
        }
        None => text.to_string(),
    };

    if let Some(wrapped) = hard_wrap_long_lines(&body) {
        body = wrapped;
        format.wrapped = true;
    }

    (body, format)
}

/// Split every line longer than [`SPILL_MAX_LINE_BYTES`] at a UTF-8 character
/// boundary, or `None` when no line was over budget.
///
/// `None` rather than an unchanged copy so the caller can report whether the
/// saved file actually differs from the fetched text.
fn hard_wrap_long_lines(text: &str) -> Option<String> {
    if !text
        .split('\n')
        .any(|line| line.len() > SPILL_MAX_LINE_BYTES)
    {
        return None;
    }

    let mut out = String::with_capacity(text.len() + text.len() / SPILL_MAX_LINE_BYTES + 1);
    // `split('\n')` round-trips exactly when rejoined with '\n', including a
    // trailing newline (which yields a final empty segment).
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            out.push('\n');
        }
        let mut rest = line;
        while rest.len() > SPILL_MAX_LINE_BYTES {
            // Walk back to the nearest character boundary. No UTF-8 character
            // is wider than 4 bytes, so one always exists just inside the
            // budget; the fallback only guards a hypothetical budget narrower
            // than a single character, and terminates the loop rather than
            // splitting mid-character.
            let cut = (1..=SPILL_MAX_LINE_BYTES)
                .rev()
                .find(|&i| rest.is_char_boundary(i))
                .unwrap_or(rest.len());
            let (head, tail) = rest.split_at(cut);
            out.push_str(head);
            out.push('\n');
            rest = tail;
        }
        out.push_str(rest);
    }

    Some(out)
}

/// Write `body` as `file_name` under `<workspace>/tmp/web_fetch/` without ever
/// following a symlink, and without ever re-resolving a pathname.
///
/// Mirrors the hardened write in [`crate::embedded_resource`]: every operation
/// goes through a [`cap_std::fs::Dir`] handle opened once on the workspace root
/// and narrowed one component at a time, so a directory or symlink swapped in
/// at any component after the handle is opened cannot redirect the write out of
/// the workspace — closing a check/act window that a post-write `canonicalize`
/// could only detect after the bytes had already landed.
///
/// The file itself is opened `create_new`, which refuses rather than follows
/// whatever is already sitting at the destination. Because `file_name` is the
/// full digest of `body`, a *regular* file already at that name holds exactly
/// these bytes, so it is reused as-is; a symlink, directory, or special file is
/// not ours to write and the spill is abandoned so the caller falls back to
/// returning the response inline.
fn write_spill_file(workspace_dir: &Path, file_name: &str, body: &str) -> anyhow::Result<()> {
    use cap_std::ambient_authority;
    use cap_std::fs::{Dir, OpenOptions};
    use std::io::Write;

    let mut dir = Dir::open_ambient_dir(workspace_dir, ambient_authority())
        .map_err(|e| anyhow::Error::msg(format!("workspace root is not openable: {e}")))?;

    for component in SPILL_DIR_COMPONENTS {
        // `create_dir_all` and `open_dir` both resolve within the handle, but a
        // symlink that stays inside the workspace would still relocate the
        // spill somewhere the operator did not ask for. Refuse it outright.
        if dir
            .symlink_metadata(component)
            .is_ok_and(|meta| meta.is_symlink())
        {
            anyhow::bail!("spill directory component '{component}' is a symlink");
        }
        dir.create_dir_all(component)
            .map_err(|e| anyhow::Error::msg(format!("failed to create spill directory: {e}")))?;
        dir = dir
            .open_dir(component)
            .map_err(|e| anyhow::Error::msg(format!("failed to open spill directory: {e}")))?;
    }

    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    match dir.open_with(file_name, &options) {
        Ok(mut file) => file
            .write_all(body.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|e| anyhow::Error::msg(format!("failed to write spill file: {e}"))),
        Err(e) => {
            if dir
                .symlink_metadata(file_name)
                .is_ok_and(|meta| meta.is_file())
            {
                // Content-addressed: this exact name means these exact bytes.
                return Ok(());
            }
            Err(anyhow::Error::msg(format!(
                "refusing to write spill file: {e}"
            )))
        }
    }
}

/// File extension for a spilled body.
///
/// Derived from the content type the caller already classified — this is a
/// pure mapping for naming only and does not affect which content types
/// `web_fetch` accepts or how their bodies are processed.
fn spill_extension(content_type: &str, body_mode: &str) -> &'static str {
    if body_mode == "html" {
        // HTML arrives converted to readable text, which reads as markdown.
        return "md";
    }
    if content_type.contains("application/json") {
        return "json";
    }
    if content_type.contains("text/markdown") {
        return "md";
    }
    "txt"
}

/// Filename for a spilled response: sanitized URL host + the full SHA-256 of
/// the bytes that will be written.
///
/// The identity is the *whole* 256-bit digest, never a prefix: a 32-bit prefix
/// is cheap to collide, which would let one chosen body be served from another
/// body's path. The host is a legibility prefix only and carries no identity.
///
/// Two properties follow, and both are load-bearing. Deterministic: refetching
/// unchanged content resolves to the same file instead of accumulating copies.
/// Content-addressed over the *written* bytes, not the fetched ones: a file
/// already present at this name holds exactly this body, which is what lets the
/// no-follow `create_new` write in [`write_spill_file`] treat "already exists"
/// as success rather than overwriting anything.
fn spill_file_name(url: &str, body: &str, extension: &str) -> String {
    let host = extract_host(url)
        .map(|h| sanitize_path_component(&h))
        .unwrap_or_else(|_| "unknown-host".to_string());
    let digest = format!("{:x}", Sha256::digest(body.as_bytes()));
    format!("{host}-{digest}.{extension}")
}

/// Reduce a host to a single safe filename component.
///
/// ASCII alphanumerics, `-` and `.` survive; everything else (including any
/// path separator) becomes `-`. Leading and trailing dots are stripped so the
/// result can never be `.`, `..`, or a hidden file, and the length is capped
/// so host + hash + extension stays well inside filesystem name limits.
fn sanitize_path_component(host: &str) -> String {
    let mut sanitized: String = host
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    // Every char above is ASCII, so this byte index is always a char boundary.
    sanitized.truncate(60);
    let trimmed = sanitized.trim_matches('.');
    if trimmed.is_empty() {
        "unknown-host".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Cheap `<title>` extraction from raw HTML — a bounded substring scan, no
/// regex compile and no parse. Only used on the spill path.
fn extract_html_title(html: &str) -> Option<String> {
    /// A page whose `<title>` is not in the first 64 KiB does not have one
    /// worth paying for.
    const SCAN_LIMIT: usize = 64 * 1024;

    let mut end = html.len().min(SCAN_LIMIT);
    while end > 0 && !html.is_char_boundary(end) {
        end -= 1;
    }
    let head = &html[..end];

    // `to_ascii_lowercase` only maps A-Z, so it preserves byte length and
    // byte indices line up with `head` exactly.
    let lower = head.to_ascii_lowercase();
    let open = lower.find("<title")?;
    let tag_end = open + lower[open..].find('>')?;
    let close = tag_end + lower[tag_end..].find("</title>")?;
    if close < tag_end + 1 {
        return None;
    }

    let title = head[tag_end + 1..close]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if title.is_empty() {
        return None;
    }
    Some(title.chars().take(120).collect())
}

/// The short message that replaces an oversized body in the tool result.
///
/// Names the source, the size, and the saved path, and points at the tools
/// that can actually consume it (`file_read` for paging, `content_search` for
/// searching), whose paths resolve relative to the workspace root.
fn spill_message(
    url: &str,
    title: Option<&str>,
    byte_count: usize,
    relative_path: &Path,
    cap_hit: bool,
    format: &SpillFormat,
) -> String {
    let path = relative_path.display();
    let mut message = format!("Fetched {url}\n");
    if let Some(title) = title {
        message.push_str(&format!("Title: {title}\n"));
    }
    message.push_str(&format!(
        "Size: {byte_count} bytes of converted text — too large to return inline, so the \
         full text was written to a file in the agent workspace.\n\n\
         Saved to: {path}\n\n\
         Read it with the file_read tool (path=\"{path}\", using its offset and limit \
         parameters to page through), or search it with the content_search tool \
         (path=\"{path}\" plus a pattern). That path is relative to the workspace root.\n"
    ));
    if format.pretty_printed {
        message.push_str(
            "\nNote: the response was valid JSON and was saved pretty-printed, so the file is \
             indented rather than byte-identical to what the server served.\n",
        );
    }
    if format.wrapped {
        message.push_str(&format!(
            "\nNote: lines longer than {SPILL_MAX_LINE_BYTES} bytes were hard-wrapped so the file \
             can be paged by line. Line breaks that the source did not contain were added.\n"
        ));
    }
    if cap_hit {
        message.push_str(
            "\nNote: the response hit web_fetch's max_response_size stream cap, so the saved \
             content is the truncated head of the page, not the whole page.\n",
        );
    }
    message
}

fn validate_target_url(
    raw_url: &str,
    allowed_domains: &[String],
    blocked_domains: &[String],
    allowed_private_hosts: &[String],
    tool_name: &str,
) -> anyhow::Result<String> {
    validate_target_url_with_dns_check(
        raw_url,
        allowed_domains,
        blocked_domains,
        allowed_private_hosts,
        tool_name,
        validate_resolved_host_is_public,
    )
}

fn validate_target_url_with_dns_check(
    raw_url: &str,
    allowed_domains: &[String],
    blocked_domains: &[String],
    allowed_private_hosts: &[String],
    tool_name: &str,
    validate_dns: impl FnOnce(&str) -> anyhow::Result<()>,
) -> anyhow::Result<String> {
    let url = raw_url.trim();

    if url.is_empty() {
        anyhow::bail!("URL cannot be empty");
    }

    if url.chars().any(char::is_whitespace) {
        anyhow::bail!("URL cannot contain whitespace");
    }

    if !url.starts_with("http://") && !url.starts_with("https://") {
        anyhow::bail!("Only http:// and https:// URLs are allowed");
    }

    if allowed_domains.is_empty() {
        anyhow::bail!(
            "{tool_name} tool is enabled but no allowed_domains are configured. \
             Add [{tool_name}].allowed_domains in config.toml"
        );
    }

    let host = extract_host(url)?;

    // blocked_domains always takes precedence
    if domain_guard::host_matches_allowlist(&host, blocked_domains) {
        anyhow::bail!("Host '{host}' is in {tool_name}.blocked_domains");
    }

    let host_is_private_or_local = domain_guard::is_private_or_local_host(&host);
    let private_match = private_allowlist_match(&host, allowed_private_hosts);
    // An explicit entry (a specific host/IP or suffix) is a deliberate per-host
    // carve-out; the "*" wildcard blanket-tolerates a private/internal
    // resolution for any host. The distinction only affects the WARN below.
    let private_explicit = matches!(private_match, PrivateAllow::Explicit);
    // Either an explicit entry or "*" tolerates a private/internal host: it lifts
    // the literal private-host block and skips the resolved-IP public check.
    let private_tolerated = !matches!(private_match, PrivateAllow::None);

    if host_is_private_or_local && !private_tolerated {
        anyhow::bail!(
            "Blocked local/private host: {host}. \
             To allow this host, add it (or \"*\") to \
             {tool_name}.allowed_private_hosts in config.toml"
        );
    }

    if private_explicit || (private_tolerated && host_is_private_or_local) {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"tool_name": tool_name, "host": host})),
            "web_fetch: allowing host via allowed_private_hosts"
        );
    }

    let skip_allowed_domains = host_is_private_or_local && private_tolerated;

    if !skip_allowed_domains && !domain_guard::host_matches_allowlist(&host, allowed_domains) {
        anyhow::bail!("Host '{host}' is not in {tool_name}.allowed_domains");
    }

    // Skip the resolved-IP public check only when the host is covered by the
    // private allowlist (explicit OR "*"). This is what lets a domain that
    // resolves to a private IP through under allowed_private_hosts = ["*"].
    if !private_tolerated {
        validate_dns(&host)?;
    }

    Ok(url.to_string())
}

fn append_chunk_with_cap(buffer: &mut Vec<u8>, chunk: &[u8], hard_cap: usize) -> bool {
    if buffer.len() >= hard_cap {
        return true;
    }

    let remaining = hard_cap - buffer.len();
    if chunk.len() > remaining {
        buffer.extend_from_slice(&chunk[..remaining]);
        return true;
    }

    buffer.extend_from_slice(chunk);
    buffer.len() >= hard_cap
}

fn extract_host(url: &str) -> anyhow::Result<String> {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"url": url})),
                "web_fetch: non-http(s) URL rejected"
            );
            anyhow::Error::msg("Only http:// and https:// URLs are allowed")
        })?;

    let authority = rest.split(['/', '?', '#']).next().ok_or_else(|| {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({"url": url})),
            "web_fetch: invalid URL"
        );
        anyhow::Error::msg("Invalid URL")
    })?;

    if authority.is_empty() {
        anyhow::bail!("URL must include a host");
    }

    if authority.contains('@') {
        anyhow::bail!("URL userinfo is not allowed");
    }

    if authority.starts_with('[') {
        anyhow::bail!("IPv6 hosts are not supported in web_fetch");
    }

    let host = authority
        .split(':')
        .next()
        .unwrap_or_default()
        .trim()
        .trim_end_matches('.')
        .to_lowercase();

    if host.is_empty() {
        anyhow::bail!("URL must include a valid host");
    }

    Ok(host)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrivateAllow {
    /// Not covered by the private allowlist.
    None,
    /// Covered only by a `*` wildcard entry.
    Wildcard,
    /// Covered by a specific host/IP or suffix entry.
    Explicit,
}

fn private_allowlist_match(host: &str, allowed_private_hosts: &[String]) -> PrivateAllow {
    let mut wildcard = false;
    for entry in allowed_private_hosts {
        if entry == "*" {
            // Record the wildcard but keep scanning: a later explicit entry
            // should still win, since it is a deliberate per-host carve-out.
            wildcard = true;
        } else if domain_guard::host_matches_allowlist(host, std::slice::from_ref(entry)) {
            return PrivateAllow::Explicit;
        }
    }
    if wildcard {
        PrivateAllow::Wildcard
    } else {
        PrivateAllow::None
    }
}

#[cfg(not(test))]
fn validate_resolved_host_is_public(host: &str) -> anyhow::Result<()> {
    use std::net::ToSocketAddrs;

    let ips = (host, 0)
        .to_socket_addrs()
        .map_err(|e| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "host": host,
                        "error": format!("{}", e),
                    })),
                "web_fetch: failed to resolve host"
            );
            anyhow::Error::msg(format!("Failed to resolve host '{host}': {e}"))
        })?
        .map(|addr| addr.ip())
        .collect::<Vec<_>>();

    validate_resolved_ips_are_public(host, &ips)
}

#[cfg(test)]
fn validate_resolved_host_is_public(_host: &str) -> anyhow::Result<()> {
    // DNS checks are covered by validate_resolved_ips_are_public unit tests.
    Ok(())
}

fn validate_resolved_ips_are_public(host: &str, ips: &[std::net::IpAddr]) -> anyhow::Result<()> {
    if ips.is_empty() {
        anyhow::bail!("Failed to resolve host '{host}'");
    }

    for ip in ips {
        let non_global = match ip {
            std::net::IpAddr::V4(v4) => domain_guard::is_non_global_v4(*v4),
            std::net::IpAddr::V6(v6) => domain_guard::is_non_global_v6(*v6),
        };
        if non_global {
            anyhow::bail!(
                "Blocked host '{host}' resolved to non-global address {ip}. \
                 To allow hosts that resolve to private/internal IPs, add '{host}' \
                 (or \"*\") to web_fetch.allowed_private_hosts in config.toml"
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::autonomy::AutonomyLevel;
    use zeroclaw_config::policy::SecurityPolicy;
    use zeroclaw_config::schema::FirecrawlConfig;

    fn test_tool(allowed_domains: Vec<&str>) -> WebFetchTool {
        test_tool_with_blocklist(allowed_domains, vec![])
    }

    fn test_tool_with_blocklist(
        allowed_domains: Vec<&str>,
        blocked_domains: Vec<&str>,
    ) -> WebFetchTool {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            ..SecurityPolicy::default()
        });
        WebFetchTool::new(
            security,
            allowed_domains.into_iter().map(String::from).collect(),
            blocked_domains.into_iter().map(String::from).collect(),
            500_000,
            30,
            FirecrawlConfig::default(),
            vec![],
        )
        .unwrap()
    }

    fn test_tool_with_private_hosts(
        allowed_domains: Vec<&str>,
        blocked_domains: Vec<&str>,
        allowed_private_hosts: Vec<&str>,
    ) -> WebFetchTool {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            ..SecurityPolicy::default()
        });
        WebFetchTool::new(
            security,
            allowed_domains.into_iter().map(String::from).collect(),
            blocked_domains.into_iter().map(String::from).collect(),
            500_000,
            30,
            FirecrawlConfig::default(),
            allowed_private_hosts
                .into_iter()
                .map(String::from)
                .collect(),
        )
        .unwrap()
    }

    fn test_tool_with_firecrawl(firecrawl: FirecrawlConfig) -> WebFetchTool {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            ..SecurityPolicy::default()
        });
        WebFetchTool::new(
            security,
            vec!["*".into()],
            vec![],
            500_000,
            30,
            firecrawl,
            vec![],
        )
        .unwrap()
    }

    // ── Fetch-attempt helpers ────────────────────────────────────
    //
    // `standard_fetch` and `fetch_via_firecrawl` hand back a `FetchAttempt`
    // (content as fetched) rather than a finished `ToolResult`, so delivery —
    // inline or spilled — happens once, after fallback orchestration.

    /// The converted text of a successful attempt.
    fn attempt_text(attempt: &FetchAttempt) -> &str {
        match attempt {
            FetchAttempt::Content(content) => &content.text,
            FetchAttempt::Failure(result) => {
                panic!("expected fetched content, got failure: {:?}", result.error)
            }
        }
    }

    /// The error message of a failed attempt.
    fn attempt_error(attempt: &FetchAttempt) -> &str {
        match attempt {
            FetchAttempt::Failure(result) => result.error.as_deref().unwrap_or_default(),
            FetchAttempt::Content(_) => panic!("expected a failed attempt, got content"),
        }
    }

    /// An attempt carrying `text` as plain-text content.
    fn text_attempt(text: &str) -> FetchAttempt {
        FetchAttempt::Content(FetchedContent {
            text: text.to_string(),
            extension: "txt",
            title: None,
            cap_hit: false,
        })
    }

    // ── Name and schema ──────────────────────────────────────────

    #[test]
    fn name_is_web_fetch() {
        let tool = test_tool(vec!["example.com"]);
        assert_eq!(tool.name(), "web_fetch");
    }

    #[test]
    fn parameters_schema_requires_url() {
        let tool = test_tool(vec!["example.com"]);
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["url"].is_object());
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("url")));
    }

    // ── HTML to text conversion ──────────────────────────────────

    #[test]
    fn html_to_text_conversion() {
        let html = "<html><body><h1>Title</h1><p>Hello <b>world</b></p></body></html>";
        let text = nanohtml2text::html2text(html);
        assert!(text.contains("Title"));
        assert!(text.contains("Hello"));
        assert!(text.contains("world"));
        assert!(!text.contains("<h1>"));
        assert!(!text.contains("<p>"));
    }

    // ── URL validation ───────────────────────────────────────────

    #[test]
    fn validate_accepts_exact_domain() {
        let tool = test_tool(vec!["example.com"]);
        let got = tool.validate_url("https://example.com/page").unwrap();
        assert_eq!(got, "https://example.com/page");
    }

    #[test]
    fn validate_accepts_subdomain() {
        let tool = test_tool(vec!["example.com"]);
        assert!(tool.validate_url("https://docs.example.com/guide").is_ok());
    }

    #[test]
    fn validate_accepts_wildcard() {
        let tool = test_tool(vec!["*"]);
        assert!(tool.validate_url("https://news.ycombinator.com").is_ok());
    }

    #[test]
    fn validate_rejects_empty_url() {
        let tool = test_tool(vec!["example.com"]);
        let err = tool.validate_url("").unwrap_err().to_string();
        assert!(err.contains("empty"));
    }

    #[test]
    fn validate_rejects_missing_url() {
        let tool = test_tool(vec!["example.com"]);
        let err = tool.validate_url("  ").unwrap_err().to_string();
        assert!(err.contains("empty"));
    }

    #[test]
    fn validate_rejects_ftp_scheme() {
        let tool = test_tool(vec!["example.com"]);
        let err = tool
            .validate_url("ftp://example.com")
            .unwrap_err()
            .to_string();
        assert!(err.contains("http://") || err.contains("https://"));
    }

    #[test]
    fn validate_rejects_allowlist_miss() {
        let tool = test_tool(vec!["example.com"]);
        let err = tool
            .validate_url("https://google.com")
            .unwrap_err()
            .to_string();
        assert!(err.contains("allowed_domains"));
    }

    #[test]
    fn validate_requires_allowlist() {
        let security = Arc::new(SecurityPolicy::default());
        let tool = WebFetchTool::new(
            security,
            vec![],
            vec![],
            500_000,
            30,
            FirecrawlConfig::default(),
            vec![],
        )
        .unwrap();
        let err = tool
            .validate_url("https://example.com")
            .unwrap_err()
            .to_string();
        assert!(err.contains("allowed_domains"));
    }

    // ── SSRF protection ──────────────────────────────────────────

    #[test]
    fn ssrf_blocks_localhost() {
        let tool = test_tool(vec!["localhost"]);
        let err = tool
            .validate_url("https://localhost:8080")
            .unwrap_err()
            .to_string();
        assert!(err.contains("local/private"));
    }

    #[test]
    fn ssrf_blocks_private_ipv4() {
        let tool = test_tool(vec!["192.168.1.5"]);
        let err = tool
            .validate_url("https://192.168.1.5")
            .unwrap_err()
            .to_string();
        assert!(err.contains("local/private"));
    }

    #[test]
    fn ssrf_wildcard_still_blocks_private() {
        let tool = test_tool(vec!["*"]);
        let err = tool
            .validate_url("https://localhost:8080")
            .unwrap_err()
            .to_string();
        assert!(err.contains("local/private"));
    }

    #[test]
    fn redirect_target_validation_allows_permitted_host() {
        let allowed = vec!["example.com".to_string()];
        let blocked = vec![];
        assert!(
            validate_target_url(
                "https://docs.example.com/page",
                &allowed,
                &blocked,
                &[],
                "web_fetch"
            )
            .is_ok()
        );
    }

    #[test]
    fn redirect_target_validation_blocks_private_host() {
        let allowed = vec!["example.com".to_string()];
        let blocked = vec![];
        let err = validate_target_url(
            "https://127.0.0.1/admin",
            &allowed,
            &blocked,
            &[],
            "web_fetch",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("local/private"));
    }

    #[test]
    fn redirect_target_validation_blocks_blocklisted_host() {
        let allowed = vec!["*".to_string()];
        let blocked = vec!["evil.com".to_string()];
        let err = validate_target_url(
            "https://evil.com/phish",
            &allowed,
            &blocked,
            &[],
            "web_fetch",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("blocked_domains"));
    }

    // ── Security policy ──────────────────────────────────────────

    #[tokio::test]
    async fn blocks_readonly_mode() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let tool = WebFetchTool::new(
            security,
            vec!["example.com".into()],
            vec![],
            500_000,
            30,
            FirecrawlConfig::default(),
            vec![],
        )
        .unwrap();
        let result = tool
            .execute(json!({"url": "https://example.com"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("read-only"));
    }

    // ── Response truncation ──────────────────────────────────────

    #[test]
    fn truncate_within_limit() {
        let tool = test_tool(vec!["example.com"]);
        let text = "hello world";
        assert_eq!(tool.truncate_response(text), "hello world");
    }

    #[test]
    fn truncate_response_zero_means_unlimited() {
        // max_response_size == 0 must be treated as unlimited — no truncation
        // marker, full text returned regardless of length.
        let tool = WebFetchTool::new(
            Arc::new(SecurityPolicy::default()),
            vec!["example.com".into()],
            vec![],
            0, // unlimited
            30,
            FirecrawlConfig::default(),
            vec![],
        )
        .unwrap();
        let long_text = "x".repeat(10_000);
        let result = tool.truncate_response(&long_text);
        assert_eq!(result.len(), 10_000, "zero limit must not truncate");
        assert!(
            !result.contains("[Response truncated"),
            "must not append truncation marker"
        );
    }

    #[tokio::test]
    async fn standard_fetch_with_zero_limit_returns_full_body_and_skips_firecrawl_fallback() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let addr = server.address();

        // Body must exceed FIRECRAWL_MIN_BODY_LEN (100 bytes) so any
        // truncation to <100 bytes would (incorrectly) trigger fallback.
        let body = "a".repeat(500);
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body.clone()))
            .mount(&server)
            .await;

        let tool = WebFetchTool::new(
            Arc::new(SecurityPolicy {
                autonomy: AutonomyLevel::Supervised,
                ..SecurityPolicy::default()
            }),
            vec!["*".into()],
            vec![],
            0, // max_response_size = unlimited
            30,
            FirecrawlConfig {
                enabled: true,
                ..FirecrawlConfig::default()
            },
            vec![],
        )
        .unwrap();

        // Bypass SSRF-guarded execute() — call standard_fetch directly so
        // wiremock on 127.0.0.1 is reachable.
        let url = format!("http://{}:{}/", addr.ip(), addr.port());
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .expect("reqwest client");
        let standard_attempt = tool.standard_fetch(&client, &url).await;

        // (a) standard result IS the full body — proves streamed read did
        // not stop after 1 byte under the zero-limit path.
        assert_eq!(
            attempt_text(&standard_attempt).len(),
            body.len(),
            "streamed body length under zero-limit must equal full body"
        );
        assert_eq!(
            attempt_text(&standard_attempt),
            body,
            "streamed body content must equal full body"
        );

        // (b) result does NOT trip should_fallback_to_firecrawl — proves
        // the regression (1-byte short body) is locked out.
        assert!(
            !tool.should_fallback_to_firecrawl(&standard_attempt),
            "500-byte body under zero limit must not trigger Firecrawl fallback"
        );

        // (c) delivered inline with no truncation marker under the zero limit.
        let FetchAttempt::Content(content) = standard_attempt else {
            panic!("expected fetched content");
        };
        let delivered = tool.deliver(&url, content).await;
        assert!(delivered.success, "error={:?}", delivered.error);
        assert!(
            !delivered.output.contains("[Response truncated"),
            "must not append truncation marker under zero limit"
        );
    }

    #[test]
    fn truncate_over_limit() {
        let tool = WebFetchTool::new(
            Arc::new(SecurityPolicy::default()),
            vec!["example.com".into()],
            vec![],
            10,
            30,
            FirecrawlConfig::default(),
            vec![],
        )
        .unwrap();
        let text = "hello world this is long";
        let truncated = tool.truncate_response(text);
        assert!(truncated.contains("[Response truncated"));
    }

    // ── Domain normalization ─────────────────────────────────────
    // ── Blocked domains ──────────────────────────────────────────

    #[test]
    fn blocklist_rejects_exact_match() {
        let tool = test_tool_with_blocklist(vec!["*"], vec!["evil.com"]);
        let err = tool
            .validate_url("https://evil.com/page")
            .unwrap_err()
            .to_string();
        assert!(err.contains("blocked_domains"));
    }

    #[test]
    fn blocklist_rejects_subdomain() {
        let tool = test_tool_with_blocklist(vec!["*"], vec!["evil.com"]);
        let err = tool
            .validate_url("https://api.evil.com/v1")
            .unwrap_err()
            .to_string();
        assert!(err.contains("blocked_domains"));
    }

    #[test]
    fn blocklist_wins_over_allowlist() {
        let tool = test_tool_with_blocklist(vec!["evil.com"], vec!["evil.com"]);
        let err = tool
            .validate_url("https://evil.com")
            .unwrap_err()
            .to_string();
        assert!(err.contains("blocked_domains"));
    }

    #[test]
    fn blocklist_allows_non_blocked() {
        let tool = test_tool_with_blocklist(vec!["*"], vec!["evil.com"]);
        assert!(tool.validate_url("https://example.com").is_ok());
    }

    #[test]
    fn append_chunk_with_cap_truncates_and_stops() {
        let mut buffer = Vec::new();
        assert!(!append_chunk_with_cap(&mut buffer, b"hello", 8));
        assert!(append_chunk_with_cap(&mut buffer, b"world", 8));
        assert_eq!(buffer, b"hellowor");
    }

    #[test]
    fn resolved_private_ip_is_rejected() {
        let ips = vec!["127.0.0.1".parse().unwrap()];
        let err = validate_resolved_ips_are_public("example.com", &ips)
            .unwrap_err()
            .to_string();
        assert!(err.contains("non-global address"));
    }

    #[test]
    fn resolved_mixed_ips_are_rejected() {
        let ips = vec![
            "93.184.216.34".parse().unwrap(),
            "10.0.0.1".parse().unwrap(),
        ];
        let err = validate_resolved_ips_are_public("example.com", &ips)
            .unwrap_err()
            .to_string();
        assert!(err.contains("non-global address"));
    }

    #[test]
    fn resolved_public_ips_are_allowed() {
        let ips = vec!["93.184.216.34".parse().unwrap(), "1.1.1.1".parse().unwrap()];
        assert!(validate_resolved_ips_are_public("example.com", &ips).is_ok());
    }

    // ── Firecrawl config parsing ────────────────────────────────────

    #[test]
    fn firecrawl_config_defaults() {
        let cfg = FirecrawlConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.api_key_env, "FIRECRAWL_API_KEY");
        assert_eq!(cfg.api_url, "https://api.firecrawl.dev/v1");
        assert_eq!(cfg.mode, zeroclaw_config::schema::FirecrawlMode::Scrape);
    }

    #[test]
    fn firecrawl_config_deserializes_from_toml() {
        let toml_str = r#"
            enabled = true
            api_key_env = "MY_FC_KEY"
            api_url = "https://custom.firecrawl.io/v2"
            mode = "crawl"
        "#;
        let cfg: FirecrawlConfig = toml::from_str(toml_str).unwrap();
        assert!(cfg.enabled);
        assert_eq!(cfg.api_key_env, "MY_FC_KEY");
        assert_eq!(cfg.api_url, "https://custom.firecrawl.io/v2");
        assert_eq!(cfg.mode, zeroclaw_config::schema::FirecrawlMode::Crawl);
    }

    #[test]
    fn firecrawl_config_deserializes_defaults_from_empty_toml() {
        let cfg: FirecrawlConfig = toml::from_str("").unwrap();
        assert!(!cfg.enabled);
        assert_eq!(cfg.api_key_env, "FIRECRAWL_API_KEY");
    }

    #[test]
    fn web_fetch_config_with_firecrawl_section() {
        use zeroclaw_config::schema::WebFetchConfig;
        let toml_str = r#"
            enabled = true
            [firecrawl]
            enabled = true
            api_key_env = "FC_KEY"
        "#;
        let cfg: WebFetchConfig = toml::from_str(toml_str).unwrap();
        assert!(cfg.enabled);
        assert!(cfg.firecrawl.enabled);
        assert_eq!(cfg.firecrawl.api_key_env, "FC_KEY");
    }

    // ── Firecrawl fallback trigger conditions ───────────────────────

    #[test]
    fn fallback_disabled_when_firecrawl_not_enabled() {
        let tool = test_tool_with_firecrawl(FirecrawlConfig::default());
        let attempt = FetchAttempt::failure("HTTP 403 Forbidden".into());
        assert!(!tool.should_fallback_to_firecrawl(&attempt));
    }

    #[test]
    fn fallback_triggers_on_http_error() {
        let tool = test_tool_with_firecrawl(FirecrawlConfig {
            enabled: true,
            ..FirecrawlConfig::default()
        });
        let attempt = FetchAttempt::failure("HTTP 403 Forbidden".into());
        assert!(tool.should_fallback_to_firecrawl(&attempt));
    }

    #[test]
    fn fallback_triggers_on_empty_body() {
        let tool = test_tool_with_firecrawl(FirecrawlConfig {
            enabled: true,
            ..FirecrawlConfig::default()
        });
        assert!(tool.should_fallback_to_firecrawl(&text_attempt("")));
    }

    #[test]
    fn fallback_triggers_on_short_body() {
        let tool = test_tool_with_firecrawl(FirecrawlConfig {
            enabled: true,
            ..FirecrawlConfig::default()
        });
        // < 100 chars, JS-only page
        assert!(tool.should_fallback_to_firecrawl(&text_attempt("Loading...")));
    }

    #[test]
    fn fallback_skipped_on_good_response() {
        let tool = test_tool_with_firecrawl(FirecrawlConfig {
            enabled: true,
            ..FirecrawlConfig::default()
        });
        // well above 100 chars
        assert!(!tool.should_fallback_to_firecrawl(&text_attempt(&"A".repeat(200))));
    }

    // ── Firecrawl response parsing ──────────────────────────────────

    #[test]
    fn firecrawl_response_parses_markdown() {
        let response_json = json!({
            "success": true,
            "data": {
                "markdown": "# Hello World\n\nThis is extracted content from Firecrawl.",
                "metadata": {
                    "title": "Test Page"
                }
            }
        });
        let markdown = response_json
            .get("data")
            .and_then(|d| d.get("markdown"))
            .and_then(|m| m.as_str())
            .unwrap_or("");
        assert!(markdown.contains("Hello World"));
        assert!(markdown.contains("extracted content"));
    }

    #[test]
    fn firecrawl_response_handles_missing_markdown() {
        let response_json = json!({
            "success": true,
            "data": {}
        });
        let markdown = response_json
            .get("data")
            .and_then(|d| d.get("markdown"))
            .and_then(|m| m.as_str())
            .unwrap_or("");
        assert!(markdown.is_empty());
    }

    #[test]
    fn firecrawl_response_handles_missing_data() {
        let response_json = json!({
            "success": false,
            "error": "Rate limit exceeded"
        });
        let markdown = response_json
            .get("data")
            .and_then(|d| d.get("markdown"))
            .and_then(|m| m.as_str())
            .unwrap_or("");
        assert!(markdown.is_empty());
    }

    // ── Boundary test: FIRECRAWL_MIN_BODY_LEN (100 chars) ────────────

    #[test]
    fn fallback_triggers_at_exactly_99_chars() {
        let tool = test_tool_with_firecrawl(FirecrawlConfig {
            enabled: true,
            ..FirecrawlConfig::default()
        });
        assert!(
            tool.should_fallback_to_firecrawl(&text_attempt(&"A".repeat(99))),
            "99-char body (below threshold) should trigger fallback"
        );
    }

    #[test]
    fn fallback_skipped_at_exactly_100_chars() {
        let tool = test_tool_with_firecrawl(FirecrawlConfig {
            enabled: true,
            ..FirecrawlConfig::default()
        });
        assert!(
            !tool.should_fallback_to_firecrawl(&text_attempt(&"A".repeat(100))),
            "100-char body (at threshold) should NOT trigger fallback"
        );
    }

    // ── Item 1: missing API key env var falls back gracefully ─────────

    #[tokio::test]
    async fn firecrawl_missing_api_key_returns_error() {
        // Ensure the env var is unset for this test
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("FIRECRAWL_TEST_MISSING_KEY") };

        let tool = test_tool_with_firecrawl(FirecrawlConfig {
            enabled: true,
            api_key_env: "FIRECRAWL_TEST_MISSING_KEY".into(),
            ..FirecrawlConfig::default()
        });

        let result = tool.fetch_via_firecrawl("https://example.com").await;
        assert!(
            result.is_err(),
            "fetch_via_firecrawl should return Err when API key env var is missing"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("FIRECRAWL_TEST_MISSING_KEY"),
            "Error should mention the missing env var name, got: {err_msg}"
        );
    }

    // ── Item 2: double-failure returns original standard result ───────

    #[tokio::test]
    async fn execute_double_failure_returns_original_result() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let addr = server.address();

        // Standard fetch returns 403 (failure)
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        // Ensure Firecrawl API key env is missing so fallback also fails
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("FIRECRAWL_DOUBLE_FAIL_KEY") };

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            ..SecurityPolicy::default()
        });
        let tool = WebFetchTool::new(
            security,
            vec!["*".into()],
            vec![],
            500_000,
            30,
            FirecrawlConfig {
                enabled: true,
                api_key_env: "FIRECRAWL_DOUBLE_FAIL_KEY".into(),
                api_url: format!("http://{addr}"),
                ..FirecrawlConfig::default()
            },
            vec![],
        )
        .unwrap();

        // Bypass SSRF-guarded execute() — call standard_fetch + fallback
        // logic directly so wiremock on 127.0.0.1 is reachable.
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap();

        let url = format!("http://{addr}/page");
        let standard_attempt = tool.standard_fetch(&client, &url).await;

        // standard_fetch should fail with 403
        assert!(matches!(standard_attempt, FetchAttempt::Failure(_)));
        assert!(tool.should_fallback_to_firecrawl(&standard_attempt));

        // Firecrawl fallback should also fail (missing API key)
        let firecrawl_result = Box::pin(tool.fetch_via_firecrawl(&url)).await;
        assert!(
            match &firecrawl_result {
                Err(_) => true,
                Ok(attempt) => matches!(attempt, FetchAttempt::Failure(_)),
            },
            "Expected Firecrawl fallback to fail without API key"
        );

        // The orchestration should return the original 403 error
        let error = attempt_error(&standard_attempt);
        assert!(
            error.contains("403"),
            "Expected original HTTP 403 error, got: {error}"
        );
    }

    // ── Item 3: end-to-end fallback orchestration in execute() ───────

    #[tokio::test]
    async fn execute_falls_back_to_firecrawl_on_short_body() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // Standard-fetch server: returns a very short body (JS-only placeholder)
        let standard_server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("<html><body>Loading...</body></html>")
                    .insert_header("content-type", "text/html"),
            )
            .mount(&standard_server)
            .await;

        // Firecrawl server: returns rich markdown content
        let firecrawl_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/scrape"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": true,
                "data": {
                    "markdown": "# Real Content\n\nThis is the full page content extracted by Firecrawl, with enough text to be clearly above the minimum body length threshold."
                }
            })))
            .mount(&firecrawl_server)
            .await;

        // Set up API key env var for this test
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("FIRECRAWL_E2E_TEST_KEY", "test-key-12345") };

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            ..SecurityPolicy::default()
        });
        let standard_addr = standard_server.address();
        let firecrawl_addr = firecrawl_server.address();
        let tool = WebFetchTool::new(
            security,
            vec!["*".into()],
            vec![],
            500_000,
            30,
            FirecrawlConfig {
                enabled: true,
                api_key_env: "FIRECRAWL_E2E_TEST_KEY".into(),
                api_url: format!("http://{firecrawl_addr}"),
                ..FirecrawlConfig::default()
            },
            vec![],
        )
        .unwrap();

        // Bypass SSRF-guarded execute() — call standard_fetch + fallback
        // logic directly so wiremock on 127.0.0.1 is reachable.
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap();

        let url = format!("http://{standard_addr}/page");
        let standard_attempt = tool.standard_fetch(&client, &url).await;

        // Standard fetch returns short body, should trigger fallback
        assert!(tool.should_fallback_to_firecrawl(&standard_attempt));

        // Firecrawl fallback should succeed with rich content
        let attempt = Box::pin(tool.fetch_via_firecrawl(&url)).await.unwrap();
        let text = attempt_text(&attempt);
        assert!(
            text.contains("Real Content"),
            "Expected Firecrawl markdown content, got: {text}"
        );

        // Clean up env var
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("FIRECRAWL_E2E_TEST_KEY") };
    }

    // ── Allowed private hosts ─────────────────────────────────────

    #[test]
    fn allowed_private_host_bypasses_ssrf_block() {
        let tool = test_tool_with_private_hosts(vec!["*"], vec![], vec!["192.168.1.5"]);
        assert!(tool.validate_url("https://192.168.1.5/api").is_ok());
    }

    #[test]
    fn allowed_private_domain_skips_dns_public_check() {
        let allowed_domains = vec!["*".to_string()];
        let blocked_domains = vec![];
        let allowed_private_hosts = vec!["local.internal".to_string()];

        let result = validate_target_url_with_dns_check(
            "https://local.internal/api",
            &allowed_domains,
            &blocked_domains,
            &allowed_private_hosts,
            "web_fetch",
            |_| {
                panic!("DNS public-host validation should be skipped");
            },
        );

        assert!(
            result.is_ok(),
            "allowlisted private domain was rejected: {result:?}"
        );
    }

    #[test]
    fn private_wildcard_allows_domain_resolving_to_private_ip() {
        // allowed_private_hosts = ["*"] must permit a
        // regular domain that resolves to a private/internal IP, as long as the
        // name itself passes allowed_domains. The DNS public check must be
        // skipped (closure panics if reached).
        let allowed_domains = vec!["example.com".to_string()];
        let blocked_domains = vec![];
        let allowed_private_hosts = vec!["*".to_string()];

        let result = validate_target_url_with_dns_check(
            "https://internal.example.com/api",
            &allowed_domains,
            &blocked_domains,
            &allowed_private_hosts,
            "web_fetch",
            |_| panic!("DNS public-host validation should be skipped under private wildcard"),
        );

        assert!(
            result.is_ok(),
            "private wildcard should allow subdomain of allowed_domains: {result:?}"
        );
    }

    #[test]
    fn private_wildcard_allows_literal_private_ip_without_allowed_domains_entry() {
        // The "*" wildcard must keep its historical scope for *literal* private
        // hosts: an IP literal (or localhost/.local) is allowed even when it is
        // not listed in allowed_domains. Only ordinary domain names stay gated
        // on allowed_domains under "*".
        let allowed_domains = vec!["example.com".to_string()];
        let blocked_domains = vec![];
        let allowed_private_hosts = vec!["*".to_string()];

        let result = validate_target_url_with_dns_check(
            "https://10.0.0.1/api",
            &allowed_domains,
            &blocked_domains,
            &allowed_private_hosts,
            "web_fetch",
            |_| panic!("DNS public-host validation should be skipped for a literal private IP"),
        );

        assert!(
            result.is_ok(),
            "private wildcard should allow a literal private IP: {result:?}"
        );
    }

    #[test]
    fn private_allowlist_explicit_entry_must_pass_allowed_domains() {
        // An explicit (non-private) entry in allowed_private_hosts is NOT a free
        // pass: a non-private host still has to be in allowed_domains.
        let allowed_domains = vec!["example.com".to_string()];
        let blocked_domains = vec![];
        let allowed_private_hosts = vec!["unrelated.com".to_string()];

        let err = validate_target_url_with_dns_check(
            "https://unrelated.com/api",
            &allowed_domains,
            &blocked_domains,
            &allowed_private_hosts,
            "web_fetch",
            |_| anyhow::Ok(()),
        )
        .unwrap_err()
        .to_string();

        assert!(err.contains("allowed_domains"), "unexpected error: {err}");
    }

    #[test]
    fn private_wildcard_still_requires_allowed_domains() {
        // The "*" private wildcard must NOT widen the name allowlist: a public
        // domain that is not in allowed_domains stays blocked.
        let allowed_domains = vec!["example.com".to_string()];
        let blocked_domains = vec![];
        let allowed_private_hosts = vec!["*".to_string()];

        let err = validate_target_url_with_dns_check(
            "https://evil.com/api",
            &allowed_domains,
            &blocked_domains,
            &allowed_private_hosts,
            "web_fetch",
            |_| anyhow::Ok(()),
        )
        .unwrap_err()
        .to_string();

        assert!(err.contains("allowed_domains"), "unexpected error: {err}");
    }

    #[test]
    fn unallowed_domain_resolving_private_ip_still_blocked() {
        let allowed_domains = vec!["*".to_string()];
        let blocked_domains = vec![];
        let allowed_private_hosts = vec![];

        let err = validate_target_url_with_dns_check(
            "https://local.internal/api",
            &allowed_domains,
            &blocked_domains,
            &allowed_private_hosts,
            "web_fetch",
            |host| {
                validate_resolved_ips_are_public(
                    host,
                    &[std::net::IpAddr::V4(std::net::Ipv4Addr::new(
                        192, 168, 1, 5,
                    ))],
                )
            },
        )
        .unwrap_err()
        .to_string();

        assert!(
            err.contains("non-global address"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn private_allowlist_wildcard_does_not_allow_public_domain_miss() {
        let allowed_domains = vec!["example.com".to_string()];
        let blocked_domains = vec![];
        let allowed_private_hosts = vec!["*".to_string()];

        let err = validate_target_url_with_dns_check(
            "https://not-example.com/api",
            &allowed_domains,
            &blocked_domains,
            &allowed_private_hosts,
            "web_fetch",
            |_| anyhow::Ok(()),
        )
        .unwrap_err()
        .to_string();

        assert!(err.contains("allowed_domains"), "unexpected error: {err}");
    }

    #[test]
    fn blocklist_overrides_allowed_private_domain() {
        let allowed_domains = vec!["*".to_string()];
        let blocked_domains = vec!["local.internal".to_string()];
        let allowed_private_hosts = vec!["local.internal".to_string()];

        let err = validate_target_url_with_dns_check(
            "https://local.internal/api",
            &allowed_domains,
            &blocked_domains,
            &allowed_private_hosts,
            "web_fetch",
            |_| anyhow::bail!("blocklist should run before DNS validation"),
        )
        .unwrap_err()
        .to_string();

        assert!(err.contains("blocked_domains"), "unexpected error: {err}");
    }

    #[test]
    fn unallowed_private_host_still_blocked() {
        let tool = test_tool_with_private_hosts(vec!["*"], vec![], vec!["192.168.1.5"]);
        let err = tool
            .validate_url("https://10.0.0.1/admin")
            .unwrap_err()
            .to_string();
        assert!(err.contains("local/private"));
        assert!(err.contains("allowed_private_hosts"));
    }

    #[test]
    fn blocklist_overrides_allowed_private_host() {
        let tool =
            test_tool_with_private_hosts(vec!["*"], vec!["192.168.1.5"], vec!["192.168.1.5"]);
        let err = tool
            .validate_url("https://192.168.1.5/secret")
            .unwrap_err()
            .to_string();
        assert!(err.contains("blocked_domains"));
    }

    #[test]
    fn allowed_private_host_with_port() {
        let tool = test_tool_with_private_hosts(vec!["*"], vec![], vec!["192.168.1.5"]);
        assert!(tool.validate_url("https://192.168.1.5:8080/api").is_ok());
    }

    // ── Spill-to-workspace-file for oversized responses ───────────
    //
    // These drive `standard_fetch` through wiremock so the whole
    // read → convert → spill path is exercised, and root the tool at a
    // throwaway workspace so no test can write into the repo.

    /// A tool whose `SecurityPolicy.workspace_dir` — the canonical
    /// workspace root, same field `file_read`/`file_write` resolve
    /// against — points at a throwaway directory.
    fn spill_test_tool(workspace: &std::path::Path, max_response_size: usize) -> WebFetchTool {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: workspace.to_path_buf(),
            ..SecurityPolicy::default()
        });
        WebFetchTool::new(
            security,
            vec!["*".into()],
            vec![],
            max_response_size,
            30,
            FirecrawlConfig::default(),
            vec![],
        )
        .unwrap()
    }

    /// Serve `body` as `content_type` and run `standard_fetch` against it.
    async fn fetch_body(tool: &WebFetchTool, body: &str, content_type: &str) -> ToolResult {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        // `set_body_raw` takes the content type explicitly. `set_body_string`
        // + `insert_header("content-type", ...)` does NOT override it —
        // the body helper's `text/plain` wins, which silently routes every
        // such test down the plain-text branch.
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body.as_bytes(), content_type))
            .mount(&server)
            .await;

        // Bypass SSRF-guarded execute() — call standard_fetch directly so
        // wiremock on 127.0.0.1 is reachable, then run the same delivery step
        // execute() runs, which is where inline-vs-spill is decided.
        let url = format!("http://{}/page", server.address());
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("reqwest client");
        match tool.standard_fetch(&client, &url).await {
            FetchAttempt::Content(content) => tool.deliver(&url, content).await,
            FetchAttempt::Failure(result) => result,
        }
    }

    /// Pins the pre-existing inline behaviour: a body comfortably under the
    /// spill threshold is returned verbatim and nothing is written to disk.
    #[tokio::test]
    async fn body_below_spill_threshold_is_returned_inline() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let tool = spill_test_tool(workspace.path(), 500_000);
        let body = "b".repeat(1_000);

        let result = fetch_body(&tool, &body, "text/plain").await;

        assert!(result.success, "error={:?}", result.error);
        assert_eq!(
            result.output.as_str(),
            body,
            "sub-threshold body must be returned inline, unchanged"
        );
        assert!(
            !workspace.path().join("tmp").exists(),
            "sub-threshold fetch must not create the spill directory"
        );
    }

    /// Boundary: exactly `SPILL_THRESHOLD_BYTES` still returns inline.
    /// Only *above* the threshold spills.
    #[tokio::test]
    async fn body_at_exact_spill_threshold_is_returned_inline() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let tool = spill_test_tool(workspace.path(), 500_000);
        let body = "c".repeat(SPILL_THRESHOLD_BYTES);

        let result = fetch_body(&tool, &body, "text/plain").await;

        assert!(result.success, "error={:?}", result.error);
        assert_eq!(
            result.output.as_str().len(),
            SPILL_THRESHOLD_BYTES,
            "a body of exactly the threshold must be returned inline"
        );
        assert!(
            !workspace.path().join("tmp").exists(),
            "threshold-sized fetch must not create the spill directory"
        );
    }

    /// Pull the `Saved to: <path>` line out of a spill message, with separators
    /// normalized to `/`.
    ///
    /// The message renders the path with `Path::display()`, which uses the
    /// platform separator — backslashes on Windows. Comparing components rather
    /// than the raw string keeps these assertions meaningful on every platform,
    /// and `/`-joined paths still `join()` correctly on Windows.
    fn saved_path(message: &str) -> String {
        let raw = message
            .lines()
            .find_map(|line| line.strip_prefix("Saved to: "))
            .unwrap_or_else(|| panic!("no 'Saved to:' line in message:\n{message}"))
            .trim();
        std::path::Path::new(raw)
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/")
    }

    #[tokio::test]
    async fn body_above_spill_threshold_is_written_to_a_workspace_file() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let tool = spill_test_tool(workspace.path(), 500_000);
        let body = "d".repeat(60_000);

        let result = fetch_body(&tool, &body, "text/plain").await;

        assert!(result.success, "error={:?}", result.error);
        let message = result.output.as_str();

        // The message replaces the body: short, and not the payload itself.
        assert!(
            message.len() < 1_000,
            "spill message should be short, got {} bytes",
            message.len()
        );
        assert!(
            !message.contains(&"d".repeat(1_000)),
            "spill message must not carry the body inline"
        );
        assert!(
            message.contains("60000 bytes"),
            "message must state the byte count, got:\n{message}"
        );
        assert!(
            message.contains("file_read") && message.contains("content_search"),
            "message must point at the workspace file tools, got:\n{message}"
        );
        assert!(
            !message.contains("max_response_size"),
            "no stream-cap note expected when the cap did not fire, got:\n{message}"
        );

        // The file holds the FULL converted text, with no truncation marker.
        let relative = saved_path(message);
        assert!(
            relative.starts_with("tmp/web_fetch/") && relative.ends_with(".txt"),
            "unexpected spill path: {relative}"
        );
        let written = std::fs::read_to_string(workspace.path().join(&relative))
            .expect("spill file must exist at the advertised path");
        // A 60 KB single-line body is hard-wrapped on the way out (see
        // SPILL_MAX_LINE_BYTES), so the file differs from the body only by the
        // inserted line breaks — no character of the response is lost.
        assert_eq!(
            written.replace('\n', ""),
            body,
            "spill file must hold the full converted text"
        );
        assert!(
            written
                .lines()
                .all(|line| line.len() <= SPILL_MAX_LINE_BYTES),
            "spilled file must not contain an unpageable line"
        );
        assert!(
            !written.contains("[Response truncated"),
            "spilled file must not carry the inline truncation marker"
        );
    }

    /// The load-bearing safety property: the write lands inside the workspace
    /// root resolved from `SecurityPolicy::workspace_dir`, and nowhere else.
    #[tokio::test]
    async fn spilled_file_stays_inside_the_workspace_root() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let tool = spill_test_tool(workspace.path(), 500_000);
        let body = "e".repeat(60_000);

        let result = fetch_body(&tool, &body, "text/plain").await;
        let relative = saved_path(result.output.as_str());

        let root = workspace
            .path()
            .canonicalize()
            .expect("canonical workspace");
        let written = workspace
            .path()
            .join(&relative)
            .canonicalize()
            .expect("spill file must exist");

        assert!(
            written.starts_with(&root),
            "spill file {} escaped workspace root {}",
            written.display(),
            root.display()
        );
        assert!(
            !std::path::Path::new(&relative).is_absolute(),
            "advertised path must be workspace-relative, got {relative}"
        );
        assert!(
            !relative.contains(".."),
            "advertised path must not contain parent traversal, got {relative}"
        );
    }

    /// The containment guard, exercised for real: a symlinked `tmp/` planted
    /// inside the workspace points at an outside directory. `create_dir_all`
    /// follows it, so the post-creation canonicalize + `starts_with` check is
    /// the only thing standing between the fetch and a write outside the
    /// sandbox. No page content may land outside the workspace root.
    #[cfg(unix)]
    #[tokio::test]
    async fn spill_refuses_to_write_through_a_symlink_out_of_the_workspace() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let outside = tempfile::tempdir().expect("outside tempdir");
        std::os::unix::fs::symlink(outside.path(), workspace.path().join("tmp"))
            .expect("plant symlink");

        let tool = spill_test_tool(workspace.path(), 500_000);
        let body = "i".repeat(60_000);

        let result = fetch_body(&tool, &body, "text/plain").await;

        // Falls back to the inline path rather than writing outside.
        assert!(result.success, "error={:?}", result.error);
        assert_eq!(
            result.output.as_str(),
            body,
            "a blocked spill must fall back to the inline body"
        );

        // Nothing — no page content at all — may exist outside the workspace.
        let escaped: Vec<_> = walk_files(outside.path());
        assert!(
            escaped.is_empty(),
            "page content escaped the workspace to {escaped:?}"
        );
    }

    /// The directory guard, isolated from the sandbox handle.
    ///
    /// The handle rejects a symlink whose target is absolute or climbs out of
    /// the workspace, so those cases prove nothing about this guard. A
    /// *relative* `tmp -> decoy-dir` link resolves entirely inside the
    /// workspace and the handle follows it happily — only the explicit
    /// `is_symlink` refusal stops the spill being quietly relocated.
    #[cfg(unix)]
    #[tokio::test]
    async fn spill_refuses_a_spill_directory_symlinked_inside_the_workspace() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let decoy = workspace.path().join("decoy-dir");
        std::fs::create_dir(&decoy).expect("decoy dir");
        // Relative target, sibling of the link: stays inside the sandbox.
        std::os::unix::fs::symlink("decoy-dir", workspace.path().join("tmp"))
            .expect("plant symlink");

        let tool = spill_test_tool(workspace.path(), 500_000);
        let body = "r".repeat(60_000);

        let result = fetch_body(&tool, &body, "text/plain").await;

        assert!(result.success, "error={:?}", result.error);
        assert_eq!(
            result.output.as_str(),
            body,
            "a refused spill must fall back to the inline body"
        );
        let relocated = walk_files(&decoy);
        assert!(
            relocated.is_empty(),
            "spill was relocated through the symlink to {relocated:?}"
        );
    }

    /// The leaf guard, isolated from the sandbox handle.
    ///
    /// Same reasoning as the directory case: the handle already refuses an
    /// absolute or escaping link target, so only a *relative* link to a
    /// sibling inside the spill directory tests `create_new` itself. Without
    /// it the write would follow the link and land on the decoy.
    #[cfg(unix)]
    #[tokio::test]
    async fn spill_abandons_a_leaf_symlink_pointing_inside_the_spill_directory() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let tool = spill_test_tool(workspace.path(), 500_000);
        let body = "s".repeat(60_000);

        // Spill once to learn the content-addressed destination.
        let first = fetch_body(&tool, &body, "text/plain").await;
        let relative = saved_path(first.output.as_str());
        let destination = workspace.path().join(&relative);
        std::fs::remove_file(&destination).expect("clear the spilled file");

        let decoy = destination
            .parent()
            .expect("spill directory")
            .join("decoy.txt");
        std::os::unix::fs::symlink("decoy.txt", &destination).expect("plant leaf symlink");

        let second = fetch_body(&tool, &body, "text/plain").await;

        assert!(second.success, "error={:?}", second.error);
        assert_eq!(
            second.output.as_str(),
            body,
            "a refused spill must fall back to the inline body"
        );
        assert!(
            !decoy.exists(),
            "wrote through the symlink to {}",
            decoy.display()
        );
        assert!(
            std::fs::symlink_metadata(&destination)
                .expect("destination must still exist")
                .is_symlink(),
            "the planted symlink must be left untouched, not overwritten"
        );
    }

    /// Every regular file under `dir`, recursively.
    #[cfg(unix)]
    fn walk_files(dir: &std::path::Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return found;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                found.extend(walk_files(&path));
            } else {
                found.push(path);
            }
        }
        found
    }

    #[tokio::test]
    async fn spill_uses_markdown_extension_for_converted_html() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let tool = spill_test_tool(workspace.path(), 500_000);
        let body = format!(
            "<html><head><title>  Big \n Page  </title></head><body><p>{}</p></body></html>",
            "word ".repeat(15_000)
        );

        let result = fetch_body(&tool, &body, "text/html").await;

        assert!(result.success, "error={:?}", result.error);
        let message = result.output.as_str();
        let relative = saved_path(message);
        assert!(
            relative.ends_with(".md"),
            "converted HTML must spill as .md, got {relative}"
        );
        // Title is lifted from the raw HTML and whitespace-collapsed.
        assert!(
            message.contains("Title: Big Page"),
            "message must carry the page title, got:\n{message}"
        );
        // The file holds converted text, not the source markup.
        let written =
            std::fs::read_to_string(workspace.path().join(&relative)).expect("spill file");
        assert!(
            !written.contains("<p>"),
            "spilled HTML must be stored converted, not raw"
        );
        assert!(written.contains("word"));
    }

    /// Stream-cap interaction: `max_response_size` stays the absolute guard,
    /// and when it fires the message says the saved content is incomplete.
    #[tokio::test]
    async fn spill_message_flags_stream_cap_truncation() {
        let workspace = tempfile::tempdir().expect("tempdir");
        // Cap below the body size but above the spill threshold, so the read
        // is cut short AND the surviving text still spills.
        let tool = spill_test_tool(workspace.path(), 60_000);
        let body = "f".repeat(80_000);

        let result = fetch_body(&tool, &body, "text/plain").await;

        assert!(result.success, "error={:?}", result.error);
        let message = result.output.as_str();
        assert!(
            message.contains("max_response_size"),
            "message must disclose the stream-cap truncation, got:\n{message}"
        );

        // The stream cap still bounds what was written: hard_cap is
        // max_response_size + 1, so the file holds 60_001 bytes of response
        // (plus the line breaks hard-wrapping inserted), not 80_000.
        let written = std::fs::read_to_string(workspace.path().join(saved_path(message)))
            .expect("spill file");
        assert_eq!(
            written.replace('\n', "").len(),
            60_001,
            "stream cap must still bound the spilled bytes"
        );
    }

    /// A spill must never be mistaken for a JS-only page. The structural
    /// guarantee is that the Firecrawl heuristic reads the converted text as
    /// fetched, before delivery — so how large the *message* is cannot matter.
    #[tokio::test]
    async fn spilling_cannot_trigger_a_firecrawl_fallback() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let workspace = tempfile::tempdir().expect("tempdir");
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: workspace.path().to_path_buf(),
            ..SecurityPolicy::default()
        });
        let tool = WebFetchTool::new(
            security,
            vec!["*".into()],
            vec![],
            500_000,
            30,
            FirecrawlConfig {
                enabled: true,
                ..FirecrawlConfig::default()
            },
            vec![],
        )
        .unwrap();

        let body = "g".repeat(60_000);
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body.as_bytes(), "text/plain"))
            .mount(&server)
            .await;
        let url = format!("http://{}/page", server.address());
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("reqwest client");

        // The fallback decision sees the full 60 KB, not the short message the
        // model eventually receives.
        let attempt = tool.standard_fetch(&client, &url).await;
        assert_eq!(attempt_text(&attempt).len(), 60_000);
        assert!(
            !tool.should_fallback_to_firecrawl(&attempt),
            "a large body must never be mistaken for a JS-only page"
        );

        // Only afterwards is it spilled, and the message is short by design.
        let FetchAttempt::Content(content) = attempt else {
            panic!("expected fetched content");
        };
        let result = tool.deliver(&url, content).await;
        assert!(result.success, "error={:?}", result.error);
        assert!(
            result.output.as_str().contains("Saved to: "),
            "expected a spill message, got:\n{}",
            result.output.as_str()
        );
    }

    /// No resolvable workspace → the pre-existing inline behaviour, never a
    /// write outside the sandbox and never a dropped response.
    #[tokio::test]
    async fn spill_falls_back_to_inline_when_workspace_is_unresolvable() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let missing = workspace.path().join("no-such-workspace");
        let tool = spill_test_tool(&missing, 500_000);
        let body = "h".repeat(60_000);

        let result = fetch_body(&tool, &body, "text/plain").await;

        assert!(result.success, "error={:?}", result.error);
        assert_eq!(
            result.output.as_str(),
            body,
            "with no workspace the body must come back inline, unchanged"
        );
        assert!(
            !missing.exists(),
            "an unresolvable workspace must not be created behind the operator's back"
        );
    }

    // ── Leaf-symlink refusal ─────────────────────────────────────

    /// The containment check covers the spill *directory*; this covers the
    /// spill *file*. A symlink pre-planted at the exact destination filename
    /// must be refused, not followed — otherwise a single planted link turns
    /// an auto-approved fetch into an arbitrary out-of-workspace overwrite.
    ///
    /// The destination is content-addressed, so fetching the same body twice
    /// targets the same name: spill once to learn the path, plant a symlink
    /// there, then fetch again.
    #[cfg(unix)]
    #[tokio::test]
    async fn spill_abandons_a_symlink_planted_at_the_destination_file() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let outside = tempfile::tempdir().expect("outside tempdir");
        let tool = spill_test_tool(workspace.path(), 500_000);
        let body = "j".repeat(60_000);

        let first = fetch_body(&tool, &body, "text/plain").await;
        let relative = saved_path(first.output.as_str());
        let destination = workspace.path().join(&relative);
        std::fs::remove_file(&destination).expect("clear the spilled file");

        let target = outside.path().join("stolen.txt");
        std::os::unix::fs::symlink(&target, &destination).expect("plant leaf symlink");

        let second = fetch_body(&tool, &body, "text/plain").await;

        // Abandoned, so the response comes back inline rather than being lost.
        assert!(second.success, "error={:?}", second.error);
        assert_eq!(
            second.output.as_str(),
            body,
            "a refused spill must fall back to the inline body"
        );

        // Nothing was written through the link.
        assert!(
            !target.exists(),
            "wrote through the planted symlink to {}",
            target.display()
        );
        let escaped = walk_files(outside.path());
        assert!(
            escaped.is_empty(),
            "page content escaped the workspace to {escaped:?}"
        );

        // And the link itself was left alone, not replaced by a regular file.
        assert!(
            std::fs::symlink_metadata(&destination)
                .expect("destination must still exist")
                .is_symlink(),
            "the planted symlink must be left untouched, not overwritten"
        );
    }

    /// The other side of `create_new`: an existing *regular* file at a
    /// content-addressed name already holds exactly these bytes, so the spill
    /// reuses it instead of failing or rewriting it.
    #[tokio::test]
    async fn identical_content_reuses_the_existing_spill_file() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let tool = spill_test_tool(workspace.path(), 500_000);
        let body = "m".repeat(60_000);

        let first = fetch_body(&tool, &body, "text/plain").await;
        let first_path = saved_path(first.output.as_str());
        let second = fetch_body(&tool, &body, "text/plain").await;
        let second_path = saved_path(second.output.as_str());

        assert_eq!(
            first_path, second_path,
            "identical content must resolve to the same file"
        );

        let mut spill_dir = workspace.path().to_path_buf();
        for component in SPILL_DIR_COMPONENTS {
            spill_dir.push(component);
        }
        let files = walk_spill_files(&spill_dir);
        assert_eq!(files.len(), 1, "refetching must not accumulate copies");

        let written = std::fs::read_to_string(workspace.path().join(&second_path))
            .expect("spill file must still exist");
        assert_eq!(
            written.replace('\n', ""),
            body,
            "the reused file must still hold the full response"
        );
    }

    /// Every regular file directly under `dir`.
    fn walk_spill_files(dir: &std::path::Path) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        entries.flatten().map(|entry| entry.path()).collect()
    }

    // ── Write-policy gate ────────────────────────────────────────

    /// Spilling is a durable filesystem write performed by a tool the operator
    /// auto-approved as a fetch. A profile that denies `file_write` must not
    /// get one by way of `web_fetch`.
    fn spill_tool_with_policy(
        workspace: &std::path::Path,
        max_response_size: usize,
        allowed_tools: Option<Vec<String>>,
        excluded_tools: Option<Vec<String>>,
    ) -> WebFetchTool {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: workspace.to_path_buf(),
            allowed_tools,
            excluded_tools,
            ..SecurityPolicy::default()
        });
        WebFetchTool::new(
            security,
            vec!["*".into()],
            vec![],
            max_response_size,
            30,
            FirecrawlConfig::default(),
            vec![],
        )
        .unwrap()
    }

    #[tokio::test]
    async fn spill_is_skipped_when_file_write_is_excluded() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let tool = spill_tool_with_policy(
            workspace.path(),
            500_000,
            None,
            Some(vec!["file_write".into()]),
        );
        let body = "n".repeat(60_000);

        let result = fetch_body(&tool, &body, "text/plain").await;

        assert!(result.success, "error={:?}", result.error);
        assert_eq!(
            result.output.as_str(),
            body,
            "a policy that denies file writes must get the body inline"
        );
        assert!(
            !workspace.path().join("tmp").exists(),
            "no spill directory may be created when writes are denied"
        );
    }

    #[tokio::test]
    async fn spill_is_skipped_when_file_write_is_outside_the_allowlist() {
        let workspace = tempfile::tempdir().expect("tempdir");
        // An allowlist that admits web_fetch but not file_write.
        //
        // The response cap must sit ABOVE the spill threshold: it bounds the
        // streamed read, so a cap below the threshold would leave the text too
        // small to spill and this test would pass without the gate ever being
        // consulted. Above it, the text is large enough to spill AND large
        // enough for the cap to truncate, so both halves are observable.
        const CAP: usize = SPILL_THRESHOLD_BYTES + 5_000;
        let tool = spill_tool_with_policy(
            workspace.path(),
            CAP,
            Some(vec!["web_fetch".into(), "file_read".into()]),
            None,
        );
        let body = "p".repeat(200_000);

        let result = fetch_body(&tool, &body, "text/plain").await;

        assert!(result.success, "error={:?}", result.error);
        let output = result.output.as_str();
        assert!(
            output.contains("[Response truncated"),
            "must fall back to the pre-existing inline truncation, got:\n{output}"
        );
        assert!(
            !output.contains("Saved to: "),
            "must not report a saved file, got:\n{output}"
        );
        assert!(
            !workspace.path().join("tmp").exists(),
            "no spill directory may be created when writes are denied"
        );
    }

    /// The gate reads the policy, so a profile that permits `file_write`
    /// still spills. Without this the test above would pass even if spilling
    /// were broken outright.
    #[tokio::test]
    async fn spill_happens_when_file_write_is_permitted() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let tool = spill_tool_with_policy(
            workspace.path(),
            500_000,
            Some(vec!["web_fetch".into(), "file_write".into()]),
            None,
        );
        let body = "q".repeat(60_000);

        let result = fetch_body(&tool, &body, "text/plain").await;

        assert!(result.success, "error={:?}", result.error);
        assert!(
            result.output.as_str().contains("Saved to: "),
            "an allowlist containing file_write must still spill, got:\n{}",
            result.output.as_str()
        );
    }

    // ── Spill body reshaping ─────────────────────────────────────

    #[tokio::test]
    async fn json_spill_is_pretty_printed_and_paged_by_line() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let tool = spill_test_tool(workspace.path(), 500_000);

        // Minified JSON on a single line, comfortably over the threshold.
        let value = json!({
            "items": (0..2_000)
                .map(|i| json!({"id": i, "name": format!("item-{i}"), "tag": "xyzzy"}))
                .collect::<Vec<_>>()
        });
        let body = serde_json::to_string(&value).expect("minified json");
        assert!(body.len() > SPILL_THRESHOLD_BYTES);
        assert_eq!(body.lines().count(), 1, "source must be one line");

        let result = fetch_body(&tool, &body, "application/json").await;

        assert!(result.success, "error={:?}", result.error);
        let message = result.output.as_str();
        let relative = saved_path(message);
        assert!(relative.ends_with(".json"), "unexpected path: {relative}");
        assert!(
            message.contains("pretty-printed"),
            "message must disclose the reformat, got:\n{message}"
        );

        let written =
            std::fs::read_to_string(workspace.path().join(&relative)).expect("spill file");

        // Pageable: many lines, none over budget.
        assert!(
            written.lines().count() > 1_000,
            "pretty-printed JSON must be many lines, got {}",
            written.lines().count()
        );
        assert!(
            written
                .lines()
                .all(|line| line.len() <= SPILL_MAX_LINE_BYTES),
            "no line may exceed the wrap budget"
        );

        // And it is still the same document, not a lossy reformat.
        let reparsed: serde_json::Value =
            serde_json::from_str(&written).expect("saved JSON must still parse");
        assert_eq!(
            reparsed, value,
            "pretty-printing must preserve the document"
        );
    }

    #[test]
    fn prepare_spill_body_pretty_prints_only_parseable_json() {
        let (body, format) = prepare_spill_body(r#"{"a":1,"b":[2,3]}"#, "json");
        assert!(format.pretty_printed);
        assert!(!format.wrapped);
        assert!(body.lines().count() > 1, "expected indented output: {body}");

        // Same bytes, but not typed as JSON: left exactly as fetched.
        let (body, format) = prepare_spill_body(r#"{"a":1,"b":[2,3]}"#, "md");
        assert_eq!(body, r#"{"a":1,"b":[2,3]}"#);
        assert!(!format.pretty_printed);

        // Typed as JSON but not parseable: left alone rather than guessed at.
        let (body, format) = prepare_spill_body("{not json", "json");
        assert_eq!(body, "{not json");
        assert!(!format.pretty_printed);
    }

    #[test]
    fn hard_wrap_leaves_short_lines_untouched() {
        assert!(hard_wrap_long_lines("short\nlines\nonly").is_none());
        // Exactly at the budget is not over it.
        let exact = "a".repeat(SPILL_MAX_LINE_BYTES);
        assert!(hard_wrap_long_lines(&exact).is_none());
        // One byte over is.
        let over = "a".repeat(SPILL_MAX_LINE_BYTES + 1);
        assert_eq!(
            hard_wrap_long_lines(&over).as_deref(),
            Some(format!("{}\na", "a".repeat(SPILL_MAX_LINE_BYTES)).as_str())
        );
    }

    #[test]
    fn hard_wrap_splits_only_at_character_boundaries() {
        // "€" is 3 bytes, so 4000 is never a boundary — a naive byte split
        // here would panic or corrupt the text.
        let line = "€".repeat(5_000);
        let wrapped = hard_wrap_long_lines(&line).expect("must wrap");

        assert!(
            wrapped.lines().all(|l| l.len() <= SPILL_MAX_LINE_BYTES),
            "no line may exceed the wrap budget"
        );
        assert_eq!(
            wrapped.replace('\n', ""),
            line,
            "wrapping must not lose or alter a single character"
        );
        for l in wrapped.lines() {
            assert_eq!(
                l.len() % 3,
                0,
                "split landed mid-character: {} bytes",
                l.len()
            );
        }
    }

    #[test]
    fn hard_wrap_preserves_existing_line_structure() {
        let text = format!("head\n{}\ntail\n", "z".repeat(SPILL_MAX_LINE_BYTES + 10));
        let wrapped = hard_wrap_long_lines(&text).expect("must wrap");

        assert!(wrapped.starts_with("head\n"));
        assert!(
            wrapped.ends_with("\ntail\n"),
            "trailing newline must survive"
        );
        assert_eq!(
            wrapped.replace('\n', ""),
            text.replace('\n', ""),
            "wrapping must only add line breaks"
        );
    }

    // ── Firecrawl delivery ───────────────────────────────────────

    /// Firecrawl content is delivered through the same step as a standard
    /// fetch, so an oversized fallback result spills instead of being
    /// hard-truncated and losing its tail.
    #[tokio::test]
    async fn firecrawl_content_above_the_threshold_is_spilled() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let markdown = format!(
            "# Big Page\n\n{}",
            "long firecrawl paragraph. ".repeat(3_000)
        );
        assert!(markdown.len() > SPILL_THRESHOLD_BYTES);

        let firecrawl_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/scrape"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": true,
                "data": {"markdown": markdown}
            })))
            .mount(&firecrawl_server)
            .await;

        // SAFETY: test-only, and the variable name is unique to this test.
        unsafe { std::env::set_var("FIRECRAWL_SPILL_TEST_KEY", "test-key-12345") };

        let workspace = tempfile::tempdir().expect("tempdir");
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: workspace.path().to_path_buf(),
            ..SecurityPolicy::default()
        });
        let tool = WebFetchTool::new(
            security,
            vec!["*".into()],
            vec![],
            500_000,
            30,
            FirecrawlConfig {
                enabled: true,
                api_key_env: "FIRECRAWL_SPILL_TEST_KEY".into(),
                api_url: format!("http://{}", firecrawl_server.address()),
                ..FirecrawlConfig::default()
            },
            vec![],
        )
        .unwrap();

        let url = "https://example.com/page";
        let attempt = Box::pin(tool.fetch_via_firecrawl(url))
            .await
            .expect("firecrawl call");
        let FetchAttempt::Content(content) = attempt else {
            panic!("expected firecrawl content");
        };
        let result = tool.deliver(url, content).await;

        // SAFETY: test-only, and the variable name is unique to this test.
        unsafe { std::env::remove_var("FIRECRAWL_SPILL_TEST_KEY") };

        assert!(result.success, "error={:?}", result.error);
        let message = result.output.as_str();
        assert!(
            !message.contains("[Response truncated"),
            "Firecrawl content must spill, not hard-truncate, got:\n{message}"
        );

        // Markdown, saved under the same directory, holding the whole thing.
        let relative = saved_path(message);
        assert!(
            relative.starts_with("tmp/web_fetch/") && relative.ends_with(".md"),
            "unexpected Firecrawl spill path: {relative}"
        );
        let written =
            std::fs::read_to_string(workspace.path().join(&relative)).expect("spill file");
        assert_eq!(
            written.replace('\n', ""),
            markdown.replace('\n', ""),
            "the spilled file must hold the full Firecrawl markdown"
        );
    }

    #[tokio::test]
    async fn firecrawl_content_below_the_threshold_stays_inline() {
        let tool = test_tool_with_firecrawl(FirecrawlConfig::default());
        let content = FetchedContent {
            text: "# Small\n\nA short page.".to_string(),
            extension: "md",
            title: None,
            cap_hit: false,
        };

        let result = tool.deliver("https://example.com/page", content).await;

        assert!(result.success, "error={:?}", result.error);
        assert_eq!(result.output.as_str(), "# Small\n\nA short page.");
    }

    // ── Spill filename ───────────────────────────────────────────

    #[test]
    fn spill_file_name_is_stable_for_the_same_host_and_content() {
        let text = "same content";
        let first = spill_file_name("https://example.com/a", text, "md");
        let second = spill_file_name("https://example.com/a", text, "md");
        assert_eq!(first, second, "same host + content must be deterministic");

        // Path within the host does not change the name; only host + content do.
        let other_path = spill_file_name("https://example.com/b", text, "md");
        assert_eq!(first, other_path);

        assert!(
            first.starts_with("example.com-") && first.ends_with(".md"),
            "unexpected name: {first}"
        );
    }

    /// The identity is the FULL SHA-256, not a prefix. A 32-bit prefix is
    /// cheap to collide, which would let one chosen body be served from
    /// another body's path.
    #[test]
    fn spill_file_name_carries_the_full_sha256_digest() {
        let body = "content";
        let name = spill_file_name("https://example.com/a", body, "md");

        let digest = name
            .strip_prefix("example.com-")
            .and_then(|rest| rest.strip_suffix(".md"))
            .unwrap_or_else(|| panic!("unexpected name shape: {name}"));

        assert_eq!(digest.len(), 64, "digest must be the full 256-bit hash");
        assert!(
            digest
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
            "digest must be lowercase hex, got {digest}"
        );
        assert_eq!(
            digest,
            format!("{:x}", Sha256::digest(body.as_bytes())),
            "digest must be the SHA-256 of the written body"
        );
    }

    #[test]
    fn spill_file_name_changes_with_content_and_host() {
        let base = spill_file_name("https://example.com/a", "content one", "md");
        assert_ne!(
            base,
            spill_file_name("https://example.com/a", "content two", "md"),
            "different content must not overwrite a different page's file"
        );
        assert_ne!(
            base,
            spill_file_name("https://other.example.org/a", "content one", "md"),
            "different host must produce a different file"
        );
    }

    #[test]
    fn spill_file_name_is_a_single_safe_component() {
        // Ports are already stripped by extract_host; prove the sanitizer
        // still yields one component with no separator or traversal.
        for url in [
            "https://example.com:8443/a",
            "http://sub.example.co.uk/x?y=1",
        ] {
            let name = spill_file_name(url, "body", "txt");
            assert!(!name.contains('/'), "{name} contains a separator");
            assert!(!name.contains('\\'), "{name} contains a separator");
            assert!(!name.contains(".."), "{name} contains traversal");
            assert_eq!(
                std::path::Path::new(&name).components().count(),
                1,
                "{name} is not a single path component"
            );
        }
    }

    #[test]
    fn sanitize_path_component_strips_separators_and_traversal() {
        // Separators become `-`, so a traversal string collapses into one
        // harmless filename component. `..` survives only as literal
        // characters inside a name, never as a path component.
        let hostile = sanitize_path_component("../../etc/passwd");
        assert_eq!(hostile, "-..-etc-passwd");
        assert_eq!(
            std::path::Path::new(&hostile).components().count(),
            1,
            "sanitized host must be a single path component"
        );
        assert_eq!(
            std::path::Path::new(&hostile).parent(),
            Some(std::path::Path::new("")),
            "sanitized host must not re-parent"
        );

        assert_eq!(sanitize_path_component(".."), "unknown-host");
        assert_eq!(sanitize_path_component("."), "unknown-host");
        assert_eq!(sanitize_path_component(""), "unknown-host");
        assert_eq!(sanitize_path_component("a/b\\c"), "a-b-c");
        assert_eq!(sanitize_path_component("exämple.com"), "ex-mple.com");
        assert!(sanitize_path_component(&"a".repeat(200)).len() <= 60);
    }

    // ── Spill extension mapping ──────────────────────────────────

    #[test]
    fn spill_extension_maps_content_types() {
        assert_eq!(spill_extension("text/html; charset=utf-8", "html"), "md");
        assert_eq!(spill_extension("", "html"), "md");
        assert_eq!(spill_extension("application/json", "plain"), "json");
        assert_eq!(spill_extension("text/markdown", "plain"), "md");
        assert_eq!(spill_extension("text/plain; charset=utf-8", "plain"), "txt");
    }

    // ── Title extraction ─────────────────────────────────────────

    #[test]
    fn extract_html_title_handles_case_whitespace_and_absence() {
        assert_eq!(
            extract_html_title("<HTML><HEAD><TITLE>  Hello \n World </TITLE>").as_deref(),
            Some("Hello World")
        );
        assert_eq!(
            extract_html_title("<title lang=\"en\">Attr Title</title>").as_deref(),
            Some("Attr Title")
        );
        assert_eq!(extract_html_title("<html><body>no title</body>"), None);
        assert_eq!(extract_html_title("<title>   </title>"), None);
        assert_eq!(extract_html_title("<title>unclosed"), None);

        // Multi-byte titles inside the scan window survive intact.
        let near = format!("{}<title>Späte Seite</title>", "€".repeat(100));
        assert_eq!(extract_html_title(&near).as_deref(), Some("Späte Seite"));

        // Beyond the scan window the title is simply not found — and, the
        // point of this case, the 64 KiB cut must not land mid-character and
        // panic. "€" is 3 bytes, so byte 65536 is never a char boundary.
        let far = format!("{}<title>Too Late</title>", "€".repeat(30_000));
        assert_eq!(
            extract_html_title(&far),
            None,
            "title past the scan limit is skipped, not a panic"
        );
    }
}
