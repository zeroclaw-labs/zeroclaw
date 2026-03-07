use parking_lot::{const_mutex, Mutex};

// Serialize tests that mutate process-global plugin runtime state.
pub(crate) static PLUGIN_RUNTIME_LOCK: Mutex<()> = const_mutex(());
