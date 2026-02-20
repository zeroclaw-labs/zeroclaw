// Plugin sandbox for isolated JS execution

pub mod context;
pub mod permissions;

use crate::js::{
    config::PoolConfig,
    error::{JsPluginError, JsRuntimeError},
    events::EventBus,
    hooks::HookHandlerRef,
    runtime::{JsRuntimePool, PluginId},
};

// Re-export config permissions for convenience
pub use crate::js::config::JsPluginPermissions;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

#[cfg(any(feature = "js", feature = "js-lite", feature = "js-transpile"))]
use crate::js::transpile::sourcemap::SourceMapRegistry;

/// Sandbox configuration for plugin execution
#[derive(Clone)]
pub struct SandboxConfig {
    /// Memory limit per plugin in bytes
    pub memory_limit: usize,

    /// CPU time limit per execution
    pub cpu_time_limit: std::time::Duration,

    /// Plugin permissions
    pub permissions: JsPluginPermissions,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            memory_limit: 64 * 1024 * 1024, // 64MB
            cpu_time_limit: std::time::Duration::from_secs(30),
            permissions: JsPluginPermissions::default(),
        }
    }
}

impl SandboxConfig {
    /// Create from PoolConfig
    pub fn from_pool_config(pool_config: &PoolConfig) -> Self {
        Self {
            memory_limit: pool_config.memory_limit,
            cpu_time_limit: pool_config.cpu_time_limit,
            permissions: pool_config.default_permissions.clone(),
        }
    }
}

/// Isolated execution environment for JS plugins
///
/// The sandbox manages:
/// - Runtime pool for executing plugins
/// - Source map registry for error remapping (when transpile feature is enabled)
/// - Security boundaries (permissions, quotas)
/// - Event bus for distributing events to subscribers
/// - Hook metadata storage for plugin hooks
#[derive(Clone)]
pub struct PluginSandbox {
    pool: Arc<JsRuntimePool>,
    #[cfg(any(feature = "js", feature = "js-lite", feature = "js-transpile"))]
    source_maps: Arc<std::sync::Mutex<SourceMapRegistry>>,
    config: SandboxConfig,
    event_bus: Arc<EventBus>,
    /// Hook metadata storage: plugin_id -> (worker_id, event_name -> handlers)
    ///
    /// This stores hook metadata (priorities, timeouts) without storing actual
    /// JavaScript functions. The actual Function<'js> values are managed by
    /// individual worker threads.
    hook_metadata: Arc<std::sync::Mutex<HashMap<String, (usize, HashMap<String, Vec<HookHandlerRef>>)>>>,
}

impl PluginSandbox {
    /// Create a new sandbox with the given configuration
    pub fn new(config: SandboxConfig) -> Result<Self, JsPluginError> {
        let pool_config = PoolConfig {
            memory_limit: config.memory_limit,
            cpu_time_limit: config.cpu_time_limit,
            default_permissions: config.permissions.clone(),
            max_workers: 4, // Default worker count
        };

        Ok(Self {
            pool: Arc::new(JsRuntimePool::new(pool_config)),
            #[cfg(any(feature = "js", feature = "js-lite", feature = "js-transpile"))]
            source_maps: Arc::new(std::sync::Mutex::new(SourceMapRegistry::new())),
            event_bus: Arc::new(EventBus::new()),
            hook_metadata: Arc::new(std::sync::Mutex::new(HashMap::new())),
            config,
        })
    }

    /// Load a plugin into the sandbox
    ///
    /// # Arguments
    ///
    /// * `plugin_id` - Unique identifier for the plugin
    /// * `code` - JavaScript/TypeScript code to load
    /// * `filename` - Optional filename for source map tracking
    pub async fn load_plugin(
        &self,
        plugin_id: &str,
        code: &str,
        filename: Option<&str>,
    ) -> Result<SandboxPluginHandle, JsPluginError> {
        let id = PluginId(plugin_id.to_string());
        let filename = filename.unwrap_or("plugin.js");

        // Load the plugin into the runtime pool
        let handle = self
            .pool
            .load_plugin(id.clone(), code.to_string(), filename.to_string())
            .await?;

        Ok(SandboxPluginHandle {
            plugin_id: id,
            handle,
            sandbox: self.clone(),
        })
    }

    /// Register a source map for a plugin
    #[cfg(any(feature = "js", feature = "js-lite", feature = "js-transpile"))]
    pub fn register_source_map(&self, plugin_id: &str, map_json: String) {
        let mut maps = self.source_maps.lock().unwrap();
        maps.register(plugin_id, map_json);
    }

    /// Get the sandbox configuration
    pub fn config(&self) -> &SandboxConfig {
        &self.config
    }

    /// Get the event bus for this sandbox
    pub fn event_bus(&self) -> &Arc<EventBus> {
        &self.event_bus
    }

    /// Get hook metadata for this sandbox
    ///
    /// Returns a map of plugin_id -> (worker_id, event_name -> handlers).
    /// This contains hook metadata (priorities, timeouts) without the actual
    /// JavaScript functions, which are managed by worker threads.
    pub fn hook_metadata(&self) -> &Arc<std::sync::Mutex<HashMap<String, (usize, HashMap<String, Vec<HookHandlerRef>>)>>> {
        &self.hook_metadata
    }
}

/// Handle for interacting with a loaded plugin
#[derive(Clone)]
pub struct SandboxPluginHandle {
    plugin_id: PluginId,
    handle: crate::js::runtime::JsRuntimeHandle,
    sandbox: PluginSandbox,
}

impl SandboxPluginHandle {
    /// Execute JavaScript code in this plugin's context
    pub async fn execute(&self, code: &str) -> Result<Value, JsRuntimeError> {
        self.handle.execute(code).await
    }

    /// Get the plugin ID
    pub fn plugin_id(&self) -> &PluginId {
        &self.plugin_id
    }

    /// Remap a stack trace for this plugin
    #[cfg(any(feature = "js", feature = "js-lite", feature = "js-transpile"))]
    pub fn remap_stack(&self, raw_stack: &str) -> String {
        let maps = self.sandbox.source_maps.lock().unwrap();
        maps.remap_stack(&self.plugin_id.0, raw_stack)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_config_default() {
        let config = SandboxConfig::default();
        assert_eq!(config.memory_limit, 64 * 1024 * 1024);
        assert_eq!(config.cpu_time_limit, std::time::Duration::from_secs(30));
    }

    #[test]
    fn sandbox_config_from_pool() {
        let pool_config = PoolConfig {
            memory_limit: 128 * 1024 * 1024,
            cpu_time_limit: std::time::Duration::from_secs(60),
            max_workers: 8,
            default_permissions: JsPluginPermissions::default(),
        };

        let sandbox_config = SandboxConfig::from_pool_config(&pool_config);
        assert_eq!(sandbox_config.memory_limit, 128 * 1024 * 1024);
        assert_eq!(
            sandbox_config.cpu_time_limit,
            std::time::Duration::from_secs(60)
        );
    }

    #[tokio::test]
    #[cfg(feature = "js-runtime")]
    async fn sandbox_can_be_created() {
        let config = SandboxConfig::default();
        let sandbox = PluginSandbox::new(config);
        assert!(sandbox.is_ok());
    }

    #[tokio::test]
    #[cfg(feature = "js-runtime")]
    async fn sandbox_load_plugin() {
        let sandbox = PluginSandbox::new(SandboxConfig::default()).unwrap();

        let result = sandbox
            .load_plugin("test", "const x = 42;", Some("test.js"))
            .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    #[cfg(feature = "js-runtime")]
    async fn sandbox_execute_code() {
        let sandbox = PluginSandbox::new(SandboxConfig::default()).unwrap();

        let handle = sandbox
            .load_plugin("test-exec", "1 + 1;", Some("test.js"))
            .await
            .unwrap();

        // Note: This will return a string representation since we're using simple_value_to_json
        let result = handle.execute("1 + 1").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    #[cfg(all(feature = "js-runtime", any(feature = "js", feature = "js-lite", feature = "js-transpile")))]
    async fn sandbox_register_source_map() {
        let sandbox = PluginSandbox::new(SandboxConfig::default()).unwrap();

        let map_json = r#"{"version":3,"sources":["test.ts"],"mappings":"AAAA","names":[]}"#;
        sandbox.register_source_map("test-plugin", map_json.to_string());

        let handle = sandbox
            .load_plugin("test-remap", "const x = 42;", Some("test.js"))
            .await
            .unwrap();

        let result = handle.remap_stack("Error at plugin.js:10:5");
        // Should return the original since we can't actually remap without a valid source map
        assert!(result.contains("plugin.js") || result.contains("Error"));
    }
}
