//! Robust text-file reader for legal markdown sources.
//!
//! The Korean legal corpus ships in several encodings depending on
//! vintage:
//!   - National Law Information Centre (국가법령정보) API dumps —
//!     UTF-8 (sometimes with BOM).
//!   - Older court decisions exported from internal tools — CP949
//!     (Windows Korean) or EUC-KR.
//!   - Scanned-to-text archives — occasionally UTF-16 LE with BOM.
//!
//! `std::fs::read_to_string` only accepts UTF-8 and errors with an
//! `InvalidData` message that obscures the encoding issue. This
//! module probes the bytes:
//!
//!   1. Strip UTF-8 BOM and try UTF-8 strict.
//!   2. Handle UTF-16 LE/BE BOMs.
//!   3. Fall back to CP949 (which `encoding_rs` models as `EUC-KR`
//!      per the WHATWG Encoding standard — `EUC-KR` there is the
//!      superset, identical to CP949).
//!   4. Report the detected encoding so the operator can spot
//!      mixed-encoding corpora.
//!
//! Design note: we don't try to auto-detect arbitrary encodings via
//! statistical classifiers (chardet et al.). The three above cover
//! ~99% of real Korean legal inputs; anything exotic should be
//! pre-converted.

use anyhow::{Context, Result};
use std::path::Path;

/// Result of reading + decoding a file.
#[derive(Debug, Clone)]
pub struct DecodedFile {
    pub content: String,
    /// Human-readable label: `"utf-8"` / `"utf-8-bom"` / `"utf-16-le"` /
    /// `"utf-16-be"` / `"cp949"` (the WHATWG name for CP949/EUC-KR).
    pub encoding: &'static str,
    /// True if the fallback decoder hit replacement characters (indicates
    /// the file is probably a different encoding, or corrupt).
    pub had_errors: bool,
}

/// Read `path` and return its contents as a UTF-8 `String` along with the
/// detected encoding. Never fails on encoding grounds — if all known
/// decoders produce replacements, we return the best-effort CP949 output
/// with `had_errors = true` so the caller can decide whether to ingest
/// or bail.
pub fn read_markdown_auto(path: &Path) -> Result<DecodedFile> {
    let bytes =
        std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(decode_bytes(&bytes))
}

/// Pure-bytes variant — exposed for unit tests. Mirrors
/// [`read_markdown_auto`] but skips the filesystem step.
pub fn decode_bytes(bytes: &[u8]) -> DecodedFile {
    // 1. UTF-8 BOM.
    if bytes.starts_with(b"\xEF\xBB\xBF") {
        if let Ok(s) = std::str::from_utf8(&bytes[3..]) {
            return DecodedFile {
                content: s.to_string(),
                encoding: "utf-8-bom",
                had_errors: false,
            };
        }
    }
    // 2. UTF-16 BOMs.
    if bytes.len() >= 2 {
        match (bytes[0], bytes[1]) {
            (0xFF, 0xFE) => {
                let (cow, _, had_errors) = encoding_rs::UTF_16LE.decode(&bytes[2..]);
                return DecodedFile {
                    content: cow.into_owned(),
                    encoding: "utf-16-le",
                    had_errors,
                };
            }
            (0xFE, 0xFF) => {
                let (cow, _, had_errors) = encoding_rs::UTF_16BE.decode(&bytes[2..]);
                return DecodedFile {
                    content: cow.into_owned(),
                    encoding: "utf-16-be",
                    had_errors,
                };
            }
            _ => {}
        }
    }
    // 3. UTF-8 strict.
    if let Ok(s) = std::str::from_utf8(bytes) {
        return DecodedFile {
            content: s.to_string(),
            encoding: "utf-8",
            had_errors: false,
        };
    }
    // 4. CP949 / EUC-KR fallback.
    let (cow, _actual, had_errors) = encoding_rs::EUC_KR.decode(bytes);
    DecodedFile {
        content: cow.into_owned(),
        encoding: "cp949",
        had_errors,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_ascii_is_utf8() {
        let d = decode_bytes(b"hello world");
        assert_eq!(d.content, "hello world");
        assert_eq!(d.encoding, "utf-8");
        assert!(!d.had_errors);
    }

    #[test]
    fn utf8_korean_passes_through() {
        let src = "근로기준법 제36조(금품 청산)";
        let d = decode_bytes(src.as_bytes());
        assert_eq!(d.content, src);
        assert_eq!(d.encoding, "utf-8");
    }

    #[test]
    fn utf8_bom_is_stripped() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice("민법 제750조".as_bytes());
        let d = decode_bytes(&bytes);
        assert_eq!(d.content, "민법 제750조");
        assert_eq!(d.encoding, "utf-8-bom");
    }

    #[test]
    fn cp949_korean_falls_back_correctly() {
        // Encode `민법 제36조` as CP949.
        let expected = "민법 제36조";
        let (cp949_bytes, _, had_errors) = encoding_rs::EUC_KR.encode(expected);
        assert!(!had_errors, "EUC-KR must be able to encode Korean");
        let d = decode_bytes(&cp949_bytes);
        assert_eq!(d.encoding, "cp949");
        assert_eq!(d.content, expected);
    }

    #[test]
    fn utf16le_bom_is_decoded() {
        let expected = "민법";
        let mut bytes = vec![0xFF, 0xFE];
        for cu in expected.encode_utf16() {
            bytes.push((cu & 0xFF) as u8);
            bytes.push((cu >> 8) as u8);
        }
        let d = decode_bytes(&bytes);
        assert_eq!(d.encoding, "utf-16-le");
        assert_eq!(d.content, expected);
    }

    #[test]
    fn read_markdown_auto_reads_tempfile() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "민법 제1조").unwrap();
        let d = read_markdown_auto(tmp.path()).unwrap();
        assert_eq!(d.content, "민법 제1조");
        assert_eq!(d.encoding, "utf-8");
    }
}
