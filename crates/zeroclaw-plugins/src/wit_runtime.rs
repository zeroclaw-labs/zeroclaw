//! The wasmtime component-model (WIT) tool runtime.
//!
//! Ported from `ironclaw_wasm::runtime`, adapted to zeroclaw's `tool` WIT
//! interface: the guest exports `name`/`description`/`parameters-schema` and an
//! `execute` that returns `result<tool-result, string>` (a tool-result carries
//! its own `success` flag). The engine is shared (`Send + Sync`); a fresh
//! `Store` + instance is built per `execute` for isolation.

use std::time::Instant;

use wasmtime::component::Linker;
use wasmtime::{Config, Engine, Store};

use crate::bindings;
use crate::store::StoreData;
use crate::usage::ResourceUsage;
use crate::wit_config::{
    EPOCH_TICK_INTERVAL, WIT_TOOL_VERSION, WitToolLimits, WitToolRuntimeConfig,
};
use crate::wit_error::WasmError;
use crate::wit_host::WitToolHost;
use crate::wit_types::{PreparedWitTool, WitToolExecution, WitToolRequest};

/// wasmtime component-model runtime for zeroclaw WIT tool plugins.
pub struct WitToolRuntime {
    engine: Engine,
    config: WitToolRuntimeConfig,
}

impl WitToolRuntime {
    pub fn new(config: WitToolRuntimeConfig) -> Result<Self, WasmError> {
        let mut wasmtime_config = Config::new();
        wasmtime_config.wasm_component_model(true);
        wasmtime_config.consume_fuel(true);
        wasmtime_config.epoch_interruption(true);
        wasmtime_config.debug_info(false);

        let engine = Engine::new(&wasmtime_config)
            .map_err(|error| WasmError::EngineCreationFailed(error.to_string()))?;
        spawn_epoch_ticker(engine.clone())?;

        Ok(Self { engine, config })
    }

    pub fn config(&self) -> &WitToolRuntimeConfig {
        &self.config
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Compile a component once and read its metadata from the WIT exports.
    /// `fallback_name` is used only if the component's `name` export is empty.
    pub fn prepare(
        &self,
        fallback_name: &str,
        wasm_bytes: &[u8],
    ) -> Result<PreparedWitTool, WasmError> {
        let component = compile_component(&self.engine, wasm_bytes)?;
        let limits = self.config.default_limits.clone();
        let (name, description, schema) = self.extract_metadata(&component, &limits)?;
        let name = if name.trim().is_empty() {
            fallback_name.to_string()
        } else {
            name
        };

        Ok(PreparedWitTool {
            name,
            description,
            schema,
            component,
            limits,
        })
    }

    /// Execute a prepared tool with a fresh instance and the given host services.
    pub fn execute(
        &self,
        prepared: &PreparedWitTool,
        host: WitToolHost,
        request: WitToolRequest,
    ) -> Result<WitToolExecution, WasmError> {
        let started = Instant::now();
        let (mut store, instance) =
            self.instantiate(&prepared.component, host, &prepared.limits)?;
        let tool = instance.zeroclaw_plugin_tool();
        let result = match tool.call_execute(&mut store, &request.args_json) {
            Ok(result) => result,
            Err(error) => {
                let message = if store.data().deadline_exceeded() {
                    "WASM execution deadline exceeded".to_string()
                } else {
                    error.to_string()
                };
                return Err(execution_failed_with_usage(message, &store, started));
            }
        };
        if store.data().deadline_exceeded() {
            return Err(execution_failed_with_usage(
                "WASM execution deadline exceeded".to_string(),
                &store,
                started,
            ));
        }

        // `result` is `result<tool-result, string>`: Ok carries the tool-result
        // (with its own success flag); Err is the guest reporting a hard failure.
        let (success, output, error) = match result {
            Ok(tool_result) => (tool_result.success, tool_result.output, tool_result.error),
            Err(message) => (false, String::new(), Some(message)),
        };

        let mut usage = store.data().usage.clone();
        usage.wall_clock_ms = elapsed_millis(started);
        usage.output_bytes = output.len().min(u64::MAX as usize) as u64;
        let logs = store.data().logs.clone();

        Ok(WitToolExecution {
            success,
            output,
            error,
            usage,
            logs,
        })
    }

    fn extract_metadata(
        &self,
        component: &wasmtime::component::Component,
        limits: &WitToolLimits,
    ) -> Result<(String, String, serde_json::Value), WasmError> {
        let (mut store, instance) = self.instantiate(component, WitToolHost::deny_all(), limits)?;
        let tool = instance.zeroclaw_plugin_tool();
        let name = tool
            .call_name(&mut store)
            .map_err(|error| WasmError::execution_failed(error.to_string()))?;
        let description = tool
            .call_description(&mut store)
            .map_err(|error| WasmError::execution_failed(error.to_string()))?;
        let schema_json = tool
            .call_parameters_schema(&mut store)
            .map_err(|error| WasmError::execution_failed(error.to_string()))?;
        let schema = serde_json::from_str::<serde_json::Value>(&schema_json)
            .map_err(|error| WasmError::InvalidSchema(error.to_string()))?;
        if !schema.is_object() {
            return Err(WasmError::InvalidSchema(
                "parameters-schema export must return a JSON object".to_string(),
            ));
        }
        Ok((name, description, schema))
    }

    fn instantiate(
        &self,
        component: &wasmtime::component::Component,
        host: WitToolHost,
        limits: &WitToolLimits,
    ) -> Result<(Store<StoreData>, bindings::ToolPlugin), WasmError> {
        let mut store = Store::new(
            &self.engine,
            StoreData::new(host, limits.memory_bytes, limits.timeout),
        );
        configure_store(&mut store, limits)?;
        let linker = create_linker(&self.engine)?;
        let instance = bindings::ToolPlugin::instantiate(&mut store, component, &linker)
            .map_err(|error| classify_instantiation_error(error.to_string()))?;
        Ok((store, instance))
    }
}

impl std::fmt::Debug for WitToolRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WitToolRuntime")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

fn spawn_epoch_ticker(engine: Engine) -> Result<(), WasmError> {
    std::thread::Builder::new()
        .name("zeroclaw-wasm-epoch-ticker".into())
        .spawn(move || {
            loop {
                std::thread::sleep(EPOCH_TICK_INTERVAL);
                engine.increment_epoch();
            }
        })
        .map(|_| ())
        .map_err(|error| WasmError::EngineCreationFailed(error.to_string()))
}

fn elapsed_millis(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

fn execution_failed_with_usage(
    message: String,
    store: &Store<StoreData>,
    started: Instant,
) -> WasmError {
    let mut usage: ResourceUsage = store.data().usage.clone();
    usage.wall_clock_ms = elapsed_millis(started);
    WasmError::ExecutionFailed {
        message,
        usage,
        logs: store.data().logs.clone(),
    }
}

fn configure_store(store: &mut Store<StoreData>, limits: &WitToolLimits) -> Result<(), WasmError> {
    store
        .set_fuel(limits.fuel)
        .map_err(|error| WasmError::StoreConfiguration(error.to_string()))?;
    store.epoch_deadline_trap();
    let ticks = (limits.timeout.as_millis() / EPOCH_TICK_INTERVAL.as_millis()).max(1) as u64;
    store.set_epoch_deadline(ticks);
    store.limiter(|data| &mut data.limiter);
    Ok(())
}

fn create_linker(engine: &Engine) -> Result<Linker<StoreData>, WasmError> {
    let mut linker = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
        .map_err(|error| WasmError::LinkerConfiguration(error.to_string()))?;
    // The WIT items are `@unstable(feature = plugins-wit-v0)`, so bindgen gates
    // their host imports behind LinkOptions; enable the feature to link them.
    let mut options = bindings::LinkOptions::default();
    options.plugins_wit_v0(true);
    bindings::ToolPlugin::add_to_linker::<_, wasmtime::component::HasSelf<_>>(
        &mut linker,
        &options,
        |state: &mut StoreData| state,
    )
    .map_err(|error| WasmError::LinkerConfiguration(error.to_string()))?;
    Ok(linker)
}

/// Compile a component from bytes. Requires a wasmtime compiler backend
/// (`plugins-wasm-cranelift` or `plugins-wasm-pulley`); `wasmtime::Component::new`
/// does not exist in runtime-only builds. The backend-less variant compiles but
/// fails with a clear message, so `cargo check` of the crate stays green without
/// pulling a JIT.
#[cfg(any(feature = "plugins-wasm-cranelift", feature = "plugins-wasm-pulley"))]
fn compile_component(
    engine: &Engine,
    wasm_bytes: &[u8],
) -> Result<wasmtime::component::Component, WasmError> {
    wasmtime::component::Component::new(engine, wasm_bytes)
        .map_err(|error| WasmError::CompilationFailed(error.to_string()))
}

#[cfg(not(any(feature = "plugins-wasm-cranelift", feature = "plugins-wasm-pulley")))]
fn compile_component(
    _engine: &Engine,
    _wasm_bytes: &[u8],
) -> Result<wasmtime::component::Component, WasmError> {
    Err(WasmError::CompilationFailed(
        "no WASM compiler backend compiled in; rebuild with the \
         `plugins-wasm-cranelift` or `plugins-wasm-pulley` feature"
            .to_string(),
    ))
}

fn classify_instantiation_error(message: String) -> WasmError {
    if message.contains("zeroclaw:plugin") || message.contains("import") {
        WasmError::InstantiationFailed(format!(
            "{message}. This usually means the component was built against a different WIT version than the host supports (host: {WIT_TOOL_VERSION})."
        ))
    } else {
        WasmError::InstantiationFailed(message)
    }
}
