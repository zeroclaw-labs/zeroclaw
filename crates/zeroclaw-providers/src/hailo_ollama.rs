use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::Semaphore;
use zeroclaw_api::attribution::{Attributable, ModelProviderKind, ProviderKind, Role};
use zeroclaw_config::schema::{HailoOllamaEndpoint, ModelEndpoint};

use crate::multimodal;
use crate::ollama::OllamaTuning;
use crate::ollama_wire::{
    ApiChatResponse, ChatRequest as WireChatRequest, Message, Options, OutgoingFunction,
    OutgoingToolCall,
};
use crate::traits::{
    ChatMessage, ChatRequest, ChatResponse, ModelProvider, NonRetryableProviderError,
    ProviderCapabilities, TokenUsage, ToolCall, ToolsPayload,
};

pub const HAILO_DEFAULT_NUM_CTX: u32 = 2048;
pub const HAILO_DEFAULT_NUM_PREDICT: i32 = 256;
pub const HAILO_DEFAULT_QUEUE_TIMEOUT_SECS: u64 = 30;
const HAILO_MAX_HISTORY_MESSAGES: usize = 12;
const HAILO_MAX_MESSAGE_CHARS: usize = 2_000;
const TEMPERATURE_DEFAULT: f64 = 0.8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuarantineReason {
    AmbiguousTimeout,
    AmbiguousTransport,
    WorkerTerminated,
}

impl QuarantineReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::AmbiguousTimeout => "ambiguous_timeout",
            Self::AmbiguousTransport => "ambiguous_transport",
            Self::WorkerTerminated => "worker_terminated",
        }
    }
}

struct GenerationGate {
    semaphore: Arc<Semaphore>,
    quarantine_reason: OnceLock<QuarantineReason>,
}

impl GenerationGate {
    fn quarantine(&self, reason: QuarantineReason) {
        if self.quarantine_reason.set(reason).is_err() {
            return;
        }

        ::zeroclaw_log::record!(
            ERROR,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "error_key": "hailo_endpoint_quarantined",
                    "reason": reason.as_str(),
                })),
            "Hailo-Ollama quarantined an endpoint after an ambiguous in-flight failure"
        );
    }
}

struct InFlightGuard {
    gate: Arc<GenerationGate>,
    armed: bool,
}

impl InFlightGuard {
    fn new(gate: Arc<GenerationGate>) -> Self {
        Self { gate, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }

    fn quarantine(&mut self, reason: QuarantineReason) {
        self.gate.quarantine(reason);
        self.disarm();
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        if self.armed {
            self.gate.quarantine(QuarantineReason::WorkerTerminated);
        }
    }
}

fn canonical_endpoint(endpoint: &str) -> String {
    let Ok(mut url) = reqwest::Url::parse(endpoint.trim()) else {
        return endpoint.trim().trim_end_matches('/').to_string();
    };

    let is_loopback_alias = url.host_str().is_some_and(|host| {
        matches!(
            host,
            "localhost" | "127.0.0.1" | "::1" | "[::1]" | "0.0.0.0"
        )
    });
    if is_loopback_alias {
        let _ = url.set_host(Some("localhost"));
    }

    let path = url.path().trim_end_matches('/');
    let base_path = path
        .strip_suffix("/api/chat")
        .or_else(|| path.strip_suffix("/api"))
        .unwrap_or(path)
        .to_string();
    url.set_path(if base_path.is_empty() {
        "/"
    } else {
        &base_path
    });
    url.to_string().trim_end_matches('/').to_string()
}

fn shared_generation_gate(endpoint: &str) -> Arc<GenerationGate> {
    type GateRegistry = Mutex<HashMap<String, Arc<GenerationGate>>>;
    static GATES: OnceLock<GateRegistry> = OnceLock::new();

    let gates = GATES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut gates = gates
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    gates
        .entry(canonical_endpoint(endpoint))
        .or_insert_with(|| {
            Arc::new(GenerationGate {
                semaphore: Arc::new(Semaphore::new(1)),
                quarantine_reason: OnceLock::new(),
            })
        })
        .clone()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MessageKind {
    User,
    Assistant,
    ToolResult,
}

struct MessageCandidate {
    kind: MessageKind,
    content: String,
}

/// Native Hailo-Ollama provider.
///
/// The provider owns Hailo-specific URL validation, transport policy, message
/// bounding, and endpoint-scoped generation lifecycle. Only stateless wire
/// DTOs are shared with the ordinary Ollama provider.
#[derive(Clone)]
pub struct HailoOllamaModelProvider {
    alias: String,
    base_url: String,
    request_timeout_secs: u64,
    queue_timeout: Duration,
    tuning: OllamaTuning,
    generation_gate: Arc<GenerationGate>,
}

impl HailoOllamaModelProvider {
    pub fn new(
        alias: &str,
        base_url: Option<&str>,
        request_timeout_secs: u64,
        queue_timeout_secs: u64,
        tuning: OllamaTuning,
    ) -> anyhow::Result<Self> {
        let raw_base_url = base_url
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| HailoOllamaEndpoint::default().uri());
        let base_url = Self::normalize_base_url(raw_base_url)?;
        let generation_gate = shared_generation_gate(&base_url);

        Ok(Self {
            alias: alias.to_string(),
            base_url,
            request_timeout_secs: request_timeout_secs.max(1),
            queue_timeout: Duration::from_secs(queue_timeout_secs.max(1)),
            tuning,
            generation_gate,
        })
    }

    fn invalid_url(reason: &'static str, message: &'static str) -> anyhow::Error {
        ::zeroclaw_log::record!(
            ERROR,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "error_key": "hailo_url_invalid",
                    "reason": reason,
                })),
            "Hailo-Ollama URL validation failed"
        );
        anyhow::Error::msg(message)
    }

    fn normalize_base_url(raw_url: &str) -> anyhow::Result<String> {
        let mut url = reqwest::Url::parse(raw_url.trim())
            .map_err(|_| Self::invalid_url("parse", "Hailo-Ollama URL is invalid"))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(Self::invalid_url(
                "scheme",
                "Hailo-Ollama URL must use http or https",
            ));
        }
        if url.host_str().is_none() {
            return Err(Self::invalid_url(
                "host",
                "Hailo-Ollama URL must include a host",
            ));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(Self::invalid_url(
                "credentials",
                "Hailo-Ollama URL must not contain credentials",
            ));
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err(Self::invalid_url(
                "query_or_fragment",
                "Hailo-Ollama URL must not contain a query or fragment",
            ));
        }

        let path = url.path().trim_end_matches('/');
        let base_path = path
            .strip_suffix("/api/chat")
            .or_else(|| path.strip_suffix("/api"))
            .unwrap_or(path)
            .to_string();
        url.set_path(if base_path.is_empty() {
            "/"
        } else {
            &base_path
        });
        Ok(url.as_str().trim_end_matches('/').to_string())
    }

    fn http_client(&self) -> anyhow::Result<Client> {
        zeroclaw_config::schema::try_build_runtime_proxy_client_with_timeouts(
            "model_provider.hailo_ollama",
            self.request_timeout_secs,
            10,
        )
    }

    fn ensure_gate_ready(&self, gate: &GenerationGate) -> anyhow::Result<()> {
        let Some(reason) = gate.quarantine_reason.get().copied() else {
            return Ok(());
        };

        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "error_key": "hailo_endpoint_quarantined",
                    "model_provider": self.alias,
                    "reason": reason.as_str(),
                })),
            "Hailo-Ollama rejected a request because the endpoint is quarantined"
        );
        let detail = match reason {
            QuarantineReason::AmbiguousTimeout => "an ambiguous request timeout",
            QuarantineReason::AmbiguousTransport => "an ambiguous post-connect transport failure",
            QuarantineReason::WorkerTerminated => "an in-flight request worker failure",
        };
        Err(anyhow::Error::new(NonRetryableProviderError::new(format!(
            "Hailo-Ollama provider is quarantined after {detail}; restart ZeroClaw after \
             confirming the backend is idle"
        ))))
    }

    fn quarantine_reason(error: &anyhow::Error) -> Option<QuarantineReason> {
        error.chain().find_map(|source| {
            let error = source.downcast_ref::<reqwest::Error>()?;
            if error.is_connect() || error.is_builder() {
                None
            } else if error.is_timeout() {
                Some(QuarantineReason::AmbiguousTimeout)
            } else {
                Some(QuarantineReason::AmbiguousTransport)
            }
        })
    }

    fn redact_transport_error(&self, error: anyhow::Error) -> anyhow::Error {
        let Some(request_error) = error
            .chain()
            .find_map(|source| source.downcast_ref::<reqwest::Error>())
        else {
            return error;
        };

        let (transport_kind, message) = if request_error.is_connect() {
            ("connect", "Hailo-Ollama connection failed".to_string())
        } else if request_error.is_timeout() {
            ("timeout", "Hailo-Ollama request timed out".to_string())
        } else if request_error.is_builder() {
            (
                "builder",
                "Hailo-Ollama request could not be built".to_string(),
            )
        } else if let Some(status) = request_error.status() {
            (
                "http_status",
                format!("Hailo-Ollama API request failed with HTTP {status}"),
            )
        } else if request_error.is_decode() {
            (
                "decode",
                "Hailo-Ollama response decoding failed".to_string(),
            )
        } else if request_error.is_body() {
            (
                "body",
                "Hailo-Ollama response body transfer failed".to_string(),
            )
        } else {
            ("other", "Hailo-Ollama transport request failed".to_string())
        };

        ::zeroclaw_log::record!(
            ERROR,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "error_key": "hailo_transport_error",
                    "model_provider": self.alias,
                    "transport_kind": transport_kind,
                    "status": request_error.status().map(|status| status.as_u16()),
                })),
            "Hailo-Ollama transport request failed"
        );

        anyhow::Error::msg(message)
    }
}

/// Observed with Hailo-Ollama 0.5.1: literal non-ASCII UTF-8 could wedge a
/// request while semantically equivalent ASCII-escaped JSON completed.
struct AsciiJsonFormatter;

impl serde_json::ser::Formatter for AsciiJsonFormatter {
    fn write_string_fragment<W>(&mut self, writer: &mut W, fragment: &str) -> std::io::Result<()>
    where
        W: ?Sized + std::io::Write,
    {
        const HEX: &[u8; 16] = b"0123456789abcdef";

        let fragment_bytes = fragment.as_bytes();
        let mut ascii_start = 0;
        for (index, character) in fragment.char_indices() {
            if character.is_ascii() {
                continue;
            }

            std::io::Write::write_all(writer, &fragment_bytes[ascii_start..index])?;
            let mut utf16 = [0_u16; 2];
            for code_unit in character.encode_utf16(&mut utf16) {
                let escaped = [
                    b'\\',
                    b'u',
                    HEX[(((*code_unit) >> 12) & 0x0f) as usize],
                    HEX[(((*code_unit) >> 8) & 0x0f) as usize],
                    HEX[(((*code_unit) >> 4) & 0x0f) as usize],
                    HEX[((*code_unit) & 0x0f) as usize],
                ];
                std::io::Write::write_all(writer, &escaped)?;
            }
            ascii_start = index + character.len_utf8();
        }

        std::io::Write::write_all(writer, &fragment_bytes[ascii_start..])
    }
}

fn serialize_request(request: &WireChatRequest) -> anyhow::Result<Vec<u8>> {
    let mut body = Vec::new();
    let mut serializer = serde_json::Serializer::with_formatter(&mut body, AsciiJsonFormatter);
    request.serialize(&mut serializer).map_err(|error| {
        anyhow::Error::msg(format!("Failed to serialize Hailo-Ollama request: {error}"))
    })?;
    Ok(body)
}

impl HailoOllamaModelProvider {
    fn parse_tool_arguments(arguments: &str) -> serde_json::Value {
        serde_json::from_str(arguments).unwrap_or_else(|_| serde_json::json!({}))
    }

    fn convert_user_message_content(content: &str) -> (Option<String>, Option<Vec<String>>) {
        let (cleaned, image_refs) = multimodal::parse_image_markers(content);
        if image_refs.is_empty() {
            return (Some(content.to_string()), None);
        }

        let images: Vec<String> = image_refs
            .iter()
            .filter_map(|reference| multimodal::extract_ollama_image_payload(reference))
            .collect();
        if images.is_empty() {
            return (Some(content.to_string()), None);
        }

        let cleaned = cleaned.trim();
        let content = (!cleaned.is_empty()).then(|| cleaned.to_string());
        (content, Some(images))
    }

    fn convert_messages(&self, messages: &[ChatMessage]) -> Vec<Message> {
        let mut tool_name_by_id: HashMap<String, String> = HashMap::new();

        messages
            .iter()
            .map(|message| {
                if message.role == "assistant"
                    && let Ok(value) = serde_json::from_str::<serde_json::Value>(&message.content)
                    && let Some(tool_calls_value) = value.get("tool_calls")
                    && let Ok(parsed_calls) =
                        serde_json::from_value::<Vec<ToolCall>>(tool_calls_value.clone())
                {
                    let outgoing_calls = parsed_calls
                        .into_iter()
                        .map(|call| {
                            tool_name_by_id.insert(call.id.clone(), call.name.clone());
                            OutgoingToolCall {
                                kind: "function".to_string(),
                                function: OutgoingFunction {
                                    name: call.name,
                                    arguments: Self::parse_tool_arguments(&call.arguments),
                                },
                            }
                        })
                        .collect();
                    let content = value
                        .get("content")
                        .and_then(serde_json::Value::as_str)
                        .map(ToString::to_string);
                    return Message {
                        role: "assistant".to_string(),
                        content,
                        images: None,
                        tool_calls: Some(outgoing_calls),
                        tool_name: None,
                    };
                }

                if message.role == "tool"
                    && let Ok(value) = serde_json::from_str::<serde_json::Value>(&message.content)
                {
                    let tool_name = value
                        .get("tool_name")
                        .and_then(serde_json::Value::as_str)
                        .map(ToString::to_string)
                        .or_else(|| {
                            value
                                .get("tool_call_id")
                                .and_then(serde_json::Value::as_str)
                                .and_then(|id| tool_name_by_id.get(id))
                                .cloned()
                        });
                    let content = value
                        .get("content")
                        .and_then(serde_json::Value::as_str)
                        .map(ToString::to_string)
                        .or_else(|| {
                            (!message.content.trim().is_empty()).then(|| message.content.clone())
                        });
                    return Message {
                        role: "tool".to_string(),
                        content,
                        images: None,
                        tool_calls: None,
                        tool_name,
                    };
                }

                if message.role == "user" {
                    let (content, images) = Self::convert_user_message_content(&message.content);
                    return Message {
                        role: "user".to_string(),
                        content,
                        images,
                        tool_calls: None,
                        tool_name: None,
                    };
                }

                Message {
                    role: message.role.clone(),
                    content: Some(message.content.clone()),
                    images: None,
                    tool_calls: None,
                    tool_name: None,
                }
            })
            .collect()
    }

    fn normalize_content(content: &str) -> String {
        content
            .chars()
            .filter(|character| !character.is_control() || matches!(character, '\r' | '\n' | '\t'))
            .collect()
    }

    /// Hailo-Ollama 0.5.1 parses the decoded `content` string as JSON again
    /// while rendering the prompt. Keep layout controls semantically intact
    /// for that second parse by escaping them once more in the API value.
    fn encode_layout_controls_for_native(content: &str) -> String {
        let mut encoded = String::with_capacity(content.len());
        for character in content.chars() {
            match character {
                '\r' => encoded.push_str(r"\r"),
                '\n' => encoded.push_str(r"\n"),
                '\t' => encoded.push_str(r"\t"),
                _ => encoded.push(character),
            }
        }
        encoded
    }

    fn truncate_content(content: &str, max_chars: usize) -> String {
        let char_count = content.chars().count();
        if char_count <= max_chars {
            return content.to_string();
        }
        if max_chars <= 3 {
            return content.chars().take(max_chars).collect();
        }

        let head_chars = (max_chars - 3) / 2;
        let tail_chars = max_chars - 3 - head_chars;
        let head: String = content.chars().take(head_chars).collect();
        let tail_reversed: String = content.chars().rev().take(tail_chars).collect();
        let tail: String = tail_reversed.chars().rev().collect();
        format!("{head}...{tail}")
    }

    fn fold_system(&self, system: &str, user: &str) -> anyhow::Result<String> {
        const INSTRUCTIONS_PREFIX: &str = "Instructions: ";
        const REQUEST_PREFIX: &str = " Request: ";

        let overhead = INSTRUCTIONS_PREFIX.chars().count() + REQUEST_PREFIX.chars().count();
        let available = HAILO_MAX_MESSAGE_CHARS.saturating_sub(overhead);
        let system_chars = system.chars().count();
        let user_chars = user.chars().count();
        let reserved_system = system_chars.min(available / 3);
        let user_budget = user_chars.min(available.saturating_sub(reserved_system));
        let system_budget = system_chars.min(available.saturating_sub(user_budget));
        let system_truncated = system_chars > system_budget;
        let user_truncated = user_chars > user_budget;

        if system_truncated && system.contains("## Tool Use Protocol") {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "error_key": "hailo_tool_prompt_too_large",
                        "model_provider": self.alias,
                        "system_chars": system_chars,
                        "system_budget": system_budget,
                        "user_chars": user_chars,
                    })),
                "Hailo-Ollama rejected an oversized prompt-guided tool prompt"
            );
            anyhow::bail!(
                "Hailo-Ollama prompt-guided tool instructions exceed the bounded system prompt; reduce enabled tools or compact the agent context"
            );
        }

        if system_truncated || user_truncated {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "error_key": "hailo_prompt_bounded",
                        "model_provider": self.alias,
                        "system_chars": system_chars,
                        "system_budget": system_budget,
                        "user_chars": user_chars,
                        "user_budget": user_budget,
                    })),
                "Hailo-Ollama bounded an oversized system/user prompt"
            );
        }

        let system = Self::truncate_content(system, system_budget);
        let user = Self::truncate_content(user, user_budget);
        Ok(format!(
            "{INSTRUCTIONS_PREFIX}{system}{REQUEST_PREFIX}{user}"
        ))
    }

    fn push_message(&self, messages: &mut Vec<Message>, role: String, content: String) {
        if content.is_empty() {
            return;
        }
        if let Some(previous) = messages.last_mut()
            && previous.role == role
        {
            let previous_content = previous.content.take().unwrap_or_default();
            let merged_chars = previous_content.chars().count() + 1 + content.chars().count();
            if merged_chars > HAILO_MAX_MESSAGE_CHARS {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({
                            "error_key": "hailo_message_bounded",
                            "model_provider": self.alias,
                            "role": role.as_str(),
                            "message_chars": merged_chars,
                            "max_message_chars": HAILO_MAX_MESSAGE_CHARS,
                        })),
                    "Hailo-Ollama bounded a merged history message"
                );
            }
            previous.content = Some(Self::truncate_content(
                &format!("{previous_content} {content}"),
                HAILO_MAX_MESSAGE_CHARS,
            ));
            return;
        }

        let message_chars = content.chars().count();
        if message_chars > HAILO_MAX_MESSAGE_CHARS {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "error_key": "hailo_message_bounded",
                        "model_provider": self.alias,
                        "role": role.as_str(),
                        "message_chars": message_chars,
                        "max_message_chars": HAILO_MAX_MESSAGE_CHARS,
                    })),
                "Hailo-Ollama bounded a history message"
            );
        }

        messages.push(Message {
            role,
            content: Some(Self::truncate_content(&content, HAILO_MAX_MESSAGE_CHARS)),
            images: None,
            tool_calls: None,
            tool_name: None,
        });
    }

    fn normalize_messages(&self, messages: Vec<Message>) -> anyhow::Result<Vec<Message>> {
        let mut system_parts = Vec::new();
        let mut candidates = Vec::new();

        for message in messages {
            if message
                .images
                .as_ref()
                .is_some_and(|images| !images.is_empty())
            {
                anyhow::bail!("Hailo-Ollama does not support image inputs");
            }
            let mut content = Self::normalize_content(message.content.as_deref().unwrap_or(""));
            if message.role == "system" {
                if !content.is_empty() {
                    system_parts.push(Self::encode_layout_controls_for_native(&content));
                }
                continue;
            }

            if message.role == "assistant"
                && let Some(tool_calls) = message
                    .tool_calls
                    .as_ref()
                    .filter(|tool_calls| !tool_calls.is_empty())
            {
                let rendered = serde_json::to_string(tool_calls).map_err(|error| {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "error_key": "hailo_tool_history_serialize",
                                "model_provider": self.alias,
                                "error": error.to_string(),
                            })),
                        "Hailo-Ollama failed to render tool history"
                    );
                    anyhow::Error::msg(format!("failed to render Hailo tool history: {error}"))
                })?;
                content = if content.is_empty() {
                    format!("Tool calls: {rendered}")
                } else {
                    format!("{content} Tool calls: {rendered}")
                };
            }

            let content = Self::encode_layout_controls_for_native(&content);
            let (kind, content) = match message.role.as_str() {
                "user" => (MessageKind::User, content),
                "assistant" => (MessageKind::Assistant, content),
                "tool" => {
                    let source = message
                        .tool_name
                        .as_deref()
                        .map_or_else(String::new, |name| format!(" from {name}"));
                    (
                        MessageKind::ToolResult,
                        format!("Tool result{source}: {content}"),
                    )
                }
                _ => continue,
            };
            if !content.is_empty() {
                candidates.push(MessageCandidate { kind, content });
            }
        }

        if candidates.len() > HAILO_MAX_HISTORY_MESSAGES {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "error_key": "hailo_history_bounded",
                        "model_provider": self.alias,
                        "history_messages": candidates.len(),
                        "retained_messages": HAILO_MAX_HISTORY_MESSAGES,
                    })),
                "Hailo-Ollama bounded chat history"
            );
            candidates = candidates.split_off(candidates.len() - HAILO_MAX_HISTORY_MESSAGES);
        }

        let first_user = candidates
            .iter()
            .position(|candidate| candidate.kind == MessageKind::User)
            .unwrap_or(candidates.len());
        if first_user > 0 {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "error_key": "hailo_orphan_history_dropped",
                        "model_provider": self.alias,
                        "dropped_messages": first_user,
                    })),
                "Hailo-Ollama dropped orphaned leading history"
            );
            candidates.drain(..first_user);
        }

        let mut converted = Vec::new();
        for candidate in candidates {
            let role = match candidate.kind {
                MessageKind::User | MessageKind::ToolResult => "user",
                MessageKind::Assistant => "assistant",
            };
            self.push_message(&mut converted, role.to_string(), candidate.content);
        }

        let system = system_parts.join(" ");
        if converted.is_empty() {
            let content = if system.is_empty() {
                "hello".to_string()
            } else {
                self.fold_system(&system, "hello")?
            };
            self.push_message(&mut converted, "user".to_string(), content);
        } else if !system.is_empty() {
            let first_user = &mut converted[0];
            let user = first_user.content.take().unwrap_or_default();
            first_user.content = Some(self.fold_system(&system, &user)?);
        }

        Ok(converted)
    }

    fn with_prompt_guided_tool_instructions(
        &self,
        messages: &[ChatMessage],
        tools: Option<&[zeroclaw_api::tool::ToolSpec]>,
    ) -> anyhow::Result<Vec<ChatMessage>> {
        let Some(tools) = tools.filter(|items| !items.is_empty()) else {
            return Ok(messages.to_vec());
        };

        let ToolsPayload::PromptGuided { instructions } = self.convert_tools(tools) else {
            anyhow::bail!(
                "Hailo-Ollama returned a native tools payload while native tools are disabled"
            );
        };
        let mut modified_messages = messages.to_vec();
        if let Some(system_message) = modified_messages
            .iter_mut()
            .find(|message| message.role == "system")
        {
            if !system_message.content.is_empty() {
                system_message.content.push_str("\n\n");
            }
            system_message.content.push_str(&instructions);
        } else {
            modified_messages.insert(0, ChatMessage::system(instructions));
        }
        Ok(modified_messages)
    }

    fn build_request(
        &self,
        messages: Vec<Message>,
        model: &str,
        temperature: Option<f64>,
    ) -> WireChatRequest {
        WireChatRequest {
            model: model.to_string(),
            messages,
            stream: false,
            options: Options {
                temperature: self.tuning.temperature_override.or(temperature),
                num_ctx: Some(self.tuning.num_ctx),
                num_predict: Some(self.tuning.num_predict),
            },
            think: Some(false),
            tools: None,
        }
    }

    async fn send_request_inner(
        &self,
        messages: &[Message],
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<ApiChatResponse> {
        let request = self.build_request(messages.to_vec(), model, temperature);
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
                ::serde_json::json!({
                    "model_provider": self.alias,
                    "model": model,
                    "message_count": request.messages.len(),
                    "temperature": temperature,
                })
            ),
            "Hailo-Ollama request"
        );

        let url = format!("{}/api/chat", self.base_url);
        let response = self
            .http_client()?
            .post(url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::CONNECTION, "close")
            .body(serialize_request(&request)?)
            .send()
            .await?;
        let status = response.status();
        let body = response.bytes().await?;

        if !status.is_success() {
            let raw = String::from_utf8_lossy(&body);
            let sanitized = super::sanitize_api_error(&raw);
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "error_key": "hailo_api_error",
                        "model_provider": self.alias,
                        "status": status.as_u16(),
                    })),
                &format!(
                    "Hailo-Ollama error response: status={} body_excerpt={}",
                    status, sanitized
                )
            );
            anyhow::bail!(
                "Hailo-Ollama API error ({}): {}. Check that Hailo-Ollama is running and the \
                 model is loaded",
                status,
                sanitized
            );
        }

        serde_json::from_slice(&body).map_err(|error| {
            let raw = String::from_utf8_lossy(&body);
            let sanitized = super::sanitize_api_error(&raw);
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "error_key": "hailo_response_deserialize",
                        "model_provider": self.alias,
                        "error": error.to_string(),
                    })),
                &format!(
                    "Hailo-Ollama response deserialization failed: {error}. body_excerpt={}",
                    sanitized
                )
            );
            anyhow::Error::msg(format!("Failed to parse Hailo-Ollama response: {error}"))
        })
    }

    async fn send_request(
        &self,
        messages: Vec<Message>,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<ApiChatResponse> {
        let messages = self.normalize_messages(messages)?;
        let gate = self.generation_gate.clone();

        self.ensure_gate_ready(&gate)?;
        let permit =
            tokio::time::timeout(self.queue_timeout, gate.semaphore.clone().acquire_owned())
                .await
                .map_err(|_| {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "error_key": "hailo_queue_timeout",
                                "model_provider": self.alias,
                                "queue_timeout_secs": self.queue_timeout.as_secs(),
                            })),
                        "Hailo-Ollama rejected a request after its queue wait expired"
                    );
                    anyhow::Error::msg(
                        "Hailo-Ollama queue wait timed out at its configured deadline",
                    )
                })?
                .map_err(|_| {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "error_key": "hailo_generation_semaphore_closed",
                                "model_provider": self.alias,
                            })),
                        "Hailo-Ollama generation semaphore closed"
                    );
                    anyhow::Error::msg("Hailo-Ollama generation semaphore closed")
                })?;
        self.ensure_gate_ready(&gate)?;

        let provider = self.clone();
        let model = model.to_string();
        let worker_gate = gate.clone();
        let worker = zeroclaw_spawn::spawn!(async move {
            let _permit = permit;
            let mut in_flight = InFlightGuard::new(worker_gate);
            let result = provider
                .send_request_inner(&messages, &model, temperature)
                .await;
            if let Err(error) = &result
                && let Some(reason) = Self::quarantine_reason(error)
            {
                in_flight.quarantine(reason);
            } else {
                in_flight.disarm();
            }
            result
        });

        let result = worker.await.map_err(|error| {
            gate.quarantine(QuarantineReason::WorkerTerminated);
            let worker_failure = if error.is_panic() {
                "panic"
            } else if error.is_cancelled() {
                "cancelled"
            } else {
                "unknown"
            };
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "error_key": "hailo_request_worker_failed",
                        "model_provider": self.alias,
                        "worker_failure": worker_failure,
                    })),
                "Hailo-Ollama request worker failed"
            );
            anyhow::Error::msg("Hailo-Ollama request worker failed")
        })?;
        result.map_err(|error| self.redact_transport_error(error))
    }

    fn strip_think_tags(content: &str) -> String {
        let mut result = String::new();
        let mut remaining = content;
        while let Some(start) = remaining.find("<think>") {
            result.push_str(&remaining[..start]);
            let after_start = &remaining[start + "<think>".len()..];
            let Some(end) = after_start.find("</think>") else {
                return result;
            };
            remaining = &after_start[end + "</think>".len()..];
        }
        result.push_str(remaining);
        result
    }

    fn response_text(response: &ApiChatResponse) -> String {
        let content = Self::strip_think_tags(&response.message.content);
        if !content.trim().is_empty() {
            return content;
        }
        if let Some(thinking) = response.message.thinking.as_deref() {
            let thinking = Self::strip_think_tags(thinking);
            if !thinking.trim().is_empty() {
                return thinking;
            }
        }
        "Hailo-Ollama returned an empty response. Please try again.".to_string()
    }

    fn response_to_chat_response(response: ApiChatResponse) -> ChatResponse {
        let usage = if response.prompt_eval_count.is_some() || response.eval_count.is_some() {
            Some(TokenUsage {
                input_tokens: response.prompt_eval_count,
                output_tokens: response.eval_count,
                cached_input_tokens: None,
            })
        } else {
            None
        };
        let text = Self::response_text(&response);
        ChatResponse {
            text: Some(text),
            tool_calls: Vec::new(),
            usage,
            reasoning_content: None,
        }
    }

    async fn chat_messages(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<ApiChatResponse> {
        let messages = self.convert_messages(messages);
        self.send_request(messages, model, temperature).await
    }
}

#[async_trait]
impl ModelProvider for HailoOllamaModelProvider {
    fn default_temperature(&self) -> f64 {
        TEMPERATURE_DEFAULT
    }

    fn default_timeout_secs(&self) -> u64 {
        self.request_timeout_secs
    }

    fn default_base_url(&self) -> Option<&str> {
        Some(HailoOllamaEndpoint::default().uri())
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            native_tool_calling: false,
            vision: false,
            prompt_caching: false,
            extended_thinking: false,
        }
    }

    fn supports_streaming(&self) -> bool {
        false
    }

    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        let mut messages = Vec::new();
        if let Some(system_prompt) = system_prompt {
            messages.push(ChatMessage::system(system_prompt));
        }
        messages.push(ChatMessage::user(message));
        let response = self.chat_messages(&messages, model, temperature).await?;
        Ok(Self::response_text(&response))
    }

    async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        let response = self.chat_messages(messages, model, temperature).await?;
        Ok(Self::response_text(&response))
    }

    async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<ChatResponse> {
        if !tools.is_empty() {
            anyhow::bail!("Hailo-Ollama does not support native tool calling");
        }
        let response = self.chat_messages(messages, model, temperature).await?;
        Ok(Self::response_to_chat_response(response))
    }

    fn supports_native_tools(&self) -> bool {
        false
    }

    async fn chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<ChatResponse> {
        let messages =
            self.with_prompt_guided_tool_instructions(request.messages, request.tools)?;
        let response = self.chat_messages(&messages, model, temperature).await?;
        Ok(Self::response_to_chat_response(response))
    }

    async fn list_models(&self) -> anyhow::Result<Vec<String>> {
        #[derive(Deserialize)]
        struct Catalog {
            models: Vec<CatalogEntry>,
        }

        #[derive(Deserialize)]
        struct CatalogEntry {
            name: String,
        }

        let response = self
            .http_client()?
            .get(format!("{}/api/tags", self.base_url))
            .send()
            .await
            .map_err(|error| self.redact_transport_error(error.into()))?
            .error_for_status()
            .map_err(|error| self.redact_transport_error(error.into()))?;
        let catalog: Catalog = response
            .json()
            .await
            .map_err(|error| self.redact_transport_error(error.into()))?;
        Ok(catalog.models.into_iter().map(|entry| entry.name).collect())
    }
}

impl Attributable for HailoOllamaModelProvider {
    fn role(&self) -> Role {
        Role::Provider(ProviderKind::Model(ModelProviderKind::HailoOllama))
    }

    fn alias(&self) -> &str {
        &self.alias
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructor_normalizes_request_and_gate_url_identity() {
        let provider = HailoOllamaModelProvider::new(
            "edge",
            Some("HTTP://LOCALHOST:8000/prefix/api/chat/"),
            90,
            5,
            OllamaTuning::default(),
        )
        .expect("valid Hailo URL");
        let equivalent = HailoOllamaModelProvider::new(
            "edge-equivalent",
            Some("http://127.0.0.1:8000/prefix"),
            90,
            5,
            OllamaTuning::default(),
        )
        .expect("equivalent valid Hailo URL");

        assert_eq!(provider.base_url, "http://localhost:8000/prefix");
        assert!(Arc::ptr_eq(
            &provider.generation_gate,
            &equivalent.generation_gate
        ));
    }

    #[test]
    fn constructor_uses_typed_default_endpoint() {
        let provider = HailoOllamaModelProvider::new("edge", None, 90, 5, OllamaTuning::default())
            .expect("typed default Hailo URL must be valid");
        assert_eq!(provider.base_url, HailoOllamaEndpoint::default().uri());
    }

    #[test]
    fn constructor_rejects_ambiguous_or_authenticated_urls() {
        for url in [
            "http://user:placeholder@localhost:8000",
            "http://localhost:8000?route=canary",
            "http://localhost:8000#fragment",
            "ftp://localhost:8000",
        ] {
            let error = match HailoOllamaModelProvider::new(
                "edge",
                Some(url),
                90,
                5,
                OllamaTuning::default(),
            ) {
                Ok(_) => panic!("unsafe Hailo URL must be rejected"),
                Err(error) => error,
            }
            .to_string();
            assert!(
                error.contains("Hailo-Ollama URL"),
                "unexpected error: {error}"
            );
        }
    }

    #[test]
    fn gate_key_canonicalizes_loopback_aliases() {
        assert_eq!(
            canonical_endpoint("http://localhost:8000/api/chat"),
            canonical_endpoint("http://127.0.0.1:8000")
        );
        assert_eq!(
            canonical_endpoint("http://localhost:8000"),
            canonical_endpoint("http://[::1]:8000/api")
        );
    }

    #[test]
    fn request_keeps_model_identifier_opaque() {
        let provider = HailoOllamaModelProvider::new("edge", None, 90, 5, OllamaTuning::default())
            .expect("typed default Hailo URL must be valid");
        let request = provider.build_request(Vec::new(), "local-model:cloud", None);

        assert_eq!(request.model, "local-model:cloud");
    }

    #[test]
    fn ascii_serialization_preserves_decoded_content_exactly() {
        let content = "  first line\r\n\tindented näyttö 🧪\nlast line  ";
        let request = WireChatRequest {
            model: "edge".to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: Some(content.to_string()),
                images: None,
                tool_calls: None,
                tool_name: None,
            }],
            stream: false,
            options: Options {
                temperature: None,
                num_ctx: Some(2048),
                num_predict: Some(64),
            },
            think: Some(false),
            tools: None,
        };

        let serialized = serialize_request(&request).expect("Hailo request serializes");
        assert!(serialized.is_ascii());
        let decoded: serde_json::Value =
            serde_json::from_slice(&serialized).expect("serialized request is valid JSON");
        assert_eq!(decoded["messages"][0]["content"], content);
    }

    #[test]
    fn content_normalization_preserves_layout_controls() {
        let content = "  first line\r\n\tindented\nlast line  \0";
        assert_eq!(
            HailoOllamaModelProvider::normalize_content(content),
            "  first line\r\n\tindented\nlast line  "
        );
    }

    #[test]
    fn native_layout_controls_are_double_escaped_for_prompt_parser() {
        assert_eq!(
            HailoOllamaModelProvider::encode_layout_controls_for_native(
                "first\r\n\tindented\nlast"
            ),
            r"first\r\n\tindented\nlast"
        );
    }

    #[tokio::test]
    async fn panic_guard_quarantines_before_releasing_gate() {
        let gate = Arc::new(GenerationGate {
            semaphore: Arc::new(Semaphore::new(1)),
            quarantine_reason: OnceLock::new(),
        });
        let permit = gate
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("test gate remains open");
        let worker_gate = gate.clone();
        let worker = zeroclaw_spawn::spawn!(async move {
            let _permit = permit;
            let _guard = InFlightGuard::new(worker_gate);
            panic!("synthetic Hailo worker panic");
        });

        let _ = worker.await;
        let _next = gate
            .semaphore
            .acquire()
            .await
            .expect("test gate remains open");
        assert!(
            gate.quarantine_reason.get().is_some(),
            "the endpoint must be quarantined before a panic releases its permit"
        );
    }
}
