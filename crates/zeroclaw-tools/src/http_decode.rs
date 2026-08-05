//! Response-body decompression scoped to the HTTP tools that ask for it.
//!
//! `web_fetch` and `http_request` decode `Content-Encoding: gzip | deflate | br`
//! bodies here, streaming the decoded output into a hard byte cap. Decoding is
//! deliberately NOT enabled through reqwest's crate features: those unify across
//! the whole workspace (Cargo unifies features for the shared `reqwest`
//! package), which would turn on transparent decompression for every other
//! client and let a small compressed body expand past caps that assume
//! `Content-Length`. Keeping the decoders here confines the behaviour to the two
//! readers that are bounded for it.

use std::io::{self, Write};

use futures_util::StreamExt;

/// A `Write` sink that retains at most `cap` bytes and silently drops the rest,
/// so a streaming decoder can be driven while the decoded output stays bounded.
struct CappedWriter {
    buf: Vec<u8>,
    cap: usize,
}

impl CappedWriter {
    fn new(cap: usize) -> Self {
        Self {
            buf: Vec::new(),
            cap,
        }
    }

    fn is_full(&self) -> bool {
        self.buf.len() >= self.cap
    }
}

impl Write for CappedWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if self.buf.len() < self.cap {
            let take = (self.cap - self.buf.len()).min(data.len());
            self.buf.extend_from_slice(&data[..take]);
        }
        // Report the whole slice as consumed so the decoder keeps running even
        // after the cap is reached; the caller stops feeding once `is_full`.
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

// The compressed decoders are boxed: `brotli::DecompressorWriter` is far larger
// than the identity sink, so an unboxed enum would carry that size everywhere.
enum BodyDecoder {
    Identity(CappedWriter),
    Gzip(Box<flate2::write::GzDecoder<CappedWriter>>),
    Zlib(Box<flate2::write::ZlibDecoder<CappedWriter>>),
    Brotli(Box<brotli::DecompressorWriter<CappedWriter>>),
}

impl BodyDecoder {
    fn for_encoding(encoding: Option<&str>, cap: usize) -> Self {
        let sink = CappedWriter::new(cap);
        match encoding
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("gzip") | Some("x-gzip") => {
                Self::Gzip(Box::new(flate2::write::GzDecoder::new(sink)))
            }
            // HTTP `deflate` is zlib-wrapped in practice (and reqwest decoded it
            // that way); the tests encode with `flate2`'s ZlibEncoder.
            Some("deflate") => Self::Zlib(Box::new(flate2::write::ZlibDecoder::new(sink))),
            Some("br") => Self::Brotli(Box::new(brotli::DecompressorWriter::new(sink, 4096))),
            _ => Self::Identity(sink),
        }
    }

    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        match self {
            Self::Identity(w) => w.write_all(data),
            Self::Gzip(d) => d.write_all(data),
            Self::Zlib(d) => d.write_all(data),
            Self::Brotli(d) => d.write_all(data),
        }
    }

    fn is_full(&self) -> bool {
        match self {
            Self::Identity(w) => w.is_full(),
            Self::Gzip(d) => d.get_ref().is_full(),
            Self::Zlib(d) => d.get_ref().is_full(),
            Self::Brotli(d) => d.get_ref().is_full(),
        }
    }

    /// Decoded bytes accumulated so far, without finalising the stream. Used when
    /// the cap was hit mid-stream, where a strict `finish` would wrongly reject a
    /// legitimately over-cap body as truncated/corrupt.
    fn buffered(&self) -> Vec<u8> {
        match self {
            Self::Identity(w) => w.buf.clone(),
            Self::Gzip(d) => d.get_ref().buf.clone(),
            Self::Zlib(d) => d.get_ref().buf.clone(),
            Self::Brotli(d) => d.get_ref().buf.clone(),
        }
    }

    /// Finalise the stream and return the decoded bytes. Errors on a malformed or
    /// incomplete compressed body.
    fn finish(self) -> io::Result<Vec<u8>> {
        match self {
            Self::Identity(w) => Ok(w.buf),
            Self::Gzip(d) => Ok((*d).finish()?.buf),
            Self::Zlib(d) => Ok((*d).finish()?.buf),
            Self::Brotli(d) => match (*d).into_inner() {
                Ok(w) => Ok(w.buf),
                Err(_) => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "incomplete or malformed brotli stream",
                )),
            },
        }
    }
}

/// Read an HTTP response body, decoding `Content-Encoding: gzip | deflate | br`,
/// and return it as text capped at `max_response_size` decoded bytes (one byte
/// over the limit, so the caller can still detect and mark truncation). A
/// `max_response_size` of `0` means unlimited. Returns an error on a body-stream
/// failure or a malformed compressed body.
pub(crate) async fn read_decoded_text_capped(
    response: reqwest::Response,
    max_response_size: usize,
) -> anyhow::Result<String> {
    let hard_cap = if max_response_size == 0 {
        usize::MAX
    } else {
        max_response_size.saturating_add(1)
    };

    let encoding = response
        .headers()
        .get(reqwest::header::CONTENT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);

    let mut decoder = BodyDecoder::for_encoding(encoding.as_deref(), hard_cap);
    let mut stream = response.bytes_stream();
    let mut truncated = false;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        decoder.write_all(&chunk)?;
        if decoder.is_full() {
            truncated = true;
            break;
        }
    }

    let bytes = if truncated {
        // Stopped at the cap: keep the decoded prefix without finalising.
        decoder.buffered()
    } else {
        decoder.finish()?
    };
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}
