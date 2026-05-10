// src/messaging/mod.rs
pub mod media;

pub use media::{download_image_as_base64, extract_markdown_images, process_markdown_with_images};

use base64::Engine;

/// Encode a text content string as a WuKongIM type-1 Base64 payload.
pub fn encode_text_payload(content: &str) -> anyhow::Result<String> {
    let obj = serde_json::json!({ "type": 1, "content": content });
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
        let decoded = base64::engine::general_purpose::STANDARD.decode(&b64).unwrap();
        let val: serde_json::Value = serde_json::from_slice(&decoded).unwrap();
        assert_eq!(val["type"], 1);
        assert_eq!(val["content"], "hello");
    }
}
