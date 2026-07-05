//! MCP resource protocol types (`resources/list`, `resources/read`).

use serde::{Deserialize, Serialize};

/// A resource advertised by an MCP server (from `resources/list`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpResourceDef {
    pub uri: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

/// Result payload of `resources/list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpResourcesListResult {
    pub resources: Vec<McpResourceDef>,
    #[serde(rename = "nextCursor", skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// A single content block returned by `resources/read`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpResourceContent {
    pub uri: String,
    #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blob: Option<String>,
}

/// Result payload of `resources/read`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpResourceContents {
    pub contents: Vec<McpResourceContent>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_def_deserializes_with_mime() {
        let json =
            r#"{"uri":"file:///a.txt","name":"a","description":"A file","mimeType":"text/plain"}"#;
        let def: McpResourceDef = serde_json::from_str(json).unwrap();
        assert_eq!(def.uri, "file:///a.txt");
        assert_eq!(def.name, "a");
        assert_eq!(def.mime_type.as_deref(), Some("text/plain"));
    }

    #[test]
    fn resources_list_result_carries_next_cursor() {
        let json = r#"{"resources":[{"uri":"u","name":"n"}],"nextCursor":"c1"}"#;
        let res: McpResourcesListResult = serde_json::from_str(json).unwrap();
        assert_eq!(res.resources.len(), 1);
        assert_eq!(res.next_cursor.as_deref(), Some("c1"));
    }

    #[test]
    fn resource_contents_deserializes_text_and_blob() {
        let json = r#"{"contents":[{"uri":"u","mimeType":"text/plain","text":"hi"},{"uri":"u2","blob":"YmFzZTY0"}]}"#;
        let c: McpResourceContents = serde_json::from_str(json).unwrap();
        assert_eq!(c.contents.len(), 2);
        assert_eq!(c.contents[0].text.as_deref(), Some("hi"));
        assert_eq!(c.contents[1].blob.as_deref(), Some("YmFzZTY0"));
    }
}
