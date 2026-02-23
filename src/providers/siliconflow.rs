//! SiliconFlow AI 推理平台 Provider。
//!
//! 接口文档: https://docs.siliconflow.cn/cn/api-reference/chat-completions/chat-completions
//!
//! 使用标准 Bearer Token 授权，endpoint 为 `https://api.siliconflow.cn/v1/chat/completions`。
//! 响应体与 OpenAI chat completions 格式完全兼容，同时支持：
//! - `reasoning_content` 推理字段（GLM-4.7、DeepSeek、Qwen3 等推理模型）
//! - 原生 Function Calling（`tools` / `tool_calls`）
//! - SSE 流式输出（`stream: true`）

use crate::providers::traits::{
    ChatMessage, ChatRequest as ProviderChatRequest, ChatResponse as ProviderChatResponse,
    Provider, ProviderCapabilities, StreamChunk, StreamError, StreamOptions, StreamResult,
    TokenUsage, ToolCall as ProviderToolCall,
};
use crate::tools::ToolSpec;
use async_trait::async_trait;
use futures_util::{stream, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};

/// SiliconFlow 平台接入地址。
const SILICONFLOW_BASE_URL: &str = "https://api.siliconflow.cn/v1";

pub struct SiliconFlowProvider {
    /// Bearer Token（来自 https://cloud.siliconflow.cn/account/ak）
    api_key: Option<String>,
}

// ─── 请求结构体 ─────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<Message>,
    temperature: f64,
    /// 是否启用 SSE 流式输出。
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ToolSpec_>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
}

#[derive(Debug, Serialize)]
struct Message {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    /// 工具调用结果消息：role = "tool"
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    /// Assistant 发起的工具调用
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ToolCall {
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    function: FunctionCall,
}

#[derive(Debug, Serialize, Deserialize)]
struct FunctionCall {
    name: String,
    arguments: String,
}

#[derive(Debug, Serialize)]
struct ToolSpec_ {
    #[serde(rename = "type")]
    kind: String,
    function: ToolFunctionSpec,
}

#[derive(Debug, Serialize)]
struct ToolFunctionSpec {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

// ─── 非流式响应结构体 ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ApiResponse {
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<UsageInfo>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Debug, Deserialize)]
struct ResponseMessage {
    #[serde(default)]
    content: Option<String>,
    /// 推理模型（GLM-4.7、DeepSeek-R1 等）会在此字段输出思维链内容。
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Debug, Deserialize)]
struct UsageInfo {
    #[serde(default)]
    prompt_tokens: Option<u64>,
    #[serde(default)]
    completion_tokens: Option<u64>,
}

// ─── 流式 SSE 响应结构体 ─────────────────────────────────────────────────────

/// SSE 流返回的单个 chunk JSON（`data: {...}` 格式）。
#[derive(Debug, Deserialize)]
struct SseChunkResponse {
    #[serde(default)]
    choices: Vec<SseChoice>,
}

#[derive(Debug, Deserialize)]
struct SseChoice {
    delta: SseDelta,
}

#[derive(Debug, Deserialize)]
struct SseDelta {
    #[serde(default)]
    content: Option<String>,
    /// 推理模型的思维链流式输出字段。
    #[serde(default)]
    reasoning_content: Option<String>,
}

// ─── 私有 SSE 工具函数 ────────────────────────────────────────────────────────

/// 解析单行 SSE 数据，返回文本 delta 或 None（空行/注释/DONE）。
fn parse_sse_line(line: &str) -> StreamResult<Option<String>> {
    let line = line.trim();
    if line.is_empty() || line.starts_with(':') {
        return Ok(None);
    }
    if let Some(data) = line.strip_prefix("data:") {
        let data = data.trim();
        if data == "[DONE]" {
            return Ok(None);
        }
        let chunk: SseChunkResponse = serde_json::from_str(data).map_err(StreamError::Json)?;
        if let Some(choice) = chunk.choices.first() {
            // 优先输出正文内容，其次才是思维链（避免混入推理噪音）
            if let Some(text) = &choice.delta.content {
                if !text.is_empty() {
                    return Ok(Some(text.clone()));
                }
            }
            if let Some(reasoning) = &choice.delta.reasoning_content {
                if !reasoning.is_empty() {
                    return Ok(Some(reasoning.clone()));
                }
            }
        }
    }
    Ok(None)
}

/// 将 SSE 字节流转为 `StreamChunk` 异步流。
fn sse_response_to_stream(
    response: reqwest::Response,
    count_tokens: bool,
) -> stream::BoxStream<'static, StreamResult<StreamChunk>> {
    let (tx, rx) = tokio::sync::mpsc::channel::<StreamResult<StreamChunk>>(128);

    tokio::spawn(async move {
        if let Err(e) = response.error_for_status_ref() {
            let _ = tx.send(Err(StreamError::Http(e))).await;
            return;
        }

        let mut buffer = String::new();
        let mut bytes_stream = response.bytes_stream();

        while let Some(item) = bytes_stream.next().await {
            match item {
                Ok(bytes) => {
                    let text = match String::from_utf8(bytes.to_vec()) {
                        Ok(t) => t,
                        Err(e) => {
                            let _ = tx
                                .send(Err(StreamError::InvalidSse(format!("UTF-8 解码错误: {e}"))))
                                .await;
                            break;
                        }
                    };
                    buffer.push_str(&text);

                    // 逐行处理完整 SSE 行
                    while let Some(pos) = buffer.find('\n') {
                        let line: String = buffer.drain(..=pos).collect();
                        match parse_sse_line(&line) {
                            Ok(Some(content)) => {
                                let mut chunk = StreamChunk::delta(content);
                                if count_tokens {
                                    chunk = chunk.with_token_estimate();
                                }
                                if tx.send(Ok(chunk)).await.is_err() {
                                    return; // 接收端已关闭
                                }
                            }
                            Ok(None) => {}
                            Err(e) => {
                                let _ = tx.send(Err(e)).await;
                                return;
                            }
                        }
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(StreamError::Http(e))).await;
                    break;
                }
            }
        }

        // 发送终止 chunk
        let _ = tx.send(Ok(StreamChunk::final_chunk())).await;
    });

    stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|chunk| (chunk, rx))
    })
    .boxed()
}

// ─── Provider 实现 ──────────────────────────────────────────────────────────

impl SiliconFlowProvider {
    pub fn new(api_key: Option<&str>) -> Self {
        Self {
            api_key: api_key.map(ToString::to_string),
        }
    }

    fn http_client(&self) -> Client {
        crate::config::build_runtime_proxy_client_with_timeouts("provider.siliconflow", 120, 10)
    }

    fn api_key(&self) -> anyhow::Result<&str> {
        self.api_key.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "SiliconFlow API Key 未设置。请通过 `zeroclaw onboard` 配置，\
                 或者设置环境变量 SILICONFLOW_API_KEY。\
                 API Key 可前往 https://cloud.siliconflow.cn/account/ak 获取。"
            )
        })
    }

    fn convert_tools(specs: Option<&[ToolSpec]>) -> Option<Vec<ToolSpec_>> {
        let items = specs?;
        if items.is_empty() {
            return None;
        }
        Some(
            items
                .iter()
                .map(|t| ToolSpec_ {
                    kind: "function".to_string(),
                    function: ToolFunctionSpec {
                        name: t.name.clone(),
                        description: t.description.clone(),
                        parameters: t.parameters.clone(),
                    },
                })
                .collect(),
        )
    }

    fn convert_messages(messages: &[ChatMessage]) -> Vec<Message> {
        messages
            .iter()
            .map(|m| {
                if m.role == "assistant" {
                    if let Ok(val) = serde_json::from_str::<serde_json::Value>(&m.content) {
                        if let Some(tc_val) = val.get("tool_calls") {
                            if let Ok(calls) =
                                serde_json::from_value::<Vec<ProviderToolCall>>(tc_val.clone())
                            {
                                let tool_calls = calls
                                    .into_iter()
                                    .map(|tc| ToolCall {
                                        id: Some(tc.id),
                                        kind: Some("function".to_string()),
                                        function: FunctionCall {
                                            name: tc.name,
                                            arguments: tc.arguments,
                                        },
                                    })
                                    .collect::<Vec<_>>();
                                let content = val
                                    .get("content")
                                    .and_then(serde_json::Value::as_str)
                                    .map(ToString::to_string);
                                return Message {
                                    role: "assistant".to_string(),
                                    content,
                                    tool_call_id: None,
                                    tool_calls: Some(tool_calls),
                                };
                            }
                        }
                    }
                }

                if m.role == "tool" {
                    if let Ok(val) = serde_json::from_str::<serde_json::Value>(&m.content) {
                        let tool_call_id = val
                            .get("tool_call_id")
                            .and_then(serde_json::Value::as_str)
                            .map(ToString::to_string);
                        let content = val
                            .get("content")
                            .and_then(serde_json::Value::as_str)
                            .map(ToString::to_string)
                            .or_else(|| Some(m.content.clone()));
                        return Message {
                            role: "tool".to_string(),
                            content,
                            tool_call_id,
                            tool_calls: None,
                        };
                    }
                }

                Message {
                    role: m.role.clone(),
                    content: Some(m.content.clone()),
                    tool_call_id: None,
                    tool_calls: None,
                }
            })
            .collect()
    }

    fn parse_response(msg: ResponseMessage, usage: Option<UsageInfo>) -> ProviderChatResponse {
        let tool_calls = msg
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .map(|tc| ProviderToolCall {
                id: tc.id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                name: tc.function.name,
                arguments: tc.function.arguments,
            })
            .collect::<Vec<_>>();

        ProviderChatResponse {
            text: msg.content,
            tool_calls,
            usage: usage.map(|u| TokenUsage {
                input_tokens: u.prompt_tokens,
                output_tokens: u.completion_tokens,
            }),
            reasoning_content: msg.reasoning_content,
        }
    }

    fn endpoint() -> String {
        format!("{SILICONFLOW_BASE_URL}/chat/completions")
    }
}

#[async_trait]
impl Provider for SiliconFlowProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            native_tool_calling: true,
            vision: false,
        }
    }

    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let api_key = self.api_key()?;

        let mut messages = Vec::new();
        if let Some(sys) = system_prompt {
            messages.push(Message {
                role: "system".to_string(),
                content: Some(sys.to_string()),
                tool_call_id: None,
                tool_calls: None,
            });
        }
        messages.push(Message {
            role: "user".to_string(),
            content: Some(message.to_string()),
            tool_call_id: None,
            tool_calls: None,
        });

        let request = ChatRequest {
            model: model.to_string(),
            messages,
            temperature,
            stream: Some(false),
            tools: None,
            tool_choice: None,
        };

        let response = self
            .http_client()
            .post(Self::endpoint())
            .header("Authorization", format!("Bearer {api_key}"))
            .json(&request)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(super::api_error("SiliconFlow", response).await);
        }

        let api_resp: ApiResponse = response.json().await?;
        api_resp
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .ok_or_else(|| anyhow::anyhow!("SiliconFlow 未返回有效响应内容"))
    }

    async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let api_key = self.api_key()?;

        let request = ChatRequest {
            model: model.to_string(),
            messages: Self::convert_messages(messages),
            temperature,
            stream: Some(false),
            tools: None,
            tool_choice: None,
        };

        let response = self
            .http_client()
            .post(Self::endpoint())
            .header("Authorization", format!("Bearer {api_key}"))
            .json(&request)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(super::api_error("SiliconFlow", response).await);
        }

        let api_resp: ApiResponse = response.json().await?;
        api_resp
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .ok_or_else(|| anyhow::anyhow!("SiliconFlow 未返回有效响应内容"))
    }

    async fn chat(
        &self,
        request: ProviderChatRequest<'_>,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ProviderChatResponse> {
        let api_key = self.api_key()?;

        let tools = Self::convert_tools(request.tools);
        let api_request = ChatRequest {
            model: model.to_string(),
            messages: Self::convert_messages(request.messages),
            temperature,
            stream: Some(false),
            tool_choice: tools.as_ref().map(|_| "auto".to_string()),
            tools,
        };

        let response = self
            .http_client()
            .post(Self::endpoint())
            .header("Authorization", format!("Bearer {api_key}"))
            .json(&api_request)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(super::api_error("SiliconFlow", response).await);
        }

        let api_resp: ApiResponse = response.json().await?;
        let usage = api_resp.usage;
        let msg = api_resp
            .choices
            .into_iter()
            .next()
            .map(|c| c.message)
            .ok_or_else(|| anyhow::anyhow!("SiliconFlow 未返回有效 choices"))?;

        Ok(Self::parse_response(msg, usage))
    }

    fn supports_native_tools(&self) -> bool {
        true
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    /// 单轮流式对话：实时逐字输出模型响应。
    fn stream_chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
        options: StreamOptions,
    ) -> stream::BoxStream<'static, StreamResult<StreamChunk>> {
        // 鉴权失败时直接返回错误流
        let api_key = match self.api_key() {
            Ok(k) => k.to_string(),
            Err(e) => {
                return stream::once(async move { Err(StreamError::Provider(e.to_string())) })
                    .boxed();
            }
        };

        let mut messages = Vec::new();
        if let Some(sys) = system_prompt {
            messages.push(Message {
                role: "system".to_string(),
                content: Some(sys.to_string()),
                tool_call_id: None,
                tool_calls: None,
            });
        }
        messages.push(Message {
            role: "user".to_string(),
            content: Some(message.to_string()),
            tool_call_id: None,
            tool_calls: None,
        });

        let request = ChatRequest {
            model: model.to_string(),
            messages,
            temperature,
            stream: Some(true),
            tools: None,
            tool_choice: None,
        };

        let client = self.http_client();
        let count_tokens = options.count_tokens;
        let endpoint = Self::endpoint();

        let (tx, rx) = tokio::sync::mpsc::channel::<StreamResult<StreamChunk>>(128);

        tokio::spawn(async move {
            let response = client
                .post(&endpoint)
                .header("Authorization", format!("Bearer {api_key}"))
                .header("Accept", "text/event-stream")
                .json(&request)
                .send()
                .await;

            match response {
                Ok(resp) => {
                    if !resp.status().is_success() {
                        let status = resp.status();
                        let body = resp
                            .text()
                            .await
                            .unwrap_or_else(|_| format!("HTTP {status}"));
                        let _ = tx
                            .send(Err(StreamError::Provider(format!(
                                "SiliconFlow 流式请求失败 ({status}): {body}"
                            ))))
                            .await;
                        return;
                    }
                    let mut stream = sse_response_to_stream(resp, count_tokens);
                    while let Some(chunk) = stream.next().await {
                        if tx.send(chunk).await.is_err() {
                            break;
                        }
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(StreamError::Http(e))).await;
                }
            }
        });

        stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|chunk| (chunk, rx))
        })
        .boxed()
    }

    /// 多轮历史流式对话。
    fn stream_chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: f64,
        options: StreamOptions,
    ) -> stream::BoxStream<'static, StreamResult<StreamChunk>> {
        let api_key = match self.api_key() {
            Ok(k) => k.to_string(),
            Err(e) => {
                return stream::once(async move { Err(StreamError::Provider(e.to_string())) })
                    .boxed();
            }
        };

        let request = ChatRequest {
            model: model.to_string(),
            messages: Self::convert_messages(messages),
            temperature,
            stream: Some(true),
            tools: None,
            tool_choice: None,
        };

        let client = self.http_client();
        let count_tokens = options.count_tokens;
        let endpoint = Self::endpoint();

        let (tx, rx) = tokio::sync::mpsc::channel::<StreamResult<StreamChunk>>(128);

        tokio::spawn(async move {
            let response = client
                .post(&endpoint)
                .header("Authorization", format!("Bearer {api_key}"))
                .header("Accept", "text/event-stream")
                .json(&request)
                .send()
                .await;

            match response {
                Ok(resp) => {
                    if !resp.status().is_success() {
                        let status = resp.status();
                        let body = resp
                            .text()
                            .await
                            .unwrap_or_else(|_| format!("HTTP {status}"));
                        let _ = tx
                            .send(Err(StreamError::Provider(format!(
                                "SiliconFlow 流式请求失败 ({status}): {body}"
                            ))))
                            .await;
                        return;
                    }
                    let mut stream = sse_response_to_stream(resp, count_tokens);
                    while let Some(chunk) = stream.next().await {
                        if tx.send(chunk).await.is_err() {
                            break;
                        }
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(StreamError::Http(e))).await;
                }
            }
        });

        stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|chunk| (chunk, rx))
        })
        .boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::traits::Provider;

    #[test]
    fn 创建带密钥的provider() {
        let p = SiliconFlowProvider::new(Some("sf-test-key"));
        assert_eq!(p.api_key.as_deref(), Some("sf-test-key"));
    }

    #[test]
    fn 创建无密钥的provider() {
        let p = SiliconFlowProvider::new(None);
        assert!(p.api_key.is_none());
    }

    #[test]
    fn capabilities_声明原生工具调用() {
        let p = SiliconFlowProvider::new(Some("sf-test-key"));
        let caps = <SiliconFlowProvider as Provider>::capabilities(&p);
        assert!(caps.native_tool_calling, "应声明支持原生 Function Calling");
        assert!(!caps.vision, "文本 Provider 不应声明 Vision 支持");
    }

    #[test]
    fn 声明支持流式输出() {
        let p = SiliconFlowProvider::new(Some("sf-test-key"));
        assert!(
            <SiliconFlowProvider as Provider>::supports_streaming(&p),
            "应声明支持流式输出"
        );
    }

    #[tokio::test]
    async fn 无密钥时chat_with_system返回错误() {
        let p = SiliconFlowProvider::new(None);
        let result = p
            .chat_with_system(None, "你好", "Pro/zai-org/GLM-4.7", 0.7)
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("API Key 未设置"));
    }

    #[tokio::test]
    async fn 无密钥时chat_with_history返回错误() {
        let p = SiliconFlowProvider::new(None);
        let msgs = vec![ChatMessage::system("你是助手"), ChatMessage::user("你好")];
        let result = p.chat_with_history(&msgs, "Pro/zai-org/GLM-4.7", 0.7).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn 无密钥时stream_chat_with_system返回错误流() {
        use futures_util::StreamExt;
        let p = SiliconFlowProvider::new(None);
        let mut stream = p.stream_chat_with_system(
            None,
            "你好",
            "zai-org/GLM-4.6",
            0.7,
            StreamOptions::new(true),
        );
        let first = stream.next().await;
        assert!(first.is_some());
        assert!(first.unwrap().is_err(), "无密钥时应返回错误 chunk");
    }

    #[test]
    fn 消息转换_普通文本消息() {
        let msgs = vec![ChatMessage::system("你是助手"), ChatMessage::user("你好")];
        let converted = SiliconFlowProvider::convert_messages(&msgs);
        assert_eq!(converted[0].role, "system");
        assert_eq!(converted[0].content.as_deref(), Some("你是助手"));
        assert_eq!(converted[1].role, "user");
        assert_eq!(converted[1].content.as_deref(), Some("你好"));
    }

    #[test]
    fn 响应解析_含reasoning_content() {
        let msg = ResponseMessage {
            content: Some("最终答案".to_string()),
            reasoning_content: Some("思维过程...".to_string()),
            tool_calls: None,
        };
        let resp = SiliconFlowProvider::parse_response(msg, None);
        assert_eq!(resp.text.as_deref(), Some("最终答案"));
        assert_eq!(resp.reasoning_content.as_deref(), Some("思维过程..."));
        assert!(resp.tool_calls.is_empty());
    }

    #[test]
    fn 响应解析_含工具调用() {
        let msg = ResponseMessage {
            content: None,
            reasoning_content: None,
            tool_calls: Some(vec![ToolCall {
                id: Some("call_001".to_string()),
                kind: Some("function".to_string()),
                function: FunctionCall {
                    name: "shell".to_string(),
                    arguments: r#"{"command":"pwd"}"#.to_string(),
                },
            }]),
        };
        let resp = SiliconFlowProvider::parse_response(msg, None);
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].name, "shell");
        assert_eq!(resp.tool_calls[0].id, "call_001");
    }

    #[test]
    fn sse解析_普通文本行() {
        let line = r#"data: {"choices":[{"delta":{"content":"你好"}}]}"#;
        let result = parse_sse_line(line).unwrap();
        assert_eq!(result, Some("你好".to_string()));
    }

    #[test]
    fn sse解析_done信号() {
        let result = parse_sse_line("data: [DONE]").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn sse解析_空行() {
        let result = parse_sse_line("").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn sse解析_reasoning_content回退() {
        let line = r#"data: {"choices":[{"delta":{"reasoning_content":"思考中..."}}]}"#;
        let result = parse_sse_line(line).unwrap();
        assert_eq!(result, Some("思考中...".to_string()));
    }

    #[test]
    fn 工具规格转换() {
        let specs = vec![ToolSpec {
            name: "shell".to_string(),
            description: "执行 Shell 命令".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        }];
        let converted = SiliconFlowProvider::convert_tools(Some(&specs)).unwrap();
        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0].function.name, "shell");
    }
}
