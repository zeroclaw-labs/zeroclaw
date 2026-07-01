//! MCP prompt protocol types (`prompts/list`, `prompts/get`).

use serde::{Deserialize, Serialize};

/// One declared argument of an MCP prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpPromptArgDef {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
}

/// A prompt advertised by an MCP server (from `prompts/list`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpPromptDef {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub arguments: Vec<McpPromptArgDef>,
}

/// Result payload of `prompts/list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpPromptsListResult {
    pub prompts: Vec<McpPromptDef>,
    #[serde(rename = "nextCursor", skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// A single rendered message returned by `prompts/get`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpPromptMessage {
    pub role: String,
    /// Content block as returned by the server (text/image/resource).
    /// Kept as raw JSON so all MCP content shapes round-trip.
    pub content: serde_json::Value,
}

/// Result payload of `prompts/get`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpGetPromptResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub messages: Vec<McpPromptMessage>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_def_deserializes_with_arguments() {
        let json = r#"{"name":"summarize","description":"Summarize text","arguments":[{"name":"text","required":true}]}"#;
        let def: McpPromptDef = serde_json::from_str(json).unwrap();
        assert_eq!(def.name, "summarize");
        assert_eq!(def.arguments.len(), 1);
        assert_eq!(def.arguments[0].name, "text");
        assert_eq!(def.arguments[0].required, Some(true));
    }

    #[test]
    fn prompt_def_defaults_empty_arguments() {
        let json = r#"{"name":"noargs"}"#;
        let def: McpPromptDef = serde_json::from_str(json).unwrap();
        assert!(def.arguments.is_empty());
    }

    #[test]
    fn prompts_list_result_carries_next_cursor() {
        let json = r#"{"prompts":[{"name":"p"}],"nextCursor":"c2"}"#;
        let res: McpPromptsListResult = serde_json::from_str(json).unwrap();
        assert_eq!(res.prompts.len(), 1);
        assert_eq!(res.next_cursor.as_deref(), Some("c2"));
    }

    #[test]
    fn get_prompt_result_deserializes_messages() {
        let json = r#"{"description":"d","messages":[{"role":"user","content":{"type":"text","text":"hi"}}]}"#;
        let res: McpGetPromptResult = serde_json::from_str(json).unwrap();
        assert_eq!(res.messages.len(), 1);
        assert_eq!(res.messages[0].role, "user");
    }
}
