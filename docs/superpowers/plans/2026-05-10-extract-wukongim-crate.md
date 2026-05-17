# Extract WuKongIM Channel into Standalone Crate

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将 `crates/zeroclaw-channels/src/wukongim.rs` 提取为独立的 workspace crate `zeroclaw-channel-wukongim`，内部按 5 个职责域以目录方式组织，`zeroclaw-channels` 通过可选依赖引用它。

**Architecture:** 新 crate 仅依赖 `zeroclaw-api`（Channel trait）、`zeroclaw-config`（WuKongIMConfig）及直接依赖（tokio-tungstenite、serde 等）。内部 5 个目录模块各自独立：`connection/` 管连接与 JSON-RPC 协议，`messaging/` 管消息收发与媒体处理，`filter/` 管权限与过滤，`approval/` 管审批流程，`config/` 管配置构造。顶层 `channel.rs` 定义 `WuKongIMChannel` 结构体并实现 `Channel` trait，协调各模块。`zeroclaw-channels` 保持 `channel-wukongim` feature 语义不变，仅将依赖源从内部模块改为新 crate。

**Tech Stack:** Rust 2024 edition, tokio-tungstenite, serde/serde_json, base64, uuid, reqwest, async-trait, futures-util, zeroclaw-api, zeroclaw-config

---

## 文件结构

### 新建

```
crates/zeroclaw-channel-wukongim/
├── Cargo.toml
└── src/
    ├── lib.rs                  # crate 文档 + pub use WuKongIMChannel + mod 声明
    ├── channel.rs              # WuKongIMChannel 结构体 + Channel trait impl（协调各模块）
    ├── connection/
    │   ├── mod.rs              # WsSink 类型别名、send_rpc()、send_ack()、心跳常量
    │   └── protocol.rs         # JSON-RPC 2.0 类型、WkMessageType、WkChannelType、Header、ConnectParams、SendParams、RecvNotificationParams、RecvAckParams
    ├── messaging/
    │   ├── mod.rs              # Channel::send() 实现、listen() 消息分发循环
    │   └── media.rs            # 图片下载（download_image_as_base64）、Markdown 图片处理、detect_image_mime
    ├── filter/
    │   └── mod.rs              # is_user_allowed()、parse_recipient()、mention_only 群组过滤
    ├── approval/
    │   ├── mod.rs              # request_approval() 实现、pending_approvals 状态管理
    │   └── card.rs             # WkApprovalCard、WkApprovalBody、WkAction、WkApprovalAction、build_approval_card()
    └── config/
        └── mod.rs              # from_config() 构造器、pub use zeroclaw_config::schema::WuKongIMConfig
```

### 修改

| 文件 | 变更内容 |
|------|---------|
| `Cargo.toml`（workspace root） | `[workspace.members]` 加入新 crate；`[workspace.dependencies]` 声明路径 |
| `crates/zeroclaw-channels/Cargo.toml` | 增加 `zeroclaw-channel-wukongim` optional dep；`channel-wukongim` feature 改为 `["dep:zeroclaw-channel-wukongim"]` |
| `crates/zeroclaw-channels/src/lib.rs` | 将 `pub mod wukongim` 替换为 `pub use zeroclaw_channel_wukongim::WuKongIMChannel` |
| `crates/zeroclaw-channels/src/orchestrator/mod.rs` | 将 `pub use crate::wukongim::WuKongIMChannel` 替换为 `pub use zeroclaw_channel_wukongim::WuKongIMChannel` |

### 删除

| 文件 | 原因 |
|------|------|
| `crates/zeroclaw-channels/src/wukongim.rs` | 内容已迁移到新 crate |

---

## Task 1: 创建新 crate 骨架并加入 workspace

**Files:**
- Create: `crates/zeroclaw-channel-wukongim/Cargo.toml`
- Create: `crates/zeroclaw-channel-wukongim/src/lib.rs`
- Create: 5 个目录各自的占位 mod.rs / 子文件
- Modify: `Cargo.toml` (workspace root)

- [ ] **Step 1: 创建 Cargo.toml**

```toml
# crates/zeroclaw-channel-wukongim/Cargo.toml
[package]
name = "zeroclaw-channel-wukongim"
version.workspace = true
edition.workspace = true
license.workspace = true
description = "WuKongIM channel implementation for ZeroClaw."
publish = false

[dependencies]
zeroclaw-api.workspace = true
zeroclaw-config = { workspace = true, features = ["channel-wukongim"] }
anyhow = "1.0"
async-trait = "0.1"
base64 = "0.22"
futures-util = { version = "0.3", default-features = false, features = ["sink"] }
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls-webpki-roots-no-provider", "__rustls-ring", "stream"] }
serde = { version = "1.0", default-features = false, features = ["derive"] }
serde_json = { version = "1.0", default-features = false, features = ["std"] }
tokio = { version = "1.50", default-features = false, features = ["rt-multi-thread", "macros", "time", "net", "sync"] }
tokio-tungstenite = { version = "0.29", default-features = false, features = ["connect", "rustls-tls-webpki-roots"] }
tracing = { version = "0.1", default-features = false }
uuid = { version = "1.22", default-features = false, features = ["v4", "std"] }

[dev-dependencies]
tokio = { version = "1.50", features = ["rt-multi-thread", "macros"] }
```

- [ ] **Step 2: 创建 lib.rs**

```rust
// crates/zeroclaw-channel-wukongim/src/lib.rs
//! WuKongIM channel implementation for ZeroClaw.
//!
//! 模块结构按职责域划分：
//! - [`connection`] — WebSocket 连接与通信（JSON-RPC 2.0 协议）
//! - [`messaging`] — 消息收发与媒体处理
//! - [`filter`]    — 权限校验与消息过滤
//! - [`approval`]  — 工具调用审批流程
//! - [`config`]    — 配置构造

pub mod approval;
pub mod channel;
pub mod config;
pub mod connection;
pub mod filter;
pub mod messaging;

pub use channel::WuKongIMChannel;
```

- [ ] **Step 3: 创建各模块占位文件**

以下文件内容均为 `// placeholder`，用于让 crate 能编译：

- `src/channel.rs`
- `src/connection/mod.rs`
- `src/connection/protocol.rs`
- `src/messaging/mod.rs`
- `src/messaging/media.rs`
- `src/filter/mod.rs`
- `src/approval/mod.rs`
- `src/approval/card.rs`
- `src/config/mod.rs`

- [ ] **Step 4: 在 workspace Cargo.toml 中注册新 crate**

`[workspace].members` 追加 `"crates/zeroclaw-channel-wukongim"`。

`[workspace.dependencies]` 追加：

```toml
zeroclaw-channel-wukongim = { path = "crates/zeroclaw-channel-wukongim", version = "0.7.5" }
```

- [ ] **Step 5: 验证骨架编译**

```powershell
cargo check -p zeroclaw-channel-wukongim
```

期望：无错误（允许 dead_code warning）。

- [ ] **Step 6: Commit**

```bash
git add crates/zeroclaw-channel-wukongim/ Cargo.toml Cargo.lock
git commit -m "chore(channels): scaffold zeroclaw-channel-wukongim crate with 5 domain modules"
```

---

## Task 2: connection/ — 连接与通信

职责：WebSocket 连接管理、心跳机制、JSON-RPC 2.0 协议类型、`send_rpc()`、`send_ack()`。

**Files:**
- Modify: `src/connection/protocol.rs`
- Modify: `src/connection/mod.rs`

- [ ] **Step 1: 在 connection/protocol.rs 末尾写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_type_constants() {
        assert_eq!(WkMessageType::TEXT, 1);
        assert_eq!(WkMessageType::IMAGE, 2);
        assert_eq!(WkMessageType::MARKDOWN, 14);
        assert_eq!(WkMessageType::INTERACTIVE_CARD, 20);
        assert_eq!(WkMessageType::INTERACTIVE_RESPONSE, 21);
        assert_eq!(WkMessageType::CMD, 99);
    }

    #[test]
    fn channel_type_constants() {
        assert_eq!(WkChannelType::PERSONAL, 1);
        assert_eq!(WkChannelType::GROUP, 2);
    }

    #[test]
    fn jsonrpc_request_roundtrip() {
        let req: JsonRpcRequest<serde_json::Value> = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "ping".to_string(),
            id: "abc".to_string(),
            params: serde_json::json!({}),
        };
        let s = serde_json::to_string(&req).unwrap();
        assert!(s.contains("\"jsonrpc\":\"2.0\""));
        assert!(s.contains("\"method\":\"ping\""));
        assert!(s.contains("\"id\":\"abc\""));
    }

    #[test]
    fn recv_notification_params_deserializes() {
        let json = r#"{
            "messageId":"m1","messageSeq":5,"fromUid":"u1",
            "channelId":"c1","channelType":1,"payload":"dGVzdA==","timestamp":9999
        }"#;
        let p: RecvNotificationParams = serde_json::from_str(json).unwrap();
        assert_eq!(p.message_id, "m1");
        assert_eq!(p.channel_type, 1);
        assert_eq!(p.timestamp, 9999);
    }

    #[test]
    fn header_skips_none_fields() {
        let h = Header::default();
        assert_eq!(serde_json::to_string(&h).unwrap(), "{}");
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

```powershell
cargo test -p zeroclaw-channel-wukongim connection::protocol
```

期望：编译错误（类型未定义）。

- [ ] **Step 3: 实现 connection/protocol.rs**

```rust
// src/connection/protocol.rs
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub const WUKONGIM_RPC_VERSION: &str = "2.0";

pub struct WkMessageType;
impl WkMessageType {
    pub const TEXT: u32 = 1;
    pub const IMAGE: u32 = 2;
    pub const MARKDOWN: u32 = 14;
    pub const INTERACTIVE_CARD: u32 = 20;
    pub const INTERACTIVE_RESPONSE: u32 = 21;
    pub const CMD: u32 = 99;
}

pub struct WkChannelType;
impl WkChannelType {
    pub const PERSONAL: u8 = 1;
    pub const GROUP: u8 = 2;
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcRequest<P> {
    pub jsonrpc: String,
    pub method: String,
    pub id: String,
    pub params: P,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(bound(deserialize = "R: DeserializeOwned"))]
pub struct JsonRpcResponse<R> {
    pub jsonrpc: String,
    pub id: Option<String>,
    pub result: Option<R>,
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(bound(deserialize = "P: DeserializeOwned"))]
pub struct JsonRpcNotification<P> {
    pub jsonrpc: String,
    pub method: String,
    pub params: P,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Header {
    #[serde(rename = "noPersist", skip_serializing_if = "Option::is_none")]
    pub no_persist: Option<bool>,
    #[serde(rename = "redDot", skip_serializing_if = "Option::is_none")]
    pub red_dot: Option<bool>,
    #[serde(rename = "syncOnce", skip_serializing_if = "Option::is_none")]
    pub sync_once: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dup: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ConnectParams {
    pub uid: String,
    pub token: String,
    #[serde(rename = "deviceId")]
    pub device_id: String,
    #[serde(rename = "deviceFlag")]
    pub device_flag: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<i32>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SendParams {
    #[serde(rename = "fromUid", skip_serializing_if = "Option::is_none")]
    pub from_uid: Option<String>,
    #[serde(rename = "clientMsgNo")]
    pub client_msg_no: String,
    #[serde(rename = "channelId")]
    pub channel_id: String,
    #[serde(rename = "channelType")]
    pub channel_type: u8,
    pub payload: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<Header>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setting: Option<u32>,
    #[serde(rename = "msgKey", skip_serializing_if = "Option::is_none")]
    pub msg_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expire: Option<u32>,
    #[serde(rename = "streamNo", skip_serializing_if = "Option::is_none")]
    pub stream_no: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RecvNotificationParams {
    #[serde(rename = "messageId")]
    pub message_id: String,
    #[serde(rename = "messageSeq")]
    pub message_seq: u32,
    #[serde(rename = "fromUid")]
    pub from_uid: String,
    #[serde(rename = "channelId")]
    pub channel_id: String,
    #[serde(rename = "channelType")]
    pub channel_type: u8,
    pub payload: String,
    pub timestamp: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RecvAckParams {
    #[serde(rename = "messageId")]
    pub message_id: String,
    #[serde(rename = "messageSeq")]
    pub message_seq: u32,
}

// tests block (written in Step 1) goes here
```

- [ ] **Step 4: 实现 connection/mod.rs**

`connection/mod.rs` 提供 `WsSink` 类型别名、心跳常量，以及两个方法签名（实际实现在 channel.rs 中以 `&WuKongIMChannel` 方法的形式存在；这里只公开常量与类型供其他模块使用）：

```rust
// src/connection/mod.rs
pub mod protocol;

pub use protocol::{
    ConnectParams, JsonRpcError, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
    RecvAckParams, RecvNotificationParams, SendParams, Header,
    WkChannelType, WkMessageType, WUKONGIM_RPC_VERSION,
};

use std::time::Duration;
use tokio_tungstenite::tungstenite::Message as WsMsg;

pub const PING_INTERVAL: Duration = Duration::from_secs(30);
pub const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(90);

pub type WsSink = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    WsMsg,
>;
```

- [ ] **Step 5: 运行测试确认通过**

```powershell
cargo test -p zeroclaw-channel-wukongim connection
```

期望：5 tests passed。

- [ ] **Step 6: Commit**

```bash
git add crates/zeroclaw-channel-wukongim/src/connection/
git commit -m "feat(channel-wukongim): add connection module — JSON-RPC protocol types and WS constants"
```

---

## Task 3: messaging/ — 消息收发

职责：`Channel::send()` 的消息编码、`listen()` 中消息帧的解析与分发、图片与 Markdown 媒体处理。

**Files:**
- Modify: `src/messaging/media.rs`
- Modify: `src/messaging/mod.rs`

- [ ] **Step 1: 在 messaging/media.rs 末尾写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_no_images_from_plain_text() {
        assert!(extract_markdown_images("Hello world").is_empty());
    }

    #[test]
    fn extract_single_image() {
        let imgs = extract_markdown_images("![logo](https://example.com/logo.png)");
        assert_eq!(imgs.len(), 1);
        assert_eq!(imgs[0].0, "logo");
        assert_eq!(imgs[0].1, "https://example.com/logo.png");
    }

    #[test]
    fn extract_multiple_images() {
        let text = "![a](https://a.com/a.png) text ![b](https://b.com/b.jpg)";
        let imgs = extract_markdown_images(text);
        assert_eq!(imgs.len(), 2);
        assert_eq!(imgs[0].1, "https://a.com/a.png");
        assert_eq!(imgs[1].1, "https://b.com/b.jpg");
    }

    #[test]
    fn detect_png_by_magic_bytes() {
        let png: &[u8] = &[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 0, 0];
        assert_eq!(detect_image_mime(None, png).as_deref(), Some("image/png"));
    }

    #[test]
    fn detect_jpeg_by_magic_bytes() {
        let jpeg: &[u8] = &[0xff, 0xd8, 0xff, 0xe0, 0, 0, 0, 0];
        assert_eq!(detect_image_mime(None, jpeg).as_deref(), Some("image/jpeg"));
    }

    #[test]
    fn detect_mime_falls_back_to_content_type() {
        assert_eq!(
            detect_image_mime(Some("image/webp; charset=utf-8"), &[0u8; 4]).as_deref(),
            Some("image/webp")
        );
    }

    #[test]
    fn detect_non_image_returns_none() {
        assert!(detect_image_mime(Some("application/json"), &[0u8; 4]).is_none());
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

```powershell
cargo test -p zeroclaw-channel-wukongim messaging::media
```

期望：编译错误。

- [ ] **Step 3: 实现 messaging/media.rs**

```rust
// src/messaging/media.rs
use base64::Engine;
use std::time::Duration;

const IMAGE_MAX_BYTES: usize = 5 * 1024 * 1024;

const SUPPORTED_IMAGE_MIMES: &[&str] =
    &["image/png", "image/jpeg", "image/gif", "image/webp", "image/bmp"];

pub fn detect_image_mime(content_type: Option<&str>, bytes: &[u8]) -> Option<String> {
    if bytes.len() >= 8
        && bytes.starts_with(&[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'])
    {
        return Some("image/png".to_string());
    }
    if bytes.len() >= 3 && bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some("image/jpeg".to_string());
    }
    if bytes.len() >= 6
        && (bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"))
    {
        return Some("image/gif".to_string());
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("image/webp".to_string());
    }
    if bytes.len() >= 2 && bytes.starts_with(b"BM") {
        return Some("image/bmp".to_string());
    }
    content_type
        .and_then(|ct| ct.split(';').next())
        .map(|ct| ct.trim().to_lowercase())
        .filter(|ct| ct.starts_with("image/"))
}

pub async fn download_image_as_base64(url: &str) -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let resp = match client.get(url).send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("wukongim media: request failed: url={url}, err={e}");
            return None;
        }
    };
    if !resp.status().is_success() {
        tracing::warn!("wukongim media: HTTP {}: {url}", resp.status());
        return None;
    }
    if let Some(cl) = resp.content_length()
        && cl > IMAGE_MAX_BYTES as u64
    {
        tracing::warn!("wukongim media: image too large ({cl} bytes): {url}");
        return None;
    }

    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("wukongim media: body read failed: {url}, {e}");
            return None;
        }
    };
    if bytes.is_empty() || bytes.len() > IMAGE_MAX_BYTES {
        return None;
    }

    let mime = match detect_image_mime(content_type.as_deref(), &bytes) {
        Some(m) if SUPPORTED_IMAGE_MIMES.contains(&m.as_str()) => m,
        other => {
            tracing::warn!("wukongim media: unsupported MIME {other:?}: {url}");
            return None;
        }
    };

    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Some(format!("[IMAGE:data:{mime};base64,{encoded}]"))
}

pub fn extract_markdown_images(text: &str) -> Vec<(String, String)> {
    let mut images = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("![") {
        let after = &rest[start + 2..];
        if let Some(cb) = after.find(']') {
            let alt = after[..cb].to_string();
            let tail = &after[cb + 1..];
            if let Some(inner) = tail.strip_prefix('(') {
                if let Some(pe) = inner.find(')') {
                    images.push((alt, inner[..pe].to_string()));
                    rest = &tail[pe + 1..];
                    continue;
                }
            }
        }
        break;
    }
    images
}

pub async fn process_markdown_with_images(text: &str) -> String {
    let mut result = text.to_string();
    for (alt, url) in extract_markdown_images(text) {
        if let Some(marker) = download_image_as_base64(&url).await {
            result = result.replace(
                &format!("![{}]({})", alt, url),
                &format!("![{}]({})", alt, marker),
            );
        }
    }
    result
}

// tests block (Step 1) goes here
```

- [ ] **Step 4: 实现 messaging/mod.rs**

`messaging/mod.rs` 公开子模块并提供 `encode_text_payload()` 工具函数（供 `channel.rs` 的 `send()` 调用）：

```rust
// src/messaging/mod.rs
pub mod media;

pub use media::{download_image_as_base64, extract_markdown_images, process_markdown_with_images};

use base64::Engine;

/// Encode a text content string as a WuKongIM type-1 Base64 payload.
pub fn encode_text_payload(content: &str) -> anyhow::Result<String> {
    let obj = serde_json::json!({ "type": 1, "content": content });
    let json = serde_json::to_string(&obj)?;
    Ok(base64::engine::general_purpose::STANDARD.encode(json))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_text_payload_is_valid_base64_json() {
        let b64 = encode_text_payload("hello").unwrap();
        let decoded = base64::engine::general_purpose::STANDARD.decode(&b64).unwrap();
        let val: serde_json::Value = serde_json::from_slice(&decoded).unwrap();
        assert_eq!(val["type"], 1);
        assert_eq!(val["content"], "hello");
    }
}
```

- [ ] **Step 5: 运行测试确认通过**

```powershell
cargo test -p zeroclaw-channel-wukongim messaging
```

期望：8 tests passed（7 media + 1 mod）。

- [ ] **Step 6: Commit**

```bash
git add crates/zeroclaw-channel-wukongim/src/messaging/
git commit -m "feat(channel-wukongim): add messaging module — media download and payload encoding"
```

---

## Task 4: filter/ — 权限与过滤

职责：用户白名单校验、收件人解析、群组 mention_only 过滤逻辑。

**Files:**
- Modify: `src/filter/mod.rs`

- [ ] **Step 1: 在 filter/mod.rs 末尾写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_allows_everyone() {
        assert!(is_user_allowed(&["*".to_string()], "any-uid"));
    }

    #[test]
    fn specific_list_allows_only_listed() {
        let list = vec!["u1".to_string(), "u2".to_string()];
        assert!(is_user_allowed(&list, "u1"));
        assert!(!is_user_allowed(&list, "u3"));
    }

    #[test]
    fn empty_list_denies_all() {
        assert!(!is_user_allowed(&[], "anyone"));
    }

    #[test]
    fn parse_recipient_defaults_to_personal() {
        let (id, t) = parse_recipient("user123");
        assert_eq!(id, "user123");
        assert_eq!(t, 1u8);
    }

    #[test]
    fn parse_recipient_group_prefix() {
        let (id, t) = parse_recipient("2:group456");
        assert_eq!(id, "group456");
        assert_eq!(t, 2u8);
    }

    #[test]
    fn mention_check_uid_in_uids_array() {
        let payload = serde_json::json!({
            "mention": { "uids": ["bot001"] }
        });
        assert!(is_mentioned("bot001", &payload, ""));
    }

    #[test]
    fn mention_check_all_flag() {
        let payload = serde_json::json!({ "mention": { "all": 1 } });
        assert!(is_mentioned("anybot", &payload, ""));
    }

    #[test]
    fn mention_check_at_in_text() {
        let payload = serde_json::json!({});
        assert!(is_mentioned("bot001", &payload, "@bot001 please help"));
    }

    #[test]
    fn mention_check_not_mentioned() {
        let payload = serde_json::json!({});
        assert!(!is_mentioned("bot001", &payload, "hello world"));
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

```powershell
cargo test -p zeroclaw-channel-wukongim filter
```

期望：编译错误。

- [ ] **Step 3: 实现 filter/mod.rs**

```rust
// src/filter/mod.rs
use crate::connection::WkChannelType;

/// Returns true if `uid` is permitted by the allowlist.
/// An empty list denies everyone; a list containing `"*"` allows everyone.
pub fn is_user_allowed(allowed_users: &[String], uid: &str) -> bool {
    allowed_users.iter().any(|u| u == "*" || u == uid)
}

/// Parse a `recipient` string into `(channel_id, channel_type)`.
/// Format: `"<type>:<id>"` (e.g. `"2:group123"`) or bare `"<id>"` (personal).
pub fn parse_recipient(recipient: &str) -> (String, u8) {
    if let Some(pos) = recipient.find(':') {
        let (t_str, rest) = recipient.split_at(pos);
        let id = rest[1..].to_string();
        let t = t_str.parse::<u8>().unwrap_or(WkChannelType::PERSONAL);
        (id, t)
    } else {
        (recipient.to_string(), WkChannelType::PERSONAL)
    }
}

/// Returns true if the bot (`bot_uid`) is @-mentioned in this group message.
/// Checks the `mention` object in `payload_json` and falls back to text content scan.
pub fn is_mentioned(bot_uid: &str, payload_json: &serde_json::Value, content: &str) -> bool {
    if let Some(mention) = payload_json.get("mention") {
        if let Some(all) = mention.get("all") {
            let flagged = all.as_u64() == Some(1)
                || all.as_str() == Some("1")
                || all.as_str() == Some("true")
                || all.as_bool() == Some(true);
            if flagged {
                return true;
            }
        }
        if let Some(uids) = mention.get("uids").and_then(|v| v.as_array()) {
            if uids.iter().any(|u| {
                u.as_str() == Some(bot_uid)
                    || u.as_u64().map(|n| n.to_string()).as_deref() == Some(bot_uid)
            }) {
                return true;
            }
        }
    }
    content.contains(&format!("@{}", bot_uid)) || content.contains("@all")
}

// tests block (Step 1) goes here
```

- [ ] **Step 4: 运行测试确认通过**

```powershell
cargo test -p zeroclaw-channel-wukongim filter
```

期望：9 tests passed。

- [ ] **Step 5: Commit**

```bash
git add crates/zeroclaw-channel-wukongim/src/filter/
git commit -m "feat(channel-wukongim): add filter module — allowlist and mention detection"
```

---

## Task 5: approval/ — 审批流程

职责：审批卡片类型与构建、`request_approval()` 实现、pending_approvals 状态管理。

**Files:**
- Modify: `src/approval/card.rs`
- Modify: `src/approval/mod.rs`

- [ ] **Step 1: 在 approval/card.rs 末尾写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_api::channel::ChannelApprovalRequest;

    fn req(tool: &str, summary: &str) -> ChannelApprovalRequest {
        ChannelApprovalRequest {
            tool_name: tool.to_string(),
            arguments_summary: summary.to_string(),
        }
    }

    #[test]
    fn card_has_type_20() {
        let card = build_approval_card("id1", &req("shell_exec", "cmd: ls"), 300);
        let json = serde_json::to_string(&card).unwrap();
        assert!(json.contains("\"type\":20"));
    }

    #[test]
    fn card_has_approve_and_deny_actions() {
        let card = build_approval_card("id2", &req("shell_exec", "cmd: echo"), 60);
        let actions = card.actions.unwrap();
        assert_eq!(actions[0].value, "approve");
        assert_eq!(actions[1].value, "deny");
    }

    #[test]
    fn cron_add_card_localizes_job_type() {
        let card = build_approval_card(
            "id3",
            &req("cron_add", "job_type: agent, name: daily, schedule: 0 9 * * *"),
            300,
        );
        assert!(card.body.content.contains("智能体"));
        assert!(card.body.content.contains("daily"));
    }

    #[test]
    fn approval_action_deny_deserializes() {
        let json = r#"{"type":21,"approval_id":"id1","action":"deny"}"#;
        let a: WkApprovalAction = serde_json::from_str(json).unwrap();
        assert_eq!(a.action, "deny");
        assert_eq!(a.msg_type, 21);
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

```powershell
cargo test -p zeroclaw-channel-wukongim approval::card
```

期望：编译错误。

- [ ] **Step 3: 实现 approval/card.rs**

```rust
// src/approval/card.rs
use serde::{Deserialize, Serialize};
use zeroclaw_api::channel::ChannelApprovalRequest;
use crate::connection::WkMessageType;

#[derive(Debug, Serialize, Deserialize)]
pub struct WkApprovalCard {
    #[serde(rename = "type")]
    pub msg_type: u32,
    pub approval_id: String,
    pub timeout_secs: u64,
    pub title: String,
    pub body: WkApprovalBody,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actions: Option<Vec<WkAction>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WkApprovalBody {
    pub content: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WkAction {
    pub text: String,
    pub value: String,
    pub style: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WkApprovalAction {
    #[serde(rename = "type")]
    pub msg_type: u32,
    pub approval_id: String,
    pub action: String,
}

pub fn build_approval_card(
    approval_id: &str,
    request: &ChannelApprovalRequest,
    timeout_secs: u64,
) -> WkApprovalCard {
    let (title, content) = if request.tool_name == "cron_add" {
        let mut summary = request.arguments_summary.clone();
        summary = summary
            .replace("job_type: agent, ", "任务类型: 智能体, ")
            .replace("job_type: shell, ", "任务类型: 脚本, ")
            .replace("name: ", "任务名称: ")
            .replace("prompt: ", "提示词: ")
            .replace("command: ", "执行命令: ")
            .replace("schedule: ", "\n执行计划: ");

        let mut time_info = summary
            .split("\n执行计划: ")
            .last()
            .unwrap_or("按计划执行")
            .to_string();
        if time_info.contains("\"at\":") {
            if let Some(start) = time_info.find("\"at\":\"") {
                let rest = &time_info[start + 6..];
                if let Some(end) = rest.find('"') {
                    time_info = rest[..end].replace('T', " ").replace('Z', " (UTC)");
                }
            }
        }
        (
            "📋 任务执行审批",
            format!(
                "1. **执行的是什么**\n添加定时任务: **{}**\n\n2. **执行的时间相关信息**\n{}\n\n3. **执行内容的总结**\n{}",
                request.tool_name, time_info, summary
            ),
        )
    } else {
        (
            "📋 任务执行审批",
            format!(
                "🔧 智能体请求执行: **{}**\n\n**执行内容总结**:\n{}",
                request.tool_name, request.arguments_summary
            ),
        )
    };

    WkApprovalCard {
        msg_type: WkMessageType::INTERACTIVE_CARD,
        approval_id: approval_id.to_string(),
        timeout_secs,
        title: title.to_string(),
        body: WkApprovalBody { content: content.to_string() },
        actions: Some(vec![
            WkAction { text: "同意".to_string(), value: "approve".to_string(), style: "primary".to_string() },
            WkAction { text: "拒绝".to_string(), value: "deny".to_string(), style: "danger".to_string() },
        ]),
    }
}

// tests block (Step 1) goes here
```

- [ ] **Step 4: 实现 approval/mod.rs**

```rust
// src/approval/mod.rs
pub mod card;

pub use card::{WkApprovalAction, WkApprovalCard, build_approval_card};

use std::collections::HashMap;
use tokio::sync::RwLock;
use zeroclaw_api::channel::ChannelApprovalResponse;

/// Type alias for the pending approvals map.
/// Key = approval_id, Value = oneshot sender to resolve the approval.
pub type PendingApprovals =
    RwLock<HashMap<String, tokio::sync::oneshot::Sender<ChannelApprovalResponse>>>;
```

- [ ] **Step 5: 运行测试确认通过**

```powershell
cargo test -p zeroclaw-channel-wukongim approval
```

期望：4 tests passed。

- [ ] **Step 6: Commit**

```bash
git add crates/zeroclaw-channel-wukongim/src/approval/
git commit -m "feat(channel-wukongim): add approval module — card types, builder, and pending state alias"
```

---

## Task 6: config/ — 配置项

职责：对外公开 `WuKongIMConfig` 类型、提供 `from_config()` 构造器入口。

**Files:**
- Modify: `src/config/mod.rs`

- [ ] **Step 1: 在 config/mod.rs 末尾写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_fields() {
        let cfg = WuKongIMConfig {
            enabled: true,
            ws_url: "ws://localhost:5200".to_string(),
            uid: "bot".to_string(),
            token: "tok".to_string(),
            allowed_users: vec!["*".to_string()],
            mention_only: false,
            approval_timeout_secs: 300,
        };
        assert_eq!(cfg.approval_timeout_secs, 300);
        assert!(!cfg.mention_only);
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

```powershell
cargo test -p zeroclaw-channel-wukongim config
```

期望：编译错误。

- [ ] **Step 3: 实现 config/mod.rs**

```rust
// src/config/mod.rs
pub use zeroclaw_config::schema::WuKongIMConfig;

// tests block (Step 1) goes here
```

- [ ] **Step 4: 运行测试确认通过**

```powershell
cargo test -p zeroclaw-channel-wukongim config
```

期望：1 test passed。

- [ ] **Step 5: Commit**

```bash
git add crates/zeroclaw-channel-wukongim/src/config/
git commit -m "feat(channel-wukongim): add config module — re-export WuKongIMConfig"
```

---

## Task 7: channel.rs — WuKongIMChannel 结构体与 Channel trait

职责：定义 `WuKongIMChannel` 结构体（持有各模块所需状态），实现 `Channel` trait，通过调用各模块函数协调完整流程。

**Files:**
- Modify: `src/channel.rs`

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WuKongIMConfig;

    fn make_config(allowed: Vec<String>, mention_only: bool) -> WuKongIMConfig {
        WuKongIMConfig {
            enabled: true,
            ws_url: "ws://localhost:5200".to_string(),
            uid: "bot001".to_string(),
            token: "tok".to_string(),
            allowed_users: allowed,
            mention_only,
            approval_timeout_secs: 300,
        }
    }

    #[test]
    fn from_config_maps_fields() {
        let ch = WuKongIMChannel::from_config(&make_config(vec!["*".to_string()], true));
        assert_eq!(ch.ws_url, "ws://localhost:5200");
        assert_eq!(ch.uid, "bot001");
        assert!(ch.mention_only);
        assert_eq!(ch.approval_timeout_secs, 300);
    }

    #[test]
    fn channel_name_is_wukongim() {
        use zeroclaw_api::channel::Channel;
        let ch = WuKongIMChannel::from_config(&make_config(vec![], false));
        assert_eq!(ch.name(), "wukongim");
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

```powershell
cargo test -p zeroclaw-channel-wukongim channel
```

期望：编译错误（结构体未定义）。

- [ ] **Step 3: 实现 channel.rs**

```rust
// src/channel.rs
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::RwLock;
use tokio_tungstenite::tungstenite::Message as WsMsg;
use uuid::Uuid;
use zeroclaw_api::channel::{
    Channel, ChannelApprovalRequest, ChannelApprovalResponse, ChannelMessage, SendMessage,
};

use crate::approval::{PendingApprovals, WkApprovalAction, build_approval_card};
use crate::config::WuKongIMConfig;
use crate::connection::{
    ConnectParams, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
    RecvAckParams, RecvNotificationParams, SendParams, WkChannelType, WkMessageType,
    WsSink, HEARTBEAT_TIMEOUT, PING_INTERVAL, WUKONGIM_RPC_VERSION,
};
use crate::filter::{is_mentioned, is_user_allowed, parse_recipient};
use crate::messaging::{
    download_image_as_base64, encode_text_payload, process_markdown_with_images,
};

#[derive(Clone)]
pub struct WuKongIMChannel {
    pub(crate) ws_url: String,
    pub(crate) uid: String,
    pub(crate) token: String,
    pub(crate) device_id: String,
    pub(crate) allowed_users: Vec<String>,
    pub(crate) approval_timeout_secs: u64,
    pub(crate) mention_only: bool,
    pub(crate) pending_responses:
        Arc<RwLock<HashMap<String, tokio::sync::oneshot::Sender<serde_json::Value>>>>,
    pub(crate) pending_approvals: Arc<PendingApprovals>,
    pub(crate) ws_sink: Arc<RwLock<Option<WsSink>>>,
}

impl WuKongIMChannel {
    pub fn from_config(config: &WuKongIMConfig) -> Self {
        Self {
            ws_url: config.ws_url.clone(),
            uid: config.uid.clone(),
            token: config.token.clone(),
            device_id: format!("zeroclaw-{}", &Uuid::new_v4().to_string()[..8]),
            allowed_users: config.allowed_users.clone(),
            approval_timeout_secs: config.approval_timeout_secs,
            mention_only: config.mention_only,
            pending_responses: Arc::new(RwLock::new(HashMap::new())),
            pending_approvals: Arc::new(RwLock::new(HashMap::new())),
            ws_sink: Arc::new(RwLock::new(None)),
        }
    }

    async fn send_rpc<P: serde::Serialize, R: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: P,
    ) -> anyhow::Result<R> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let id = Uuid::new_v4().to_string();
        let req = JsonRpcRequest {
            jsonrpc: WUKONGIM_RPC_VERSION.to_string(),
            method: method.to_string(),
            id: id.clone(),
            params,
        };
        self.pending_responses.write().await.insert(id.clone(), tx);
        let msg = serde_json::to_string(&req)?;
        {
            let mut g = self.ws_sink.write().await;
            if let Some(s) = g.as_mut() {
                tracing::info!("WuKongIM: RPC {} id={}", method, id);
                s.send(WsMsg::Text(msg.into())).await?;
            } else {
                anyhow::bail!("WuKongIM: WebSocket not connected");
            }
        }
        match tokio::time::timeout(Duration::from_secs(30), rx).await {
            Ok(Ok(val)) => {
                let resp: JsonRpcResponse<R> = serde_json::from_value(val)?;
                if let Some(err) = resp.error {
                    anyhow::bail!("WuKongIM RPC error: {} (code {})", err.message, err.code);
                }
                resp.result.ok_or_else(|| anyhow::anyhow!("WuKongIM RPC: missing result"))
            }
            _ => {
                self.pending_responses.write().await.remove(&id);
                anyhow::bail!("WuKongIM RPC timeout: {}", method);
            }
        }
    }

    async fn send_ack(&self, message_id: String, message_seq: u32) -> anyhow::Result<()> {
        let req = JsonRpcNotification {
            jsonrpc: WUKONGIM_RPC_VERSION.to_string(),
            method: "recvack".to_string(),
            params: RecvAckParams { message_id, message_seq },
        };
        let msg = serde_json::to_string(&req)?;
        let mut g = self.ws_sink.write().await;
        if let Some(s) = g.as_mut() {
            s.send(WsMsg::Text(msg.into())).await?;
        }
        Ok(())
    }
}

#[async_trait]
impl Channel for WuKongIMChannel {
    fn name(&self) -> &str {
        "wukongim"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let content = match message.content.as_str() {
            "ERR:context_window_exceeded" => "⚠️ 模型服务暂时遇到问题，请稍后重试。",
            other => other,
        };
        let payload_b64 = encode_text_payload(content)?;
        let (channel_id, channel_type) = parse_recipient(&message.recipient);
        let params = SendParams {
            from_uid: Some(self.uid.clone()),
            client_msg_no: Uuid::new_v4().to_string(),
            channel_id,
            channel_type,
            payload: payload_b64,
            header: None,
            setting: None,
            msg_key: None,
            expire: None,
            stream_no: None,
            topic: None,
        };
        let _: serde_json::Value = self.send_rpc("send", params).await?;
        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        tracing::info!("WuKongIM: connecting to {}", self.ws_url);
        let (ws_stream, _) = tokio_tungstenite::connect_async(&self.ws_url).await?;
        let (write, mut read) = ws_stream.split();
        *self.ws_sink.write().await = Some(write);

        // Handshake
        {
            let connect_id = Uuid::new_v4().to_string();
            let req = JsonRpcRequest {
                jsonrpc: WUKONGIM_RPC_VERSION.to_string(),
                method: "connect".to_string(),
                id: connect_id,
                params: ConnectParams {
                    uid: self.uid.clone(),
                    token: self.token.clone(),
                    device_id: self.device_id.clone(),
                    device_flag: 1,
                    version: Some(2),
                },
            };
            let msg = serde_json::to_string(&req)?;
            if let Some(s) = self.ws_sink.write().await.as_mut() {
                s.send(WsMsg::Text(msg.into())).await?;
            }
            let connack = tokio::time::timeout(Duration::from_secs(15), read.next())
                .await
                .map_err(|_| anyhow::anyhow!("WuKongIM: connect timeout"))?
                .ok_or_else(|| anyhow::anyhow!("WuKongIM: stream closed during connect"))??;
            if let WsMsg::Text(text) = connack {
                let val: serde_json::Value = serde_json::from_str(&text)?;
                if let Some(err) = val.get("error").filter(|e| !e.is_null()) {
                    anyhow::bail!("WuKongIM: connect rejected: {}", err);
                }
            }
        }
        tracing::info!("WuKongIM: connected as {}", self.uid);

        let mut hb = tokio::time::interval(PING_INTERVAL);
        let mut last_activity = Instant::now();

        loop {
            tokio::select! {
                _ = hb.tick() => {
                    if last_activity.elapsed() > HEARTBEAT_TIMEOUT {
                        anyhow::bail!("WuKongIM: heartbeat timeout");
                    }
                    let ping = JsonRpcRequest {
                        jsonrpc: WUKONGIM_RPC_VERSION.to_string(),
                        method: "ping".to_string(),
                        id: Uuid::new_v4().to_string(),
                        params: serde_json::json!({}),
                    };
                    if let Ok(msg) = serde_json::to_string(&ping) {
                        if let Some(s) = self.ws_sink.write().await.as_mut() {
                            let _ = s.send(WsMsg::Text(msg.into())).await;
                        }
                    }
                }
                frame = read.next() => {
                    let frame = frame.ok_or_else(|| anyhow::anyhow!("WuKongIM: stream closed"))??;
                    last_activity = Instant::now();
                    let WsMsg::Text(text) = frame else { continue; };
                    let val: serde_json::Value = serde_json::from_str(&text)?;

                    // pong
                    if val.get("method").and_then(|m| m.as_str()) == Some("pong") { continue; }

                    // RPC response (matched by id)
                    let msg_id = val.get("id").and_then(|i| {
                        if i.is_string() { i.as_str().map(str::to_string) }
                        else if i.is_number() { Some(i.to_string()) }
                        else { None }
                    });
                    if let Some(id) = msg_id {
                        if let Some(resp_tx) = self.pending_responses.write().await.remove(&id) {
                            let _ = resp_tx.send(val);
                            continue;
                        }
                    }

                    // Inbound message notification
                    if val.get("method").and_then(|m| m.as_str()) != Some("recv") { continue; }
                    let notif: JsonRpcNotification<RecvNotificationParams> = serde_json::from_value(val)?;
                    let params = notif.params;

                    if params.from_uid == self.uid { continue; }
                    if !is_user_allowed(&self.allowed_users, &params.from_uid) {
                        tracing::warn!("WuKongIM: unauthorized sender {}", params.from_uid);
                        continue;
                    }

                    let decoded = base64::engine::general_purpose::STANDARD.decode(&params.payload)?;
                    let payload_json: serde_json::Value = serde_json::from_slice(&decoded)?;
                    let msg_type = payload_json.get("type").and_then(|t| t.as_u64()).unwrap_or(0);

                    // System command — ack and skip
                    if msg_type == WkMessageType::CMD as u64 || payload_json.get("cmd").is_some() {
                        let _ = self.send_ack(params.message_id.clone(), params.message_seq).await;
                        continue;
                    }

                    // Interactive response (approval answer)
                    if msg_type == WkMessageType::INTERACTIVE_RESPONSE as u64 {
                        let _ = self.send_ack(params.message_id.clone(), params.message_seq).await;
                        if let Ok(action) = serde_json::from_value::<WkApprovalAction>(payload_json) {
                            let resp = match action.action.as_str() {
                                "approve" => Some(ChannelApprovalResponse::Approve),
                                "deny"    => Some(ChannelApprovalResponse::Deny),
                                "always"  => Some(ChannelApprovalResponse::AlwaysApprove),
                                _         => None,
                            };
                            if let Some(r) = resp {
                                if let Some(ptx) = self.pending_approvals.write().await.remove(&action.approval_id) {
                                    let _ = ptx.send(r);
                                }
                            }
                        }
                        continue;
                    }

                    // mention_only filter for group messages
                    if self.mention_only && params.channel_type == WkChannelType::GROUP {
                        let content_str = payload_json.get("content").and_then(|c| c.as_str()).unwrap_or("");
                        if !is_mentioned(&self.uid, &payload_json, content_str) {
                            continue;
                        }
                    }

                    let _ = self.send_ack(params.message_id.clone(), params.message_seq).await;

                    // Decode content by message type
                    let content = match msg_type {
                        2 => {
                            let url = payload_json.get("url").and_then(|u| u.as_str()).unwrap_or("");
                            download_image_as_base64(url).await
                                .unwrap_or_else(|| format!("[图片下载失败]{}\n请直接描述图片内容", url))
                        }
                        5 => {
                            let url  = payload_json.get("url").and_then(|u| u.as_str()).unwrap_or("");
                            let name = payload_json.get("name").and_then(|n| n.as_str()).unwrap_or("文件");
                            format!("[文件]{}: {}", name, url)
                        }
                        14 => {
                            let text = payload_json
                                .get("content").and_then(|c| c.get("text")).and_then(|t| t.as_str())
                                .unwrap_or("");
                            process_markdown_with_images(text).await
                        }
                        _ => payload_json.get("content").and_then(|c| c.as_str()).unwrap_or("").to_string(),
                    };

                    let target_id = if params.channel_type == WkChannelType::PERSONAL {
                        &params.from_uid
                    } else {
                        &params.channel_id
                    };

                    let ch_msg = ChannelMessage {
                        id: params.message_id,
                        sender: target_id.clone(),
                        reply_target: format!("{}:{}", params.channel_type, target_id),
                        content,
                        channel: "wukongim".to_string(),
                        timestamp: params.timestamp as u64,
                        thread_ts: None,
                        interruption_scope_id: None,
                        attachments: vec![],
                    };
                    if tx.send(ch_msg).await.is_err() { break; }
                }
            }
        }
        Ok(())
    }

    async fn health_check(&self) -> bool {
        let addr = self.ws_url.trim_start_matches("ws://").trim_start_matches("wss://");
        tokio::net::TcpStream::connect(addr).await.is_ok()
    }

    async fn request_approval(
        &self,
        recipient: &str,
        request: &ChannelApprovalRequest,
    ) -> anyhow::Result<Option<ChannelApprovalResponse>> {
        let approval_id = Uuid::new_v4().to_string();
        let card = build_approval_card(&approval_id, request, self.approval_timeout_secs);
        let payload_b64 =
            base64::engine::general_purpose::STANDARD.encode(serde_json::to_string(&card)?);
        let (channel_id, channel_type) = parse_recipient(recipient);
        let params = SendParams {
            from_uid: Some(self.uid.clone()),
            client_msg_no: Uuid::new_v4().to_string(),
            channel_id,
            channel_type,
            payload: payload_b64,
            header: None, setting: None, msg_key: None, expire: None, stream_no: None, topic: None,
        };
        let (otx, orx) = tokio::sync::oneshot::channel();
        self.pending_approvals.write().await.insert(approval_id.clone(), otx);
        self.send_rpc::<_, serde_json::Value>("send", params).await?;
        match tokio::time::timeout(Duration::from_secs(self.approval_timeout_secs), orx).await {
            Ok(Ok(resp)) => Ok(Some(resp)),
            _ => {
                self.pending_approvals.write().await.remove(&approval_id);
                Ok(Some(ChannelApprovalResponse::Deny))
            }
        }
    }
}

// tests block (Step 1) goes here
```

- [ ] **Step 4: 运行测试确认通过**

```powershell
cargo test -p zeroclaw-channel-wukongim
```

期望：全部 tests passed（包含前几个 Task 的测试）。

- [ ] **Step 5: Commit**

```bash
git add crates/zeroclaw-channel-wukongim/src/channel.rs crates/zeroclaw-channel-wukongim/src/lib.rs
git commit -m "feat(channel-wukongim): implement WuKongIMChannel orchestrating all 5 domain modules"
```

---

## Task 8: 接入 zeroclaw-channels — 替换依赖与 import

**Files:**
- Modify: `crates/zeroclaw-channels/Cargo.toml`
- Modify: `crates/zeroclaw-channels/src/lib.rs`
- Modify: `crates/zeroclaw-channels/src/orchestrator/mod.rs`
- Delete: `crates/zeroclaw-channels/src/wukongim.rs`

- [ ] **Step 1: 更新 zeroclaw-channels/Cargo.toml**

在 `[dependencies]` 中添加：

```toml
zeroclaw-channel-wukongim = { workspace = true, optional = true }
```

将 `[features]` 中的：

```toml
channel-wukongim = []
```

改为：

```toml
channel-wukongim = ["dep:zeroclaw-channel-wukongim"]
```

- [ ] **Step 2: 更新 zeroclaw-channels/src/lib.rs**

将：

```rust
#[cfg(feature = "channel-wukongim")]
pub mod wukongim;
```

替换为：

```rust
#[cfg(feature = "channel-wukongim")]
pub use zeroclaw_channel_wukongim::WuKongIMChannel;
```

- [ ] **Step 3: 更新 orchestrator/mod.rs**

将：

```rust
#[cfg(feature = "channel-wukongim")]
pub use crate::wukongim::WuKongIMChannel;
```

替换为：

```rust
#[cfg(feature = "channel-wukongim")]
pub use zeroclaw_channel_wukongim::WuKongIMChannel;
```

（orchestrator 内其他三处使用 `WuKongIMChannel::from_config(wk)` 的代码无需改动——`WuKongIMChannel` 已通过 `pub use` 引入当前命名空间。）

- [ ] **Step 4: 删除旧文件**

```powershell
Remove-Item "crates/zeroclaw-channels/src/wukongim.rs"
```

- [ ] **Step 5: 验证带 feature 时编译**

```powershell
cargo check -p zeroclaw-channels --features channel-wukongim
```

期望：无错误。

- [ ] **Step 6: 验证不带 feature 时编译**

```powershell
cargo check -p zeroclaw-channels
```

期望：无错误。

- [ ] **Step 7: Commit**

```bash
git add crates/zeroclaw-channels/Cargo.toml \
        crates/zeroclaw-channels/src/lib.rs \
        crates/zeroclaw-channels/src/orchestrator/mod.rs \
        Cargo.lock
git rm crates/zeroclaw-channels/src/wukongim.rs
git commit -m "refactor(channels): wire zeroclaw-channel-wukongim as optional dep, remove wukongim.rs"
```

---

## Task 9: 最终验证

- [ ] **Step 1: clippy（新 crate）**

```powershell
cargo clippy -p zeroclaw-channel-wukongim -- -D warnings
```

期望：无 warning。

- [ ] **Step 2: clippy（zeroclaw-channels 带 feature）**

```powershell
cargo clippy -p zeroclaw-channels --features channel-wukongim -- -D warnings
```

期望：无 warning。

- [ ] **Step 3: 格式检查**

```powershell
cargo fmt --all -- --check
```

期望：无差异。

- [ ] **Step 4: 全量 workspace check**

```powershell
cargo check --features agent-runtime
```

期望：无错误。

- [ ] **Step 5: 全量测试**

```powershell
cargo test --features agent-runtime
```

期望：全部通过，无回归。

- [ ] **Step 6: Commit（如 fmt 产生改动）**

```bash
git add -u
git commit -m "style(channel-wukongim): cargo fmt"
```

---

## 自检

### Spec Coverage

| 需求 | Task |
|------|------|
| 独立 crate `zeroclaw-channel-wukongim` | Task 1 |
| 5 个功能域目录：connection/、messaging/、filter/、approval/、config/ | Task 2–6 |
| 顶层 channel.rs 协调各模块 | Task 7 |
| zeroclaw-channels 通过 optional dep 引用，feature 语义不变 | Task 8 |
| 旧 wukongim.rs 删除 | Task 8 Step 4 |
| clippy + fmt + 全量测试 | Task 9 |

### Placeholder Scan

无 TBD/TODO — 所有模块均有完整实现代码。

### Type Consistency

| 跨 Task 引用 | 定义处 | 使用处 | 一致 |
|---|---|---|---|
| `WkMessageType::*` | Task 2 `protocol.rs` | Task 5 `card.rs`, Task 7 `channel.rs` | ✓ |
| `WkChannelType::*` | Task 2 `protocol.rs` | Task 4 `filter/mod.rs`, Task 7 `channel.rs` | ✓ |
| `build_approval_card(id, req, timeout)` | Task 5 `card.rs` | Task 7 `channel.rs` | ✓ |
| `is_user_allowed(list, uid)` | Task 4 `filter/mod.rs` | Task 7 `channel.rs` | ✓ |
| `is_mentioned(uid, json, text)` | Task 4 `filter/mod.rs` | Task 7 `channel.rs` | ✓ |
| `parse_recipient(str)` | Task 4 `filter/mod.rs` | Task 7 `channel.rs` | ✓ |
| `encode_text_payload(str)` | Task 3 `messaging/mod.rs` | Task 7 `channel.rs` | ✓ |
| `download_image_as_base64(url)` | Task 3 `messaging/media.rs` | Task 7 `channel.rs` | ✓ |
| `process_markdown_with_images(str)` | Task 3 `messaging/media.rs` | Task 7 `channel.rs` | ✓ |
| `PendingApprovals` type alias | Task 5 `approval/mod.rs` | Task 7 `channel.rs` | ✓ |
| `WkApprovalAction` | Task 5 `approval/card.rs` | Task 7 `channel.rs` | ✓ |
