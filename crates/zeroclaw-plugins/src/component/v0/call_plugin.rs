// ── Helper macro for plugin calls ─────────────────────────────────────
#[macro_export]
macro_rules! call_plugin {
    ($self:expr, $op:literal, $block:expr) => {{
        let state = Arc::clone(&$self.state);
        let plugin_name = $self.plugin_name.clone();
        let plugin_version = $self.plugin_version.clone();
        let mut guard = state.lock().await;
        let (ref mut store, ref mut bindings) = *guard;
        super::wrap_plugin::wrap_plugin_call(
            &plugin_name,
            &plugin_version,
            $op,
            $block(store, bindings),
        )
        .await
    }};
}
