use super::web_search_provider_routing::{
    SearchStatus, WebSearchProviderRoute, resolve_web_search_provider,
};
use async_trait::async_trait;
use regex::Regex;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;
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

        let builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(self.timeout_secs))
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36");
        let builder =
            zeroclaw_config::schema::apply_runtime_proxy_to_builder(builder, "tool.web_search");
        let client = builder.build()?;

        let response = client.get(&search_url).send().await?;
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
            return Ok(format!("No results found for: {}", query));
        }

        let mut lines = vec![format!("Search results for: {} (via DuckDuckGo)", query)];

        let count = link_matches.len().min(self.max_results);

        for i in 0..count {
            let caps = &link_matches[i];
            let url_str = decode_ddg_redirect_url(&caps[1]);
            let title = strip_tags(&caps[2]);

            lines.push(format!("{}. {}", i + 1, title.trim()));
            lines.push(format!("   {}", url_str.trim()));

            // Add snippet if available
            if i < snippet_matches.len() {
                let snippet = strip_tags(&snippet_matches[i][1]);
                let snippet = snippet.trim();
                if !snippet.is_empty() {
                    lines.push(format!("   {}", snippet));
                }
            }
        }

        Ok(lines.join("\n"))
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
            return Ok(format!("No results found for: {}", query));
        }

        let mut lines = vec![format!("Search results for: {} (via Tavily)", query)];

        for (i, result) in results.iter().take(self.max_results).enumerate() {
            let title = result
                .get("title")
                .and_then(|t| t.as_str())
                .unwrap_or("No title");
            let url = result.get("url").and_then(|u| u.as_str()).unwrap_or("");
            // Tavily returns a pre-cleaned `content` field (not just a snippet),
            // so it doubles as the description for the LLM caller.
            let content = result.get("content").and_then(|c| c.as_str()).unwrap_or("");

            lines.push(format!("{}. {}", i + 1, title));
            lines.push(format!("   {}", url));
            if !content.is_empty() {
                lines.push(format!("   {}", content));
            }
        }

        Ok(lines.join("\n"))
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
            return Ok(format!("No results found for: {}", query));
        }

        let mut lines = vec![format!("Search results for: {} (via Jina AI)", query)];

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

            lines.push(format!("{}. {}", i + 1, title));
            lines.push(format!("   {}", url));
            if !snippet.is_empty() {
                lines.push(format!("   {}", snippet));
            }
        }

        Ok(lines.join("\n"))
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
            anyhow::bail!("Bocha AI search returned error (code {code}): {msg}");
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
            return Ok(format!("No results found for: {}", query));
        }

        let mut lines = vec![format!("Search results for: {} (via Bocha)", query)];

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

            lines.push(format!("{}. {}", i + 1, title));
            lines.push(format!("   {}", url));

            // Compact attribution line: "siteName · date" when either is present.
            let attribution = match (site.is_empty(), date.is_empty()) {
                (false, false) => format!("   {site} · {date}"),
                (false, true) => format!("   {site}"),
                (true, false) => format!("   {date}"),
                (true, true) => String::new(),
            };
            if !attribution.is_empty() {
                lines.push(attribution);
            }

            if !body.is_empty() {
                lines.push(format!("   {}", body));
            }
        }

        Ok(lines.join("\n"))
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
            return Ok(format!("No results found for: {}", query));
        }

        let mut lines = vec![format!("Search results for: {} (via Brave)", query)];

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

            lines.push(format!("{}. {}", i + 1, title));
            lines.push(format!("   {}", url));
            if !description.is_empty() {
                lines.push(format!("   {}", description));
            }
        }

        Ok(lines.join("\n"))
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
                anyhow::Error::msg(
                    "SearXNG instance URL not configured. Set [web_search] searxng_instance_url \
                     in config.toml or the SEARXNG_INSTANCE_URL environment variable.",
                )
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
            return Ok(format!("No results found for: {}", query));
        }

        let mut lines = vec![format!("Search results for: {} (via SearXNG)", query)];

        for (i, result) in results.iter().take(self.max_results).enumerate() {
            let title = result
                .get("title")
                .and_then(|t| t.as_str())
                .unwrap_or("No title");
            let url = result.get("url").and_then(|u| u.as_str()).unwrap_or("");
            let content = result.get("content").and_then(|c| c.as_str()).unwrap_or("");

            lines.push(format!("{}. {}", i + 1, title));
            lines.push(format!("   {}", url));
            if !content.is_empty() {
                lines.push(format!("   {}", content));
            }
        }

        Ok(lines.join("\n"))
    }
}

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

const DUCKDUCKGO_BLOCK_MESSAGE: &str = "DuckDuckGo blocked the automated search request. Try configuring SearXNG, Brave, or Tavily as the web search provider.";

fn duckduckgo_block_message(
    status: reqwest::StatusCode,
    final_url_is_block: bool,
    html_contains_block: bool,
) -> Option<&'static str> {
    if status == reqwest::StatusCode::FORBIDDEN || final_url_is_block || html_contains_block {
        Some(DUCKDUCKGO_BLOCK_MESSAGE)
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

        assert!(message.contains("DuckDuckGo blocked"));
        assert!(message.contains("SearXNG"));
    }

    #[test]
    fn test_duckduckgo_block_detection_reports_verification_redirect() {
        let message = duckduckgo_block_message(reqwest::StatusCode::OK, true, false)
            .expect("verification redirects should be classified as a DuckDuckGo block");

        assert!(message.contains("DuckDuckGo blocked"));
        assert!(message.contains("SearXNG"));
    }

    #[test]
    fn test_duckduckgo_block_detection_reports_verification_form_in_html() {
        let message = duckduckgo_block_message(reqwest::StatusCode::OK, false, true)
            .expect("verification form HTML should be classified as a DuckDuckGo block");

        assert!(message.contains("DuckDuckGo blocked"));
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

        assert!(err.to_string().contains("DuckDuckGo blocked"));
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

        assert!(err.to_string().contains("DuckDuckGo blocked"));
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

        assert!(err.to_string().contains("DuckDuckGo blocked"));
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

        assert!(err.to_string().contains("DuckDuckGo blocked"));
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
                .contains("SearXNG instance URL not configured")
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
}
