# Extension Examples

ZeroClaw's architecture is trait-driven and modular.
To add a new provider, channel, tool, or memory backend, implement the corresponding trait and register it in the factory module.

This page contains minimal, working examples for each core extension point.

> **Source of truth**: the trait definitions live in `crates/zeroclaw-api/src/`.
> If an example here conflicts with the trait file, the trait file wins.

---

## Tool (`crates/zeroclaw-api/src/tool.rs`)

Tools are the agent's hands — they let it interact with the world.

**Required methods**: `name()`, `description()`, `parameters_schema()`, `execute()`.
The `spec()` method has a default implementation that composes the others.

Register your tool in `crates/zeroclaw-tools/src/lib.rs` via `default_tools()`.

```rust
// In your crate: use zeroclaw::tools::traits::{Tool, ToolResult};

use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};

/// A tool that fetches a URL and returns the status code.
pub struct HttpGetTool;

#[async_trait]
impl Tool for HttpGetTool {
    fn name(&self) -> &str {
        "http_get"
    }

    fn description(&self) -> &str {
        "Fetch a URL and return the HTTP status code and content length"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "URL to fetch" }
            },
            "required": ["url"]
        })
    }

    async fn execute(&self, args: Value) -> Result<ToolResult> {
        let url = args["url"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'url' parameter"))?;

        match reqwest::get(url).await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let len = resp.content_length().unwrap_or(0);
                Ok(ToolResult {
                    success: status < 400,
                    output: format!("HTTP {status} — {len} bytes"),
                    error: None,
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Request failed: {e}")),
            }),
        }
    }
}
```

---

## Channel (`crates/zeroclaw-api/src/channel.rs`)

Channels let ZeroClaw communicate through any messaging platform.

**Required methods**: `name()`, `send(&SendMessage)`, `listen()`.
Default implementations exist for `health_check()`, `start_typing()`, `stop_typing()`,
draft methods (`send_draft`, `update_draft`, `finalize_draft`, `cancel_draft`),
and reaction methods (`add_reaction`, `remove_reaction`).

Register your channel in `crates/zeroclaw-channels/src/lib.rs` and add config to `ChannelsConfig` in `crates/zeroclaw-config/src/schema.rs`.

```rust
// In your crate: use zeroclaw::channels::traits::{Channel, ChannelMessage, SendMessage};

use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::mpsc;

/// Telegram channel via Bot API.
pub struct TelegramChannel {
    bot_token: String,
    allowed_users: Vec<String>,
    client: reqwest::Client,
}

impl TelegramChannel {
    pub fn new(bot_token: &str, allowed_users: Vec<String>) -> Self {
        Self {
            bot_token: bot_token.to_string(),
            allowed_users,
            client: reqwest::Client::new(),
        }
    }

    fn api_url(&self, method: &str) -> String {
        format!("https://api.telegram.org/bot{}/{method}", self.bot_token)
    }
}

#[async_trait]
impl Channel for TelegramChannel {
    fn name(&self) -> &str {
        "telegram"
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        self.client
            .post(self.api_url("sendMessage"))
            .json(&serde_json::json!({
                "chat_id": message.recipient,
                "text": message.content,
                "parse_mode": "Markdown",
            }))
            .send()
            .await?;
        Ok(())
    }

    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> Result<()> {
        let mut offset: i64 = 0;

        loop {
            let resp = self
                .client
                .get(self.api_url("getUpdates"))
                .query(&[("offset", offset.to_string()), ("timeout", "30".into())])
                .send()
                .await?
                .json::<serde_json::Value>()
                .await?;

            if let Some(updates) = resp["result"].as_array() {
                for update in updates {
                    if let Some(msg) = update.get("message") {
                        let sender = msg["from"]["username"]
                            .as_str()
                            .unwrap_or("unknown")
                            .to_string();

                        if !self.allowed_users.is_empty()
                            && !self.allowed_users.contains(&sender)
                        {
                            continue;
                        }

                        let chat_id = msg["chat"]["id"].to_string();

                        let channel_msg = ChannelMessage {
                            id: msg["message_id"].to_string(),
                            sender,
                            reply_target: chat_id,
                            content: msg["text"].as_str().unwrap_or("").to_string(),
                            channel: "telegram".into(),
                            timestamp: msg["date"].as_u64().unwrap_or(0),
                            thread_ts: None,
                        };

                        if tx.send(channel_msg).await.is_err() {
                            return Ok(());
                        }
                    }
                    offset = update["update_id"].as_i64().unwrap_or(offset) + 1;
                }
            }
        }
    }

    async fn health_check(&self) -> bool {
        self.client
            .get(self.api_url("getMe"))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }
}
```

---

## Provider (`crates/zeroclaw-api/src/provider.rs`)

Providers are LLM backend adapters. Each provider connects ZeroClaw to a different model API.

**Required method**: `chat_with_system(system_prompt: Option<&str>, message: &str, model: &str, temperature: Option<f64>) -> Result<String>`.
Everything else has default implementations:
`simple_chat()` and `chat_with_history()` delegate to `chat_with_system()`;
`capabilities()` returns no native tool calling by default;
streaming methods return empty/error streams by default.

Register your provider in `crates/zeroclaw-providers/src/lib.rs`.

```rust
// In your crate: use zeroclaw::providers::traits::Provider;

use anyhow::Result;
use async_trait::async_trait;

/// Ollama local provider.
pub struct OllamaProvider {
    base_url: String,
    client: reqwest::Client,
}

impl OllamaProvider {
    pub fn new(base_url: Option<&str>) -> Self {
        Self {
            base_url: base_url.unwrap_or("http://localhost:11434").to_string(),
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl Provider for OllamaProvider {
    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: Option<f64>,
    ) -> Result<String> {
        let temperature = temperature.unwrap_or(self.default_temperature());
        let url = format!("{}/api/generate", self.base_url);

        let mut body = serde_json::json!({
            "model": model,
            "prompt": message,
            "temperature": temperature,
            "stream": false,
        });

        if let Some(system) = system_prompt {
            body["system"] = serde_json::Value::String(system.to_string());
        }

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await?
            .json::<serde_json::Value>()
            .await?;

        resp["response"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("No response field in Ollama reply"))
    }
}
```

---

## Memory (`crates/zeroclaw-api/src/memory_traits.rs`)

Memory backends provide pluggable persistence for the agent's knowledge.

**Required methods**: `name()`, `store()`, `recall()`, `get()`, `list()`, `forget()`, `count()`, `health_check()`.
Both `store()` and `recall()` accept an optional `session_id` for scoping.

Register your backend in `crates/zeroclaw-memory/src/lib.rs`.

```rust
// In your crate: use zeroclaw::memory::traits::{Memory, MemoryEntry, MemoryCategory};

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Mutex;

/// In-memory HashMap backend (useful for testing or ephemeral sessions).
pub struct InMemoryBackend {
    store: Mutex<HashMap<String, MemoryEntry>>,
}

impl InMemoryBackend {
    pub fn new() -> Self {
        Self {
            store: Mutex::new(HashMap::new()),
        }
    }
}

#[async_trait]
impl Memory for InMemoryBackend {
    fn name(&self) -> &str {
        "in-memory"
    }

    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        let entry = MemoryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            key: key.to_string(),
            content: content.to_string(),
            category,
            timestamp: chrono::Local::now().to_rfc3339(),
            session_id: session_id.map(|s| s.to_string()),
            score: None,
        };
        self.store
            .lock()
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .insert(key.to_string(), entry);
        Ok(())
    }

    async fn recall(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        let store = self.store.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let query_lower = query.to_lowercase();

        let mut results: Vec<MemoryEntry> = store
            .values()
            .filter(|e| e.content.to_lowercase().contains(&query_lower))
            .filter(|e| match session_id {
                Some(sid) => e.session_id.as_deref() == Some(sid),
                None => true,
            })
            .cloned()
            .collect();

        results.truncate(limit);
        Ok(results)
    }

    async fn get(&self, key: &str) -> anyhow::Result<Option<MemoryEntry>> {
        let store = self.store.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(store.get(key).cloned())
    }

    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        let store = self.store.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(store
            .values()
            .filter(|e| match category {
                Some(cat) => &e.category == cat,
                None => true,
            })
            .filter(|e| match session_id {
                Some(sid) => e.session_id.as_deref() == Some(sid),
                None => true,
            })
            .cloned()
            .collect())
    }

    async fn forget(&self, key: &str) -> anyhow::Result<bool> {
        let mut store = self.store.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(store.remove(key).is_some())
    }

    async fn count(&self) -> anyhow::Result<usize> {
        let store = self.store.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(store.len())
    }

    async fn health_check(&self) -> bool {
        true
    }
}
```

---

## Registration Pattern

All extension traits follow the same wiring pattern:

1. Create your implementation file in the relevant `crates/zeroclaw-*/src/` directory.
2. Register it in the module's factory function (e.g., `default_tools()`, provider match arm).
3. Add any needed config keys to `crates/zeroclaw-config/src/schema.rs`.
4. Write focused tests for factory wiring and error paths.

---

## Architecture boundary rules

A few invariants that hold across every extension. Breaking these tends to be the source of cross-cutting cleanup PRs later, so internalise them up front:

- **Extend by trait + factory wiring first.** Adding a new provider/channel/tool/peripheral is implementing a trait and registering it in the relevant factory. Avoid cross-module rewrites for what should be an isolated feature.
- **Dependency direction goes inward to contracts.** Concrete integrations depend on `zeroclaw-api` traits, `zeroclaw-config` schema, and `zeroclaw-infra` utilities — not on each other. Provider code does not import channel internals; tool code does not mutate gateway policy directly.
- **Module responsibilities stay single-purpose.** Orchestration in `zeroclaw-runtime/src/agent/`, transport in `zeroclaw-channels/`, model I/O in `zeroclaw-providers/`, policy in `zeroclaw-runtime/src/security/`, execution in `zeroclaw-tools/`.
- **Rule of three for shared abstractions.** Introduce new shared types only after a third real caller materialises. Premature abstractions accrete weight that future contributors have to navigate around.
- **Config keys are public contract.** Schema changes need defaults, compatibility impact, and a migration/rollback path documented in the PR.

## Tool shared state

Any tool that owns long-lived shared state (rate limiters, connection pools, cached credentials, broadcast channels) follows a small contract that keeps the daemon's per-client isolation guarantees intact:

- **`Arc<RwLock<T>>` handle pattern.** Accept handles at construction; do not create global or static mutable state inside a tool. Tests need to instantiate tools with isolated state, and the daemon needs to construct multiple instances for namespacing.
- **`ClientId` is daemon-supplied.** Use it to namespace per-client state. Never construct identity keys inside a tool — the daemon owns identity and the tool consumes it.
- **Security state isolates per client.** Credentials, quotas, anything that can leak between sessions stays per-`ClientId`. Display/broadcast state is allowed to share, with optional namespace prefixing for trace clarity.
- **Cached validation invalidates on config change.** Tools must re-validate before the next execution when the config-change signal fires. The daemon emits the signal; the tool subscribes.

In short: per-client isolation is enforced by the daemon constructing one tool instance per `ClientId`. Broadcast state can be shared across clients but should be namespace-prefixed in trace output so a per-client filter still works.
