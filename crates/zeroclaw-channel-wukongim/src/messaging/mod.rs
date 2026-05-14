// src/messaging/mod.rs
pub mod media;

pub use media::{
    download_file_to_workspace, download_image_as_base64, extract_markdown_images,
    extract_markdown_links, is_blocked_extension, process_markdown_resources,
};

use base64::Engine;

/// Encode a text content string as a WuKongIM type-14 Markdown Base64 payload.
pub fn encode_text_payload(content: &str) -> anyhow::Result<String> {
    let obj = serde_json::json!({
        "type": 14,
        "content": {
            "type": "markdown",
            "text": content
        }
    });
    let json = serde_json::to_string(&obj)?;
    Ok(base64::engine::general_purpose::STANDARD.encode(json))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    #[test]
    fn encode_text_payload_is_valid_base64_json() {
        let b64 = encode_text_payload("hello").unwrap();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&b64)
            .unwrap();
        let val: serde_json::Value = serde_json::from_slice(&decoded).unwrap();
        assert_eq!(val["type"], 14);
        assert_eq!(val["content"]["type"], "markdown");
        assert_eq!(val["content"]["text"], "hello");
    }
}
