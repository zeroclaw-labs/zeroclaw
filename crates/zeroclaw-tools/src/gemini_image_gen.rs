//! Gemini image generation/edit tool routed via LiteLLM.
//!
//! Mirrors the `nano-banana-pro` skill: calls the LiteLLM `/chat/completions`
//! endpoint with `modalities: ["image","text"]` against models like
//! `gemini-api-image-banana2` (Gemini 3 Pro Image). Decodes the returned
//! base64 image, saves it under `{workspace}/images/`, and returns a
//! ready-to-use `[IMAGE:...]` marker in the tool output so channels that
//! parse markers (Telegram, Matrix, …) deliver the file.
//!
//! Credentials are resolved in this order:
//!   1. `LITELLM_BASE_URL` / `LITELLM_API_KEY` env vars
//!   2. `[providers.models.litellm]` from `~/.zeroclaw/config.toml`,
//!      decrypting `enc2:` values via the `SecretStore`.

use anyhow::Context;
use async_trait::async_trait;
use base64::Engine as _;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::policy::ToolOperation;
use zeroclaw_config::secrets::SecretStore;

const DEFAULT_MODEL: &str = "gemini-api-image-banana2";
const ALLOWED_MODELS: &[&str] = &["gemini-api-image-banana", "gemini-api-image-banana2"];

pub struct GeminiImageGenTool {
    security: Arc<SecurityPolicy>,
    workspace_dir: PathBuf,
    default_model: String,
}

impl GeminiImageGenTool {
    pub fn new(
        security: Arc<SecurityPolicy>,
        workspace_dir: PathBuf,
        default_model: String,
    ) -> Self {
        let default_model = if default_model.trim().is_empty() {
            DEFAULT_MODEL.to_string()
        } else {
            default_model
        };
        Self {
            security,
            workspace_dir,
            default_model,
        }
    }

    fn http_client() -> reqwest::Client {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .unwrap_or_default()
    }

    /// Resolve LiteLLM (base_url, api_key). Env vars first, then config.toml.
    fn resolve_litellm_creds() -> Result<(String, String), String> {
        if let (Ok(base), Ok(key)) =
            (std::env::var("LITELLM_BASE_URL"), std::env::var("LITELLM_API_KEY"))
            && !base.trim().is_empty()
            && !key.trim().is_empty()
        {
            return Ok((base.trim().trim_end_matches('/').to_string(), key.trim().to_string()));
        }

        let home = std::env::var("HOME")
            .map_err(|_| "HOME not set; cannot locate ~/.zeroclaw/config.toml".to_string())?;
        let config_path = PathBuf::from(&home).join(".zeroclaw").join("config.toml");
        if !config_path.exists() {
            return Err(format!(
                "LiteLLM credentials not found: set LITELLM_BASE_URL and LITELLM_API_KEY, \
                 or configure [providers.models.litellm] in {}",
                config_path.display()
            ));
        }

        let raw = std::fs::read_to_string(&config_path)
            .map_err(|e| format!("read {}: {e}", config_path.display()))?;
        let toml_doc: toml::Table = toml::from_str(&raw)
            .map_err(|e| format!("parse {}: {e}", config_path.display()))?;

        let prov = toml_doc
            .get("providers")
            .and_then(|v| v.get("models"))
            .and_then(|v| v.get("litellm"))
            .ok_or_else(|| {
                "[providers.models.litellm] not configured in ~/.zeroclaw/config.toml".to_string()
            })?;

        let base_url = prov
            .get("base_url")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().trim_end_matches('/').to_string())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "[providers.models.litellm].base_url missing".to_string())?;

        let raw_key = prov
            .get("api_key")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "[providers.models.litellm].api_key missing".to_string())?;

        let api_key = if raw_key.starts_with("enc2:") || raw_key.starts_with("enc:") {
            let store = SecretStore::new(&PathBuf::from(&home).join(".zeroclaw"), false);
            store
                .decrypt(&raw_key)
                .map_err(|e| format!("decrypt litellm api_key: {e}"))?
        } else {
            raw_key
        };

        Ok((base_url, api_key))
    }

    async fn run(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let prompt = match args.get("prompt").and_then(|v| v.as_str()) {
            Some(p) if !p.trim().is_empty() => p.trim().to_string(),
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("Missing required parameter: 'prompt'".into()),
                });
            }
        };

        let filename = args
            .get("filename")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("generated_image");
        let safe_name = PathBuf::from(filename).file_name().map_or_else(
            || "generated_image".to_string(),
            |n| n.to_string_lossy().to_string(),
        );
        let safe_name = if safe_name.to_ascii_lowercase().ends_with(".png") {
            safe_name
        } else {
            format!("{safe_name}.png")
        };

        let model_arg = args
            .get("model")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let model = model_arg.unwrap_or(self.default_model.as_str()).to_string();
        if !ALLOWED_MODELS.contains(&model.as_str()) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Invalid model '{model}'. Allowed: {}",
                    ALLOWED_MODELS.join(", ")
                )),
            });
        }

        // Optional input images for edit / multi-image composition.
        let input_paths: Vec<PathBuf> = args
            .get("inputs")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| PathBuf::from(shellexpand_home(s)))
                    .collect()
            })
            .unwrap_or_default();

        for p in &input_paths {
            if !p.exists() {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Input image not found: {}", p.display())),
                });
            }
        }

        let (base_url, api_key) = match Self::resolve_litellm_creds() {
            Ok(c) => c,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(e),
                });
            }
        };

        // Build chat/completions payload (image + optional inputs).
        let mut content: Vec<serde_json::Value> = Vec::new();
        content.push(json!({"type": "text", "text": prompt}));
        for p in &input_paths {
            let bytes = tokio::fs::read(p)
                .await
                .with_context(|| format!("read input image {}", p.display()))?;
            let mime = guess_mime(p);
            let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
            content.push(json!({
                "type": "image_url",
                "image_url": {"url": format!("data:{mime};base64,{b64}")}
            }));
        }

        let payload = json!({
            "model": model,
            "messages": [{"role": "user", "content": content}],
            "modalities": ["image", "text"],
        });

        let client = Self::http_client();
        let url = format!("{base_url}/chat/completions");
        let resp = client
            .post(&url)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
            .await
            .context("LiteLLM request failed")?;

        let status = resp.status();
        let body_text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("LiteLLM API error ({status}): {body_text}")),
            });
        }

        let data: serde_json::Value = serde_json::from_str(&body_text)
            .with_context(|| format!("parse LiteLLM response: {body_text}"))?;

        let png_bytes = extract_png_from_chat_response(&data).ok_or_else(|| {
            anyhow::anyhow!("No image in LiteLLM response: {body_text}")
        })?;

        let images_dir = self.workspace_dir.join("images");
        tokio::fs::create_dir_all(&images_dir)
            .await
            .context("Failed to create images directory")?;
        let output_path = images_dir.join(&safe_name);
        tokio::fs::write(&output_path, &png_bytes)
            .await
            .context("Failed to write image file")?;

        let size_kb = png_bytes.len() / 1024;
        Ok(ToolResult {
            success: true,
            output: format!(
                "Image generated successfully.\n\
                 File: {path}\n\
                 Size: {size_kb} KB\n\
                 Model: {model}\n\
                 Prompt: {prompt}\n\
                 \n\
                 To deliver this image to the user, include the following marker verbatim in your reply (on its own line, outside any code fence):\n\
                 [IMAGE:{path}]\n\
                 Without this marker the user receives no image, only your text.",
                path = output_path.display(),
            ),
            error: None,
        })
    }
}

fn shellexpand_home(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{home}/{rest}");
        }
    }
    p.to_string()
}

fn guess_mime(p: &std::path::Path) -> &'static str {
    match p
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        _ => "image/png",
    }
}

fn extract_png_from_chat_response(data: &serde_json::Value) -> Option<Vec<u8>> {
    let msg = data
        .get("choices")?
        .as_array()?
        .first()?
        .get("message")?;

    // OpenAI/LiteLLM "images" field shape.
    if let Some(images) = msg.get("images").and_then(|v| v.as_array()) {
        for img in images {
            if let Some(url) = img
                .get("image_url")
                .and_then(|v| v.get("url"))
                .and_then(|v| v.as_str())
                && let Some(b64) = url.split(";base64,").nth(1)
                && let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64)
            {
                return Some(bytes);
            }
        }
    }

    // Some gateways place a data URL inside content[]→ image_url.
    if let Some(parts) = msg.get("content").and_then(|v| v.as_array()) {
        for part in parts {
            if let Some(url) = part
                .get("image_url")
                .and_then(|v| v.get("url"))
                .and_then(|v| v.as_str())
                && let Some(b64) = url.split(";base64,").nth(1)
                && let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64)
            {
                return Some(bytes);
            }
        }
    }

    None
}

#[async_trait]
impl Tool for GeminiImageGenTool {
    fn name(&self) -> &str {
        "gemini_image_gen"
    }

    fn description(&self) -> &str {
        "Generate or edit an image via Gemini 2.5/3 Pro Image (Nano Banana / Nano Banana Pro), \
         routed through the LiteLLM provider configured in ~/.zeroclaw/config.toml. \
         Saves the result to the workspace images directory and returns the file path \
         plus a ready-to-use [IMAGE:...] marker for channel delivery."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "required": ["prompt"],
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "Text prompt describing the image (or edit instructions if 'inputs' is set)."
                },
                "filename": {
                    "type": "string",
                    "description": "Output filename. '.png' is appended if missing. Saved in workspace/images/."
                },
                "model": {
                    "type": "string",
                    "enum": ["gemini-api-image-banana", "gemini-api-image-banana2"],
                    "description": "LiteLLM model name. Default: gemini-api-image-banana2 (Nano Banana Pro / Gemini 3 Pro Image)."
                },
                "inputs": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Optional list of absolute paths to input images for edit or multi-image composition."
                }
            }
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "gemini_image_gen")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
        }
        self.run(args).await
    }
}
