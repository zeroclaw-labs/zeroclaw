use async_trait::async_trait;
use serde::{Deserialize, Serialize};

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

pub const EPHEMERAL_WORKSPACE_WARNING: &str = "\u{26a0}\u{fe0f} EPHEMERAL WORKSPACE: the active runtime uses an ephemeral workspace \
     (tmpfs / no host volume mount). Files written here do NOT persist on the host after this \
     session ends, and reads may return stale or empty data. To make the workspace persistent, \
     set `runtime.docker.mount_workspace = true` in your config and ensure the workspace \
     directory is bind-mounted into the container.";

pub fn with_ephemeral_workspace_warning(text: &str) -> String {
    if text.is_empty() {
        EPHEMERAL_WORKSPACE_WARNING.to_string()
    } else {
        format!("{EPHEMERAL_WORKSPACE_WARNING}\n\n{text}")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: std::sync::Arc<serde_json::Value>,
}

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
