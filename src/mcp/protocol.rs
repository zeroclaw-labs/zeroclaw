use anyhow::{Context, bail};
use serde_json::Value;

pub fn decode_frame(raw: &[u8]) -> anyhow::Result<Value> {
    let separator = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|idx| (idx, 4))
        .or_else(|| raw.windows(2).position(|window| window == b"\n\n").map(|idx| (idx, 2)))
        .ok_or_else(|| anyhow::anyhow!("missing frame separator"))?;

    let (headers, rest) = raw.split_at(separator.0);
    let body = &rest[separator.1..];

    let headers = std::str::from_utf8(headers).context("headers are not valid utf-8")?;

    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.trim().eq_ignore_ascii_case("Content-Length") {
                return Some(value.trim().parse::<usize>());
            }
            None
        })
        .ok_or_else(|| anyhow::anyhow!("missing Content-Length header"))?
        .context("invalid Content-Length header value")?;

    if body.len() < content_length {
        bail!(
            "incomplete frame body: expected {} bytes, found {}",
            content_length,
            body.len()
        );
    }

    let json = serde_json::from_slice::<Value>(&body[..content_length])
        .context("failed to decode frame json payload")?;
    Ok(json)
}

pub fn encode_frame(value: &Value) -> anyhow::Result<Vec<u8>> {
    let body = serde_json::to_vec(value).context("failed to serialize frame json payload")?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());

    let mut frame = Vec::with_capacity(header.len() + body.len());
    frame.extend_from_slice(header.as_bytes());
    frame.extend_from_slice(&body);
    Ok(frame)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{decode_frame, encode_frame};

    #[test]
    fn decodes_content_length_frame() {
        let raw = b"Content-Length: 52\r\n\r\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\",\"params\":{}}";
        let decoded = decode_frame(raw).expect("frame should decode");
        assert_eq!(decoded["jsonrpc"], "2.0");
    }

    #[test]
    fn encodes_content_length_frame() {
        let value = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "ping",
            "params": {}
        });

        let encoded = encode_frame(&value).expect("frame should encode");
        assert!(encoded.starts_with(b"Content-Length: "));
        assert!(encoded.windows(5).any(|window| window == b"\r\n\r\n{"));

        let encoded_text = std::str::from_utf8(&encoded).expect("frame bytes should be utf8-safe");
        assert!(encoded_text.contains("\"jsonrpc\":\"2.0\""));
    }
}
