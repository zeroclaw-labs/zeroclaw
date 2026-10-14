//! Decoding for bytes captured from shell stdout and stderr.
//!
//! Shells and programs launched by them are not required to use UTF-8. Keep
//! the captured bytes intact until this boundary so every shell execution
//! path applies the same decoding policy.

/// Decode shell output as text without panicking on arbitrary bytes.
///
/// Valid UTF-8 is always preferred. For other byte sequences, use chardetng
/// to select an `encoding_rs` decoder. On Windows, a short legacy-encoded
/// result uses the current console/system code page as context because there
/// is not enough text for reliable statistical detection. The final UTF-8
/// lossy conversion keeps the result representable for malformed input.
const WINDOWS_SHORT_OUTPUT_LIMIT: usize = 32;

pub(crate) fn decode_shell_output(bytes: &[u8]) -> String {
    decode_shell_output_with_context(bytes, false, windows_code_page_hint())
}

/// Decode output captured at a byte limit. An incomplete UTF-8 suffix is only
/// preserved when the caller knows that the capture was truncated; without
/// that signal, bytes such as a lone CP1252 `0xe9` must remain eligible for
/// legacy encoding detection.
pub(crate) fn decode_truncated_shell_output(bytes: &[u8]) -> String {
    decode_shell_output_with_context(bytes, true, windows_code_page_hint())
}

fn decode_shell_output_with_context(
    bytes: &[u8],
    capture_was_truncated: bool,
    code_page_hint: Option<&'static encoding_rs::Encoding>,
) -> String {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_owned();
    }

    // A capture limit can split a UTF-8 sequence at EOF. Preserve the valid
    // prefix instead of feeding it back into a legacy-encoding detector.
    let utf8_error = std::str::from_utf8(bytes).expect_err("invalid UTF-8 checked above");
    if capture_was_truncated
        && utf8_error.error_len().is_none()
        && utf8_error.valid_up_to() > 0
        && (code_page_hint.is_none()
            || (bytes.len() > WINDOWS_SHORT_OUTPUT_LIMIT
                && has_utf8_continuation_context(bytes, utf8_error.valid_up_to())))
    {
        let valid_up_to = utf8_error.valid_up_to();
        let prefix = std::str::from_utf8(&bytes[..valid_up_to])
            .expect("valid_up_to always identifies a UTF-8 boundary");
        return format!("{prefix}{}", String::from_utf8_lossy(&bytes[valid_up_to..]));
    }

    // Very short legacy output is inherently ambiguous to statistical
    // detection. On Windows the active console code page is useful context,
    // but only as a short-output fallback; longer output remains detector-led.
    if bytes.len() <= WINDOWS_SHORT_OUTPUT_LIMIT
        && let Some(encoding) = code_page_hint
    {
        return encoding.decode(bytes).0.into_owned();
    }

    let mut detector = chardetng::EncodingDetector::new();
    detector.feed(bytes, true);
    let encoding = detector.guess(None, true);
    let (text, _, had_errors) = encoding.decode(bytes);

    if had_errors && std::ptr::eq(encoding, encoding_rs::UTF_8) {
        return String::from_utf8_lossy(bytes).into_owned();
    }

    text.into_owned()
}

fn has_utf8_continuation_context(bytes: &[u8], valid_up_to: usize) -> bool {
    let suffix = &bytes[valid_up_to..];
    let Some(&lead) = suffix.first() else {
        return false;
    };
    let expected_len = match lead {
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => return false,
    };
    if suffix.len() >= expected_len || !suffix[1..].iter().all(|byte| (byte & 0xc0) == 0x80) {
        return false;
    }

    // A non-ASCII UTF-8 prefix is strong evidence that the stream is UTF-8,
    // while a short ASCII prefix followed by one legacy byte remains eligible
    // for the Windows code-page fallback.
    std::str::from_utf8(&bytes[..valid_up_to])
        .ok()
        .is_some_and(|prefix| prefix.chars().any(|character| character.len_utf8() > 1))
}

#[cfg(target_os = "windows")]
fn windows_code_page_hint() -> Option<&'static encoding_rs::Encoding> {
    use windows::Win32::Globalization::GetACP;
    use windows::Win32::System::Console::GetConsoleOutputCP;

    // SAFETY: both Win32 functions are parameter-free code-page queries. A
    // zero console code page selects the documented system ANSI fallback.
    let code_page = unsafe {
        let console_code_page = GetConsoleOutputCP();
        if console_code_page == 0 {
            GetACP()
        } else {
            console_code_page
        }
    };

    windows_code_page_to_encoding(code_page)
}

#[cfg(not(target_os = "windows"))]
fn windows_code_page_hint() -> Option<&'static encoding_rs::Encoding> {
    None
}

#[cfg(target_os = "windows")]
fn windows_code_page_to_encoding(code_page: u32) -> Option<&'static encoding_rs::Encoding> {
    Some(match code_page {
        932 => encoding_rs::SHIFT_JIS,
        936 | 54936 => encoding_rs::GBK,
        949 => encoding_rs::EUC_KR,
        950 => encoding_rs::BIG5,
        1250 => encoding_rs::WINDOWS_1250,
        1251 => encoding_rs::WINDOWS_1251,
        1252 => encoding_rs::WINDOWS_1252,
        1253 => encoding_rs::WINDOWS_1253,
        1254 => encoding_rs::WINDOWS_1254,
        1255 => encoding_rs::WINDOWS_1255,
        1256 => encoding_rs::WINDOWS_1256,
        1257 => encoding_rs::WINDOWS_1257,
        1258 => encoding_rs::WINDOWS_1258,
        20127 | 65001 => encoding_rs::UTF_8,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        decode_shell_output, decode_shell_output_with_context, decode_truncated_shell_output,
    };

    #[test]
    fn preserves_valid_utf8() {
        let input = "shell 输出\n";
        assert_eq!(decode_shell_output(input.as_bytes()), input);
    }

    #[test]
    fn decodes_non_utf8_text() {
        // GBK for "中文输出". Repeating the sample gives chardetng enough
        // context to distinguish it from other East Asian encodings.
        let sample = [0xd6, 0xd0, 0xce, 0xc4, 0xca, 0xe4, 0xb3, 0xf6, 0x20];
        let bytes = sample.repeat(8);
        let text = decode_shell_output(&bytes);
        assert!(text.contains("中文输出"), "decoded text: {text:?}");
    }

    #[test]
    fn malformed_bytes_never_panic() {
        let text = decode_shell_output(&[0xff, 0xfe, 0xfd, 0x80]);
        assert!(!text.is_empty());
    }

    #[test]
    fn truncated_utf8_preserves_valid_prefix() {
        let decoded = decode_truncated_shell_output(&[b'p', b'r', b'e', b'f', 0xe2, 0x82]);
        assert!(decoded.starts_with("pref"), "decoded text: {decoded:?}");
        assert!(decoded.contains('\u{fffd}'), "decoded text: {decoded:?}");
    }

    #[test]
    fn truncated_utf8_preserves_prefix_when_only_lead_byte_remains() {
        let mut bytes = "€€€".as_bytes().to_vec();
        bytes.push(0xe2);
        let decoded = decode_truncated_shell_output(&bytes);
        assert!(decoded.starts_with("€€€"), "decoded text: {decoded:?}");
        assert!(decoded.ends_with('\u{fffd}'), "decoded text: {decoded:?}");
    }

    #[test]
    fn short_legacy_output_uses_explicit_hint() {
        let gbk = [0xc4, 0xe3, 0xba, 0xc3];
        let decoded = decode_shell_output_with_context(&gbk, false, Some(encoding_rs::GBK));
        assert_eq!(decoded, "你好");
    }

    #[test]
    fn short_single_byte_legacy_output_is_not_truncation() {
        let cp1252 =
            decode_shell_output_with_context(&[0xe9], false, Some(encoding_rs::WINDOWS_1252));
        let cp1251 =
            decode_shell_output_with_context(&[0xc0], false, Some(encoding_rs::WINDOWS_1251));
        assert_eq!(cp1252, "é");
        assert_eq!(cp1251, "А");
    }
}
