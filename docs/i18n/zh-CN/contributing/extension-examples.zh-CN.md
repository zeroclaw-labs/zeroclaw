# 扩展示例

ZeroClaw 的架构是特征（trait）驱动和模块化的。
要添加新的提供商、渠道、工具或内存后端，实现对应的特征并在工厂模块中注册即可。

本页面包含每个核心扩展点的最小可运行示例。
如需分步集成检查清单，请参见 [change-playbooks.md](./change-playbooks.zh-CN.md)。

> **权威来源：** 特征定义位于 `src/*/traits.rs`。
> 如果此处的示例与特征文件冲突，以特征文件为准。

---

## 工具（`src/tools/traits.rs`）

工具是代理的手 —— 让它能够与世界交互。

**必需方法：** `name()`、`description()`、`parameters_schema()`、`execute()`。
`spec()` 方法有默认实现，由其他方法组合而成。

在 `src/tools/mod.rs` 中通过 `default_tools()` 注册你的工具。

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

## 渠道（`src/channels/traits.rs`）

渠道让 ZeroClaw 可以通过任何消息平台通信。

**必需方法：** `name()`、`send(&SendMessage)`、`listen()`。
以下方法有默认实现：`health_check()`、`start_typing()`、`stop_typing()`、
草稿方法（`send_draft`、`update_draft`、`finalize_draft`、`cancel_draft`），
以及反应方法（`add_reaction`、`remove_reaction`）。

在 `src/channels/mod.rs` 中注册你的渠道，并在 `src/config/schema.rs` 的 `ChannelsConfig` 中添加配置。

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

## 提供商（`src/providers/traits.rs`）

提供商是 LLM 后端适配器。每个提供商将 ZeroClaw 连接到不同的模型 API。

**必需方法：** `chat_with_system(system_prompt: Option<&str>, message: &str, model: &str, temperature: f64) -> Result<String>`。
其他所有方法都有默认实现：
`simple_chat()` 和 `chat_with_history()` 委托给 `chat_with_system()`；
`capabilities()` 默认返回不支持原生工具调用；
流方法默认返回空/错误流。

在 `src/providers/mod.rs` 中注册你的提供商。

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
        temperature: f64,
    ) -> Result<String> {
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

## 内存（`src/memory/traits.rs`）

内存后端为代理的知识提供可插拔的持久化。

**必需方法：** `name()`、`store()`、`recall()`、`get()`、`list()`、`forget()`、`count()`、`health_check()`。
`store()` 和 `recall()` 都接受可选的 `session_id` 用于范围限定。

在 `src/memory/mod.rs` 中注册你的后端。

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

## 注册模式

所有扩展特征都遵循相同的接线模式：

1. 在相关的 `src/*/` 目录中创建你的实现文件。
2. 在模块的工厂函数中注册（例如 `default_tools()`、provider 匹配分支）。
3. 在 `src/config/schema.rs` 中添加任何需要的配置键。
4. 为工厂接线和错误路径编写聚焦的测试。

每种扩展类型的完整检查清单请参见 [change-playbooks.md](./change-playbooks.zh-CN.md)。
