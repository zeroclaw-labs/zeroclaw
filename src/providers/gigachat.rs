//! Example: Implementing a custom Provider for ZeroClaw
//!
//! This shows how to add a new LLM backend in ~30 lines of code.
//! Copy this file, modify the API call, and register in `src/providers/mod.rs`.

use crate::providers::Provider;
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct AccessToken {
    access_token: String,
    expires_at: u64,
}

// https://developers.sber.ru/docs/ru/gigachat/api/reference/rest/post-chat
#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<Message>,
    // function_call: Option<FunctionCall>,
    // functions: Option<Vec<Function>>,
    temperature: f64,
    top_p: f64, // alternative to temprerature
    stream: bool,
    max_tokens: u32,
    repetition_penalty: f64,
    update_interval: u32,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<ChatResponseChoice>,
    created: u64,
    model: String,
    usage: ModelUsage,
    object: String,
}

#[derive(Debug, Deserialize)]
struct ModelUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    precached_prompt_tokens: u32,
    total_tokens: u32,
}

#[derive(Debug, Deserialize)]
struct ChatResponseChoice {
    message: MessageResponse,
    index: u32,
    finish_reason: Option<String>, // TODO: explicit enum [stop, length, function_call, blacklist, error]
}

#[derive(Debug, Serialize)]
struct Message {
    role: String, //  [user, system, assistant, function]
    content: Option<String>,
    functions_state_id: Option<String>, // UUID4
    attachments: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct MessageResponse {
    role: String, // TODO: explicit enum [assistant, function_in_progress]
    content: String,
    created: Option<u64>,
    name: Option<String>,
    functions_state_id: Option<String>, // UUID4
                                        // function_call_id: Option<FunctionCall>,
}

const OAUTH_API_ENDPOINT: &str = "https://ngw.devices.sberbank.ru:9443/api/v2/oauth";
const CHAT_COMPLETIONS_ENDPOINT: &str =
    "https://gigachat.devices.sberbank.ru/api/v1/chat/completions";

pub struct GigaChatProvider {
    base_url: String,
    credentials: String,
    scope: String,
    client: reqwest::Client,
    access_token: Option<AccessToken>,
}

impl GigaChatProvider {
    pub fn new(base_url: Option<&str>, credentials: Option<&str>) -> Self {
        Self {
            base_url: base_url.unwrap_or("http://localhost:11434").to_string(),
            scope: "GIGACHAT_API_PERS".to_string(),
            credentials: credentials.unwrap_or("").to_string(),
            client: Self::build_client().unwrap(),
            access_token: None,
        }
    }

    pub fn build_client() -> Result<reqwest::Client> {
        let builder = reqwest::Client::builder()
            .danger_accept_invalid_certs(true) // Sber GigaChat uses own certs, so ignoring
            .build();
        Ok(builder?)
    }

    // pub fn fetch_models(&self) -> anyhow::Result<Vec<String>> {
    //     let access_token = self.fetch_auth_token().await?;
    //
    //     let response = self
    //         .client
    //         .get("https://gigachat.devices.sberbank.ru/api/v1/models")
    //         .header(
    //             "Authorization",
    //             format!("Bearer {}", access_token.access_token),
    //         )
    //         .send()
    //         .await
    //         .or_else(|error| {
    //             tracing::error!("Response error: {:?}", error);
    //             Err(error)
    //         })?;
    //
    //     let models = response.text().await?;
    //
    //     tracing::debug!("GigaChat models: {:?}", models);
    //
    //     Ok(vec![])
    // }

    pub async fn fetch_auth_token(&self) -> anyhow::Result<AccessToken> {
        let req_id = Uuid::new_v4();

        let response = self
            .client
            .post(OAUTH_API_ENDPOINT)
            .body(format!("scope={}", self.scope))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("Accept", "application/json")
            .header("RqUID", req_id.to_string())
            .header("Authorization", format!("Basic {}", self.credentials))
            .send()
            .await
            .or_else(|error| {
                tracing::error!("Response error: {:?}", error);
                Err(error)
            })?;

        let token = response.json().await?;

        Ok(token)
    }

    // https://developers.sber.ru/docs/ru/gigachat/api/reference/rest/post-chat
    async fn fetch_chat_completions(
        &self,
        request: &ChatRequest,
        access_token: &AccessToken,
    ) -> anyhow::Result<ChatResponse> {
        let req_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();

        let response = self
            .client
            .post(CHAT_COMPLETIONS_ENDPOINT)
            .json(&request)
            // .header("X-Client-Id", "gigachat-web") // FIXME: need to find out what to put there
            .header("X-Request-Id", req_id.to_string())
            .header("X-Session-Id", session_id.to_string())
            .header(
                "Authorization",
                format!("Bearer {}", access_token.access_token),
            )
            .send()
            .await
            .or_else(|error| {
                tracing::error!("Response error: {:?}", error);
                Err(error)
            })?;

        let chat_response = response.json().await?;

        Ok(chat_response)
    }
}

#[async_trait]
impl Provider for GigaChatProvider {
    /// One-shot chat with optional system prompt.
    ///
    /// Kept for compatibility and advanced one-shot prompting.
    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        tracing::debug!(
            "chat with system model: '{}', message: '{}', temperature: '{}'",
            model,
            message,
            temperature
        );

        let access_token = self.fetch_auth_token().await?;

        let request = ChatRequest {
            model: model.to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: Some(message.to_string()),
                functions_state_id: None,
                attachments: None,
            }],
            stream: false,
            temperature: temperature,
            top_p: 0.0,
            max_tokens: 8192,
            repetition_penalty: 1.0,
            update_interval: 0,
        };

        let chat_response = self.fetch_chat_completions(&request, &access_token).await?;
        tracing::debug!("Chat Response: {:?}", chat_response);

        // join the response messages to single string
        let result = chat_response
            .choices
            .iter()
            .fold(String::new(), |acc, choice| acc + &choice.message.content);

        Ok(result.to_string())
    }
}
