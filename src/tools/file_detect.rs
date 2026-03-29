//! Binary file type detection and preprocessing for the file read tool.
//!
//! Classifies files before reading to avoid wasting tokens on binary content.
//! Detection uses a three-tier strategy: extension lookup, magic bytes check,
//! and null-byte / UTF-8 validity heuristic on the first 8 KiB.

use std::path::Path;

/// Size of the header buffer used for magic-byte and binary detection.
const HEADER_SIZE: usize = 8192;

/// Detected file type category.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileType {
    /// Valid UTF-8 text — proceed with normal line-numbered read.
    Text,
    /// Image file — return a short summary or route to multimodal.
    Image { mime: &'static str },
    /// PDF document — delegate to pdf_read extraction.
    Pdf,
    /// Office document (docx/xlsx/pptx) — advise using a conversion tool.
    Office { mime: &'static str },
    /// Generic binary file — return a short summary.
    Binary { mime: &'static str },
}

/// Detect the file type from its extension and (optionally) the first bytes.
///
/// When `header` is `None`, classification relies solely on the extension.
/// When `header` is provided, magic bytes and null-byte heuristics are used
/// as a fallback for ambiguous or extension-less files.
pub fn detect_file_type(path: &Path, header: Option<&[u8]>) -> FileType {
    // ── Fast path: extension-based classification ──
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        if let Some(ft) = classify_by_extension(ext) {
            return ft;
        }
    }

    // ── Fallback: inspect header bytes ──
    if let Some(bytes) = header {
        if let Some(ft) = classify_by_magic(bytes) {
            return ft;
        }

        if looks_binary(bytes) {
            return FileType::Binary {
                mime: "application/octet-stream",
            };
        }
    }

    FileType::Text
}

/// Format a human-readable size string (e.g. "1.2 MB", "345 KB").
pub fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{} KB", bytes / KB)
    } else {
        format!("{bytes} bytes")
    }
}

/// Build the short description returned to the LLM for non-text files.
pub fn binary_summary(file_type: &FileType, size_bytes: u64) -> Option<String> {
    let size = format_size(size_bytes);
    match file_type {
        FileType::Text => None,
        FileType::Pdf => None, // handled by pdf extraction path
        FileType::Image { mime } => Some(format!("[Binary file: {mime}, {size}]")),
        FileType::Office { mime } => Some(format!(
            "[Binary file: {mime}, {size} \u{2014} use a document conversion tool to read]"
        )),
        FileType::Binary { mime } => Some(format!("[Binary file: {mime}, {size}]")),
    }
}

// ── Extension lookup ──────────────────────────────────────────────

fn classify_by_extension(ext: &str) -> Option<FileType> {
    let lower = ext.to_ascii_lowercase();
    match lower.as_str() {
        // Images
        "png" => Some(FileType::Image { mime: "image/png" }),
        "jpg" | "jpeg" => Some(FileType::Image { mime: "image/jpeg" }),
        "gif" => Some(FileType::Image { mime: "image/gif" }),
        "webp" => Some(FileType::Image { mime: "image/webp" }),
        "bmp" => Some(FileType::Image { mime: "image/bmp" }),
        "svg" => Some(FileType::Text), // SVG is XML text
        "ico" => Some(FileType::Image {
            mime: "image/x-icon",
        }),
        "tiff" | "tif" => Some(FileType::Image { mime: "image/tiff" }),

        // PDF
        "pdf" => Some(FileType::Pdf),

        // Office
        "docx" => Some(FileType::Office {
            mime: "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        }),
        "xlsx" => Some(FileType::Office {
            mime: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        }),
        "pptx" => Some(FileType::Office {
            mime: "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        }),
        "doc" => Some(FileType::Office {
            mime: "application/msword",
        }),
        "xls" => Some(FileType::Office {
            mime: "application/vnd.ms-excel",
        }),
        "ppt" => Some(FileType::Office {
            mime: "application/vnd.ms-powerpoint",
        }),
        "odt" => Some(FileType::Office {
            mime: "application/vnd.oasis.opendocument.text",
        }),
        "ods" => Some(FileType::Office {
            mime: "application/vnd.oasis.opendocument.spreadsheet",
        }),
        "odp" => Some(FileType::Office {
            mime: "application/vnd.oasis.opendocument.presentation",
        }),

        // Archives & binaries
        "zip" => Some(FileType::Binary {
            mime: "application/zip",
        }),
        "gz" | "gzip" => Some(FileType::Binary {
            mime: "application/gzip",
        }),
        "tar" => Some(FileType::Binary {
            mime: "application/x-tar",
        }),
        "bz2" => Some(FileType::Binary {
            mime: "application/x-bzip2",
        }),
        "xz" => Some(FileType::Binary {
            mime: "application/x-xz",
        }),
        "zst" | "zstd" => Some(FileType::Binary {
            mime: "application/zstd",
        }),
        "7z" => Some(FileType::Binary {
            mime: "application/x-7z-compressed",
        }),
        "rar" => Some(FileType::Binary {
            mime: "application/vnd.rar",
        }),

        // Executables & libraries
        "exe" | "dll" | "msi" => Some(FileType::Binary {
            mime: "application/vnd.microsoft.portable-executable",
        }),
        "so" | "dylib" => Some(FileType::Binary {
            mime: "application/x-sharedlib",
        }),
        "a" | "lib" => Some(FileType::Binary {
            mime: "application/x-archive",
        }),
        "o" | "obj" => Some(FileType::Binary {
            mime: "application/x-object",
        }),
        "class" => Some(FileType::Binary {
            mime: "application/java-vm",
        }),
        "pyc" | "pyo" => Some(FileType::Binary {
            mime: "application/x-python-bytecode",
        }),
        "wasm" => Some(FileType::Binary {
            mime: "application/wasm",
        }),

        // Media
        "mp3" => Some(FileType::Binary { mime: "audio/mpeg" }),
        "mp4" => Some(FileType::Binary { mime: "video/mp4" }),
        "wav" => Some(FileType::Binary { mime: "audio/wav" }),
        "ogg" => Some(FileType::Binary { mime: "audio/ogg" }),
        "flac" => Some(FileType::Binary { mime: "audio/flac" }),
        "avi" => Some(FileType::Binary {
            mime: "video/x-msvideo",
        }),
        "mkv" => Some(FileType::Binary {
            mime: "video/x-matroska",
        }),
        "webm" => Some(FileType::Binary { mime: "video/webm" }),
        "mov" => Some(FileType::Binary {
            mime: "video/quicktime",
        }),

        // Databases
        "sqlite" | "sqlite3" | "db" => Some(FileType::Binary {
            mime: "application/x-sqlite3",
        }),

        // Not recognized — fall through to magic/heuristic
        _ => None,
    }
}

// ── Magic bytes lookup ────────────────────────────────────────────

fn classify_by_magic(bytes: &[u8]) -> Option<FileType> {
    if bytes.len() < 4 {
        return None;
    }

    // PNG: 89 50 4E 47 0D 0A 1A 0A
    if bytes.len() >= 8 && bytes.starts_with(&[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']) {
        return Some(FileType::Image { mime: "image/png" });
    }

    // JPEG: FF D8 FF
    if bytes.len() >= 3 && bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some(FileType::Image { mime: "image/jpeg" });
    }

    // GIF: GIF87a or GIF89a
    if bytes.len() >= 6 && (bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
        return Some(FileType::Image { mime: "image/gif" });
    }

    // WebP: RIFF....WEBP
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some(FileType::Image { mime: "image/webp" });
    }

    // BMP: BM
    if bytes.starts_with(b"BM") {
        return Some(FileType::Image { mime: "image/bmp" });
    }

    // PDF: %PDF-
    if bytes.starts_with(b"%PDF-") {
        return Some(FileType::Pdf);
    }

    // ZIP (also covers docx/xlsx/pptx since they are ZIP-based)
    if bytes.starts_with(&[0x50, 0x4B, 0x03, 0x04]) {
        return Some(FileType::Binary {
            mime: "application/zip",
        });
    }

    // Gzip: 1F 8B
    if bytes.starts_with(&[0x1F, 0x8B]) {
        return Some(FileType::Binary {
            mime: "application/gzip",
        });
    }

    // ELF: 7F 45 4C 46
    if bytes.starts_with(&[0x7F, b'E', b'L', b'F']) {
        return Some(FileType::Binary {
            mime: "application/x-elf",
        });
    }

    // Mach-O (32/64-bit, both endiannesses)
    if bytes.starts_with(&[0xFE, 0xED, 0xFA, 0xCE])
        || bytes.starts_with(&[0xFE, 0xED, 0xFA, 0xCF])
        || bytes.starts_with(&[0xCE, 0xFA, 0xED, 0xFE])
        || bytes.starts_with(&[0xCF, 0xFA, 0xED, 0xFE])
    {
        return Some(FileType::Binary {
            mime: "application/x-mach-binary",
        });
    }

    // PE/COFF (MZ header)
    if bytes.starts_with(b"MZ") {
        return Some(FileType::Binary {
            mime: "application/vnd.microsoft.portable-executable",
        });
    }

    // SQLite: "SQLite format 3\0"
    if bytes.len() >= 16 && bytes.starts_with(b"SQLite format 3\0") {
        return Some(FileType::Binary {
            mime: "application/x-sqlite3",
        });
    }

    // WASM: \0asm
    if bytes.starts_with(&[0x00, b'a', b's', b'm']) {
        return Some(FileType::Binary {
            mime: "application/wasm",
        });
    }

    None
}

// ── Binary heuristic ──────────────────────────────────────────────

/// Returns `true` if the header contains characteristics typical of binary
/// data: null bytes within the first `HEADER_SIZE` bytes, or a majority of
/// non-UTF-8 sequences.
fn looks_binary(header: &[u8]) -> bool {
    let check = if header.len() > HEADER_SIZE {
        &header[..HEADER_SIZE]
    } else {
        header
    };

    if check.is_empty() {
        return false;
    }

    // Null bytes are a strong binary signal.
    if check.contains(&0x00) {
        return true;
    }

    // If the header is not valid UTF-8, treat as binary.
    std::str::from_utf8(check).is_err()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn detect_text_file_by_extension() {
        let path = PathBuf::from("README.md");
        assert_eq!(detect_file_type(&path, None), FileType::Text);
    }

    #[test]
    fn detect_text_file_with_text_header() {
        let path = PathBuf::from("data.txt");
        let header = b"Hello, world!\nThis is a text file.\n";
        assert_eq!(detect_file_type(&path, Some(header)), FileType::Text);
    }

    #[test]
    fn detect_png_by_extension() {
        let path = PathBuf::from("photo.png");
        assert_eq!(
            detect_file_type(&path, None),
            FileType::Image { mime: "image/png" }
        );
    }

    #[test]
    fn detect_png_by_magic() {
        let path = PathBuf::from("no_extension");
        let header = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 0, 0];
        assert_eq!(
            detect_file_type(&path, Some(&header)),
            FileType::Image { mime: "image/png" }
        );
    }

    #[test]
    fn detect_jpeg_by_extension() {
        let path = PathBuf::from("photo.jpg");
        assert_eq!(
            detect_file_type(&path, None),
            FileType::Image { mime: "image/jpeg" }
        );
    }

    #[test]
    fn detect_jpeg_by_magic() {
        let path = PathBuf::from("mystery");
        let header = [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        assert_eq!(
            detect_file_type(&path, Some(&header)),
            FileType::Image { mime: "image/jpeg" }
        );
    }

    #[test]
    fn detect_gif_by_magic() {
        let path = PathBuf::from("anim");
        let header = b"GIF89a\x01\x00\x01\x00";
        assert_eq!(
            detect_file_type(&path, Some(header)),
            FileType::Image { mime: "image/gif" }
        );
    }

    #[test]
    fn detect_pdf_by_extension() {
        let path = PathBuf::from("report.pdf");
        assert_eq!(detect_file_type(&path, None), FileType::Pdf);
    }

    #[test]
    fn detect_pdf_by_magic() {
        let path = PathBuf::from("document");
        let header = b"%PDF-1.4 stuff";
        assert_eq!(detect_file_type(&path, Some(header)), FileType::Pdf);
    }

    #[test]
    fn detect_docx_by_extension() {
        let path = PathBuf::from("file.docx");
        let ft = detect_file_type(&path, None);
        assert!(matches!(ft, FileType::Office { .. }));
        if let FileType::Office { mime } = ft {
            assert!(mime.contains("wordprocessingml"));
        }
    }

    #[test]
    fn detect_xlsx_by_extension() {
        let path = PathBuf::from("data.xlsx");
        let ft = detect_file_type(&path, None);
        assert!(matches!(ft, FileType::Office { .. }));
    }

    #[test]
    fn detect_pptx_by_extension() {
        let path = PathBuf::from("slides.pptx");
        let ft = detect_file_type(&path, None);
        assert!(matches!(ft, FileType::Office { .. }));
    }

    #[test]
    fn detect_exe_by_extension() {
        let path = PathBuf::from("app.exe");
        let ft = detect_file_type(&path, None);
        assert!(matches!(ft, FileType::Binary { .. }));
    }

    #[test]
    fn detect_dll_by_extension() {
        let path = PathBuf::from("lib.dll");
        let ft = detect_file_type(&path, None);
        assert!(matches!(ft, FileType::Binary { .. }));
    }

    #[test]
    fn detect_so_by_extension() {
        let path = PathBuf::from("lib.so");
        let ft = detect_file_type(&path, None);
        assert!(matches!(ft, FileType::Binary { .. }));
    }

    #[test]
    fn detect_zip_by_extension() {
        let path = PathBuf::from("archive.zip");
        let ft = detect_file_type(&path, None);
        assert!(matches!(ft, FileType::Binary { .. }));
        if let FileType::Binary { mime } = ft {
            assert_eq!(mime, "application/zip");
        }
    }

    #[test]
    fn detect_zip_by_magic() {
        let path = PathBuf::from("unknown");
        let header = [0x50, 0x4B, 0x03, 0x04, 0x00, 0x00];
        let ft = detect_file_type(&path, Some(&header));
        assert!(matches!(ft, FileType::Binary { .. }));
    }

    #[test]
    fn detect_elf_by_magic() {
        let path = PathBuf::from("program");
        let header = [0x7F, b'E', b'L', b'F', 0x02, 0x01];
        let ft = detect_file_type(&path, Some(&header));
        assert!(matches!(ft, FileType::Binary { .. }));
    }

    #[test]
    fn detect_mz_by_magic() {
        let path = PathBuf::from("something");
        let header = b"MZ\x90\x00\x03\x00\x00\x00";
        let ft = detect_file_type(&path, Some(header));
        assert!(matches!(ft, FileType::Binary { .. }));
    }

    #[test]
    fn detect_binary_by_null_bytes() {
        let path = PathBuf::from("unknown_file");
        let header = b"some text\x00with null bytes";
        let ft = detect_file_type(&path, Some(header));
        assert!(matches!(ft, FileType::Binary { .. }));
    }

    #[test]
    fn detect_binary_by_invalid_utf8() {
        let path = PathBuf::from("no_ext");
        let header: &[u8] = &[0x80, 0x81, 0x82, 0x83, 0xFE, 0xFF];
        let ft = detect_file_type(&path, Some(header));
        assert!(matches!(ft, FileType::Binary { .. }));
    }

    #[test]
    fn detect_text_no_extension_valid_utf8() {
        let path = PathBuf::from("Makefile");
        let header = b"CC=gcc\nCFLAGS=-Wall\n";
        assert_eq!(detect_file_type(&path, Some(header)), FileType::Text);
    }

    #[test]
    fn detect_svg_as_text() {
        let path = PathBuf::from("icon.svg");
        assert_eq!(detect_file_type(&path, None), FileType::Text);
    }

    #[test]
    fn detect_wasm_by_extension() {
        let path = PathBuf::from("module.wasm");
        let ft = detect_file_type(&path, None);
        assert!(matches!(ft, FileType::Binary { .. }));
    }

    #[test]
    fn detect_wasm_by_magic() {
        let path = PathBuf::from("noext");
        let header = [0x00, b'a', b's', b'm', 0x01, 0x00, 0x00, 0x00];
        let ft = detect_file_type(&path, Some(&header));
        assert!(matches!(ft, FileType::Binary { .. }));
    }

    #[test]
    fn detect_sqlite_by_magic() {
        let path = PathBuf::from("data");
        let mut header = b"SQLite format 3\0".to_vec();
        header.extend_from_slice(&[0; 16]);
        let ft = detect_file_type(&path, Some(&header));
        assert!(matches!(ft, FileType::Binary { .. }));
    }

    #[test]
    fn detect_tar_gz_by_extension() {
        // ".tar.gz" — the Path extension is "gz"
        let path = PathBuf::from("archive.tar.gz");
        let ft = detect_file_type(&path, None);
        assert!(matches!(ft, FileType::Binary { .. }));
    }

    #[test]
    fn detect_mp4_by_extension() {
        let path = PathBuf::from("video.mp4");
        let ft = detect_file_type(&path, None);
        assert!(matches!(ft, FileType::Binary { .. }));
    }

    #[test]
    fn format_size_bytes() {
        assert_eq!(format_size(500), "500 bytes");
    }

    #[test]
    fn format_size_kb() {
        assert_eq!(format_size(2048), "2 KB");
    }

    #[test]
    fn format_size_mb() {
        assert_eq!(format_size(1_500_000), "1.4 MB");
    }

    #[test]
    fn format_size_gb() {
        assert_eq!(format_size(2_500_000_000), "2.3 GB");
    }

    #[test]
    fn binary_summary_text_returns_none() {
        assert!(binary_summary(&FileType::Text, 100).is_none());
    }

    #[test]
    fn binary_summary_pdf_returns_none() {
        assert!(binary_summary(&FileType::Pdf, 100).is_none());
    }

    #[test]
    fn binary_summary_image() {
        let s = binary_summary(&FileType::Image { mime: "image/png" }, 250_000).unwrap();
        assert!(s.contains("image/png"));
        assert!(s.contains("244 KB"));
    }

    #[test]
    fn binary_summary_office() {
        let s = binary_summary(
            &FileType::Office {
                mime: "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            },
            1_200_000,
        )
        .unwrap();
        assert!(s.contains("wordprocessingml"));
        assert!(s.contains("document conversion tool"));
    }

    #[test]
    fn binary_summary_generic_binary() {
        let s = binary_summary(
            &FileType::Binary {
                mime: "application/zip",
            },
            5_000_000,
        )
        .unwrap();
        assert!(s.contains("application/zip"));
        assert!(s.contains("4.8 MB"));
    }

    #[test]
    fn empty_header_is_not_binary() {
        assert!(!looks_binary(b""));
    }

    #[test]
    fn gzip_by_magic() {
        let path = PathBuf::from("data");
        let header = [0x1F, 0x8B, 0x08, 0x00];
        let ft = detect_file_type(&path, Some(&header));
        assert!(matches!(ft, FileType::Binary { .. }));
    }

    #[test]
    fn webp_by_magic() {
        let path = PathBuf::from("image");
        let mut header = b"RIFF".to_vec();
        header.extend_from_slice(&[0x00; 4]); // size placeholder
        header.extend_from_slice(b"WEBP");
        let ft = detect_file_type(&path, Some(&header));
        assert_eq!(ft, FileType::Image { mime: "image/webp" });
    }

    #[test]
    fn case_insensitive_extension() {
        assert_eq!(
            detect_file_type(Path::new("IMAGE.PNG"), None),
            FileType::Image { mime: "image/png" }
        );
        assert_eq!(
            detect_file_type(Path::new("DOC.DOCX"), None),
            FileType::Office {
                mime: "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
            }
        );
    }
}
