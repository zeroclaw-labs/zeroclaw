//! Live Canvas (A2UI) tool — push rendered content to a web canvas in real time.
//!
//! The agent can render HTML/SVG/Markdown to a named canvas, snapshot its
//! current state, clear it, or evaluate a JavaScript expression in the canvas
//! context. Content is stored in a shared [`CanvasStore`] and broadcast to
//! connected WebSocket clients via per-canvas channels.

use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::broadcast;

/// Maximum content size per canvas frame (256 KB).
pub const MAX_CONTENT_SIZE: usize = 256 * 1024;

/// Maximum number of history frames kept per canvas.
const MAX_HISTORY_FRAMES: usize = 50;

/// Broadcast channel capacity per canvas.
const BROADCAST_CAPACITY: usize = 64;

/// Maximum number of concurrent canvases to prevent memory exhaustion.
const MAX_CANVAS_COUNT: usize = 100;

/// Allowed content types for canvas frames via the REST API.
pub const ALLOWED_CONTENT_TYPES: &[&str] = &["html", "svg", "markdown", "text"];

/// A single canvas frame (one render).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanvasFrame {
    /// Unique frame identifier.
    pub frame_id: String,
    /// Content type: `html`, `svg`, `markdown`, or `text`.
    pub content_type: String,
    /// The rendered content.
    pub content: String,
    /// ISO-8601 timestamp of when the frame was created.
    pub timestamp: String,
}

/// Per-canvas state: current content + history + broadcast sender.
struct CanvasEntry {
    current: Option<CanvasFrame>,
    history: Vec<CanvasFrame>,
    tx: broadcast::Sender<CanvasFrame>,
}

/// Shared canvas store — holds all active canvases.
///
/// Thread-safe and cheaply cloneable (wraps `Arc`).
#[derive(Clone)]
pub struct CanvasStore {
    inner: Arc<RwLock<HashMap<String, CanvasEntry>>>,
}

impl Default for CanvasStore {
    fn default() -> Self {
        Self::new()
    }
}

impl CanvasStore {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Push a new frame to a canvas. Creates the canvas if it does not exist.
    /// Returns `None` if the maximum canvas count has been reached and this is a new canvas.
    pub fn render(
        &self,
        canvas_id: &str,
        content_type: &str,
        content: &str,
    ) -> Option<CanvasFrame> {
        let frame = CanvasFrame {
            frame_id: uuid::Uuid::new_v4().to_string(),
            content_type: content_type.to_string(),
            content: content.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
        };

        let mut store = self.inner.write();

        // Enforce canvas count limit for new canvases.
        if !store.contains_key(canvas_id) && store.len() >= MAX_CANVAS_COUNT {
            return None;
        }

        let entry = store
            .entry(canvas_id.to_string())
            .or_insert_with(|| CanvasEntry {
                current: None,
                history: Vec::new(),
                tx: broadcast::channel(BROADCAST_CAPACITY).0,
            });

        entry.current = Some(frame.clone());
        entry.history.push(frame.clone());
        if entry.history.len() > MAX_HISTORY_FRAMES {
            let excess = entry.history.len() - MAX_HISTORY_FRAMES;
            entry.history.drain(..excess);
        }

        // Best-effort broadcast — ignore errors (no receivers is fine).
        let _ = entry.tx.send(frame.clone());

        Some(frame)
    }

    /// Get the current (most recent) frame for a canvas.
    pub fn snapshot(&self, canvas_id: &str) -> Option<CanvasFrame> {
        let store = self.inner.read();
        store.get(canvas_id).and_then(|entry| entry.current.clone())
    }

    /// Get the frame history for a canvas.
    pub fn history(&self, canvas_id: &str) -> Vec<CanvasFrame> {
        let store = self.inner.read();
        store
            .get(canvas_id)
            .map(|entry| entry.history.clone())
            .unwrap_or_default()
    }

    /// Clear a canvas (removes current content and history).
    pub fn clear(&self, canvas_id: &str) -> bool {
        let mut store = self.inner.write();
        if let Some(entry) = store.get_mut(canvas_id) {
            entry.current = None;
            entry.history.clear();
            // Send an empty frame to signal clear to subscribers.
            let clear_frame = CanvasFrame {
                frame_id: uuid::Uuid::new_v4().to_string(),
                content_type: "clear".to_string(),
                content: String::new(),
                timestamp: chrono::Utc::now().to_rfc3339(),
            };
            let _ = entry.tx.send(clear_frame);
            true
        } else {
            false
        }
    }

    /// Subscribe to real-time updates for a canvas.
    /// Creates the canvas entry if it does not exist (subject to canvas count limit).
    /// Returns `None` if the canvas does not exist and the limit has been reached.
    pub fn subscribe(&self, canvas_id: &str) -> Option<broadcast::Receiver<CanvasFrame>> {
        let mut store = self.inner.write();

        // Enforce canvas count limit for new entries.
        if !store.contains_key(canvas_id) && store.len() >= MAX_CANVAS_COUNT {
            return None;
        }

        let entry = store
            .entry(canvas_id.to_string())
            .or_insert_with(|| CanvasEntry {
                current: None,
                history: Vec::new(),
                tx: broadcast::channel(BROADCAST_CAPACITY).0,
            });
        Some(entry.tx.subscribe())
    }

    /// List all canvas IDs that currently have content.
    pub fn list(&self) -> Vec<String> {
        let store = self.inner.read();
        store.keys().cloned().collect()
    }
}

/// `CanvasTool` — agent-callable tool for the Live Canvas (A2UI) system.
pub struct CanvasTool {
    store: CanvasStore,
}

impl CanvasTool {
    pub fn new(store: CanvasStore) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for CanvasTool {
    fn name(&self) -> &str {
        "canvas"
    }

    fn description(&self) -> &str {
        "Push rendered content (HTML, SVG, Markdown) to a live web canvas that users can see \
         in real-time. Actions: render (push content), snapshot (get current content), \
         clear (reset canvas), eval (evaluate JS expression in canvas context). \
         Each canvas is identified by a canvas_id string."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "description": "Action to perform on the canvas.",
                    "enum": ["render", "snapshot", "clear", "eval"]
                },
                "canvas_id": {
                    "type": "string",
                    "description": "Unique identifier for the canvas. Defaults to 'default'."
                },
                "content_type": {
                    "type": "string",
                    "description": "Content type for render action: html, svg, markdown, or text.",
                    "enum": ["html", "svg", "markdown", "text"]
                },
                "content": {
                    "type": "string",
                    "description": "Content to render (for render action)."
                },
                "expression": {
                    "type": "string",
                    "description": "JavaScript expression to evaluate (for eval action). \
                        The result is returned as text. Evaluated client-side in the canvas iframe."
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let action = match args.get("action").and_then(|v| v.as_str()) {
            Some(a) => a,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("Missing required parameter: action".to_string()),
                });
            }
        };

        let canvas_id = args
            .get("canvas_id")
            .and_then(|v| v.as_str())
            .unwrap_or("default");

        match action {
            "render" => {
                let content_type = args
                    .get("content_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("html");

                let content = match args.get("content").and_then(|v| v.as_str()) {
                    Some(c) => c,
                    None => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some(
                                "Missing required parameter: content (for render action)"
                                    .to_string(),
                            ),
                        });
                    }
                };

                if content.len() > MAX_CONTENT_SIZE {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!(
                            "Content exceeds maximum size of {} bytes",
                            MAX_CONTENT_SIZE
                        )),
                    });
                }

                match self.store.render(canvas_id, content_type, content) {
                    Some(frame) => Ok(ToolResult {
                        success: true,
                        output: format!(
                            "Rendered {} content to canvas '{}' (frame: {})",
                            content_type, canvas_id, frame.frame_id
                        ),
                        error: None,
                    }),
                    None => Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!(
                            "Maximum canvas count ({}) reached. Clear unused canvases first.",
                            MAX_CANVAS_COUNT
                        )),
                    }),
                }
            }

            "snapshot" => match self.store.snapshot(canvas_id) {
                Some(frame) => Ok(ToolResult {
                    success: true,
                    output: serde_json::to_string_pretty(&frame)
                        .unwrap_or_else(|_| frame.content.clone()),
                    error: None,
                }),
                None => Ok(ToolResult {
                    success: true,
                    output: format!("Canvas '{}' is empty", canvas_id),
                    error: None,
                }),
            },

            "clear" => {
                let existed = self.store.clear(canvas_id);
                Ok(ToolResult {
                    success: true,
                    output: if existed {
                        format!("Canvas '{}' cleared", canvas_id)
                    } else {
                        format!("Canvas '{}' was already empty", canvas_id)
                    },
                    error: None,
                })
            }

            "eval" => {
                // Eval is handled client-side. We store an eval request as a special frame
                // that the web viewer interprets.
                let expression = match args.get("expression").and_then(|v| v.as_str()) {
                    Some(e) => e,
                    None => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some(
                                "Missing required parameter: expression (for eval action)"
                                    .to_string(),
                            ),
                        });
                    }
                };

                // Push a special eval frame so connected clients know to evaluate it.
                match self.store.render(canvas_id, "eval", expression) {
                    Some(frame) => Ok(ToolResult {
                        success: true,
                        output: format!(
                            "Eval request sent to canvas '{}' (frame: {}). \
                             Result will be available to connected viewers.",
                            canvas_id, frame.frame_id
                        ),
                        error: None,
                    }),
                    None => Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!(
                            "Maximum canvas count ({}) reached. Clear unused canvases first.",
                            MAX_CANVAS_COUNT
                        )),
                    }),
                }
            }

            other => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Unknown action: '{}'. Valid actions: render, snapshot, clear, eval",
                    other
                )),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canvas_store_render_and_snapshot() {
        let store = CanvasStore::new();
        let frame = store.render("test", "html", "<h1>Hello</h1>").unwrap();
        assert_eq!(frame.content_type, "html");
        assert_eq!(frame.content, "<h1>Hello</h1>");

        let snapshot = store.snapshot("test").unwrap();
        assert_eq!(snapshot.frame_id, frame.frame_id);
        assert_eq!(snapshot.content, "<h1>Hello</h1>");
    }

    #[test]
    fn canvas_store_snapshot_empty_returns_none() {
        let store = CanvasStore::new();
        assert!(store.snapshot("nonexistent").is_none());
    }

    #[test]
    fn canvas_store_clear_removes_content() {
        let store = CanvasStore::new();
        store.render("test", "html", "<p>content</p>");
        assert!(store.snapshot("test").is_some());

        let cleared = store.clear("test");
        assert!(cleared);
        assert!(store.snapshot("test").is_none());
    }

    #[test]
    fn canvas_store_clear_nonexistent_returns_false() {
        let store = CanvasStore::new();
        assert!(!store.clear("nonexistent"));
    }

    #[test]
    fn canvas_store_history_tracks_frames() {
        let store = CanvasStore::new();
        store.render("test", "html", "frame1");
        store.render("test", "html", "frame2");
        store.render("test", "html", "frame3");

        let history = store.history("test");
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].content, "frame1");
        assert_eq!(history[2].content, "frame3");
    }

    #[test]
    fn canvas_store_history_limit_enforced() {
        let store = CanvasStore::new();
        for i in 0..60 {
            store.render("test", "html", &format!("frame{i}"));
        }

        let history = store.history("test");
        assert_eq!(history.len(), MAX_HISTORY_FRAMES);
        // Oldest frames should have been dropped
        assert_eq!(history[0].content, "frame10");
    }

    #[test]
    fn canvas_store_list_returns_canvas_ids() {
        let store = CanvasStore::new();
        store.render("alpha", "html", "a");
        store.render("beta", "svg", "b");

        let mut ids = store.list();
        ids.sort();
        assert_eq!(ids, vec!["alpha", "beta"]);
    }

    #[test]
    fn canvas_store_subscribe_receives_updates() {
        let store = CanvasStore::new();
        let mut rx = store.subscribe("test").unwrap();
        store.render("test", "html", "<p>live</p>");

        let frame = rx.try_recv().unwrap();
        assert_eq!(frame.content, "<p>live</p>");
    }

    #[tokio::test]
    async fn canvas_tool_render_action() {
        let store = CanvasStore::new();
        let tool = CanvasTool::new(store.clone());
        let result = tool
            .execute(json!({
                "action": "render",
                "canvas_id": "test",
                "content_type": "html",
                "content": "<h1>Hello World</h1>"
            }))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("Rendered html content"));

        let snapshot = store.snapshot("test").unwrap();
        assert_eq!(snapshot.content, "<h1>Hello World</h1>");
    }

    #[tokio::test]
    async fn canvas_tool_snapshot_action() {
        let store = CanvasStore::new();
        store.render("test", "html", "<p>snap</p>");
        let tool = CanvasTool::new(store);
        let result = tool
            .execute(json!({"action": "snapshot", "canvas_id": "test"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("<p>snap</p>"));
    }

    #[tokio::test]
    async fn canvas_tool_snapshot_empty() {
        let store = CanvasStore::new();
        let tool = CanvasTool::new(store);
        let result = tool
            .execute(json!({"action": "snapshot", "canvas_id": "empty"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("empty"));
    }

    #[tokio::test]
    async fn canvas_tool_clear_action() {
        let store = CanvasStore::new();
        store.render("test", "html", "<p>clear me</p>");
        let tool = CanvasTool::new(store.clone());
        let result = tool
            .execute(json!({"action": "clear", "canvas_id": "test"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("cleared"));
        assert!(store.snapshot("test").is_none());
    }

    #[tokio::test]
    async fn canvas_tool_eval_action() {
        let store = CanvasStore::new();
        let tool = CanvasTool::new(store.clone());
        let result = tool
            .execute(json!({
                "action": "eval",
                "canvas_id": "test",
                "expression": "document.title"
            }))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("Eval request sent"));

        let snapshot = store.snapshot("test").unwrap();
        assert_eq!(snapshot.content_type, "eval");
        assert_eq!(snapshot.content, "document.title");
    }

    #[tokio::test]
    async fn canvas_tool_unknown_action() {
        let store = CanvasStore::new();
        let tool = CanvasTool::new(store);
        let result = tool.execute(json!({"action": "invalid"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("Unknown action"));
    }

    #[tokio::test]
    async fn canvas_tool_missing_action() {
        let store = CanvasStore::new();
        let tool = CanvasTool::new(store);
        let result = tool.execute(json!({})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("action"));
    }

    #[tokio::test]
    async fn canvas_tool_render_missing_content() {
        let store = CanvasStore::new();
        let tool = CanvasTool::new(store);
        let result = tool
            .execute(json!({"action": "render", "canvas_id": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("content"));
    }

    #[tokio::test]
    async fn canvas_tool_render_content_too_large() {
        let store = CanvasStore::new();
        let tool = CanvasTool::new(store);
        let big_content = "x".repeat(MAX_CONTENT_SIZE + 1);
        let result = tool
            .execute(json!({
                "action": "render",
                "canvas_id": "test",
                "content": big_content
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("maximum size"));
    }

    #[tokio::test]
    async fn canvas_tool_default_canvas_id() {
        let store = CanvasStore::new();
        let tool = CanvasTool::new(store.clone());
        let result = tool
            .execute(json!({
                "action": "render",
                "content_type": "html",
                "content": "<p>default</p>"
            }))
            .await
            .unwrap();
        assert!(result.success);
        assert!(store.snapshot("default").is_some());
    }

    #[test]
    fn canvas_store_enforces_max_canvas_count() {
        let store = CanvasStore::new();
        // Create MAX_CANVAS_COUNT canvases
        for i in 0..MAX_CANVAS_COUNT {
            assert!(
                store
                    .render(&format!("canvas_{i}"), "html", "content")
                    .is_some()
            );
        }
        // The next new canvas should be rejected
        assert!(store.render("one_too_many", "html", "content").is_none());
        // But rendering to an existing canvas should still work
        assert!(store.render("canvas_0", "html", "updated").is_some());
    }

    #[tokio::test]
    async fn canvas_tool_eval_missing_expression() {
        let store = CanvasStore::new();
        let tool = CanvasTool::new(store);
        let result = tool
            .execute(json!({"action": "eval", "canvas_id": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("expression"));
    }
}
