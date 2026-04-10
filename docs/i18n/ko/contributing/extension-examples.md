# 확장 예제

ZeroClaw의 아키텍처는 trait 기반이며 모듈화되어 있습니다.
새로운 프로바이더, 채널, 도구 또는 메모리 백엔드를 추가하려면 해당 trait을 구현하고 팩토리 모듈에 등록합니다.

이 페이지에는 각 핵심 확장 포인트에 대한 최소한의 동작하는 예제가 포함되어 있습니다.
단계별 통합 체크리스트는 [change-playbooks.md](./change-playbooks.md)를 참조합니다.

> **정보 출처**: trait 정의는 `src/*/traits.rs`에 있습니다.
> 여기의 예제가 trait 파일과 충돌하는 경우, trait 파일이 우선합니다.

---

## Tool (`src/tools/traits.rs`)

도구는 에이전트의 손입니다 — 세계와 상호 작용할 수 있게 합니다.

**필수 메서드**: `name()`, `description()`, `parameters_schema()`, `execute()`.
`spec()` 메서드는 나머지를 조합하는 기본 구현이 있습니다.

`src/tools/mod.rs`의 `default_tools()`에 도구를 등록합니다.

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

## Channel (`src/channels/traits.rs`)

채널은 ZeroClaw가 모든 메시징 플랫폼을 통해 통신할 수 있게 합니다.

**필수 메서드**: `name()`, `send(&SendMessage)`, `listen()`.
`health_check()`, `start_typing()`, `stop_typing()`,
draft 메서드(`send_draft`, `update_draft`, `finalize_draft`, `cancel_draft`),
reaction 메서드(`add_reaction`, `remove_reaction`)에 대한 기본 구현이 있습니다.

`src/channels/mod.rs`에 채널을 등록하고 `src/config/schema.rs`의 `ChannelsConfig`에 설정을 추가합니다.

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

## Provider (`src/providers/traits.rs`)

프로바이더는 LLM 백엔드 어댑터입니다. 각 프로바이더는 ZeroClaw를 다른 모델 API에 연결합니다.

**필수 메서드**: `chat_with_system(system_prompt: Option<&str>, message: &str, model: &str, temperature: f64) -> Result<String>`.
나머지는 기본 구현이 있습니다:
`simple_chat()`과 `chat_with_history()`는 `chat_with_system()`에 위임합니다;
`capabilities()`는 기본적으로 네이티브 도구 호출 없음을 반환합니다;
스트리밍 메서드는 기본적으로 빈/오류 스트림을 반환합니다.

`src/providers/mod.rs`에 프로바이더를 등록합니다.

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

## Memory (`src/memory/traits.rs`)

메모리 백엔드는 에이전트의 지식에 대한 플러거블 영속성을 제공합니다.

**필수 메서드**: `name()`, `store()`, `recall()`, `get()`, `list()`, `forget()`, `count()`, `health_check()`.
`store()`와 `recall()` 모두 범위 지정을 위한 선택적 `session_id`를 받습니다.

`src/memory/mod.rs`에 백엔드를 등록합니다.

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

## 등록 패턴

모든 확장 trait은 동일한 연결 패턴을 따릅니다:

1. 관련 `src/*/` 디렉토리에 구현 파일을 생성합니다.
2. 모듈의 팩토리 함수에 등록합니다 (예: `default_tools()`, 프로바이더 match arm).
3. 필요한 설정 키를 `src/config/schema.rs`에 추가합니다.
4. 팩토리 연결 및 오류 경로에 대한 집중 테스트를 작성합니다.

확장 유형별 전체 체크리스트는 [change-playbooks.md](./change-playbooks.md)를 참조합니다.
