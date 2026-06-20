/// Wires a [`crate::tool::ToolPlugin`] implementation into the generated
/// `export!` for the `tool-plugin` world: generates the `tool` and
/// `plugin-info` `Guest` impls (delegating to the trait), then calls the
/// raw `export!` once.
///
/// A single plugin component implements exactly one world. Invoking
/// `export_tool!`/`export_memory!`/`export_channel!` more than once in the
/// same crate fails to compile (each defines the same fixed-name unit
/// struct), which is the desired behavior — it's a plugin-author error to
/// try to export two worlds from one component.
#[macro_export]
macro_rules! export_tool {
    ($ty:ty) => {
        struct __ZeroclawPluginComponent;

        impl $crate::bindings::tool::exports::zeroclaw::plugin::tool::Guest
            for __ZeroclawPluginComponent
        {
            fn name() -> String {
                <$ty as $crate::tool::ToolPlugin>::metadata().name
            }

            fn description() -> String {
                <$ty as $crate::tool::ToolPlugin>::metadata().description
            }

            fn parameters_schema() -> String {
                <$ty as $crate::tool::ToolPlugin>::metadata().parameters_schema
            }

            fn execute(args: String) -> Result<$crate::tool::ToolResult, String> {
                <$ty as $crate::tool::ToolPlugin>::execute(args)
            }
        }

        impl $crate::bindings::tool::exports::zeroclaw::plugin::plugin_info::Guest
            for __ZeroclawPluginComponent
        {
            fn plugin_name() -> String {
                <$ty as $crate::tool::ToolPlugin>::plugin_info().0.to_string()
            }

            fn plugin_version() -> String {
                <$ty as $crate::tool::ToolPlugin>::plugin_info().1.to_string()
            }
        }

        $crate::bindings::tool::export!(__ZeroclawPluginComponent with_types_in $crate::bindings::tool);
    };
}

/// Wires a [`crate::memory::MemoryPlugin`] implementation into the
/// generated `export!` for the `memory-plugin` world.
#[macro_export]
macro_rules! export_memory {
    ($ty:ty) => {
        struct __ZeroclawPluginComponent;

        impl $crate::bindings::memory::exports::zeroclaw::plugin::memory::Guest
            for __ZeroclawPluginComponent
        {
            fn name() -> String {
                <$ty as $crate::memory::MemoryPlugin>::name()
            }

            fn get_memory_capabilities() -> $crate::memory::MemoryCapabilities {
                <$ty as $crate::memory::MemoryPlugin>::get_memory_capabilities()
            }

            fn store_entry(
                key: String,
                content: String,
                category: $crate::memory::MemoryCategory,
                session_id: Option<String>,
            ) -> Result<(), String> {
                <$ty as $crate::memory::MemoryPlugin>::store_entry(key, content, category, session_id)
            }

            fn recall(
                query: String,
                limit: u64,
                session_id: Option<String>,
                since: Option<String>,
                until: Option<String>,
            ) -> Result<Vec<$crate::memory::MemoryEntry>, String> {
                <$ty as $crate::memory::MemoryPlugin>::recall(query, limit, session_id, since, until)
            }

            fn get(key: String) -> Result<Option<$crate::memory::MemoryEntry>, String> {
                <$ty as $crate::memory::MemoryPlugin>::get(key)
            }

            fn list_entries(
                category: Option<$crate::memory::MemoryCategory>,
                session_id: Option<String>,
            ) -> Result<Vec<$crate::memory::MemoryEntry>, String> {
                <$ty as $crate::memory::MemoryPlugin>::list_entries(category, session_id)
            }

            fn forget(key: String) -> Result<bool, String> {
                <$ty as $crate::memory::MemoryPlugin>::forget(key)
            }

            fn forget_for_agent(key: String, agent_id: String) -> Result<bool, String> {
                <$ty as $crate::memory::MemoryPlugin>::forget_for_agent(key, agent_id)
            }

            fn count() -> Result<u64, String> {
                <$ty as $crate::memory::MemoryPlugin>::count()
            }

            fn health_check() -> bool {
                <$ty as $crate::memory::MemoryPlugin>::health_check()
            }

            fn store_with_agent(
                key: String,
                content: String,
                category: $crate::memory::MemoryCategory,
                session_id: Option<String>,
                namespace: Option<String>,
                importance: Option<f64>,
                agent_id: Option<String>,
            ) -> Result<(), String> {
                <$ty as $crate::memory::MemoryPlugin>::store_with_agent(
                    key, content, category, session_id, namespace, importance, agent_id,
                )
            }

            fn recall_for_agents(
                agents: $crate::memory::AgentFilter,
                query: String,
                limit: u64,
                session_id: Option<String>,
                since: Option<String>,
                until: Option<String>,
            ) -> Result<Vec<$crate::memory::MemoryEntry>, String> {
                <$ty as $crate::memory::MemoryPlugin>::recall_for_agents(
                    agents, query, limit, session_id, since, until,
                )
            }

            fn get_for_agent(
                key: String,
                agent_id: String,
            ) -> Result<Option<$crate::memory::MemoryEntry>, String> {
                <$ty as $crate::memory::MemoryPlugin>::get_for_agent(key, agent_id)
            }

            fn purge_namespace(namespace: String) -> Result<u64, String> {
                <$ty as $crate::memory::MemoryPlugin>::purge_namespace(namespace)
            }

            fn purge_session(session_id: String) -> Result<u64, String> {
                <$ty as $crate::memory::MemoryPlugin>::purge_session(session_id)
            }

            fn purge_session_for_agent(
                session_id: String,
                agent_id: String,
            ) -> Result<u64, String> {
                <$ty as $crate::memory::MemoryPlugin>::purge_session_for_agent(session_id, agent_id)
            }

            fn purge_agent(agent_alias: String) -> Result<u64, String> {
                <$ty as $crate::memory::MemoryPlugin>::purge_agent(agent_alias)
            }

            fn reindex() -> Result<u64, String> {
                <$ty as $crate::memory::MemoryPlugin>::reindex()
            }

            fn store_procedural(
                messages: Vec<$crate::memory::ProceduralMessage>,
                session_id: Option<String>,
            ) -> Result<(), String> {
                <$ty as $crate::memory::MemoryPlugin>::store_procedural(messages, session_id)
            }

            fn ensure_agent_uuid(alias: String) -> Result<String, String> {
                <$ty as $crate::memory::MemoryPlugin>::ensure_agent_uuid(alias)
            }

            fn recall_namespaced(
                namespace: String,
                query: String,
                limit: u64,
                session_id: Option<String>,
                since: Option<String>,
                until: Option<String>,
            ) -> Result<Vec<$crate::memory::MemoryEntry>, String> {
                <$ty as $crate::memory::MemoryPlugin>::recall_namespaced(
                    namespace, query, limit, session_id, since, until,
                )
            }

            fn export_entries(
                filter: $crate::memory::ExportFilter,
            ) -> Result<Vec<$crate::memory::MemoryEntry>, String> {
                <$ty as $crate::memory::MemoryPlugin>::export_entries(filter)
            }

            fn store_with_metadata(
                key: String,
                content: String,
                category: $crate::memory::MemoryCategory,
                session_id: Option<String>,
                namespace: Option<String>,
                importance: Option<f64>,
            ) -> Result<(), String> {
                <$ty as $crate::memory::MemoryPlugin>::store_with_metadata(
                    key, content, category, session_id, namespace, importance,
                )
            }
        }

        impl $crate::bindings::memory::exports::zeroclaw::plugin::plugin_info::Guest
            for __ZeroclawPluginComponent
        {
            fn plugin_name() -> String {
                <$ty as $crate::memory::MemoryPlugin>::plugin_info().0.to_string()
            }

            fn plugin_version() -> String {
                <$ty as $crate::memory::MemoryPlugin>::plugin_info().1.to_string()
            }
        }

        $crate::bindings::memory::export!(__ZeroclawPluginComponent with_types_in $crate::bindings::memory);
    };
}
