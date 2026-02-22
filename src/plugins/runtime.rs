use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::SystemTime;
use tokio::sync::Semaphore;
use tokio::time::{timeout, Duration};
use wasmtime::{Engine, Extern, Instance, Memory, Module, Store, TypedFunc};

use super::manifest::PluginManifest;
use super::registry::PluginRegistry;
use crate::config::PluginsConfig;
use crate::tools::ToolResult;

const ABI_TOOL_EXEC_FN: &str = "zeroclaw_tool_execute";
const ABI_PROVIDER_CHAT_FN: &str = "zeroclaw_provider_chat";
const ABI_ALLOC_FN: &str = "alloc";
const ABI_DEALLOC_FN: &str = "dealloc";
const MAX_WASM_PAYLOAD_BYTES_FALLBACK: usize = 4 * 1024 * 1024;

#[derive(Debug, Default)]
pub struct PluginRuntime;

impl PluginRuntime {
    pub fn new() -> Self {
        Self
    }

    pub fn load_manifest(&self, manifest: PluginManifest) -> Result<PluginManifest> {
        if !manifest.is_valid() {
            anyhow::bail!("invalid plugin manifest")
        }
        Ok(manifest)
    }

    pub fn load_registry_from_config(&self, config: &PluginsConfig) -> Result<PluginRegistry> {
        let mut registry = PluginRegistry::default();
        if !config.enabled {
            return Ok(registry);
        }
        for dir in &config.dirs {
            let path = Path::new(dir);
            if !path.exists() {
                continue;
            }
            let entries = std::fs::read_dir(path)
                .with_context(|| format!("failed to read plugin directory {}", path.display()))?;
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }
                let file_name = path
                    .file_name()
                    .and_then(std::ffi::OsStr::to_str)
                    .unwrap_or("");
                if !(file_name.ends_with(".plugin.toml") || file_name.ends_with(".plugin.json")) {
                    continue;
                }
                let raw = std::fs::read_to_string(&path).with_context(|| {
                    format!("failed to read plugin manifest {}", path.display())
                })?;
                let manifest: PluginManifest = if file_name.ends_with(".plugin.toml") {
                    toml::from_str(&raw).with_context(|| {
                        format!("failed to parse plugin TOML manifest {}", path.display())
                    })?
                } else {
                    serde_json::from_str(&raw).with_context(|| {
                        format!("failed to parse plugin JSON manifest {}", path.display())
                    })?
                };
                let manifest = self.load_manifest(manifest)?;
                registry.register(manifest);
            }
        }
        Ok(registry)
    }
}

#[derive(Debug, Serialize)]
struct ProviderPluginRequest<'a> {
    provider: &'a str,
    system_prompt: Option<&'a str>,
    message: &'a str,
    model: &'a str,
    temperature: f64,
}

#[derive(Debug, Deserialize)]
struct ProviderPluginResponse {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

fn instantiate_module(
    module_path: &str,
) -> Result<(
    Store<()>,
    Instance,
    Memory,
    TypedFunc<i32, i32>,
    TypedFunc<(i32, i32), ()>,
)> {
    let engine = Engine::default();
    let module = Module::from_file(&engine, module_path)
        .with_context(|| format!("failed to load wasm module {module_path}"))?;
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[])
        .with_context(|| format!("failed to instantiate wasm module {module_path}"))?;
    let memory = match instance.get_export(&mut store, "memory") {
        Some(Extern::Memory(memory)) => memory,
        _ => anyhow::bail!("wasm module '{module_path}' missing exported memory"),
    };
    let alloc = instance
        .get_typed_func::<i32, i32>(&mut store, ABI_ALLOC_FN)
        .with_context(|| format!("wasm module '{module_path}' missing '{ABI_ALLOC_FN}'"))?;
    let dealloc = instance
        .get_typed_func::<(i32, i32), ()>(&mut store, ABI_DEALLOC_FN)
        .with_context(|| format!("wasm module '{module_path}' missing '{ABI_DEALLOC_FN}'"))?;
    Ok((store, instance, memory, alloc, dealloc))
}

fn write_guest_bytes(
    store: &mut Store<()>,
    memory: &Memory,
    alloc: &TypedFunc<i32, i32>,
    bytes: &[u8],
) -> Result<(i32, i32)> {
    let len_i32 = i32::try_from(bytes.len()).context("input too large for wasm ABI i32 length")?;
    let ptr = alloc
        .call(&mut *store, len_i32)
        .context("wasm alloc call failed")?;
    let ptr_usize = usize::try_from(ptr).context("wasm alloc returned invalid pointer")?;
    memory
        .write(&mut *store, ptr_usize, bytes)
        .context("failed to write input bytes into wasm memory")?;
    Ok((ptr, len_i32))
}

fn read_guest_bytes(store: &mut Store<()>, memory: &Memory, ptr: i32, len: i32) -> Result<Vec<u8>> {
    if ptr < 0 || len < 0 {
        anyhow::bail!("wasm ABI returned negative ptr/len");
    }
    let ptr_usize = usize::try_from(ptr).context("invalid output pointer")?;
    let len_usize = usize::try_from(len).context("invalid output length")?;
    let end = ptr_usize
        .checked_add(len_usize)
        .context("overflow in output range")?;
    if end > memory.data_size(&mut *store) {
        anyhow::bail!("output range exceeds wasm memory bounds");
    }
    let mut out = vec![0u8; len_usize];
    memory
        .read(&mut *store, ptr_usize, &mut out)
        .context("failed to read wasm output bytes")?;
    Ok(out)
}

fn unpack_ptr_len(packed: i64) -> Result<(i32, i32)> {
    let raw = u64::try_from(packed).context("wasm ABI returned negative packed ptr/len")?;
    let ptr_u32 = (raw >> 32) as u32;
    let len_u32 = (raw & 0xffff_ffff) as u32;
    let ptr = i32::try_from(ptr_u32).context("ptr out of i32 range")?;
    let len = i32::try_from(len_u32).context("len out of i32 range")?;
    Ok((ptr, len))
}

fn call_wasm_json(module_path: &str, fn_name: &str, input_json: &str) -> Result<String> {
    if input_json.len() > MAX_WASM_PAYLOAD_BYTES_FALLBACK {
        anyhow::bail!("wasm input payload exceeds safety limit");
    }
    let (mut store, instance, memory, alloc, dealloc) = instantiate_module(module_path)?;
    let call = instance
        .get_typed_func::<(i32, i32), i64>(&mut store, fn_name)
        .with_context(|| format!("wasm module '{module_path}' missing '{fn_name}'"))?;

    let (in_ptr, in_len) = write_guest_bytes(&mut store, &memory, &alloc, input_json.as_bytes())?;
    let packed = call
        .call(&mut store, (in_ptr, in_len))
        .with_context(|| format!("wasm function '{fn_name}' failed"))?;
    let _ = dealloc.call(&mut store, (in_ptr, in_len));

    let (out_ptr, out_len) = unpack_ptr_len(packed)?;
    if usize::try_from(out_len).unwrap_or(usize::MAX) > MAX_WASM_PAYLOAD_BYTES_FALLBACK {
        anyhow::bail!("wasm output payload exceeds safety limit");
    }
    let out_bytes = read_guest_bytes(&mut store, &memory, out_ptr, out_len)?;
    let _ = dealloc.call(&mut store, (out_ptr, out_len));

    String::from_utf8(out_bytes).context("wasm function returned non-utf8 output")
}

fn semaphore_cell() -> &'static RwLock<Arc<Semaphore>> {
    static CELL: OnceLock<RwLock<Arc<Semaphore>>> = OnceLock::new();
    CELL.get_or_init(|| RwLock::new(Arc::new(Semaphore::new(8))))
}

#[derive(Debug, Clone, Copy)]
struct PluginExecutionLimits {
    invoke_timeout_ms: u64,
    memory_limit_bytes: u64,
}

fn current_limits() -> PluginExecutionLimits {
    let guard = registry_cell()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard.limits
}

async fn call_wasm_json_limited(
    module_path: String,
    fn_name: &'static str,
    payload: String,
) -> Result<String> {
    let limits = current_limits();
    let permit = {
        let sem = semaphore_cell()
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        sem.acquire_owned()
            .await
            .context("plugin concurrency limiter closed")?
    };
    let max_by_config = usize::try_from(limits.memory_limit_bytes).unwrap_or(usize::MAX);
    let max_payload = max_by_config.min(MAX_WASM_PAYLOAD_BYTES_FALLBACK);
    if payload.len() > max_payload {
        anyhow::bail!("plugin payload exceeds configured memory limit");
    }

    let handle = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        call_wasm_json(&module_path, fn_name, &payload)
    });
    let result = timeout(Duration::from_millis(limits.invoke_timeout_ms), handle)
        .await
        .context("plugin invocation timed out")?
        .context("plugin blocking task join failed")??;
    Ok(result)
}

pub async fn execute_plugin_tool(tool_name: &str, args: &Value) -> Result<ToolResult> {
    let registry = current_registry();
    let module_path = registry
        .tool_module_path(tool_name)
        .ok_or_else(|| anyhow::anyhow!("plugin tool '{tool_name}' not found in registry"))?
        .to_string();
    let payload = serde_json::json!({
        "tool": tool_name,
        "args": args,
    });
    let output = call_wasm_json_limited(module_path, ABI_TOOL_EXEC_FN, payload.to_string()).await?;
    if let Ok(parsed) = serde_json::from_str::<ToolResult>(&output) {
        return Ok(parsed);
    }
    Ok(ToolResult {
        success: true,
        output,
        error: None,
    })
}

pub async fn execute_plugin_provider_chat(
    provider_name: &str,
    system_prompt: Option<&str>,
    message: &str,
    model: &str,
    temperature: f64,
) -> Result<String> {
    let registry = current_registry();
    let module_path = registry
        .provider_module_path(provider_name)
        .ok_or_else(|| anyhow::anyhow!("plugin provider '{provider_name}' not found in registry"))?
        .to_string();
    let request = ProviderPluginRequest {
        provider: provider_name,
        system_prompt,
        message,
        model,
        temperature,
    };
    let output = call_wasm_json_limited(
        module_path,
        ABI_PROVIDER_CHAT_FN,
        serde_json::to_string(&request)?,
    )
    .await?;
    if let Ok(parsed) = serde_json::from_str::<ProviderPluginResponse>(&output) {
        if let Some(error) = parsed.error {
            anyhow::bail!("plugin provider error: {error}");
        }
        return Ok(parsed.text.unwrap_or_default());
    }
    Ok(output)
}

fn registry_cell() -> &'static RwLock<RuntimeState> {
    static CELL: OnceLock<RwLock<RuntimeState>> = OnceLock::new();
    CELL.get_or_init(|| RwLock::new(RuntimeState::default()))
}

#[derive(Debug, Clone)]
struct RuntimeState {
    registry: PluginRegistry,
    hot_reload: bool,
    config: Option<PluginsConfig>,
    fingerprints: HashMap<String, SystemTime>,
    limits: PluginExecutionLimits,
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self {
            registry: PluginRegistry::default(),
            hot_reload: false,
            config: None,
            fingerprints: HashMap::new(),
            limits: PluginExecutionLimits {
                invoke_timeout_ms: 2_000,
                memory_limit_bytes: 64 * 1024 * 1024,
            },
        }
    }
}

fn collect_manifest_fingerprints(dirs: &[String]) -> HashMap<String, SystemTime> {
    let mut out = HashMap::new();
    for dir in dirs {
        let path = Path::new(dir);
        let Ok(entries) = std::fs::read_dir(path) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let file_name = path
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                .unwrap_or("");
            if !(file_name.ends_with(".plugin.toml") || file_name.ends_with(".plugin.json")) {
                continue;
            }
            if let Ok(metadata) = std::fs::metadata(&path) {
                if let Ok(modified) = metadata.modified() {
                    out.insert(path.to_string_lossy().to_string(), modified);
                }
            }
        }
    }
    out
}

fn maybe_hot_reload() {
    let (hot_reload, config, previous_fingerprints) = {
        let guard = registry_cell()
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (
            guard.hot_reload,
            guard.config.clone(),
            guard.fingerprints.clone(),
        )
    };
    if !hot_reload {
        return;
    }
    let Some(config) = config else {
        return;
    };
    let current_fingerprints = collect_manifest_fingerprints(&config.dirs);
    if current_fingerprints == previous_fingerprints {
        return;
    }

    let runtime = PluginRuntime::new();
    let load_result = runtime.load_registry_from_config(&config);
    if let Ok(new_registry) = load_result {
        let mut guard = registry_cell()
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.registry = new_registry;
        guard.fingerprints = current_fingerprints;
    }
}

pub fn initialize_from_config(config: &PluginsConfig) -> Result<()> {
    let runtime = PluginRuntime::new();
    let registry = runtime.load_registry_from_config(config)?;
    let fingerprints = collect_manifest_fingerprints(&config.dirs);
    let mut guard = registry_cell()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard.registry = registry;
    guard.hot_reload = config.hot_reload;
    guard.config = Some(config.clone());
    guard.fingerprints = fingerprints;
    guard.limits = PluginExecutionLimits {
        invoke_timeout_ms: config.limits.invoke_timeout_ms,
        memory_limit_bytes: config.limits.memory_limit_bytes,
    };
    let mut sem_guard = semaphore_cell()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *sem_guard = Arc::new(Semaphore::new(config.limits.max_concurrency.max(1)));
    Ok(())
}

pub fn current_registry() -> PluginRegistry {
    maybe_hot_reload();
    registry_cell()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .registry
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn runtime_rejects_invalid_manifest() {
        let runtime = PluginRuntime::new();
        assert!(runtime.load_manifest(PluginManifest::default()).is_err());
    }

    #[test]
    fn runtime_loads_plugin_manifest_files() {
        let dir = TempDir::new().expect("temp dir");
        let manifest_path = dir.path().join("demo.plugin.toml");
        std::fs::write(
            &manifest_path,
            r#"
id = "demo"
version = "1.0.0"
module_path = "plugins/demo.wasm"
wit_packages = ["zeroclaw:tools@1.0.0"]
providers = ["demo-provider"]

[[tools]]
name = "demo_tool"
description = "demo tool"
"#,
        )
        .expect("write manifest");

        let runtime = PluginRuntime::new();
        let cfg = PluginsConfig {
            enabled: true,
            dirs: vec![dir.path().to_string_lossy().to_string()],
            ..PluginsConfig::default()
        };
        let reg = runtime
            .load_registry_from_config(&cfg)
            .expect("load registry");
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.tools().len(), 1);
        assert!(reg.has_provider("demo-provider"));
        assert!(reg.tool_module_path("demo_tool").is_some());
        assert!(reg.provider_module_path("demo-provider").is_some());
    }

    #[test]
    fn unpack_ptr_len_roundtrip() {
        let ptr: u32 = 0x1234_5678;
        let len: u32 = 0x0000_0100;
        let packed = ((u64::from(ptr)) << 32) | u64::from(len);
        let (decoded_ptr, decoded_len) = unpack_ptr_len(packed as i64).expect("unpack");
        assert_eq!(decoded_ptr as u32, ptr);
        assert_eq!(decoded_len as u32, len);
    }
}
