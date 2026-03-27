//! WASM sandbox runtime — in-process tool isolation.
//!
//! This runtime is intentionally narrow:
//! - runs a single `.wasm` module per invocation
//! - enforces fuel and memory limits
//! - has no shell access
//! - does not expose arbitrary host syscalls
//!
//! The first implementation only supports pure modules that export either:
//! - `run() -> i32`
//! - `_start()`
//!
//! That matches the immediate goal: make `runtime.kind = "wasm"` real in the
//! main runtime factory without pretending that the whole ZeroClaw process can
//! already live inside WASM.
//!
//! # Feature gate
//! This module is only compiled when `--features runtime-wasm` is enabled.
//! The default ZeroClaw binary still excludes it unless the feature is
//! requested explicitly.

use super::traits::RuntimeAdapter;
use crate::config::WasmRuntimeConfig;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct WasmRuntime {
    config: WasmRuntimeConfig,
    workspace_dir: Option<PathBuf>,
}

/// Result of executing a WASM module.
#[derive(Debug, Clone)]
pub struct WasmExecutionResult {
    /// Standard output captured from the module.
    pub stdout: String,
    /// Standard error captured from the module.
    pub stderr: String,
    /// Exit code (0 = success).
    pub exit_code: i32,
    /// Fuel consumed during execution.
    pub fuel_consumed: u64,
}

/// Capabilities granted to a WASM tool module.
#[derive(Debug, Clone, Default)]
pub struct WasmCapabilities {
    /// Allow reading files from workspace.
    pub read_workspace: bool,
    /// Allow writing files to workspace.
    pub write_workspace: bool,
    /// Allowed HTTP hosts (reserved for a future host-mediated network bridge).
    pub allowed_hosts: Vec<String>,
    /// Custom fuel override (0 = use config default).
    pub fuel_override: u64,
    /// Custom memory override in MB (0 = use config default).
    pub memory_override_mb: u64,
}

impl WasmRuntime {
    /// Create a new WASM runtime with the given configuration.
    pub fn new(config: WasmRuntimeConfig) -> Self {
        Self {
            config,
            workspace_dir: None,
        }
    }

    /// Create a WASM runtime bound to a specific workspace directory.
    pub fn with_workspace(config: WasmRuntimeConfig, workspace_dir: PathBuf) -> Self {
        Self {
            config,
            workspace_dir: Some(workspace_dir),
        }
    }

    /// Check if the WASM runtime feature is available in this build.
    pub fn is_available() -> bool {
        cfg!(feature = "runtime-wasm")
    }

    /// Validate the WASM config for common misconfigurations.
    pub fn validate_config(&self) -> Result<()> {
        if self.config.memory_limit_mb == 0 {
            bail!("runtime.wasm.memory_limit_mb must be > 0");
        }
        if self.config.memory_limit_mb > 4096 {
            bail!("runtime.wasm.memory_limit_mb exceeds the 4 GB safety limit");
        }
        if self.config.tools_dir.trim().is_empty() {
            bail!("runtime.wasm.tools_dir cannot be empty");
        }
        let tools_path = Path::new(&self.config.tools_dir);
        if tools_path.is_absolute() {
            bail!("runtime.wasm.tools_dir must be relative to the workspace");
        }
        if self.config.tools_dir.contains("..") {
            bail!("runtime.wasm.tools_dir must not contain '..' path traversal");
        }
        Ok(())
    }

    /// Resolve the absolute path to the WASM tools directory.
    pub fn tools_dir(&self, workspace_dir: &Path) -> PathBuf {
        workspace_dir.join(&self.config.tools_dir)
    }

    /// Build capabilities from config defaults.
    pub fn default_capabilities(&self) -> WasmCapabilities {
        WasmCapabilities {
            read_workspace: self.config.allow_workspace_read,
            write_workspace: self.config.allow_workspace_write,
            allowed_hosts: self.config.allowed_hosts.clone(),
            fuel_override: 0,
            memory_override_mb: 0,
        }
    }

    /// Get the effective fuel limit for an invocation.
    pub fn effective_fuel(&self, caps: &WasmCapabilities) -> u64 {
        if caps.fuel_override > 0 {
            caps.fuel_override
        } else {
            self.config.fuel_limit
        }
    }

    /// Get the effective memory limit in bytes.
    pub fn effective_memory_bytes(&self, caps: &WasmCapabilities) -> u64 {
        let mb = if caps.memory_override_mb > 0 {
            caps.memory_override_mb
        } else {
            self.config.memory_limit_mb
        };
        mb.saturating_mul(1024 * 1024)
    }

    #[cfg(feature = "runtime-wasm")]
    pub fn execute_module(
        &self,
        module_name: &str,
        workspace_dir: &Path,
        caps: &WasmCapabilities,
    ) -> Result<WasmExecutionResult> {
        use wasmtime::{Config, Engine, Linker, Module, Store, StoreLimitsBuilder};

        self.validate_config()?;

        if caps.memory_override_mb > 4096 {
            bail!("runtime.wasm.memory_limit_mb exceeds the 4 GB safety limit");
        }

        let tools_path = self.tools_dir(workspace_dir);
        let module_path = tools_path.join(format!("{module_name}.wasm"));
        if !module_path.exists() {
            bail!(
                "WASM module not found: {} (looked in {})",
                module_name,
                tools_path.display()
            );
        }

        let wasm_bytes = std::fs::read(&module_path)
            .with_context(|| format!("Failed to read WASM module: {}", module_path.display()))?;
        if wasm_bytes.len() > 50 * 1024 * 1024 {
            bail!(
                "WASM module {} is too large: {} bytes exceeds 50 MB limit",
                module_name,
                wasm_bytes.len()
            );
        }

        let mut engine_config = Config::new();
        engine_config.consume_fuel(true);
        let engine = Engine::new(&engine_config).context("Failed to create wasmtime engine")?;
        let module = Module::from_binary(&engine, &wasm_bytes)
            .with_context(|| format!("Failed to compile WASM module: {module_name}"))?;

        let limits = StoreLimitsBuilder::new()
            .memory_size(self.effective_memory_bytes(caps) as usize)
            .build();
        let mut store = Store::new(&engine, limits);
        store.limiter(|state| state);

        let fuel = self.effective_fuel(caps);
        if fuel > 0 {
            store
                .set_fuel(fuel)
                .with_context(|| format!("Failed to set fuel budget ({fuel})"))?;
        }

        let linker = Linker::new(&engine);
        let instance = linker
            .instantiate(&mut store, &module)
            .with_context(|| format!("Failed to instantiate WASM module: {module_name}"))?;

        let fuel_before = store.get_fuel().unwrap_or(fuel);

        // Support both a pure `run() -> i32` tool contract and `_start()`.
        let execution = if let Ok(run_fn) = instance.get_typed_func::<(), i32>(&mut store, "run") {
            run_fn
                .call(&mut store, ())
                .map(|exit_code| WasmExecutionResult {
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code,
                    fuel_consumed: 0,
                })
        } else if let Ok(start_fn) = instance.get_typed_func::<(), ()>(&mut store, "_start") {
            start_fn.call(&mut store, ()).map(|()| WasmExecutionResult {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 0,
                fuel_consumed: 0,
            })
        } else {
            bail!("WASM module '{module_name}' must export 'run() -> i32' or '_start()'");
        };

        match execution {
            Ok(mut result) => {
                let fuel_after = store.get_fuel().unwrap_or(0);
                result.fuel_consumed = fuel_before.saturating_sub(fuel_after);
                Ok(result)
            }
            Err(err) => {
                let fuel_after = store.get_fuel().unwrap_or(0);
                if fuel > 0 && fuel_after == 0 {
                    return Ok(WasmExecutionResult {
                        stdout: String::new(),
                        stderr: format!(
                            "WASM module '{module_name}' exceeded fuel limit ({fuel} ticks)"
                        ),
                        exit_code: -1,
                        fuel_consumed: fuel,
                    });
                }

                bail!("WASM execution error in '{module_name}': {err}");
            }
        }
    }

    #[cfg(not(feature = "runtime-wasm"))]
    pub fn execute_module(
        &self,
        module_name: &str,
        _workspace_dir: &Path,
        _caps: &WasmCapabilities,
    ) -> Result<WasmExecutionResult> {
        bail!(
            "WASM runtime is not available in this build. Rebuild with `--features runtime-wasm`. Module requested: {module_name}"
        )
    }

    /// List available WASM tool modules in the tools directory.
    pub fn list_modules(&self, workspace_dir: &Path) -> Result<Vec<String>> {
        self.validate_config()?;

        let tools_path = self.tools_dir(workspace_dir);
        if !tools_path.exists() {
            return Ok(Vec::new());
        }

        let mut modules = Vec::new();
        for entry in std::fs::read_dir(&tools_path)
            .with_context(|| format!("Failed to read tools dir: {}", tools_path.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "wasm") {
                if let Some(stem) = path.file_stem() {
                    modules.push(stem.to_string_lossy().to_string());
                }
            }
        }
        modules.sort();
        Ok(modules)
    }
}

impl RuntimeAdapter for WasmRuntime {
    fn name(&self) -> &str {
        "wasm"
    }

    fn has_shell_access(&self) -> bool {
        false
    }

    fn has_filesystem_access(&self) -> bool {
        self.config.allow_workspace_read || self.config.allow_workspace_write
    }

    fn storage_path(&self) -> PathBuf {
        self.workspace_dir
            .as_ref()
            .map_or_else(|| PathBuf::from(".zeroclaw"), |w| w.join(".zeroclaw"))
    }

    fn supports_long_running(&self) -> bool {
        false
    }

    fn memory_budget(&self) -> u64 {
        self.config.memory_limit_mb.saturating_mul(1024 * 1024)
    }

    fn build_shell_command(
        &self,
        _command: &str,
        _workspace_dir: &Path,
    ) -> Result<tokio::process::Command> {
        bail!("WASM runtime does not support shell commands. Use `execute_module()` instead.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> WasmRuntimeConfig {
        WasmRuntimeConfig::default()
    }

    #[test]
    fn runtime_trait_surface_is_locked_down() {
        let runtime = WasmRuntime::new(default_config());
        assert_eq!(runtime.name(), "wasm");
        assert!(!runtime.has_shell_access());
        assert!(!runtime.has_filesystem_access());
        assert!(!runtime.supports_long_running());
        assert_eq!(runtime.memory_budget(), 64 * 1024 * 1024);
    }

    #[test]
    fn storage_path_uses_workspace_when_present() {
        let runtime =
            WasmRuntime::with_workspace(default_config(), PathBuf::from("/tmp/zeroclaw-space"));
        assert_eq!(
            runtime.storage_path(),
            PathBuf::from("/tmp/zeroclaw-space/.zeroclaw")
        );
    }

    #[test]
    fn build_shell_command_is_rejected() {
        let runtime = WasmRuntime::new(default_config());
        let err = runtime
            .build_shell_command("echo hello", Path::new("."))
            .unwrap_err();
        assert!(err.to_string().contains("does not support shell"));
    }

    #[test]
    fn validate_config_rejects_bad_paths_and_limits() {
        let mut config = default_config();
        config.memory_limit_mb = 0;
        let err = WasmRuntime::new(config).validate_config().unwrap_err();
        assert!(err.to_string().contains("must be > 0"));

        let mut config = default_config();
        config.tools_dir = "/tmp/escape".into();
        let err = WasmRuntime::new(config).validate_config().unwrap_err();
        assert!(err.to_string().contains("must be relative"));

        let mut config = default_config();
        config.tools_dir = "../escape".into();
        let err = WasmRuntime::new(config).validate_config().unwrap_err();
        assert!(err.to_string().contains("path traversal"));
    }

    #[test]
    fn default_capabilities_follow_config() {
        let mut config = default_config();
        config.allow_workspace_read = true;
        config.allowed_hosts = vec!["api.example.com".into()];
        let caps = WasmRuntime::new(config).default_capabilities();
        assert!(caps.read_workspace);
        assert!(!caps.write_workspace);
        assert_eq!(caps.allowed_hosts, vec!["api.example.com"]);
    }

    #[test]
    fn list_modules_reads_tools_dir() {
        let temp = tempfile::tempdir().unwrap();
        let tools_dir = temp.path().join("tools/wasm");
        std::fs::create_dir_all(&tools_dir).unwrap();
        std::fs::write(tools_dir.join("one.wasm"), b"\0asm\x01\0\0\0").unwrap();
        std::fs::write(tools_dir.join("two.wasm"), b"\0asm\x01\0\0\0").unwrap();
        std::fs::write(tools_dir.join("notes.txt"), b"ignore").unwrap();

        let modules = WasmRuntime::new(default_config())
            .list_modules(temp.path())
            .unwrap();
        assert_eq!(modules, vec!["one", "two"]);
    }

    #[cfg(feature = "runtime-wasm")]
    fn write_wasm_module(dir: &Path, name: &str, wat_source: &str) {
        let tools_dir = dir.join("tools/wasm");
        std::fs::create_dir_all(&tools_dir).unwrap();
        let bytes = wat::parse_str(wat_source).unwrap();
        std::fs::write(tools_dir.join(format!("{name}.wasm")), bytes).unwrap();
    }

    #[cfg(feature = "runtime-wasm")]
    #[test]
    fn execute_module_runs_exported_run_function() {
        let temp = tempfile::tempdir().unwrap();
        write_wasm_module(
            temp.path(),
            "adder",
            r#"(module
                (func (export "run") (result i32)
                    i32.const 7
                )
            )"#,
        );

        let runtime = WasmRuntime::new(default_config());
        let result = runtime
            .execute_module("adder", temp.path(), &WasmCapabilities::default())
            .unwrap();

        assert_eq!(result.exit_code, 7);
        assert!(result.stderr.is_empty());
    }

    #[cfg(feature = "runtime-wasm")]
    #[test]
    fn execute_module_runs_exported_start_function() {
        let temp = tempfile::tempdir().unwrap();
        write_wasm_module(
            temp.path(),
            "starter",
            r#"(module
                (func (export "_start"))
            )"#,
        );

        let runtime = WasmRuntime::new(default_config());
        let result = runtime
            .execute_module("starter", temp.path(), &WasmCapabilities::default())
            .unwrap();

        assert_eq!(result.exit_code, 0);
    }

    #[cfg(feature = "runtime-wasm")]
    #[test]
    fn execute_module_enforces_fuel_limit() {
        let temp = tempfile::tempdir().unwrap();
        write_wasm_module(
            temp.path(),
            "loop_forever",
            r#"(module
                (func (export "run") (result i32)
                    (loop $spin
                        br $spin
                    )
                    i32.const 0
                )
            )"#,
        );

        let runtime = WasmRuntime::new(default_config());
        let caps = WasmCapabilities {
            fuel_override: 10_000,
            ..WasmCapabilities::default()
        };
        let result = runtime
            .execute_module("loop_forever", temp.path(), &caps)
            .unwrap();

        assert_eq!(result.exit_code, -1);
        assert!(result.stderr.contains("exceeded fuel limit"));
    }

    #[cfg(feature = "runtime-wasm")]
    #[test]
    fn execute_module_rejects_invalid_binary() {
        let temp = tempfile::tempdir().unwrap();
        let tools_dir = temp.path().join("tools/wasm");
        std::fs::create_dir_all(&tools_dir).unwrap();
        std::fs::write(tools_dir.join("bad.wasm"), b"not valid wasm").unwrap();

        let runtime = WasmRuntime::new(default_config());
        let err = runtime
            .execute_module("bad", temp.path(), &WasmCapabilities::default())
            .unwrap_err();
        assert!(err.to_string().contains("Failed to compile"));
    }

    #[cfg(not(feature = "runtime-wasm"))]
    #[test]
    fn execute_module_requires_feature() {
        let temp = tempfile::tempdir().unwrap();
        let tools_dir = temp.path().join("tools/wasm");
        std::fs::create_dir_all(&tools_dir).unwrap();
        std::fs::write(tools_dir.join("test.wasm"), b"\0asm\x01\0\0\0").unwrap();

        let runtime = WasmRuntime::new(default_config());
        let err = runtime
            .execute_module("test", temp.path(), &WasmCapabilities::default())
            .unwrap_err();
        assert!(err.to_string().contains("runtime-wasm"));
    }
}
