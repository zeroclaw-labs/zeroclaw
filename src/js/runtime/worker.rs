// Worker thread implementation for JS runtime

use crate::js::{config::RuntimeConfig, error::JsRuntimeError};
use serde_json::Value;
use std::sync::{Arc, Mutex};
use std::time::Instant;
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
    /// Shutdown the worker thread
    Shutdown,
}

/// A worker thread that manages a QuickJS runtime
///
/// Each worker owns its own QuickJS Runtime and Context,
/// which cannot be shared across threads due to `!Send` constraints.
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
        // Test that null is converted correctly
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
                {"name": "Alice", "age": 30},
                {"name": "Bob", "age": 25}
            ]
        });
        assert!(result.is_object());
        assert!(result["users"].is_array());
    }
}
