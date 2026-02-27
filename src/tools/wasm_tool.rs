//! WASM plugin tool — executes a `.wasm` binary as a ZeroClaw tool.
//!
//! # Feature gate
//! Only compiled when `--features wasm-tools` is active.
//! Without the feature, [`WasmTool`] stubs return a clear error.
//!
//! # Protocol (WASI stdio)
//!
//! The WASM module communicates via standard WASI stdin / stdout:
//!
//! ```text
//! Host → stdin  : UTF-8 JSON of the tool args (from LLM)
//! Host ← stdout : UTF-8 JSON of ToolResult
//! ```
//!
//! Expected stdout shape:
//! ```json
//! { "success": true, "output": "...", "error": null }
//! ```
//!
//! This means **any language** that can read stdin / write stdout works:
//! TypeScript (Javy), Rust (wasm32-wasip1), Go (TinyGo), Python (componentize-py), etc.
//! No custom SDK or ABI boilerplate required.
//!
//! # Security
//! - No filesystem preopened dirs (deny-by-default).
//! - No network sockets (WASI sockets not enabled).
//! - Execution time capped via wasmtime epoch interruption: a 1 Hz ticker
//!   thread advances the epoch each second; the WASM store's deadline is set to
//!   [`WASM_TIMEOUT_SECS`] epochs so runaway modules are preempted without
//!   relying on OS-level process signals.
//! - Output capped at 1 MiB (enforced by [`MemoryOutputPipe`] capacity).

use super::traits::{Tool, ToolResult};
use anyhow::{bail, Context};
use async_trait::async_trait;
use serde_json::Value;
use std::path::Path;

/// Maximum tool output size (1 MiB).
const MAX_OUTPUT_BYTES: usize = 1_048_576;

/// Wall-clock timeout for a single WASM invocation.
const WASM_TIMEOUT_SECS: u64 = 30;

// ─── Feature-gated implementation ─────────────────────────────────────────────

#[cfg(feature = "wasm-tools")]
mod inner {
    use super::{
        async_trait, bail, Context, Path, Tool, ToolResult, Value, MAX_OUTPUT_BYTES,
        WASM_TIMEOUT_SECS,
    };
    use wasmtime::{Config as WtConfig, Engine, Linker, Module, Store};
    use wasmtime_wasi::{
        pipe::{MemoryInputPipe, MemoryOutputPipe},
        preview1::{self, WasiP1Ctx},
        WasiCtxBuilder,
    };

    pub struct WasmTool {
        name: String,
        description: String,
        parameters_schema: Value,
        engine: Engine,
        module: Module,
        /// Guards against concurrent invocations: epoch tickers from concurrent
        /// calls would advance the shared engine epoch at a multiple of 1 Hz,
        /// causing premature timeouts.
        is_running: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    impl WasmTool {
        pub fn load(
            path: &Path,
            name: String,
            description: String,
            parameters_schema: Value,
        ) -> anyhow::Result<Self> {
            let mut cfg = WtConfig::new();
            cfg.epoch_interruption(true);

            let engine = Engine::new(&cfg).context("failed to create WASM engine")?;

            let bytes = std::fs::read(path)
                .with_context(|| format!("cannot read WASM file: {}", path.display()))?;
            let module = Module::new(&engine, &bytes)
                .with_context(|| format!("cannot compile WASM module: {}", path.display()))?;

            Ok(Self {
                name,
                description,
                parameters_schema,
                engine,
                module,
                is_running: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            })
        }

        fn invoke_sync(&self, args: &Value) -> anyhow::Result<ToolResult> {
            let input_bytes = serde_json::to_vec(args)?;

            let stdout_pipe = MemoryOutputPipe::new(MAX_OUTPUT_BYTES);
            let stdout_for_read = stdout_pipe.clone();

            let wasi_ctx: WasiP1Ctx = WasiCtxBuilder::new()
                .stdin(MemoryInputPipe::new(input_bytes))
                .stdout(stdout_pipe)
                .build_p1();

            let mut store = Store::new(&self.engine, wasi_ctx);
            // epoch_deadline is in ticks; the incrementer thread below fires at 1 Hz.
            store.set_epoch_deadline(WASM_TIMEOUT_SECS);

            let mut linker: Linker<WasiP1Ctx> = Linker::new(&self.engine);
            preview1::add_to_linker_sync(&mut linker, |ctx| ctx)
                .context("failed to add WASI to linker")?;

            let instance = linker.instantiate(&mut store, &self.module)?;

            // Spawn a background thread that increments the epoch every second.
            // When the deadline is reached wasmtime returns a trap, unblocking
            // the call below.
            let engine_for_ticker = self.engine.clone();
            let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
            let ticker = std::thread::spawn(move || {
                while stop_rx
                    .recv_timeout(std::time::Duration::from_secs(1))
                    .is_err()
                {
                    engine_for_ticker.increment_epoch();
                }
            });

            let call_result = instance
                .get_typed_func::<(), ()>(&mut store, "_start")
                .context("WASM module must export '_start' (compile as a WASI binary)")
                .and_then(|start| {
                    start
                        .call(&mut store, ())
                        .context("WASM execution failed or timed out")
                });

            // Stop the epoch ticker regardless of outcome.
            let _ = stop_tx.send(());
            let _ = ticker.join();

            call_result?;

            let raw = stdout_for_read.contents().to_vec();
            if raw.is_empty() {
                bail!("WASM tool wrote nothing to stdout");
            }
            // Note: MemoryOutputPipe::new(MAX_OUTPUT_BYTES) already caps writes
            // at construction time, so no separate size check is needed here.

            serde_json::from_slice::<ToolResult>(&raw)
                .context("WASM tool stdout is not valid ToolResult JSON")
        }
    }

    #[async_trait]
    impl Tool for WasmTool {
        fn name(&self) -> &str {
            &self.name
        }
        fn description(&self) -> &str {
            &self.description
        }
        fn parameters_schema(&self) -> Value {
            self.parameters_schema.clone()
        }

        async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
            use std::sync::atomic::Ordering;

            // Prevent concurrent invocations: two simultaneous tickers would
            // advance the shared engine epoch at 2 Hz, halving the timeout.
            if self
                .is_running
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                bail!(
                    "WASM tool '{}' is already running; concurrent invocations are not supported",
                    self.name
                );
            }

            // Clone fields needed inside the blocking closure.
            // Engine and Module are cheaply Arc-backed clones.
            let name = self.name.clone();
            let engine = self.engine.clone();
            let module = self.module.clone();
            let schema = self.parameters_schema.clone();
            let desc = self.description.clone();
            let is_running = self.is_running.clone();

            tokio::task::spawn_blocking(move || {
                let tool = WasmTool {
                    name,
                    description: desc,
                    parameters_schema: schema,
                    engine,
                    module,
                    is_running: is_running.clone(),
                };
                let result = tool
                    .invoke_sync(&args)
                    .with_context(|| format!("WASM tool '{}' execution failed", tool.name));
                is_running.store(false, Ordering::Release);
                result
            })
            .await
            .context("WASM blocking task panicked")?
        }
    }

    pub use WasmTool as WasmToolImpl;
}

// ─── Feature-absent stub ──────────────────────────────────────────────────────

#[cfg(not(feature = "wasm-tools"))]
mod inner {
    use super::*;

    /// Stub: returned when the `wasm-tools` feature is not compiled in.
    /// Construction succeeds so callers can enumerate plugins; execution returns a clear error.
    pub struct WasmTool {
        name: String,
        description: String,
        parameters_schema: Value,
    }

    impl WasmTool {
        pub fn load(
            _path: &Path,
            name: String,
            description: String,
            parameters_schema: Value,
        ) -> anyhow::Result<Self> {
            Ok(Self {
                name,
                description,
                parameters_schema,
            })
        }
    }

    #[async_trait]
    impl Tool for WasmTool {
        fn name(&self) -> &str {
            &self.name
        }
        fn description(&self) -> &str {
            &self.description
        }
        fn parameters_schema(&self) -> Value {
            self.parameters_schema.clone()
        }

        async fn execute(&self, _args: Value) -> anyhow::Result<ToolResult> {
            Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    "WASM tools are not enabled in this build. \
                     Recompile with '--features wasm-tools'."
                        .into(),
                ),
            })
        }
    }

    pub use WasmTool as WasmToolImpl;
}

// ─── Public re-export ─────────────────────────────────────────────────────────

pub use inner::WasmToolImpl as WasmTool;

// ─── Manifest ────────────────────────────────────────────────────────────────

/// The `manifest.json` file that accompanies every WASM tool.
///
/// Stored at:
/// - Dev layout:       `<skill-dir>/manifest.json`
/// - Installed layout: `<skill-dir>/tools/<tool-name>/manifest.json`
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WasmManifest {
    /// Tool name exposed to the LLM (snake_case, e.g. `my_weather_tool`).
    pub name: String,
    /// Human-readable description shown to the LLM.
    pub description: String,
    /// JSON Schema for the tool's parameters.
    pub parameters: Value,
    /// Manifest format version (currently `"1"`).
    #[serde(default = "default_manifest_version")]
    pub version: String,
    /// Optional homepage / source URL (shown in `zeroclaw skill list`).
    #[serde(default)]
    pub homepage: Option<String>,
}

fn default_manifest_version() -> String {
    "1".to_string()
}

impl WasmManifest {
    pub fn load_from(path: &Path) -> anyhow::Result<Self> {
        let bytes = std::fs::read(path)
            .with_context(|| format!("cannot read manifest: {}", path.display()))?;
        serde_json::from_slice(&bytes)
            .with_context(|| format!("invalid manifest JSON: {}", path.display()))
    }
}

// ─── Loader ──────────────────────────────────────────────────────────────────

/// Scan the skills directory and load any WASM tools found.
///
/// Supports two layouts:
///
/// **Installed layout** (from `zeroclaw skill install`):
/// ```text
/// skills/<skill-name>/tools/<tool-name>/tool.wasm
/// skills/<skill-name>/tools/<tool-name>/manifest.json
/// ```
///
/// **Dev layout** (direct from `zeroclaw skill install ./my-tool`):
/// ```text
/// skills/<skill-name>/tool.wasm
/// skills/<skill-name>/manifest.json
/// ```
pub fn load_wasm_tools_from_skills(skills_dir: &std::path::Path) -> Vec<Box<dyn Tool>> {
    let mut tools: Vec<Box<dyn Tool>> = Vec::new();

    let entries = match std::fs::read_dir(skills_dir) {
        Ok(e) => e,
        Err(_) => return tools,
    };

    for entry in entries.flatten() {
        let skill_dir = entry.path();

        // Dev layout: tool.wasm + manifest.json at skill root
        let wasm = skill_dir.join("tool.wasm");
        let manifest_path = skill_dir.join("manifest.json");
        if wasm.exists() && manifest_path.exists() {
            load_single_tool(&wasm, &manifest_path, &mut tools);
            continue;
        }

        // Installed layout: tools/<name>/tool.wasm
        let tools_subdir = skill_dir.join("tools");
        if let Ok(tool_entries) = std::fs::read_dir(&tools_subdir) {
            for tool_entry in tool_entries.flatten() {
                let tool_dir = tool_entry.path();
                let wasm = tool_dir.join("tool.wasm");
                let manifest_path = tool_dir.join("manifest.json");
                if wasm.exists() && manifest_path.exists() {
                    load_single_tool(&wasm, &manifest_path, &mut tools);
                }
            }
        }
    }

    tools
}

/// Collect the tool names declared by installed WASM skill packages by reading
/// only the `manifest.json` files — no WASM module is compiled or loaded.
///
/// Used to pre-populate `auto_approve` for the channel approval manager so that
/// sandboxed WASM skills are not denied when running on non-CLI channels.
pub fn wasm_tool_names_from_skills(skills_dir: &std::path::Path) -> Vec<String> {
    let mut names = Vec::new();

    let entries = match std::fs::read_dir(skills_dir) {
        Ok(e) => e,
        Err(_) => return names,
    };

    for entry in entries.flatten() {
        let skill_dir = entry.path();

        // Dev layout: manifest.json at skill root
        let manifest_path = skill_dir.join("manifest.json");
        if manifest_path.exists() {
            if let Ok(m) = WasmManifest::load_from(&manifest_path) {
                if !m.name.is_empty() {
                    names.push(m.name);
                }
            }
            continue;
        }

        // Installed layout: tools/<name>/manifest.json
        let tools_subdir = skill_dir.join("tools");
        if let Ok(tool_entries) = std::fs::read_dir(&tools_subdir) {
            for tool_entry in tool_entries.flatten() {
                let manifest_path = tool_entry.path().join("manifest.json");
                if manifest_path.exists() {
                    if let Ok(m) = WasmManifest::load_from(&manifest_path) {
                        if !m.name.is_empty() {
                            names.push(m.name);
                        }
                    }
                }
            }
        }
    }

    names
}

fn load_single_tool(
    wasm: &std::path::Path,
    manifest_path: &std::path::Path,
    out: &mut Vec<Box<dyn Tool>>,
) {
    let manifest = match WasmManifest::load_from(manifest_path) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(path = %manifest_path.display(), error = %e, "skipping WASM tool: bad manifest");
            return;
        }
    };

    // Validate manifest.name: snake_case only (lowercase letters, digits,
    // underscores), non-empty, max 64 chars (matches function-calling API limits).
    let name_ok = !manifest.name.is_empty()
        && manifest.name.len() <= 64
        && manifest
            .name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !name_ok {
        tracing::warn!(
            path = %manifest_path.display(),
            name = %manifest.name,
            "skipping WASM tool: invalid name (must be snake_case, max 64 chars)"
        );
        return;
    }

    match WasmTool::load(
        wasm,
        manifest.name.clone(),
        manifest.description.clone(),
        manifest.parameters.clone(),
    ) {
        Ok(t) => {
            tracing::debug!(name = %manifest.name, "loaded WASM tool");
            out.push(Box::new(t));
        }
        Err(e) => {
            tracing::warn!(
                name = %manifest.name,
                wasm = %wasm.display(),
                error = %e,
                "skipping WASM tool: failed to load"
            );
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    #[allow(clippy::wildcard_imports)]
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn manifest_round_trips() {
        let json = serde_json::json!({
            "name": "zeroclaw_test_tool",
            "description": "Test tool",
            "parameters": { "type": "object", "properties": {} }
        });
        let m: WasmManifest = serde_json::from_value(json).unwrap();
        assert_eq!(m.name, "zeroclaw_test_tool");
        assert_eq!(m.version, "1");
        assert!(m.homepage.is_none());
    }

    #[test]
    fn load_from_empty_dir_returns_empty() {
        let tools = load_wasm_tools_from_skills(std::path::Path::new(
            "/tmp/zeroclaw_wasm_test_nonexistent_xyz",
        ));
        assert!(tools.is_empty());
    }

    #[cfg(not(feature = "wasm-tools"))]
    #[tokio::test]
    async fn stub_reports_feature_disabled() {
        let t = WasmTool::load(
            &PathBuf::from("/dev/null"),
            "zeroclaw_test_stub".into(),
            "stub".into(),
            serde_json::json!({}),
        )
        .unwrap();
        let r = t.execute(serde_json::json!({})).await.unwrap();
        assert!(!r.success);
        assert!(r.error.unwrap().contains("wasm-tools"));
    }

    // ── WasmManifest error paths ──────────────────────────────────────────────

    #[test]
    fn manifest_load_from_missing_file_returns_error() {
        let result = WasmManifest::load_from(&PathBuf::from(
            "/nonexistent_zeroclaw_test_dir/manifest.json",
        ));
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("cannot read manifest"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn manifest_load_from_invalid_json_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("manifest.json");
        std::fs::write(&path, b"not valid json {{{{").unwrap();
        let result = WasmManifest::load_from(&path);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("invalid manifest JSON"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn manifest_with_optional_fields_parsed() {
        let json = serde_json::json!({
            "name": "zeroclaw_optional_test",
            "description": "Tool with all optional fields",
            "parameters": { "type": "object", "properties": {} },
            "version": "2",
            "homepage": "https://example.com/zeroclaw_optional_test"
        });
        let m: WasmManifest = serde_json::from_value(json).unwrap();
        assert_eq!(m.version, "2");
        assert_eq!(
            m.homepage.as_deref(),
            Some("https://example.com/zeroclaw_optional_test")
        );
    }

    // ── load_wasm_tools_from_skills: skip / layout detection ─────────────────

    #[test]
    fn load_wasm_tools_skips_dir_missing_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("zeroclaw_test_skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        // tool.wasm present but no manifest.json — should be skipped silently
        std::fs::write(skill_dir.join("tool.wasm"), b"\x00asm\x01\x00\x00\x00").unwrap();
        let tools = load_wasm_tools_from_skills(dir.path());
        assert!(tools.is_empty());
    }

    #[test]
    fn load_wasm_tools_skips_dir_missing_wasm() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("zeroclaw_test_skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        // manifest.json present but no tool.wasm — dev layout check fails
        std::fs::write(
            skill_dir.join("manifest.json"),
            serde_json::json!({
                "name": "zeroclaw_test_tool",
                "description": "test",
                "parameters": {}
            })
            .to_string(),
        )
        .unwrap();
        let tools = load_wasm_tools_from_skills(dir.path());
        assert!(tools.is_empty());
    }

    #[test]
    fn load_wasm_tools_skips_bad_manifest_json() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("zeroclaw_test_skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("tool.wasm"), b"\x00asm\x01\x00\x00\x00").unwrap();
        std::fs::write(skill_dir.join("manifest.json"), b"not valid json").unwrap();
        let tools = load_wasm_tools_from_skills(dir.path());
        assert!(tools.is_empty(), "bad manifest should be skipped");
    }

    #[test]
    fn load_wasm_tools_installed_layout_skips_bad_manifest() {
        // installed layout: skills/<pkg>/tools/<tool>/{tool.wasm, manifest.json}
        let dir = tempfile::tempdir().unwrap();
        let tool_dir = dir
            .path()
            .join("zeroclaw_test_pkg")
            .join("tools")
            .join("zeroclaw_test_func");
        std::fs::create_dir_all(&tool_dir).unwrap();
        std::fs::write(tool_dir.join("tool.wasm"), b"\x00asm\x01\x00\x00\x00").unwrap();
        std::fs::write(tool_dir.join("manifest.json"), b"{ invalid }").unwrap();
        let tools = load_wasm_tools_from_skills(dir.path());
        assert!(
            tools.is_empty(),
            "bad installed-layout manifest should be skipped"
        );
    }

    #[test]
    fn load_wasm_tools_ignores_plain_files_in_skills_root() {
        let dir = tempfile::tempdir().unwrap();
        // A file at the skills root — not a directory, must be ignored
        std::fs::write(dir.path().join("not-a-skill.txt"), b"noise").unwrap();
        let tools = load_wasm_tools_from_skills(dir.path());
        assert!(tools.is_empty());
    }

    // ── Feature-gated: invalid WASM binary fails at compile time ─────────────

    #[cfg(feature = "wasm-tools")]
    #[test]
    #[ignore = "slow: initializes wasmtime Cranelift compiler; run with --include-ignored"]
    fn wasm_tool_load_rejects_invalid_binary() {
        let dir = tempfile::tempdir().unwrap();
        let wasm_path = dir.path().join("tool.wasm");
        std::fs::write(&wasm_path, b"this is not a valid wasm binary").unwrap();
        let result = WasmTool::load(
            &wasm_path,
            "zeroclaw_invalid_test".into(),
            "desc".into(),
            serde_json::json!({}),
        );
        assert!(result.is_err());
        let msg = result.err().unwrap().to_string();
        assert!(
            msg.contains("cannot compile WASM module"),
            "unexpected error: {msg}"
        );
    }

    #[cfg(feature = "wasm-tools")]
    #[test]
    #[ignore = "slow: initializes wasmtime Cranelift compiler; run with --include-ignored"]
    fn wasm_tool_load_rejects_missing_file() {
        let result = WasmTool::load(
            &PathBuf::from("/nonexistent_zeroclaw_test_wasm/tool.wasm"),
            "zeroclaw_missing_test".into(),
            "desc".into(),
            serde_json::json!({}),
        );
        assert!(result.is_err());
        let msg = result.err().unwrap().to_string();
        assert!(
            msg.contains("cannot read WASM file"),
            "unexpected error: {msg}"
        );
    }
}
