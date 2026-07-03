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

/// Typed tool output. The LLM-facing string is derived from the structured
/// value exactly once, at construction, so the two can never drift. `Deref` to
/// `str` keeps every text read site working on the rendered form.
///
/// Wire format: serializes as a bare string when no structured value is
/// attached (byte-identical to the legacy `output: String` field), and as
/// `{"text", "data"}` when a tool declares structured output. Both shapes
/// deserialize.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolOutput {
    text: String,
    data: Option<serde_json::Value>,
}

impl ToolOutput {
    /// Plain text output with no structured value.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            data: None,
        }
    }

    /// Structured output; the display text is the pretty-printed JSON.
    pub fn json(data: serde_json::Value) -> Self {
        let text = serde_json::to_string_pretty(&data).unwrap_or_else(|_| data.to_string());
        Self {
            text,
            data: Some(data),
        }
    }

    /// Structured output rendered with custom display text.
    pub fn json_with_text(data: serde_json::Value, text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            data: Some(data),
        }
    }

    /// The structured value, when the tool declared one.
    pub fn data(&self) -> Option<&serde_json::Value> {
        self.data.as_ref()
    }

    /// Take the structured value, when the tool declared one.
    pub fn into_data(self) -> Option<serde_json::Value> {
        self.data
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn into_string(self) -> String {
        self.text
    }
}

impl std::ops::Deref for ToolOutput {
    type Target = str;
    fn deref(&self) -> &str {
        &self.text
    }
}

impl std::fmt::Display for ToolOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

impl From<String> for ToolOutput {
    fn from(text: String) -> Self {
        Self::text(text)
    }
}

impl From<&str> for ToolOutput {
    fn from(text: &str) -> Self {
        Self::text(text)
    }
}

impl From<serde_json::Value> for ToolOutput {
    fn from(data: serde_json::Value) -> Self {
        Self::json(data)
    }
}

impl PartialEq<str> for ToolOutput {
    fn eq(&self, other: &str) -> bool {
        self.text == other
    }
}

impl PartialEq<&str> for ToolOutput {
    fn eq(&self, other: &&str) -> bool {
        self.text == *other
    }
}

impl PartialEq<String> for ToolOutput {
    fn eq(&self, other: &String) -> bool {
        &self.text == other
    }
}

impl Serialize for ToolOutput {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match &self.data {
            None => serializer.serialize_str(&self.text),
            Some(data) => {
                use serde::ser::SerializeStruct;
                let mut s = serializer.serialize_struct("ToolOutput", 2)?;
                s.serialize_field("text", &self.text)?;
                s.serialize_field("data", data)?;
                s.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for ToolOutput {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Text(String),
            Structured {
                text: String,
                data: serde_json::Value,
            },
        }
        Ok(match Repr::deserialize(deserializer)? {
            Repr::Text(text) => Self::text(text),
            Repr::Structured { text, data } => Self::json_with_text(data, text),
        })
    }
}

/// Result of a tool execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub success: bool,
    pub output: ToolOutput,
    pub error: Option<String>,
}

impl ToolResult {
    /// Successful result from any output form (`String`, `&str`, `Value`).
    pub fn ok(output: impl Into<ToolOutput>) -> Self {
        Self {
            success: true,
            output: output.into(),
            error: None,
        }
    }

    /// Failed result with no output.
    pub fn err(error: impl Into<String>) -> Self {
        Self {
            success: false,
            output: ToolOutput::default(),
            error: Some(error.into()),
        }
    }

    /// Failed result that still carries partial output.
    pub fn partial(output: impl Into<ToolOutput>, error: impl Into<String>) -> Self {
        Self {
            success: false,
            output: output.into(),
            error: Some(error.into()),
        }
    }
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
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
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name().to_string(),
            description: self.description().to_string(),
            parameters: self.parameters_schema(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_output_serializes_as_bare_string() {
        let r = ToolResult::ok("hello");
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["output"], serde_json::json!("hello"));
    }

    #[test]
    fn legacy_string_output_deserializes() {
        let r: ToolResult =
            serde_json::from_str(r#"{"success":true,"output":"plain","error":null}"#).unwrap();
        assert_eq!(r.output, "plain");
        assert!(r.output.data().is_none());
    }

    #[test]
    fn json_output_roundtrips_with_data() {
        let data = serde_json::json!({"status": 200, "body": {"ok": true}});
        let r = ToolResult::ok(data.clone());
        let wire = serde_json::to_string(&r).unwrap();
        let back: ToolResult = serde_json::from_str(&wire).unwrap();
        assert_eq!(back.output.data(), Some(&data));
        assert_eq!(back.output.as_str(), r.output.as_str());
    }

    #[test]
    fn json_string_value_keeps_structured_form() {
        // A JSON string *value* must stay data-carrying, not collapse to Text.
        let r = ToolResult::ok(serde_json::json!("quoted"));
        let wire = serde_json::to_string(&r).unwrap();
        let back: ToolResult = serde_json::from_str(&wire).unwrap();
        assert_eq!(back.output.data(), Some(&serde_json::json!("quoted")));
    }

    #[test]
    fn deref_and_display_expose_rendered_text() {
        let out = ToolOutput::json(serde_json::json!({"a": 1}));
        assert!(out.contains("\"a\": 1"));
        assert_eq!(out.to_string(), out.as_str());
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
