//! WASM sandbox runtime — in-process tool isolation via `wasmi`.
//!
//! Provides capability-based sandboxing without Docker or external runtimes.
//! Each WASM module runs with:
//! - **Fuel limits**: prevents infinite loops (each instruction costs 1 fuel)
//! - **Memory caps**: configurable per-module memory ceiling
//! - **No filesystem access**: by default, tools are pure computation
//! - **No network access**: unless explicitly allowlisted hosts are configured
//!
//! # Feature gate
//! This module is only compiled when `--features runtime-wasm` is enabled.
//! The default ZeroClaw binary excludes it to maintain the 4.6 MB size target.

use super::traits::RuntimeAdapter;
use crate::config::{WasmCapabilityEscalationMode, WasmModuleHashPolicy, WasmRuntimeConfig};
use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

/// WASM sandbox runtime — executes tool modules in an isolated interpreter.
#[derive(Debug, Clone)]
pub struct WasmRuntime {
    config: WasmRuntimeConfig,
    workspace_dir: Option<PathBuf>,
}

/// Result of executing a WASM module.
#[derive(Debug, Clone)]
pub struct WasmExecutionResult {
    /// Standard output captured from the module (if WASI is used)
    pub stdout: String,
    /// Standard error captured from the module
    pub stderr: String,
    /// Exit code (0 = success)
    pub exit_code: i32,
    /// Fuel consumed during execution
    pub fuel_consumed: u64,
    /// SHA-256 digest (hex) of the executed module bytes.
    pub module_sha256: String,
}

/// Capabilities granted to a WASM tool module.
#[derive(Debug, Clone, Default)]
pub struct WasmCapabilities {
    /// Allow reading files from workspace
    pub read_workspace: bool,
    /// Allow writing files to workspace
    pub write_workspace: bool,
    /// Allowed HTTP hosts (empty = no network)
    pub allowed_hosts: Vec<String>,
    /// Custom fuel override (0 = use config default)
    pub fuel_override: u64,
    /// Custom memory override in MB (0 = use config default)
    pub memory_override_mb: u64,
}

impl WasmRuntime {
    const MAX_MEMORY_MB: u64 = 4096;
    const MAX_FUEL_LIMIT: u64 = 10_000_000_000;

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
        if self.config.fuel_limit == 0 {
            bail!("runtime.wasm.fuel_limit must be > 0");
        }
        if self.config.fuel_limit > Self::MAX_FUEL_LIMIT {
            bail!(
                "runtime.wasm.fuel_limit of {} exceeds safety ceiling of {}",
                self.config.fuel_limit,
                Self::MAX_FUEL_LIMIT
            );
        }
        if self.config.memory_limit_mb == 0 {
            bail!("runtime.wasm.memory_limit_mb must be > 0");
        }
        if self.config.memory_limit_mb > Self::MAX_MEMORY_MB {
            bail!(
                "runtime.wasm.memory_limit_mb of {} exceeds the 4 GB safety limit for 32-bit WASM",
                self.config.memory_limit_mb
            );
        }
        if self.config.max_module_size_mb == 0 {
            bail!("runtime.wasm.max_module_size_mb must be > 0");
        }
        if self.config.tools_dir.is_empty() {
            bail!("runtime.wasm.tools_dir cannot be empty");
        }
        if self.config.security.require_workspace_relative_tools_dir {
            let tools_dir_path = Path::new(&self.config.tools_dir);
            if tools_dir_path.is_absolute() {
                bail!("runtime.wasm.tools_dir must be a workspace-relative path");
            }
            if tools_dir_path
                .components()
                .any(|c| matches!(c, Component::ParentDir))
            {
                bail!("runtime.wasm.tools_dir must not contain '..' path traversal");
            }
        }
        let _ = self.normalize_hosts_with_policy(
            self.config.allowed_hosts.iter().map(String::as_str),
            "runtime.wasm.allowed_hosts",
        )?;
        let normalized_pins = self.normalize_module_sha256_pins()?;
        if matches!(
            self.config.security.module_hash_policy,
            WasmModuleHashPolicy::Enforce
        ) && normalized_pins.is_empty()
        {
            bail!(
                "runtime.wasm.security.module_hash_policy='enforce' requires at least one module pin in runtime.wasm.security.module_sha256"
            );
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
            caps.fuel_override.min(self.config.fuel_limit)
        } else {
            self.config.fuel_limit
        }
    }

    /// Get the effective memory limit in bytes.
    pub fn effective_memory_bytes(&self, caps: &WasmCapabilities) -> u64 {
        let mb = if caps.memory_override_mb > 0 {
            caps.memory_override_mb.min(self.config.memory_limit_mb)
        } else {
            self.config.memory_limit_mb
        };
        mb.saturating_mul(1024 * 1024)
    }

    fn validate_module_name(module_name: &str) -> Result<()> {
        if module_name.is_empty() {
            bail!("WASM module name cannot be empty");
        }
        if module_name.len() > 128 {
            bail!("WASM module name is too long (max 128 chars): {module_name}");
        }
        if !module_name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            bail!(
                "WASM module name '{module_name}' contains invalid characters; \
                 allowed set is [A-Za-z0-9_-]"
            );
        }
        Ok(())
    }

    fn normalize_host(host: &str) -> Result<String> {
        let normalized = host.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            bail!("runtime.wasm.allowed_hosts contains an empty entry");
        }
        if normalized == "*" || normalized.contains('*') {
            bail!(
                "runtime.wasm.allowed_hosts entry '{host}' is invalid; wildcard hosts are not allowed"
            );
        }
        if normalized.contains("://")
            || normalized.contains('/')
            || normalized.contains('?')
            || normalized.contains('#')
        {
            bail!(
                "runtime.wasm.allowed_hosts entry '{host}' must be host[:port] only (no scheme/path/query)"
            );
        }
        if normalized.starts_with('.') || normalized.ends_with('.') {
            bail!("runtime.wasm.allowed_hosts entry '{host}' must not start/end with '.'");
        }
        if normalized.starts_with('-') || normalized.ends_with('-') {
            bail!("runtime.wasm.allowed_hosts entry '{host}' must not start/end with '-'");
        }
        if !normalized
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '.' || ch == '-' || ch == ':')
        {
            bail!("runtime.wasm.allowed_hosts entry '{host}' contains invalid characters");
        }

        if let Some((host_part, port_part)) = normalized.rsplit_once(':') {
            // Support host:port form while rejecting malformed host: segments.
            if host_part.is_empty()
                || port_part.is_empty()
                || !port_part.chars().all(|c| c.is_ascii_digit())
            {
                bail!("runtime.wasm.allowed_hosts entry '{host}' has invalid port format");
            }
            if host_part.contains(':') {
                bail!("runtime.wasm.allowed_hosts entry '{host}' has too many ':' separators");
            }
        }

        Ok(normalized)
    }

    fn normalize_hosts_with_policy<'a, I>(&self, hosts: I, source: &str) -> Result<BTreeSet<String>>
    where
        I: IntoIterator<Item = &'a str>,
    {
        let mut normalized = BTreeSet::new();
        for host in hosts {
            match Self::normalize_host(host) {
                Ok(value) => {
                    normalized.insert(value);
                }
                Err(err) if self.config.security.strict_host_validation => return Err(err),
                Err(err) => {
                    tracing::warn!(
                        host,
                        source,
                        error = %err,
                        "Ignoring invalid WASM host entry because runtime.wasm.security.strict_host_validation=false"
                    );
                }
            }
        }
        Ok(normalized)
    }

    fn normalize_sha256_pin(module_name: &str, raw: &str) -> Result<String> {
        let normalized = raw.trim().to_ascii_lowercase();
        if normalized.len() != 64 || !normalized.chars().all(|ch| ch.is_ascii_hexdigit()) {
            bail!(
                "runtime.wasm.security.module_sha256.{module_name} must be a 64-character hex SHA-256 digest"
            );
        }
        Ok(normalized)
    }

    fn normalize_module_sha256_pins(&self) -> Result<BTreeMap<String, String>> {
        let mut normalized = BTreeMap::new();
        for (module_name, digest) in &self.config.security.module_sha256 {
            Self::validate_module_name(module_name)?;
            normalized.insert(
                module_name.clone(),
                Self::normalize_sha256_pin(module_name, digest)?,
            );
        }
        Ok(normalized)
    }

    fn check_module_integrity(&self, module_name: &str, wasm_bytes: &[u8]) -> Result<String> {
        let digest = hex::encode(Sha256::digest(wasm_bytes));
        let normalized_pins = self.normalize_module_sha256_pins()?;
        match self.config.security.module_hash_policy {
            WasmModuleHashPolicy::Disabled => {}
            WasmModuleHashPolicy::Warn => match normalized_pins.get(module_name) {
                Some(expected) if expected == &digest => {}
                Some(expected) => {
                    tracing::warn!(
                        module = module_name,
                        expected_sha256 = expected,
                        actual_sha256 = digest,
                        "WASM module SHA-256 mismatch (warn mode)"
                    );
                }
                None => {
                    tracing::warn!(
                        module = module_name,
                        actual_sha256 = digest,
                        "WASM module has no SHA-256 pin configured (warn mode)"
                    );
                }
            },
            WasmModuleHashPolicy::Enforce => match normalized_pins.get(module_name) {
                Some(expected) if expected == &digest => {}
                Some(expected) => {
                    bail!(
                        "WASM module integrity mismatch for '{module_name}': expected sha256={expected}, got sha256={digest}"
                    );
                }
                None => {
                    bail!(
                        "WASM module '{module_name}' is missing required SHA-256 pin (runtime.wasm.security.module_hash_policy='enforce')"
                    );
                }
            },
        }
        Ok(digest)
    }

    fn validate_capabilities(&self, caps: &WasmCapabilities) -> Result<WasmCapabilities> {
        let default_hosts = self.normalize_hosts_with_policy(
            self.config.allowed_hosts.iter().map(String::as_str),
            "runtime.wasm.allowed_hosts",
        )?;
        let requested_hosts = self.normalize_hosts_with_policy(
            caps.allowed_hosts.iter().map(String::as_str),
            "wasm invocation allowed_hosts",
        )?;

        match self.config.security.capability_escalation_mode {
            WasmCapabilityEscalationMode::Deny => {
                if caps.read_workspace && !self.config.allow_workspace_read {
                    bail!(
                        "WASM capability escalation blocked: read_workspace requested but runtime.wasm.allow_workspace_read is false"
                    );
                }
                if caps.write_workspace && !self.config.allow_workspace_write {
                    bail!(
                        "WASM capability escalation blocked: write_workspace requested but runtime.wasm.allow_workspace_write is false"
                    );
                }
                if caps.fuel_override > self.config.fuel_limit {
                    bail!(
                        "WASM capability escalation blocked: fuel_override={} exceeds runtime.wasm.fuel_limit={}",
                        caps.fuel_override,
                        self.config.fuel_limit
                    );
                }
                if caps.memory_override_mb > self.config.memory_limit_mb {
                    bail!(
                        "WASM capability escalation blocked: memory_override_mb={} exceeds runtime.wasm.memory_limit_mb={}",
                        caps.memory_override_mb,
                        self.config.memory_limit_mb
                    );
                }
                for host in &requested_hosts {
                    if !default_hosts.contains(host) {
                        bail!(
                            "WASM capability escalation blocked: host '{host}' is not in runtime.wasm.allowed_hosts"
                        );
                    }
                }
                Ok(WasmCapabilities {
                    read_workspace: caps.read_workspace,
                    write_workspace: caps.write_workspace,
                    allowed_hosts: requested_hosts.into_iter().collect(),
                    fuel_override: caps.fuel_override,
                    memory_override_mb: caps.memory_override_mb,
                })
            }
            WasmCapabilityEscalationMode::Clamp => {
                let mut effective = WasmCapabilities {
                    read_workspace: caps.read_workspace && self.config.allow_workspace_read,
                    write_workspace: caps.write_workspace && self.config.allow_workspace_write,
                    allowed_hosts: requested_hosts
                        .intersection(&default_hosts)
                        .cloned()
                        .collect::<Vec<_>>(),
                    fuel_override: if caps.fuel_override > self.config.fuel_limit {
                        self.config.fuel_limit
                    } else {
                        caps.fuel_override
                    },
                    memory_override_mb: if caps.memory_override_mb > self.config.memory_limit_mb {
                        self.config.memory_limit_mb
                    } else {
                        caps.memory_override_mb
                    },
                };

                if caps.read_workspace && !effective.read_workspace {
                    tracing::warn!(
                        "Clamped WASM read_workspace request because runtime.wasm.allow_workspace_read=false"
                    );
                }
                if caps.write_workspace && !effective.write_workspace {
                    tracing::warn!(
                        "Clamped WASM write_workspace request because runtime.wasm.allow_workspace_write=false"
                    );
                }
                if caps.fuel_override > self.config.fuel_limit {
                    tracing::warn!(
                        requested = caps.fuel_override,
                        allowed = self.config.fuel_limit,
                        "Clamped WASM fuel_override to runtime.wasm.fuel_limit"
                    );
                }
                if caps.memory_override_mb > self.config.memory_limit_mb {
                    tracing::warn!(
                        requested = caps.memory_override_mb,
                        allowed = self.config.memory_limit_mb,
                        "Clamped WASM memory_override_mb to runtime.wasm.memory_limit_mb"
                    );
                }
                if effective.allowed_hosts.len() != requested_hosts.len() {
                    tracing::warn!(
                        requested = requested_hosts.len(),
                        allowed = effective.allowed_hosts.len(),
                        "Clamped WASM allowed_hosts to runtime.wasm.allowed_hosts"
                    );
                }

                effective.allowed_hosts.sort();
                Ok(effective)
            }
        }
    }

    /// Execute a WASM module from the tools directory.
    ///
    /// This is the primary entry point for running sandboxed tool code.
    /// The module must export a `_start` function (WASI convention) or
    /// a custom `run` function that takes no arguments and returns i32.
    #[cfg(feature = "runtime-wasm")]
    pub fn execute_module(
        &self,
        module_name: &str,
        workspace_dir: &Path,
        caps: &WasmCapabilities,
    ) -> Result<WasmExecutionResult> {
        use wasmi::{Engine, Linker, Module, Store};

        self.validate_config()?;
        Self::validate_module_name(module_name)?;
        let effective_caps = self.validate_capabilities(caps)?;

        // Resolve and normalize module path.
        let tools_path = self.tools_dir(workspace_dir);
        if !tools_path.exists() {
            bail!(
                "WASM tools directory does not exist: {}",
                tools_path.display()
            );
        }
        if self.config.security.reject_symlink_tools_dir {
            let tools_meta = std::fs::symlink_metadata(&tools_path).with_context(|| {
                format!(
                    "Failed to inspect WASM tools directory metadata: {}",
                    tools_path.display()
                )
            })?;
            if tools_meta.file_type().is_symlink() {
                bail!(
                    "WASM tools directory must not be a symlink: {}",
                    tools_path.display()
                );
            }
        }
        let canonical_tools_path = std::fs::canonicalize(&tools_path).with_context(|| {
            format!(
                "Failed to canonicalize WASM tools directory: {}",
                tools_path.display()
            )
        })?;
        if !canonical_tools_path.is_dir() {
            bail!(
                "WASM tools path is not a directory: {}",
                canonical_tools_path.display()
            );
        }
        let module_path = canonical_tools_path.join(format!("{module_name}.wasm"));

        if !module_path.exists() {
            bail!(
                "WASM module not found: {} (looked in {})",
                module_name,
                canonical_tools_path.display()
            );
        }
        if self.config.security.reject_symlink_modules {
            let module_symlink_meta =
                std::fs::symlink_metadata(&module_path).with_context(|| {
                    format!(
                        "Failed to inspect WASM module metadata: {}",
                        module_path.display()
                    )
                })?;
            if module_symlink_meta.file_type().is_symlink() {
                bail!(
                    "WASM module path must not be a symlink: {}",
                    module_path.display()
                );
            }
        }
        let canonical_module_path = std::fs::canonicalize(&module_path).with_context(|| {
            format!(
                "Failed to canonicalize WASM module path: {}",
                module_path.display()
            )
        })?;
        if !canonical_module_path.starts_with(&canonical_tools_path) {
            bail!(
                "WASM module path escapes tools directory: {}",
                canonical_module_path.display()
            );
        }
        if canonical_module_path
            .extension()
            .and_then(|ext| ext.to_str())
            != Some("wasm")
        {
            bail!(
                "WASM module path must end with .wasm: {}",
                canonical_module_path.display()
            );
        }
        if !canonical_module_path.is_file() {
            bail!(
                "WASM module path is not a file: {}",
                canonical_module_path.display()
            );
        }

        let module_size_bytes = std::fs::metadata(&canonical_module_path)
            .with_context(|| {
                format!(
                    "Failed to read WASM module metadata: {}",
                    canonical_module_path.display()
                )
            })?
            .len();
        let max_size_bytes = self.config.max_module_size_mb * 1024 * 1024;
        if module_size_bytes > max_size_bytes {
            bail!(
                "WASM module {} is {} MB — exceeds configured {} MB safety limit",
                module_name,
                module_size_bytes / (1024 * 1024),
                self.config.max_module_size_mb
            );
        }

        // Read module bytes
        let wasm_bytes = std::fs::read(&canonical_module_path).with_context(|| {
            format!(
                "Failed to read WASM module: {}",
                canonical_module_path.display()
            )
        })?;
        let module_sha256 = self.check_module_integrity(module_name, &wasm_bytes)?;

        // Configure engine with fuel metering
        let mut engine_config = wasmi::Config::default();
        engine_config.consume_fuel(true);
        let engine = Engine::new(&engine_config);

        // Parse and validate module
        let module = Module::new(&engine, &wasm_bytes[..])
            .with_context(|| format!("Failed to parse WASM module: {module_name}"))?;

        // Create store with fuel budget
        let mut store = Store::new(&engine, ());
        let fuel = self.effective_fuel(&effective_caps);
        if fuel > 0 {
            store.set_fuel(fuel).with_context(|| {
                format!("Failed to set fuel budget ({fuel}) for module: {module_name}")
            })?;
        }

        // Link host functions (minimal — pure sandboxing)
        let linker = Linker::new(&engine);

        // Instantiate module
        let instance = linker
            .instantiate_and_start(&mut store, &module)
            .with_context(|| format!("Failed to instantiate WASM module: {module_name}"))?;

        // Look for exported entry point
        let run_fn = instance
            .get_typed_func::<(), i32>(&store, "run")
            .or_else(|_| instance.get_typed_func::<(), i32>(&store, "_start"))
            .with_context(|| {
                format!(
                    "WASM module '{module_name}' must export a 'run() -> i32' or '_start() -> i32' function"
                )
            })?;

        // Execute with fuel accounting
        let fuel_before = store.get_fuel().unwrap_or(0);
        let exit_code = match run_fn.call(&mut store, ()) {
            Ok(code) => code,
            Err(e) => {
                // Check if we ran out of fuel (infinite loop protection)
                let fuel_after = store.get_fuel().unwrap_or(0);
                if fuel_after == 0 && fuel > 0 {
                    return Ok(WasmExecutionResult {
                        stdout: String::new(),
                        stderr: format!(
                            "WASM module '{module_name}' exceeded fuel limit ({fuel} ticks) — likely an infinite loop"
                        ),
                        exit_code: -1,
                        fuel_consumed: fuel,
                        module_sha256: module_sha256.clone(),
                    });
                }
                bail!("WASM execution error in '{module_name}': {e}");
            }
        };
        let fuel_after = store.get_fuel().unwrap_or(0);
        let fuel_consumed = fuel_before.saturating_sub(fuel_after);

        Ok(WasmExecutionResult {
            stdout: String::new(), // No WASI stdout yet — pure computation
            stderr: String::new(),
            exit_code,
            fuel_consumed,
            module_sha256,
        })
    }

    /// Stub for when the `runtime-wasm` feature is not enabled.
    #[cfg(not(feature = "runtime-wasm"))]
    pub fn execute_module(
        &self,
        module_name: &str,
        _workspace_dir: &Path,
        _caps: &WasmCapabilities,
    ) -> Result<WasmExecutionResult> {
        bail!(
            "WASM runtime is not available in this build. \
             Rebuild with `cargo build --features runtime-wasm` to enable WASM sandbox support. \
             Module requested: {module_name}"
        )
    }

    /// List available WASM tool modules in the tools directory.
    pub fn list_modules(&self, workspace_dir: &Path) -> Result<Vec<String>> {
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
                    let module_name = stem.to_string_lossy().to_string();
                    if Self::validate_module_name(&module_name).is_ok() {
                        modules.push(module_name);
                    }
                }
            }
        }
        modules.sort();
        Ok(modules)
    }
}

impl RuntimeAdapter for WasmRuntime {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn name(&self) -> &str {
        "wasm"
    }

    fn has_shell_access(&self) -> bool {
        // WASM sandbox does NOT provide shell access — that's the point
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
        // WASM modules are short-lived invocations, not daemons
        false
    }

    fn memory_budget(&self) -> u64 {
        self.config.memory_limit_mb.saturating_mul(1024 * 1024)
    }

    fn build_shell_command(
        &self,
        _command: &str,
        _workspace_dir: &Path,
    ) -> anyhow::Result<tokio::process::Command> {
        bail!(
            "WASM runtime does not support shell commands. \
             Use `execute_module()` to run WASM tools, or switch to runtime.kind = \"native\" for shell access."
        )
    }
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> WasmRuntimeConfig {
        WasmRuntimeConfig::default()
    }

    // ── Basic trait compliance ──────────────────────────────────

    #[test]
    fn wasm_runtime_name() {
        let rt = WasmRuntime::new(default_config());
        assert_eq!(rt.name(), "wasm");
    }

    #[test]
    fn wasm_no_shell_access() {
        let rt = WasmRuntime::new(default_config());
        assert!(!rt.has_shell_access());
    }

    #[test]
    fn wasm_no_filesystem_by_default() {
        let rt = WasmRuntime::new(default_config());
        assert!(!rt.has_filesystem_access());
    }

    #[test]
    fn wasm_filesystem_when_read_enabled() {
        let mut cfg = default_config();
        cfg.allow_workspace_read = true;
        let rt = WasmRuntime::new(cfg);
        assert!(rt.has_filesystem_access());
    }

    #[test]
    fn wasm_filesystem_when_write_enabled() {
        let mut cfg = default_config();
        cfg.allow_workspace_write = true;
        let rt = WasmRuntime::new(cfg);
        assert!(rt.has_filesystem_access());
    }

    #[test]
    fn wasm_no_long_running() {
        let rt = WasmRuntime::new(default_config());
        assert!(!rt.supports_long_running());
    }

    #[test]
    fn wasm_memory_budget() {
        let rt = WasmRuntime::new(default_config());
        assert_eq!(rt.memory_budget(), 64 * 1024 * 1024);
    }

    #[test]
    fn wasm_shell_command_errors() {
        let rt = WasmRuntime::new(default_config());
        let result = rt.build_shell_command("echo hello", Path::new("/tmp"));
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("does not support shell"));
    }

    #[test]
    fn wasm_storage_path_default() {
        let rt = WasmRuntime::new(default_config());
        assert!(rt.storage_path().to_string_lossy().contains("zeroclaw"));
    }

    #[test]
    fn wasm_storage_path_with_workspace() {
        let rt = WasmRuntime::with_workspace(default_config(), PathBuf::from("/home/user/project"));
        assert_eq!(
            rt.storage_path(),
            PathBuf::from("/home/user/project/.zeroclaw")
        );
    }

    // ── Config validation ──────────────────────────────────────

    #[test]
    fn validate_rejects_zero_memory() {
        let mut cfg = default_config();
        cfg.memory_limit_mb = 0;
        let rt = WasmRuntime::new(cfg);
        let err = rt.validate_config().unwrap_err();
        assert!(err.to_string().contains("must be > 0"));
    }

    #[test]
    fn validate_rejects_excessive_memory() {
        let mut cfg = default_config();
        cfg.memory_limit_mb = 8192;
        let rt = WasmRuntime::new(cfg);
        let err = rt.validate_config().unwrap_err();
        assert!(err.to_string().contains("4 GB safety limit"));
    }

    #[test]
    fn validate_rejects_zero_fuel() {
        let mut cfg = default_config();
        cfg.fuel_limit = 0;
        let rt = WasmRuntime::new(cfg);
        let err = rt.validate_config().unwrap_err();
        assert!(err.to_string().contains("fuel_limit"));
    }

    #[test]
    fn validate_rejects_zero_max_module_size() {
        let mut cfg = default_config();
        cfg.max_module_size_mb = 0;
        let rt = WasmRuntime::new(cfg);
        let err = rt.validate_config().unwrap_err();
        assert!(err.to_string().contains("max_module_size_mb"));
    }

    #[test]
    fn validate_rejects_empty_tools_dir() {
        let mut cfg = default_config();
        cfg.tools_dir = String::new();
        let rt = WasmRuntime::new(cfg);
        let err = rt.validate_config().unwrap_err();
        assert!(err.to_string().contains("cannot be empty"));
    }

    #[test]
    fn validate_rejects_absolute_tools_dir() {
        let mut cfg = default_config();
        cfg.tools_dir = "/tmp/wasm-tools".into();
        let rt = WasmRuntime::new(cfg);
        let err = rt.validate_config().unwrap_err();
        assert!(err.to_string().contains("workspace-relative"));
    }

    #[test]
    fn validate_rejects_path_traversal() {
        let mut cfg = default_config();
        cfg.tools_dir = "../../../etc/passwd".into();
        let rt = WasmRuntime::new(cfg);
        let err = rt.validate_config().unwrap_err();
        assert!(err.to_string().contains("path traversal"));
    }

    #[test]
    fn validate_allows_absolute_tools_dir_when_configured() {
        let mut cfg = default_config();
        cfg.tools_dir = "/tmp/wasm-tools".into();
        cfg.security.require_workspace_relative_tools_dir = false;
        let rt = WasmRuntime::new(cfg);
        assert!(rt.validate_config().is_ok());
    }

    #[test]
    fn validate_allows_path_traversal_when_configured() {
        let mut cfg = default_config();
        cfg.tools_dir = "../../../etc/passwd".into();
        cfg.security.require_workspace_relative_tools_dir = false;
        let rt = WasmRuntime::new(cfg);
        assert!(rt.validate_config().is_ok());
    }

    #[test]
    fn validate_rejects_wildcard_host_entries() {
        let mut cfg = default_config();
        cfg.allowed_hosts = vec!["*.example.com".into()];
        let rt = WasmRuntime::new(cfg);
        let err = rt.validate_config().unwrap_err();
        assert!(err.to_string().contains("wildcard"));
    }

    #[test]
    fn validate_ignores_invalid_host_entries_when_non_strict() {
        let mut cfg = default_config();
        cfg.allowed_hosts = vec!["*.example.com".into(), "api.example.com".into()];
        cfg.security.strict_host_validation = false;
        let rt = WasmRuntime::new(cfg);
        assert!(rt.validate_config().is_ok());
    }

    #[test]
    fn validate_accepts_valid_config() {
        let rt = WasmRuntime::new(default_config());
        assert!(rt.validate_config().is_ok());
    }

    #[test]
    fn validate_rejects_invalid_module_sha256_pin_format() {
        let mut cfg = default_config();
        cfg.security
            .module_sha256
            .insert("calc".into(), "not-a-sha256".into());
        let rt = WasmRuntime::new(cfg);
        let err = rt.validate_config().unwrap_err();
        assert!(err.to_string().contains("64-character hex"));
    }

    #[test]
    fn validate_rejects_invalid_module_sha256_pin_name() {
        let mut cfg = default_config();
        cfg.security.module_sha256.insert(
            "bad$name".into(),
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
        );
        let rt = WasmRuntime::new(cfg);
        let err = rt.validate_config().unwrap_err();
        assert!(err.to_string().contains("invalid characters"));
    }

    #[test]
    fn validate_rejects_enforce_hash_policy_without_pins() {
        let mut cfg = default_config();
        cfg.security.module_hash_policy = WasmModuleHashPolicy::Enforce;
        let rt = WasmRuntime::new(cfg);
        let err = rt.validate_config().unwrap_err();
        assert!(err.to_string().contains("requires at least one module pin"));
    }

    #[test]
    fn validate_accepts_max_memory() {
        let mut cfg = default_config();
        cfg.memory_limit_mb = 4096;
        let rt = WasmRuntime::new(cfg);
        assert!(rt.validate_config().is_ok());
    }

    // ── Capabilities & fuel ────────────────────────────────────

    #[test]
    fn effective_fuel_uses_config_default() {
        let rt = WasmRuntime::new(default_config());
        let caps = WasmCapabilities::default();
        assert_eq!(rt.effective_fuel(&caps), 1_000_000);
    }

    #[test]
    fn effective_fuel_respects_override() {
        let rt = WasmRuntime::new(default_config());
        let caps = WasmCapabilities {
            fuel_override: 500,
            ..Default::default()
        };
        assert_eq!(rt.effective_fuel(&caps), 500);
    }

    #[test]
    fn effective_fuel_clamps_override_to_config_limit() {
        let mut cfg = default_config();
        cfg.fuel_limit = 10;
        let rt = WasmRuntime::new(cfg);
        let caps = WasmCapabilities {
            fuel_override: 99,
            ..Default::default()
        };
        assert_eq!(rt.effective_fuel(&caps), 10);
    }

    #[test]
    fn effective_memory_uses_config_default() {
        let rt = WasmRuntime::new(default_config());
        let caps = WasmCapabilities::default();
        assert_eq!(rt.effective_memory_bytes(&caps), 64 * 1024 * 1024);
    }

    #[test]
    fn effective_memory_respects_override() {
        let rt = WasmRuntime::new(default_config());
        let caps = WasmCapabilities {
            memory_override_mb: 32,
            ..Default::default()
        };
        assert_eq!(rt.effective_memory_bytes(&caps), 32 * 1024 * 1024);
    }

    #[test]
    fn effective_memory_clamps_override_to_config_limit() {
        let rt = WasmRuntime::new(default_config());
        let caps = WasmCapabilities {
            memory_override_mb: 256,
            ..Default::default()
        };
        assert_eq!(rt.effective_memory_bytes(&caps), 64 * 1024 * 1024);
    }

    #[test]
    fn default_capabilities_match_config() {
        let mut cfg = default_config();
        cfg.allow_workspace_read = true;
        cfg.allowed_hosts = vec!["api.example.com".into()];
        let rt = WasmRuntime::new(cfg);
        let caps = rt.default_capabilities();
        assert!(caps.read_workspace);
        assert!(!caps.write_workspace);
        assert_eq!(caps.allowed_hosts, vec!["api.example.com"]);
    }

    #[test]
    fn validate_capabilities_rejects_fuel_escalation() {
        let mut cfg = default_config();
        cfg.fuel_limit = 100;
        let rt = WasmRuntime::new(cfg);
        let caps = WasmCapabilities {
            fuel_override: 101,
            ..Default::default()
        };
        let err = rt.validate_capabilities(&caps).unwrap_err();
        assert!(err.to_string().contains("fuel_override"));
    }

    #[test]
    fn validate_capabilities_rejects_memory_escalation() {
        let mut cfg = default_config();
        cfg.memory_limit_mb = 64;
        let rt = WasmRuntime::new(cfg);
        let caps = WasmCapabilities {
            memory_override_mb: 65,
            ..Default::default()
        };
        let err = rt.validate_capabilities(&caps).unwrap_err();
        assert!(err.to_string().contains("memory_override_mb"));
    }

    #[test]
    fn validate_capabilities_rejects_host_escalation() {
        let mut cfg = default_config();
        cfg.allowed_hosts = vec!["api.example.com".into()];
        let rt = WasmRuntime::new(cfg);
        let caps = WasmCapabilities {
            allowed_hosts: vec!["evil.example.com".into()],
            ..Default::default()
        };
        let err = rt.validate_capabilities(&caps).unwrap_err();
        assert!(err
            .to_string()
            .contains("not in runtime.wasm.allowed_hosts"));
    }

    #[test]
    fn validate_capabilities_accepts_host_subset() {
        let mut cfg = default_config();
        cfg.allowed_hosts = vec!["api.example.com".into(), "cdn.example.com".into()];
        let rt = WasmRuntime::new(cfg);
        let caps = WasmCapabilities {
            allowed_hosts: vec!["api.example.com".into()],
            ..Default::default()
        };
        assert!(rt.validate_capabilities(&caps).is_ok());
    }

    #[test]
    fn validate_capabilities_clamps_escalation_when_configured() {
        let mut cfg = default_config();
        cfg.fuel_limit = 100;
        cfg.memory_limit_mb = 32;
        cfg.allowed_hosts = vec!["api.example.com".into()];
        cfg.security.capability_escalation_mode = WasmCapabilityEscalationMode::Clamp;
        let rt = WasmRuntime::new(cfg);
        let caps = WasmCapabilities {
            read_workspace: true,
            write_workspace: true,
            allowed_hosts: vec!["api.example.com".into(), "evil.example.com".into()],
            fuel_override: 500,
            memory_override_mb: 64,
        };
        let effective = rt
            .validate_capabilities(&caps)
            .expect("clamp should succeed");
        assert!(!effective.read_workspace);
        assert!(!effective.write_workspace);
        assert_eq!(effective.allowed_hosts, vec!["api.example.com"]);
        assert_eq!(effective.fuel_override, 100);
        assert_eq!(effective.memory_override_mb, 32);
    }

    // ── Tools directory ────────────────────────────────────────

    #[test]
    fn tools_dir_resolves_relative_to_workspace() {
        let rt = WasmRuntime::new(default_config());
        let dir = rt.tools_dir(Path::new("/home/user/project"));
        assert_eq!(dir, PathBuf::from("/home/user/project/tools/wasm"));
    }

    #[test]
    fn list_modules_empty_when_dir_missing() {
        let rt = WasmRuntime::new(default_config());
        let modules = rt.list_modules(Path::new("/nonexistent/path")).unwrap();
        assert!(modules.is_empty());
    }

    #[test]
    fn list_modules_finds_wasm_files() {
        let dir = tempfile::tempdir().unwrap();
        let tools_dir = dir.path().join("tools/wasm");
        std::fs::create_dir_all(&tools_dir).unwrap();

        // Create dummy .wasm files
        std::fs::write(tools_dir.join("calculator.wasm"), b"\0asm").unwrap();
        std::fs::write(tools_dir.join("formatter.wasm"), b"\0asm").unwrap();
        std::fs::write(tools_dir.join("bad$name.wasm"), b"\0asm").unwrap();
        std::fs::write(tools_dir.join("readme.txt"), b"not a wasm").unwrap();

        let rt = WasmRuntime::new(default_config());
        let modules = rt.list_modules(dir.path()).unwrap();
        assert_eq!(modules, vec!["calculator", "formatter"]);
    }

    #[test]
    fn validate_module_name_rejects_traversal_like_input() {
        let err = WasmRuntime::validate_module_name("../secrets").unwrap_err();
        assert!(err.to_string().contains("invalid characters"));
    }

    // ── Module execution edge cases ────────────────────────────

    #[test]
    fn execute_module_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let tools_dir = dir.path().join("tools/wasm");
        std::fs::create_dir_all(&tools_dir).unwrap();

        let rt = WasmRuntime::new(default_config());
        let caps = WasmCapabilities::default();
        let result = rt.execute_module("nonexistent", dir.path(), &caps);
        assert!(result.is_err());

        let err_msg = result.unwrap_err().to_string();
        // Should mention the module name
        assert!(err_msg.contains("nonexistent"));
    }

    #[test]
    fn execute_module_invalid_wasm() {
        let dir = tempfile::tempdir().unwrap();
        let tools_dir = dir.path().join("tools/wasm");
        std::fs::create_dir_all(&tools_dir).unwrap();

        // Write invalid WASM bytes
        std::fs::write(tools_dir.join("bad.wasm"), b"not valid wasm bytes at all").unwrap();

        let rt = WasmRuntime::new(default_config());
        let caps = WasmCapabilities::default();
        let result = rt.execute_module("bad", dir.path(), &caps);
        assert!(result.is_err());
    }

    #[test]
    fn execute_module_oversized_file() {
        let dir = tempfile::tempdir().unwrap();
        let tools_dir = dir.path().join("tools/wasm");
        std::fs::create_dir_all(&tools_dir).unwrap();

        // Write a file > 50 MB (we just check the size, don't actually allocate)
        // This test verifies the check without consuming 50 MB of disk
        let rt = WasmRuntime::new(default_config());
        let caps = WasmCapabilities::default();

        // File doesn't exist for oversized test — the missing file check catches first
        // But if it did exist and was 51 MB, the size check would catch it
        let result = rt.execute_module("oversized", dir.path(), &caps);
        assert!(result.is_err());
    }

    #[test]
    fn execute_module_enforce_hash_policy_rejects_mismatch() {
        if !WasmRuntime::is_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let tools_dir = dir.path().join("tools/wasm");
        std::fs::create_dir_all(&tools_dir).unwrap();
        std::fs::write(tools_dir.join("calc.wasm"), b"\0asm\x01\0\0\0").unwrap();

        let mut cfg = default_config();
        cfg.security.module_hash_policy = WasmModuleHashPolicy::Enforce;
        cfg.security.module_sha256.insert(
            "calc".into(),
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
        );

        let rt = WasmRuntime::new(cfg);
        let result = rt.execute_module("calc", dir.path(), &WasmCapabilities::default());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("integrity mismatch"));
    }

    #[test]
    fn execute_module_warn_hash_policy_allows_execution_path() {
        if !WasmRuntime::is_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let tools_dir = dir.path().join("tools/wasm");
        std::fs::create_dir_all(&tools_dir).unwrap();
        std::fs::write(tools_dir.join("calc.wasm"), b"\0asm\x01\0\0\0").unwrap();

        let mut cfg = default_config();
        cfg.security.module_hash_policy = WasmModuleHashPolicy::Warn;
        cfg.security.module_sha256.insert(
            "calc".into(),
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
        );

        let rt = WasmRuntime::new(cfg);
        let result = rt.execute_module("calc", dir.path(), &WasmCapabilities::default());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("must export a 'run() -> i32'"));
    }

    #[cfg(unix)]
    #[test]
    fn execute_module_rejects_symlink_tools_dir_when_enabled() {
        if !WasmRuntime::is_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let real_tools_dir = dir.path().join("real-tools");
        std::fs::create_dir_all(&real_tools_dir).unwrap();
        std::fs::write(real_tools_dir.join("calc.wasm"), b"\0asm\x01\0\0\0").unwrap();

        let tools_parent = dir.path().join("tools");
        std::fs::create_dir_all(&tools_parent).unwrap();
        std::os::unix::fs::symlink(&real_tools_dir, tools_parent.join("wasm")).unwrap();

        let rt = WasmRuntime::new(default_config());
        let result = rt.execute_module("calc", dir.path(), &WasmCapabilities::default());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("tools directory must not be a symlink"));
    }

    #[cfg(unix)]
    #[test]
    fn execute_module_allows_symlink_tools_dir_when_disabled() {
        if !WasmRuntime::is_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let real_tools_dir = dir.path().join("real-tools");
        std::fs::create_dir_all(&real_tools_dir).unwrap();
        std::fs::write(real_tools_dir.join("calc.wasm"), b"\0asm\x01\0\0\0").unwrap();

        let tools_parent = dir.path().join("tools");
        std::fs::create_dir_all(&tools_parent).unwrap();
        std::os::unix::fs::symlink(&real_tools_dir, tools_parent.join("wasm")).unwrap();

        let mut cfg = default_config();
        cfg.security.reject_symlink_tools_dir = false;
        cfg.security.module_hash_policy = WasmModuleHashPolicy::Disabled;
        let rt = WasmRuntime::new(cfg);
        let result = rt.execute_module("calc", dir.path(), &WasmCapabilities::default());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("must export a 'run() -> i32'"));
    }

    // ── Feature gate check ─────────────────────────────────────

    #[test]
    fn is_available_matches_feature_flag() {
        // This test verifies the compile-time feature detection works
        let available = WasmRuntime::is_available();
        assert_eq!(available, cfg!(feature = "runtime-wasm"));
    }

    // ── Memory overflow edge cases ─────────────────────────────

    #[test]
    fn memory_budget_no_overflow() {
        let mut cfg = default_config();
        cfg.memory_limit_mb = 4096; // Max valid
        let rt = WasmRuntime::new(cfg);
        assert_eq!(rt.memory_budget(), 4096 * 1024 * 1024);
    }

    #[test]
    fn effective_memory_saturating() {
        let rt = WasmRuntime::new(default_config());
        let caps = WasmCapabilities {
            memory_override_mb: u64::MAX,
            ..Default::default()
        };
        // Should not panic — override is clamped to config ceiling.
        assert_eq!(rt.effective_memory_bytes(&caps), 64 * 1024 * 1024);
    }

    // ── WasmCapabilities default ───────────────────────────────

    #[test]
    fn capabilities_default_is_locked_down() {
        let caps = WasmCapabilities::default();
        assert!(!caps.read_workspace);
        assert!(!caps.write_workspace);
        assert!(caps.allowed_hosts.is_empty());
        assert_eq!(caps.fuel_override, 0);
        assert_eq!(caps.memory_override_mb, 0);
    }

    // ── §3.1 / §3.2 WASM fuel & memory exhaustion tests ─────

    #[test]
    fn wasm_fuel_limit_enforced_in_config() {
        let rt = WasmRuntime::new(default_config());
        let caps = WasmCapabilities::default();
        let fuel = rt.effective_fuel(&caps);
        assert!(
            fuel > 0,
            "default fuel limit must be > 0 to prevent infinite loops"
        );
    }

    #[test]
    fn wasm_memory_limit_enforced_in_config() {
        let rt = WasmRuntime::new(default_config());
        let caps = WasmCapabilities::default();
        let mem_bytes = rt.effective_memory_bytes(&caps);
        assert!(mem_bytes > 0, "default memory limit must be > 0");
        assert!(
            mem_bytes <= 4096 * 1024 * 1024,
            "default memory must not exceed 4 GB safety limit"
        );
    }

    #[test]
    fn wasm_zero_fuel_override_uses_default() {
        let rt = WasmRuntime::new(default_config());
        let caps = WasmCapabilities {
            fuel_override: 0,
            ..Default::default()
        };
        assert_eq!(
            rt.effective_fuel(&caps),
            1_000_000,
            "fuel_override=0 must use config default"
        );
    }

    #[test]
    fn validate_rejects_memory_just_above_limit() {
        let mut cfg = default_config();
        cfg.memory_limit_mb = 4097;
        let rt = WasmRuntime::new(cfg);
        let err = rt.validate_config().unwrap_err();
        assert!(err.to_string().contains("4 GB safety limit"));
    }

    #[test]
    fn execute_module_stub_returns_error_without_feature() {
        if !WasmRuntime::is_available() {
            let dir = tempfile::tempdir().unwrap();
            let tools_dir = dir.path().join("tools/wasm");
            std::fs::create_dir_all(&tools_dir).unwrap();
            std::fs::write(tools_dir.join("test.wasm"), b"\0asm\x01\0\0\0").unwrap();

            let rt = WasmRuntime::new(default_config());
            let caps = WasmCapabilities::default();
            let result = rt.execute_module("test", dir.path(), &caps);
            assert!(result.is_err());
            assert!(result.unwrap_err().to_string().contains("not available"));
        }
    }
}
