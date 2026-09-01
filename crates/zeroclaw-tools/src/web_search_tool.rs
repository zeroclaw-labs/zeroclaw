use super::web_search_provider_routing::{
    SearchStatus, WebSearchProviderRoute, resolve_web_search_provider,
};
use crate::util_helpers::truncate_with_ellipsis;
use async_trait::async_trait;
use regex::Regex;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use zeroclaw_api::tool::{Tool, ToolResult};

/// Web search tool for searching the internet.
/// Supports multiple model_providers: DuckDuckGo (free), Brave (requires API key),
/// Tavily (requires API key), SearXNG (self-hosted, requires instance URL),
/// Jina AI (requires API key), Bocha AI (requires API key, Chinese-friendly).
///
/// API keys are resolved lazily at execution time: if the boot-time key
/// is missing or still encrypted, the tool re-reads `config.toml`, decrypts the
/// corresponding `[web_search]` field, and uses the result. This ensures that
/// keys set or rotated after boot, and encrypted keys, are correctly picked up.
/// The Bocha key has no boot-time snapshot at all — it is always resolved from
/// `config.toml` at use time (see `resolve_bocha_api_key`), so the
/// canonical `[web_search] bocha_api_key` field stays the single source of
/// truth and rotation/removal takes effect without a restart.
pub struct WebSearchTool {
    /// ModelProvider selector as configured by user. Routed via model_provider aliases at runtime.
    model_provider: String,
    /// Boot-time key snapshot (may be `None` if not yet configured at startup).
    boot_brave_api_key: Option<String>,
    /// Boot-time Tavily key snapshot.
    boot_tavily_api_key: Option<String>,
    /// Boot-time Jina AI key snapshot.
    boot_jina_api_key: Option<String>,
    /// SearXNG instance base URL (e.g. `"https://searx.example.com"`).
    searxng_instance_url: Option<String>,
    max_results: usize,
    timeout_secs: u64,
    /// Path to `config.toml` for lazy re-read of keys at execution time.
    config_path: PathBuf,
    /// Whether secret encryption is enabled (needed to create a `SecretStore`).
    secrets_encrypt: bool,
}

impl WebSearchTool {
    pub fn new(
        model_provider: String,
        brave_api_key: Option<String>,
        jina_api_key: Option<String>,
        max_results: usize,
        timeout_secs: u64,
    ) -> Self {
        Self {
            model_provider: model_provider.trim().to_lowercase(),
            boot_brave_api_key: brave_api_key,
            boot_tavily_api_key: None,
            boot_jina_api_key: jina_api_key,
            searxng_instance_url: None,
            max_results: max_results.clamp(1, 10),
            timeout_secs: timeout_secs.max(1),
            config_path: PathBuf::new(),
            secrets_encrypt: false,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_config(
        model_provider: String,
        brave_api_key: Option<String>,
        tavily_api_key: Option<String>,
        jina_api_key: Option<String>,
        searxng_instance_url: Option<String>,
        max_results: usize,
        timeout_secs: u64,
        config_path: PathBuf,
        secrets_encrypt: bool,
    ) -> Self {
        Self {
            model_provider: model_provider.trim().to_lowercase(),
            boot_brave_api_key: brave_api_key,
            boot_tavily_api_key: tavily_api_key,
            boot_jina_api_key: jina_api_key,
            searxng_instance_url,
            max_results: max_results.clamp(1, 10),
            timeout_secs: timeout_secs.max(1),
            config_path,
            secrets_encrypt,
        }
    }

    /// Resolve the Brave API key, preferring the boot-time value but falling
    /// back to a fresh config read + decryption when the boot-time value is
    /// absent.
    fn resolve_brave_api_key(&self) -> anyhow::Result<String> {
        // Fast path: boot-time key is present and usable (not an encrypted blob).
        if let Some(ref key) = self.boot_brave_api_key
            && !key.is_empty()
            && !zeroclaw_config::secrets::SecretStore::is_encrypted(key)
        {
            return Ok(key.clone());
        }

        // Slow path: re-read config.toml to pick up keys set/rotated after boot.
        self.reload_brave_api_key()
    }

    /// Re-read `config.toml` and decrypt `[web_search] brave_api_key`.
    fn reload_brave_api_key(&self) -> anyhow::Result<String> {
        let contents = std::fs::read_to_string(&self.config_path).map_err(|e| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "path": self.config_path.display().to_string(),
                        "search_provider": "brave",
                        "error": format!("{}", e),
                    })),
                "web_search: failed to read config for Brave API key"
            );
            anyhow::Error::msg(format!(
                "Failed to read config file {} for Brave API key: {e}",
                self.config_path.display()
            ))
        })?;

        let config: zeroclaw_config::schema::Config = toml::from_str(&contents).map_err(|e| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "path": self.config_path.display().to_string(),
                        "search_provider": "brave",
                        "error": format!("{}", e),
                    })),
                "web_search: failed to parse config for Brave API key"
            );
            anyhow::Error::msg(format!(
                "Failed to parse config file {} for Brave API key: {e}",
                self.config_path.display()
            ))
        })?;

        let raw_key = config
            .web_search
            .brave_api_key
            .filter(|k| !k.is_empty())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"search_provider": "brave"})),
                    "web_search: Brave API key not configured"
                );
                anyhow::Error::msg("Brave API key not configured")
            })?;

        // Decrypt if necessary.
        if zeroclaw_config::secrets::SecretStore::is_encrypted(&raw_key) {
            let zeroclaw_dir = self.config_path.parent().unwrap_or_else(|| Path::new("."));
            let store =
                zeroclaw_config::secrets::SecretStore::new(zeroclaw_dir, self.secrets_encrypt);
            let plaintext = store.decrypt(&raw_key)?;
            if plaintext.is_empty() {
                anyhow::bail!("Brave API key not configured (decrypted value is empty)");
            }
            Ok(plaintext)
        } else {
            Ok(raw_key)
        }
    }

    async fn search_duckduckgo(&self, query: &str) -> anyhow::Result<String> {
        // Throttling lives here rather than in `search_duckduckgo_at` so the
        // wiremock-backed request tests that target the inner method do not
        // each pay — and serialize on — a multi-second scrape gap.
        DUCKDUCKGO_THROTTLE
            .acquire(duckduckgo_gap(scrape_entropy()))
            .await;
        self.search_duckduckgo_at("https://html.duckduckgo.com/html/", query)
            .await
    }

    /// Inner DuckDuckGo request implementation, parameterized on the endpoint URL
    /// so request-flow tests can target a local mock server. Production calls
    /// always go through [`Self::search_duckduckgo`].
    async fn search_duckduckgo_at(
        &self,
        endpoint_url: &str,
        query: &str,
    ) -> anyhow::Result<String> {
        let encoded_query = urlencoding::encode(query);
        let search_url = format!("{}?q={}", endpoint_url, encoded_query);

        let headers = duckduckgo_request_headers(scrape_entropy());

        let builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(self.timeout_secs))
            .user_agent(headers.user_agent);
        let builder =
            zeroclaw_config::schema::apply_runtime_proxy_to_builder(builder, "tool.web_search");
        let client = builder.build()?;

        let response = client
            .get(&search_url)
            .header("Accept", DUCKDUCKGO_ACCEPT)
            .header("Accept-Language", headers.accept_language)
            .header("DNT", "1")
            .send()
            .await?;
        let status = response.status();
        let final_url_is_block =
            contains_ascii_case_insensitive(response.url().as_str(), "/wr.do?");

        if !status.is_success() {
            if let Some(message) = duckduckgo_block_message(status, final_url_is_block, false) {
                anyhow::bail!(message);
            }
            return Err(http_search_failure("duckduckgo", status));
        }

        let html = response.text().await?;
        let html_contains_block = contains_ascii_case_insensitive(&html, "/wr.do?")
            || contains_ascii_case_insensitive(&html, "anomaly-modal");
        if let Some(message) =
            duckduckgo_block_message(status, final_url_is_block, html_contains_block)
        {
            anyhow::bail!(message);
        }
        self.parse_duckduckgo_results(&html, query)
    }

    fn parse_duckduckgo_results(&self, html: &str, query: &str) -> anyhow::Result<String> {
        // Extract result links: <a class="result__a" href="...">Title</a>
        let link_regex = Regex::new(
            r#"<a[^>]*class="[^"]*result__a[^"]*"[^>]*href="([^"]+)"[^>]*>([\s\S]*?)</a>"#,
        )?;

        // Extract snippets: <a class="result__snippet">...</a>
        let snippet_regex = Regex::new(r#"<a class="result__snippet[^"]*"[^>]*>([\s\S]*?)</a>"#)?;

        let link_matches: Vec<_> = link_regex
            .captures_iter(html)
            .take(self.max_results + 2)
            .collect();

        let snippet_matches: Vec<_> = snippet_regex
            .captures_iter(html)
            .take(self.max_results + 2)
            .collect();

        if link_matches.is_empty() {
            return Ok(no_results_message(query));
        }

        let count = link_matches.len().min(self.max_results);
        let mut blocks: Vec<Vec<String>> = Vec::with_capacity(count);

        for i in 0..count {
            let caps = &link_matches[i];
            let url_str = decode_ddg_redirect_url(&caps[1]);
            let title = strip_tags(&caps[2]);

            let mut block = vec![
                format!("{}. {}", i + 1, title.trim()),
                format!("   {}", url_str.trim()),
            ];

            // Add snippet if available
            if i < snippet_matches.len() {
                let snippet = strip_tags(&snippet_matches[i][1]);
                let snippet = snippet.trim();
                if !snippet.is_empty() {
                    block.push(format!("   {}", cap_result_content(snippet)));
                }
            }

            blocks.push(block);
        }

        Ok(render_results(results_header(query, "DuckDuckGo"), blocks))
    }

    async fn search_brave(&self, query: &str) -> anyhow::Result<String> {
        let api_key = self.resolve_brave_api_key()?;

        let encoded_query = urlencoding::encode(query);
        let search_url = format!(
            "https://api.search.brave.com/res/v1/web/search?q={}&count={}",
            encoded_query, self.max_results
        );

        let builder = reqwest::Client::builder().timeout(Duration::from_secs(self.timeout_secs));
        let builder =
            zeroclaw_config::schema::apply_runtime_proxy_to_builder(builder, "tool.web_search");
        let client = builder.build()?;

        let response = client
            .get(&search_url)
            .header("Accept", "application/json")
            .header("X-Subscription-Token", &api_key)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(http_search_failure("brave", response.status()));
        }

        let json: serde_json::Value = response.json().await?;
        self.parse_brave_results(&json, query)
    }

    /// Resolve the Tavily API key from the boot-time snapshot, falling back
    /// to a fresh config read + decryption when the boot-time value is absent.
    fn resolve_tavily_api_key(&self) -> anyhow::Result<String> {
        if let Some(ref key) = self.boot_tavily_api_key
            && !key.is_empty()
            && !zeroclaw_config::secrets::SecretStore::is_encrypted(key)
        {
            return Ok(key.clone());
        }
        self.reload_tavily_api_key()
    }

    /// Re-read `config.toml` and decrypt `[web_search] tavily_api_key`.
    fn reload_tavily_api_key(&self) -> anyhow::Result<String> {
        let contents = std::fs::read_to_string(&self.config_path).map_err(|e| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "path": self.config_path.display().to_string(),
                        "search_provider": "tavily",
                        "error": format!("{}", e),
                    })),
                "web_search: failed to read config for Tavily API key"
            );
            anyhow::Error::msg(format!(
                "Failed to read config file {} for Tavily API key: {e}",
                self.config_path.display()
            ))
        })?;

        let config: zeroclaw_config::schema::Config = toml::from_str(&contents).map_err(|e| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "path": self.config_path.display().to_string(),
                        "search_provider": "tavily",
                        "error": format!("{}", e),
                    })),
                "web_search: failed to parse config for Tavily API key"
            );
            anyhow::Error::msg(format!(
                "Failed to parse config file {} for Tavily API key: {e}",
                self.config_path.display()
            ))
        })?;

        let raw_key = config
            .web_search
            .tavily_api_key
            .filter(|k| !k.is_empty())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"search_provider": "tavily"})),
                    "web_search: Tavily API key not configured"
                );
                anyhow::Error::msg("Tavily API key not configured")
            })?;

        if zeroclaw_config::secrets::SecretStore::is_encrypted(&raw_key) {
            let zeroclaw_dir = self.config_path.parent().unwrap_or_else(|| Path::new("."));
            let store =
                zeroclaw_config::secrets::SecretStore::new(zeroclaw_dir, self.secrets_encrypt);
            let plaintext = store.decrypt(&raw_key)?;
            if plaintext.is_empty() {
                anyhow::bail!("Tavily API key not configured (decrypted value is empty)");
            }
            Ok(plaintext)
        } else {
            Ok(raw_key)
        }
    }

    async fn search_tavily(&self, query: &str) -> anyhow::Result<String> {
        let client = self.build_tavily_client()?;
        self.search_tavily_with_client(&client, "https://api.tavily.com/search", query)
            .await
    }

    fn build_tavily_client(&self) -> anyhow::Result<reqwest::Client> {
        let builder = reqwest::Client::builder().timeout(Duration::from_secs(self.timeout_secs));
        let builder =
            zeroclaw_config::schema::apply_runtime_proxy_to_builder(builder, "tool.web_search");
        Ok(builder.build()?)
    }

    /// Inner Tavily request implementation, parameterized on the HTTP
    /// client and endpoint URL so request-shape tests can target a local
    /// mock server with a client that doesn't read process-global proxy
    /// state. Production calls always go through [`Self::search_tavily`].
    async fn search_tavily_with_client(
        &self,
        client: &reqwest::Client,
        url: &str,
        query: &str,
    ) -> anyhow::Result<String> {
        let api_key = self.resolve_tavily_api_key()?;

        // Tavily authenticates via `Authorization: Bearer <key>` per
        // https://docs.tavily.com/documentation/api-reference/endpoint/search
        // (the API also tolerates `api_key` in the body for legacy clients,
        // but bearer-header is the documented contract).
        let body = serde_json::json!({
            "query": query,
            "max_results": self.max_results,
            "search_depth": "basic",
            "include_answer": false,
            "include_raw_content": false,
        });

        let response = client
            .post(url)
            .bearer_auth(&api_key)
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(http_search_failure("tavily", response.status()));
        }

        let json: serde_json::Value = response.json().await?;
        self.parse_tavily_results(&json, query)
    }

    fn parse_tavily_results(
        &self,
        json: &serde_json::Value,
        query: &str,
    ) -> anyhow::Result<String> {
        let results = json
            .get("results")
            .and_then(|r| r.as_array())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"search_provider": "tavily"})),
                    "web_search: invalid Tavily response"
                );
                anyhow::Error::msg("Invalid Tavily API response")
            })?;

        if results.is_empty() {
            return Ok(no_results_message(query));
        }

        let mut blocks: Vec<Vec<String>> = Vec::new();

        for (i, result) in results.iter().take(self.max_results).enumerate() {
            let title = result
                .get("title")
                .and_then(|t| t.as_str())
                .unwrap_or("No title");
            let url = result.get("url").and_then(|u| u.as_str()).unwrap_or("");
            // Tavily returns a pre-cleaned `content` field (not just a snippet),
            // so it doubles as the description for the LLM caller.
            let content = result.get("content").and_then(|c| c.as_str()).unwrap_or("");

            let mut block = vec![format!("{}. {}", i + 1, title), format!("   {}", url)];
            if !content.is_empty() {
                block.push(format!("   {}", cap_result_content(content)));
            }
            blocks.push(block);
        }

        Ok(render_results(results_header(query, "Tavily"), blocks))
    }

    /// Resolve the Jina AI API key from the boot-time snapshot, falling back
    /// to a fresh config read + decryption when the boot-time value is absent.
    fn resolve_jina_api_key(&self) -> anyhow::Result<String> {
        if let Some(ref key) = self.boot_jina_api_key
            && !key.is_empty()
            && !zeroclaw_config::secrets::SecretStore::is_encrypted(key)
        {
            return Ok(key.clone());
        }
        self.reload_jina_api_key()
    }

    /// Re-read `config.toml` and decrypt `[web_search] jina_api_key`.
    fn reload_jina_api_key(&self) -> anyhow::Result<String> {
        let contents = std::fs::read_to_string(&self.config_path).map_err(|e| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "path": self.config_path.display().to_string(),
                        "search_provider": "jina",
                        "error": format!("{}", e),
                    })),
                "web_search: failed to read config for Jina AI API key"
            );
            anyhow::Error::msg(format!(
                "Failed to read config file {} for Jina AI API key: {e}",
                self.config_path.display()
            ))
        })?;

        let config: zeroclaw_config::schema::Config = toml::from_str(&contents).map_err(|e| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "path": self.config_path.display().to_string(),
                        "search_provider": "jina",
                        "error": format!("{}", e),
                    })),
                "web_search: failed to parse config for Jina AI API key"
            );
            anyhow::Error::msg(format!(
                "Failed to parse config file {} for Jina AI API key: {e}",
                self.config_path.display()
            ))
        })?;

        let raw_key = config
            .web_search
            .jina_api_key
            .filter(|k| !k.is_empty())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"search_provider": "jina"})),
                    "web_search: Jina AI API key not configured"
                );
                anyhow::Error::msg("Jina AI API key not configured")
            })?;

        if zeroclaw_config::secrets::SecretStore::is_encrypted(&raw_key) {
            let zeroclaw_dir = self.config_path.parent().unwrap_or_else(|| Path::new("."));
            let store =
                zeroclaw_config::secrets::SecretStore::new(zeroclaw_dir, self.secrets_encrypt);
            let plaintext = store.decrypt(&raw_key)?;
            if plaintext.is_empty() {
                anyhow::bail!("Jina AI API key not configured (decrypted value is empty)");
            }
            Ok(plaintext)
        } else {
            Ok(raw_key)
        }
    }

    async fn search_jina(&self, query: &str) -> anyhow::Result<String> {
        let api_key = self.resolve_jina_api_key()?;

        let builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(self.timeout_secs))
            .user_agent("ZeroClaw/1.0 (https://zeroclaw.ai)");
        let builder =
            zeroclaw_config::schema::apply_runtime_proxy_to_builder(builder, "tool.web_search");
        let client = builder.build()?;

        // Jina Search API requires POST with JSON body
        let body = serde_json::json!({"q": query});

        let response = client
            .post("https://s.jina.ai/")
            .header("Authorization", format!("Bearer {}", api_key))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(http_search_failure("jina", response.status()));
        }

        let json: serde_json::Value = response.json().await?;
        self.parse_jina_results(&json, query)
    }

    fn parse_jina_results(&self, json: &serde_json::Value, query: &str) -> anyhow::Result<String> {
        // Jina API returns {"code": 200, "status": 20000, "data": [...]}
        let results = json.get("data").and_then(|r| r.as_array()).ok_or_else(|| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"search_provider": "jina"})),
                "web_search: invalid Jina AI response"
            );
            anyhow::Error::msg("Invalid Jina AI API response")
        })?;

        if results.is_empty() {
            return Ok(no_results_message(query));
        }

        let mut blocks: Vec<Vec<String>> = Vec::new();

        for (i, result) in results.iter().take(self.max_results).enumerate() {
            let title = result
                .get("title")
                .and_then(|t| t.as_str())
                .unwrap_or("No title");
            let url = result.get("url").and_then(|u| u.as_str()).unwrap_or("");
            // Jina's content field contains richer markdown-formatted page content;
            // fall back to description if content is absent
            let snippet = result
                .get("content")
                .and_then(|c| c.as_str())
                .or_else(|| result.get("description").and_then(|d| d.as_str()))
                .unwrap_or("");

            let mut block = vec![format!("{}. {}", i + 1, title), format!("   {}", url)];
            if !snippet.is_empty() {
                block.push(format!("   {}", cap_result_content(snippet)));
            }
            blocks.push(block);
        }

        Ok(render_results(results_header(query, "Jina AI"), blocks))
    }

    fn resolve_bocha_api_key(&self) -> anyhow::Result<String> {
        let contents = std::fs::read_to_string(&self.config_path).map_err(|e| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "path": self.config_path.display().to_string(),
                        "search_provider": "bocha",
                        "error": format!("{}", e),
                    })),
                "web_search: failed to read config for Bocha AI API key"
            );
            anyhow::Error::msg(format!(
                "Failed to read config file {} for Bocha AI API key: {e}",
                self.config_path.display()
            ))
        })?;

        let config: zeroclaw_config::schema::Config = toml::from_str(&contents).map_err(|e| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "path": self.config_path.display().to_string(),
                        "search_provider": "bocha",
                        "error": format!("{}", e),
                    })),
                "web_search: failed to parse config for Bocha AI API key"
            );
            anyhow::Error::msg(format!(
                "Failed to parse config file {} for Bocha AI API key: {e}",
                self.config_path.display()
            ))
        })?;

        let raw_key = config
            .web_search
            .bocha_api_key
            .filter(|k| !k.is_empty())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"search_provider": "bocha"})),
                    "web_search: Bocha AI API key not configured"
                );
                anyhow::Error::msg(
                    "Bocha AI API key not configured. Set [web_search] bocha_api_key in \
                     config.toml. Obtain one at https://open.bochaai.com",
                )
            })?;

        if zeroclaw_config::secrets::SecretStore::is_encrypted(&raw_key) {
            let zeroclaw_dir = self.config_path.parent().unwrap_or_else(|| Path::new("."));
            let store =
                zeroclaw_config::secrets::SecretStore::new(zeroclaw_dir, self.secrets_encrypt);
            let plaintext = store.decrypt(&raw_key)?;
            if plaintext.is_empty() {
                anyhow::bail!("Bocha AI API key not configured (decrypted value is empty)");
            }
            Ok(plaintext)
        } else {
            Ok(raw_key)
        }
    }

    async fn search_bocha(&self, query: &str) -> anyhow::Result<String> {
        let builder = reqwest::Client::builder().timeout(Duration::from_secs(self.timeout_secs));
        let builder =
            zeroclaw_config::schema::apply_runtime_proxy_to_builder(builder, "tool.web_search");
        let client = builder.build()?;
        self.search_bocha_with_client(&client, "https://api.bochaai.com/v1/web-search", query)
            .await
    }

    async fn search_bocha_with_client(
        &self,
        client: &reqwest::Client,
        url: &str,
        query: &str,
    ) -> anyhow::Result<String> {
        let api_key = self.resolve_bocha_api_key()?;

        let body = serde_json::json!({
            "query": query,
            "count": self.max_results,
            "summary": true,
            "freshness": "noLimit",
        });

        let response = client
            .post(url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .bearer_auth(&api_key)
            .json(&body)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            return Err(http_search_failure("bocha", status));
        }

        let json: serde_json::Value = response.json().await?;
        self.parse_bocha_results(&json, query)
    }

    fn parse_bocha_results(&self, json: &serde_json::Value, query: &str) -> anyhow::Result<String> {
        if let Some(code) = json.get("code").and_then(|c| c.as_i64())
            && code != 200
        {
            let msg = json
                .get("msg")
                .and_then(|m| m.as_str())
                .unwrap_or("(no message)");
            // This returns before the capped success path, so `msg` is the one
            // piece of provider-controlled text that would otherwise reach the
            // model unbounded.
            anyhow::bail!(
                "Bocha AI search returned error (code {code}): {}",
                cap_provider_error(msg)
            );
        }

        let results = json
            .get("data")
            .and_then(|d| d.get("webPages"))
            .and_then(|w| w.get("value"))
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"search_provider": "bocha"})),
                    "web_search: invalid Bocha AI response"
                );
                anyhow::Error::msg("Invalid Bocha AI API response")
            })?;

        if results.is_empty() {
            return Ok(no_results_message(query));
        }

        let mut blocks: Vec<Vec<String>> = Vec::new();

        for (i, result) in results.iter().take(self.max_results).enumerate() {
            let title = result
                .get("name")
                .and_then(|t| t.as_str())
                .unwrap_or("No title");
            let url = result.get("url").and_then(|u| u.as_str()).unwrap_or("");
            // Prefer Bocha's AI summary; fall back to the raw snippet.
            let body = result
                .get("summary")
                .and_then(|s| s.as_str())
                .filter(|s| !s.is_empty())
                .or_else(|| result.get("snippet").and_then(|s| s.as_str()))
                .unwrap_or("");
            let site = result
                .get("siteName")
                .and_then(|s| s.as_str())
                .unwrap_or("");
            let date = result
                .get("datePublished")
                .and_then(|d| d.as_str())
                .or_else(|| result.get("dateLastCrawled").and_then(|d| d.as_str()))
                .unwrap_or("");

            let mut block = vec![format!("{}. {}", i + 1, title), format!("   {}", url)];

            // Compact attribution line: "siteName · date" when either is present.
            let attribution = match (site.is_empty(), date.is_empty()) {
                (false, false) => format!("   {site} · {date}"),
                (false, true) => format!("   {site}"),
                (true, false) => format!("   {date}"),
                (true, true) => String::new(),
            };
            if !attribution.is_empty() {
                block.push(attribution);
            }

            if !body.is_empty() {
                block.push(format!("   {}", cap_result_content(body)));
            }

            blocks.push(block);
        }

        Ok(render_results(results_header(query, "Bocha"), blocks))
    }

    fn parse_brave_results(&self, json: &serde_json::Value, query: &str) -> anyhow::Result<String> {
        let results = json
            .get("web")
            .and_then(|w| w.get("results"))
            .and_then(|r| r.as_array())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"search_provider": "brave"})),
                    "web_search: invalid Brave response"
                );
                anyhow::Error::msg("Invalid Brave API response")
            })?;

        if results.is_empty() {
            return Ok(no_results_message(query));
        }

        let mut blocks: Vec<Vec<String>> = Vec::new();

        for (i, result) in results.iter().take(self.max_results).enumerate() {
            let title = result
                .get("title")
                .and_then(|t| t.as_str())
                .unwrap_or("No title");
            let url = result.get("url").and_then(|u| u.as_str()).unwrap_or("");
            let description = result
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("");

            let mut block = vec![format!("{}. {}", i + 1, title), format!("   {}", url)];
            if !description.is_empty() {
                block.push(format!("   {}", cap_result_content(description)));
            }
            blocks.push(block);
        }

        Ok(render_results(results_header(query, "Brave"), blocks))
    }

    /// Resolve the SearXNG instance URL from the boot-time config or by
    /// re-reading `config.toml` at runtime.
    fn resolve_searxng_instance_url(&self) -> anyhow::Result<String> {
        if let Some(ref url) = self.searxng_instance_url
            && !url.is_empty()
        {
            return Ok(url.clone());
        }

        // Slow path: re-read config.toml to pick up values set after boot.
        let contents = std::fs::read_to_string(&self.config_path).map_err(|e| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "path": self.config_path.display().to_string(),
                        "search_provider": "searxng",
                        "error": format!("{}", e),
                    })),
                "web_search: failed to read config for SearXNG URL"
            );
            anyhow::Error::msg(format!(
                "Failed to read config file {} for SearXNG instance URL: {e}",
                self.config_path.display()
            ))
        })?;

        let config: zeroclaw_config::schema::Config = toml::from_str(&contents).map_err(|e| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "path": self.config_path.display().to_string(),
                        "search_provider": "searxng",
                        "error": format!("{}", e),
                    })),
                "web_search: failed to parse config for SearXNG URL"
            );
            anyhow::Error::msg(format!(
                "Failed to parse config file {} for SearXNG instance URL: {e}",
                self.config_path.display()
            ))
        })?;

        config
            .web_search
            .searxng_instance_url
            .filter(|u| !u.is_empty())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"search_provider": "searxng"})),
                    "web_search: SearXNG instance URL not configured"
                );
                anyhow::Error::msg(SEARXNG_NOT_CONFIGURED_MESSAGE.as_str())
            })
    }

    async fn search_searxng(&self, query: &str) -> anyhow::Result<String> {
        let instance_url = self.resolve_searxng_instance_url()?;
        let base_url = instance_url.trim_end_matches('/');

        let encoded_query = urlencoding::encode(query);
        let search_url = format!(
            "{}/search?q={}&format=json&pageno=1",
            base_url, encoded_query
        );

        let builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(self.timeout_secs))
            .user_agent("ZeroClaw/1.0");
        let builder =
            zeroclaw_config::schema::apply_runtime_proxy_to_builder(builder, "tool.web_search");
        let client = builder.build()?;

        let response = client
            .get(&search_url)
            .header("Accept", "application/json")
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(http_search_failure("searxng", response.status()));
        }

        let json: serde_json::Value = response.json().await?;
        self.parse_searxng_results(&json, query)
    }

    fn parse_searxng_results(
        &self,
        json: &serde_json::Value,
        query: &str,
    ) -> anyhow::Result<String> {
        let results = json
            .get("results")
            .and_then(|r| r.as_array())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"search_provider": "searxng"})),
                    "web_search: invalid SearXNG response"
                );
                anyhow::Error::msg("Invalid SearXNG API response")
            })?;

        if results.is_empty() {
            return Ok(no_results_message(query));
        }

        let mut blocks: Vec<Vec<String>> = Vec::new();

        for (i, result) in results.iter().take(self.max_results).enumerate() {
            let title = result
                .get("title")
                .and_then(|t| t.as_str())
                .unwrap_or("No title");
            let url = result.get("url").and_then(|u| u.as_str()).unwrap_or("");
            let content = result.get("content").and_then(|c| c.as_str()).unwrap_or("");

            let mut block = vec![format!("{}. {}", i + 1, title), format!("   {}", url)];
            if !content.is_empty() {
                block.push(format!("   {}", cap_result_content(content)));
            }
            blocks.push(block);
        }

        Ok(render_results(results_header(query, "SearXNG"), blocks))
    }
}

// ── Output caps ──────────────────────────────────────────────────────────────
//
// Every provider response is untrusted, unbounded text that lands straight in
// the model's context window. Jina and Bocha are the acute cases: both return
// long markdown / AI-generated summaries per hit, so a single search can crowd
// out a small model's entire context.
//
// Both limits are intentionally constants rather than config keys. There is no
// concrete operator use case for tuning them (per AGENTS.md: no config keys
// without one), and a key here would be one more surface to drift.

/// Maximum characters of provider-supplied body text — snippet, description,
/// content, or AI summary — kept per individual result.
const MAX_RESULT_CONTENT_CHARS: usize = 500;

/// Maximum characters of rendered search output, header line included. The
/// omission note below is appended on top of this budget so the signal that
/// trimming happened can never itself be trimmed away.
const MAX_TOTAL_OUTPUT_CHARS: usize = 16_000;

/// Maximum characters of the caller-supplied query echoed back into rendered
/// output or a "no results" reply. The query is model-controlled and
/// unbounded, so echoing it verbatim lets one oversized query consume — or on
/// the "no results" path, entirely escape — the total output budget.
const MAX_QUERY_ECHO_CHARS: usize = 200;

/// Maximum characters of a provider-supplied error message quoted back to the
/// model. Business-error text is as provider-controlled as result content, and
/// it returns before the rendering caps rather than through them.
const MAX_PROVIDER_ERROR_CHARS: usize = 500;

/// Fluent key for the note appended when [`MAX_TOTAL_OUTPUT_CHARS`] forced
/// results to be dropped or the output to be cut.
const TRUNCATED_RESULTS_NOTE_KEY: &str = "tool-web-search-tool-note-truncated-results";

/// Appended when [`MAX_TOTAL_OUTPUT_CHARS`] forced results to be dropped or
/// the output to be cut, so the model knows the list it sees is partial.
static TRUNCATED_RESULTS_NOTE: LazyLock<String> =
    LazyLock::new(|| crate::i18n::get_required_tool_string(TRUNCATED_RESULTS_NOTE_KEY));

/// Cap one provider-supplied body string, appending a visible `...` marker
/// when text was dropped.
///
/// Truncation is by Unicode character, never by byte, so multibyte text is
/// never split mid-codepoint.
fn cap_result_content(text: &str) -> String {
    truncate_with_ellipsis(text, MAX_RESULT_CONTENT_CHARS)
}

/// Cap the caller's query before it is echoed back to the model.
fn cap_query_echo(query: &str) -> String {
    truncate_with_ellipsis(query, MAX_QUERY_ECHO_CHARS)
}

/// Cap a provider-supplied error message before it is quoted back to the
/// model.
fn cap_provider_error(message: &str) -> String {
    truncate_with_ellipsis(message, MAX_PROVIDER_ERROR_CHARS)
}

/// Header line for a rendered result list. Shared so the echoed query is
/// bounded — and the wording stays identical — across all six providers.
fn results_header(query: &str, provider: &str) -> String {
    format!(
        "Search results for: {} (via {provider})",
        cap_query_echo(query)
    )
}

/// The reply every provider returns when a search matched nothing.
///
/// One shared function rather than six copies: this path returns before
/// [`render_results`], so it is the only place the echoed query is bounded at
/// all, and a per-provider copy would be a per-provider chance to forget.
fn no_results_message(query: &str) -> String {
    format!("No results found for: {}", cap_query_echo(query))
}

/// Render a header line plus one line-block per result, bounded by
/// [`MAX_TOTAL_OUTPUT_CHARS`].
///
/// Trimming happens at whole-result granularity so the model never receives a
/// result cut off mid-field. Shared by all six provider parsers — the cap must
/// not depend on which provider answered.
fn render_results(header: String, blocks: Vec<Vec<String>>) -> String {
    let mut out = header;
    let mut used = out.chars().count();
    let mut trimmed = false;

    for (index, block) in blocks.into_iter().enumerate() {
        let rendered = block.join("\n");
        let cost = rendered.chars().count() + 1; // + the joining newline
        // The first result is always emitted: a bare header is strictly less
        // useful than one oversized result, and the hard cap below still
        // bounds whatever that result contains.
        if index > 0 && used + cost > MAX_TOTAL_OUTPUT_CHARS {
            trimmed = true;
            break;
        }
        out.push('\n');
        out.push_str(&rendered);
        used += cost;
    }

    // Only reachable when the always-emitted first result overshoots on its
    // own (a pathological title or URL — those are not content-capped, since a
    // truncated URL is useless for a `web_fetch` follow-up).
    if used > MAX_TOTAL_OUTPUT_CHARS {
        out = truncate_with_ellipsis(&out, MAX_TOTAL_OUTPUT_CHARS);
        trimmed = true;
    }

    if trimmed {
        out.push('\n');
        out.push_str(TRUNCATED_RESULTS_NOTE.as_str());
    }

    out
}

// ── DuckDuckGo scrape hygiene ────────────────────────────────────────────────
//
// The default provider scrapes `html.duckduckgo.com`, and DuckDuckGo blocks
// machines that look automated — which is why this module ships block
// detection at all. Two cheap signals are worth removing: a single fixed
// User-Agent, and a burst of back-to-back requests with no human-scale gap.

/// Realistic desktop browser User-Agents, rotated per request. A single fixed
/// UA is a trivially stable fingerprint across every ZeroClaw install.
const DUCKDUCKGO_USER_AGENTS: &[&str] = &[
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:133.0) Gecko/20100101 Firefox/133.0",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:133.0) Gecko/20100101 Firefox/133.0",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.1 Safari/605.1.15",
];

/// `Accept-Language` values varied alongside the User-Agent, so the header set
/// as a whole varies rather than one field moving under a constant remainder.
const DUCKDUCKGO_ACCEPT_LANGUAGES: &[&str] = &[
    "en-US,en;q=0.9",
    "en-US,en;q=0.8",
    "en-GB,en;q=0.9",
    "en-US,en;q=0.9,en-GB;q=0.7",
];

/// `Accept` value sent with every scrape. Static on purpose: real browsers
/// send an essentially fixed value here, so varying it would stand out.
const DUCKDUCKGO_ACCEPT: &str =
    "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8";

/// The browser-shaped header set selected for one scrape request.
struct DuckDuckGoRequestHeaders {
    user_agent: &'static str,
    accept_language: &'static str,
}

/// Select a header set from `entropy`. Pure and total: every input maps to a
/// pool member, so there is no "unlucky value" that yields a ZeroClaw-branded
/// or empty header.
fn duckduckgo_request_headers(entropy: u64) -> DuckDuckGoRequestHeaders {
    let ua_len = DUCKDUCKGO_USER_AGENTS.len() as u64;
    let lang_len = DUCKDUCKGO_ACCEPT_LANGUAGES.len() as u64;
    DuckDuckGoRequestHeaders {
        user_agent: DUCKDUCKGO_USER_AGENTS[(entropy % ua_len) as usize],
        // Divide out the UA index first so the language is not locked to the
        // User-Agent in a fixed pairing.
        accept_language: DUCKDUCKGO_ACCEPT_LANGUAGES[((entropy / ua_len) % lang_len) as usize],
    }
}

/// Minimum randomized gap between two consecutive DuckDuckGo scrapes.
const DUCKDUCKGO_MIN_GAP_MS: u64 = 500;
/// Maximum randomized gap between two consecutive DuckDuckGo scrapes.
const DUCKDUCKGO_MAX_GAP_MS: u64 = 2_000;

/// Map raw entropy onto the inter-request gap, inclusive at both ends.
fn duckduckgo_gap(entropy: u64) -> Duration {
    let span = DUCKDUCKGO_MAX_GAP_MS - DUCKDUCKGO_MIN_GAP_MS + 1;
    Duration::from_millis(DUCKDUCKGO_MIN_GAP_MS + entropy % span)
}

/// Non-cryptographic entropy for scrape jitter and header rotation.
///
/// Deliberately not `rand`: this crate does not otherwise depend on it, and
/// the requirement here is only "not a fixed, fingerprintable pattern", not
/// unpredictability against an adversary. A seeded xorshift64 over one atomic
/// meets that at zero dependency cost.
fn scrape_entropy() -> u64 {
    static STATE: AtomicU64 = AtomicU64::new(0);

    let mut next = 0u64;
    // `fetch_update` retries on contention, so concurrent callers advance the
    // stream rather than reading the same value.
    let _ = STATE.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |previous| {
        // xorshift64 requires non-zero state; `| 1` guarantees it even if the
        // clock read fails or lands on a zero low word.
        let mut x = if previous == 0 {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0x9E37_79B9_7F4A_7C15)
                | 1
        } else {
            previous
        };
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        next = x;
        Some(x)
    });

    next
}

/// Serializes scrape requests so concurrent searches cannot burst.
#[derive(Default)]
struct ScrapeThrottle {
    /// Earliest instant at which the next scrape may start. `None` until the
    /// first request has gone out.
    next_allowed: tokio::sync::Mutex<Option<Instant>>,
}

impl ScrapeThrottle {
    /// Block until this caller's turn, then reserve the following `gap`.
    ///
    /// The mutex is held across the sleep on purpose: that is what makes
    /// concurrent callers queue behind one another. Releasing it before
    /// sleeping would let every waiter observe the same `next_allowed` and
    /// fire simultaneously — exactly the burst this exists to prevent.
    async fn acquire(&self, gap: Duration) {
        let mut next_allowed = self.next_allowed.lock().await;

        if let Some(deadline) = *next_allowed {
            let now = Instant::now();
            if deadline > now {
                tokio::time::sleep(deadline - now).await;
            }
        }

        *next_allowed = Some(Instant::now() + gap);
    }
}

/// Process-global: DuckDuckGo rate-limits per source address, so the gap has
/// to hold across every agent and tool instance in the process, not per tool.
static DUCKDUCKGO_THROTTLE: LazyLock<ScrapeThrottle> = LazyLock::new(ScrapeThrottle::default);

fn decode_ddg_redirect_url(raw_url: &str) -> String {
    if let Some(index) = raw_url.find("uddg=") {
        let encoded = &raw_url[index + 5..];
        let encoded = encoded.split('&').next().unwrap_or(encoded);
        if let Ok(decoded) = urlencoding::decode(encoded) {
            return decoded.into_owned();
        }
    }

    raw_url.to_string()
}

/// Fluent key for the unconfigured-SearXNG guidance.
const SEARXNG_NOT_CONFIGURED_KEY: &str = "tool-web-search-tool-error-searxng-not-configured";

/// Names only surfaces that exist: no code reads a `SEARXNG_INSTANCE_URL` env
/// var, so the env route is the generic `ZEROCLAW_<path>` override grammar
/// (`.` -> `__`) implemented in `zeroclaw-config::env_overrides`.
static SEARXNG_NOT_CONFIGURED_MESSAGE: LazyLock<String> =
    LazyLock::new(|| crate::i18n::get_required_tool_string(SEARXNG_NOT_CONFIGURED_KEY));

/// Fluent key for the DuckDuckGo block guidance.
const DUCKDUCKGO_BLOCK_MESSAGE_KEY: &str = "tool-web-search-tool-error-duckduckgo-blocked";

/// Addressed to the model, not to a human operator: this text is returned as a
/// tool error and the model is the only reader. Retrying or rephrasing is the
/// default instinct and the worst possible response — it deepens the block — so
/// the message names the recoveries explicitly instead of describing the fault.
static DUCKDUCKGO_BLOCK_MESSAGE: LazyLock<String> =
    LazyLock::new(|| crate::i18n::get_required_tool_string(DUCKDUCKGO_BLOCK_MESSAGE_KEY));

fn duckduckgo_block_message(
    status: reqwest::StatusCode,
    final_url_is_block: bool,
    html_contains_block: bool,
) -> Option<&'static str> {
    if status == reqwest::StatusCode::FORBIDDEN || final_url_is_block || html_contains_block {
        Some(DUCKDUCKGO_BLOCK_MESSAGE.as_str())
    } else {
        None
    }
}

/// Classify a non-2xx HTTP status into a coarse search status for the agent-
/// visible error tag. Called only on the failure path (`!status.is_success()`);
/// 2xx never reaches here.
///
/// These classes are coarse heuristics, not verified provider contracts — a
/// status code alone does not prove why a provider refused the request, and
/// providers differ. 451 stays `Blocked` because RFC 9110 ties it to a
/// legal-refusal reason; 5xx, 429, and 408 are `Unavailable` (provider-side or
/// transient); other non-2xx statuses fall through to `ClientError` (request/
/// credential side). DuckDuckGo's confirmed CAPTCHA block is intercepted
/// upstream by `duckduckgo_block_message`, so this helper only sees non-block
/// failures. The agent should treat the tag as a hint to verify, not a diagnosis.
fn classify_http_status(status: reqwest::StatusCode) -> SearchStatus {
    match status.as_u16() {
        451 => SearchStatus::Blocked, // legal block (RFC-tied refusal reason)
        408 | 429 | 500..=599 => SearchStatus::Unavailable, // provider-side / transient
        _ => SearchStatus::ClientError, // other non-success → request/credential side (coarse)
    }
}

/// Build a provider HTTP-failure error whose message carries a precise
/// `search_status` tag (blocked / unavailable / client_error) and an actionable
/// hint matching the class. The central tool executor owns the failure log
/// record; this helper emits no log of its own.
///
/// The runtime (`tool_execution.rs`) forwards the `Err` returned by `execute`
/// to the agent as readable text, so placing actionable hints in the message
/// makes them visible to the agent.
fn http_search_failure(provider: &str, status: reqwest::StatusCode) -> anyhow::Error {
    let search_status = classify_http_status(status);
    let hint = match search_status {
        SearchStatus::Blocked | SearchStatus::Unavailable => {
            "Provider may be transiently unavailable or blocking the request; retry, or try a different provider (SearXNG, Brave, or Tavily)."
        }
        SearchStatus::ClientError => {
            "The provider refused the request; verify the query, credentials, billing or quota, and provider configuration."
        }
    };
    anyhow::Error::msg(format!(
        "{provider} search failed (search_status={}, http={status}). {hint}",
        search_status.as_str()
    ))
}

fn contains_ascii_case_insensitive(haystack: &str, needle: &str) -> bool {
    haystack
        .as_bytes()
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}

fn strip_tags(content: &str) -> String {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"<[^>]+>").expect("strip_tags regex must compile"));
    RE.replace_all(content, "").to_string()
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search_tool"
    }

    fn description(&self) -> &str {
        "Search the web for information. Returns relevant search results with titles, URLs, and descriptions. Use this to find current information, news, or research topics."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query. Be specific for better results."
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let query = args.get("query").and_then(|q| q.as_str()).ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"param": "query"})),
                "web_search: missing query parameter"
            );
            anyhow::Error::msg("Missing required parameter: query")
        })?;

        if query.trim().is_empty() {
            anyhow::bail!("Search query cannot be empty");
        }

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!("Searching web for: {}", query)
        );

        let resolution = resolve_web_search_provider(&self.model_provider);
        if resolution.used_fallback {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                &format!(
                    "Unknown web search model_provider '{}'; falling back to '{}'",
                    self.model_provider, resolution.canonical_provider
                )
            );
        }

        let result = match resolution.route {
            WebSearchProviderRoute::DuckDuckGo => self.search_duckduckgo(query).await?,
            WebSearchProviderRoute::Brave => self.search_brave(query).await?,
            WebSearchProviderRoute::Tavily => self.search_tavily(query).await?,
            WebSearchProviderRoute::SearXNG => self.search_searxng(query).await?,
            WebSearchProviderRoute::Jina => self.search_jina(query).await?,
            WebSearchProviderRoute::Bocha => self.search_bocha(query).await?,
        };

        Ok(ToolResult {
            success: true,
            output: result.into(),
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_name() {
        let tool = WebSearchTool::new("duckduckgo".to_string(), None, None, 5, 15);
        assert_eq!(tool.name(), "web_search_tool");
    }

    #[test]
    fn test_tool_description() {
        let tool = WebSearchTool::new("duckduckgo".to_string(), None, None, 5, 15);
        assert!(tool.description().contains("Search the web"));
    }

    #[test]
    fn test_parameters_schema() {
        let tool = WebSearchTool::new("duckduckgo".to_string(), None, None, 5, 15);
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["query"].is_object());
    }

    #[test]
    fn test_strip_tags() {
        let html = "<b>Hello</b> <i>World</i>";
        assert_eq!(strip_tags(html), "Hello World");
    }

    #[test]
    fn test_parse_duckduckgo_results_empty() {
        let tool = WebSearchTool::new("duckduckgo".to_string(), None, None, 5, 15);
        let result = tool
            .parse_duckduckgo_results("<html>No results here</html>", "test")
            .unwrap();
        assert!(result.contains("No results found"));
    }

    #[test]
    fn test_parse_duckduckgo_results_with_data() {
        let tool = WebSearchTool::new("duckduckgo".to_string(), None, None, 5, 15);
        let html = r#"
            <a class="result__a" href="https://example.com">Example Title</a>
            <a class="result__snippet">This is a description</a>
        "#;
        let result = tool.parse_duckduckgo_results(html, "test").unwrap();
        assert!(result.contains("Example Title"));
        assert!(result.contains("https://example.com"));
    }

    #[test]
    fn test_parse_duckduckgo_results_decodes_redirect_url() {
        let tool = WebSearchTool::new("duckduckgo".to_string(), None, None, 5, 15);
        let html = r#"
            <a class="result__a" href="https://duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fpath%3Fa%3D1&amp;rut=test">Example Title</a>
            <a class="result__snippet">This is a description</a>
        "#;
        let result = tool.parse_duckduckgo_results(html, "test").unwrap();
        assert!(result.contains("https://example.com/path?a=1"));
        assert!(!result.contains("rut=test"));
    }

    #[test]
    fn test_duckduckgo_block_detection_reports_forbidden_status() {
        let message = duckduckgo_block_message(reqwest::StatusCode::FORBIDDEN, false, false)
            .expect("403 responses should be classified as a DuckDuckGo block");

        assert_eq!(message, DUCKDUCKGO_BLOCK_MESSAGE.as_str());
        assert!(message.contains("SearXNG"));
    }

    #[test]
    fn test_duckduckgo_block_detection_reports_verification_redirect() {
        let message = duckduckgo_block_message(reqwest::StatusCode::OK, true, false)
            .expect("verification redirects should be classified as a DuckDuckGo block");

        assert_eq!(message, DUCKDUCKGO_BLOCK_MESSAGE.as_str());
        assert!(message.contains("SearXNG"));
    }

    #[test]
    fn test_duckduckgo_block_detection_reports_verification_form_in_html() {
        let message = duckduckgo_block_message(reqwest::StatusCode::OK, false, true)
            .expect("verification form HTML should be classified as a DuckDuckGo block");

        assert_eq!(message, DUCKDUCKGO_BLOCK_MESSAGE.as_str());
        assert!(message.contains("SearXNG"));
    }

    #[test]
    fn test_duckduckgo_block_detection_ignores_normal_empty_results() {
        let message = duckduckgo_block_message(reqwest::StatusCode::OK, false, false);

        assert!(message.is_none());
    }

    #[test]
    fn test_duckduckgo_block_detection_is_case_insensitive_without_allocating_html() {
        assert!(contains_ascii_case_insensitive(
            r#"<form action="/WR.DO?u=https%3A%2F%2Fhtml.duckduckgo.com%2Fhtml%2F"></form>"#,
            "/wr.do?"
        ));
    }

    #[test]
    fn http_search_failure_classifies_legal_block_as_blocked() {
        // 451 (legal block) is the one status RFC 9110 ties to a refusal reason,
        // so it is the one status classified as `Blocked`. It must surface
        // search_status=blocked and the "different provider" hint. (403 and other
        // 4xx fall through to client_error — see that case.)
        let err = http_search_failure("brave", reqwest::StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS);
        let msg = format!("{err}");
        assert!(
            msg.contains("search_status=blocked"),
            "451 must tag search_status=blocked, got: {msg}"
        );
        assert!(msg.contains("http=451"));
        assert!(
            msg.contains("different provider"),
            "blocked status must suggest switching providers, got: {msg}"
        );
    }

    #[test]
    fn http_search_failure_classifies_provider_side_failures_as_unavailable() {
        // 5xx outages, 429 rate limiting, and 408 timeout are provider-side or
        // transient — retrying or switching provider is the actionable remedy;
        // each must tag `search_status=unavailable` and surface the "different
        // provider" hint.
        for status in [
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            reqwest::StatusCode::BAD_GATEWAY,
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            reqwest::StatusCode::REQUEST_TIMEOUT,
        ] {
            let err = http_search_failure("searxng", status);
            let msg = format!("{err}");
            assert!(
                msg.contains("search_status=unavailable"),
                "{status} must classify as unavailable, got: {msg}"
            );
            assert!(
                msg.contains(&format!("http={}", status.as_u16())),
                "message must include the HTTP status code, got: {msg}"
            );
            assert!(
                msg.contains("different provider"),
                "unavailable status must suggest switching providers, got: {msg}"
            );
        }
    }

    #[test]
    fn http_search_failure_classifies_client_errors_as_client_error() {
        // 400/401/402/403/404/410 all fall through to client_error as a coarse
        // request/credential-side bucket — a status code alone doesn't prove the
        // cause, so the hint stays neutral and asks the agent to verify, not to
        // switch provider. DuckDuckGo's confirmed-block 403 is intercepted
        // upstream by duckduckgo_block_message.
        for status in [
            reqwest::StatusCode::BAD_REQUEST,
            reqwest::StatusCode::UNAUTHORIZED,
            reqwest::StatusCode::PAYMENT_REQUIRED,
            reqwest::StatusCode::FORBIDDEN,
            reqwest::StatusCode::NOT_FOUND,
            reqwest::StatusCode::GONE,
        ] {
            let err = http_search_failure("tavily", status);
            let msg = format!("{err}");
            assert!(
                msg.contains("search_status=client_error"),
                "{status} must classify as client_error, got: {msg}"
            );
            assert!(
                msg.contains(&format!("http={}", status.as_u16())),
                "message must include the HTTP status code, got: {msg}"
            );
            assert!(
                msg.contains("provider refused the request"),
                "client_error hint must stay neutral, got: {msg}"
            );
            assert!(
                !msg.contains("different provider"),
                "client_error must NOT suggest switching providers, got: {msg}"
            );
        }
    }

    #[tokio::test]
    async fn test_duckduckgo_request_reports_forbidden_status() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/html/"))
            .and(query_param("q", "test"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let tool = WebSearchTool::new("duckduckgo".to_string(), None, None, 5, 15);
        let err = tool
            .search_duckduckgo_at(&format!("{}/html/", server.uri()), "test")
            .await
            .expect_err("403 should be reported as a DuckDuckGo block");

        assert!(err.to_string().contains(DUCKDUCKGO_BLOCK_MESSAGE.as_str()));
        assert!(err.to_string().contains("SearXNG"));
    }

    #[tokio::test]
    async fn test_duckduckgo_request_reports_non_block_failure_with_status_tag() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/html/"))
            .and(query_param("q", "test"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let tool = WebSearchTool::new("duckduckgo".to_string(), None, None, 5, 15);
        let err = tool
            .search_duckduckgo_at(&format!("{}/html/", server.uri()), "test")
            .await
            .expect_err("500 should be reported as a non-block HTTP failure");

        let msg = err.to_string();
        assert!(
            msg.contains("search_status=unavailable"),
            "non-block DDG failure must carry the search_status tag, got: {msg}"
        );
        assert!(
            msg.contains("http=500"),
            "non-block DDG failure must carry the HTTP status code, got: {msg}"
        );
    }

    #[tokio::test]
    async fn test_duckduckgo_request_reports_verification_redirect_url() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/html/"))
            .and(query_param("q", "test"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", format!("{}/wr.do?u=blocked", server.uri())),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/wr.do"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<html></html>"))
            .mount(&server)
            .await;

        let tool = WebSearchTool::new("duckduckgo".to_string(), None, None, 5, 15);
        let err = tool
            .search_duckduckgo_at(&format!("{}/html/", server.uri()), "test")
            .await
            .expect_err("verification redirects should be reported as a DuckDuckGo block");

        assert!(err.to_string().contains(DUCKDUCKGO_BLOCK_MESSAGE.as_str()));
        assert!(err.to_string().contains("SearXNG"));
    }

    #[tokio::test]
    async fn test_duckduckgo_request_reports_verification_form_html() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/html/"))
            .and(query_param("q", "test"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"<form action="/wr.do?u=https%3A%2F%2Fhtml.duckduckgo.com%2Fhtml%2F"></form>"#,
            ))
            .mount(&server)
            .await;

        let tool = WebSearchTool::new("duckduckgo".to_string(), None, None, 5, 15);
        let err = tool
            .search_duckduckgo_at(&format!("{}/html/", server.uri()), "test")
            .await
            .expect_err("verification HTML should be reported as a DuckDuckGo block");

        assert!(err.to_string().contains(DUCKDUCKGO_BLOCK_MESSAGE.as_str()));
        assert!(err.to_string().contains("SearXNG"));
    }

    #[tokio::test]
    async fn test_duckduckgo_request_reports_anomaly_modal_block() {
        // DuckDuckGo's anti-bot page now ships an
        // `anomaly-modal` interstitial (HTTP 200/202, no `/wr.do?` redirect,
        // no verification form), and the old detector slid past it,
        // returning a misleading "No results found" message to the agent.
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/html/"))
            .and(query_param("q", "test"))
            .respond_with(ResponseTemplate::new(202).set_body_string(
                r#"<html><body><div class="anomaly-modal__title">Unusual Traffic Detected</div></body></html>"#,
            ))
            .mount(&server)
            .await;

        let tool = WebSearchTool::new("duckduckgo".to_string(), None, None, 5, 15);
        let err = tool
            .search_duckduckgo_at(&format!("{}/html/", server.uri()), "test")
            .await
            .expect_err("anomaly-modal page should be reported as a DuckDuckGo block");

        assert!(err.to_string().contains(DUCKDUCKGO_BLOCK_MESSAGE.as_str()));
        assert!(err.to_string().contains("SearXNG"));
    }

    #[tokio::test]
    async fn test_duckduckgo_request_preserves_normal_empty_results() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/html/"))
            .and(query_param("q", "test"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string("<html>No results here</html>"),
            )
            .mount(&server)
            .await;

        let tool = WebSearchTool::new("duckduckgo".to_string(), None, None, 5, 15);
        let result = tool
            .search_duckduckgo_at(&format!("{}/html/", server.uri()), "test")
            .await
            .expect("normal empty result HTML should still parse");

        assert!(result.contains("No results found"));
    }

    #[test]
    fn test_constructor_clamps_web_search_limits() {
        let tool = WebSearchTool::new("duckduckgo".to_string(), None, None, 0, 0);
        let html = r#"
            <a class="result__a" href="https://example.com">Example Title</a>
            <a class="result__snippet">This is a description</a>
        "#;
        let result = tool.parse_duckduckgo_results(html, "test").unwrap();
        assert!(result.contains("Example Title"));
    }

    #[tokio::test]
    async fn test_execute_missing_query() {
        let tool = WebSearchTool::new("duckduckgo".to_string(), None, None, 5, 15);
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_execute_empty_query() {
        let tool = WebSearchTool::new("duckduckgo".to_string(), None, None, 5, 15);
        let result = tool.execute(json!({"query": ""})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_execute_brave_without_api_key() {
        let tool = WebSearchTool::new("brave".to_string(), None, None, 5, 15);
        let result = tool.execute(json!({"query": "test"})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("API key"));
    }

    #[test]
    fn test_resolve_brave_api_key_uses_boot_key() {
        let tool = WebSearchTool::new(
            "brave".to_string(),
            Some("sk-plaintext-key".to_string()),
            None,
            5,
            15,
        );
        let key = tool.resolve_brave_api_key().unwrap();
        assert_eq!(key, "sk-plaintext-key");
    }

    #[test]
    fn test_resolve_brave_api_key_reloads_from_config() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");
        std::fs::write(
            &config_path,
            "[web_search]\nbrave_api_key = \"fresh-key-from-disk\"\n",
        )
        .unwrap();

        // No boot key -- forces reload from config
        let tool = WebSearchTool::new_with_config(
            "brave".to_string(),
            None,
            None,
            None,
            None,
            5,
            15,
            config_path,
            false,
        );
        let key = tool.resolve_brave_api_key().unwrap();
        assert_eq!(key, "fresh-key-from-disk");
    }

    #[test]
    fn test_resolve_brave_api_key_decrypts_encrypted_key() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = zeroclaw_config::secrets::SecretStore::new(tmp.path(), true);
        let encrypted = store.encrypt("brave-secret-key").unwrap();

        let config_path = tmp.path().join("config.toml");
        std::fs::write(
            &config_path,
            format!("[web_search]\nbrave_api_key = \"{}\"\n", encrypted),
        )
        .unwrap();

        // Boot key is the encrypted blob -- should trigger reload + decrypt
        let tool = WebSearchTool::new_with_config(
            "brave".to_string(),
            Some(encrypted),
            None,
            None,
            None,
            5,
            15,
            config_path,
            true,
        );
        let key = tool.resolve_brave_api_key().unwrap();
        assert_eq!(key, "brave-secret-key");
    }

    #[tokio::test]
    async fn test_execute_searxng_without_instance_url() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");
        std::fs::write(&config_path, "[web_search]\n").unwrap();

        let tool = WebSearchTool::new_with_config(
            "searxng".to_string(),
            None,
            None,
            None,
            None,
            5,
            15,
            config_path,
            false,
        );
        let result = tool.execute(json!({"query": "test"})).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains(SEARXNG_NOT_CONFIGURED_MESSAGE.as_str())
        );
    }

    #[test]
    fn test_parse_tavily_results_empty() {
        let tool = WebSearchTool::new("tavily".to_string(), None, None, 5, 15);
        let json = serde_json::json!({"results": []});
        let result = tool.parse_tavily_results(&json, "test").unwrap();
        assert!(result.contains("No results found"));
    }

    #[test]
    fn test_parse_tavily_results_with_data() {
        let tool = WebSearchTool::new("tavily".to_string(), None, None, 5, 15);
        let json = serde_json::json!({
            "query": "test",
            "results": [
                {
                    "title": "Tavily Example",
                    "url": "https://example.com",
                    "content": "Pre-cleaned summary content from Tavily",
                    "score": 0.91
                },
                {
                    "title": "Another Result",
                    "url": "https://example.org",
                    "content": "Second result body"
                }
            ]
        });
        let result = tool.parse_tavily_results(&json, "test").unwrap();
        assert!(result.contains("Tavily Example"));
        assert!(result.contains("https://example.com"));
        assert!(result.contains("Pre-cleaned summary content from Tavily"));
        assert!(result.contains("via Tavily"));
    }

    #[test]
    fn test_parse_tavily_results_invalid_response() {
        let tool = WebSearchTool::new("tavily".to_string(), None, None, 5, 15);
        let json = serde_json::json!({"error": "bad api key"});
        let result = tool.parse_tavily_results(&json, "test");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Invalid Tavily API response")
        );
    }

    #[tokio::test]
    async fn test_execute_tavily_without_api_key() {
        // No boot key + no config field → resolve_tavily_api_key must error
        // before any network call is attempted.
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.toml");
        std::fs::write(&config_path, "[web_search]\n").unwrap();

        let tool = WebSearchTool::new_with_config(
            "tavily".to_string(),
            None,
            None,
            None,
            None,
            5,
            15,
            config_path,
            false,
        );
        let result = tool.execute(json!({"query": "test"})).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Tavily API key not configured")
        );
    }

    #[test]
    fn test_resolve_tavily_api_key_uses_boot_key() {
        let tool = WebSearchTool::new_with_config(
            "tavily".to_string(),
            None,
            Some("tvly-boot-key".to_string()),
            None,
            None,
            5,
            15,
            PathBuf::new(),
            false,
        );
        let key = tool.resolve_tavily_api_key().unwrap();
        assert_eq!(key, "tvly-boot-key");
    }

    #[test]
    fn test_resolve_tavily_api_key_reloads_from_config() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.toml");
        std::fs::write(
            &config_path,
            "[web_search]\ntavily_api_key = \"tvly-fresh-from-disk\"\n",
        )
        .unwrap();

        // No boot key — forces reload from config
        let tool = WebSearchTool::new_with_config(
            "tavily".to_string(),
            None,
            None,
            None,
            None,
            5,
            15,
            config_path,
            false,
        );
        let key = tool.resolve_tavily_api_key().unwrap();
        assert_eq!(key, "tvly-fresh-from-disk");
    }

    #[test]
    fn test_resolve_tavily_api_key_decrypts_encrypted_key() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = zeroclaw_config::secrets::SecretStore::new(tmp.path(), true);
        let encrypted = store.encrypt("tvly-secret-key").unwrap();

        let config_path = tmp.path().join("config.toml");
        std::fs::write(
            &config_path,
            format!("[web_search]\ntavily_api_key = \"{}\"\n", encrypted),
        )
        .unwrap();

        // Boot key is the encrypted blob -- should trigger reload + decrypt
        let tool = WebSearchTool::new_with_config(
            "tavily".to_string(),
            None,
            None,
            Some(encrypted),
            None,
            5,
            15,
            config_path,
            true,
        );
        let key = tool.resolve_tavily_api_key().unwrap();
        assert_eq!(key, "tvly-secret-key");
    }

    #[tokio::test]
    async fn test_tavily_request_uses_bearer_auth_header_not_body_field() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/search"))
            .and(header("authorization", "Bearer tvly-test-key"))
            .and(header("content-type", "application/json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "query": "what is rust",
                "results": []
            })))
            .mount(&server)
            .await;

        let tool = WebSearchTool::new_with_config(
            "tavily".to_string(),
            None,
            Some("tvly-test-key".to_string()),
            None,
            None,
            5,
            15,
            PathBuf::new(),
            false,
        );

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .expect("client builder should succeed without a proxy");
        let result = tool
            .search_tavily_with_client(&client, &format!("{}/search", server.uri()), "what is rust")
            .await
            .expect("request should succeed against the mock");
        assert!(
            result.contains("No results found"),
            "parser should report empty results: {result}"
        );

        let recorded = server
            .received_requests()
            .await
            .expect("wiremock should have captured the request");
        assert_eq!(recorded.len(), 1, "expected exactly one POST /search");

        let body: serde_json::Value =
            serde_json::from_slice(&recorded[0].body).expect("body should be JSON");

        // Auth must NOT leak into the body — bearer header is the only auth channel.
        assert!(
            body.get("api_key").is_none(),
            "api_key must not appear in the request body; got: {body}"
        );

        // The documented body fields must still be present so the search
        // contract continues to match the upstream API spec.
        assert_eq!(body["query"], "what is rust");
        assert_eq!(body["search_depth"], "basic");
        assert_eq!(body["max_results"], 5);
        assert_eq!(body["include_answer"], false);
        assert_eq!(body["include_raw_content"], false);
    }

    #[test]
    fn test_parse_searxng_results_empty() {
        let tool = WebSearchTool::new("searxng".to_string(), None, None, 5, 15);
        let json = serde_json::json!({"results": []});
        let result = tool.parse_searxng_results(&json, "test").unwrap();
        assert!(result.contains("No results found"));
    }

    #[test]
    fn test_parse_searxng_results_with_data() {
        let tool = WebSearchTool::new("searxng".to_string(), None, None, 5, 15);
        let json = serde_json::json!({
            "results": [
                {
                    "title": "SearXNG Example",
                    "url": "https://example.com",
                    "content": "A privacy-respecting metasearch engine"
                },
                {
                    "title": "Another Result",
                    "url": "https://example.org",
                    "content": "More information here"
                }
            ]
        });
        let result = tool.parse_searxng_results(&json, "test").unwrap();
        assert!(result.contains("SearXNG Example"));
        assert!(result.contains("https://example.com"));
        assert!(result.contains("A privacy-respecting metasearch engine"));
        assert!(result.contains("via SearXNG"));
    }

    #[test]
    fn test_parse_searxng_results_invalid_response() {
        let tool = WebSearchTool::new("searxng".to_string(), None, None, 5, 15);
        let json = serde_json::json!({"error": "bad request"});
        let result = tool.parse_searxng_results(&json, "test");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Invalid SearXNG API response")
        );
    }

    #[test]
    fn test_resolve_searxng_instance_url_from_boot() {
        let tool = WebSearchTool {
            model_provider: "searxng".into(),
            boot_brave_api_key: None,
            boot_tavily_api_key: None,
            boot_jina_api_key: None,
            searxng_instance_url: Some("https://searx.example.com".to_string()),
            max_results: 5,
            timeout_secs: 15,
            config_path: PathBuf::new(),
            secrets_encrypt: false,
        };
        let url = tool.resolve_searxng_instance_url().unwrap();
        assert_eq!(url, "https://searx.example.com");
    }

    #[test]
    fn test_resolve_searxng_instance_url_reloads_from_config() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");
        std::fs::write(
            &config_path,
            "[web_search]\nsearxng_instance_url = \"https://search.local\"\n",
        )
        .unwrap();

        let tool = WebSearchTool::new_with_config(
            "searxng".to_string(),
            None,
            None,
            None,
            None,
            5,
            15,
            config_path,
            false,
        );
        let url = tool.resolve_searxng_instance_url().unwrap();
        assert_eq!(url, "https://search.local");
    }

    #[test]
    fn test_resolve_brave_api_key_picks_up_runtime_update() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");

        // Start with no key in config
        std::fs::write(&config_path, "[web_search]\n").unwrap();

        let tool = WebSearchTool::new_with_config(
            "brave".to_string(),
            None,
            None,
            None,
            None,
            5,
            15,
            config_path.clone(),
            false,
        );

        // Key not configured yet -- should fail
        assert!(tool.resolve_brave_api_key().is_err());

        // Simulate runtime config update (e.g. via web_search_config set)
        std::fs::write(
            &config_path,
            "[web_search]\nbrave_api_key = \"runtime-updated-key\"\n",
        )
        .unwrap();

        // Now should succeed with the updated key
        let key = tool.resolve_brave_api_key().unwrap();
        assert_eq!(key, "runtime-updated-key");
    }

    #[test]
    fn test_resolve_jina_api_key_uses_boot_key() {
        let tool = WebSearchTool::new_with_config(
            "jina".to_string(),
            None,
            None,
            Some("jina-boot-key".to_string()),
            None,
            5,
            15,
            PathBuf::new(),
            false,
        );
        let key = tool.resolve_jina_api_key().unwrap();
        assert_eq!(key, "jina-boot-key");
    }

    #[test]
    fn test_resolve_jina_api_key_reloads_from_config() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.toml");
        std::fs::write(
            &config_path,
            "[web_search]\njina_api_key = \"jina-fresh-from-disk\"\n",
        )
        .unwrap();

        // No boot key — forces reload from config
        let tool = WebSearchTool::new_with_config(
            "jina".to_string(),
            None,
            None,
            None,
            None,
            5,
            15,
            config_path,
            false,
        );
        let key = tool.resolve_jina_api_key().unwrap();
        assert_eq!(key, "jina-fresh-from-disk");
    }

    #[test]
    fn test_parse_jina_results_empty() {
        let tool = WebSearchTool::new("jina".to_string(), None, None, 5, 15);
        // Jina API returns {"code": 200, "status": 20000, "data": [...]}
        let json = serde_json::json!({"data": []});
        let result = tool.parse_jina_results(&json, "test").unwrap();
        assert!(result.contains("No results found"));
    }

    #[test]
    fn test_parse_jina_results_with_data() {
        let tool = WebSearchTool::new("jina".to_string(), None, None, 5, 15);
        // Jina API returns {"code": 200, "status": 20000, "data": [...]}
        let json = serde_json::json!({
            "data": [
                {
                    "title": "Jina AI",
                    "url": "https://jina.ai/",
                    "content": "Best-in-class embeddings, rerankers, web reader, deepsearch"
                },
                {
                    "title": "Jina AI on GitHub",
                    "url": "https://github.com/jina-ai",
                    "description": "Open-source AI infrastructure"
                }
            ]
        });
        let result = tool.parse_jina_results(&json, "test").unwrap();
        assert!(result.contains("Jina AI"));
        assert!(result.contains("https://jina.ai/"));
        assert!(result.contains("via Jina AI"));
        // content field should be read when available
        assert!(result.contains("Best-in-class embeddings"));
    }

    #[test]
    fn test_parse_jina_results_falls_back_to_description() {
        let tool = WebSearchTool::new("jina".to_string(), None, None, 5, 15);
        // When content is absent, fall back to description
        let json = serde_json::json!({
            "data": [
                {
                    "title": "Test",
                    "url": "https://example.com",
                    "description": "Fallback description"
                }
            ]
        });
        let result = tool.parse_jina_results(&json, "test").unwrap();
        assert!(result.contains("Fallback description"));
    }

    #[test]
    fn test_parse_jina_results_invalid_response() {
        let tool = WebSearchTool::new("jina".to_string(), None, None, 5, 15);
        let json = serde_json::json!({"error": "bad api key"});
        let result = tool.parse_jina_results(&json, "test");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Invalid Jina AI API response")
        );
    }

    #[tokio::test]
    async fn test_execute_jina_without_api_key() {
        // No boot key + no config field → resolve_jina_api_key must error
        // before any network call is attempted.
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.toml");
        std::fs::write(&config_path, "[web_search]\n").unwrap();

        let tool = WebSearchTool::new_with_config(
            "jina".to_string(),
            None,
            None,
            None,
            None,
            5,
            15,
            config_path,
            false,
        );
        let result = tool.execute(json!({"query": "test"})).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Jina AI API key not configured")
        );
    }

    /// Build a Bocha-routed tool over `config_path`. There is no boot-time
    /// Bocha key parameter by design — the key always comes from config.
    fn bocha_tool(config_path: PathBuf, secrets_encrypt: bool) -> WebSearchTool {
        WebSearchTool::new_with_config(
            "bocha".to_string(),
            None,
            None,
            None,
            None,
            5,
            15,
            config_path,
            secrets_encrypt,
        )
    }

    #[tokio::test]
    async fn test_execute_bocha_without_api_key() {
        // No config field → resolve_bocha_api_key must error before any
        // network call is attempted.
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.toml");
        std::fs::write(&config_path, "[web_search]\n").unwrap();

        let tool = bocha_tool(config_path, false);
        let result = tool.execute(json!({"query": "test"})).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Bocha AI API key not configured")
        );
    }

    #[test]
    fn test_resolve_bocha_api_key_reads_from_config() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");
        std::fs::write(
            &config_path,
            "[web_search]\nbocha_api_key = \"fresh-bocha-from-disk\"\n",
        )
        .unwrap();

        let tool = bocha_tool(config_path, false);
        let key = tool.resolve_bocha_api_key().unwrap();
        assert_eq!(key, "fresh-bocha-from-disk");
    }

    #[test]
    fn test_resolve_bocha_api_key_decrypts_encrypted_key() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = zeroclaw_config::secrets::SecretStore::new(tmp.path(), true);
        let encrypted = store.encrypt("bocha-secret-key").unwrap();

        let config_path = tmp.path().join("config.toml");
        std::fs::write(
            &config_path,
            format!("[web_search]\nbocha_api_key = \"{}\"\n", encrypted),
        )
        .unwrap();

        let tool = bocha_tool(config_path, true);
        let key = tool.resolve_bocha_api_key().unwrap();
        assert_eq!(key, "bocha-secret-key");
    }

    #[test]
    fn test_resolve_bocha_api_key_tracks_rotation_and_removal() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");
        std::fs::write(
            &config_path,
            "[web_search]\nbocha_api_key = \"initial-key\"\n",
        )
        .unwrap();

        let tool = bocha_tool(config_path.clone(), false);
        assert_eq!(tool.resolve_bocha_api_key().unwrap(), "initial-key");

        // Operator rotates the key on disk — same tool instance must pick
        // up the new value.
        std::fs::write(
            &config_path,
            "[web_search]\nbocha_api_key = \"rotated-key\"\n",
        )
        .unwrap();
        assert_eq!(tool.resolve_bocha_api_key().unwrap(), "rotated-key");

        // Operator removes the key — the tool must fail instead of serving
        // any previously observed value.
        std::fs::write(&config_path, "[web_search]\n").unwrap();
        let result = tool.resolve_bocha_api_key();
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Bocha AI API key not configured")
        );
    }

    #[test]
    fn test_parse_bocha_results_empty() {
        let tool = WebSearchTool::new("bocha".to_string(), None, None, 5, 15);
        let json = serde_json::json!({
            "code": 200,
            "msg": null,
            "data": {"webPages": {"value": []}}
        });
        let result = tool.parse_bocha_results(&json, "test").unwrap();
        assert!(result.contains("No results found"));
    }

    #[test]
    fn test_parse_bocha_results_with_data() {
        let tool = WebSearchTool::new("bocha".to_string(), None, None, 5, 15);
        let json = serde_json::json!({
            "code": 200,
            "msg": null,
            "data": {
                "webPages": {
                    "totalEstimatedMatches": 42,
                    "value": [
                        {
                            "name": "Bocha Example Title",
                            "url": "https://example.com/a",
                            "snippet": "raw snippet body",
                            "summary": "AI summary of the page",
                            "siteName": "Example Site",
                            "datePublished": "2025-01-15"
                        },
                        {
                            "name": "Second Result",
                            "url": "https://example.org/b",
                            "snippet": "second snippet only",
                            "siteName": "Org Site"
                        }
                    ]
                }
            }
        });
        let result = tool.parse_bocha_results(&json, "test").unwrap();
        assert!(result.contains("via Bocha"));
        assert!(result.contains("Bocha Example Title"));
        assert!(result.contains("https://example.com/a"));
        // AI summary preferred over the raw snippet when both are present.
        assert!(result.contains("AI summary of the page"));
        assert!(!result.contains("raw snippet body"));
        // Attribution line combines siteName and date.
        assert!(result.contains("Example Site · 2025-01-15"));
        // Snippet fallback when summary is absent.
        assert!(result.contains("second snippet only"));
    }

    #[test]
    fn test_parse_bocha_results_surfaces_business_error() {
        // Bocha reports business-logic failures as HTTP 200 with a non-200
        // `code` in the body — the parser must surface them instead of
        // returning a misleading "No results found".
        let tool = WebSearchTool::new("bocha".to_string(), None, None, 5, 15);
        let json = serde_json::json!({
            "code": 403,
            "msg": "Insufficient balance",
            "data": null
        });
        let result = tool.parse_bocha_results(&json, "test");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("code 403"));
        assert!(err.contains("Insufficient balance"));
    }

    #[test]
    fn test_parse_bocha_results_invalid_response() {
        let tool = WebSearchTool::new("bocha".to_string(), None, None, 5, 15);
        let json = serde_json::json!({"unexpected": "shape"});
        let result = tool.parse_bocha_results(&json, "test");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Invalid Bocha AI API response")
        );
    }

    #[tokio::test]
    async fn test_bocha_request_uses_bearer_auth_and_documented_body() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/web-search"))
            .and(header("authorization", "Bearer bocha-test-key"))
            .and(header("content-type", "application/json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 200,
                "msg": null,
                "data": {"webPages": {"value": []}}
            })))
            .mount(&server)
            .await;

        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.toml");
        std::fs::write(
            &config_path,
            "[web_search]\nbocha_api_key = \"bocha-test-key\"\n",
        )
        .unwrap();
        let tool = bocha_tool(config_path, false);

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .expect("client builder should succeed without a proxy");
        let result = tool
            .search_bocha_with_client(
                &client,
                &format!("{}/v1/web-search", server.uri()),
                "什么是 Rust",
            )
            .await
            .expect("request should succeed against the mock");
        assert!(
            result.contains("No results found"),
            "parser should report empty results: {result}"
        );

        let recorded = server
            .received_requests()
            .await
            .expect("wiremock should have captured the request");
        assert_eq!(
            recorded.len(),
            1,
            "expected exactly one POST /v1/web-search"
        );

        let body: serde_json::Value =
            serde_json::from_slice(&recorded[0].body).expect("body should be JSON");

        // Auth must NOT leak into the body — bearer header is the only auth channel.
        assert!(body.get("api_key").is_none());
        assert!(body.get("apiKey").is_none());
        assert!(body.get("token").is_none());

        assert_eq!(body["query"], "什么是 Rust");
        assert_eq!(body["count"], 5);
        assert_eq!(body["summary"], true);
        assert_eq!(body["freshness"], "noLimit");
    }

    // ── Format characterization ──────────────────────────────────────────
    //
    // These pin the *exact* rendered output of every provider parser for
    // inputs that sit comfortably under both caps. They were written against
    // the pre-cap implementation and must keep passing afterwards: capping is
    // only allowed to change output that actually exceeds a cap.

    #[test]
    fn ddg_render_format_is_stable_under_caps() {
        let tool = WebSearchTool::new("duckduckgo".to_string(), None, None, 5, 15);
        let html = r#"
            <a class="result__a" href="https://example.com/one">First Title</a>
            <a class="result__a" href="https://example.org/two">Second Title</a>
            <a class="result__snippet">First snippet</a>
            <a class="result__snippet">Second snippet</a>
        "#;
        let result = tool.parse_duckduckgo_results(html, "rust").unwrap();
        assert_eq!(
            result,
            "Search results for: rust (via DuckDuckGo)\n\
             1. First Title\n   https://example.com/one\n   First snippet\n\
             2. Second Title\n   https://example.org/two\n   Second snippet"
        );
    }

    #[test]
    fn brave_render_format_is_stable_under_caps() {
        let tool = WebSearchTool::new("brave".to_string(), None, None, 5, 15);
        let json = serde_json::json!({"web": {"results": [
            {"title": "First Title", "url": "https://example.com/one", "description": "First body"},
            {"title": "Second Title", "url": "https://example.org/two", "description": ""},
        ]}});
        let result = tool.parse_brave_results(&json, "rust").unwrap();
        assert_eq!(
            result,
            "Search results for: rust (via Brave)\n\
             1. First Title\n   https://example.com/one\n   First body\n\
             2. Second Title\n   https://example.org/two"
        );
    }

    #[test]
    fn tavily_render_format_is_stable_under_caps() {
        let tool = WebSearchTool::new("tavily".to_string(), None, None, 5, 15);
        let json = serde_json::json!({"results": [
            {"title": "First Title", "url": "https://example.com/one", "content": "First body"},
        ]});
        let result = tool.parse_tavily_results(&json, "rust").unwrap();
        assert_eq!(
            result,
            "Search results for: rust (via Tavily)\n1. First Title\n   https://example.com/one\n   First body"
        );
    }

    #[test]
    fn searxng_render_format_is_stable_under_caps() {
        let tool = WebSearchTool::new("searxng".to_string(), None, None, 5, 15);
        let json = serde_json::json!({"results": [
            {"title": "First Title", "url": "https://example.com/one", "content": "First body"},
        ]});
        let result = tool.parse_searxng_results(&json, "rust").unwrap();
        assert_eq!(
            result,
            "Search results for: rust (via SearXNG)\n1. First Title\n   https://example.com/one\n   First body"
        );
    }

    #[test]
    fn jina_render_format_is_stable_under_caps() {
        let tool = WebSearchTool::new("jina".to_string(), None, None, 5, 15);
        let json = serde_json::json!({"data": [
            {"title": "First Title", "url": "https://example.com/one", "content": "First body"},
            {"title": "Second Title", "url": "https://example.org/two", "description": "Fallback body"},
        ]});
        let result = tool.parse_jina_results(&json, "rust").unwrap();
        assert_eq!(
            result,
            "Search results for: rust (via Jina AI)\n\
             1. First Title\n   https://example.com/one\n   First body\n\
             2. Second Title\n   https://example.org/two\n   Fallback body"
        );
    }

    #[test]
    fn bocha_render_format_is_stable_under_caps() {
        let tool = WebSearchTool::new("bocha".to_string(), None, None, 5, 15);
        let json = serde_json::json!({"code": 200, "data": {"webPages": {"value": [
            {
                "name": "First Title",
                "url": "https://example.com/one",
                "summary": "AI summary",
                "snippet": "raw snippet",
                "siteName": "Example Site",
                "datePublished": "2025-01-15"
            },
            {"name": "Second Title", "url": "https://example.org/two", "snippet": "raw only"},
        ]}}});
        let result = tool.parse_bocha_results(&json, "rust").unwrap();
        assert_eq!(
            result,
            "Search results for: rust (via Bocha)\n\
             1. First Title\n   https://example.com/one\n   Example Site · 2025-01-15\n   AI summary\n\
             2. Second Title\n   https://example.org/two\n   raw only"
        );
    }

    // ── Per-result content cap ───────────────────────────────────────────

    #[test]
    fn cap_result_content_leaves_text_at_or_under_the_cap_untouched() {
        let exact = "x".repeat(MAX_RESULT_CONTENT_CHARS);
        assert_eq!(cap_result_content(&exact), exact);

        let under = "x".repeat(MAX_RESULT_CONTENT_CHARS - 1);
        assert_eq!(cap_result_content(&under), under);

        assert_eq!(cap_result_content(""), "");
    }

    #[test]
    fn cap_result_content_marks_truncation_one_char_over_the_cap() {
        let over = "x".repeat(MAX_RESULT_CONTENT_CHARS + 1);
        let capped = cap_result_content(&over);

        assert!(capped.ends_with("..."), "truncation must stay visible");
        assert_eq!(
            capped.chars().count(),
            MAX_RESULT_CONTENT_CHARS + 3,
            "cap keeps exactly {MAX_RESULT_CONTENT_CHARS} chars plus the marker"
        );
    }

    /// The cap counts Unicode characters, so a multibyte body is never split
    /// mid-codepoint. `é` is 2 bytes and `😀` is 4, so a byte-based cut at 500
    /// would land inside a character for both and produce invalid UTF-8.
    #[test]
    fn cap_result_content_never_splits_a_multibyte_character() {
        for filler in ["é", "猫", "😀"] {
            let body = filler.repeat(MAX_RESULT_CONTENT_CHARS + 50);
            let capped = cap_result_content(&body);

            assert_eq!(
                capped.chars().count(),
                MAX_RESULT_CONTENT_CHARS + 3,
                "char count must be the unit for {filler}"
            );
            assert!(capped.ends_with("..."));
            // Every retained char must be intact — a split codepoint would
            // have produced a replacement char or a shorter char count.
            assert!(
                capped.trim_end_matches('.').chars().all(|c| {
                    let mut buf = [0u8; 4];
                    c.encode_utf8(&mut buf) == filler
                }),
                "no character was mangled for {filler}"
            );
            // Byte length confirms the cut respected multibyte widths.
            assert_eq!(
                capped.len(),
                MAX_RESULT_CONTENT_CHARS * filler.len() + 3,
                "byte length must reflect whole characters for {filler}"
            );
        }
    }

    /// The cap must be applied by every provider parser — a provider that
    /// skips it re-opens the context-flooding hole for its own users.
    #[test]
    fn every_provider_parser_caps_result_content() {
        let long = "L".repeat(MAX_RESULT_CONTENT_CHARS + 400);
        let expected = format!("{}...", "L".repeat(MAX_RESULT_CONTENT_CHARS));

        let ddg = WebSearchTool::new("duckduckgo".to_string(), None, None, 5, 15);
        let html = format!(
            r#"<a class="result__a" href="https://example.com">T</a>
               <a class="result__snippet">{long}</a>"#
        );
        let rendered = ddg.parse_duckduckgo_results(&html, "q").unwrap();
        assert!(rendered.contains(&expected), "duckduckgo did not cap");
        assert!(!rendered.contains(&long), "duckduckgo leaked full content");

        let brave = WebSearchTool::new("brave".to_string(), None, None, 5, 15);
        let rendered = brave
            .parse_brave_results(
                &serde_json::json!({"web": {"results": [
                    {"title": "T", "url": "https://example.com", "description": long}
                ]}}),
                "q",
            )
            .unwrap();
        assert!(rendered.contains(&expected), "brave did not cap");
        assert!(!rendered.contains(&long), "brave leaked full content");

        let tavily = WebSearchTool::new("tavily".to_string(), None, None, 5, 15);
        let rendered = tavily
            .parse_tavily_results(
                &serde_json::json!({"results": [
                    {"title": "T", "url": "https://example.com", "content": long}
                ]}),
                "q",
            )
            .unwrap();
        assert!(rendered.contains(&expected), "tavily did not cap");
        assert!(!rendered.contains(&long), "tavily leaked full content");

        let searxng = WebSearchTool::new("searxng".to_string(), None, None, 5, 15);
        let rendered = searxng
            .parse_searxng_results(
                &serde_json::json!({"results": [
                    {"title": "T", "url": "https://example.com", "content": long}
                ]}),
                "q",
            )
            .unwrap();
        assert!(rendered.contains(&expected), "searxng did not cap");
        assert!(!rendered.contains(&long), "searxng leaked full content");

        // Jina: both the primary `content` field and the `description`
        // fallback have to go through the cap.
        let jina = WebSearchTool::new("jina".to_string(), None, None, 5, 15);
        let rendered = jina
            .parse_jina_results(
                &serde_json::json!({"data": [
                    {"title": "T", "url": "https://example.com", "content": long},
                    {"title": "T2", "url": "https://example.org", "description": long}
                ]}),
                "q",
            )
            .unwrap();
        assert_eq!(
            rendered.matches(&expected).count(),
            2,
            "jina must cap both the content field and the description fallback"
        );
        assert!(!rendered.contains(&long), "jina leaked full content");

        // Bocha: both the preferred AI `summary` and the `snippet` fallback.
        let bocha = WebSearchTool::new("bocha".to_string(), None, None, 5, 15);
        let rendered = bocha
            .parse_bocha_results(
                &serde_json::json!({"code": 200, "data": {"webPages": {"value": [
                    {"name": "T", "url": "https://example.com", "summary": long},
                    {"name": "T2", "url": "https://example.org", "snippet": long}
                ]}}}),
                "q",
            )
            .unwrap();
        assert_eq!(
            rendered.matches(&expected).count(),
            2,
            "bocha must cap both the AI summary and the snippet fallback"
        );
        assert!(!rendered.contains(&long), "bocha leaked full content");
    }

    // ── Total output cap ─────────────────────────────────────────────────

    #[test]
    fn render_results_leaves_output_under_the_total_cap_unchanged() {
        let rendered = render_results(
            "header".to_string(),
            vec![vec!["1. a".to_string()], vec!["2. b".to_string()]],
        );

        assert_eq!(rendered, "header\n1. a\n2. b");
        assert!(!rendered.contains(TRUNCATED_RESULTS_NOTE.as_str()));
    }

    #[test]
    fn render_results_drops_whole_results_and_notes_the_omission() {
        // Each block is ~4000 chars, so the 16000-char budget admits four and
        // has to drop the rest.
        let blocks: Vec<Vec<String>> = (1..=8)
            .map(|i| vec![format!("{i}. {}", "x".repeat(4_000))])
            .collect();

        let rendered = render_results("header".to_string(), blocks);

        assert!(
            rendered.ends_with(TRUNCATED_RESULTS_NOTE.as_str()),
            "omission note must be the last thing the model reads"
        );
        assert!(
            rendered.chars().count()
                <= MAX_TOTAL_OUTPUT_CHARS + TRUNCATED_RESULTS_NOTE.chars().count() + 1,
            "total output must stay within the budget plus the note"
        );
        assert!(rendered.contains("\n1. "), "first result must survive");
        assert!(
            !rendered.contains("\n8. "),
            "overflowing results are dropped"
        );
        // Trimming is at whole-result granularity: any result that appears at
        // all appears in full, never cut mid-line.
        for i in 1..=8 {
            let block = format!("\n{i}. {}", "x".repeat(4_000));
            if rendered.contains(&format!("\n{i}. ")) {
                assert!(
                    rendered.contains(&block),
                    "result {i} was emitted but truncated mid-result"
                );
            }
        }
    }

    /// A single result can exceed the whole budget on its own, because titles
    /// and URLs are deliberately not content-capped. The first result is still
    /// emitted (a bare header helps nobody) but is hard-cut, and the note
    /// survives on top of the budget.
    #[test]
    fn render_results_hard_caps_a_single_oversized_result() {
        let rendered = render_results(
            "header".to_string(),
            vec![vec![format!(
                "1. {}",
                "x".repeat(MAX_TOTAL_OUTPUT_CHARS * 2)
            )]],
        );

        assert!(rendered.ends_with(TRUNCATED_RESULTS_NOTE.as_str()));
        assert!(rendered.starts_with("header\n1. x"));
        assert!(
            rendered.chars().count()
                <= MAX_TOTAL_OUTPUT_CHARS + TRUNCATED_RESULTS_NOTE.chars().count() + 4,
            "hard cap must bound even a single oversized result: {}",
            rendered.chars().count()
        );
    }

    /// End-to-end through a real parser rather than the helper alone.
    #[test]
    fn total_cap_applies_through_a_provider_parser() {
        let tool = WebSearchTool::new("brave".to_string(), None, None, 10, 15);
        let results: Vec<serde_json::Value> = (1..=10)
            .map(|i| {
                serde_json::json!({
                    // Titles are not content-capped, so they are the realistic
                    // way a provider can still blow the total budget.
                    "title": format!("{i} {}", "T".repeat(5_000)),
                    "url": "https://example.com",
                    "description": "short"
                })
            })
            .collect();

        let rendered = tool
            .parse_brave_results(&serde_json::json!({"web": {"results": results}}), "q")
            .unwrap();

        assert!(rendered.ends_with(TRUNCATED_RESULTS_NOTE.as_str()));
        assert!(
            rendered.chars().count()
                <= MAX_TOTAL_OUTPUT_CHARS + TRUNCATED_RESULTS_NOTE.chars().count() + 4
        );
        assert!(rendered.contains("via Brave"), "header must survive");

        // Trimming must drop whole results, not fall through to the hard-cut
        // backstop: every title that appears at all appears in full.
        for i in 1..=10 {
            let title = format!("\n{i} {}", "T".repeat(5_000));
            if rendered.contains(&format!("\n{i} ")) {
                assert!(
                    rendered.contains(&title),
                    "result {i} was cut mid-title instead of being dropped whole"
                );
            }
        }
        assert!(
            !rendered.contains("...\n"),
            "no result should have been hard-cut: block dropping should have sufficed"
        );
    }

    // ── Echoed-input bounds ──────────────────────────────────────────────

    /// Build every provider's empty-result reply for one query.
    fn empty_result_replies(query: &str) -> Vec<(&'static str, String)> {
        let tool = |provider: &str| WebSearchTool::new(provider.to_string(), None, None, 5, 15);

        vec![
            (
                "duckduckgo",
                tool("duckduckgo")
                    .parse_duckduckgo_results("<html><body>nothing</body></html>", query)
                    .unwrap(),
            ),
            (
                "brave",
                tool("brave")
                    .parse_brave_results(&serde_json::json!({"web": {"results": []}}), query)
                    .unwrap(),
            ),
            (
                "tavily",
                tool("tavily")
                    .parse_tavily_results(&serde_json::json!({"results": []}), query)
                    .unwrap(),
            ),
            (
                "searxng",
                tool("searxng")
                    .parse_searxng_results(&serde_json::json!({"results": []}), query)
                    .unwrap(),
            ),
            (
                "jina",
                tool("jina")
                    .parse_jina_results(&serde_json::json!({"data": []}), query)
                    .unwrap(),
            ),
            (
                "bocha",
                tool("bocha")
                    .parse_bocha_results(
                        &serde_json::json!({"code": 200, "data": {"webPages": {"value": []}}}),
                        query,
                    )
                    .unwrap(),
            ),
        ]
    }

    /// The empty-result reply echoes the caller's query, which is
    /// model-controlled and unbounded. It returns before `render_results`, so
    /// nothing else bounds it: without its own cap a huge query walks straight
    /// past the advertised total cap on every provider.
    #[test]
    fn empty_result_replies_bound_the_echoed_query() {
        // Multibyte on purpose: the cut must count characters, not bytes.
        let query = "漢".repeat(MAX_QUERY_ECHO_CHARS + 5_000);
        let prefix_chars = "No results found for: ".chars().count();

        for (provider, rendered) in empty_result_replies(&query) {
            assert!(
                !rendered.contains(&query),
                "{provider} echoed the whole query"
            );
            assert!(
                rendered.chars().count() <= prefix_chars + MAX_QUERY_ECHO_CHARS + 3,
                "{provider} exceeded the query-echo budget: {} chars",
                rendered.chars().count()
            );
            assert_eq!(
                rendered.chars().filter(|c| *c == '漢').count(),
                MAX_QUERY_ECHO_CHARS,
                "{provider} must keep whole characters up to the cap"
            );
            assert!(
                !rendered.contains('\u{FFFD}'),
                "{provider} split a codepoint"
            );
        }
    }

    /// A short query must still round-trip verbatim: the bound may only touch
    /// output that actually exceeds it.
    #[test]
    fn empty_result_replies_leave_short_queries_verbatim() {
        for (provider, rendered) in empty_result_replies("rust ownership") {
            assert_eq!(
                rendered, "No results found for: rust ownership",
                "{provider} changed the reply for an under-cap query"
            );
        }
    }

    /// The success path echoes the query in its header too. `render_results`
    /// keeps the total cap by hard-cutting the whole output, so an oversized
    /// query technically stays inside the budget — but it does so by evicting
    /// every result. Bounding the echo keeps the results.
    #[test]
    fn rendered_header_bounds_the_echoed_query() {
        let query = "漢".repeat(MAX_QUERY_ECHO_CHARS + 5_000);
        let tool = WebSearchTool::new("brave".to_string(), None, None, 5, 15);

        let rendered = tool
            .parse_brave_results(
                &serde_json::json!({"web": {"results": [
                    {"title": "T", "url": "https://example.com", "description": "short"}
                ]}}),
                &query,
            )
            .unwrap();

        assert!(!rendered.contains(&query), "header echoed the whole query");
        assert_eq!(
            rendered.chars().filter(|c| *c == '漢').count(),
            MAX_QUERY_ECHO_CHARS
        );
        assert!(
            rendered.contains("1. T"),
            "the query echo must not crowd out the results: {rendered}"
        );
        assert!(!rendered.contains(TRUNCATED_RESULTS_NOTE.as_str()));
    }

    /// Bocha reports business failures as a 200 with a non-200 `code` and a
    /// provider-controlled `msg`. That message returns before the capped
    /// success path, so it needs its own bound.
    #[test]
    fn bocha_business_error_bounds_the_provider_message() {
        let tool = WebSearchTool::new("bocha".to_string(), None, None, 5, 15);
        // Multibyte on purpose, same reason as the query echo.
        let msg = "é".repeat(MAX_PROVIDER_ERROR_CHARS + 20_000);
        let prefix_chars = "Bocha AI search returned error (code 403): "
            .chars()
            .count();

        let err = tool
            .parse_bocha_results(&serde_json::json!({"code": 403, "msg": msg}), "q")
            .expect_err("a non-200 code must be reported as an error")
            .to_string();

        assert!(
            err.contains("code 403"),
            "error semantics must be preserved: {err}"
        );
        assert!(!err.contains(&msg), "the whole provider message leaked");
        assert!(
            err.chars().count() <= prefix_chars + MAX_PROVIDER_ERROR_CHARS + 3,
            "provider error exceeded its budget: {} chars",
            err.chars().count()
        );
        assert_eq!(
            err.chars().filter(|c| *c == 'é').count(),
            MAX_PROVIDER_ERROR_CHARS,
            "must keep whole characters up to the cap"
        );
        assert!(!err.contains('\u{FFFD}'), "split a codepoint");
    }

    /// A short provider message must survive verbatim.
    #[test]
    fn bocha_business_error_leaves_short_messages_verbatim() {
        let tool = WebSearchTool::new("bocha".to_string(), None, None, 5, 15);

        let err = tool
            .parse_bocha_results(
                &serde_json::json!({"code": 401, "msg": "invalid api key"}),
                "q",
            )
            .expect_err("a non-200 code must be reported as an error")
            .to_string();

        assert_eq!(
            err,
            "Bocha AI search returned error (code 401): invalid api key"
        );
    }

    // ── Message texts ────────────────────────────────────────────────────

    /// Runtime tool text is contractually Fluent-backed, not inline literals.
    /// Pin each message to its key: a key that is missing from the catalog
    /// resolves to the `{key}` placeholder, so an unregistered key fails here
    /// instead of shipping a brace-wrapped identifier to the model.
    #[test]
    fn user_facing_strings_resolve_through_the_tool_catalog() {
        for (key, resolved) in [
            (
                DUCKDUCKGO_BLOCK_MESSAGE_KEY,
                DUCKDUCKGO_BLOCK_MESSAGE.as_str(),
            ),
            (
                SEARXNG_NOT_CONFIGURED_KEY,
                SEARXNG_NOT_CONFIGURED_MESSAGE.as_str(),
            ),
            (TRUNCATED_RESULTS_NOTE_KEY, TRUNCATED_RESULTS_NOTE.as_str()),
        ] {
            let catalog = crate::i18n::get_required_tool_string(key);
            assert_ne!(
                catalog,
                format!("{{{key}}}"),
                "{key} is missing from the tool Fluent catalog"
            );
            assert!(!catalog.is_empty(), "{key} resolved to an empty string");
            assert_eq!(
                resolved, catalog,
                "{key} must be read from the catalog, not inlined"
            );
        }
    }

    /// The block message is read by the model, not a human. It must forbid the
    /// retry-and-rephrase reflex (which deepens the block) and name concrete
    /// recoveries.
    #[test]
    fn duckduckgo_block_message_instructs_the_model() {
        let message = DUCKDUCKGO_BLOCK_MESSAGE.as_str();

        assert!(message.contains("rate-limiting"));
        assert!(
            message.contains("Do not retry or rephrase"),
            "must forbid the retry reflex: {message}"
        );
        assert!(message.contains("web_fetch"), "must name the fallback tool");
        for provider in ["SearXNG", "Brave", "Tavily"] {
            assert!(message.contains(provider), "must name {provider}");
        }
        // The stale phrasing described the fault instead of the recovery.
        assert!(!message.contains("DuckDuckGo blocked the automated search request"));
    }

    /// Regression: the old text pointed at a `SEARXNG_INSTANCE_URL` env var
    /// that no code has ever read. The real surfaces are the config key and
    /// the generic `ZEROCLAW_<path>` override grammar.
    #[test]
    fn searxng_missing_url_message_names_only_real_surfaces() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");
        std::fs::write(&config_path, "[web_search]\n").unwrap();

        let tool = WebSearchTool::new_with_config(
            "searxng".to_string(),
            None,
            None,
            None,
            None,
            5,
            15,
            config_path,
            false,
        );

        let message = tool
            .resolve_searxng_instance_url()
            .expect_err("missing URL must error")
            .to_string();

        assert!(message.contains("[web_search] searxng_instance_url"));
        assert!(message.contains("ZEROCLAW_web_search__searxng_instance_url"));
        assert!(
            !message.contains("SEARXNG_INSTANCE_URL environment variable"),
            "must not advertise an env var nothing reads: {message}"
        );
    }

    // ── DuckDuckGo scrape hygiene ────────────────────────────────────────

    #[test]
    fn duckduckgo_headers_are_always_drawn_from_the_pools() {
        // Sweep the interesting arithmetic edges plus a spread of values.
        let probes = [0u64, 1, 5, 6, 7, 23, 24, u64::MAX, u64::MAX - 1, 1 << 63];
        for entropy in probes {
            let headers = duckduckgo_request_headers(entropy);
            assert!(
                DUCKDUCKGO_USER_AGENTS.contains(&headers.user_agent),
                "entropy {entropy} produced an off-pool UA"
            );
            assert!(
                DUCKDUCKGO_ACCEPT_LANGUAGES.contains(&headers.accept_language),
                "entropy {entropy} produced an off-pool language"
            );
        }
    }

    #[test]
    fn duckduckgo_user_agents_look_like_browsers_not_zeroclaw() {
        assert!(
            DUCKDUCKGO_USER_AGENTS.len() > 1,
            "a one-entry pool is not a rotation"
        );
        for ua in DUCKDUCKGO_USER_AGENTS {
            assert!(ua.starts_with("Mozilla/5.0"), "not browser-shaped: {ua}");
            assert!(
                !ua.contains("ZeroClaw"),
                "self-identifying UA defeats the purpose: {ua}"
            );
        }
    }

    #[test]
    fn duckduckgo_headers_actually_rotate_across_the_pools() {
        let uas: std::collections::HashSet<_> = (0..64)
            .map(|e| duckduckgo_request_headers(e).user_agent)
            .collect();
        let langs: std::collections::HashSet<_> = (0..64)
            .map(|e| duckduckgo_request_headers(e).accept_language)
            .collect();

        assert_eq!(uas.len(), DUCKDUCKGO_USER_AGENTS.len(), "UA pool unused");
        assert_eq!(
            langs.len(),
            DUCKDUCKGO_ACCEPT_LANGUAGES.len(),
            "language pool unused"
        );
    }

    #[test]
    fn duckduckgo_gap_stays_inside_the_configured_window() {
        let probes = [0u64, 1, 500, 1_500, 1_501, u64::MAX, u64::MAX - 1];
        for entropy in probes {
            let gap = duckduckgo_gap(entropy).as_millis() as u64;
            assert!(
                (DUCKDUCKGO_MIN_GAP_MS..=DUCKDUCKGO_MAX_GAP_MS).contains(&gap),
                "entropy {entropy} produced an out-of-window gap of {gap}ms"
            );
        }

        // Both ends of the window are reachable, so the gap is a real range
        // rather than a constant with decoration.
        assert_eq!(duckduckgo_gap(0).as_millis() as u64, DUCKDUCKGO_MIN_GAP_MS);
        assert_eq!(
            duckduckgo_gap(DUCKDUCKGO_MAX_GAP_MS - DUCKDUCKGO_MIN_GAP_MS).as_millis() as u64,
            DUCKDUCKGO_MAX_GAP_MS
        );
    }

    #[test]
    fn scrape_entropy_varies_between_calls() {
        let values: std::collections::HashSet<u64> = (0..32).map(|_| scrape_entropy()).collect();
        assert!(
            values.len() > 24,
            "entropy stream is too repetitive: {} distinct of 32",
            values.len()
        );
        assert!(!values.contains(&0), "xorshift must never latch to zero");
    }

    /// The throttle is exercised with a short gap rather than the production
    /// 500-2000ms window: the property under test is "the second caller waits
    /// for the reserved gap", which does not depend on its size.
    #[tokio::test]
    async fn throttle_makes_the_next_scrape_wait_for_the_reserved_gap() {
        let throttle = ScrapeThrottle::default();
        let gap = Duration::from_millis(60);

        let started = Instant::now();
        throttle.acquire(gap).await;
        assert!(
            started.elapsed() < gap,
            "the first scrape must not be delayed"
        );

        throttle.acquire(gap).await;
        assert!(
            started.elapsed() >= gap,
            "the second scrape must wait out the reserved gap, waited {:?}",
            started.elapsed()
        );
    }

    /// Concurrent searches must queue rather than all reading the same
    /// `next_allowed` and firing together — that burst is precisely what gets
    /// the machine blocked.
    #[tokio::test]
    async fn throttle_serializes_concurrent_scrapes() {
        let throttle = std::sync::Arc::new(ScrapeThrottle::default());
        let gap = Duration::from_millis(50);

        let started = Instant::now();
        let mut handles = Vec::new();
        for _ in 0..3 {
            let throttle = throttle.clone();
            handles.push(zeroclaw_spawn::spawn!(async move {
                throttle.acquire(gap).await;
                started.elapsed()
            }));
        }

        let mut finishes = Vec::new();
        for handle in handles {
            finishes.push(handle.await.unwrap());
        }
        finishes.sort();

        // Three callers, one immediate and two gapped: the last one cannot
        // have been released before two full gaps elapsed.
        assert!(
            finishes[2] >= gap * 2,
            "concurrent scrapes burst through the throttle: {finishes:?}"
        );
    }

    /// The scrape request itself must carry browser-shaped headers, not just
    /// the pools existing in isolation.
    #[tokio::test]
    async fn duckduckgo_request_sends_browser_shaped_headers() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/html/"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<html></html>"))
            .mount(&server)
            .await;

        let tool = WebSearchTool::new("duckduckgo".to_string(), None, None, 5, 15);
        tool.search_duckduckgo_at(&format!("{}/html/", server.uri()), "test")
            .await
            .expect("mock should answer");

        let recorded = server.received_requests().await.expect("captured request");
        assert_eq!(recorded.len(), 1);
        let headers = &recorded[0].headers;

        let user_agent = headers
            .get("user-agent")
            .expect("a User-Agent must be sent")
            .to_str()
            .unwrap();
        assert!(
            DUCKDUCKGO_USER_AGENTS.contains(&user_agent),
            "UA must come from the rotation pool, got: {user_agent}"
        );

        let language = headers
            .get("accept-language")
            .expect("Accept-Language must be sent")
            .to_str()
            .unwrap();
        assert!(
            DUCKDUCKGO_ACCEPT_LANGUAGES.contains(&language),
            "Accept-Language must come from the pool, got: {language}"
        );

        assert_eq!(headers.get("dnt").expect("DNT must be sent"), "1");
        assert_eq!(
            headers.get("accept").expect("Accept must be sent"),
            DUCKDUCKGO_ACCEPT
        );
    }
}
