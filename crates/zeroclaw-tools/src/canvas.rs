//! Live Canvas (A2UI) tool — push rendered content to a web canvas in real time.

use crate::helpers::filesystem_boundary::write_file_atomic;
use async_trait::async_trait;
use cap_std::fs::Dir;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::broadcast;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};

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
    /// What the canvas shows, for a persistent store: the last frame rendered
    /// that was not an `eval` request, or `None` once cleared. Recorded under
    /// the store lock by the render or clear that changed it, so the saved
    /// copy never has to be worked out from `current` or the bounded history.
    displayed: Option<CanvasFrame>,
    tx: broadcast::Sender<CanvasFrame>,
}

/// Upper bound on a persisted canvas file. JSON string escaping can expand
/// content several times over, so this is deliberately loose; the decoded frame
/// is checked against `MAX_CONTENT_SIZE` after parsing.
const MAX_PERSISTED_FILE_BYTES: u64 = (MAX_CONTENT_SIZE as u64) * 8 + 4096;

/// What one canvas file holds.
#[derive(Deserialize)]
struct PersistedCanvas {
    canvas_id: String,
    frame: CanvasFrame,
}

#[derive(Serialize)]
struct PersistedCanvasRef<'a> {
    canvas_id: &'a str,
    frame: &'a CanvasFrame,
}

/// On-disk copy of each canvas's current frame, so what the user is looking at
/// survives a process restart. Only the current displayable frame is kept:
/// history stays in memory, and an `eval` frame is a request to a live viewer,
/// not content.
#[derive(Clone)]
struct CanvasPersistence {
    dir: Arc<Dir>,
    /// Per canvas, the frame id its file was last brought in line with:
    /// saved, or removed because the save failed. A canvas with nothing
    /// displayed has no entry. Holding this lock is also what makes disk
    /// operations one at a time: each one settles whatever the canvas shows
    /// when it runs, so the file converges on memory whichever render gets
    /// here first, without holding the store lock across file I/O.
    settled: Arc<parking_lot::Mutex<HashMap<String, String>>>,
}

impl CanvasPersistence {
    /// Canvas ids come from the model and from the REST path, so the file name
    /// is a digest of the id and never the id itself.
    fn file_name(canvas_id: &str) -> String {
        format!("{}.json", hex::encode(Sha256::digest(canvas_id.as_bytes())))
    }

    fn save(&self, canvas_id: &str, frame: &CanvasFrame) -> Result<(), String> {
        let bytes = serde_json::to_vec(&PersistedCanvasRef { canvas_id, frame })
            .map_err(|error| error.to_string())?;
        write_file_atomic(&self.dir, Path::new(&Self::file_name(canvas_id)), &bytes)
            .map_err(|error| error.to_string())
    }

    fn remove(&self, canvas_id: &str) -> Result<(), String> {
        match self.dir.remove_file(Self::file_name(canvas_id)) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.to_string()),
            _ => Ok(()),
        }
    }

    /// Whether `name` is a file name this store writes: a SHA-256 digest in
    /// hex plus `.json`.
    fn is_canvas_file_name(name: &str) -> bool {
        name.strip_suffix(".json").is_some_and(|stem| {
            stem.len() == 64 && stem.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
    }

    /// The most recently written frames this store saved that are still
    /// valid, up to the canvas limit. Anything else in the directory is
    /// ignored. Files are ranked by modification time before any is read, so
    /// at most the canvas limit of frames is ever held, however many files the
    /// directory has. Files beyond the limit are left where they are.
    fn load_all(&self) -> Vec<PersistedCanvas> {
        let Ok(entries) = self.dir.entries() else {
            return Vec::new();
        };
        let mut candidates = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !Self::is_canvas_file_name(name) {
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if !metadata.is_file() || metadata.len() > MAX_PERSISTED_FILE_BYTES {
                continue;
            }
            // A file whose time cannot be read is still a canvas; it ranks last.
            let modified = metadata.modified().unwrap_or_else(|_| {
                cap_std::time::SystemTime::from_std(std::time::SystemTime::UNIX_EPOCH)
            });
            candidates.push((modified, name.to_string()));
        }
        candidates.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));

        let mut loaded = Vec::new();
        for (_, name) in candidates {
            if loaded.len() >= MAX_CANVAS_COUNT {
                break;
            }
            let Ok(bytes) = self.dir.read(&name) else {
                continue;
            };
            let Ok(canvas) = serde_json::from_slice::<PersistedCanvas>(&bytes) else {
                continue;
            };
            // A file is trusted only under the name its own id maps to.
            if Self::file_name(&canvas.canvas_id) != name
                || canvas.frame.content.len() > MAX_CONTENT_SIZE
                || !ALLOWED_CONTENT_TYPES.contains(&canvas.frame.content_type.as_str())
            {
                continue;
            }
            loaded.push(canvas);
        }
        loaded
    }
}

/// Shared canvas store — holds all active canvases.
/// Thread-safe and cheaply cloneable (wraps `Arc`).
#[derive(Clone)]
pub struct CanvasStore {
    inner: Arc<RwLock<HashMap<String, CanvasEntry>>>,
    persistence: Option<CanvasPersistence>,
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
            persistence: None,
        }
    }

    /// A store whose canvases keep their current frame across process restarts,
    /// saved under `dir`. Without this a restart empties every canvas while the
    /// conversation that says "it is on the canvas" persists. Falls back to an
    /// in-memory store, with a warning, when the directory cannot be prepared.
    pub fn persistent(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        let opened = std::fs::create_dir_all(&dir).and_then(|()| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
            }
            Dir::open_ambient_dir(&dir, cap_std::ambient_authority())
        });
        let dir = match opened {
            Ok(dir) => dir,
            Err(error) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": error.to_string()})),
                    "canvas: persistence directory unavailable; canvases will not survive a restart"
                );
                return Self::new();
            }
        };
        let mut canvases = HashMap::new();
        let mut settled = HashMap::new();
        let persistence = CanvasPersistence {
            dir: Arc::new(dir),
            settled: Arc::new(parking_lot::Mutex::new(HashMap::new())),
        };
        for canvas in persistence.load_all() {
            settled.insert(canvas.canvas_id.clone(), canvas.frame.frame_id.clone());
            canvases.insert(
                canvas.canvas_id,
                CanvasEntry {
                    current: Some(canvas.frame.clone()),
                    history: vec![canvas.frame.clone()],
                    displayed: Some(canvas.frame),
                    tx: broadcast::channel(BROADCAST_CAPACITY).0,
                },
            );
        }
        *persistence.settled.lock() = settled;
        Self {
            inner: Arc::new(RwLock::new(canvases)),
            persistence: Some(persistence),
        }
    }

    /// The store a deployment uses: canvases saved under `<data_dir>/canvas`.
    pub fn for_data_dir(data_dir: &Path) -> Self {
        Self::persistent(data_dir.join("canvas"))
    }

    /// Bring the saved copy of one canvas in line with what it shows now.
    /// What it shows is recorded by the render or clear that changed it, so
    /// an `eval` request, which is not content, never decides what is saved:
    /// a render is saved even when an `eval` became the current frame before
    /// this ran, and a cleared canvas loses its file even when an `eval`
    /// recreated the entry. A frame is settled once, saved or not, so later
    /// `eval` requests never touch the disk. A canvas that shows nothing, and
    /// a frame that could not be saved, remove the file so a restart never
    /// brings back content that was replaced.
    fn sync_to_disk(&self, canvas_id: &str) {
        let Some(persistence) = &self.persistence else {
            return;
        };
        let mut settled = persistence.settled.lock();
        let unsettled = {
            let store = self.inner.read();
            match store
                .get(canvas_id)
                .and_then(|entry| entry.displayed.as_ref())
            {
                Some(frame) if settled.get(canvas_id) == Some(&frame.frame_id) => return,
                Some(frame) => Some(frame.clone()),
                None => None,
            }
        };
        let outcome = match unsettled {
            Some(frame) => {
                settled.insert(canvas_id.to_string(), frame.frame_id.clone());
                if ALLOWED_CONTENT_TYPES.contains(&frame.content_type.as_str()) {
                    persistence.save(canvas_id, &frame).inspect_err(|_| {
                        let _ = persistence.remove(canvas_id);
                    })
                } else {
                    persistence.remove(canvas_id)
                }
            }
            None => {
                settled.remove(canvas_id);
                persistence.remove(canvas_id)
            }
        };
        if let Err(error) = outcome {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"error": error})),
                "canvas: could not update the saved frame; this canvas will not survive a restart"
            );
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
                displayed: None,
                tx: broadcast::channel(BROADCAST_CAPACITY).0,
            });

        entry.current = Some(frame.clone());
        if self.persistence.is_some() && content_type != "eval" {
            entry.displayed = Some(frame.clone());
        }
        entry.history.push(frame.clone());
        if entry.history.len() > MAX_HISTORY_FRAMES {
            let excess = entry.history.len() - MAX_HISTORY_FRAMES;
            entry.history.drain(..excess);
        }

        // Best-effort broadcast — ignore errors (no receivers is fine).
        let _ = entry.tx.send(frame.clone());

        drop(store);
        self.sync_to_disk(canvas_id);

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

    /// Clear a canvas (removes current content and history). An entry nobody is
    /// subscribed to is dropped, so it stops counting against the canvas limit;
    /// a watched one keeps its channel for the next render.
    pub fn clear(&self, canvas_id: &str) -> bool {
        let existed = {
            let mut store = self.inner.write();
            let unwatched = match store.get_mut(canvas_id) {
                Some(entry) => {
                    entry.current = None;
                    entry.displayed = None;
                    entry.history.clear();
                    // Send an empty frame to signal clear to subscribers.
                    let clear_frame = CanvasFrame {
                        frame_id: uuid::Uuid::new_v4().to_string(),
                        content_type: "clear".to_string(),
                        content: String::new(),
                        timestamp: chrono::Utc::now().to_rfc3339(),
                    };
                    let _ = entry.tx.send(clear_frame);
                    Some(entry.tx.receiver_count() == 0)
                }
                None => None,
            };
            if unwatched == Some(true) {
                store.remove(canvas_id);
            }
            unwatched.is_some()
        };
        if existed {
            self.sync_to_disk(canvas_id);
        }
        existed
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
                displayed: None,
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
                    output: ToolOutput::default(),
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
                            output: ToolOutput::default(),
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
                        output: ToolOutput::default(),
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
                        )
                        .into(),
                        error: None,
                    }),
                    None => Ok(ToolResult {
                        success: false,
                        output: ToolOutput::default(),
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
                        .unwrap_or_else(|_| frame.content.clone())
                        .into(),
                    error: None,
                }),
                None => Ok(ToolResult {
                    success: true,
                    output: crate::i18n::get_required_tool_string_with_args(
                        "tool-canvas-snapshot-empty",
                        &[("canvas_id", canvas_id)],
                    )
                    .into(),
                    error: None,
                }),
            },

            "clear" => {
                let existed = self.store.clear(canvas_id);
                Ok(ToolResult {
                    success: true,
                    output: if existed {
                        format!("Canvas '{}' cleared", canvas_id).into()
                    } else {
                        format!("Canvas '{}' was already empty", canvas_id).into()
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
                            output: ToolOutput::default(),
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
                        )
                        .into(),
                        error: None,
                    }),
                    None => Ok(ToolResult {
                        success: false,
                        output: ToolOutput::default(),
                        error: Some(format!(
                            "Maximum canvas count ({}) reached. Clear unused canvases first.",
                            MAX_CANVAS_COUNT
                        )),
                    }),
                }
            }

            other => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
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

    #[tokio::test]
    async fn canvas_tool_renders_into_shared_store() {
        let gateway_store = CanvasStore::new();
        let tool = CanvasTool::new(gateway_store.clone());

        let result = tool
            .execute(json!({
                "action": "render",
                "canvas_id": "default",
                "content_type": "html",
                "content": "<h1>from chat</h1>",
            }))
            .await
            .unwrap();
        assert!(result.success, "render failed: {:?}", result.error);

        let seen = gateway_store
            .snapshot("default")
            .expect("frame must be visible via the shared gateway store");
        assert_eq!(seen.content, "<h1>from chat</h1>");
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
        assert!(result.output.contains("has no content"));
        assert!(result.output.contains("Render it again"));
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

    #[test]
    fn persistent_store_restores_the_current_frame_after_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let first = CanvasStore::persistent(dir.path());
        first.render("report", "html", "<p>v1</p>").unwrap();
        let shown = first.render("report", "html", "<p>v2</p>").unwrap();
        drop(first);

        let second = CanvasStore::persistent(dir.path());
        let frame = second.snapshot("report").expect("the canvas survives");
        assert_eq!(frame.content, "<p>v2</p>");
        assert_eq!(frame.frame_id, shown.frame_id);
        assert_eq!(second.list(), vec!["report".to_string()]);
        assert_eq!(
            second.history("report").len(),
            1,
            "only the current frame is kept on disk"
        );
    }

    #[test]
    fn persistent_store_forgets_a_cleared_canvas() {
        let dir = tempfile::tempdir().unwrap();
        let first = CanvasStore::persistent(dir.path());
        first.render("report", "html", "<p>v1</p>").unwrap();
        assert!(first.clear("report"));
        drop(first);

        let second = CanvasStore::persistent(dir.path());
        assert!(second.snapshot("report").is_none());
        assert!(second.list().is_empty());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn persistent_store_keeps_the_last_displayable_frame_when_an_eval_follows() {
        let dir = tempfile::tempdir().unwrap();
        let first = CanvasStore::persistent(dir.path());
        first.render("report", "markdown", "# shown").unwrap();
        first.render("report", "eval", "document.title").unwrap();
        drop(first);

        let second = CanvasStore::persistent(dir.path());
        let frame = second.snapshot("report").expect("the canvas survives");
        assert_eq!(frame.content_type, "markdown");
        assert_eq!(frame.content, "# shown");
    }

    /// Replace a canvas file's bytes without the store knowing, so a test can
    /// tell whether a later operation wrote the file again.
    fn overwrite_with_sentinel(dir: &Path, canvas_id: &str) -> PathBuf {
        let file = dir.join(CanvasPersistence::file_name(canvas_id));
        std::fs::write(&file, b"sentinel").unwrap();
        file
    }

    #[test]
    fn a_render_overtaken_by_an_eval_is_still_saved() {
        // A render and an `eval` can arrive close enough together that the
        // `eval` is already the current frame when the render's disk sync
        // runs. Reproduce that state: the render is in memory but not on
        // disk, and the sync that runs sees `eval` as current.
        let dir = tempfile::tempdir().unwrap();
        let store = CanvasStore::persistent(dir.path());
        store.render("report", "markdown", "# shown").unwrap();
        let persistence = store.persistence.as_ref().unwrap();
        persistence.remove("report").unwrap();
        persistence.settled.lock().clear();

        store.render("report", "eval", "document.title").unwrap();
        drop(store);

        let restarted = CanvasStore::persistent(dir.path());
        let frame = restarted.snapshot("report").expect("the render was saved");
        assert_eq!(frame.content_type, "markdown");
        assert_eq!(frame.content, "# shown");
    }

    #[test]
    fn a_clear_overtaken_by_an_eval_still_removes_the_file() {
        // The mirror case: the clear dropped the entry, an `eval` recreated
        // it, and only then does a disk sync run.
        let dir = tempfile::tempdir().unwrap();
        let store = CanvasStore::persistent(dir.path());
        store.render("report", "markdown", "# shown").unwrap();
        let file = dir.path().join(CanvasPersistence::file_name("report"));
        {
            let mut canvases = store.inner.write();
            canvases.remove("report");
        }
        assert!(file.exists(), "the clear's own sync has not run yet");

        store.render("report", "eval", "document.title").unwrap();
        assert!(!file.exists());
        drop(store);

        let restarted = CanvasStore::persistent(dir.path());
        assert!(restarted.snapshot("report").is_none());
    }

    #[test]
    fn an_eval_does_not_rewrite_a_frame_that_is_already_saved() {
        let dir = tempfile::tempdir().unwrap();
        let store = CanvasStore::persistent(dir.path());
        store.render("report", "markdown", "# shown").unwrap();
        let file = overwrite_with_sentinel(dir.path(), "report");

        store.render("report", "eval", "document.title").unwrap();

        assert_eq!(std::fs::read(&file).unwrap(), b"sentinel");
    }

    #[test]
    fn a_restored_frame_counts_as_saved() {
        let dir = tempfile::tempdir().unwrap();
        CanvasStore::persistent(dir.path())
            .render("report", "markdown", "# shown")
            .unwrap();

        let restarted = CanvasStore::persistent(dir.path());
        let file = overwrite_with_sentinel(dir.path(), "report");
        restarted
            .render("report", "eval", "document.title")
            .unwrap();

        assert_eq!(std::fs::read(&file).unwrap(), b"sentinel");
    }

    #[test]
    fn startup_keeps_the_newest_frames_up_to_the_canvas_limit() {
        let dir = tempfile::tempdir().unwrap();
        let store = CanvasStore::persistent(dir.path());
        let persistence = store.persistence.as_ref().unwrap();
        let aged = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        for index in 0..MAX_CANVAS_COUNT + 5 {
            let canvas_id = format!("canvas-{index}");
            let frame = CanvasFrame {
                frame_id: format!("frame-{index}"),
                content_type: "text".to_string(),
                content: format!("canvas {index}"),
                timestamp: chrono::Utc::now().to_rfc3339(),
            };
            persistence.save(&canvas_id, &frame).unwrap();
            if index < 5 {
                std::fs::File::options()
                    .write(true)
                    .open(dir.path().join(CanvasPersistence::file_name(&canvas_id)))
                    .unwrap()
                    .set_modified(aged)
                    .unwrap();
            }
        }
        drop(store);

        let restarted = CanvasStore::persistent(dir.path());
        let kept = restarted.list();
        assert_eq!(kept.len(), MAX_CANVAS_COUNT);
        for index in 0..5 {
            assert!(
                !kept.contains(&format!("canvas-{index}")),
                "the oldest files fall outside the limit"
            );
        }
    }

    #[test]
    fn only_files_this_store_names_are_read_at_startup() {
        assert!(CanvasPersistence::is_canvas_file_name(
            &CanvasPersistence::file_name("report")
        ));
        assert!(!CanvasPersistence::is_canvas_file_name("report.json"));
        assert!(!CanvasPersistence::is_canvas_file_name(
            "zz3f0c1a9b0e7d55a1c2b3d4e5f60718293a4b5c6d7e8f9012345678901234ab.json"
        ));
        assert!(!CanvasPersistence::is_canvas_file_name(
            "aa3f0c1a9b0e7d55a1c2b3d4e5f60718293a4b5c6d7e8f9012345678901234ab.tmp"
        ));
    }

    #[test]
    fn persistent_store_never_uses_the_canvas_id_as_a_path() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("canvas");
        let hostile = "../../outside/evil";
        let first = CanvasStore::persistent(&dir);
        first.render(hostile, "text", "contained").unwrap();
        drop(first);

        assert!(!root.path().join("outside").exists());
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 1, "{names:?}");
        let stem = names[0].strip_suffix(".json").expect("a .json file");
        assert_eq!(stem.len(), 64, "{names:?}");
        assert!(stem.chars().all(|c| c.is_ascii_hexdigit()), "{names:?}");

        let second = CanvasStore::persistent(&dir);
        assert_eq!(second.snapshot(hostile).unwrap().content, "contained");
    }

    #[test]
    fn persistent_store_ignores_files_it_did_not_write() {
        let dir = tempfile::tempdir().unwrap();
        let first = CanvasStore::persistent(dir.path());
        first.render("kept", "text", "real").unwrap();
        drop(first);

        std::fs::write(dir.path().join("notes.json"), b"not a canvas").unwrap();
        // A well-formed frame under a name its id does not map to.
        let stray = serde_json::json!({
            "canvas_id": "smuggled",
            "frame": {"frame_id": "f", "content_type": "html", "content": "x", "timestamp": "2026-01-01T00:00:00+00:00"}
        });
        std::fs::write(dir.path().join("stray.json"), stray.to_string()).unwrap();
        // The right name, but a frame type that is never displayed.
        let eval = serde_json::json!({
            "canvas_id": "evaluated",
            "frame": {"frame_id": "f", "content_type": "eval", "content": "x", "timestamp": "2026-01-01T00:00:00+00:00"}
        });
        std::fs::write(
            dir.path().join(CanvasPersistence::file_name("evaluated")),
            eval.to_string(),
        )
        .unwrap();

        let second = CanvasStore::persistent(dir.path());
        assert_eq!(second.list(), vec!["kept".to_string()]);
    }

    #[test]
    fn a_frame_that_is_not_saved_does_not_leave_the_previous_one_behind() {
        let dir = tempfile::tempdir().unwrap();
        let first = CanvasStore::persistent(dir.path());
        first.render("report", "html", "<p>v1</p>").unwrap();
        // The tool accepts content types the REST allow-list does not; such a
        // frame replaces v1 in memory, so v1 must not come back after a restart.
        first
            .render("report", "mermaid", "graph TD; a-->b")
            .unwrap();
        drop(first);

        let second = CanvasStore::persistent(dir.path());
        assert!(second.snapshot("report").is_none());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn clear_frees_the_canvas_slot_unless_someone_is_watching() {
        let store = CanvasStore::new();
        store.render("unwatched", "text", "a").unwrap();
        assert!(store.clear("unwatched"));
        assert!(
            store.list().is_empty(),
            "a cleared canvas nobody watches is dropped"
        );

        store.render("watched", "text", "b").unwrap();
        let mut viewer = store.subscribe("watched").unwrap();
        assert!(store.clear("watched"));
        assert_eq!(store.list(), vec!["watched".to_string()]);
        assert_eq!(viewer.try_recv().unwrap().content_type, "clear");
        drop(viewer);
        assert!(store.clear("watched"));

        for i in 0..MAX_CANVAS_COUNT {
            store.render(&format!("fill-{i}"), "text", "x").unwrap();
        }
        assert!(store.render("one-more", "text", "x").is_none());
        assert!(store.clear("fill-0"));
        assert!(
            store.render("one-more", "text", "x").is_some(),
            "clearing an unwatched canvas makes room for a new one"
        );
    }

    #[cfg(unix)]
    #[test]
    fn persisted_frames_are_readable_by_the_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = CanvasStore::persistent(dir.path().join("canvas"));
        store.render("report", "html", "<p>private</p>").unwrap();

        let saved = std::fs::read_dir(dir.path().join("canvas"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(
            saved.metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
        let dir_mode = std::fs::metadata(dir.path().join("canvas"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(dir_mode & 0o777, 0o700);
    }

    #[test]
    fn in_memory_store_writes_nothing() {
        let store = CanvasStore::new();
        store.render("report", "html", "<p>v1</p>").unwrap();
        assert!(store.persistence.is_none());
    }
}
