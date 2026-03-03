//! Shared text sanitization for TUI input/render paths.
//!
//! Security goals:
//! - Strip ANSI/VT escape sequences so terminal control payloads are inert.
//! - Remove control characters except newline and tab.

/// Strip ANSI/VT escape sequences while preserving regular UTF-8 text.
pub fn strip_ansi_sequences(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut output = String::with_capacity(input.len());
    let mut idx = 0usize;

    while idx < bytes.len() {
        if bytes[idx] == 0x1b {
            idx += consume_escape_sequence(&bytes[idx..]);
            continue;
        }

        if let Some(ch) = input[idx..].chars().next() {
            output.push(ch);
            idx += ch.len_utf8();
        } else {
            break;
        }
    }

    output
}

/// Keep newline and tab, drop remaining control chars.
pub fn sanitize_text(raw: &str) -> String {
    strip_ansi_sequences(raw)
        .chars()
        .filter(|&ch| ch == '\n' || ch == '\t' || !ch.is_control())
        .collect()
}

fn consume_escape_sequence(bytes: &[u8]) -> usize {
    // Input starts with ESC.
    if bytes.len() <= 1 {
        return 1;
    }

    match bytes[1] {
        // CSI: ESC [ ... final-byte
        b'[' => {
            let mut i = 2usize;
            while i < bytes.len() {
                let b = bytes[i];
                i += 1;
                if (0x40..=0x7e).contains(&b) {
                    break;
                }
            }
            i
        }
        // OSC: ESC ] ... BEL or ST (ESC \)
        b']' => {
            let mut i = 2usize;
            while i < bytes.len() {
                if bytes[i] == 0x07 {
                    return i + 1;
                }
                if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                    return i + 2;
                }
                i += 1;
            }
            i
        }
        // DCS / SOS / PM / APC: ESC P|X|^|_ ... ST (ESC \)
        b'P' | b'X' | b'^' | b'_' => {
            let mut i = 2usize;
            while i < bytes.len() {
                if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                    return i + 2;
                }
                i += 1;
            }
            i
        }
        // Single-character escape - skip ESC + next byte.
        _ => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::sanitize_text;

    #[test]
    fn sanitize_text_preserves_tab_and_newline() {
        let input = "a\u{0000}b\nc\td\u{0085}e";
        assert_eq!(sanitize_text(input), "ab\nc\tde");
    }

    #[test]
    fn sanitize_text_strips_ansi_and_osc_sequences() {
        let input = "safe\x1b[2Jtext\x1b]0;title\x07";
        assert_eq!(sanitize_text(input), "safetext");
    }
}
