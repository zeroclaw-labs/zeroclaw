//! Shared constants and functions for Discord integrations.

pub const BASE64_ALPHABET: &[u8] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Minimal base64 decode (no extra dep) — only needs to decode the user ID portion
#[allow(clippy::cast_possible_truncation)]
pub fn base64_decode(input: &str) -> Option<String> {
    let padded = match input.len() % 4 {
        2 => format!("{input}=="),
        3 => format!("{input}="),
        _ => input.to_string(),
    };

    let mut bytes = Vec::new();
    let chars: Vec<u8> = padded.bytes().collect();

    for chunk in chars.chunks(4) {
        if chunk.len() < 4 {
            break;
        }

        let mut v = [0usize; 4];
        for (i, &b) in chunk.iter().enumerate() {
            if b == b'=' {
                v[i] = 0;
            } else {
                v[i] = BASE64_ALPHABET.iter().position(|&a| a == b)?;
            }
        }

        bytes.push(((v[0] << 2) | (v[1] >> 4)) as u8);
        if chunk[2] != b'=' {
            bytes.push((((v[1] & 0xF) << 4) | (v[2] >> 2)) as u8);
        }
        if chunk[3] != b'=' {
            bytes.push((((v[2] & 0x3) << 6) | v[3]) as u8);
        }
    }

    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_decode_bot_id() {
        // ID 123456789012345678 is "MTIzNDU2Nzg5MDEyMzQ1Njc4" in base64
        let input = "MTIzNDU2Nzg5MDEyMzQ1Njc4";
        let decoded = base64_decode(input);
        assert_eq!(decoded, Some("123456789012345678".to_string()));
    }

    #[test]
    fn base64_decode_empty_string() {
        assert_eq!(base64_decode(""), Some(String::new()));
    }

    #[test]
    fn base64_decode_invalid_chars() {
        // Null byte should definitely be invalid
        assert_eq!(base64_decode("\0\0\0\0"), None);
    }
}
