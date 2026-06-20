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
