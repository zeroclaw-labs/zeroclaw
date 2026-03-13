//! Storage backends for file uploads and document processing.
//!
//! Currently supports Cloudflare R2 (S3-compatible) for the image PDF
//! upload flow: client → R2 (pre-signed PUT) → Railway downloads → Upstage OCR.

pub mod r2;
