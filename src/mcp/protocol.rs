use anyhow::{Context, Result};
use serde_json::Value;

pub fn decode_frame(raw: &[u8]) -> Result<Value> {
    let separator = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .context("missing header/body separator")?;

    let headers = std::str::from_utf8(&raw[..separator]).context("headers are not valid utf-8")?;
    let body = &raw[separator + 4..];

    let content_length = headers
        .split("\r\n")
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.eq_ignore_ascii_case("content-length") {
                Some(value.trim())
            } else {
                None
            }
        })
        .context("missing Content-Length header")?
        .parse::<usize>()
        .context("invalid Content-Length header")?;

    let payload = if content_length <= body.len() {
        &body[..content_length]
    } else {
        body
    };

    serde_json::from_slice(payload).context("invalid jsonrpc payload")
}

pub fn encode_frame(value: &Value) -> Result<Vec<u8>> {
    let body = serde_json::to_vec(value).context("failed to serialize jsonrpc payload")?;
    let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    frame.extend_from_slice(&body);
    Ok(frame)
}

#[cfg(test)]
mod tests {
    use super::{decode_frame, encode_frame};
    use serde_json::json;

    #[test]
    fn decodes_content_length_frame() {
        let raw = b"Content-Length: 27\r\n\r\n{\"jsonrpc\":\"2.0\",\"id\":1}";

        let decoded = decode_frame(raw).expect("frame should decode");

        assert_eq!(decoded["jsonrpc"], "2.0");
    }

    #[test]
    fn encodes_content_length_frame() {
        let value = json!({"jsonrpc": "2.0"});

        let encoded = encode_frame(&value).expect("frame should encode");
        let encoded_str = std::str::from_utf8(&encoded).expect("encoded frame should be utf-8");

        assert!(encoded_str.starts_with("Content-Length:"));
        assert!(encoded_str.contains("\r\n\r\n{\"jsonrpc\":\"2.0\""));
    }
}
