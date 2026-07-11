use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Boilerplate-collapsing macro: pair a concrete `Tool` impl with a
/// matching `Attributable` impl that surfaces the supplied `ToolKind`
/// and uses the tool's `name()` as its alias.
///
/// Invoke once per `Tool` struct, in the same module as the struct:
///
/// ```ignore
/// crate::tool_attribution!(ShellTool, ::zeroclaw_api::attribution::ToolKind::Shell);
/// ```
#[macro_export]
macro_rules! tool_attribution {
    ($ty:ty, $kind:expr) => {
        impl $crate::attribution::Attributable for $ty {
            fn role(&self) -> $crate::attribution::Role {
                $crate::attribution::Role::Tool($kind)
            }
            fn alias(&self) -> &str {
                <Self as $crate::tool::Tool>::name(self)
            }
        }
    };
}

/// Bulk-impl `Attributable` for one or more `Tool` mock types in a
/// test module. Every type gets `Role::Tool(ToolKind::Plugin)` and uses
/// the mock's own `name()` as the alias — sufficient for test
/// scaffolding where individual kinds don't matter.
///
/// ```ignore
/// zeroclaw_api::mock_tool_attribution!(CountingTool, FailingTool);
/// ```
#[macro_export]
macro_rules! mock_tool_attribution {
    ($($ty:ty),+ $(,)?) => {
        $(
            $crate::tool_attribution!($ty, $crate::attribution::ToolKind::Plugin);
        )+
    };
}

/// Result of a tool execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub success: bool,
    pub output: String,
    pub error: Option<String>,
}

/// Loud, actionable banner that filesystem-touching tools surface when the
/// active runtime uses an **ephemeral workspace** — e.g. a Docker container
/// with no host volume mount, where the workspace is a private tmpfs. In that
/// mode writes succeed *inside the container* but never reach the host and are
/// discarded when the session ends, and reads may return stale or empty data.
/// Surfacing this prevents the silent data loss reported in issue #4627.
///
/// `file_write` refuses outright (it exists only to persist data). The
/// general-purpose `shell`, `file_read`, and `file_edit` tools stay usable but
/// attach this warning so the agent — and through it the user — knows the
/// workspace is ephemeral and how to fix it.
pub const EPHEMERAL_WORKSPACE_WARNING: &str = "\u{26a0}\u{fe0f} EPHEMERAL WORKSPACE: the active runtime uses an ephemeral workspace \
     (tmpfs / no host volume mount). Files written here do NOT persist on the host after this \
     session ends, and reads may return stale or empty data. To make the workspace persistent, \
     set `runtime.docker.mount_workspace = true` in your config and ensure the workspace \
     directory is bind-mounted into the container.";

/// Prepend [`EPHEMERAL_WORKSPACE_WARNING`] to a tool's output/error text as a
/// clearly delimited banner, preserving the original text below it.
///
/// The banner must live in the field the dispatcher forwards to the model
/// (`output` on success, `error` on failure), so call this for whichever field
/// will be shown. Returns the banner alone when `text` is empty.
pub fn with_ephemeral_workspace_warning(text: &str) -> String {
    if text.is_empty() {
        EPHEMERAL_WORKSPACE_WARNING.to_string()
    } else {
        format!("{EPHEMERAL_WORKSPACE_WARNING}\n\n{text}")
    }
}

/// Description of a tool for the LLM
///
/// `parameters` is `Arc`-shared: tool specs are reassembled every agent-loop
/// iteration and re-emitted on every provider request, so the schema tree is
/// handed out by reference count instead of deep-cloned (#8642). Serde's `rc`
/// feature serializes `Arc<T>` exactly as `T`, so the wire format is
/// unchanged; deserializing produces a fresh allocation per spec.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: std::sync::Arc<serde_json::Value>,
}

/// Core tool trait — implement for any capability.
///
/// Every `Tool` is `Attributable`: log emissions and audit traces from
/// a tool call carry the same `<kind>.<alias>` composite the rest of
/// the runtime uses for channels, providers, and memory. The supertrait
/// bound makes `&dyn Tool` coerce to `&dyn Attributable` automatically,
/// so dispatch-site logging can attribute without knowing the concrete
/// tool type.
#[async_trait]
pub trait Tool: Send + Sync + crate::attribution::Attributable {
    /// Tool name (used in LLM function calling)
    fn name(&self) -> &str;

    /// Human-readable description
    fn description(&self) -> &str;

    /// JSON schema for parameters
    fn parameters_schema(&self) -> serde_json::Value;

    /// Execute the tool with given arguments
    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult>;

    /// Get the full spec for LLM registration
    ///
    /// Tools that store a large schema should override this to hand out an
    /// `Arc::clone` of the stored tree instead of rebuilding it per call
    /// (see `McpToolWrapper`).
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name().to_string(),
            description: self.description().to_string(),
            parameters: std::sync::Arc::new(self.parameters_schema()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #8642 wire-format guarantee: `Arc`-shared parameters must serialize
    /// byte-identically to a plain `Value`, and deserialize back losslessly.
    #[test]
    fn tool_spec_arc_parameters_serialize_transparently() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "command": { "type": "string" } },
            "required": ["command"]
        });
        let spec = ToolSpec {
            name: "shell".to_string(),
            description: "Run commands".to_string(),
            parameters: std::sync::Arc::new(schema.clone()),
        };
        let arc_params = serde_json::to_string(&spec.parameters).expect("arc serializes");
        let plain_params = serde_json::to_string(&schema).expect("plain value serializes");
        assert_eq!(arc_params, plain_params);

        let arc_json = serde_json::to_string(&spec).expect("spec serializes");
        let back: ToolSpec = serde_json::from_str(&arc_json).expect("spec deserializes");
        assert_eq!(back.name, spec.name);
        assert_eq!(*back.parameters, *spec.parameters);
    }

    #[test]
    fn ephemeral_warning_names_cause_and_fix() {
        assert!(EPHEMERAL_WORKSPACE_WARNING.contains("EPHEMERAL WORKSPACE"));
        assert!(EPHEMERAL_WORKSPACE_WARNING.contains("tmpfs"));
        assert!(EPHEMERAL_WORKSPACE_WARNING.contains("mount_workspace"));
        // Line continuations must not leave doubled spaces.
        assert!(!EPHEMERAL_WORKSPACE_WARNING.contains("  "));
    }

    #[test]
    fn empty_text_returns_banner_alone() {
        assert_eq!(
            with_ephemeral_workspace_warning(""),
            EPHEMERAL_WORKSPACE_WARNING
        );
    }

    #[test]
    fn nonempty_text_keeps_body_below_banner() {
        let out = with_ephemeral_workspace_warning("body");
        assert!(out.starts_with(EPHEMERAL_WORKSPACE_WARNING));
        assert!(out.ends_with("\n\nbody"));
    }
}
