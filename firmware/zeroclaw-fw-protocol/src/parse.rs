/// Parse an integer value from a JSON key like `"pin":13` or `"value":1`.
///
/// Returns `None` if the key is not found.
pub fn parse_arg(line: &[u8], key: &[u8]) -> Option<i32> {
    // Build the search pattern: `"key":`
    let mut suffix: [u8; 32] = [0; 32];
    suffix[0] = b'"';
    let mut len = 1;
    for (i, &k) in key.iter().enumerate() {
        if i >= 30 {
            break;
        }
        suffix[len] = k;
        len += 1;
    }
    suffix[len] = b'"';
    suffix[len + 1] = b':';
    len += 2;
    let suffix = &suffix[..len];

    let line_len = line.len();
    if line_len < len {
        return None;
    }
    for i in 0..=line_len - len {
        if line[i..].starts_with(suffix) {
            let rest = &line[i + len..];
            let mut num: i32 = 0;
            let mut neg = false;
            let mut j = 0;
            // Skip whitespace after colon
            while j < rest.len() && rest[j] == b' ' {
                j += 1;
            }
            if j < rest.len() && rest[j] == b'-' {
                neg = true;
                j += 1;
            }
            let start = j;
            while j < rest.len() && rest[j].is_ascii_digit() {
                num = num * 10 + (rest[j] - b'0') as i32;
                j += 1;
            }
            if j == start {
                return None;
            }
            return Some(if neg { -num } else { num });
        }
    }
    None
}

/// Check if a JSON line contains `"cmd":"<cmd>"`.
pub fn has_cmd(line: &[u8], cmd: &[u8]) -> bool {
    let mut pat: [u8; 64] = [0; 64];
    pat[0..7].copy_from_slice(b"\"cmd\":\"");
    let clen = cmd.len().min(50);
    pat[7..7 + clen].copy_from_slice(&cmd[..clen]);
    pat[7 + clen] = b'"';
    let pat = &pat[..8 + clen];

    let line_len = line.len();
    if line_len < pat.len() {
        return false;
    }
    for i in 0..=line_len - pat.len() {
        if line[i..].starts_with(pat) {
            return true;
        }
    }
    false
}

/// Extract the `"id"` string value from a JSON line into `out`.
///
/// Returns the number of bytes written. Falls back to `"0"` if not found.
pub fn copy_id<'a>(line: &[u8], out: &'a mut [u8]) -> usize {
    let prefix = b"\"id\":\"";
    if line.len() < prefix.len() + 1 {
        out[0] = b'0';
        return 1;
    }
    for i in 0..=line.len() - prefix.len() {
        if line[i..].starts_with(prefix) {
            let start = i + prefix.len();
            let mut j = 0;
            while start + j < line.len() && j < out.len() - 1 && line[start + j] != b'"' {
                out[j] = line[start + j];
                j += 1;
            }
            return j;
        }
    }
    out[0] = b'0';
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_arg_pin() {
        let line = br#"{"id":"1","cmd":"gpio_write","args":{"pin":13,"value":1}}"#;
        assert_eq!(parse_arg(line, b"pin"), Some(13));
        assert_eq!(parse_arg(line, b"value"), Some(1));
    }

    #[test]
    fn parse_arg_negative() {
        let line = br#"{"args":{"pin":-1}}"#;
        assert_eq!(parse_arg(line, b"pin"), Some(-1));
    }

    #[test]
    fn parse_arg_missing() {
        let line = br#"{"id":"1","cmd":"ping"}"#;
        assert_eq!(parse_arg(line, b"pin"), None);
    }

    #[test]
    fn parse_arg_zero() {
        let line = br#"{"args":{"value":0}}"#;
        assert_eq!(parse_arg(line, b"value"), Some(0));
    }

    #[test]
    fn has_cmd_matches() {
        let line = br#"{"id":"1","cmd":"gpio_read","args":{"pin":5}}"#;
        assert!(has_cmd(line, b"gpio_read"));
        assert!(!has_cmd(line, b"gpio_write"));
        assert!(!has_cmd(line, b"ping"));
    }

    #[test]
    fn has_cmd_all_commands() {
        assert!(has_cmd(br#"{"cmd":"ping"}"#, b"ping"));
        assert!(has_cmd(br#"{"cmd":"capabilities"}"#, b"capabilities"));
        assert!(has_cmd(br#"{"cmd":"gpio_read"}"#, b"gpio_read"));
        assert!(has_cmd(br#"{"cmd":"gpio_write"}"#, b"gpio_write"));
    }

    #[test]
    fn has_cmd_empty_line() {
        assert!(!has_cmd(b"", b"ping"));
    }

    #[test]
    fn copy_id_extracts() {
        let line = br#"{"id":"abc123","cmd":"ping"}"#;
        let mut buf = [0u8; 16];
        let len = copy_id(line, &mut buf);
        assert_eq!(&buf[..len], b"abc123");
    }

    #[test]
    fn copy_id_numeric() {
        let line = br#"{"id":"42","cmd":"ping"}"#;
        let mut buf = [0u8; 16];
        let len = copy_id(line, &mut buf);
        assert_eq!(&buf[..len], b"42");
    }

    #[test]
    fn copy_id_missing_defaults_to_zero() {
        let line = br#"{"cmd":"ping"}"#;
        let mut buf = [0u8; 16];
        let len = copy_id(line, &mut buf);
        assert_eq!(&buf[..len], b"0");
    }

    #[test]
    fn copy_id_empty_line() {
        let mut buf = [0u8; 16];
        let len = copy_id(b"", &mut buf);
        assert_eq!(&buf[..len], b"0");
    }
}
