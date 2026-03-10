//! Implementing a GigaChat Provider for ZeroClaw
//!

use crate::providers::Provider;
use anyhow::Result;
use async_trait::async_trait;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct AccessToken {
    access_token: String,
    expires_at: u64,
}

#[derive(Debug, Deserialize)]
pub struct HttpError {
    status: u16,
    message: String,
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

#[derive(Debug, Deserialize)]
struct ModelsResponse {
    data: Vec<Model>,
    object: String,
}

#[derive(Debug, Deserialize)]
struct Model {
    id: String,
    object: String,
    owned_by: String,
    #[serde(rename = "type")]
    model_type: String, // TODO: explicit enum [chat, aicheck, embedder]
}

const OAUTH_API_ENDPOINT: &str = "https://ngw.devices.sberbank.ru:9443/api/v2/oauth";
const BASE_URL: &str = "https://gigachat.devices.sberbank.ru/api/v1/";

pub struct GigaChatProvider {
    credentials: String,
    scope: String,
    client: reqwest::Client,
    access_token: Option<AccessToken>,
}

impl GigaChatProvider {
    pub fn new(credentials: Option<&str>) -> Self {
        Self {
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

    // https://developers.sber.ru/docs/ru/gigachat/api/reference/rest/get-models
    pub async fn fetch_models(&self) -> anyhow::Result<Vec<String>> {
        let access_token = self.fetch_auth_token().await.or_else(|error| {
            return Err(error);
        })?;

        let response = self
            .client
            .get(format!("{}/models", BASE_URL))
            .header(
                "Authorization",
                format!("Bearer {}", access_token.access_token),
            )
            .send()
            .await
            .or_else(|error| {
                return Err(error);
            })?;

        if !response.status().is_success() {
            if response.status() == StatusCode::UNAUTHORIZED {
                // For 401 explicit error returned
                // https://developers.sber.ru/docs/ru/gigachat/api/reference/rest/get-models#responsesr
                let error = response.json::<HttpError>().await?;

                return Err(anyhow::anyhow!(
                    "GigaChat API error: {} {}",
                    error.status,
                    error.message
                ));
            }

            // Just a rest of unhandled errors
            return Err(anyhow::anyhow!("GigaChat API error: {}", response.status()));
        }

        let models = response.json::<ModelsResponse>().await?;

        let result = models
            .data
            .iter()
            .map(|model| format!("{} [{}]", model.id, model.model_type))
            .collect();

        Ok(result)
    }

    // https://developers.sber.ru/docs/ru/gigachat/api/reference/rest/post-token
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
                return Err(error);
            })?;

        if !response.status().is_success() {
            if response.status() == StatusCode::UNAUTHORIZED {
                // For 401 explicit error returned
                // https://developers.sber.ru/docs/ru/gigachat/api/reference/rest/post-token#responses
                let error = response.json::<HttpError>().await?;

                return Err(anyhow::anyhow!(
                    "GigaChat fetch token error: {} {}",
                    error.status,
                    error.message
                ));
            }

            // Just a rest of unhandled errors
            return Err(anyhow::anyhow!(
                "GigaChat fetch token error: {}",
                response.status()
            ));
        }

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
            .post(format!("{}/chat/completions", BASE_URL))
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
            .or_else(|error| return Err(error))?;

        // Codes we can explicitly handle:
        // https://developers.sber.ru/docs/ru/gigachat/api/reference/rest/post-chat#responses
        const DATA_CODES: [StatusCode; 5] = [
            StatusCode::UNAUTHORIZED,
            StatusCode::NOT_FOUND,
            StatusCode::UNPROCESSABLE_ENTITY,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::INTERNAL_SERVER_ERROR,
        ];

        if !response.status().is_success() {
            if DATA_CODES.contains(&response.status()) {
                let error = response.json::<HttpError>().await?;

                return Err(anyhow::anyhow!(
                    "GigaChat completion error: {} {}",
                    error.status,
                    error.message
                ));
            }

            // Just a rest of unhandled errors
            return Err(anyhow::anyhow!(
                "GigaChat completion error: {}",
                response.status()
            ));
        }

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
        // tracing::info!(
        //     "chat with system model: '{}', message: '{}', temperature: '{}', system_prompt: '{}'",
        //     model,
        //     message,
        //     temperature,
        //     system_prompt.unwrap_or_default()
        // );

        let access_token = self.fetch_auth_token().await.or_else(|error| {
            return Err(error);
        })?;

        let mut messages = vec![];
        if system_prompt.is_some() {
            messages.push(Message {
                role: "system".to_string(),
                content: Some(system_prompt.unwrap_or_else(|| "").to_string()),
                functions_state_id: None,
                attachments: None,
            });
        }

        if message.len() > 0 {
            messages.push(Message {
                role: "user".to_string(),
                content: Some(message.to_string()),
                functions_state_id: None,
                attachments: None,
            });
        }

        if messages.is_empty() {
            return Err(anyhow::anyhow!("No messages provided"));
        }

        // TODO: better handling - get rid of hardcoded values
        let request = ChatRequest {
            model: model.to_string(),
            messages: messages,
            stream: false,
            temperature: temperature,
            top_p: 0.0,
            max_tokens: 8192,
            repetition_penalty: 1.0,
            update_interval: 0,
        };

        let chat_response = self
            .fetch_chat_completions(&request, &access_token)
            .await
            .or_else(|error| {
                return Err(error);
            })?;

        // tracing::debug!("Chat Response: {:?}", chat_response);

        // join the response messages to single string
        let result = chat_response
            .choices
            .iter()
            .fold(String::new(), |acc, choice| acc + &choice.message.content);

        Ok(result.to_string())
    }
}
