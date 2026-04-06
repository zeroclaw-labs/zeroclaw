use std::sync::{Arc, LazyLock, Mutex};

/// Callback type for checking if model has been switched during tool execution.
/// Returns Some((provider, model)) if a switch was requested, None otherwise.
pub(crate) type ModelSwitchCallback = Arc<Mutex<Option<(String, String)>>>;

/// Global model switch request state - used for runtime model switching via model_switch tool.
/// This is set by the model_switch tool and checked by the agent loop.
#[allow(clippy::type_complexity)]
static MODEL_SWITCH_REQUEST: LazyLock<Arc<Mutex<Option<(String, String)>>>> =
    LazyLock::new(|| Arc::new(Mutex::new(None)));

/// Get the global model switch request state
pub(crate) fn get_model_switch_state() -> ModelSwitchCallback {
    Arc::clone(&MODEL_SWITCH_REQUEST)
}

/// Clear any pending model switch request
pub(crate) fn clear_model_switch_request() {
    if let Ok(guard) = MODEL_SWITCH_REQUEST.lock() {
        let mut guard = guard;
        *guard = None;
    }
}

#[derive(Debug)]
pub(crate) struct ModelSwitchRequested {
    pub provider: String,
    pub model: String,
}

impl std::fmt::Display for ModelSwitchRequested {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "model switch requested to {} {}",
            self.provider, self.model
        )
    }
}

impl std::error::Error for ModelSwitchRequested {}

pub(crate) fn is_model_switch_requested(err: &anyhow::Error) -> Option<(String, String)> {
    err.chain()
        .filter_map(|source| source.downcast_ref::<ModelSwitchRequested>())
        .map(|e| (e.provider.clone(), e.model.clone()))
        .next()
}
