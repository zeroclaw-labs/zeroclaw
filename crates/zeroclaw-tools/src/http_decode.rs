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

#[cfg(test)]
pub(crate) fn empty_gzip_members_past_input_slack() -> (Vec<u8>, usize) {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(b"").unwrap();
    let member = encoder.finish().unwrap();
    let member_count = (COMPRESSED_INPUT_SLACK / member.len()) + 64;
    let body = member.repeat(member_count);
    let limit = body.len() - COMPRESSED_INPUT_SLACK;
    (body, limit)
}

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

/// The zlib (deflate) variant retains the offered compressed input so ordinary
/// EOF can prove the stream actually completed. flate2's write-mode
/// `ZlibDecoder::finish` reports success whenever output stops growing, and
/// the miniz backend maps "needs more input" at Finish to a success-variant
/// status, so without this proof a missing or truncated body silently decodes
/// as an empty or partial success. Gzip (trailer validation) and brotli
/// (incomplete-stream error) detect this themselves; deflate is the one
/// advertised coding that cannot.
struct CompletionTrackedZlib {
    decoder: Box<flate2::write::ZlibDecoder<CappedWriter>>,
    /// Every byte offered to the decoder, already clipped to the
    /// compressed-input allowance by `BoundedDecode::push` — bounded by
    /// `limit + COMPRESSED_INPUT_SLACK` when limited, and unlimited mode
    /// already retains the whole decoded body.
    offered_input: Vec<u8>,
}

impl CompletionTrackedZlib {
    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        self.offered_input.extend_from_slice(data);
        self.decoder.write_all(data)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.decoder.flush()
    }

    fn get_ref(&self) -> &CappedWriter {
        self.decoder.get_ref()
    }

    /// Finalize only a genuinely complete stream: one independent
    /// `Flush::Finish` pass over the retained input, requiring
    /// `Status::StreamEnd` — which flate2 documents as all input consumed, all
    /// output written, and the adler-32 verified. Called only at ordinary EOF;
    /// budget-triggered stops take the truncation path before this runs.
    fn finish(self) -> io::Result<CappedWriter> {
        if !Self::stream_is_complete(&self.offered_input) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "incomplete or malformed deflate (zlib) stream",
            ));
        }
        self.decoder.finish()
    }

    /// Drive the backend incrementally with `Flush::None` — the same contract
    /// the primary decoder's own writer uses — over a fixed, reused buffer
    /// whose output is discarded (the decoded content already lives in the
    /// sink). The backend reports `StreamEnd` only when the terminal block
    /// completes with the adler-32 verified and the dictionary drained; a
    /// pass that consumes no input and produces no output, a needs-more-input
    /// status, or corruption all mean the stream never completed. A one-shot
    /// `Flush::Finish` call cannot replace this: on the pinned miniz backend
    /// the first Finish uses a non-wrapping output buffer, fails regardless
    /// when the whole remaining stream does not fit, and marks the stream
    /// Failed so no retry can recover — valid bodies larger than the scratch
    /// buffer would be rejected.
    fn stream_is_complete(offered: &[u8]) -> bool {
        let mut verify = flate2::Decompress::new(true);
        let mut discard = vec![0_u8; 8192];
        let mut input = offered;
        loop {
            let (before_in, before_out) = (verify.total_in(), verify.total_out());
            match verify.decompress(input, &mut discard, flate2::FlushDecompress::None) {
                Ok(flate2::Status::StreamEnd) => return true,
                Ok(flate2::Status::Ok) => {
                    input = &input[(verify.total_in() - before_in) as usize..];
                    if verify.total_in() == before_in && verify.total_out() == before_out {
                        return false;
                    }
                }
                _ => return false,
            }
        }
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
    Zlib(Box<CompletionTrackedZlib>),
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
            "deflate" => Some(Self::Zlib(Box::new(CompletionTrackedZlib {
                decoder: Box::new(flate2::write::ZlibDecoder::new(sink)),
                offered_input: Vec::new(),
            }))),
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

    /// Feed one chunk. Returns `false` once a budget truncates the input and no
    /// further chunk should be read.
    fn push(&mut self, chunk: &[u8]) -> io::Result<bool> {
        // A spent budget already ended the read; a repeated call must not feed
        // the decoder again.
        if self.stopped_early {
            return Ok(false);
        }
        // Clip the chunk to the remaining compressed-input allowance before the
        // decoder sees any of it. Feeding the whole transport chunk first would
        // let one chunk overshoot the allowance by its full size, which is
        // exactly the unbounded low-yield decoder work this budget exists to
        // prevent.
        let (offered, clipped) = match self.input_budget {
            Some(budget) => {
                let remaining = budget.saturating_sub(self.consumed);
                if remaining == 0 {
                    self.stopped_early = true;
                    return Ok(false);
                }
                (
                    &chunk[..chunk.len().min(remaining)],
                    chunk.len() > remaining,
                )
            }
            None => (chunk, false),
        };
        let written = self
            .decoder
            .write_all(offered)
            .and_then(|()| self.decoder.flush());
        // Account what was offered even when the decoded-output cap ends the
        // write, so the input meter always matches the bytes actually fed.
        self.consumed = self.consumed.saturating_add(offered.len());
        if let Err(error) = written {
            // The sink refuses output once the decoded budget is spent; any
            // other failure is a genuine malformed-stream error.
            if !is_cap_reached(&error) && !self.decoder.is_full() {
                return Err(error);
            }
            self.stopped_early = true;
            return Ok(false);
        }
        if self.decoder.is_full() || clipped {
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

/// Whether HTTP semantics guarantee this response carries no message body, so
/// representation metadata such as `Content-Encoding` cannot describe bytes a
/// decoder would need: a `HEAD` request, or a status whose framing rules
/// forbid a payload (informational responses, `204 No Content`, `304 Not
/// Modified`). Finalizing a decompressor over the zero bytes such a response
/// actually carries would report a missing trailer and turn a correct empty
/// body into a spurious body-read failure; an empty body is the answer there.
/// An ordinary `GET 200` with an empty or malformed compressed body still
/// fails — nothing here bypasses decoding on the basis of representation
/// metadata alone.
fn response_has_no_body(method: Option<&reqwest::Method>, status: reqwest::StatusCode) -> bool {
    method.is_some_and(|method| *method == reqwest::Method::HEAD)
        || status.is_informational()
        || status == reqwest::StatusCode::NO_CONTENT
        || status == reqwest::StatusCode::NOT_MODIFIED
}

/// Read an HTTP response body, decoding `Content-Encoding: gzip | deflate | br`,
/// and return it as text alongside whether it was truncated. `limit` is the
/// decoded byte cap; `None` means unlimited. `method` is the request method
/// when the caller knows it, enabling the bodyless bypass for `HEAD`; a
/// GET-only caller passes `None`. Returns an error on a body-stream failure, a
/// malformed or unsupported encoding contract, or a malformed compressed body.
pub(crate) async fn read_decoded_text(
    response: reqwest::Response,
    limit: Option<usize>,
    method: Option<reqwest::Method>,
) -> anyhow::Result<(String, bool)> {
    if response_has_no_body(method.as_ref(), response.status()) {
        return Ok((String::new(), false));
    }
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

    fn zlib_member(payload: &[u8]) -> Vec<u8> {
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
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
    fn bodyless_semantics_cover_head_and_framing_forbidden_statuses() {
        use reqwest::StatusCode;
        let head = Some(&reqwest::Method::HEAD);
        let get = Some(&reqwest::Method::GET);
        let unknown_caller: Option<&reqwest::Method> = None;

        // The method alone guarantees no body, whatever the status advertises.
        assert!(response_has_no_body(head, StatusCode::OK));
        assert!(response_has_no_body(head, StatusCode::NOT_MODIFIED));
        // Statuses whose framing rules forbid a payload, whatever the method.
        assert!(response_has_no_body(get, StatusCode::NO_CONTENT));
        assert!(response_has_no_body(unknown_caller, StatusCode::NO_CONTENT));
        assert!(response_has_no_body(
            unknown_caller,
            StatusCode::NOT_MODIFIED
        ));
        assert!(response_has_no_body(get, StatusCode::CONTINUE));
        // Everything else decodes normally.
        assert!(!response_has_no_body(get, StatusCode::OK));
        assert!(!response_has_no_body(unknown_caller, StatusCode::OK));
        assert!(!response_has_no_body(get, StatusCode::NOT_FOUND));
        assert!(!response_has_no_body(
            unknown_caller,
            StatusCode::INTERNAL_SERVER_ERROR
        ));
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
    fn oversized_chunk_never_feeds_past_the_input_allowance() {
        // An allowance worth four empty members. The first chunk consumes one
        // member; the second chunk exceeds the remaining three and carries an
        // output-producing member immediately after the allowance-aligned
        // prefix, so a regression that fed the whole chunk while accounting
        // only the clipped slice would surface in the decoder's own output
        // meter. A final push after exhaustion must not feed the payload
        // member either, so the assertions observe what the decoder was
        // actually given, not just the returned flag.
        let empty_member = gzip_member(b"");
        let payload_member = gzip_member(b"after-exhaustion");
        let budget = empty_member.len() * 4;
        let mut low_yield_chunk = empty_member.repeat(3);
        low_yield_chunk.extend_from_slice(&payload_member);
        assert!(
            low_yield_chunk.len() > budget - empty_member.len(),
            "the chunk must exceed the remaining allowance"
        );

        let codings = vec!["gzip".to_string()];
        let mut decode = BoundedDecode::new(&codings, 1024, Some(budget)).unwrap();

        assert!(
            decode.push(&empty_member).unwrap(),
            "the first member fits inside the allowance"
        );
        assert!(
            !decode.push(&low_yield_chunk).unwrap(),
            "an oversized chunk must end the read at the allowance"
        );
        assert!(
            !decode.push(&payload_member).unwrap(),
            "a call after exhaustion must not feed the decoder"
        );

        assert_eq!(
            decode.consumed, budget,
            "offered input must stop exactly at the allowance"
        );
        assert_eq!(
            decode.decoded_offered(),
            0,
            "the decoder must have produced nothing beyond the empty members"
        );
        let (bytes, truncated) = decode.finish().unwrap();
        assert!(
            truncated,
            "a spent input allowance is truncation, not a malformed stream"
        );
        assert!(
            bytes.is_empty(),
            "the payload member must never reach the decoder, got {:?}",
            String::from_utf8_lossy(&bytes)
        );
    }

    #[test]
    fn complete_stream_ending_exactly_at_input_allowance_is_not_truncated() {
        let (body, limit) = empty_gzip_members_past_input_slack();

        for chunk_size in [body.len(), 97] {
            let chunks: Vec<&[u8]> = body.chunks(chunk_size).collect();
            let (bytes, truncated, _) = decode_chunks(&["gzip"], Some(limit), &chunks).unwrap();

            assert!(bytes.is_empty(), "the complete stream decodes to nothing");
            assert!(
                !truncated,
                "ordinary EOF exactly at the input allowance must finalize the decoder"
            );
        }
    }

    #[test]
    fn malformed_compressed_body_is_an_error() {
        let error = decode_chunks(&["gzip"], Some(1024), &[b"definitely not gzip"])
            .expect_err("a malformed stream must not decode as text");
        assert!(!error.to_string().is_empty());
    }

    #[test]
    fn empty_deflate_body_is_a_malformed_stream() {
        // flate2's write-mode zlib finalizer reports success whenever output
        // stops growing, and the miniz backend maps "needs more input" at
        // Finish to a success-variant status. A GET 200 advertising deflate
        // with zero body bytes is not an encoded empty representation — there
        // is no zlib stream at all — and must fail, exactly like the gzip
        // missing-trailer case already does.
        let error = decode_chunks(&["deflate"], Some(1024), &[])
            .expect_err("an empty deflate body has no zlib stream to finish");
        assert!(
            error
                .to_string()
                .contains("incomplete or malformed deflate"),
            "got {error}"
        );
    }

    #[test]
    fn deflate_stream_missing_trailer_is_a_malformed_stream() {
        // A zlib body whose adler32 trailer was cut off decodes its payload
        // but never completes; the reader must report the unfinished stream
        // instead of returning the decoded bytes as a complete body.
        let mut stream = zlib_member(b"payload");
        let trailer_len = 4; // adler32
        stream.truncate(stream.len() - trailer_len);
        let error = decode_chunks(&["deflate"], Some(1024), &[&stream])
            .expect_err("a zlib stream without its trailer is incomplete");
        assert!(
            error
                .to_string()
                .contains("incomplete or malformed deflate"),
            "got {error}"
        );
    }

    #[test]
    fn complete_empty_deflate_stream_is_a_valid_empty_body() {
        // Positive control: a complete zlib stream that encodes zero bytes is
        // a legitimate empty body. The completion check must distinguish
        // "empty because the stream is complete" from "empty because bytes
        // are missing".
        let stream = zlib_member(b"");
        let (bytes, truncated, _) = decode_chunks(&["deflate"], Some(1024), &[&stream]).unwrap();
        assert!(bytes.is_empty(), "the stream encodes nothing");
        assert!(!truncated, "a complete stream is not truncation");
    }

    #[test]
    fn large_complete_deflate_bodies_decode_exactly() {
        // The completion verdict must not depend on any internal buffer
        // boundary: complete zlib bodies straddling the verifier's scratch
        // size decode exactly under the cap, with no truncation and no
        // malformed-stream error.
        for size in [8_191_usize, 8_192, 8_193, 16_384] {
            let payload: Vec<u8> = (0..size).map(|i| b'a' + (i % 26) as u8).collect();
            let stream = zlib_member(&payload);
            let (bytes, truncated, _) =
                decode_chunks(&["deflate"], Some(65_536), &[&stream]).unwrap();
            assert_eq!(bytes.len(), size, "decoded length for {size}");
            assert_eq!(bytes, payload, "decoded content for {size}");
            assert!(!truncated, "a complete body is not truncation ({size})");
        }
    }

    #[test]
    fn fragmented_large_deflate_body_decodes_exactly() {
        // Transport chunking must not affect the completion verdict: the same
        // 16 KiB body delivered in small chunks still decodes exactly.
        let payload: Vec<u8> = (0..16_384).map(|i| b'a' + (i % 26) as u8).collect();
        let stream = zlib_member(&payload);
        let chunks: Vec<&[u8]> = stream.chunks(97).collect();
        let (bytes, truncated, _) = decode_chunks(&["deflate"], Some(65_536), &chunks).unwrap();
        assert_eq!(bytes, payload, "decoded content across fragmented chunks");
        assert!(!truncated);
    }

    #[test]
    fn large_complete_deflate_body_under_unlimited_mode_decodes_exactly() {
        // Unlimited mode still runs the completion verdict; it must not
        // reject a complete body because of an internal buffer boundary.
        let payload: Vec<u8> = (0..16_384).map(|i| b'a' + (i % 26) as u8).collect();
        let stream = zlib_member(&payload);
        let (bytes, truncated, _) = decode_chunks(&["deflate"], None, &[&stream]).unwrap();
        assert_eq!(bytes, payload, "decoded content under unlimited mode");
        assert!(!truncated);
    }
}
