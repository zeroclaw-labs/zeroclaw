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
//!
//! Two budgets bound the work a network-controlled body can cause. The decoded
//! budget is the caller's response cap: the sink refuses output past it, which
//! stops the decompressor part-way through a chunk rather than after it. The
//! compressed budget bounds the input fed to the decoder, so a stream that
//! consumes far more input than it produces cannot run to the request timeout
//! under a small response cap.

use std::io::{self, Write};

use futures_util::StreamExt;
use reqwest::header::CONTENT_ENCODING;

use crate::helpers::response_body::{BoundedBody, into_text};

/// Compressed input a response may spend beyond the decoded cap before the
/// reader stops feeding the decoder. The slack covers per-member headers and
/// trailers, a decoder window, and the ordinary case where a body barely
/// compresses, so a legitimate response is bounded by its decoded size rather
/// than by this.
const COMPRESSED_INPUT_SLACK: usize = 64 * 1024;

/// Marks the sink's refusal so a spent decoded budget is told apart from a
/// genuinely malformed stream, wherever the decoder surfaces it.
const CAP_REACHED: &str = "decoded response cap reached";

fn is_cap_reached(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::WriteZero && error.to_string().contains(CAP_REACHED)
}

/// A `Write` sink that retains at most `cap` bytes and refuses the rest.
///
/// Refusing rather than absorbing is the point: a decompressor asked to write
/// past the cap stops inside the chunk it is expanding, so a highly compressible
/// body cannot spend the CPU to expand a whole chunk that would be dropped.
struct CappedWriter {
    buf: Vec<u8>,
    cap: usize,
    /// Decoded bytes the decompressor has offered, retained or refused. This is
    /// the decoder-work meter the cap exists to bound, so the regression can
    /// assert the decoder stopped rather than merely that the output is short.
    offered: usize,
}

impl CappedWriter {
    fn new(cap: usize) -> Self {
        Self {
            buf: Vec::new(),
            cap,
            offered: 0,
        }
    }

    fn is_full(&self) -> bool {
        self.buf.len() >= self.cap
    }

    fn cap_reached() -> io::Error {
        io::Error::new(io::ErrorKind::WriteZero, CAP_REACHED)
    }
}

impl Write for CappedWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.offered = self.offered.saturating_add(data.len());
        let room = self.cap.saturating_sub(self.buf.len());
        if room == 0 {
            return Err(Self::cap_reached());
        }
        let take = room.min(data.len());
        self.buf.extend_from_slice(&data[..take]);
        if take < data.len() {
            // The retained prefix is kept; the rest of this slice is refused so
            // the decoder gives up here instead of expanding the remainder.
            return Err(Self::cap_reached());
        }
        Ok(take)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

// The compressed decoders are boxed: `brotli::DecompressorWriter` is far larger
// than the identity sink, so an unboxed enum would carry that size everywhere.
enum BodyDecoder {
    Identity(CappedWriter),
    // `MultiGzDecoder`, not `GzDecoder`: RFC 1952 defines a gzip body as a
    // series of members, and the single-member decoder would silently return
    // only the first one as if it were the whole response.
    Gzip(Box<flate2::write::MultiGzDecoder<CappedWriter>>),
    Zlib(Box<flate2::write::ZlibDecoder<CappedWriter>>),
    Brotli(Box<brotli::DecompressorWriter<CappedWriter>>),
}

impl BodyDecoder {
    /// Pick a decoder for the codings a response advertised. Returns `None` for
    /// an unsupported coding or for any chain of two or more, so the caller
    /// rejects it instead of handing back still-encoded bytes as garbage.
    fn for_codings(codings: &[String], cap: usize) -> Option<Self> {
        let sink = CappedWriter::new(cap);
        let [single] = codings else {
            // No coding at all is identity; a chain is refused rather than
            // guessed at.
            return codings.is_empty().then_some(Self::Identity(sink));
        };
        match single.as_str() {
            "" | "identity" => Some(Self::Identity(sink)),
            "gzip" | "x-gzip" => Some(Self::Gzip(Box::new(flate2::write::MultiGzDecoder::new(
                sink,
            )))),
            // HTTP `deflate` is zlib-wrapped in practice (and reqwest decoded it
            // that way); the tests encode with `flate2`'s ZlibEncoder.
            "deflate" => Some(Self::Zlib(Box::new(flate2::write::ZlibDecoder::new(sink)))),
            "br" => Some(Self::Brotli(Box::new(brotli::DecompressorWriter::new(
                sink, 4096,
            )))),
            _ => None,
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

    /// Push the decoder's own output buffer into the sink. Without this a
    /// decoder can hold a whole chunk's worth of output internally, leaving
    /// `is_full` blind to a budget that is already spent.
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Identity(w) => w.flush(),
            Self::Gzip(d) => d.flush(),
            Self::Zlib(d) => d.flush(),
            Self::Brotli(d) => d.flush(),
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

    /// Decoded bytes accumulated so far, without finalising the stream. Used
    /// when a budget stopped the read, where a strict `finish` would wrongly
    /// reject a legitimately over-cap body as truncated or corrupt.
    fn buffered(&self) -> Vec<u8> {
        match self {
            Self::Identity(w) => w.buf.clone(),
            Self::Gzip(d) => d.get_ref().buf.clone(),
            Self::Zlib(d) => d.get_ref().buf.clone(),
            Self::Brotli(d) => d.get_ref().buf.clone(),
        }
    }

    #[cfg(test)]
    fn decoded_offered(&self) -> usize {
        match self {
            Self::Identity(w) => w.offered,
            Self::Gzip(d) => d.get_ref().offered,
            Self::Zlib(d) => d.get_ref().offered,
            Self::Brotli(d) => d.get_ref().offered,
        }
    }

    /// Finalise the stream and return the decoded bytes. Errors on a malformed
    /// or incomplete compressed body.
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

/// The codings a response advertised, in the order they were applied.
///
/// A list-valued HTTP field may arrive comma-joined in one line or split across
/// repeated lines, and the two forms are equivalent, so every value is collected
/// and split before the contract is judged. A value that is not valid text is a
/// malformed field, not an absent one, and must not fall through to identity.
fn parse_content_codings(headers: &reqwest::header::HeaderMap) -> anyhow::Result<Vec<String>> {
    let mut codings = Vec::new();
    for value in headers.get_all(CONTENT_ENCODING) {
        let text = value
            .to_str()
            .map_err(|_| anyhow::Error::msg("malformed Content-Encoding header value"))?;
        for token in text.split(',') {
            codings.push(token.trim().to_ascii_lowercase());
        }
    }
    Ok(codings)
}

/// Streaming decode bounded by a decoded-output budget and a compressed-input
/// budget. Driven by `read_decoded_text` over a response body, and directly by
/// the tests over fixed chunks.
struct BoundedDecode {
    decoder: BodyDecoder,
    /// Compressed bytes the decoder may still be fed, `None` when unlimited.
    input_budget: Option<usize>,
    consumed: usize,
    /// A budget stopped the read, so the body is a decoded prefix rather than a
    /// complete stream.
    stopped_early: bool,
}

impl BoundedDecode {
    fn new(codings: &[String], decoded_cap: usize, input_budget: Option<usize>) -> Option<Self> {
        Some(Self {
            decoder: BodyDecoder::for_codings(codings, decoded_cap)?,
            input_budget,
            consumed: 0,
            stopped_early: false,
        })
    }

    /// Feed one chunk. Returns `false` once a budget is spent and no further
    /// chunk should be read.
    fn push(&mut self, chunk: &[u8]) -> io::Result<bool> {
        let written = self
            .decoder
            .write_all(chunk)
            .and_then(|()| self.decoder.flush());
        if let Err(error) = written {
            // The sink refuses output once the decoded budget is spent; any
            // other failure is a genuine malformed-stream error.
            if !is_cap_reached(&error) && !self.decoder.is_full() {
                return Err(error);
            }
            self.stopped_early = true;
            return Ok(false);
        }
        self.consumed = self.consumed.saturating_add(chunk.len());
        if self.decoder.is_full()
            || self
                .input_budget
                .is_some_and(|budget| self.consumed >= budget)
        {
            self.stopped_early = true;
            return Ok(false);
        }
        Ok(true)
    }

    #[cfg(test)]
    fn decoded_offered(&self) -> usize {
        self.decoder.decoded_offered()
    }

    /// Decoded bytes plus whether a budget cut the body short.
    fn finish(self) -> io::Result<(Vec<u8>, bool)> {
        if self.stopped_early {
            return Ok((self.decoder.buffered(), true));
        }
        // A decoder holds output of its own, so the flush that ends the stream
        // can be what finally spends the budget. That is truncation, not a
        // malformed body, and the prefix decoded so far is the answer.
        let prefix = self.decoder.buffered();
        match self.decoder.finish() {
            Ok(bytes) => Ok((bytes, false)),
            Err(error) if is_cap_reached(&error) => Ok((prefix, true)),
            Err(error) => Err(error),
        }
    }
}

/// Read an HTTP response body, decoding `Content-Encoding: gzip | deflate | br`,
/// and return it as text alongside whether it was truncated. `limit` is the
/// decoded byte cap; `None` means unlimited. Returns an error on a body-stream
/// failure, a malformed or unsupported encoding contract, or a malformed
/// compressed body.
pub(crate) async fn read_decoded_text(
    response: reqwest::Response,
    limit: Option<usize>,
) -> anyhow::Result<(String, bool)> {
    // One byte over the limit, so the caller can still detect truncation.
    let decoded_cap = limit.map_or(usize::MAX, |value| value.saturating_add(1));
    let input_budget = limit.map(|value| value.saturating_add(COMPRESSED_INPUT_SLACK));

    let codings = parse_content_codings(response.headers())?;
    let mut decode = BoundedDecode::new(&codings, decoded_cap, input_budget).ok_or_else(|| {
        anyhow::Error::msg(format!(
            "unsupported Content-Encoding: {}",
            codings.join(", ")
        ))
    })?;

    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        if !decode.push(&chunk?)? {
            break;
        }
    }

    let (mut bytes, stopped_early) = decode.finish()?;
    let overflowed = stopped_early || limit.is_some_and(|value| bytes.len() > value);
    if let Some(value) = limit {
        bytes.truncate(value);
    }
    Ok(into_text(BoundedBody { bytes, overflowed }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(values: &[&[u8]]) -> reqwest::header::HeaderMap {
        let mut map = reqwest::header::HeaderMap::new();
        for value in values {
            map.append(
                CONTENT_ENCODING,
                reqwest::header::HeaderValue::from_bytes(value).unwrap(),
            );
        }
        map
    }

    fn gzip_member(payload: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(payload).unwrap();
        encoder.finish().unwrap()
    }

    /// Returns the decoded bytes, whether a budget cut the body short, and how
    /// many decoded bytes the decompressor produced in total.
    fn decode_chunks(
        codings: &[&str],
        limit: Option<usize>,
        chunks: &[&[u8]],
    ) -> anyhow::Result<(Vec<u8>, bool, usize)> {
        let codings: Vec<String> = codings.iter().map(|c| (*c).to_string()).collect();
        let decoded_cap = limit.map_or(usize::MAX, |value| value.saturating_add(1));
        let input_budget = limit.map(|value| value.saturating_add(COMPRESSED_INPUT_SLACK));
        let mut decode = BoundedDecode::new(&codings, decoded_cap, input_budget)
            .ok_or_else(|| anyhow::Error::msg("unsupported Content-Encoding"))?;
        for chunk in chunks {
            if !decode.push(chunk)? {
                break;
            }
        }
        let offered = decode.decoded_offered();
        let (bytes, truncated) = decode.finish()?;
        Ok((bytes, truncated, offered))
    }

    #[test]
    fn absent_encoding_is_identity() {
        assert!(parse_content_codings(&headers(&[])).unwrap().is_empty());
    }

    #[test]
    fn repeated_field_lines_are_one_coding_list() {
        let codings = parse_content_codings(&headers(&[b"gzip", b"br"])).unwrap();
        assert_eq!(codings, vec!["gzip".to_string(), "br".to_string()]);
        // Equivalent to the comma-joined form, and refused the same way.
        assert!(BodyDecoder::for_codings(&codings, 1024).is_none());
    }

    #[test]
    fn comma_joined_chain_is_refused() {
        let codings = parse_content_codings(&headers(&[b"gzip, br"])).unwrap();
        assert_eq!(codings, vec!["gzip".to_string(), "br".to_string()]);
        assert!(BodyDecoder::for_codings(&codings, 1024).is_none());
    }

    #[test]
    fn invalid_field_bytes_fail_closed() {
        let error = parse_content_codings(&headers(&[b"\xff\xfe"]))
            .expect_err("a non-text field value must not be treated as absent");
        assert!(
            error.to_string().contains("malformed Content-Encoding"),
            "{error}"
        );
    }

    #[test]
    fn single_supported_coding_is_case_insensitive() {
        let codings = parse_content_codings(&headers(&[b"GZip"])).unwrap();
        assert_eq!(codings, vec!["gzip".to_string()]);
        assert!(BodyDecoder::for_codings(&codings, 1024).is_some());
    }

    #[test]
    fn concatenated_gzip_members_all_decode() {
        let mut body = gzip_member(b"first ");
        body.extend_from_slice(&gzip_member(b"second"));

        let (bytes, truncated, _) = decode_chunks(&["gzip"], Some(1024), &[&body]).unwrap();

        assert_eq!(String::from_utf8(bytes).unwrap(), "first second");
        assert!(!truncated);
    }

    #[test]
    fn decoded_cap_stops_the_decoder_inside_the_chunk() {
        // One network chunk that expands to 4 MiB under a 1 KiB cap. Retaining
        // only the prefix is not enough: the decompressor must stop being asked
        // for output, or a highly compressible body buys 4 MiB of decode work
        // for a response the caller capped at 1 KiB.
        let body = gzip_member(&vec![b'a'; 4 * 1024 * 1024]);
        assert!(body.len() < 8 * 1024, "the compressed fixture stays small");

        let (bytes, truncated, offered) = decode_chunks(&["gzip"], Some(1024), &[&body]).unwrap();

        assert_eq!(bytes.len(), 1025, "cap plus the one detection byte");
        assert!(truncated);
        assert!(
            offered < 256 * 1024,
            "the decoder must stop near the cap, not expand the whole chunk; \
             it produced {offered} bytes"
        );
    }

    #[test]
    fn compressed_input_budget_stops_a_low_yield_stream() {
        // Members that decode to nothing: output never reaches the cap, so only
        // an input budget can end the read.
        let empty_member = gzip_member(b"");
        let members = (COMPRESSED_INPUT_SLACK / empty_member.len()) + 64;
        let mut body = Vec::new();
        for _ in 0..members {
            body.extend_from_slice(&empty_member);
        }
        let chunks: Vec<&[u8]> = body.chunks(1024).collect();

        let (bytes, truncated, _) = decode_chunks(&["gzip"], Some(1024), &chunks).unwrap();

        assert!(bytes.is_empty(), "the stream decodes to nothing");
        assert!(
            truncated,
            "spending the compressed-input budget must be reported as truncation"
        );
    }

    #[test]
    fn malformed_compressed_body_is_an_error() {
        let error = decode_chunks(&["gzip"], Some(1024), &[b"definitely not gzip"])
            .expect_err("a malformed stream must not decode as text");
        assert!(!error.to_string().is_empty());
    }
}
