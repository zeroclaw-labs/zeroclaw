fn is_json_ws(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n')
}

fn skip_json_ws(line: &[u8], mut index: usize) -> usize {
    while index < line.len() && is_json_ws(line[index]) {
        index += 1;
    }
    index
}

fn value_start(line: &[u8], key: &[u8]) -> Option<usize> {
    if key.is_empty() || line.len() < key.len() + 3 {
        return None;
    }

    for i in 0..line.len() {
        if line[i] != b'"' {
            continue;
        }

        let key_start = i + 1;
        let key_end = key_start + key.len();
        if key_end >= line.len() || !line[key_start..].starts_with(key) || line[key_end] != b'"' {
            continue;
        }

        let colon = skip_json_ws(line, key_end + 1);
        if colon >= line.len() || line[colon] != b':' {
            continue;
        }

        return Some(skip_json_ws(line, colon + 1));
    }

    None
}

/// Parse an integer value from a JSON key like `"pin":13` or `"value":1`.
/// Returns `None` if the key is not found.
pub fn parse_arg(line: &[u8], key: &[u8]) -> Option<i32> {
    let rest = &line[value_start(line, key)?..];
    let mut num: i32 = 0;
    let mut neg = false;
    let mut j = 0;

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

    Some(if neg { -num } else { num })
}

/// Check if a JSON line contains `"cmd":"<cmd>"`.
pub fn has_cmd(line: &[u8], cmd: &[u8]) -> bool {
    let Some(start) = value_start(line, b"cmd") else {
        return false;
    };
    let value = &line[start..];
    if value.len() < cmd.len() + 2 || value[0] != b'"' {
        return false;
    }

    value[1..].starts_with(cmd) && value[1 + cmd.len()] == b'"'
}

/// Extract the `"id"` string value from a JSON line into `out`.
/// Returns the number of bytes written. Falls back to `"0"` if not found.
pub fn copy_id(line: &[u8], out: &mut [u8]) -> usize {
    if out.is_empty() {
        return 0;
    }

    let Some(start) = value_start(line, b"id") else {
        out[0] = b'0';
        return 1;
    };
    if start >= line.len() || line[start] != b'"' {
        out[0] = b'0';
        return 1;
    }

    let start = start + 1;
    let mut j = 0;
    while start + j < line.len() && j < out.len() - 1 && line[start + j] != b'"' {
        out[j] = line[start + j];
        j += 1;
    }
    j
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
    fn parse_arg_accepts_json_whitespace() {
        let line = b"{\"args\":{\"pin\" :\t13,\r\"value\" : 1}}";
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
    fn has_cmd_accepts_json_whitespace() {
        let line = b"{\"id\":\"1\", \"cmd\" :\t\"gpio_write\", \"args\":{\"pin\":13}}";
        assert!(has_cmd(line, b"gpio_write"));
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
    fn copy_id_accepts_json_whitespace() {
        let line = b"{\"id\" :\t\"abc123\", \"cmd\":\"ping\"}";
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
