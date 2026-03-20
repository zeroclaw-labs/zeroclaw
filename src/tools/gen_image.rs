//! `gen_image` tool — unified image generation routing to multiple backends.
//!
//! Supported backends:
//! 1. **ComfyUI** — direct HTTP calls to the ComfyUI REST API
//! 2. **DALL-E** — HTTP call to OpenAI's images/generations endpoint
//! 3. **Stability AI** — HTTP call to Stability's sd3 endpoint

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::json;

use crate::config::schema::ImageGenerationConfig;
use crate::tools::traits::{Tool, ToolResult};

/// Unified image generation tool routing to ComfyUI, DALL-E, or Stability AI.
pub struct GenImageTool {
    config: ImageGenerationConfig,
    workspace_dir: PathBuf,
    http_client: reqwest::Client,
}

/// Build a structured success result with JSON metadata + `[IMAGE:path]` marker.
fn success_result(path: &std::path::Path) -> ToolResult {
    let display = path.display();
    ToolResult {
        success: true,
        output: format!("{{\"ok\":true,\"file\":\"{display}\"}}\n[IMAGE:{display}]"),
        error: None,
    }
}

/// Build a structured failure result.  The error message explicitly tells the
/// LLM not to fabricate `[IMAGE:]` markers so it cannot hallucinate a path.
fn error_result(error: impl std::fmt::Display) -> ToolResult {
    ToolResult {
        success: false,
        output: String::new(),
        error: Some(format!(
            "{error} — image was NOT generated, do NOT include [IMAGE:] markers"
        )),
    }
}

impl GenImageTool {
    pub fn new(config: ImageGenerationConfig, workspace_dir: PathBuf) -> Self {
        Self {
            config,
            workspace_dir,
            http_client: crate::config::build_runtime_proxy_client_with_timeouts(
                "tool.gen_image",
                120,
                15,
            ),
        }
    }

    /// Resolve which backend to use from the args or config default.
    fn resolve_backend(&self, args: &serde_json::Value) -> String {
        args.get("backend")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_lowercase())
            .unwrap_or_else(|| self.config.default_backend.clone())
    }

    /// Resolve the output path — use provided `output_path` or auto-generate.
    fn resolve_output_path(&self, args: &serde_json::Value) -> PathBuf {
        if let Some(path) = args.get("output_path").and_then(|v| v.as_str()) {
            if !path.is_empty() {
                return PathBuf::from(path);
            }
        }
        // Auto-generate in workspace/generated_images/
        let dir = self.workspace_dir.join("generated_images");
        let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S_%3f");
        dir.join(format!("gen_{timestamp}.png"))
    }

    /// Build the built-in SDXL workflow JSON.
    fn build_default_workflow(
        &self,
        prompt: &str,
        negative_prompt: &str,
        seed: u64,
    ) -> serde_json::Value {
        let cfg = &self.config.comfyui;
        let ckpt = if cfg.checkpoint.is_empty() {
            // Use a placeholder — ComfyUI will fail clearly if no checkpoint is set.
            "v1-5-pruned-emaonly.safetensors"
        } else {
            &cfg.checkpoint
        };

        json!({
            "3": {
                "class_type": "KSampler",
                "inputs": {
                    "seed": seed,
                    "steps": cfg.steps,
                    "cfg": cfg.cfg,
                    "sampler_name": "euler_ancestral",
                    "scheduler": "normal",
                    "denoise": 1,
                    "model": ["4", 0],
                    "positive": ["6", 0],
                    "negative": ["7", 0],
                    "latent_image": ["5", 0]
                }
            },
            "4": {
                "class_type": "CheckpointLoaderSimple",
                "inputs": {
                    "ckpt_name": ckpt
                }
            },
            "5": {
                "class_type": "EmptyLatentImage",
                "inputs": {
                    "width": cfg.width,
                    "height": cfg.height,
                    "batch_size": 1
                }
            },
            "6": {
                "class_type": "CLIPTextEncode",
                "inputs": {
                    "text": prompt,
                    "clip": ["4", 1]
                }
            },
            "7": {
                "class_type": "CLIPTextEncode",
                "inputs": {
                    "text": negative_prompt,
                    "clip": ["4", 1]
                }
            },
            "8": {
                "class_type": "VAEDecode",
                "inputs": {
                    "samples": ["3", 0],
                    "vae": ["4", 2]
                }
            },
            "9": {
                "class_type": "SaveImage",
                "inputs": {
                    "filename_prefix": "gen_img",
                    "images": ["8", 0]
                }
            }
        })
    }

    /// Load a custom workflow from file and inject prompt, negative_prompt,
    /// checkpoint, width/height, steps, cfg, and seed into matching nodes.
    fn load_custom_workflow(
        &self,
        workflow_path: &str,
        prompt: &str,
        negative_prompt: &str,
        seed: u64,
    ) -> anyhow::Result<serde_json::Value> {
        let content = std::fs::read_to_string(workflow_path)
            .map_err(|e| anyhow::anyhow!("Failed to read workflow file '{workflow_path}': {e}"))?;
        let mut workflow: serde_json::Value = serde_json::from_str(&content)
            .map_err(|e| anyhow::anyhow!("Invalid JSON in workflow file '{workflow_path}': {e}"))?;

        let cfg = &self.config.comfyui;

        // We need to find CLIPTextEncode nodes and determine which is positive/negative.
        // Strategy: look at KSampler node to find which node IDs are connected to
        // "positive" and "negative" inputs.
        let mut positive_node_id: Option<String> = None;
        let mut negative_node_id: Option<String> = None;

        // First pass: find KSampler to determine positive/negative node IDs.
        if let Some(wf_obj) = workflow.as_object() {
            for (_node_id, node) in wf_obj {
                if node.get("class_type").and_then(|v| v.as_str()) == Some("KSampler") {
                    if let Some(inputs) = node.get("inputs") {
                        if let Some(pos) = inputs.get("positive").and_then(|v| v.as_array()) {
                            if let Some(id) = pos.first().and_then(|v| v.as_str()) {
                                positive_node_id = Some(id.to_string());
                            }
                        }
                        if let Some(neg) = inputs.get("negative").and_then(|v| v.as_array()) {
                            if let Some(id) = neg.first().and_then(|v| v.as_str()) {
                                negative_node_id = Some(id.to_string());
                            }
                        }
                    }
                }
            }
        }

        // Second pass: inject values into nodes.
        if let Some(wf_obj) = workflow.as_object_mut() {
            for (node_id, node) in wf_obj.iter_mut() {
                let class_type = node
                    .get("class_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                if let Some(inputs) = node.get_mut("inputs") {
                    match class_type.as_str() {
                        "CLIPTextEncode" => {
                            if positive_node_id.as_deref() == Some(node_id.as_str()) {
                                inputs["text"] = serde_json::Value::String(prompt.to_string());
                            } else if negative_node_id.as_deref() == Some(node_id.as_str()) {
                                inputs["text"] =
                                    serde_json::Value::String(negative_prompt.to_string());
                            }
                        }
                        "CheckpointLoaderSimple" => {
                            if !cfg.checkpoint.is_empty() {
                                inputs["ckpt_name"] =
                                    serde_json::Value::String(cfg.checkpoint.clone());
                            }
                        }
                        "EmptyLatentImage" => {
                            inputs["width"] = json!(cfg.width);
                            inputs["height"] = json!(cfg.height);
                        }
                        "KSampler" => {
                            inputs["steps"] = json!(cfg.steps);
                            inputs["cfg"] = json!(cfg.cfg);
                            inputs["seed"] = json!(seed);
                        }
                        _ => {}
                    }
                }
            }
        }

        Ok(workflow)
    }

    /// Generate via ComfyUI REST API.
    async fn generate_comfyui(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let prompt = args
            .get("prompt")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let negative_prompt = args
            .get("negative_prompt")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let output_path = self.resolve_output_path(args);

        let cfg = &self.config.comfyui;
        let base_url = format!("http://{}:{}", cfg.host, cfg.port);

        // Generate a random seed.
        let seed: u64 = rand::random();

        // Build workflow — either built-in or custom file.
        let workflow = if cfg.workflow_path.is_empty() {
            self.build_default_workflow(prompt, negative_prompt, seed)
        } else {
            match self.load_custom_workflow(&cfg.workflow_path, prompt, negative_prompt, seed) {
                Ok(w) => w,
                Err(e) => {
                    return Ok(error_result(format!(
                        "Failed to load custom ComfyUI workflow: {e}"
                    )));
                }
            }
        };

        // ── POST to /prompt ──────────────────────────────────────────
        let prompt_url = format!("{base_url}/prompt");
        let body = json!({ "prompt": workflow });

        tracing::debug!("ComfyUI: posting workflow to {prompt_url}");

        let resp = self
            .http_client
            .post(&prompt_url)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "ComfyUI: failed to connect to {base_url}. Is ComfyUI running? Error: {e}"
                )
            })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body_text = resp.text().await.unwrap_or_default();
            return Ok(error_result(format!(
                "ComfyUI /prompt error (HTTP {status}): {body_text}"
            )));
        }

        let resp_json: serde_json::Value = resp.json().await?;
        let prompt_id = resp_json["prompt_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("ComfyUI /prompt response missing prompt_id"))?
            .to_string();

        tracing::info!("ComfyUI: queued prompt_id={prompt_id}");

        // ── Poll /history/{prompt_id} ────────────────────────────────
        let history_url = format!("{base_url}/history/{prompt_id}");
        let poll_interval = std::time::Duration::from_secs(2);
        let timeout = std::time::Duration::from_secs(180);
        let start = std::time::Instant::now();

        let history_entry = loop {
            if start.elapsed() > timeout {
                return Ok(error_result(format!(
                    "ComfyUI: generation timed out after {}s (prompt_id={prompt_id})",
                    timeout.as_secs()
                )));
            }

            tokio::time::sleep(poll_interval).await;

            let poll_resp = match self.http_client.get(&history_url).send().await {
                Ok(r) => r,
                Err(e) => {
                    tracing::debug!("ComfyUI: history poll error: {e}");
                    continue;
                }
            };

            if !poll_resp.status().is_success() {
                continue;
            }

            let history: serde_json::Value = match poll_resp.json().await {
                Ok(v) => v,
                Err(_) => continue,
            };

            // The response is { "<prompt_id>": { "outputs": { ... } } }
            if let Some(entry) = history.get(&prompt_id) {
                if entry.get("outputs").is_some() {
                    break entry.clone();
                }
            }
        };

        // ── Extract output image filename ────────────────────────────
        // Find the SaveImage node output. Try node "9" first (built-in workflow),
        // then scan all outputs for any node with images.
        let filename = Self::extract_comfyui_filename(&history_entry);

        let filename = match filename {
            Some(f) => f,
            None => {
                return Ok(error_result(format!(
                    "ComfyUI: no output image found in history for prompt_id={prompt_id}"
                )));
            }
        };

        // ── Download the image from /view ────────────────────────────
        let view_url = format!(
            "{base_url}/view?filename={}",
            urlencoding::encode(&filename)
        );
        let image_resp =
            self.http_client.get(&view_url).send().await.map_err(|e| {
                anyhow::anyhow!("ComfyUI: failed to download image '{filename}': {e}")
            })?;

        if !image_resp.status().is_success() {
            let status = image_resp.status();
            return Ok(error_result(format!(
                "ComfyUI /view error (HTTP {status}) for filename '{filename}'"
            )));
        }

        let image_bytes = image_resp.bytes().await?;

        // Ensure output directory exists and write the image.
        if let Some(parent) = output_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&output_path, &image_bytes).await?;

        tracing::info!(
            "ComfyUI: saved generated image to {}",
            output_path.display()
        );

        Ok(success_result(&output_path))
    }

    /// Extract the first output image filename from a ComfyUI history entry.
    fn extract_comfyui_filename(history_entry: &serde_json::Value) -> Option<String> {
        let outputs = history_entry.get("outputs")?;

        // Try node "9" first (our built-in workflow).
        if let Some(filename) = outputs
            .get("9")
            .and_then(|n| n.get("images"))
            .and_then(|imgs| imgs.as_array())
            .and_then(|arr| arr.first())
            .and_then(|img| img.get("filename"))
            .and_then(|f| f.as_str())
        {
            return Some(filename.to_string());
        }

        // Scan all output nodes for any with images.
        if let Some(outputs_obj) = outputs.as_object() {
            for (_node_id, node_output) in outputs_obj {
                if let Some(filename) = node_output
                    .get("images")
                    .and_then(|imgs| imgs.as_array())
                    .and_then(|arr| arr.first())
                    .and_then(|img| img.get("filename"))
                    .and_then(|f| f.as_str())
                {
                    return Some(filename.to_string());
                }
            }
        }

        None
    }

    /// Generate via DALL-E API.
    async fn generate_dalle(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let prompt = args
            .get("prompt")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let size = args
            .get("size")
            .and_then(|v| v.as_str())
            .unwrap_or("1024x1024");
        let output_path = self.resolve_output_path(args);

        let api_key = std::env::var(&self.config.dalle.api_key_env).unwrap_or_default();
        if api_key.is_empty() {
            return Ok(error_result(format!(
                "DALL-E API key not found. Set the {} environment variable",
                self.config.dalle.api_key_env
            )));
        }

        let body = json!({
            "model": self.config.dalle.model,
            "prompt": prompt,
            "n": 1,
            "size": size,
        });

        let resp = self
            .http_client
            .post("https://api.openai.com/v1/images/generations")
            .header("Authorization", format!("Bearer {api_key}"))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body_text = resp.text().await.unwrap_or_default();
            return Ok(error_result(format!(
                "DALL-E API error (HTTP {status}): {body_text}"
            )));
        }

        let resp_json: serde_json::Value = resp.json().await?;
        let image_url = resp_json["data"][0]["url"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("DALL-E response missing image URL"))?;

        // Download the image
        let image_bytes = self
            .http_client
            .get(image_url)
            .send()
            .await?
            .bytes()
            .await?;

        // Ensure output directory exists
        if let Some(parent) = output_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&output_path, &image_bytes).await?;

        Ok(success_result(&output_path))
    }

    /// Generate via Stability AI API.
    async fn generate_stability(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let prompt = args
            .get("prompt")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let output_path = self.resolve_output_path(args);

        let api_key = std::env::var(&self.config.stability.api_key_env).unwrap_or_default();
        if api_key.is_empty() {
            return Ok(error_result(format!(
                "Stability API key not found. Set the {} environment variable",
                self.config.stability.api_key_env
            )));
        }

        let mut form = reqwest::multipart::Form::new()
            .text("prompt", prompt.to_string())
            .text("output_format", "png");

        if let Some(neg) = args.get("negative_prompt").and_then(|v| v.as_str()) {
            if !neg.is_empty() {
                form = form.text("negative_prompt", neg.to_string());
            }
        }

        let resp = self
            .http_client
            .post("https://api.stability.ai/v2beta/stable-image/generate/sd3")
            .header("Authorization", format!("Bearer {api_key}"))
            .header("Accept", "image/*")
            .multipart(form)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body_text = resp.text().await.unwrap_or_default();
            return Ok(error_result(format!(
                "Stability AI API error (HTTP {status}): {body_text}"
            )));
        }

        let image_bytes = resp.bytes().await?;

        // Ensure output directory exists
        if let Some(parent) = output_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&output_path, &image_bytes).await?;

        Ok(success_result(&output_path))
    }
}

#[async_trait]
impl Tool for GenImageTool {
    fn name(&self) -> &str {
        "gen_image"
    }

    fn description(&self) -> &str {
        "Generate an image using AI models. Supports ComfyUI (direct API), DALL-E, and Stability AI backends. On success returns JSON with ok:true and an [IMAGE:path] marker. On failure returns ok:false — you MUST NOT fabricate [IMAGE:] markers when generation fails."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "Text prompt describing the image to generate"
                },
                "negative_prompt": {
                    "type": "string",
                    "description": "Negative prompt for Stable Diffusion-style backends (things to avoid)"
                },
                "backend": {
                    "type": "string",
                    "enum": ["comfyui", "dalle", "stability"],
                    "description": "Image generation backend. Defaults to config value."
                },
                "size": {
                    "type": "string",
                    "description": "Image dimensions (e.g. '1024x1024'). Default: '1024x1024'."
                },
                "output_path": {
                    "type": "string",
                    "description": "File path to save the generated image. Auto-generated if omitted."
                }
            },
            "required": ["prompt"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let prompt = args.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
        if prompt.is_empty() {
            return Ok(error_result("'prompt' is required and cannot be empty"));
        }

        let backend = self.resolve_backend(&args);
        tracing::info!(backend = %backend, "gen_image: routing to backend");

        match backend.as_str() {
            "comfyui" => self.generate_comfyui(&args).await,
            "dalle" => self.generate_dalle(&args).await,
            "stability" => self.generate_stability(&args).await,
            other => Ok(error_result(format!(
                "Unknown image generation backend: '{other}'. Supported: comfyui, dalle, stability"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_schema_has_required_prompt() {
        let tool = GenImageTool::new(ImageGenerationConfig::default(), PathBuf::from("/tmp"));
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "prompt"));
    }

    #[test]
    fn resolve_backend_uses_default() {
        let tool = GenImageTool::new(
            ImageGenerationConfig {
                default_backend: "dalle".into(),
                ..ImageGenerationConfig::default()
            },
            PathBuf::from("/tmp"),
        );
        let args = json!({ "prompt": "test" });
        assert_eq!(tool.resolve_backend(&args), "dalle");
    }

    #[test]
    fn resolve_backend_uses_override() {
        let tool = GenImageTool::new(ImageGenerationConfig::default(), PathBuf::from("/tmp"));
        let args = json!({ "prompt": "test", "backend": "stability" });
        assert_eq!(tool.resolve_backend(&args), "stability");
    }

    #[test]
    fn build_default_workflow_has_expected_nodes() {
        let tool = GenImageTool::new(ImageGenerationConfig::default(), PathBuf::from("/tmp"));
        let wf = tool.build_default_workflow("a cat", "ugly", 42);

        // Check KSampler
        assert_eq!(wf["3"]["class_type"], "KSampler");
        assert_eq!(wf["3"]["inputs"]["seed"], 42);
        assert_eq!(wf["3"]["inputs"]["steps"], 25);

        // Check positive prompt
        assert_eq!(wf["6"]["inputs"]["text"], "a cat");

        // Check negative prompt
        assert_eq!(wf["7"]["inputs"]["text"], "ugly");

        // Check image dimensions
        assert_eq!(wf["5"]["inputs"]["width"], 1024);
        assert_eq!(wf["5"]["inputs"]["height"], 1024);
    }

    #[test]
    fn extract_comfyui_filename_from_node_9() {
        let entry = json!({
            "outputs": {
                "9": {
                    "images": [
                        { "filename": "gen_img_00001_.png", "subfolder": "", "type": "output" }
                    ]
                }
            }
        });
        assert_eq!(
            GenImageTool::extract_comfyui_filename(&entry),
            Some("gen_img_00001_.png".to_string())
        );
    }

    #[test]
    fn extract_comfyui_filename_from_other_node() {
        let entry = json!({
            "outputs": {
                "12": {
                    "images": [
                        { "filename": "custom_00001_.png" }
                    ]
                }
            }
        });
        assert_eq!(
            GenImageTool::extract_comfyui_filename(&entry),
            Some("custom_00001_.png".to_string())
        );
    }

    #[test]
    fn extract_comfyui_filename_none_when_empty() {
        let entry = json!({ "outputs": {} });
        assert!(GenImageTool::extract_comfyui_filename(&entry).is_none());
    }

    #[test]
    fn success_result_contains_json_and_image_marker() {
        let path = PathBuf::from("/tmp/workspace/gen_20260101.png");
        let result = success_result(&path);
        assert!(result.success);
        assert!(result.output.contains("\"ok\":true"));
        assert!(result
            .output
            .contains("[IMAGE:/tmp/workspace/gen_20260101.png]"));
        assert!(result.error.is_none());
    }

    #[test]
    fn error_result_contains_no_image_marker() {
        let result = error_result("ComfyUI unreachable");
        assert!(!result.success);
        assert!(result.output.is_empty());
        let err = result.error.as_deref().unwrap();
        assert!(err.contains("ComfyUI unreachable"));
        assert!(err.contains("do NOT include [IMAGE:]"));
        assert!(!err.contains("[IMAGE:/"));
    }
}
