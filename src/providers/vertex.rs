use crate::providers::traits::{
    ChatMessage, ChatRequest, ChatResponse, Provider, TokenUsage, ToolCall,
};
use anyhow::Result;
use async_trait::async_trait;
use google_cloud_aiplatform_v1::client::PredictionService;
use google_cloud_aiplatform_v1::model::function_calling_config::Mode;
use google_cloud_aiplatform_v1::model::part::Data;
use google_cloud_aiplatform_v1::model::{
    Content, FunctionCallingConfig, FunctionDeclaration, FunctionResponse, GenerateContentRequest,
    GenerationConfig, Part, Schema, Tool, ToolConfig,
};
use serde_json::Value;
use std::sync::Arc;

pub struct VertexProvider {
    project_id: String,
    location: String,
    key_path: Option<String>,
    client: tokio::sync::OnceCell<Arc<PredictionService>>,
}

impl VertexProvider {
    pub fn new(
        project_id: Option<String>,
        location: Option<String>,
        api_key: Option<&str>,
        key_path: Option<String>,
    ) -> Result<Self> {
        let project_id = project_id
            .or_else(|| api_key.map(|s| s.to_string()))
            .or_else(|| std::env::var("GOOGLE_CLOUD_PROJECT").ok())
            .or_else(|| std::env::var("VERTEX_PROJECT_ID").ok());

        let project_id = project_id.ok_or_else(|| anyhow::anyhow!("Missing Google Cloud project ID. Set [provider.vertex] project, GOOGLE_CLOUD_PROJECT, VERTEX_PROJECT_ID, or provide a service account key file."))?;

        let location = location
            .or_else(|| std::env::var("GOOGLE_CLOUD_LOCATION").ok())
            .or_else(|| std::env::var("VERTEX_LOCATION").ok())
            .unwrap_or_else(|| "global".to_string());

        Ok(Self {
            project_id,
            location,
            key_path,
            client: tokio::sync::OnceCell::new(),
        })
    }

    async fn get_client(&self) -> Result<Arc<PredictionService>> {
        self.client
            .get_or_try_init(|| async {
                let mut builder = PredictionService::builder();

                if let Some(path) = &self.key_path {
                    let expanded_path = shellexpand::full(path)?;
                    let content = std::fs::read_to_string(expanded_path.as_ref())?;
                    let json: Value = serde_json::from_str(&content)?;
                    let creds = google_cloud_auth::credentials::service_account::Builder::new(json)
                        .build()?;
                    builder = builder.with_credentials(creds);
                } else {
                    let scopes = ["https://www.googleapis.com/auth/cloud-platform"];
                    let creds = google_cloud_auth::credentials::Builder::default()
                        .with_scopes(scopes)
                        .build()?;
                    builder = builder.with_credentials(creds);
                }

                if self.location != "global" {
                    builder = builder.with_endpoint(format!(
                        "https://{}-aiplatform.googleapis.com",
                        self.location
                    ));
                }
                // For "global" location, we use the default endpoint (aiplatform.googleapis.com)
                let client = builder.build().await?;
                Ok(Arc::new(client))
            })
            .await
            .cloned()
    }

    fn model_path(&self, model: &str) -> String {
        format!(
            "projects/{}/locations/{}/publishers/google/models/{}",
            self.project_id, self.location, model
        )
    }

    fn convert_messages(messages: &[ChatMessage]) -> Result<Vec<Content>> {
        let mut contents = Vec::new();

        for msg in messages {
            let role = match msg.role.as_str() {
                "assistant" => "model",
                "system" => continue, // Handled separately in system_instruction
                _ => "user",          // Includes "user" and "tool"
            };

            let parts = if msg.role == "tool" {
                // Parse tool result
                if let Ok(tool_result) = serde_json::from_str::<Value>(&msg.content) {
                    // Try to extract tool_use_id/name if present, or just put the content
                    let name = tool_result
                        .get("name")
                        .and_then(|v| v.as_str())
                        .or(tool_result.get("tool_name").and_then(|v| v.as_str()))
                        .unwrap_or("unknown_tool")
                        .to_string();

                    // The response content
                    let response_content = tool_result.get("content").unwrap_or(&tool_result);

                    // Convert response content to struct or map for FunctionResponse
                    let response_struct = if let Some(obj) = response_content.as_object() {
                        let mut s = google_cloud_wkt::Struct::default();
                        for (k, v) in obj {
                            s.insert(k.clone(), serde_json::from_value(v.clone())?);
                        }
                        s
                    } else {
                        let mut s = google_cloud_wkt::Struct::default();
                        s.insert(
                            "content".to_string(),
                            serde_json::from_value(response_content.clone())?,
                        );
                        s
                    };

                    let mut func_resp = FunctionResponse::default();
                    func_resp.name = name;
                    func_resp.response = Some(response_struct);

                    let mut part = Part::default();
                    part.data = Some(Data::FunctionResponse(Box::new(func_resp)));
                    vec![part]
                } else {
                    // Fallback for non-JSON tool results
                    let mut s = google_cloud_wkt::Struct::default();
                    s.insert(
                        "content".to_string(),
                        google_cloud_wkt::Value::String(msg.content.clone()),
                    );

                    let mut func_resp = FunctionResponse::default();
                    func_resp.name = "unknown".to_string();
                    func_resp.response = Some(s);

                    let mut part = Part::default();
                    part.data = Some(Data::FunctionResponse(Box::new(func_resp)));
                    vec![part]
                }
            } else {
                let mut part = Part::default();
                part.data = Some(Data::Text(msg.content.clone()));
                vec![part]
            };

            let mut content = Content::default();
            content.role = role.to_string();
            content.parts = parts;
            contents.push(content);
        }
        Ok(contents)
    }
}

#[async_trait]
impl Provider for VertexProvider {
    async fn chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: f64,
    ) -> Result<ChatResponse> {
        let client = self.get_client().await?;

        let system_prompt = request
            .messages
            .iter()
            .find(|m| m.role == "system")
            .map(|m| m.content.as_str());

        let system_instruction = system_prompt.map(|prompt| {
            let mut part = Part::default();
            part.data = Some(Data::Text(prompt.to_string()));

            let mut content = Content::default();
            content.role = "system".to_string();
            content.parts = vec![part];
            content
        });

        let contents = Self::convert_messages(request.messages)?;

        let tools = if let Some(tools) = request.tools {
            let mut function_declarations = Vec::new();
            for tool in tools {
                let schema: Schema = serde_json::from_value(tool.parameters.clone())
                    .map_err(|e| anyhow::anyhow!("Failed to parse tool schema: {}", e))?;

                let mut decl = FunctionDeclaration::default();
                decl.name = tool.name.clone();
                decl.description = tool.description.clone();
                decl.parameters = Some(schema);

                function_declarations.push(decl);
            }
            let mut tool_obj = Tool::default();
            tool_obj.function_declarations = function_declarations;
            Some(vec![tool_obj])
        } else {
            None
        };

        let tool_config = if tools.is_some() {
            let mut config = ToolConfig::default();
            let mut fc_config = FunctionCallingConfig::default();
            fc_config.mode = Mode::Auto;
            config.function_calling_config = Some(fc_config);
            Some(config)
        } else {
            None
        };

        let mut req = GenerateContentRequest::default();
        req.model = self.model_path(model);
        req.contents = contents;
        req.system_instruction = system_instruction;
        if let Some(t) = tools {
            req.tools = t;
        }
        req.tool_config = tool_config;

        let mut gen_config = GenerationConfig::default();
        #[allow(clippy::cast_possible_truncation)]
        {
            gen_config.temperature = Some(temperature as f32);
        }
        gen_config.max_output_tokens = Some(8192);
        req.generation_config = Some(gen_config);

        let response = client.generate_content().with_request(req).send().await?;

        let mut text_response = String::new();
        let mut tool_calls = Vec::new();
        let mut usage = None;

        if let Some(meta) = response.usage_metadata {
            #[allow(clippy::cast_sign_loss)]
            {
                usage = Some(TokenUsage {
                    input_tokens: Some(meta.prompt_token_count as u64),
                    output_tokens: Some(meta.candidates_token_count as u64),
                });
            }
        }

        if let Some(candidate) = response.candidates.first() {
            if let Some(content) = &candidate.content {
                for part in &content.parts {
                    if let Some(data) = &part.data {
                        match data {
                            Data::Text(text) => text_response.push_str(text),
                            Data::FunctionCall(call) => {
                                let args = if let Some(s) = &call.args {
                                    serde_json::to_string(s)?
                                } else {
                                    "{}".to_string()
                                };
                                tool_calls.push(ToolCall {
                                    id: "unknown".to_string(), // Vertex doesn't send IDs in the same way?
                                    name: call.name.clone(),
                                    arguments: args,
                                });
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        Ok(ChatResponse {
            text: if text_response.is_empty() {
                None
            } else {
                Some(text_response)
            },
            tool_calls,
            usage,
            reasoning_content: None,
        })
    }

    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> Result<String> {
        let client = self.get_client().await?;

        let system_instruction = system_prompt.map(|prompt| {
            let mut part = Part::default();
            part.data = Some(Data::Text(prompt.to_string()));

            let mut content = Content::default();
            content.role = "system".to_string();
            content.parts = vec![part];
            content
        });

        let mut part = Part::default();
        part.data = Some(Data::Text(message.to_string()));
        let mut content = Content::default();
        content.role = "user".to_string();
        content.parts = vec![part];

        let contents = vec![content];

        let mut req = GenerateContentRequest::default();
        req.model = self.model_path(model);
        req.contents = contents;
        req.system_instruction = system_instruction;

        let mut gen_config = GenerationConfig::default();
        #[allow(clippy::cast_possible_truncation)]
        {
            gen_config.temperature = Some(temperature as f32);
        }
        gen_config.max_output_tokens = Some(8192);
        req.generation_config = Some(gen_config);

        let response = client.generate_content().with_request(req).send().await?;

        if let Some(candidate) = response.candidates.first() {
            if let Some(content) = &candidate.content {
                for part in &content.parts {
                    if let Some(Data::Text(text)) = &part.data {
                        return Ok(text.clone());
                    }
                }
            }
        }

        Ok(String::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vertex_provider_new() {
        let provider = VertexProvider::new(
            Some("test-project".to_string()),
            Some("us-central1".to_string()),
            None,
            None,
        )
        .unwrap();

        assert_eq!(provider.project_id, "test-project");
        assert_eq!(provider.location, "us-central1");
    }

    #[test]
    fn test_vertex_provider_new_env() {
        std::env::set_var("GOOGLE_CLOUD_PROJECT", "env-project");
        std::env::set_var("GOOGLE_CLOUD_LOCATION", "env-location");

        let provider = VertexProvider::new(None, None, None, None).unwrap();

        assert_eq!(provider.project_id, "env-project");
        assert_eq!(provider.location, "env-location");

        std::env::remove_var("GOOGLE_CLOUD_PROJECT");
        std::env::remove_var("GOOGLE_CLOUD_LOCATION");
    }
}
