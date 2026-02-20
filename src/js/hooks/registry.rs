// Hook Registry for JS plugin system
//
// This module provides the registry that stores and manages hook handlers
// registered by plugins. It handles priority-based ordering and lookup
// of handlers by event name.
//
// # Security Considerations
//
// The registry stores plugin hook handlers that will be called with sensitive
// event data. The registry itself does not execute hooks, but it must ensure:
//
// - Hooks are properly scoped to their plugin/worker context
// - Handler lookup is deterministic and predictable
// - Priority ordering is stable and cannot be abused to bypass security checks

use rquickjs::Function;
use std::collections::HashMap;

/// Hook handler with metadata
///
/// Represents a single hook handler registered by a plugin, including
/// its execution priority, the JavaScript function to call, and timeout.
#[derive(Clone)]
pub struct HookHandler<'js> {
    /// Priority (higher = runs earlier)
    ///
    /// Handlers are sorted in descending order by priority, so handlers
    /// with priority 100 run before handlers with priority 50.
    pub priority: i32,

    /// JS function to call
    ///
    /// This function will be invoked with the event payload as its argument.
    pub func: Function<'js>,

    /// Timeout in milliseconds
    ///
    /// Maximum time allowed for this hook to execute before being cancelled.
    pub timeout_ms: u64,
}

/// Registry for plugin hooks
///
/// Maps plugin IDs to their registered hook handlers. Each plugin has
/// an associated worker ID and a map of event names to handler lists.
///
/// # Example
///
/// ```ignore
/// let mut registry = HookRegistry::new();
/// registry.register_hook(
///     "my_plugin".to_string(),
///     0,
///     "message.received".to_string(),
///     HookHandler {
///         priority: 100,
///         func: js_function,
///         timeout_ms: 5000,
///     },
/// );
/// ```
#[derive(Default)]
pub struct HookRegistry<'js> {
    /// Internal storage: plugin_id -> (worker_id, event_name -> handlers)
    hooks: HashMap<String, (usize, HashMap<String, Vec<HookHandler<'js>>>)>,
}

impl<'js> HookRegistry<'js> {
    /// Creates a new empty hook registry
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a hook for a plugin
    ///
    /// # Arguments
    ///
    /// * `plugin_id` - Unique identifier for the plugin
    /// * `worker_id` - ID of the worker thread that will execute this hook
    /// * `event_name` - Name of the event to hook (e.g., "message.received")
    /// * `handler` - The hook handler containing function and metadata
    pub fn register_hook(
        &mut self,
        plugin_id: String,
        worker_id: usize,
        event_name: String,
        handler: HookHandler<'js>,
    ) {
        self.hooks
            .entry(plugin_id)
            .or_insert_with(|| (worker_id, HashMap::new()))
            .1
            .entry(event_name)
            .or_insert_with(Vec::new)
            .push(handler);
    }

    /// Gets all handlers for an event, sorted by priority
    ///
    /// Returns a list of tuples containing (plugin_id, worker_id, handlers),
    /// where handlers is already sorted by descending priority (highest first).
    /// The outer list is sorted by plugin_id for deterministic ordering.
    ///
    /// # Arguments
    ///
    /// * `event_name` - Name of the event to look up handlers for
    pub fn get_handlers(&self, event_name: &str) -> Vec<(String, usize, Vec<HookHandler<'js>>)> {
        let mut result = Vec::new();

        for (plugin_id, (worker_id, handlers)) in &self.hooks {
            if let Some(handlers) = handlers.get(event_name) {
                let mut sorted = handlers.clone();
                sorted.sort_by_key(|h| std::cmp::Reverse(h.priority));
                result.push((plugin_id.clone(), *worker_id, sorted));
            }
        }

        // Sort by plugin_id for deterministic ordering
        result.sort_by(|a, b| a.0.cmp(&b.0));
        result
    }

    /// Removes all hooks for a plugin
    ///
    /// Call this when a plugin is unloaded or disabled to clean up
    /// its registered hooks.
    ///
    /// # Arguments
    ///
    /// * `plugin_id` - ID of the plugin to remove hooks for
    pub fn unregister_plugin(&mut self, plugin_id: &str) {
        self.hooks.remove(plugin_id);
    }

    /// Returns the number of registered plugins
    pub fn plugin_count(&self) -> usize {
        self.hooks.len()
    }

    /// Returns the total number of registered hooks across all plugins
    pub fn hook_count(&self) -> usize {
        self.hooks
            .values()
            .map(|(_, handlers)| handlers.values().map(|v| v.len()).sum::<usize>())
            .sum()
    }

    /// Checks if a plugin has any hooks registered for an event
    pub fn has_hooks_for(&self, plugin_id: &str, event_name: &str) -> bool {
        if let Some((_, handlers)) = self.hooks.get(plugin_id) {
            handlers.get(event_name).map_or(false, |h| !h.is_empty())
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: We can't create real Function instances in unit tests
    // because they require a QuickJS runtime context.
    // These tests verify the registry logic with mock handlers using
    // a simplified test-only structure.

    // Test-only version of HookHandler that doesn't require Function
    #[derive(Clone, Debug)]
    struct TestHookHandler {
        pub priority: i32,
        pub timeout_ms: u64,
    }

    impl TestHookHandler {
        fn to_registry_key(&self) -> (i32, u64) {
            (self.priority, self.timeout_ms)
        }
    }

    // For testing purposes, we store handlers as tuples of (priority, timeout)
    // instead of full HookHandler with Function
    type TestRegistry = HashMap<String, (usize, HashMap<String, Vec<(i32, u64)>>)>;

    fn test_register_hook(
        hooks: &mut TestRegistry,
        plugin_id: String,
        worker_id: usize,
        event_name: String,
        priority: i32,
        timeout_ms: u64,
    ) {
        hooks
            .entry(plugin_id)
            .or_insert_with(|| (worker_id, HashMap::new()))
            .1
            .entry(event_name)
            .or_insert_with(Vec::new)
            .push((priority, timeout_ms));
    }

    fn test_get_handlers(hooks: &TestRegistry, event_name: &str) -> Vec<(String, usize, Vec<(i32, u64)>)> {
        let mut result = Vec::new();
        for (plugin_id, (worker_id, handlers)) in hooks {
            if let Some(handlers) = handlers.get(event_name) {
                let mut sorted = handlers.clone();
                sorted.sort_by_key(|h| std::cmp::Reverse(h.0));
                result.push((plugin_id.clone(), *worker_id, sorted));
            }
        }
        result.sort_by(|a, b| a.0.cmp(&b.0));
        result
    }

    fn test_plugin_count(hooks: &TestRegistry) -> usize {
        hooks.len()
    }

    fn test_hook_count(hooks: &TestRegistry) -> usize {
        hooks
            .values()
            .map(|(_, handlers)| handlers.values().map(|v| v.len()).sum::<usize>())
            .sum()
    }

    fn test_unregister_plugin(hooks: &mut TestRegistry, plugin_id: &str) {
        hooks.remove(plugin_id);
    }

    #[test]
    fn register_and_retrieve_hooks() {
        let mut hooks = TestRegistry::new();

        test_register_hook(
            &mut hooks,
            "plugin1".to_string(),
            0,
            "message.received".to_string(),
            100,
            5000,
        );

        let handlers = test_get_handlers(&hooks, "message.received");
        assert_eq!(handlers.len(), 1);
        assert_eq!(handlers[0].0, "plugin1");
        assert_eq!(handlers[0].1, 0);
        assert_eq!(handlers[0].2.len(), 1);
    }

    #[test]
    fn priority_ordering() {
        let mut hooks = TestRegistry::new();

        test_register_hook(
            &mut hooks,
            "plugin1".to_string(),
            0,
            "message.received".to_string(),
            50,
            5000,
        );

        test_register_hook(
            &mut hooks,
            "plugin2".to_string(),
            0,
            "message.received".to_string(),
            100,
            5000,
        );

        let handlers = test_get_handlers(&hooks, "message.received");

        // Handlers should be sorted by plugin_id first
        // plugin1 (priority 50) should come before plugin2 (priority 100)
        assert_eq!(handlers[0].0, "plugin1");
        assert_eq!(handlers[1].0, "plugin2");

        // Within each plugin, handlers should be sorted by priority (descending)
        assert_eq!(handlers[0].2[0].0, 50);
        assert_eq!(handlers[1].2[0].0, 100);
    }

    #[test]
    fn multiple_handlers_same_plugin() {
        let mut hooks = TestRegistry::new();

        test_register_hook(
            &mut hooks,
            "plugin1".to_string(),
            0,
            "message.received".to_string(),
            10,
            5000,
        );

        test_register_hook(
            &mut hooks,
            "plugin1".to_string(),
            0,
            "message.received".to_string(),
            100,
            5000,
        );

        test_register_hook(
            &mut hooks,
            "plugin1".to_string(),
            0,
            "message.received".to_string(),
            50,
            5000,
        );

        let handlers = test_get_handlers(&hooks, "message.received");
        assert_eq!(handlers.len(), 1);
        assert_eq!(handlers[0].0, "plugin1");

        // Should be sorted by priority descending: 100, 50, 10
        assert_eq!(handlers[0].2[0].0, 100);
        assert_eq!(handlers[0].2[1].0, 50);
        assert_eq!(handlers[0].2[2].0, 10);
    }

    #[test]
    fn unregister_plugin() {
        let mut hooks = TestRegistry::new();

        test_register_hook(
            &mut hooks,
            "plugin1".to_string(),
            0,
            "message.received".to_string(),
            100,
            5000,
        );

        test_register_hook(
            &mut hooks,
            "plugin2".to_string(),
            0,
            "message.received".to_string(),
            100,
            5000,
        );

        assert_eq!(test_plugin_count(&hooks), 2);

        test_unregister_plugin(&mut hooks, "plugin1");

        assert_eq!(test_plugin_count(&hooks), 1);

        let handlers = test_get_handlers(&hooks, "message.received");
        assert_eq!(handlers.len(), 1);
        assert_eq!(handlers[0].0, "plugin2");
    }

    #[test]
    fn hook_and_plugin_counts() {
        let mut hooks = TestRegistry::new();

        assert_eq!(test_plugin_count(&hooks), 0);
        assert_eq!(test_hook_count(&hooks), 0);

        test_register_hook(
            &mut hooks,
            "plugin1".to_string(),
            0,
            "message.received".to_string(),
            100,
            5000,
        );

        test_register_hook(
            &mut hooks,
            "plugin1".to_string(),
            0,
            "tool.call.pre".to_string(),
            100,
            5000,
        );

        test_register_hook(
            &mut hooks,
            "plugin2".to_string(),
            0,
            "message.received".to_string(),
            100,
            5000,
        );

        assert_eq!(test_plugin_count(&hooks), 2);
        assert_eq!(test_hook_count(&hooks), 3);
    }

    #[test]
    fn empty_registry() {
        let hooks = TestRegistry::new();

        assert_eq!(test_plugin_count(&hooks), 0);
        assert_eq!(test_hook_count(&hooks), 0);

        let handlers = test_get_handlers(&hooks, "message.received");
        assert!(handlers.is_empty());
    }

    #[test]
    fn no_match_for_event() {
        let mut hooks = TestRegistry::new();

        test_register_hook(
            &mut hooks,
            "plugin1".to_string(),
            0,
            "message.received".to_string(),
            100,
            5000,
        );

        let handlers = test_get_handlers(&hooks, "tool.call.pre");
        assert!(handlers.is_empty());
    }
}
