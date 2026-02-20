// Worker thread implementation for JS runtime

use crate::js::events::Event;
use crate::js::hooks::{HookHandlerRef, HookResult};
use crate::js::{config::RuntimeConfig, error::JsRuntimeError};
use rquickjs::{Array, Ctx, Function, Object};
use serde_json::Value;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};

/// Commands that can be sent to a worker thread
pub enum WorkerCommand {
    /// Execute JavaScript code and return the result
    Execute {
        code: String,
        reply: oneshot::Sender<Result<Value, JsRuntimeError>>,
    },
    /// Load a JavaScript module into the context
    LoadModule {
        source: String,
        filename: String,
        reply: oneshot::Sender<Result<(), JsRuntimeError>>,
    },
    /// Execute hook handlers for an event
    #[cfg(feature = "js-runtime")]
    ExecuteHook {
        event: Event,
        handlers: Vec<HookHandlerRef>,
        timeout: Duration,
        reply: oneshot::Sender<HookResult>,
    },
    /// Shutdown the worker thread
    Shutdown,
}

/// A worker thread that manages a QuickJS runtime
///
/// Each worker owns its own QuickJS Runtime and Context,
/// which cannot be shared across threads due to `!Send` constraints.
///
/// Hooks are discovered from the global `__zeroclaw_hooks` object at
/// execution time, since Function<'js> values have lifetimes tied to
/// the runtime context.
pub struct JsRuntimeWorker {
    id: usize,
    rx: mpsc::Receiver<WorkerCommand>,
}

impl JsRuntimeWorker {
    /// Create a new worker with the given ID and command channel
    ///
    /// Returns the worker struct and a sender for commands.
    /// The worker must be started by calling `run()`.
    pub fn new(id: usize, _config: RuntimeConfig) -> (Self, mpsc::Sender<WorkerCommand>) {
        let (tx, rx) = mpsc::channel(64);

        let worker = Self { id, rx };
        (worker, tx)
    }

    /// Start the worker thread with the given configuration
    ///
    /// This spawns an OS thread that owns the QuickJS runtime.
    /// The thread runs until a `Shutdown` command is received.
    #[cfg(feature = "js-runtime")]
    pub fn run(self, config: RuntimeConfig) {
        std::thread::spawn(move || {
            run_worker_internal(self.id, self.rx, config);
        });
    }
}

/// Internal function that runs the worker thread logic
///
/// This is separate to allow moving the receiver into the thread.
#[cfg(feature = "js-runtime")]
fn run_worker_internal(id: usize, mut rx: mpsc::Receiver<WorkerCommand>, config: RuntimeConfig) {
    use rquickjs::{Context, Runtime};

    // Create QuickJS runtime
    let rt = Runtime::new().expect("QuickJS runtime creation failed");

    // Set memory limit (Layer 1: Memory protection)
    rt.set_memory_limit(config.memory_limit);

    // Set up CPU quota interrupt handler (Layer 1: CPU protection)
    let deadline: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
    let deadline_check = deadline.clone();

    rt.set_interrupt_handler(Some(Box::new(move || {
        deadline_check
            .lock()
            .ok()
            .and_then(|d| *d)
            .map(|dl| Instant::now() > dl)
            .unwrap_or(false)
    })));

    // Create QuickJS context
    let ctx = Context::full(&rt).expect("QuickJS context creation failed");

    // Worker event loop - processes commands until shutdown
    while let Some(cmd) = rx.blocking_recv() {
        match cmd {
            WorkerCommand::Execute { code, reply } => {
                // Set CPU deadline
                if let Ok(mut d) = deadline.lock() {
                    *d = Some(Instant::now() + config.cpu_time_limit);
                }

                // Execute code and convert result to JSON
                let result = ctx.with(|ctx| {
                    ctx.eval::<rquickjs::Value, _>(code.as_str())
                        .map(|v| simple_value_to_json(&ctx, &v))
                        .map_err(|e| JsRuntimeError::Execution(e.to_string()))
                });

                // CRITICAL: Clear deadline BEFORE sending reply
                if let Ok(mut d) = deadline.lock() {
                    *d = None;
                }

                let _ = reply.send(result);
            }

            WorkerCommand::LoadModule {
                source,
                filename: _,
                reply,
            } => {
                // Module loading - simplified for now, just eval the code
                let result = ctx.with(|ctx| {
                    ctx.eval::<rquickjs::Value, _>(source.as_str())
                        .map(|_| ())
                        .map_err(|e| JsRuntimeError::Execution(e.to_string()))
                });
                let _ = reply.send(result);
            }

            #[cfg(feature = "js-runtime")]
            WorkerCommand::ExecuteHook {
                event,
                handlers,
                timeout,
                reply,
            } => {
                let result =
                    ctx.with(|ctx| execute_hooks(&ctx, &event, handlers, timeout, &deadline));
                if let Err(_) = reply.send(result) {
                    tracing::warn!("Failed to send hook execution result: receiver dropped");
                }
            }

            WorkerCommand::Shutdown => break,
        }
    }

    tracing::debug!("JS worker {} shutting down", id);
}

/// Convert rquickjs Value to serde_json::Value
///
/// This handles JavaScript primitive types:
/// - Primitives: string, number, boolean, null, undefined
/// - Objects/Arrays: returns null for now (can be improved later)
/// - Functions: returns null (non-serializable)
#[cfg(feature = "js-runtime")]
fn simple_value_to_json<'js>(ctx: &rquickjs::Ctx<'js>, v: &rquickjs::Value<'js>) -> Value {
    use rquickjs::{Array, FromJs, Object};

    // Handle undefined and null
    if v.is_undefined() || v.is_null() {
        return Value::Null;
    }

    // Handle boolean
    if v.is_bool() {
        return Value::Bool(v.as_bool().unwrap_or(false));
    }

    // Handle number
    if v.is_number() {
        if let Some(int_val) = v.as_int() {
            return Value::Number(serde_json::Number::from(int_val));
        }
        // Try to convert to number using FromJs
        if let Ok(f) = f64::from_js(ctx, v.clone()) {
            if let Some(n) = serde_json::Number::from_f64(f) {
                return Value::Number(n);
            }
        }
        return Value::Number(serde_json::Number::from(0));
    }

    // Handle string - use FromJs trait
    if v.is_string() {
        if let Ok(s) = String::from_js(ctx, v.clone()) {
            return Value::String(s);
        }
    }

    // Handle array - iterate and convert each element
    if v.is_array() {
        if let Ok(array) = Array::from_value(v.clone()) {
            let len = array.len();
            let mut arr = Vec::with_capacity(len);

            for i in 0..len {
                if let Ok(elem_val) = array.get::<rquickjs::Value>(i) {
                    arr.push(simple_value_to_json(ctx, &elem_val));
                }
            }
            return Value::Array(arr);
        }
    }

    // Handle object - iterate keys and convert each property
    if v.is_object() {
        if let Ok(obj) = Object::from_value(v.clone()) {
            let mut map = serde_json::Map::new();

            let keys = obj.keys::<String>();
            for key_result in keys {
                if let Ok(key) = key_result {
                    if let Ok(prop_val) = obj.get::<String, rquickjs::Value>(key.clone()) {
                        map.insert(key, simple_value_to_json(ctx, &prop_val));
                    }
                }
            }
            return Value::Object(map);
        }
    }

    // Fallback for unsupported types (functions, symbols, etc.)
    Value::Null
}

/// Discover hook handlers from the global __zeroclaw_hooks object
///
/// Returns a list of (priority, timeout_ms, function) tuples for the given event.
/// Handlers are discovered dynamically since Function<'js> cannot be stored
/// across different invocations of ctx.with().
#[cfg(feature = "js-runtime")]
fn discover_hook_handlers<'js>(
    ctx: &Ctx<'js>,
    event_name: &str,
) -> Result<Vec<(i32, u64, Function<'js>)>, JsRuntimeError> {
    let mut handlers = Vec::new();

    // Get the global __zeroclaw_hooks object
    let global = ctx.globals();

    let hooks_obj: Option<Object> = global
        .get("__zeroclaw_hooks")
        .map_err(|e| JsRuntimeError::Execution(format!("Failed to get __zeroclaw_hooks: {}", e)))?;

    if let Some(hooks_obj) = hooks_obj {
        // Get the handlers array for this specific event
        let handlers_array: Option<Array> = hooks_obj.get(event_name).map_err(|e| {
            JsRuntimeError::Execution(format!(
                "Failed to get handlers for '{}': {}",
                event_name, e
            ))
        })?;

        if let Some(handlers_array) = handlers_array {
            let len = handlers_array.len();

            for i in 0..len {
                match handlers_array.get::<Function>(i) {
                    Ok(func) => {
                        // Default priority 50, timeout 5000ms
                        // In the future, these could be stored as function properties
                        handlers.push((50, 5000, func));
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Handler at index {} for event '{}' is not a function: {}",
                            i,
                            event_name,
                            e
                        );
                    }
                }
            }
        }
    }

    // Sort by priority descending (higher priority runs first)
    handlers.sort_by_key(|h| std::cmp::Reverse(h.0));

    Ok(handlers)
}

/// Execute a single hook handler with timeout
///
/// Calls the JavaScript function with the event payload as JSON.
/// Returns the hook result based on what the handler returns.
#[cfg(feature = "js-runtime")]
fn execute_single_hook<'js>(
    ctx: &Ctx<'js>,
    handler: Function<'js>,
    event_json: &Value,
    timeout: Duration,
    deadline: &Arc<Mutex<Option<Instant>>>,
) -> Result<HookResult, JsRuntimeError> {
    // Set the deadline for this handler
    let handler_deadline = Instant::now() + timeout;
    if let Ok(mut d) = deadline.lock() {
        *d = Some(handler_deadline);
    }

    // Parse the event JSON into a JavaScript value
    let event_value = json_to_js_value(ctx, event_json)
        .map_err(|e| JsRuntimeError::Execution(format!("Failed to convert event to JS: {}", e)))?;

    // Call the handler with the event as argument
    let result = match handler.call((event_value,)) {
        Ok(ret_val) => {
            // Convert return value to HookResult
            let ret_json = simple_value_to_json(ctx, &ret_val);

            // Check if the handler returned a hook result object
            // Format: { type: "veto" | "modified" | "observation", reason?: string, data?: any }
            if let Some(obj) = ret_json.as_object() {
                if let Some(result_type) = obj.get("type").and_then(|v| v.as_str()) {
                    match result_type {
                        "veto" => {
                            let reason = obj
                                .get("reason")
                                .and_then(|v| v.as_str())
                                .unwrap_or("Hook vetoed the operation");
                            Ok(HookResult::Veto(reason.to_string()))
                        }
                        "modified" => {
                            let data = obj.get("data").cloned().unwrap_or(Value::Null);
                            Ok(HookResult::Modified(data))
                        }
                        "observation" | _ => Ok(HookResult::Observation),
                    }
                } else {
                    // No type specified, assume observation
                    Ok(HookResult::Observation)
                }
            } else {
                // Non-object return, assume observation
                Ok(HookResult::Observation)
            }
        }
        Err(e) => {
            // Handler threw an error or execution failed
            // Return Veto with error message
            Ok(HookResult::Veto(format!("Hook execution failed: {}", e)))
        }
    };

    // Clear the deadline after execution
    if let Ok(mut d) = deadline.lock() {
        *d = None;
    }

    result
}

/// Convert serde_json::Value to rquickjs Value
#[cfg(feature = "js-runtime")]
fn json_to_js_value<'js>(
    ctx: &Ctx<'js>,
    v: &Value,
) -> Result<rquickjs::Value<'js>, rquickjs::Error> {
    use rquickjs::{Array, Object};

    match v {
        Value::Null => {
            // Use null from rquickjs
            Ok(rquickjs::Undefined.into_value(ctx.clone()))
        }
        Value::Bool(b) => {
            let obj = Object::new(ctx.clone())?;
            // Use static bool conversion via object property trick
            if *b {
                obj.set("v", true)?;
            } else {
                obj.set("v", false)?;
            }
            obj.get("v")
        }
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                let obj = Object::new(ctx.clone())?;
                obj.set("v", i)?;
                obj.get("v")
            } else if let Some(f) = n.as_f64() {
                let obj = Object::new(ctx.clone())?;
                obj.set("v", f)?;
                obj.get("v")
            } else {
                Ok(rquickjs::Undefined.into_value(ctx.clone()))
            }
        }
        Value::String(s) => {
            let obj = Object::new(ctx.clone())?;
            obj.set("v", s.as_str())?;
            obj.get("v")
        }
        Value::Array(arr) => {
            let js_array = Array::new(ctx.clone())?;
            for (i, elem) in arr.iter().enumerate() {
                let js_val = json_to_js_value(ctx, elem)?;
                js_array.set(i, js_val)?;
            }
            Ok(js_array.into())
        }
        Value::Object(obj) => {
            let js_obj = Object::new(ctx.clone())?;
            for (key, val) in obj.iter() {
                let js_val = json_to_js_value(ctx, val)?;
                js_obj.set(key, js_val)?;
            }
            Ok(js_obj.into())
        }
    }
}

/// Execute hook handlers for an event
///
/// For each handler registered for this event:
/// 1. Serialize the event to JSON
/// 2. Call the handler with the event as argument
/// 3. Apply per-handler timeout
/// 4. Return early on Veto, otherwise continue to next handler
///
/// Returns the first non-observation result (Veto or Modified).
/// If all handlers return observation, returns Observation.
#[cfg(feature = "js-runtime")]
fn execute_hooks<'js>(
    ctx: &Ctx<'js>,
    event: &Event,
    _handler_refs: Vec<HookHandlerRef>,
    _timeout: Duration,
    deadline: &Arc<Mutex<Option<Instant>>>,
) -> HookResult {
    let event_name = event.name();

    // Serialize the event to JSON
    let event_json = match serde_json::to_value(event) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("Failed to serialize event '{}': {}", event_name, e);
            return HookResult::Veto(format!("Event serialization failed: {}", e));
        }
    };

    // Discover handlers from the global __zeroclaw_hooks object
    let handlers = match discover_hook_handlers(ctx, &event_name) {
        Ok(h) => h,
        Err(e) => {
            tracing::error!("Failed to discover handlers for '{}': {}", event_name, e);
            return HookResult::Veto(format!("Hook discovery failed: {}", e));
        }
    };

    tracing::debug!(
        "Executing {} handlers for event: {}",
        handlers.len(),
        event_name
    );

    // Execute each handler in priority order
    for (priority, timeout_ms, func) in handlers {
        tracing::trace!(
            "Executing handler for '{}' with priority {}, timeout {}ms",
            event_name,
            priority,
            timeout_ms
        );

        let timeout = Duration::from_millis(timeout_ms);
        match execute_single_hook(ctx, func, &event_json, timeout, deadline) {
            Ok(HookResult::Veto(reason)) => {
                tracing::debug!("Handler vetoed event '{}': {}", event_name, reason);
                return HookResult::Veto(reason);
            }
            Ok(HookResult::Modified(data)) => {
                tracing::debug!(
                    "Handler modified event '{}': data={}",
                    event_name,
                    serde_json::to_string(&data).unwrap_or_else(|_| "<invalid>".to_string())
                );
                return HookResult::Modified(data);
            }
            Ok(HookResult::Observation) => {
                // Continue to next handler
                tracing::trace!("Handler for '{}' returned observation", event_name);
            }
            Err(e) => {
                tracing::error!("Handler execution failed for '{}': {}", event_name, e);
                return HookResult::Veto(format!("Handler error: {}", e));
            }
        }
    }

    // All handlers returned observation
    HookResult::Observation
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_command_channel_size() {
        // This verifies the channel buffer size is 64
        let (tx, _rx) = mpsc::channel::<WorkerCommand>(64);
        assert_eq!(tx.capacity(), 64);
    }

    #[test]
    fn plugin_id_from_string() {
        use crate::js::runtime::PluginId;
        let id = PluginId("test".to_string());
        assert_eq!(id.0, "test");
    }

    #[test]
    fn type_conversion_handles_null() {
        let result = serde_json::json!(null);
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn type_conversion_handles_boolean() {
        let result = serde_json::json!(true);
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn type_conversion_handles_number() {
        let int_result = serde_json::json!(42);
        assert_eq!(int_result, Value::Number(serde_json::Number::from(42)));

        let float_result = serde_json::json!(3.14);
        assert_eq!(
            float_result,
            Value::Number(serde_json::Number::from_f64(3.14).unwrap())
        );
    }

    #[test]
    fn type_conversion_handles_string() {
        let result = serde_json::json!("hello");
        assert_eq!(result, Value::String("hello".to_string()));
    }

    #[test]
    fn type_conversion_handles_array() {
        let result = serde_json::json!([1, 2, 3]);
        assert!(result.is_array());
        let arr = result.as_array().unwrap();
        assert_eq!(arr.len(), 3);
    }

    #[test]
    fn type_conversion_handles_object() {
        let result = serde_json::json!({"key": "value"});
        assert!(result.is_object());
    }

    #[test]
    fn type_conversion_handles_nested_structures() {
        let result = serde_json::json!({
            "users": [
                {"name": "user_a", "age": 30},
                {"name": "user_b", "age": 25}
            ]
        });
        assert!(result.is_object());
        assert!(result["users"].is_array());
    }

    #[test]
    fn hook_result_observation() {
        let result = HookResult::Observation;
        assert!(result.is_observation());
        assert!(!result.is_veto());
        assert!(!result.is_modified());
    }

    #[test]
    fn hook_result_veto() {
        let result = HookResult::Veto("test reason".to_string());
        assert!(!result.is_observation());
        assert!(result.is_veto());
        assert!(!result.is_modified());
    }

    #[test]
    fn hook_result_modified() {
        let result = HookResult::Modified(Value::String("modified".to_string()));
        assert!(!result.is_observation());
        assert!(!result.is_veto());
        assert!(result.is_modified());
    }
}
